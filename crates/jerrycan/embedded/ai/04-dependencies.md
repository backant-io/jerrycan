# Dependencies

## Purpose
`Dep<T>` is jerrycan's dependency injection — the signature feature. Auth,
database handles, permissions, rate limits: all reusable values resolved per
request, async, nested, memoized, and replaceable in tests.

## Signature
```rust
# use jerrycan::prelude::*;
# struct Db; struct Session; struct User { name: String }
// Register on the app (or module):
//   .provide(value)        — singleton, shared by all requests
//   .provide_dep(factory)  — async fn run at most once per request
async fn current_user(session: Dep<Session>, db: Dep<Db>) -> Result<User> {
    # let _ = (session, db);
    Ok(User { name: "ada".into() })  // factories can await I/O
}

// Consume anywhere — handlers or other factories:
async fn whoami(user: Dep<User>) -> String { user.name.clone() }
# let _ = (current_user, whoami);
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Greeting(&'static str);
async fn greet(g: Dep<Greeting>) -> String { g.0.to_string() }

let t = App::new()
    .provide(Greeting("hello"))
    .route("/", get(greet))
    .into_test();
assert_eq!(t.get("/").await.text(), "hello");
# }); }
```

## Variations
Nested factories — guards are just dependencies:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Db { url: &'static str }
struct User { name: String }
struct Admin;

async fn current_user(db: Dep<Db>) -> Result<User> {
    Ok(User { name: format!("ada@{}", db.url) })
}
async fn require_admin(user: Dep<User>) -> Result<Admin> {
    if user.name.starts_with("ada") { Ok(Admin) } else { Err(Error::new(jerrycan::http::StatusCode::FORBIDDEN, "JC0403", "admins only")) }
}
async fn dashboard(_: Dep<Admin>, user: Dep<User>) -> String { user.name.clone() }

let t = App::new()
    .provide(Db { url: "pg://prod" })
    .provide_dep(current_user)
    .provide_dep(require_admin)
    .route("/admin", get(dashboard))
    .into_test();
assert_eq!(t.get("/admin").await.text(), "ada@pg://prod");
# }); }
```

Per-request memoization: a factory runs at most once per request, no matter how
many handlers/factories ask for its type; a fresh request resolves afresh.

## Resolving deps outside a request (TaskContext)
A handler gets its deps through `Dep<T>` extractors, but background jobs and
app-level setup run with NO request. There you resolve deps imperatively from a
`TaskContext` — `ctx.resolve::<T>().await` returns `Result<Arc<T>>` (T must be
`Send + Sync + 'static`). Get a `TaskContext` from a built/test app with
`task_context()`; it sees every app-level `.provide`/`.provide_dep`, but NOT
request-only deps (those fail `JC1003`).
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Config { region: &'static str }

let t = App::new().provide(Config { region: "eu" }).into_test();
let mut ctx = t.task_context();
let cfg = ctx.resolve::<Config>().await.unwrap();   // Arc<Config>
assert_eq!(cfg.region, "eu");
# }); }
```
`ctx.fork()` returns a fresh `TaskContext` sharing the same app-level singletons
and factories but with an EMPTY resolution cache — the job worker forks one per
job so per-request-scope factories re-run and cached state never leaks between
jobs. See 15-jobs for using `resolve` inside a task body.

## Errors you'll hit
- Consuming an unregistered type → `500 JC1001` naming the missing type. Fix:
  `.provide`/`.provide_dep` it on the app or the module.
- A factory chain deeper than 32 (or cyclic) → `500 JC1002`. Break the cycle.
- An HTTP-only dependency (or extractor) resolved from a `TaskContext` → `JC1003`:
  a task has no request, so only app-level `Dep<T>` resolve there.

## Anti-patterns
- Don't pass `Dep<T>` values into helper functions by cloning everywhere —
  factories compose; make the helper a dependency.
- Don't use a singleton `.provide(value)` for per-request state (sessions,
  transactions) — that's what `.provide_dep` request scope is for.

## Testing
See 07-testing: `TestApp::override_dep` replaces ANY dependency — value or
factory product — without touching app code.
