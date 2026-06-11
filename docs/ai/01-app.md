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

Background tasks run for the lifetime of `serve` via `on_serve`. The closure gets
a `TaskContext` (resolves app-level `provide`d deps) and a shutdown watch that
flips `true` when serving stops; the task shares the 10s drain budget. `into_test`
deliberately does NOT run them — drive task logic directly in tests:
```rust
# use jerrycan::prelude::*;
# use std::sync::Arc;
# use std::sync::atomic::{AtomicBool, Ordering};
# fn main() { tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(async {
let started = Arc::new(AtomicBool::new(false));
let flag = started.clone();
let app = App::new()
    .route("/ping", get(|| async { "pong" }))
    .on_serve("warmup", move |_deps, mut shutdown| async move {
        flag.store(true, Ordering::SeqCst);   // started-flag: task is live
        let _ = shutdown.changed().await;      // park until shutdown begins
    });

let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
let (tx, rx) = tokio::sync::oneshot::channel::<()>();
let server = tokio::spawn(app.serve_with_shutdown(listener, async { let _ = rx.await; }));
while !started.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
assert!(started.load(Ordering::SeqCst));       // the task started under serve
tx.send(()).unwrap();                          // trigger graceful shutdown
server.await.unwrap().unwrap();                // drain completes before return
# }); }
```

Domain time is injectable: handlers and tasks take `Dep<Clock>` and call `now()`;
tests move it with `TestApp::clock().advance(..)` / `.set(..)` and observe the
effect through real requests. (The serve engine's own timeouts stay on real
time — `Clock` is for rate windows, schedules, and expiry, not transport.)
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn now_ms(clock: Dep<Clock>) -> Result<Json<u128>> {
    Ok(Json(clock.now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()))
}
let t = App::new().route("/now", get(now_ms)).into_test();
let t0: u128 = t.get("/now").await.json();
t.clock().advance(std::time::Duration::from_secs(3600));   // jump the injected clock 1h
let t1: u128 = t.get("/now").await.json();
assert!(t1 >= t0 + 3_600_000);                              // the handler saw the jump
# }); }
```

Need a dependency OUTSIDE a request — startup wiring, a background job, a CLI
command? `BuiltApp::task_context()` (and `TestApp::task_context()`) resolves
**app-level** `provide`/`provide_dep` deps with no request in flight; the
`on_serve` closure above receives exactly such a `TaskContext`. Only app-level
providers are in scope, and a factory that pulls an HTTP extractor
(`Json`/`Path`/`Query`/`Headers`) fails `JC1003` — those need a real request.

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

The 1 MiB body cap is per-route: a single upload route can raise it with
`.body_limit(n)` without loosening anything else. Routing is decided BEFORE the
body is read, so an unknown path (`404`) or wrong method (`405`) never drains an
oversized body; a matched route over its limit is `413`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn ok() -> Result<NoContent> { Ok(NoContent) }

let t = App::new()
    .route("/small", post(ok))                           // 1 MiB default
    .route("/big", post(ok).body_limit(4 * 1024 * 1024)) // 4 MiB for this route only
    .into_test();

let two_mib = vec![b'x'; 2 * 1024 * 1024];
assert_eq!(t.post_bytes("/big", &two_mib).await.status().as_u16(), 204);   // under 4 MiB
assert_eq!(t.post_bytes("/small", &two_mib).await.status().as_u16(), 413); // over 1 MiB
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
