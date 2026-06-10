//! Spec §4.4 secure defaults: headers, timeouts, panic containment, shutdown.

use jerrycan_core::{App, Json, NoContent, get};

#[tokio::test]
async fn security_headers_are_on_every_response_including_errors() {
    let t = App::new()
        .route("/ok", get(|| async { Json(1) }))
        .into_test();

    for path in ["/ok", "/missing"] {
        let res = t.get(path).await;
        let h = res.headers();
        assert_eq!(h["x-content-type-options"], "nosniff", "{path}");
        assert_eq!(h["x-frame-options"], "DENY", "{path}");
        assert_eq!(h["referrer-policy"], "no-referrer", "{path}");
        assert_eq!(h["content-security-policy"], "default-src 'none'", "{path}");
        assert_eq!(h["cache-control"], "no-store", "{path}");
    }
}

#[tokio::test]
async fn handler_set_headers_win_over_defaults() {
    async fn cached() -> jerrycan_core::Response {
        let mut res = jerrycan_core::IntoResponse::into_response("ok");
        res.headers_mut().insert(
            jerrycan_core::http::header::CACHE_CONTROL,
            jerrycan_core::http::HeaderValue::from_static("max-age=60"),
        );
        res
    }
    let t = App::new().route("/", get(cached)).into_test();
    let res = t.get("/").await;
    assert_eq!(res.headers()["cache-control"], "max-age=60", "handler wins");
    assert_eq!(
        res.headers()["x-frame-options"],
        "DENY",
        "others still applied"
    );
}

#[tokio::test]
async fn security_headers_can_be_explicitly_disabled() {
    let t = App::new()
        .route("/", get(|| async { NoContent }))
        .security_headers(false)
        .into_test();
    let res = t.get("/").await;
    assert!(res.headers().get("x-frame-options").is_none());
}

use std::time::Duration;

#[tokio::test]
async fn slow_handlers_hit_the_timeout_with_jc0503() {
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_secs(5)).await;
        "too late"
    }
    let t = App::new()
        .route("/slow", get(slow))
        .handler_timeout(Duration::from_millis(50))
        .into_test();
    let res = t.get("/slow").await;
    assert_eq!(
        res.status(),
        jerrycan_core::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(res.text().contains("JC0503"), "{}", res.text());
}

#[tokio::test]
async fn fast_handlers_are_unaffected_by_the_default_timeout() {
    let t = App::new().route("/", get(|| async { "quick" })).into_test();
    assert_eq!(t.get("/").await.text(), "quick");
}

use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn raw_get(addr: &str, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn handler_panics_become_500_and_the_server_survives() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new()
        .route(
            "/boom",
            get(|| async {
                if true {
                    panic!("kaboom")
                }
                "x"
            }),
        )
        .route("/fine", get(|| async { "still here" }));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let res = raw_get(&addr, "/boom").await;
    assert!(
        res.starts_with("HTTP/1.1 500"),
        "panic must become a 500: {res}"
    );
    assert!(res.contains("JC0500"), "{res}");

    let res = raw_get(&addr, "/fine").await;
    assert!(
        res.starts_with("HTTP/1.1 200") && res.ends_with("still here"),
        "server must survive: {res}"
    );
    server.abort();
}
