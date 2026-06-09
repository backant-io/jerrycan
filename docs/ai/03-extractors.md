# Extractors

## Purpose
Handler parameters ARE the request contract: each parameter implements
`FromRequest` and pulls one typed thing out of the request. Everything a
handler needs is visible in its signature.

## Signature
```rust
# use jerrycan::prelude::*;
# use serde::Deserialize;
# #[derive(Deserialize)] struct PageParams { limit: u32 }
# #[derive(Deserialize)] struct NewTodo { title: String }
# struct Db;
async fn handler(
    Path(id): Path<i64>,          // {id} from the route path, typed
    Query(page): Query<PageParams>, // ?limit=… via serde
    Json(body): Json<NewTodo>,    // JSON request body via serde
    db: Dep<Db>,                  // dependency injection (see 04-dependencies)
) -> Result<NoContent> {
    # let _ = (id, page.limit, body.title, db);
    Ok(NoContent)
}
# let _ = handler;
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn double(Path(n): Path<i64>) -> String { (n * 2).to_string() }

let t = App::new().route("/double/{n}", get(double)).into_test();
assert_eq!(t.get("/double/21").await.text(), "42");
# }); }
```

## Variations
JSON in, JSON out, with status-typed responses:
```rust
# use jerrycan::prelude::*;
# use serde::{Deserialize, Serialize};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Deserialize, Serialize)]
struct Todo { title: String }

async fn create(Json(todo): Json<Todo>) -> Result<Created<Todo>> {
    Ok(Created(todo))
}

let t = App::new().route("/todos", post(create)).into_test();
let res = t.post_json("/todos", &Todo { title: "x".into() }).await;
assert_eq!(res.status(), jerrycan::http::StatusCode::CREATED);
# }); }
```

## Errors you'll hit
- `Path<T>` parse failure → `400 JC0400` ("invalid path parameter") automatically.
- Malformed/mistyped JSON body → `422 JC0422` with the serde message.
- Bad query string → `400 JC0400`. You never write these error branches.

## Anti-patterns
- `RequestCtx` does not implement `FromRequest`, so it cannot appear in a handler signature — this is a compile error, not a style rule. If a value isn't expressible as an extractor, define a dependency for it.
- One `Path<T>` per route in v0 (one `{param}`-typed extractor); multi-param
  tuples arrive in Phase 1 — until then design routes with one variable segment
  per handler or read both via two nested modules.
