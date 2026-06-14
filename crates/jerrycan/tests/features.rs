//! The facade's feature-gated extension re-exports: generated apps depend on
//! `jerrycan = { features = ["db", "validate"] }` and import `jerrycan::db::…`.
//! This test file only compiles its bodies when the features are on — the
//! LOCAL gate runs with --all-features; CI gains --all-features when the
//! Phase 2 exit lands (Task 13).

#[cfg(feature = "db")]
#[tokio::test]
async fn db_reexport_is_usable() {
    use jerrycan::db::sea_orm::ConnectionTrait;
    let db = jerrycan::db::Db::connect("sqlite::memory:").await.unwrap();
    assert_eq!(db.backend(), jerrycan::db::Backend::Sqlite);
    // The sea-orm connection rides along for generated repos:
    db.conn()
        .execute_unprepared("CREATE TABLE t (x BIGINT)")
        .await
        .unwrap();
}

#[cfg(feature = "validate")]
#[test]
fn validate_reexport_is_usable() {
    let v = jerrycan::validate::Violation::new("f", "m");
    assert_eq!(v.field, "f");
}

#[cfg(feature = "auth")]
#[test]
fn auth_reexport_is_usable() {
    let hash = jerrycan::auth::hash_password("pw").unwrap();
    assert!(jerrycan::auth::verify_password("pw", &hash).unwrap());
}

#[cfg(feature = "observe")]
#[test]
fn observe_reexport_is_usable() {
    let m = jerrycan::observe::Metrics::new();
    m.record(200, 0.01);
    assert!(m.render().contains("jerrycan_requests_total"));
}

// The jobs engine reaches generated apps through the facade: the generated
// `crates/jobs` crate writes `jerrycan::jobs::Jobs::postgres(db)…`,
// `jerrycan::jobs::JOBS_MIGRATIONS`, and `jerrycan::jobs::JobFuture`. This pins
// that those paths resolve through the facade alone (the `jobs` feature implies
// `db`, so `jerrycan::db::Migration` is also in scope here).
#[cfg(feature = "jobs")]
#[test]
fn jobs_reexport_is_usable() {
    // The migrations constant + the builder are the surface generated registries
    // depend on. `Jobs::in_memory` builds without a live db; the real generated
    // wiring uses `Jobs::postgres(db)`, exercised by the genroute_compile gate.
    let _migrations: &[jerrycan::db::Migration] = jerrycan::jobs::JOBS_MIGRATIONS;
    let _jobs = jerrycan::jobs::Jobs::in_memory().queue("default", 4);
}

// The rate-limit extension and CORS config both reach generated apps through
// the facade: `jerrycan::ratelimit::RateLimit` (the optional sub-crate) and
// `jerrycan::{CorsConfig, CorsOrigins}` (from core via the glob re-export).
// This pins that `App::new().cors(..).extend(RateLimit::per_window(..))`
// composes through the facade alone — the path generated code writes.
#[cfg(feature = "rate-limit")]
#[test]
fn rate_limit_and_cors_reach_through_the_facade() {
    use jerrycan::ratelimit::RateLimit;
    use jerrycan::{App, CorsConfig, CorsOrigins};
    use std::time::Duration;

    let _app = App::new()
        .cors(CorsConfig::new(CorsOrigins::any()))
        .extend(RateLimit::per_window(100, Duration::from_secs(60)));
}
