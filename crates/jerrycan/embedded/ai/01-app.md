# App

## Purpose
`App` assembles a backend: register app-level dependencies, mount modules, serve.
In generated projects this file is machine-written (`crates/app/src/main.rs`) — you
rarely edit it; you generate modules instead (see 02-modules).

## Signature
```rust,no_run
# use jerrycan::prelude::*;
# struct AppConfig { greeting: &'static str }
# async fn noop() -> Result<()> {
App::new()
    .provide(AppConfig { greeting: "hi" })   // .provide(value) — app-wide singleton dependency
    .route("/ping", get(|| async { "pong" }))   // app-level route (prefer modules)
    .serve()                     // binds JERRYCAN_ADDR (default 127.0.0.1:8000)
    .await
# }
```

## Minimal example
```rust,no_run
use jerrycan::prelude::*;

#[jerrycan::main]
async fn main() -> Result<()> {
    App::new()
        .route("/ping", get(|| async { "pong" }))
        .serve()
        .await
}
```

## Variations
Mount modules (the normal shape of a generated app):
```rust
# use jerrycan::prelude::*;
let app = App::new()
    .mount("/todos", Module::new("todos").route("/", get(|| async { "list" })));
# let _ = app.into_test();
```

App-level middleware wraps every route (outermost ring):
```rust
# use jerrycan::prelude::*;
# struct Noop;
# impl Middleware for Noop {
#     fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
#         next.run(ctx)
#     }
# }
let app = App::new().middleware(Noop).route("/x", get(|| async { "x" }));
# let _ = app.into_test();
```

Bind explicitly (tests, port 0, socket activation) with `serve_with`; plain
`serve()` reads `JERRYCAN_ADDR` (default `127.0.0.1:8000`):
```rust,no_run
# use jerrycan::prelude::*;
# async fn demo() -> Result<()> {
let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
    .map_err(|e| Error::internal(format!("bind: {e}")))?;
App::new().route("/ping", get(|| async { "pong" })).serve_with(listener).await
# }
```

Secure defaults are ON for every response — security headers
(`x-content-type-options`, `x-frame-options`, `referrer-policy`,
`content-security-policy`, `cache-control: no-store`), a 30s per-request budget
(middleware + handler, `503 JC0503`), a 30s body-read timeout, a 1 MiB body cap,
and graceful shutdown on Ctrl-C. Opting out is explicit:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let t = App::new().route("/", get(|| async { "ok" })).into_test();
assert_eq!(t.get("/").await.headers()["x-content-type-options"], "nosniff");

let bare = App::new()
    .route("/", get(|| async { "ok" }))
    .security_headers(false)                                  // explicit opt-out
    .handler_timeout(std::time::Duration::from_secs(120))     // explicit re-budget
    .into_test();
assert!(bare.get("/").await.headers().get("x-frame-options").is_none());
# }); }
```

## Errors you'll hit
- Duplicate or ambiguous routes fail at **build/serve time**, not request time —
  `serve()` returns `Err` describing the conflicting path. Fix the route table;
  never work around it with ordering.
- `JC0404`/`JC0405` are produced automatically for unknown paths / known path,
  wrong method. You don't write those handlers.

## Anti-patterns
- Don't hand-edit a generated `main.rs` to add routes — generate a module
  (`jerrycan generate route <name>`); mounting is regenerated deterministically.
- Don't register many app-level routes; modules are the unit of structure,
  testing, and ownership.
