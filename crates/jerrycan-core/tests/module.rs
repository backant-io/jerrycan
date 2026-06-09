//! Spec §4.2 acceptance: nested subroutes, module-scoped deps shadowing,
//! module-scoped middleware short-circuiting.

use jerrycan_core::{
    App, Dep, IntoResponse, Json, Middleware, MiddlewareFuture, Module, Next, RequestCtx, get,
};

struct Flavor(&'static str);

async fn flavor(f: Dep<Flavor>) -> Json<String> {
    Json(f.0.to_string())
}

#[tokio::test]
async fn subroutes_nest_and_module_deps_shadow_app_deps() {
    let comments = Module::new("comments")
        .provide(Flavor("comment-scope"))
        .route("/", get(flavor));
    let todos = Module::new("todos")
        .route("/flavor", get(flavor))
        .mount("/{id}/comments", comments);
    let t = App::new()
        .provide(Flavor("app-scope"))
        .mount("/todos", todos)
        .into_test();

    assert_eq!(t.get("/todos/flavor").await.json::<String>(), "app-scope");
    assert_eq!(
        t.get("/todos/7/comments/").await.json::<String>(),
        "comment-scope"
    );
}

struct Deny;
impl Middleware for Deny {
    fn handle<'a>(&'a self, _ctx: &'a mut RequestCtx, _next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async {
            jerrycan_core::Error::new(http::StatusCode::FORBIDDEN, "JC0403", "denied")
                .into_response()
        })
    }
}

#[tokio::test]
async fn module_middleware_short_circuits_only_its_subtree() {
    let locked = Module::new("locked")
        .middleware(Deny)
        .route("/", get(|| async { "secret" }));
    let t = App::new()
        .route("/open", get(|| async { "open" }))
        .mount("/locked", locked)
        .into_test();

    assert_eq!(t.get("/open").await.status(), http::StatusCode::OK);
    assert_eq!(
        t.get("/locked/").await.status(),
        http::StatusCode::FORBIDDEN
    );
}
