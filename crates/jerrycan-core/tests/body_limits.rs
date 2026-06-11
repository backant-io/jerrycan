//! Two-phase read (spec §4.4): route BEFORE body, per-route body limits.
//! Routing wins over the body cap — an unmatched path is rejected without
//! the body ever being read; a matched route's `.body_limit` overrides the
//! 1 MiB default for that route only.

use jerrycan_core::{App, NoContent, get, post};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn ok() -> jerrycan_core::Result<NoContent> {
    Ok(NoContent)
}

// --- TestApp path: the limits hold in-process, not just over a socket. ---

#[tokio::test]
async fn per_route_body_limit_overrides_the_default() {
    let t = App::new()
        .route("/small", post(ok))
        .route("/big", post(ok).body_limit(4 * 1024 * 1024))
        .into_test();

    let big = vec![b'x'; 2 * 1024 * 1024]; // 2 MiB: over the 1 MiB default, under 4 MiB.
    assert_eq!(
        t.post_bytes("/big", &big).await.status().as_u16(),
        204,
        "the per-route 4 MiB limit admits a 2 MiB body"
    );
    assert_eq!(
        t.post_bytes("/small", &big).await.status().as_u16(),
        413,
        "the default 1 MiB limit rejects the same 2 MiB body"
    );
}

#[tokio::test]
async fn unmatched_routes_reject_before_reading_the_body() {
    let t = App::new()
        .route(
            "/x",
            get(|| async { Ok::<_, jerrycan_core::Error>(NoContent) }),
        )
        .into_test();

    let big = vec![b'x'; 8 * 1024 * 1024]; // way over every limit.
    let res = t.post_bytes("/nope", &big).await;
    assert_eq!(
        res.status().as_u16(),
        404,
        "routing wins over the body cap: a 404 path never reads the body"
    );
}

#[tokio::test]
async fn method_mismatch_rejects_before_reading_the_body() {
    let t = App::new().route("/x", get(ok)).into_test();
    let big = vec![b'x'; 8 * 1024 * 1024];
    let res = t.post_bytes("/x", &big).await;
    assert_eq!(
        res.status().as_u16(),
        405,
        "405 is decided by routing, before the oversize body is read"
    );
}

// --- Live serve path: the same policy holds over a real socket. ---

async fn post_raw(addr: &str, path: &str, body: &[u8]) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: l\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body).await; // server may reset mid-write once the cap is hit.
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn live_per_route_limit_is_enforced_over_the_socket() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new()
        .route("/small", post(ok))
        .route("/big", post(ok).body_limit(4 * 1024 * 1024));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let big = vec![b'x'; 2 * 1024 * 1024];
    let on_big = post_raw(&addr, "/big", &big).await;
    assert!(on_big.starts_with("HTTP/1.1 204"), "got: {on_big}");
    let on_small = post_raw(&addr, "/small", &big).await;
    assert!(on_small.contains("413"), "got: {on_small}");

    server.abort();
}

#[tokio::test]
async fn live_unmatched_route_is_404_before_the_body_is_read() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new().route("/x", get(ok));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let big = vec![b'x'; 8 * 1024 * 1024]; // over every limit; must still 404.
    let res = post_raw(&addr, "/nope", &big).await;
    assert!(
        res.contains("404"),
        "routing wins over the cap: {}",
        &res[..res.len().min(120)]
    );

    server.abort();
}
