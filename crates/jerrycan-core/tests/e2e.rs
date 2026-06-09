//! Real-socket smoke test: hyper serves a built app over actual TCP.

use jerrycan_core::{App, get};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn serves_over_real_tcp() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = App::new().route("/ping", get(|| async { "pong" }));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);

    assert!(text.starts_with("HTTP/1.1 200 OK"), "got: {text}");
    assert!(text.ends_with("pong"), "got: {text}");

    server.abort();
}

#[tokio::test]
async fn oversized_bodies_are_rejected_with_413() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = App::new().route(
        "/echo",
        jerrycan_core::post(|b: jerrycan_core::Json<String>| async move { b }),
    );
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let huge = "x".repeat(2 * 1024 * 1024); // 2 MiB > 1 MiB default limit
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /echo HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        huge.len() + 2
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    // Best-effort: the server hits the 1 MiB cap and closes mid-stream, so this
    // write may fail with BrokenPipe before the full 2 MiB is sent. The 413
    // response (asserted below) is what matters, not delivering every byte.
    let _ = stream.write_all(format!("\"{huge}\"").as_bytes()).await;
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await; // server may reset after responding
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("413"), "got: {text}");
    server.abort();
}
