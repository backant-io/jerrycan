//! WebSocket transport: RFC 6455 handshake over hyper's HTTP/1 upgrade, then
//! tokio-tungstenite (Role::Server) over the upgraded socket.

use jerrycan_core::http::HeaderMap;
use jerrycan_core::{Error, Result};

/// Inbound WebSocket size cap (256 KiB), mirroring jerrycan's REST body limit.
/// tungstenite defaults to a 16 MiB frame / 64 MiB message ceiling, so an
/// authenticated client on a single-tenant broadcast could force the server to
/// buffer tens of MiB per frame — a memory-amplification DoS. Capping both the
/// frame and the reassembled message makes an over-cap frame a hard protocol
/// error (the read errors, the connection is dropped) instead.
const MAX_WS_MESSAGE_SIZE: usize = 256 * 1024;

/// Validate the upgrade request headers and derive Sec-WebSocket-Accept.
/// 400-class errors — the connection never upgrades on failure.
pub(crate) fn handshake_accept(headers: &HeaderMap) -> Result<String> {
    let connection_ok = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
    let upgrade_ok = headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if !connection_ok || !upgrade_ok {
        return Err(Error::new(
            jerrycan_core::http::StatusCode::UPGRADE_REQUIRED,
            "JC0400",
            "this endpoint speaks WebSocket — send Connection: Upgrade / Upgrade: websocket",
        ));
    }
    let version_ok = headers
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "13");
    if !version_ok {
        return Err(Error::bad_request(
            "unsupported Sec-WebSocket-Version (need 13)",
        ));
    }
    let key = headers
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::bad_request("missing Sec-WebSocket-Key"))?;
    Ok(tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes()))
}

/// Map a resolver outcome to the connection's principal (#117). A resolver
/// AUTHENTICATION failure (a 401 — no/invalid credential; the wired resolver
/// returns `Error::unauthorized()`) becomes an ANONYMOUS connection (`None`)
/// rather than a hard 401 at the WS upgrade: per-topic `scope_allows` then
/// enforces access — a `None` principal reaches only scope-`none` topics,
/// while scope-`auth`/`tenant` reject it at JOIN, so a bad credential accesses
/// nothing an anonymous client couldn't. Any OTHER resolver error (e.g. a 5xx
/// backend failure) is genuine and still aborts the upgrade — it must NOT
/// silently degrade to anonymous.
fn principal_from_resolver(resolved: Result<crate::Principal>) -> Result<Option<crate::Principal>> {
    match resolved {
        Ok(p) => Ok(Some(p)),
        Err(e) if e.status().as_u16() == 401 => Ok(None),
        Err(e) => Err(e),
    }
}

/// Extractor: validate the WS handshake, authenticate, and claim the upgrade
/// handle — all BEFORE replying, so a bad handshake is a plain error response
/// and never a half-open upgrade.
pub(crate) struct WsStart {
    hub: std::sync::Arc<crate::Hub>,
    principal: Option<crate::Principal>,
    accept: String,
    on_upgrade: hyper::upgrade::OnUpgrade,
}

impl jerrycan_core::FromRequest for WsStart {
    async fn from_request(ctx: &mut jerrycan_core::RequestCtx) -> Result<Self> {
        let handle = ctx.resolve::<crate::RealtimeHandle>().await?;
        let accept = handshake_accept(ctx.headers())?;
        // Resolve the principal BEFORE the upgrade. #117: a resolver AUTHENTICATION
        // failure (401 — a missing/invalid credential) is an ANONYMOUS connection
        // (`None`), NOT a hard 401 at the upgrade, so a public scope-`none` topic
        // stays reachable; per-topic `scope_allows` still rejects `None` from every
        // scope-`auth`/`tenant` topic at JOIN. A genuine non-auth error (e.g. a 5xx
        // backend failure) still aborts the upgrade.
        let principal = match handle.resolver.as_ref() {
            Some(r) => principal_from_resolver(r(ctx).await)?,
            None => None,
        };
        let on_upgrade = ctx
            .take_extension::<hyper::upgrade::OnUpgrade>()
            .ok_or_else(|| Error::internal("connection does not support upgrades"))?;
        Ok(WsStart {
            hub: handle.hub.clone(),
            principal,
            accept,
            on_upgrade,
        })
    }
}

pub(crate) async fn ws_handler(start: WsStart) -> Result<jerrycan_core::Response> {
    use jerrycan_core::IntoResponse;
    let WsStart {
        hub,
        principal,
        accept,
        on_upgrade,
    } = start;
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let io = hyper_util::rt::TokioIo::new(upgraded);
                let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(MAX_WS_MESSAGE_SIZE))
                    .max_frame_size(Some(MAX_WS_MESSAGE_SIZE));
                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    io,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    Some(config),
                )
                .await;
                run_connection(ws, hub, principal).await;
            }
            Err(e) => eprintln!("jerrycan-realtime: upgrade failed: {e}"),
        }
    });
    let mut res = "".into_response();
    *res.status_mut() = jerrycan_core::http::StatusCode::SWITCHING_PROTOCOLS;
    let headers = res.headers_mut();
    headers.insert(
        jerrycan_core::http::header::CONNECTION,
        jerrycan_core::http::HeaderValue::from_static("upgrade"),
    );
    headers.insert(
        jerrycan_core::http::header::UPGRADE,
        jerrycan_core::http::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        jerrycan_core::http::header::SEC_WEBSOCKET_ACCEPT,
        jerrycan_core::http::HeaderValue::from_str(&accept)
            .map_err(|_| Error::internal("accept key is always a valid header"))?,
    );
    Ok(res)
}

/// The per-connection loop. Single task, no socket split: select over the
/// outbound queue and the inbound stream. Both `mpsc::recv` and
/// `StreamExt::next` are cancel-safe, so a lost select branch drops nothing.
pub(crate) async fn run_connection<S>(
    mut ws: tokio_tungstenite::WebSocketStream<S>,
    hub: std::sync::Arc<crate::Hub>,
    principal: Option<crate::Principal>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    /// Server-side idle cutoff: a client that sends nothing (not even a
    /// heartbeat) for this long is disconnected.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(60);

    let (conn, mut rx) = hub.connect(principal);
    let mut deadline = tokio::time::Instant::now() + IDLE;

    enum Step {
        Out(Option<crate::protocol::ServerMsg>),
        In(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
        Idle,
    }

    loop {
        let step = tokio::select! {
            m = rx.recv() => Step::Out(m),
            r = ws.next() => Step::In(r),
            _ = tokio::time::sleep_until(deadline) => Step::Idle,
        };
        match step {
            Step::Out(Some(msg)) => {
                let text = serde_json::to_string(&msg).expect("server frames serialize");
                if ws.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            Step::Out(None) => break, // hub dropped us (slow consumer / shutdown)
            Step::In(Some(Ok(Message::Text(t)))) => {
                deadline = tokio::time::Instant::now() + IDLE;
                hub.handle_client(conn, t.as_str()).await;
            }
            Step::In(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {
                deadline = tokio::time::Instant::now() + IDLE;
                // tungstenite auto-answers pings on the next send/flush.
            }
            Step::In(Some(Ok(Message::Close(_))) | None) => break,
            Step::In(Some(Ok(_))) => {} // binary frames ignored (protocol is JSON text)
            Step::In(Some(Err(_))) => break,
            Step::Idle => break,
        }
    }
    hub.disconnect(conn).await;
    let _ = ws.close(None).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use jerrycan_core::http::{HeaderMap, HeaderValue};

    fn ws_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("connection", HeaderValue::from_static("Upgrade"));
        h.insert("upgrade", HeaderValue::from_static("websocket"));
        h.insert("sec-websocket-version", HeaderValue::from_static("13"));
        h.insert(
            "sec-websocket-key",
            HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
        );
        h
    }

    /// RFC 6455 §1.3 sample nonce → the exact accept key.
    #[test]
    fn accept_key_matches_rfc_6455_vector() {
        assert_eq!(
            handshake_accept(&ws_headers()).unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn missing_or_wrong_headers_are_rejected() {
        let mut no_key = ws_headers();
        no_key.remove("sec-websocket-key");
        assert!(handshake_accept(&no_key).is_err());

        let mut wrong_version = ws_headers();
        wrong_version.insert("sec-websocket-version", HeaderValue::from_static("8"));
        assert!(handshake_accept(&wrong_version).is_err());

        let mut not_ws = ws_headers();
        not_ws.insert("upgrade", HeaderValue::from_static("h2c"));
        assert!(handshake_accept(&not_ws).is_err());

        // Connection may be a list: "keep-alive, Upgrade" must pass.
        let mut list = ws_headers();
        list.insert(
            "connection",
            HeaderValue::from_static("keep-alive, Upgrade"),
        );
        assert!(handshake_accept(&list).is_ok());
    }
}

/// #117: an anonymous (or bad-credential) WS client must still reach a public
/// scope-`none` topic when an auth model is present. Before the fix the resolver
/// `?`-401'd such a client at the UPGRADE, so a scope-`none` topic was
/// unreachable the moment the app had auth — contradicting `scope_allows`.
#[cfg(test)]
mod anon_scope_none_tests {
    use super::*;
    use crate::bus::{AnyBus, LocalBus};
    use crate::presence::PresenceMap;
    use crate::protocol::ServerMsg;
    use crate::{ChangeChannelSpec, Hub, Principal, RealtimeConfig, TopicScope};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

    /// A hub declaring one topic of each scope plus a changes entity — the four
    /// gates a `None` principal must be checked against.
    fn hub() -> Arc<Hub> {
        let config = RealtimeConfig {
            changes: vec![ChangeChannelSpec {
                entity: "Lead".into(),
                table: "leads".into(),
                pk_column: "id".into(),
                tenant_column: Some("workspace_id".into()),
                owner_column: None,
                hidden_columns: Vec::new(),
            }],
            broadcast: vec![
                ("lobby".into(), TopicScope::None),
                ("events".into(), TopicScope::Auth),
                ("room".into(), TopicScope::Tenant),
            ],
            presence: vec![],
        };
        Arc::new(Hub {
            config,
            node_id: 1,
            bus: AnyBus::Local(LocalBus::new()),
            db: None,
            conns: Mutex::new(HashMap::new()),
            presence: Mutex::new(PresenceMap::default()),
            changes_unavailable: AtomicBool::new(false),
            next_conn: AtomicU64::new(1),
        })
    }

    fn tenant_user() -> Principal {
        Principal {
            user_id: "u1".into(),
            tenant_id: Some("t1".into()),
            role: None,
        }
    }

    /// The mapping the fix hinges on: a resolver AUTH failure (401 — exactly what
    /// the wired resolver returns for a missing/invalid credential) ⇒ anonymous;
    /// any OTHER error (5xx, 403) still aborts the upgrade and is NOT silently
    /// downgraded to anonymous.
    #[test]
    fn resolver_401_maps_to_anonymous_other_errors_propagate() {
        // A valid credential ⇒ Some(principal).
        assert!(matches!(
            principal_from_resolver(Ok(tenant_user())),
            Ok(Some(_))
        ));
        // A 401 (missing/invalid credential) ⇒ anonymous (None), NOT a hard error.
        assert!(matches!(
            principal_from_resolver(Err(Error::unauthorized())),
            Ok(None)
        ));
        // A genuine 5xx backend failure still aborts — never silent anonymous.
        let err = principal_from_resolver(Err(Error::internal("db down")))
            .expect_err("a 500 must propagate, not become anonymous");
        assert_eq!(err.status().as_u16(), 500);
        // A 403 is not an authentication failure either ⇒ propagates.
        assert!(principal_from_resolver(Err(Error::forbidden())).is_err());
    }

    /// THE bug fix: a `None` principal (what a missing/invalid credential now
    /// maps to) JOINs a scope-`none` topic and receives its broadcast. Before
    /// #117 this client was 401'd at the upgrade and never reached the hub.
    #[tokio::test]
    async fn anonymous_joins_scope_none_and_receives_broadcast() {
        let hub = hub();
        let mut bus_rx = hub.bus.subscribe();
        let (anon, mut rx) = hub.connect(None);

        hub.handle_client(anon, r#"{"op":"join","channel":"broadcast:lobby","ref":1}"#)
            .await;
        match rx.try_recv() {
            Ok(ServerMsg::Joined { channel, r#ref }) => {
                assert_eq!(channel, "broadcast:lobby");
                assert_eq!(r#ref, Some(1));
            }
            other => panic!("anon must JOIN a scope-none topic: {other:?}"),
        }

        // And it receives a broadcast on that public topic (whole delivery seam).
        hub.publish_from_server("lobby", serde_json::json!({ "tick": 42 }))
            .await
            .expect("a scope-none topic is server-publishable");
        hub.deliver(bus_rx.recv().await.expect("bus carries the publish"));
        match rx.try_recv() {
            Ok(ServerMsg::Event { channel, payload }) => {
                assert_eq!(channel, "broadcast:lobby");
                assert_eq!(payload["tick"], 42);
            }
            other => panic!("anon must RECEIVE the scope-none broadcast: {other:?}"),
        }
    }

    /// No escalation: the same `None` principal is REJECTED from a scope-`auth`
    /// topic (JC0401 "authentication required") and a scope-`tenant` topic
    /// (JC0403 "tenant membership required") — it reaches ONLY scope-`none`.
    #[tokio::test]
    async fn anonymous_rejected_from_scope_auth_and_tenant() {
        let hub = hub();
        let (anon, mut rx) = hub.connect(None);

        hub.handle_client(
            anon,
            r#"{"op":"join","channel":"broadcast:events","ref":2}"#,
        )
        .await;
        match rx.try_recv() {
            Ok(ServerMsg::Error { code, channel, .. }) => {
                assert_eq!(code, "JC0401", "scope-auth rejects a None principal");
                assert_eq!(channel.as_deref(), Some("broadcast:events"));
            }
            other => panic!("anon must be REJECTED from a scope-auth topic: {other:?}"),
        }

        hub.handle_client(anon, r#"{"op":"join","channel":"broadcast:room","ref":3}"#)
            .await;
        match rx.try_recv() {
            Ok(ServerMsg::Error { code, channel, .. }) => {
                assert_eq!(code, "JC0403", "scope-tenant rejects a None principal");
                assert_eq!(channel.as_deref(), Some("broadcast:room"));
            }
            other => panic!("anon must be REJECTED from a scope-tenant topic: {other:?}"),
        }
    }

    /// A valid credential (`Some(principal)` with a tenant) is unchanged: it joins
    /// scope-`none`, scope-`auth`, and scope-`tenant` alike.
    #[tokio::test]
    async fn authenticated_tenant_principal_joins_every_scope() {
        let hub = hub();
        let (user, mut rx) = hub.connect(Some(tenant_user()));
        for (channel, join_ref) in [
            ("broadcast:lobby", 1),
            ("broadcast:events", 2),
            ("broadcast:room", 3),
        ] {
            hub.handle_client(
                user,
                &format!(r#"{{"op":"join","channel":"{channel}","ref":{join_ref}}}"#),
            )
            .await;
            match rx.try_recv() {
                Ok(ServerMsg::Joined { channel: c, .. }) => assert_eq!(c, channel),
                other => panic!("an authenticated tenant principal must join {channel}: {other:?}"),
            }
        }
    }
}
