//! Live-socket protocol proofs: behaviors only a real connection can show
//! (write stalls, chunked transfer, mid-stream aborts).

use jerrycan_core::{App, Json, Multipart, Result, StreamBody, get, post};
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

/// Echoes a JSON body on a `.stream_body()` route: `Json` drains the live
/// stream lane (hyper Incoming → Limited → TimedRecvBody → drain) transparently.
async fn echo(Json(v): Json<serde_json::Value>) -> Result<Json<serde_json::Value>> {
    Ok(Json(v))
}

#[tokio::test]
async fn streamed_request_body_drains_over_a_real_socket_when_written_in_dribbles() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new().route("/up", post(echo).stream_body().body_limit(1024));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    // The whole JSON body, written across 3 separate flushed writes with small
    // gaps — the server must reassemble the frames off the wire and echo it.
    let body = br#"{"hello":"streamed world"}"#;
    let (a, rest) = body.split_at(8);
    let (b, c) = rest.split_at(9);

    let exchange = async {
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let head = format!(
            "POST /up HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s.flush().await.unwrap();
        for chunk in [a, b, c] {
            // Dribble: write a slice, flush, pause well under the 30s per-frame
            // read deadline so TimedRecvBody resets instead of firing.
            s.write_all(chunk).await.unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };

    let raw = tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .expect("the dribbled streamed body must be drained and echoed, not hang");

    assert!(
        raw.starts_with("HTTP/1.1 200"),
        "got: {}",
        &raw[..raw.len().min(80)]
    );
    let resp_body = raw.split_once("\r\n\r\n").expect("response has headers").1;
    let echoed: serde_json::Value =
        serde_json::from_str(resp_body.trim()).expect("echoed JSON body");
    assert_eq!(echoed, serde_json::json!({"hello": "streamed world"}));

    server.abort();
}

/// Collects `(name, text)` pairs from a streamed multipart body. The whole
/// point: the parser reassembles parts off frames the client dribbles 5 bytes
/// at a time, with no part ever arriving whole in a single frame.
async fn upload(mut mp: Multipart) -> Result<Json<Vec<(String, String)>>> {
    let mut out = Vec::new();
    while let Some(part) = mp.next_part().await? {
        let name = part.name().to_string();
        let text = part.text().await?;
        out.push((name, text));
    }
    Ok(Json(out))
}

#[tokio::test]
async fn chunked_multipart_upload_over_a_live_socket() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new().route("/upload", post(upload).stream_body().body_limit(4096));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    // A two-field multipart body with boundary `B`. Dribbled 5 bytes at a time,
    // every boundary/header/data straddle crosses a frame edge off the wire.
    let body =
        "--B\r\ncontent-disposition: form-data; name=\"a\"\r\n\r\nhello\r\n--B--\r\n".to_string();

    let exchange = async {
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let head = format!(
            "POST /upload HTTP/1.1\r\nHost: l\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s.flush().await.unwrap();
        for chunk in body.as_bytes().chunks(5) {
            // Dribble: 5 bytes, flush, pause well under the per-frame read
            // deadline so the parser must reassemble across frames.
            s.write_all(chunk).await.unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };

    let raw = tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .expect("the dribbled multipart body must be parsed and echoed, not hang");

    assert!(
        raw.starts_with("HTTP/1.1 200"),
        "got: {}",
        &raw[..raw.len().min(80)]
    );
    let resp_body = raw.split_once("\r\n\r\n").expect("response has headers").1;
    let echoed: Vec<(String, String)> =
        serde_json::from_str(resp_body.trim()).expect("echoed pairs");
    assert_eq!(echoed, vec![("a".to_string(), "hello".to_string())]);

    server.abort();
}
