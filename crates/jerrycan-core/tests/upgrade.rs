//! HTTP/1 upgrade support: a handler can take hyper's OnUpgrade out of the
//! request, reply 101, and speak a raw protocol on the upgraded socket.
//! jerrycan-realtime's WebSocket transport rides exactly this seam, so this
//! test is the core-level contract (no tungstenite here — raw bytes).
use jerrycan_core::{App, Error, FromRequest, IntoResponse, RequestCtx, Response, Result, get};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Extractor that claims the upgrade handle (it is single-use and !Clone,
/// hence take, not get).
struct TakeUpgrade(hyper::upgrade::OnUpgrade);

impl FromRequest for TakeUpgrade {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        ctx.take_extension::<hyper::upgrade::OnUpgrade>()
            .map(TakeUpgrade)
            .ok_or_else(|| Error::internal("connection does not support upgrades"))
    }
}

async fn upgrade_echo(up: TakeUpgrade) -> Result<Response> {
    tokio::spawn(async move {
        if let Ok(upgraded) = up.0.await {
            let mut io = hyper_util::rt::TokioIo::new(upgraded);
            let mut buf = [0u8; 5];
            if io.read_exact(&mut buf).await.is_ok() {
                let _ = io.write_all(&buf).await;
            }
        }
    });
    let mut res = "".into_response();
    *res.status_mut() = jerrycan_core::http::StatusCode::SWITCHING_PROTOCOLS;
    res.headers_mut().insert(
        jerrycan_core::http::header::CONNECTION,
        jerrycan_core::http::HeaderValue::from_static("upgrade"),
    );
    res.headers_mut().insert(
        jerrycan_core::http::header::UPGRADE,
        jerrycan_core::http::HeaderValue::from_static("echo"),
    );
    Ok(res)
}

#[tokio::test]
async fn handler_upgrades_and_speaks_raw_bytes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = App::new().route("/up", get(upgrade_echo));
    let server = tokio::spawn(app.serve_with_shutdown(listener, async {
        let _ = rx.await;
    }));

    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /up HTTP/1.1\r\nHost: t\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n")
        .await
        .unwrap();
    // Read the response head (headers end at CRLFCRLF).
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
        assert!(head.len() < 4096, "response head too large");
    }
    let head = String::from_utf8_lossy(&head);
    assert!(
        head.starts_with("HTTP/1.1 101"),
        "expected 101, got: {head}"
    );

    // The socket now speaks the raw echo protocol.
    s.write_all(b"hello").await.unwrap();
    let mut echo = [0u8; 5];
    s.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"hello");

    let _ = tx.send(());
    let _ = server.await;
}
