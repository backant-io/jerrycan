//! WebSocket transport: RFC 6455 handshake over hyper's HTTP/1 upgrade, then
//! tokio-tungstenite (Role::Server) over the upgraded socket.

use jerrycan_core::http::HeaderMap;
use jerrycan_core::{Error, Result};

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

/// Extractor: validate the WS handshake, authenticate, and claim the upgrade
/// handle — all BEFORE replying, so a bad handshake/credential is a plain
/// error response and never a half-open upgrade.
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
        // Auth BEFORE upgrade: a bad credential is a plain 401 response.
        let principal = match handle.resolver.as_ref() {
            Some(r) => Some(r(ctx).await?),
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
                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    io,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    None,
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
