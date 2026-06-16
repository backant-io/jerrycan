# Response types — statuses, redirects, custom bodies

## Purpose
A handler returns anything implementing `IntoResponse`. This page is the menu:
the built-in returns (JSON, text, a bare status, errors), `Redirect` for 3xx, the
`(StatusCode, body)` tuple for a custom status with a body, and the fully-custom
escape hatch when you need arbitrary headers. Prefer the named helpers — reach
for the escape hatch only when nothing else fits.

## The built-in returns
Each of these is an `IntoResponse` you return directly (usually inside a
`Result<T>`):

| Return value         | Status | Body                                   |
|----------------------|--------|----------------------------------------|
| `Json(value)`        | 200    | `value` as JSON (`application/json`)   |
| `Created(value)`     | 201    | `value` as JSON                        |
| `NoContent`          | 204    | empty                                  |
| `&str` / `String`    | 200    | the text (`text/plain; charset=utf-8`) |
| `StatusCode::X`      | X      | empty                                  |
| `Error` / `Err(e)`   | e's    | `{"code","message"}` JSON envelope     |

```rust
# use jerrycan::prelude::*;
use jerrycan::http::StatusCode;   // StatusCode is not in the prelude
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn created() -> Result<Created<&'static str>> { Ok(Created("made it")) }
async fn gone()    -> Result<NoContent>             { Ok(NoContent) }
async fn teapot()  -> Result<StatusCode>            { Ok(StatusCode::IM_A_TEAPOT) }
async fn boom()    -> Result<&'static str>          { Err(Error::not_found()) }

let t = App::new()
    .route("/created", post(created))
    .route("/gone", get(gone))
    .route("/teapot", get(teapot))
    .route("/boom", get(boom))
    .into_test();

assert_eq!(t.post_bytes("/created", b"").await.status().as_u16(), 201);
assert_eq!(t.get("/gone").await.status().as_u16(), 204);
assert_eq!(t.get("/teapot").await.status().as_u16(), 418);

// An Error renders the JSON envelope and carries its own status.
let err = t.get("/boom").await;
assert_eq!(err.status().as_u16(), 404);
assert_eq!(err.json::<serde_json::Value>()["code"], "JC0404");
# }); }
```
`StatusCode` is not in the prelude — `use jerrycan::http::StatusCode;` (as above)
or write the path inline as `jerrycan::http::StatusCode`.

## Redirects
`Redirect` writes an empty body, a 3xx status, and the `Location` header. Pick the
constructor by semantics, not by number:

- `Redirect::to(url)` — **302 Found**, the default.
- `Redirect::see_other(url)` — **303**, for POST→GET (post-redirect-get).
- `Redirect::temporary(url)` — **307**, preserves method + body.
- `Redirect::permanent(url)` — **308**, preserves method + body, cacheable.

An OAuth-connect-style handler computes the provider URL and redirects the browser
to it:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn connect() -> Result<Redirect> {
    // In a real app this is OAuthClient::authorize_url(...) — see 16-auth-advanced.
    let authorize_url = "https://accounts.example.com/authorize?client_id=abc&state=xyz";
    Ok(Redirect::to(authorize_url))
}

let t = App::new().route("/oauth/connect", get(connect)).into_test();
let r = t.get("/oauth/connect").await;
assert_eq!(r.status().as_u16(), 302);
assert_eq!(r.headers()["location"], "https://accounts.example.com/authorize?client_id=abc&state=xyz");

// see_other / temporary / permanent set 303 / 307 / 308.
async fn moved() -> Result<Redirect> { Ok(Redirect::permanent("/v2/here")) }
let t2 = App::new().route("/old", get(moved)).into_test();
let moved = t2.get("/old").await;
assert_eq!(moved.status().as_u16(), 308);
assert_eq!(moved.headers()["location"], "/v2/here");
# }); }
```
A location that can't be a header value (e.g. one containing a newline) does not
panic — it renders a `500` with no `Location` header, so a malformed redirect
fails honestly instead of crashing the request task.

## Custom status + body
To return a status the built-ins don't cover *with* a body, return a
`(StatusCode, body)` tuple: the body renders normally (its content type and
bytes), and the status is overwritten. This is the idiom for `202 Accepted` with
a payload — work queued, not done:
```rust
# use jerrycan::prelude::*;
# use serde::Serialize;
use jerrycan::http::StatusCode;   // StatusCode is not in the prelude
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Serialize)]
struct Summary { job_id: u64, queued: u32 }

async fn enqueue() -> Result<(StatusCode, Json<Summary>)> {
    let summary = Summary { job_id: 42, queued: 3 };
    Ok((StatusCode::ACCEPTED, Json(summary)))   // 202 + JSON body
}

async fn enqueue_text() -> Result<(StatusCode, &'static str)> {
    Ok((StatusCode::ACCEPTED, "queued"))        // 202 + text body
}

let t = App::new()
    .route("/jobs", post(enqueue))
    .route("/jobs-text", post(enqueue_text))
    .into_test();

let json = t.post_bytes("/jobs", b"").await;
assert_eq!(json.status().as_u16(), 202);
assert_eq!(json.headers()["content-type"], "application/json");
assert_eq!(json.json::<serde_json::Value>()["queued"], 3);

let text = t.post_bytes("/jobs-text", b"").await;
assert_eq!(text.status().as_u16(), 202);
assert_eq!(text.text(), "queued");
# }); }
```

## Fully custom response (the escape hatch)
When you need arbitrary headers, build the response directly. The handler's
response type is `jerrycan::Response` (the alias for `jerrycan::http::Response`
wrapping a `jerrycan::JcBody` body — neither the alias nor `JcBody` is in the
prelude, so name them by path). `JcBody::full(bytes)` is a complete in-memory
body; `JcBody::empty()` is zero frames. Construct with
`jerrycan::http::Response::new(body)`, set the status with `status_mut()`, and add
headers with `headers_mut()`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::http::{HeaderValue, StatusCode, header};
use jerrycan::JcBody;

async fn custom() -> Result<jerrycan::Response> {
    let mut r = jerrycan::http::Response::new(JcBody::full("pong"));
    *r.status_mut() = StatusCode::OK;
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    r.headers_mut()
        .insert("x-trace-id", HeaderValue::from_static("abc123"));
    Ok(r)
}

let t = App::new().route("/custom", get(custom)).into_test();
let r = t.get("/custom").await;
assert_eq!(r.status().as_u16(), 200);
assert_eq!(r.headers()["x-trace-id"], "abc123");
assert_eq!(r.text(), "pong");
# }); }
```

## Large or streamed bodies
For downloads, CSV exports, or any body produced incrementally rather than
buffered whole, return a `StreamBody` instead of a buffered response — see
`Streaming` in 01-app and the `Multipart`/streaming notes in 03-extractors.

## Anti-patterns
- **Don't hand-build a `Response` just to set a status.** For a status + body use
  the `(StatusCode, body)` tuple; for a bare status return `StatusCode` directly.
  The escape hatch is for arbitrary *headers*, nothing less.
- **Don't pick a redirect status by number.** `Redirect::see_other` /
  `temporary` / `permanent` name the follow semantics; a bare `303`/`307`/`308`
  with a hand-set `Location` is the same bytes with the intent lost.
- **Don't buffer a large export into `Json`/`String`.** Stream it (`StreamBody`)
  so memory stays flat and the client can start reading immediately.
