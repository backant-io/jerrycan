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
        .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")));
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
    Ok(tokio_tungstenite::tungstenite::handshake::derive_accept_key(
        key.as_bytes(),
    ))
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
        list.insert("connection", HeaderValue::from_static("keep-alive, Upgrade"));
        assert!(handshake_accept(&list).is_ok());
    }
}
