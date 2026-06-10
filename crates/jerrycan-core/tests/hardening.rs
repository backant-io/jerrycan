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
