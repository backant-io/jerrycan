# Middleware

## Purpose
Wrap request handling with policy: auth zones, rate limits, audit logs.
One trait, explicit ordering — app middleware first, then each module's down
the mount chain. Prefer dependencies for per-handler needs; middleware is for
subtree-wide policy.

## Signature
```rust
# use jerrycan::prelude::*;
struct AuditLog;

impl Middleware for AuditLog {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async move {
            // before: inspect ctx (method/uri/headers)
            let res = next.run(&mut *ctx).await;   // reborrow: ctx stays usable
            // after: inspect/modify res
            res
        })
    }
}
# let _ = AuditLog;
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Deny;
impl Middleware for Deny {
    fn handle<'a>(&'a self, _ctx: &'a mut RequestCtx, _next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async {
            Error::new(jerrycan::http::StatusCode::FORBIDDEN, "JC0403", "locked").into_response()
        })
    }
}

let locked = Module::new("locked").middleware(Deny).route("/", get(|| async { "secret" }));
let t = App::new()
    .route("/open", get(|| async { "open" }))
    .mount("/locked", locked)
    .into_test();

assert_eq!(t.get("/open").await.status(), jerrycan::http::StatusCode::OK);
assert_eq!(t.get("/locked/").await.status(), jerrycan::http::StatusCode::FORBIDDEN);
# }); }
```

## Variations
- App-wide: `App::new().middleware(AuditLog)` — outermost ring, every route.
- Module-scoped: `Module::new("admin").middleware(RequireStaff)` — that subtree only.
- Ordering: parents run before children; within one level, registration order.

## Errors you'll hit
- Borrow error inside `handle` after `next.run(ctx)` → you moved `ctx`; call
  `next.run(&mut *ctx)` (reborrow) as in the Signature example.

## Anti-patterns
- Don't do per-handler work (current user, db txn) in middleware — that's a
  dependency (`Dep<T>`), which handlers declare explicitly and tests override.
- Don't mutate the request body in middleware; extractors own body semantics.
