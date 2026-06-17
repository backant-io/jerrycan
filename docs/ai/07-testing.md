# Testing

## Purpose
`TestApp` runs real requests through your real app **in memory** — no sockets,
no network, no test server. `override_dep` swaps any dependency (database,
current user, clock) without touching app code. Generated acceptance tests use
exactly this API.

## Signature
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
# struct Db { url: String }
# let app = App::new().provide(Db { url: "pg://prod".into() }).route("/", get(|| async { "ok" }));
let t = app
    .into_test()                                  // build + panic on route conflicts
    .override_dep(Db { url: "sqlite::memory:".into() }); // fake ANY dependency

let res = t.get("/").await;                        // .post_json / .put_json / .patch_json / .delete
assert_eq!(res.status(), jerrycan::http::StatusCode::OK);
assert_eq!(res.text(), "ok");                      // or res.json::<T>()
# }); }
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# use serde::{Deserialize, Serialize};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Serialize, Deserialize)]
struct NewTodo { title: String }

async fn create(Json(t): Json<NewTodo>) -> Result<Created<NewTodo>> { Ok(Created(t)) }

let t = App::new().route("/todos", post(create)).into_test();
let res = t.post_json("/todos", &NewTodo { title: "ship".into() }).await;

assert_eq!(res.status(), jerrycan::http::StatusCode::CREATED);
assert_eq!(res.json::<NewTodo>().title, "ship");
# }); }
```

## Variations
Override a factory's product directly (skip the whole auth chain in one line):
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct User { name: String }
async fn current_user() -> Result<User> { Err(Error::new(jerrycan::http::StatusCode::UNAUTHORIZED, "JC0401", "no session")) }
async fn whoami(u: Dep<User>) -> String { u.name.clone() }

let app = App::new().provide_dep(current_user).route("/me", get(whoami));
let t = app.into_test().override_dep(User { name: "test-user".into() });
assert_eq!(t.get("/me").await.text(), "test-user");
# }); }
```

Assert on response headers with `headers()`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let t = App::new().route("/", get(|| async { Json(42) })).into_test();
let res = t.get("/").await;
assert_eq!(res.headers()["content-type"], "application/json");
# }); }
```

Post a `multipart/form-data` request with `post_multipart` — `TestPart::text`
for fields, `TestPart::file` for uploads (it builds the boundary and wire body
for you). `post_multipart_with` adds request headers (auth cookies, etc.).
`TestResponse::bytes()` returns the raw response body for non-text downloads:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn upload(mut form: Multipart) -> Result<Json<Vec<String>>> {
    let mut seen = Vec::new();
    while let Some(part) = form.next_part().await? {
        let label = match part.filename() {
            Some(f) => format!("{}:{}", part.name(), f),
            None => part.name().to_string(),
        };
        seen.push(format!("{label}({})", part.bytes().await?.len()));
    }
    Ok(Json(seen))
}

let t = App::new().route("/upload", post(upload).stream_body()).into_test();
let res = t.post_multipart("/upload", &[
    TestPart::text("title", "Q3 leads"),
    TestPart::file("csv", "leads.csv", "text/csv", b"a,b\n1,2\n"),
]).await;
assert_eq!(res.status().as_u16(), 200);
assert_eq!(
    res.json::<Vec<String>>(),
    vec!["title(8)".to_string(), "csv:leads.csv(8)".to_string()]
);
assert_eq!(res.bytes()[0], b'[');   // raw bytes, for binary downloads
# }); }
```

## Generated tests: happy path vs declared error-cases
`jerrycan gen-tests` writes one TOOL-OWNED `tests/acceptance.rs` per module. Know
exactly what it does and does NOT cover so you author the rest:
- **Success probe.** Each endpoint gets a test that sends a *minimal valid body*
  (required fields filled with type-shaped fixtures, enum fields set to their
  first declared value, `belongs_to` fks pointed at the seeded tenant) and
  asserts the design's declared `success.status`. It is the success path, not a
  security probe — it carries no signature/credential beyond what the design
  declares.
- **Auth guard.** A session-guarded endpoint (auth mode) also gets a
  `<op>_without_auth_is_401` test: the same request with **no** cookie must 401.
  That covers the framework's session guard — NOT a custom credential.
- **Custom credentials (signatures, webhook secrets, API-key headers).** The probe
  posts the unsigned/minimal shape and expects `success.status`. That's only
  correct if your endpoint is *meant* to accept that shape — e.g. a "no signing
  secret configured" mode that processes the body. If instead the endpoint must
  **reject** a missing/invalid signature (e.g. webhook → `400`/`401`/`403`), that
  rejection is NOT generated: every design error-case that isn't a `404` on a
  single-`{id}` path is emitted as an `// AGENT TODO` comment. Turn each TODO into
  a real error test in a sibling file (send a body with a bad/missing signature,
  assert the rejection status) — don't loosen the handler to make the happy-path
  probe pass.

## Errors you'll hit
- `panic: app failed to build` — your route table has a conflict; the message
  names the path. This is the same failure `serve()` would return.
- `response body is not the expected JSON shape` — `res.json::<T>()` panics
  with the body text included; read it, fix the handler or the test type.

## Anti-patterns
- Don't boot real servers/sockets in tests — `TestApp` is the contract.
- Don't build special "test mode" branches into handlers — if a handler needs
  faking, model the thing being faked as a dependency and override it.
