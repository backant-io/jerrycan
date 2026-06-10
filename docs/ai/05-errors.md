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

The full constructor set (prefer these over raw `Error::new`):
```rust
# use jerrycan::prelude::*;
let all = [
    Error::bad_request("bad input"),      // 400 JC0400
    Error::not_found(),                   // 404 JC0404
    Error::method_not_allowed(),          // 405 JC0405
    Error::payload_too_large(),           // 413 JC0413
    Error::unprocessable("bad field"),    // 422 JC0422
    Error::internal("boom"),              // 500 JC0500
];
assert_eq!(all[0].code(), "JC0400");
```

## Errors you'll hit (the built-in code table)
| Code | Status | Produced when |
|---|---|---|
| JC0400 | 400 | Bad path param / query string / malformed percent-encoding in path |
| JC0401 | 401 | Authentication required or failed (jerrycan::auth) |
| JC0403 | 403 | Authenticated but not permitted (require_role) |
| JC0404 | 404 | No route matched, or `Error::not_found()` |
| JC0405 | 405 | Path exists, method doesn't |
| JC0408 | 408 | Request body wasn't received within the read budget (default 30s) |
| JC0413 | 413 | Body over the limit (default 1 MiB) |
| JC0422 | 422 | JSON body failed to parse, or `Valid<T>` found violations (structured `details` array) |
| JC0500 | 500 | `Error::internal` / response serialization failure |
| JC0503 | 503 | Handler exceeded its time budget (default 30s) |
| JC0510 | 500 | Database failure (jerrycan::db) — detail on stderr, never in the body |
| JC1001 | 500 | Dependency type has no provider |
| JC1002 | 500 | Dependency cycle / chain > 32 |

## Anti-patterns
- Don't `panic!`/`unwrap()` in handlers for expected failures — return `Err`.
- Don't put internal detail (queries, file paths) in `message` — it goes to the
  client. Log internals; respond with intent.
