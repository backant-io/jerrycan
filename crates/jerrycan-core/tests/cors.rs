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
