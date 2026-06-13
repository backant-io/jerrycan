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
async fn api_key_partition_beats_ip() {
    let t = app(1);
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
async fn options_is_exempt() {
    let t = app(1);
    let ip: std::net::SocketAddr = "203.0.113.2:9".parse().unwrap();
    assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
    let opt = t
        .request_from(http::Method::OPTIONS, "/ping", &[], ip)
        .await;
    assert_ne!(
        opt.status().as_u16(),
        429,
        "OPTIONS must bypass rate limiting"
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
