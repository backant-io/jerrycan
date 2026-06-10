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

Query fields are REQUIRED by default — a missing `?limit=` is `400 JC0400`.
Make pagination optional with `Option<T>` or `#[serde(default)]`:
```rust
# use jerrycan::prelude::*;
# use serde::Deserialize;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Deserialize)]
struct Page { limit: Option<u32>, #[serde(default)] offset: u32 }

async fn list(Query(p): Query<Page>) -> String {
    format!("limit={:?} offset={}", p.limit, p.offset)
}

let t = App::new().route("/items", get(list)).into_test();
assert_eq!(t.get("/items").await.text(), "limit=None offset=0"); // no query string: fine
# }); }
```

Multiple path parameters extract as a tuple, in route order:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn cell(Path((row, col)): Path<(i64, i64)>) -> String {
    format!("r{row}c{col}")
}

let t = App::new().route("/grid/{row}/{col}", get(cell)).into_test();
assert_eq!(t.get("/grid/3/9").await.text(), "r3c9");
# }); }
```

## Errors you'll hit
- `Path<T>` parse failure → `400 JC0400` ("invalid path parameter") automatically.
- Malformed/mistyped JSON body → `422 JC0422` with the serde message.
- Bad query string → `400 JC0400`. You never write these error branches.

## Anti-patterns
- `RequestCtx` does not implement `FromRequest`, so it cannot appear in a handler signature — this is a compile error, not a style rule. If a value isn't expressible as an extractor, define a dependency for it.
- Up to three `{params}` per route: one → `Path<T>`, two/three → tuple form
  `Path<(A, B)>` in route order. More than three params is a design smell —
  split the route or use a subroute.
