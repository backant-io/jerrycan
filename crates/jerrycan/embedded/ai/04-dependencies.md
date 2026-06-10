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

## Errors you'll hit
- Consuming an unregistered type → `500 JC1001` naming the missing type. Fix:
  `.provide`/`.provide_dep` it on the app or the module.
- A factory chain deeper than 32 (or cyclic) → `500 JC1002`. Break the cycle.

## Anti-patterns
- Don't pass `Dep<T>` values into helper functions by cloning everywhere —
  factories compose; make the helper a dependency.
- Don't use a singleton `.provide(value)` for per-request state (sessions,
  transactions) — that's what `.provide_dep` request scope is for.

## Testing
See 07-testing: `TestApp::override_dep` replaces ANY dependency — value or
factory product — without touching app code.
