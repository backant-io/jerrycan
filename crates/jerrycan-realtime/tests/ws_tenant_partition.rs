//! Live-WS end-to-end proof of the #104 (0.7.2) membership-verified WS tenant
//! resolve → tenant-partitioned delivery seam.
//!
//! WHY this test exists (adversarial review of #104, Q5): the generated WS tenant
//! resolver (`jerrycan::platform::realtimegen::tenant_resolve_block`) is SAFE by
//! construction, but was only compile-tested and structure-string-tested — never
//! EXECUTED against a real `{tenant}_members` table. The one leak-shaped regression
//! that would pass every other gate is moving the `Some(tenant)` out from behind the
//! `let Some(row) else { return forbidden() }` membership guard, so a NON-member
//! gets `Some(tenant)`. Only a runtime test catches that. This converts the
//! by-construction guarantee into an executed one for the highest-risk seam.
//!
//! APPROACH — byte-mirrored resolver, NOT the generated one. Driving the generated
//! resolver here is impossible: the generator lives in the `jerrycan` crate, which
//! depends on `jerrycan-realtime` (not the reverse), and the emitted resolver
//! references generated `shared::*` auth types that only exist inside a scaffolded
//! app. Scaffolding + compiling a full app inside this crate's test suite would be
//! disproportionate. So the resolver installed below is the BYTE-FOR-BYTE runtime
//! twin of the SQL + control flow `tenant_resolve_block` emits for the integer-pk
//! `Workspace` tenancy design: same `?tenant=` parse, same
//! `SELECT role FROM workspace_members WHERE workspace_id = ? AND user_id = ?`
//! verify, same non-member ⇒ `forbidden()` guard, same
//! `SELECT workspace_id, role ... WHERE user_id = ? LIMIT 2` sole-membership
//! fallback, same zero/many ⇒ `None`. The `x-user` header stands in for the JWT
//! `CurrentUser` the generated resolver authenticates (its `user.0.id`).
//!
//! DRIFT-GUARD: because this mirror lives in a different crate than the generator,
//! it cannot assert against `tenant_resolve_block`'s output directly. That pin lives
//! on the generator side — `tenant_resolve_block_pins_the_ws_live_mirror_contract`
//! in `crates/jerrycan/src/platform/realtimegen.rs` — which asserts the generated
//! resolver still emits the two SQL statements below verbatim AND keeps the
//! membership-guard-BEFORE-`Some(tenant)` ordering this mirror reproduces. If the
//! generator's SQL or ordering changes, that test goes red and forces this mirror to
//! be updated in lockstep.

use futures_util::{SinkExt, StreamExt};
use jerrycan_core::{Dep, Headers, NoContent, Result};
use jerrycan_db::sea_orm::ConnectionTrait;
use jerrycan_realtime::{Principal, PrincipalResolver, Realtime, RealtimeHandle, TopicScope};
use tokio_tungstenite::tungstenite::Message;

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The two membership queries the generated `tenant_resolve_block` emits for the
/// integer-pk `Workspace` design — copied verbatim so the mirror executes exactly
/// what the generator produces. Pinned against generator drift by
/// `tenant_resolve_block_pins_the_ws_live_mirror_contract` (jerrycan crate).
const MEMBERSHIP_VERIFY_SQL: &str =
    "SELECT role FROM workspace_members WHERE workspace_id = ? AND user_id = ?";
const SOLE_MEMBERSHIP_SQL: &str =
    "SELECT workspace_id, role FROM workspace_members WHERE user_id = ? LIMIT 2";

/// Build the byte-mirrored membership-verify + tenant-select resolver over `db`.
/// This is the runtime twin of `tenant_resolve_block` for the integer-pk Workspace
/// design (see the module header for the fidelity contract).
fn membership_resolver(db: jerrycan_db::Db) -> PrincipalResolver {
    use jerrycan_db::sea_orm::Statement;
    std::sync::Arc::new(move |ctx: &mut jerrycan_core::RequestCtx| {
        let db = db.clone();
        Box::pin(async move {
            // The authenticated session user (generated: `user.0.id` from the JWT
            // CurrentUser). No credential ⇒ 401, which the WS upgrade maps to an
            // anonymous connection (#117) rather than aborting.
            let user_id = ctx
                .headers()
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(jerrycan_core::Error::unauthorized)?
                .to_string();
            // The optional `?tenant=` from the WS connect query — the SAME parse the
            // generated resolver performs.
            let requested = jerrycan_core::serde_urlencoded::from_str::<
                std::collections::HashMap<String, String>,
            >(ctx.uri().query().unwrap_or(""))
            .ok()
            .and_then(|m| m.get("tenant").cloned());
            let backend = db.conn().get_database_backend();
            let (resolved_tenant, resolved_role): (Option<String>, Option<String>) = match requested
            {
                Some(tenant) => {
                    // #104: an explicit ?tenant= is honored ONLY for a VERIFIED
                    // member — a non-member REFUSES the upgrade.
                    let tenant_key: i32 = match tenant.parse() {
                        Ok(v) => v,
                        Err(_) => return Err(jerrycan_core::Error::forbidden()),
                    };
                    let row = db
                        .conn()
                        .query_one(Statement::from_sql_and_values(
                            backend,
                            db.sql(MEMBERSHIP_VERIFY_SQL),
                            [tenant_key.into(), user_id.clone().into()],
                        ))
                        .await
                        .map_err(jerrycan_db::db_error)?;
                    let Some(row) = row else {
                        return Err(jerrycan_core::Error::forbidden());
                    };
                    let role: String = row.try_get("", "role").map_err(jerrycan_db::db_error)?;
                    (Some(tenant_key.to_string()), Some(role))
                }
                None => {
                    // No ?tenant=: EXACTLY ONE membership ⇒ that tenant; ZERO or
                    // MANY ⇒ None (connect, None/Auth topics only).
                    let rows = db
                        .conn()
                        .query_all(Statement::from_sql_and_values(
                            backend,
                            db.sql(SOLE_MEMBERSHIP_SQL),
                            [user_id.clone().into()],
                        ))
                        .await
                        .map_err(jerrycan_db::db_error)?;
                    if rows.len() == 1 {
                        let tenant_key: i32 = rows[0]
                            .try_get("", "workspace_id")
                            .map_err(jerrycan_db::db_error)?;
                        let role: String =
                            rows[0].try_get("", "role").map_err(jerrycan_db::db_error)?;
                        (Some(tenant_key.to_string()), Some(role))
                    } else {
                        (None, None)
                    }
                }
            };
            Ok(Principal {
                user_id,
                tenant_id: resolved_tenant,
                role: resolved_role,
            })
        })
    })
}

/// A server-side publish route: reads the target tenant/topic/label from request
/// headers and drives the REAL `RealtimeHandle::publish_to` (the tenant-partitioned
/// server publish, #104). Lets the test trigger a genuine `publish_to` from outside
/// the hub without reaching into `pub(crate)` internals.
async fn publish(headers: Headers, handle: Dep<RealtimeHandle>) -> Result<NoContent> {
    let tenant = headers.get("x-pub-tenant").unwrap_or_default().to_string();
    let topic = headers.get("x-pub-topic").unwrap_or("room").to_string();
    let label = headers.get("x-pub-label").unwrap_or("").to_string();
    handle
        .publish_to(&tenant, &topic, serde_json::json!({ "from": label }))
        .await?;
    Ok(NoContent)
}

/// Seed the `{tenant}_members` table the resolver verifies against:
/// user 1 ∈ {A=1}, user 2 ∈ {A=1, B=2}, user 3 ∈ {} (no rows).
async fn seed_members(db: &jerrycan_db::Db) {
    db.conn()
        .execute_unprepared(
            "CREATE TABLE workspace_members (\
                 user_id TEXT NOT NULL, \
                 workspace_id INTEGER NOT NULL, \
                 role TEXT NOT NULL, \
                 PRIMARY KEY (user_id, workspace_id))",
        )
        .await
        .expect("create workspace_members");
    db.conn()
        .execute_unprepared(
            "INSERT INTO workspace_members (user_id, workspace_id, role) VALUES \
                 ('1', 1, 'member'), ('2', 1, 'member'), ('2', 2, 'admin')",
        )
        .await
        .expect("seed workspace_members");
}

/// Serve an app with the membership resolver + a Tenant `room` topic, an Auth
/// `notices` topic, and the `/pub` server-publish route. Returns (port, shutdown,
/// task).
async fn serve(
    db: jerrycan_db::Db,
) -> (
    u16,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let rt = Realtime::new(db.clone())
        .broadcast("room", TopicScope::Tenant)
        .broadcast("notices", TopicScope::Auth)
        .principal(membership_resolver(db));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = jerrycan_core::App::new()
        .extend(rt)
        .route("/pub", jerrycan_core::get(publish));
    let task = tokio::spawn(async move {
        let _ = app
            .serve_with_shutdown(listener, async {
                let _ = rx.await;
            })
            .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (port, tx, task)
}

/// Connect a WS client as `user` (via `x-user`) with the given raw query string
/// (e.g. `"tenant=2"`, or `""` for none). Returns the connect Result so a REFUSED
/// upgrade can be asserted.
async fn connect(
    port: u16,
    user: &str,
    query: &str,
) -> std::result::Result<WsClient, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = if query.is_empty() {
        format!("ws://127.0.0.1:{port}/realtime")
    } else {
        format!("ws://127.0.0.1:{port}/realtime?{query}")
    };
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("x-user", user.parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn recv_json(ws: &mut WsClient) -> serde_json::Value {
    loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).expect("server frames are JSON");
        }
    }
}

async fn send_text(ws: &mut WsClient, text: &str) {
    ws.send(Message::Text(text.into())).await.unwrap();
}

async fn join(ws: &mut WsClient, channel: &str) {
    send_text(
        ws,
        &format!(r#"{{"op":"join","channel":"{channel}","ref":1}}"#),
    )
    .await;
}

/// Prove a socket received NOTHING on its channel: a heartbeat's ack must be the
/// very next frame (a leaked event would arrive first). The caller must sequence a
/// POSITIVE delivery on ANOTHER socket first, so the hub's single synchronous
/// `deliver_broadcast` pass (which would have leaked here if the filter were wrong)
/// has already run before the heartbeat.
async fn assert_silent(ws: &mut WsClient) {
    send_text(ws, r#"{"op":"heartbeat","ref":9}"#).await;
    let next = recv_json(ws).await;
    assert_eq!(next["op"], "heartbeat_ack", "leaked a tenant event: {next}");
}

/// Trigger a server-side `publish_to(tenant, topic, {"from": label})` over HTTP.
async fn publish_to(port: u16, tenant: &str, topic: &str, label: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!(
        "GET /pub HTTP/1.1\r\nHost: t\r\nx-pub-tenant: {tenant}\r\n\
         x-pub-topic: {topic}\r\nx-pub-label: {label}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    let resp = String::from_utf8_lossy(&buf);
    assert!(
        resp.starts_with("HTTP/1.1 2"),
        "server publish_to failed: {resp}"
    );
}

/// THE end-to-end proof: a real WS whose principal is produced by the executed
/// membership-verify + tenant-select logic, driving real tenant-partitioned
/// delivery. Covers all four review-recommended cases in one served app so each
/// negative control is sequenced behind a positive delivery.
#[tokio::test(flavor = "multi_thread")]
async fn membership_resolver_partitions_realtime_delivery_end_to_end() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    seed_members(&db).await;
    let (port, shutdown, task) = serve(db).await;

    // (1) NON-MEMBER REFUSED: user 1 (∈ {A=1}) asking for ?tenant=2 (B) is not a
    // member — the resolver returns forbidden() and the upgrade is REFUSED. A socket
    // can NEVER scope to a tenant its user isn't a verified member of.
    assert!(
        connect(port, "1", "tenant=2").await.is_err(),
        "a non-member's ?tenant= must REFUSE the WS upgrade (403), not connect"
    );

    // (2) MULTI-MEMBERSHIP PARTITION: user 2 (∈ {A=1, B=2}) opens two sockets, one
    // scoped to each tenant it chose at connect. publish_to("1") reaches ONLY the
    // tenant-1 socket; publish_to("2") ONLY the tenant-2 socket — even though the
    // SAME user is a member of both. The core cross-tenant-leak control.
    let mut a = connect(port, "2", "tenant=1").await.expect("member of A");
    let mut b = connect(port, "2", "tenant=2").await.expect("member of B");
    for ws in [&mut a, &mut b] {
        join(ws, "broadcast:room").await;
        assert_eq!(recv_json(ws).await["op"], "joined");
    }

    publish_to(port, "1", "room", "for-A").await;
    let ev = recv_json(&mut a).await; // positive: tenant-1 socket receives
    assert_eq!(ev["op"], "event");
    assert_eq!(ev["payload"]["from"], "for-A");
    assert_silent(&mut b).await; // negative: tenant-2 socket got nothing

    publish_to(port, "2", "room", "for-B").await;
    let ev = recv_json(&mut b).await; // positive: tenant-2 socket receives
    assert_eq!(ev["payload"]["from"], "for-B");
    assert_silent(&mut a).await; // negative: tenant-1 socket got nothing (chose A)

    drop(a);
    drop(b);

    // (3) SOLE-MEMBERSHIP: user 1 (∈ {A=1}) connecting WITHOUT ?tenant= resolves to
    // tenant_id = Some("1") via the LIMIT-2 sole-membership fallback and receives
    // its tenant's events.
    let mut sole = connect(port, "1", "")
        .await
        .expect("sole membership connects");
    join(&mut sole, "broadcast:room").await;
    assert_eq!(recv_json(&mut sole).await["op"], "joined");
    publish_to(port, "1", "room", "sole").await;
    let ev = recv_json(&mut sole).await;
    assert_eq!(ev["payload"]["from"], "sole", "sole-membership scopes to A");

    // (4) ZERO-MEMBERSHIP CONNECTS, NO TENANT EVENTS: user 3 (∈ {}) is NOT 403'd —
    // it connects with tenant_id = None. It may JOIN an Auth topic but is REJECTED
    // from a Tenant topic, and receives NO tenant publish_to event.
    let mut zero = connect(port, "3", "")
        .await
        .expect("zero-membership NOT refused");
    join(&mut zero, "broadcast:notices").await; // Auth topic — allowed
    assert_eq!(
        recv_json(&mut zero).await["op"],
        "joined",
        "a zero-membership authed principal may join an Auth topic"
    );
    join(&mut zero, "broadcast:room").await; // Tenant topic — rejected
    let err = recv_json(&mut zero).await;
    assert_eq!(err["op"], "error");
    assert_eq!(
        err["code"], "JC0403",
        "a None-tenant principal is refused a Tenant topic at JOIN: {err}"
    );
    // A tenant publish reaches the tenant-1 `sole` socket (positive, sequences the
    // pass) but never the zero-membership socket.
    publish_to(port, "1", "room", "z").await;
    assert_eq!(recv_json(&mut sole).await["payload"]["from"], "z");
    assert_silent(&mut zero).await;

    let _ = shutdown.send(());
    let _ = task.await;
}
