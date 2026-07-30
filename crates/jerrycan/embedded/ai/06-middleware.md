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

## Reading the request (`RequestCtx`)
The `ctx` handed to `handle` (and to dependency factories) exposes the request's
head — useful for audit and rate-limit policy:
- `ctx.method() -> &http::Method`
- `ctx.uri() -> &http::Uri`
- `ctx.headers() -> &http::HeaderMap`
- `ctx.peer_addr() -> Option<std::net::SocketAddr>` — the client socket address
  (the OWNED `SocketAddr` by value, `None` when there's no peer, e.g. an
  in-memory `TestApp` request unless you use `t.get_from(path, addr)`). Use it to
  key an audit log or a rate-limit partition.

```rust
# use jerrycan::prelude::*;
struct AuditPeer;
impl Middleware for AuditPeer {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async move {
            let who = ctx.peer_addr().map(|a| a.to_string()).unwrap_or_else(|| "-".into());
            let line = format!("{} {} from {who}", ctx.method(), ctx.uri());
            let _ = line;  // emit to your access log
            next.run(&mut *ctx).await
        })
    }
}
# let _ = AuditPeer;
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

## Rate limiting
`jerrycan-ratelimit` (the `rate-limit` feature) is an extension, not a hand-rolled
middleware: `app.extend(RateLimit::per_window(n, dur))` allows `n` requests per
fixed `dur` window per partition key. The partition is chosen by precedence —
**api-key header (opt-in, off by default) → user-key closure → client IP** — so an
authenticated caller is limited by identity, anonymous traffic by source. Over-limit requests get
`429 JC0429` with a `Retry-After` header; `OPTIONS` is exempt (CORS preflight must
never be throttled). Time comes from the injected `Clock`, so windows are
deterministic in tests — `t.clock().advance(dur)` rolls to the next window.

Builders tune the partition and store: `api_key_header("x-api-key")` OPTS IN to an
api-key partition tier (**off by default** — there is no default header) so the
named header becomes tier 1; `user_key(|ctx| ..)` supplies a stable user key (e.g.
a JWT sub) for tier 2; `trust_forwarded_for(true)` makes the IP tier honor
`X-Forwarded-For` (default OFF — the header is client-spoofable, so only enable it
behind a trusted proxy); `store(Arc::new(..))` swaps the backend — the default is
in-memory, and `RedisStore` (behind `rate-limit-redis`) shares one window across
replicas.

**Api-key caveat:** an api-key header is only safe to partition by when the key is
authenticated upstream of the limiter. An unauthenticated header is
client-controlled, so a caller can mint a fresh budget per request by rotating the
value — bypassing the limit entirely. Leave it off (the default) unless the key is
validated before the limiter runs; with no api-key tier, the partition falls back
to the authenticated user then client IP, both unspoofable.

Failure modes split by cause: a missing identity (no peer, no api-key, no
user-key) or a missing clock fails **open** — a misconfigured limiter must not
break all traffic, so the request is admitted. But a **store** error (e.g. Redis
down) fails **closed** — it surfaces as a `500`, never silently admitting traffic
past the limit.

Fixed windows are deterministic but allow a **burst across the boundary**: a client
can spend its full quota at the end of one window and again at the start of the
next, so up to ~2× the limit in a short span. That is the known fixed-window
property, by design — not a bug.

```rust
# use jerrycan::prelude::*;
# #[cfg(feature = "rate-limit")]
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::ratelimit::RateLimit;
use std::time::Duration;

let t = App::new()
    .extend(RateLimit::per_window(2, Duration::from_secs(60)))
    .route("/ping", get(|| async { NoContent }))
    .into_test();

// IP tier (no api-key, no user_key): two pass, the third trips
let ip: std::net::SocketAddr = "198.51.100.4:1111".parse().unwrap();
assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
let limited = t.get_from("/ping", ip).await;
assert_eq!(limited.status().as_u16(), 429);
assert!(limited.headers().contains_key("retry-after"));
assert!(limited.text().contains("JC0429"));

// advancing the injected clock past the window resets the count
t.clock().advance(Duration::from_secs(61));
assert_eq!(t.get_from("/ping", ip).await.status().as_u16(), 204);
# }); }
# #[cfg(not(feature = "rate-limit"))]
# fn main() {}
```

In a generated app you declare rate limiting in `design.json` rather than
hand-editing the tool-owned `crates/app/src/main.rs` (that edit trips the JL0003
tool-owned-file lint and is wiped by the next `jerrycan generate`):

```json
{
  "rate_limit": {
    "limit": 100,
    "window": "1m",
    "api_key_header": "x-api-key",
    "trust_forwarded_for": false
  }
}
```

The generator wires `.extend(RateLimit::per_window(100, Duration::from_secs(60)))`
into app assembly, right after the `Auth` extension so the limiter's identity-aware
partition can resolve the authenticated user — so there is no hand-edit and JL0003
never trips. `limit` is the requests allowed per `window` per partition key;
`window` is a duration string (`30s`/`1m`/`1h`/`1d`, or a bare number of seconds).
`api_key_header` and `trust_forwarded_for` are optional — omit them (the default)
to partition by the authenticated user then client IP, both unspoofable; the
api-key tier is opt-in and carries the caveat above. Over-limit requests get
`429 JC0429` with a `Retry-After` header. A malformed block — a 0 limit (blocks
every request), an unparseable or zero `window`, or an `api_key_header` that is not
a valid `^[A-Za-z0-9-]+$` token — is refused up front by `jerrycan check` with
`JC0563`.

## CORS
Cross-origin access is a policy on the `App`, not a middleware — preflight `OPTIONS`
is answered *before* routing, and actual cross-origin responses (including 404/405)
are decorated with the CORS headers. `App::cors(CorsConfig::new(origins))` installs
it: `CorsOrigins::list([..])` is an exact allowlist, `CorsOrigins::any()` sends
`Access-Control-Allow-Origin: *`. Chain `allow_methods([..])` / `allow_headers([..])`
to pin the preflight response (omit them to reflect the request's method/headers),
and `allow_credentials(true)` to allow cookies / `Authorization` — the Fetch spec
forbids combining credentials with `any()`, so that pairing is a build error.

```rust
# use jerrycan::prelude::*;
# fn main() {
let _app = App::new()
    .cors(
        CorsConfig::new(CorsOrigins::list(["https://app.example"]))
            .allow_methods([jerrycan::http::Method::GET, jerrycan::http::Method::POST])
            .allow_headers(["content-type", "authorization"])
            .allow_credentials(true),
    )
    .route("/", get(|| async { NoContent }));
# }
```

In a generated app you declare CORS in `design.json` rather than hand-editing the
tool-owned `crates/app/src/main.rs` (that edit trips the JL0003 tool-owned-file lint
and is wiped by the next `jerrycan generate`):

```json
{
  "cors": {
    "origins": ["https://app.example", "https://admin.example"],
    "methods": ["GET", "POST", "PUT", "PATCH", "DELETE"],
    "headers": ["content-type", "authorization"],
    "allow_credentials": true
  }
}
```

The generator emits `.cors(CorsConfig::new(..)..)` into app assembly. The allowed
origins are overridable at deploy time via the `JERRYCAN_CORS_ORIGINS` env var
(comma-separated; `*` for any) — falling back to the design's list — so the same
binary serves staging and production SPAs without a rebuild. Use `"origins": ["*"]`
for any origin (which cannot be combined with `allow_credentials`).

## Errors you'll hit
- Borrow error inside `handle` after `next.run(ctx)` → you moved `ctx`; call
  `next.run(&mut *ctx)` (reborrow) as in the Signature example.

## Anti-patterns
- Don't do per-handler work (current user, db txn) in middleware — that's a
  dependency (`Dep<T>`), which handlers declare explicitly and tests override.
- Don't mutate the request body in middleware; extractors own body semantics.
