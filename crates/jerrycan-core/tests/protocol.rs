//! Live-socket protocol proofs: behaviors only a real connection can show
//! (write stalls, chunked transfer, mid-stream aborts).

use jerrycan_core::{App, StreamBody, get};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Streams 64 KiB chunks forever (until the client goes away). A stalled
/// reader lets the kernel write buffer fill, after which hyper's socket write
/// blocks — the case `write_stall_timeout` exists to catch.
async fn endless() -> StreamBody {
    let (body, tx) = StreamBody::channel();
    tokio::spawn(async move {
        let chunk = vec![b'x'; 64 * 1024];
        while tx.send(chunk.clone()).await {}
    });
    body
}

/// Streams exactly three chunks then drops the sender (clean EOF). Proves a
/// prompt reader gets the full body and is NOT disconnected by `TimedIo`.
async fn three() -> StreamBody {
    let (body, tx) = StreamBody::channel();
    tokio::spawn(async move {
        for _ in 0..3 {
            tx.send(vec![b'y'; 1024]).await;
        }
    });
    body
}

/// Sum the payload bytes of a chunked-transfer body. Walks `<hexlen>\r\n<data>\r\n`
/// frames until the terminating `0` chunk, returning total decoded bytes — so
/// chunk-size lines never get miscounted as payload.
fn decode_chunked(mut body: &str) -> usize {
    let mut total = 0;
    while let Some((len_line, rest)) = body.split_once("\r\n") {
        let len = usize::from_str_radix(len_line.trim(), 16).expect("valid chunk size");
        if len == 0 {
            break;
        }
        total += len;
        // Skip the chunk data and its trailing CRLF.
        body = &rest[len + 2..];
    }
    total
}

#[tokio::test]
async fn stalled_reader_is_disconnected_after_write_stall_cap() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new()
        .route("/endless", get(endless))
        .write_stall_timeout(Duration::from_millis(500));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    // The whole observation must finish well inside this cap: the server is
    // expected to drop the connection ~500ms after the buffers fill, so a 10s
    // outer cap firing means TimedIo never disconnected the stalled reader.
    let observe = async {
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        s.write_all(b"GET /endless HTTP/1.1\r\nhost: t\r\n\r\n")
            .await
            .unwrap();

        // Read a first nonzero chunk: the stream has started flowing.
        let mut buf = [0u8; 8 * 1024];
        let n = s.read(&mut buf).await.unwrap();
        assert!(n > 0, "the stream must start before we stall");

        // STOP reading for ~3s. The producer keeps pushing 64 KiB chunks, the
        // kernel write buffer fills, hyper's socket write goes Pending, and the
        // 500ms stall cap must fire and drop the connection.
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Resume reading: drain whatever the kernel buffered, then expect the
        // socket to close (Ok(0)) or error — the loop must END, not hang.
        loop {
            match s.read(&mut buf).await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(10), observe)
        .await
        .expect("server must drop the stalled reader, not hang on the write");

    server.abort();
}

#[tokio::test]
async fn prompt_reader_gets_the_full_stream_and_is_not_disconnected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new()
        .route("/three", get(three))
        .write_stall_timeout(Duration::from_millis(500));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let read_all = async {
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        s.write_all(b"GET /three HTTP/1.1\r\nhost: t\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };

    let raw = tokio::time::timeout(Duration::from_secs(10), read_all)
        .await
        .expect("a prompt reader must never be disconnected by the stall cap");

    assert!(
        raw.starts_with("HTTP/1.1 200"),
        "got: {}",
        &raw[..raw.len().min(64)]
    );
    // Decode the chunked body (everything after the blank line). The payload is
    // 3 chunks of 1 KiB 'y'; the decoded length proves the full stream arrived
    // without counting chunk-size lines as payload.
    let body = raw.split_once("\r\n\r\n").expect("response has headers").1;
    let payload_len = decode_chunked(body);
    assert_eq!(
        payload_len,
        3 * 1024,
        "the full streamed payload must arrive"
    );

    server.abort();
}
