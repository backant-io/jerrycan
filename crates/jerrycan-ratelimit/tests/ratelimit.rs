use jerrycan_core::prelude::*;
use jerrycan_ratelimit::RateLimit;
use std::time::Duration;

fn app(limit: u32) -> TestApp {
    App::new()
        .extend(RateLimit::per_window(limit, Duration::from_secs(60)))
        .route("/ping", get(|| async { NoContent }))
        .into_test()
}

#[tokio::test]
async fn ip_partition_trips_at_the_limit_then_resets_next_window() {
    let t = app(2);
    let ip: std::net::SocketAddr = "198.51.100.4:1111".parse().unwrap();
    assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
    assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
    let limited = t.get_from("/ping", ip).await;
    assert_eq!(limited.status().as_u16(), 429);
    assert!(limited.headers().contains_key("retry-after"));
    assert!(limited.text().contains("JC0429"));
    let ip2: std::net::SocketAddr = "198.51.100.9:2222".parse().unwrap();
    assert_eq!(t.get_from("/ping", ip2).await.status().as_u16(), 204);
    t.clock().advance(Duration::from_secs(61));
    assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
}

#[tokio::test]
async fn default_per_window_does_not_partition_on_unauthenticated_api_key() {
    // The api-key tier is OPT-IN. With the default config, a rotating x-api-key
    // from one IP must NOT mint fresh buckets — the client stays IP-limited.
    let t = app(1); // default per_window, no api-key tier configured
    let ip: std::net::SocketAddr = "198.51.100.50:7000".parse().unwrap();
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-api-key", "rotate-1")], ip)
            .await
            .status()
            .as_u16(),
        204
    );
    // a DIFFERENT x-api-key, SAME ip — still limited by IP, not a fresh bucket
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-api-key", "rotate-2")], ip)
            .await
            .status()
            .as_u16(),
        429,
        "rotating an unauthenticated api-key must NOT bypass the IP limit"
    );
}

#[tokio::test]
async fn api_key_partition_beats_ip() {
    let t = App::new()
        .extend(RateLimit::per_window(1, Duration::from_secs(60)).api_key_header("x-api-key"))
        .route("/ping", get(|| async { NoContent }))
        .into_test();
    let ip: std::net::SocketAddr = "203.0.113.1:9".parse().unwrap();
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-api-key", "alpha")], ip)
            .await
            .status()
            .as_u16(),
        204
    );
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-api-key", "beta")], ip)
            .await
            .status()
            .as_u16(),
        204
    );
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-api-key", "alpha")], ip)
            .await
            .status()
            .as_u16(),
        429
    );
}

#[tokio::test]
async fn options_is_exempt_even_when_the_budget_is_burned() {
    // /ping has BOTH get and an explicit OPTIONS handler, so OPTIONS resolves to
    // RouteMatch::Found and actually reaches the middleware (not a 405-at-routing,
    // which the old `options_is_exempt` hit before the limiter ever ran). limit=1:
    // burn the budget with a GET, then assert OPTIONS still passes (exempt), not 429.
    let t = App::new()
        .extend(RateLimit::per_window(1, Duration::from_secs(60)))
        .route(
            "/ping",
            get(|| async { NoContent }).on(http::Method::OPTIONS, || async { NoContent }),
        )
        .into_test();
    let ip: std::net::SocketAddr = "203.0.113.20:9".parse().unwrap();
    assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204); // burn budget
    assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 429); // confirm limited
    // OPTIONS to the SAME ip, budget burned — must be exempt (the middleware
    // returns next.run for OPTIONS instead of short-circuiting with a 429).
    let opt = t
        .request_from(http::Method::OPTIONS, "/ping", &[], ip)
        .await;
    assert_ne!(
        opt.status().as_u16(),
        429,
        "OPTIONS must bypass rate limiting even when the budget is exhausted"
    );
    assert_eq!(
        opt.status().as_u16(),
        204,
        "the OPTIONS handler ran (exempt, reached the handler)"
    );
}

#[tokio::test]
async fn rate_limited_429_still_carries_cors_headers() {
    // The cross-feature composition: a 429 from the rate-limit middleware must
    // still carry CORS headers, because CORS decorates at the dispatch exit AFTER
    // the middleware short-circuits. Without it, the browser masks the 429 behind
    // a CORS error and JS can't surface the rate-limit to the user.
    let t = App::new()
        .cors(CorsConfig::new(CorsOrigins::list(["https://app.example"])))
        .extend(RateLimit::per_window(1, Duration::from_secs(60)))
        .route("/ping", get(|| async { NoContent }))
        .into_test();
    let ip: std::net::SocketAddr = "203.0.113.30:9".parse().unwrap();
    // first cross-origin request OK
    assert_eq!(
        t.request_from(
            http::Method::GET,
            "/ping",
            &[("origin", "https://app.example")],
            ip
        )
        .await
        .status()
        .as_u16(),
        204
    );
    // second is 429 AND must carry Access-Control-Allow-Origin (browser must see the 429)
    let limited = t
        .request_from(
            http::Method::GET,
            "/ping",
            &[("origin", "https://app.example")],
            ip,
        )
        .await;
    assert_eq!(limited.status().as_u16(), 429);
    assert_eq!(
        limited.headers()["access-control-allow-origin"],
        "https://app.example",
        "a cross-origin 429 must carry CORS headers so the browser surfaces the rate-limit to JS"
    );
}

#[tokio::test]
async fn user_key_closure_partitions_when_configured() {
    let t = App::new()
        .extend(
            RateLimit::per_window(1, Duration::from_secs(60)).user_key(|ctx| {
                ctx.headers()
                    .get("x-user")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            }),
        )
        .route("/ping", get(|| async { NoContent }))
        .into_test();
    let ip: std::net::SocketAddr = "203.0.113.3:9".parse().unwrap();
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-user", "u1")], ip)
            .await
            .status()
            .as_u16(),
        204
    );
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-user", "u2")], ip)
            .await
            .status()
            .as_u16(),
        204
    );
    assert_eq!(
        t.request_from(http::Method::GET, "/ping", &[("x-user", "u1")], ip)
            .await
            .status()
            .as_u16(),
        429
    );
}

#[tokio::test]
async fn no_identity_fails_open() {
    // no api-key, no user_key configured, no peer addr (synthetic request via get/post, not get_from)
    let t = app(1);
    // get() sets no peer; with no identity the limiter must NOT block (fail open)
    assert_eq!(t.get("/ping").await.status().as_u16(), 204);
    assert_eq!(
        t.get("/ping").await.status().as_u16(),
        204,
        "no identity => not limited"
    );
}
