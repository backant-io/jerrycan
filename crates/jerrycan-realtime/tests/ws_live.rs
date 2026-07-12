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

#[tokio::test(flavor = "multi_thread")]
async fn join_heartbeat_and_error_envelopes_round_trip() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;

    let mut ws = connect(port).await;
    send_text(&mut ws, r#"{"op":"join","channel":"broadcast:lobby","ref":1}"#).await;
    let joined = recv_json(&mut ws).await;
    assert_eq!(joined["op"], "joined");
    assert_eq!(joined["channel"], "broadcast:lobby");
    assert_eq!(joined["ref"], 1);

    send_text(&mut ws, r#"{"op":"heartbeat","ref":2}"#).await;
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["op"], "heartbeat_ack");

    // Unknown channel ⇒ error envelope, connection stays up.
    send_text(&mut ws, r#"{"op":"join","channel":"broadcast:ghost","ref":3}"#).await;
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
