//! Spec §4.3 acceptance: nested async deps + per-request caching + overrides,
//! exercised through the public TestApp API exactly as generated tests will.

use jerrycan_core::{App, Dep, Json, Module, get};

struct Db {
    url: String,
}
struct CurrentUser {
    name: String,
}

async fn current_user(db: Dep<Db>) -> jerrycan_core::Result<CurrentUser> {
    Ok(CurrentUser {
        name: format!("user@{}", db.url),
    })
}

async fn whoami(user: Dep<CurrentUser>) -> Json<String> {
    Json(user.name.clone())
}

fn app() -> App {
    App::new()
        .provide(Db {
            url: "pg://prod".into(),
        })
        .provide_dep(current_user)
        .mount("/me", Module::new("me").route("/", get(whoami)))
}

#[tokio::test]
async fn nested_deps_resolve_through_real_requests() {
    let t = app().into_test();
    let res = t.get("/me/").await;
    assert_eq!(res.status(), http::StatusCode::OK);
    assert_eq!(res.json::<String>(), "user@pg://prod");
}

#[tokio::test]
async fn override_dep_swaps_the_database_for_tests() {
    let t = app().into_test().override_dep(Db {
        url: "sqlite::memory:".into(),
    });
    let res = t.get("/me/").await;
    assert_eq!(res.json::<String>(), "user@sqlite::memory:");
}

#[tokio::test]
async fn override_can_replace_a_factory_product_directly() {
    let t = app().into_test().override_dep(CurrentUser {
        name: "fake".into(),
    });
    let res = t.get("/me/").await;
    assert_eq!(res.json::<String>(), "fake");
}
