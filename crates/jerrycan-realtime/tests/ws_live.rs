//! Loopback WS integration: a real serve on 127.0.0.1, a real
//! tokio-tungstenite client, zero external services. Broadcast/presence run
//! without Postgres (resolved decision #9), so `Db` here is sqlite::memory:.
use jerrycan_realtime::{Realtime, TopicScope};
use tokio_tungstenite::tungstenite::Message;

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Serve an app with the given Realtime extension on an ephemeral port;
/// returns (port, shutdown sender, server task).
async fn serve(
    rt: Realtime,
) -> (
    u16,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = jerrycan_core::App::new().extend(rt);
    let task = tokio::spawn(async move {
        let _ = app
            .serve_with_shutdown(listener, async {
                let _ = rx.await;
            })
            .await;
    });
    // Let the accept loop come up.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (port, tx, task)
}

async fn connect(port: u16) -> WsClient {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/realtime"))
        .await
        .expect("ws connect");
    ws
}

async fn recv_json(ws: &mut WsClient) -> serde_json::Value {
    use futures_util::StreamExt;
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
    use futures_util::SinkExt;
    ws.send(Message::Text(text.into())).await.unwrap();
}

fn header_resolver() -> jerrycan_realtime::PrincipalResolver {
    std::sync::Arc::new(|ctx: &mut jerrycan_core::RequestCtx| {
        Box::pin(async move {
            let user = ctx
                .headers()
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(jerrycan_core::Error::unauthorized)?
                .to_string();
            let tenant = ctx
                .headers()
                .get("x-tenant")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            Ok(jerrycan_realtime::Principal {
                user_id: user,
                tenant_id: tenant,
                role: None,
            })
        })
    })
}

async fn connect_as(port: u16, user: &str, tenant: &str) -> WsClient {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/realtime")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("x-user", user.parse().unwrap());
    req.headers_mut()
        .insert("x-tenant", tenant.parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("ws connect");
    ws
}

#[tokio::test(flavor = "multi_thread")]
async fn broadcast_reaches_subscribers_but_not_publisher_or_other_tenants() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db)
        .broadcast("room", TopicScope::Tenant)
        .principal(header_resolver());
    let (port, shutdown, task) = serve(rt).await;

    let mut a = connect_as(port, "alice", "t1").await; // publisher
    let mut b = connect_as(port, "bob", "t1").await; // same tenant — receives
    let mut c = connect_as(port, "carol", "t2").await; // OTHER tenant — must not

    for ws in [&mut a, &mut b, &mut c] {
        send_text(ws, r#"{"op":"join","channel":"broadcast:room","ref":1}"#).await;
        assert_eq!(recv_json(ws).await["op"], "joined");
    }

    send_text(
        &mut a,
        r#"{"op":"publish","channel":"broadcast:room","payload":{"msg":"hi"},"ref":2}"#,
    )
    .await;

    // Bob gets the event.
    let ev = recv_json(&mut b).await;
    assert_eq!(ev["op"], "event");
    assert_eq!(ev["channel"], "broadcast:room");
    assert_eq!(ev["payload"]["msg"], "hi");

    // NEGATIVE CONTROLS: carol (cross-tenant) and alice (self) get NOTHING.
    // Prove it by round-tripping a heartbeat on each — the next frame must be
    // the ack, not a leaked event.
    for ws in [&mut c, &mut a] {
        send_text(ws, r#"{"op":"heartbeat","ref":9}"#).await;
        let next = recv_json(ws).await;
        assert_eq!(next["op"], "heartbeat_ack", "leaked broadcast: {next}");
    }

    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_join_sync_track_and_leave() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db)
        .presence("editors", TopicScope::Auth)
        .principal(header_resolver());
    let (port, shutdown, task) = serve(rt).await;

    let mut a = connect_as(port, "alice", "t1").await;
    send_text(
        &mut a,
        r#"{"op":"join","channel":"presence:editors","ref":1}"#,
    )
    .await;
    assert_eq!(recv_json(&mut a).await["op"], "joined");
    // Initial sync: empty state.
    let state = recv_json(&mut a).await;
    assert_eq!(state["op"], "presence_state");
    assert_eq!(state["state"], serde_json::json!({}));

    send_text(
        &mut a,
        r#"{"op":"track","channel":"presence:editors","state":{"cursor":1}}"#,
    )
    .await;
    let diff = recv_json(&mut a).await;
    assert_eq!(diff["op"], "presence_diff");
    assert_eq!(diff["joins"]["alice"]["cursor"], 1);

    // Bob joins late: his initial state already contains alice.
    let mut b = connect_as(port, "bob", "t1").await;
    send_text(
        &mut b,
        r#"{"op":"join","channel":"presence:editors","ref":1}"#,
    )
    .await;
    assert_eq!(recv_json(&mut b).await["op"], "joined");
    let state = recv_json(&mut b).await;
    assert_eq!(state["state"]["alice"]["cursor"], 1);

    // Alice disconnects: bob sees the leave diff.
    drop(a);
    let diff = recv_json(&mut b).await;
    assert_eq!(diff["op"], "presence_diff");
    assert!(
        diff["leaves"]["alice"].is_object(),
        "leave diff for alice: {diff}"
    );

    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_channel_on_sqlite_answers_jc0530() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db)
        .changes(jerrycan_realtime::ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
        })
        .principal(header_resolver());
    let (port, shutdown, task) = serve(rt).await;
    // Give the supervisor a beat to run detection and mark changes unavailable.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let mut ws = connect_as(port, "alice", "t1").await;
    send_text(&mut ws, r#"{"op":"join","channel":"changes:Lead","ref":1}"#).await;
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["code"], "JC0530");

    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_requires_membership_of_the_channel() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;
    let mut ws = connect(port).await;
    // Publish WITHOUT joining ⇒ 403-coded error envelope.
    send_text(
        &mut ws,
        r#"{"op":"publish","channel":"broadcast:lobby","payload":{},"ref":1}"#,
    )
    .await;
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["code"], "JC0403");
    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_heartbeat_and_error_envelopes_round_trip() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;

    let mut ws = connect(port).await;
    send_text(
        &mut ws,
        r#"{"op":"join","channel":"broadcast:lobby","ref":1}"#,
    )
    .await;
    let joined = recv_json(&mut ws).await;
    assert_eq!(joined["op"], "joined");
    assert_eq!(joined["channel"], "broadcast:lobby");
    assert_eq!(joined["ref"], 1);

    send_text(&mut ws, r#"{"op":"heartbeat","ref":2}"#).await;
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["op"], "heartbeat_ack");

    // Unknown channel ⇒ error envelope, connection stays up.
    send_text(
        &mut ws,
        r#"{"op":"join","channel":"broadcast:ghost","ref":3}"#,
    )
    .await;
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["ref"], 3);

    // Malformed JSON ⇒ error envelope with JC0422.
    send_text(&mut ws, "not json").await;
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["code"], "JC0422");

    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn non_websocket_get_is_rejected_without_upgrade() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;
    let res = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        s.write_all(b"GET /realtime HTTP/1.1\r\nHost: t\r\n\r\n")
            .await
            .unwrap();
        let mut buf = vec![0u8; 512];
        let n = s.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    };
    assert!(
        res.starts_with("HTTP/1.1 426"),
        "expected 426 Upgrade Required: {res}"
    );
    let _ = shutdown.send(());
    let _ = task.await;
}
