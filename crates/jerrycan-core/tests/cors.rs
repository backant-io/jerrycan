use jerrycan_core::prelude::*;

fn app() -> TestApp {
    App::new()
        .cors(CorsConfig::new(CorsOrigins::list(["https://app.example"])))
        .route(
            "/todos",
            get(|| async { Json(vec![1, 2, 3]) }).post(|| async { NoContent }),
        )
        .into_test()
}

#[tokio::test]
async fn preflight_to_a_known_path_is_204_with_allowed_methods_not_405() {
    let t = app();
    let res = t
        .options_with(
            "/todos",
            &[
                ("origin", "https://app.example"),
                ("access-control-request-method", "POST"),
            ],
        )
        .await;
    assert_eq!(
        res.status().as_u16(),
        204,
        "preflight must be answered, not 405"
    );
    assert_eq!(
        res.headers()["access-control-allow-origin"],
        "https://app.example"
    );
    let methods = res.headers()["access-control-allow-methods"]
        .to_str()
        .unwrap();
    assert!(
        methods.contains("GET") && methods.contains("POST"),
        "{methods}"
    );
    assert_eq!(res.headers()["vary"], "Origin");
}

#[tokio::test]
async fn preflight_from_a_disallowed_origin_gets_no_cors_headers() {
    let t = app();
    let res = t
        .options_with(
            "/todos",
            &[
                ("origin", "https://evil.example"),
                ("access-control-request-method", "POST"),
            ],
        )
        .await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn options_without_cors_config_still_405s() {
    let t = App::new()
        .route("/todos", get(|| async { NoContent }))
        .into_test();
    let res = t
        .options_with(
            "/todos",
            &[
                ("origin", "https://x"),
                ("access-control-request-method", "GET"),
            ],
        )
        .await;
    assert_eq!(res.status().as_u16(), 405);
}

#[tokio::test]
async fn preflight_reflects_request_headers_and_max_age_when_configured() {
    let t = App::new()
        .cors(
            CorsConfig::new(CorsOrigins::list(["https://app.example"]))
                .max_age(std::time::Duration::from_secs(600)),
        )
        .route("/todos", get(|| async { NoContent }))
        .into_test();
    let res = t
        .options_with(
            "/todos",
            &[
                ("origin", "https://app.example"),
                ("access-control-request-method", "GET"),
                ("access-control-request-headers", "x-custom, authorization"),
            ],
        )
        .await;
    assert_eq!(res.status().as_u16(), 204);
    assert_eq!(
        res.headers()["access-control-allow-headers"],
        "x-custom, authorization"
    );
    assert_eq!(res.headers()["access-control-max-age"], "600");
}

#[tokio::test]
async fn actual_cross_origin_response_carries_allow_origin() {
    let t = app(); // the existing helper: cors allowlist ["https://app.example"], /todos GET+POST
    let res = t
        .get_with("/todos", &[("origin", "https://app.example")])
        .await;
    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(
        res.headers()["access-control-allow-origin"],
        "https://app.example"
    );
    assert_eq!(res.headers()["vary"], "Origin");
}

#[tokio::test]
async fn cross_origin_404_still_carries_allow_origin() {
    let t = app();
    let res = t
        .get_with("/nope", &[("origin", "https://app.example")])
        .await;
    assert_eq!(res.status().as_u16(), 404);
    assert_eq!(
        res.headers()["access-control-allow-origin"],
        "https://app.example",
        "browser must see the 404, so CORS headers ride even on rejects"
    );
}

#[tokio::test]
async fn disallowed_origin_gets_no_allow_origin() {
    let t = app();
    let res = t
        .get_with("/todos", &[("origin", "https://evil.example")])
        .await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn same_origin_request_is_undecorated() {
    let t = app();
    let res = t.get("/todos").await; // no Origin header
    assert!(res.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn handler_set_allow_origin_is_not_clobbered() {
    // a handler that sets its own Access-Control-Allow-Origin must win (insert-if-absent)
    use jerrycan_core::{Response, http};
    async fn custom() -> Response {
        let mut r = NoContent.into_response();
        r.headers_mut().insert(
            "access-control-allow-origin",
            http::HeaderValue::from_static("https://override.example"),
        );
        r
    }
    let t = App::new()
        .cors(CorsConfig::new(CorsOrigins::list(["https://app.example"])))
        .route("/x", get(custom))
        .into_test();
    let res = t.get_with("/x", &[("origin", "https://app.example")]).await;
    assert_eq!(
        res.headers()["access-control-allow-origin"],
        "https://override.example"
    );
}

#[tokio::test]
async fn credentials_and_expose_headers_appear_when_configured() {
    let t = App::new()
        .cors(
            CorsConfig::new(CorsOrigins::list(["https://app.example"]))
                .allow_credentials(true)
                .expose_headers(["x-total-count"]),
        )
        .route("/todos", get(|| async { NoContent }))
        .into_test();
    let res = t
        .get_with("/todos", &[("origin", "https://app.example")])
        .await;
    assert_eq!(res.headers()["access-control-allow-credentials"], "true");
    assert_eq!(
        res.headers()["access-control-expose-headers"],
        "x-total-count"
    );
}
