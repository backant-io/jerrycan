# Errors

## Purpose
One error type: `jerrycan::Error`. Every error has an HTTP status and a stable
code (`JC####`) that links to documentation. Responses render as
`{"code":"…","message":"…"}` — never stack traces, paths, or SQL.

## Signature
```rust
# use jerrycan::prelude::*;
# struct Item;
# async fn find_item(_id: i64) -> Option<Item> { None }
async fn show(Path(id): Path<i64>) -> Result<Json<String>> {
    let _item = find_item(id).await.ok_or_else(Error::not_found)?; // ? just works
    Err(Error::unprocessable("demo"))                              // explicit errors
}
# let _ = show;
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn boom() -> Result<&'static str> { Err(Error::not_found()) }

let t = App::new().route("/x", get(boom)).into_test();
let res = t.get("/x").await;
assert_eq!(res.status(), jerrycan::http::StatusCode::NOT_FOUND);
assert_eq!(res.text(), r#"{"code":"JC0404","message":"not found"}"#);
# }); }
```

## Variations
Custom status + code (use sparingly; prefer the constructors):
```rust
# use jerrycan::prelude::*;
let e = Error::new(jerrycan::http::StatusCode::CONFLICT, "JC0409", "title already exists");
assert_eq!(e.code(), "JC0409");
```

The full constructor set — every named helper on `Error` (prefer these over raw
`Error::new`). The comment is `<HTTP status> <JC code>`:
```rust
# use jerrycan::prelude::*;
let all = [
    Error::bad_request("bad input"),       // 400 JC0400
    Error::unauthorized(),                 // 401 JC0401
    Error::forbidden(),                    // 403 JC0403
    Error::not_found(),                    // 404 JC0404
    Error::method_not_allowed(),           // 405 JC0405
    Error::conflict("title taken"),        // 409 JC0409
    Error::payload_too_large(),            // 413 JC0413
    Error::unsupported_media_type(),       // 415 JC0415
    Error::unprocessable("bad field"),     // 422 JC0422
    Error::too_many_requests(),            // 429 JC0429
    Error::internal("boom"),               // 500 JC0500
    Error::handler_timeout(),              // 503 JC0503
    Error::job_failed("retry exhausted"),  // 500 JC0521 (code 0521, wire status 500)
    Error::missing_dependency("Db"),       // 500 JC1001
    Error::dependency_cycle(),             // 500 JC1002
    Error::task_context(),                 // 500 JC1003
];
assert_eq!(all[0].code(), "JC0400");
assert_eq!(all[1].code(), "JC0401");
assert_eq!(Error::job_failed("x").status().as_u16(), 500); // 0521 maps to a 500 wire status
```
`conflict`, `job_failed`, and `internal`/`bad_request`/`unprocessable` take
`impl Into<String>`; `missing_dependency` takes `&str` (the type name); the rest
take no argument. `job_failed` carries code `JC0521` but responds with HTTP 500.

## Errors you'll hit (the built-in code table)
| Code | Status | Produced when |
|---|---|---|
| JC0400 | 400 | Bad path param / query string / malformed percent-encoding in path |
| JC0401 | 401 | Authentication required or failed (jerrycan::auth) |
| JC0403 | 403 | Authenticated but not permitted (require_role) |
| JC0404 | 404 | No route matched, or `Error::not_found()` |
| JC0405 | 405 | Path exists, method doesn't |
| JC0408 | 408 | Request body wasn't received within the read budget (default 30s) |
| JC0409 | 409 | `Error::conflict` — unique-key violation (a re-POSTed id), version conflict |
| JC0413 | 413 | Body over the limit (default 1 MiB) |
| JC0415 | 415 | `Error::unsupported_media_type` — wrong content type (e.g. multipart without a boundary) |
| JC0422 | 422 | JSON body failed to parse, or `Valid<T>` found violations (structured `details` array) |
| JC0429 | 429 | `Error::too_many_requests` — rate limit exceeded (jerrycan::ratelimit) |
| JC0500 | 500 | `Error::internal` / response serialization failure |
| JC0503 | 503 | Handler exceeded its time budget (default 30s), or `Error::handler_timeout` |
| JC0510 | 500 | Database failure (jerrycan::db) — detail on stderr, never in the body |
| JC0521 | 500 | `Error::job_failed` — a background job exhausted its retries / dead-lettered (code 0521, wire status 500) |
| JC1001 | 500 | Dependency type has no provider (`Error::missing_dependency`) |
| JC1002 | 500 | Dependency cycle / chain > 32 (`Error::dependency_cycle`) |
| JC1003 | 500 | A request-only dependency was resolved in a background task (`Error::task_context`) |

## Anti-patterns
- Don't `panic!`/`unwrap()` in handlers for expected failures — return `Err`.
- Don't put internal detail (queries, file paths) in `message` — it goes to the
  client. Log internals; respond with intent.
