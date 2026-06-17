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

### Leaf binding under a param-carrying mount
A single `Path<T>` binds the LEAF-MOST (last) captured parameter. So a route
mounted under a prefix that itself carries a `{param}` addresses its own param,
not the mount's — `Path<i64>` on `/leads/{id}` under `/ws/{ws}` is the `{id}`.
Reach the mount's param by taking the whole set as a tuple (root→leaf):
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn show(Path(id): Path<i64>) -> Result<Json<i64>> { Ok(Json(id)) }        // leaf {id}
async fn pair(Path((ws, id)): Path<(i64, i64)>) -> Result<Json<(i64, i64)>> {   // both, root→leaf
    Ok(Json((ws, id)))
}

let t = App::new()
    .mount("/ws/{ws}", Module::new("leads")
        .route("/leads/{id}", get(show))
        .route("/leads/{id}/full", get(pair)))
    .into_test();
assert_eq!(t.get("/ws/7/leads/42").await.json::<i64>(), 42);          // leaf, not mount
assert_eq!(t.get("/ws/7/leads/42/full").await.json::<(i64, i64)>(), (7, 42));
# }); }
```

Custom id newtypes become path params through `jerrycan::path_param!`: the type
just needs `FromStr` with a `Display` error; a parse failure is the same
`400 JC0400` the built-in impls produce:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Debug)]
struct LeadId(i64);
impl std::str::FromStr for LeadId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> { Ok(LeadId(s.parse()?)) }
}
jerrycan::path_param!(LeadId);

async fn show(Path(id): Path<LeadId>) -> Result<Json<i64>> { Ok(Json(id.0)) }

let t = App::new().route("/leads/{id}", get(show)).into_test();
assert_eq!(t.get("/leads/42").await.json::<i64>(), 42);
# }); }
```

## RawBody — the exact request bytes
`RawBody(pub Bytes)` is the request body as the EXACT wire bytes, untouched by
any parser. It's the extractor for webhook signature verification: the provider
signs the bytes it sent, so the HMAC must cover those bytes — `Json<T>` would
re-serialize the value and the digest would never match. `RawBody` works on both
lanes: on a buffered route it's a cheap clone of the already-read body; on a
`.stream_body()` route it drains the body and caches it. Either way the route's
`body_limit` still caps the total. Pair it with `Headers` to read the signature
header the provider sent:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn webhook(headers: Headers, RawBody(body): RawBody) -> Result<NoContent> {
    let signature = headers.get("x-signature")
        .ok_or_else(|| Error::new(jerrycan::http::StatusCode::UNAUTHORIZED, "JC0401", "missing signature"))?;
    // `body` is the exact bytes the sender signed — verify against `signature`
    // (see 10-auth for the Stripe/Twilio HMAC recipes).
    let _ = (signature, &body);
    Ok(NoContent)
}

let t = App::new().route("/hook", post(webhook)).into_test();
let res = t.post_bytes_with("/hook", b"{\"event\":1}", &[("x-signature", "abc")]).await;
assert_eq!(res.status().as_u16(), 204);
# }); }
```

## Multipart — file uploads and form-data
`Multipart` parses `multipart/form-data` bodies — file uploads and mixed
form/file submissions. Pair it with `.stream_body()` so a large upload is never
buffered whole before the handler runs; without the marker it still works for
anything inside the route's `body_limit`. Parts arrive in wire order: loop
`next_part()`, then stream a file part with `part.chunk()` (governed by the
route's cumulative `body_limit`, not the per-part cap) and pull small fields
with `part.text()`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn import(mut form: Multipart) -> Result<Json<(String, usize)>> {
    let mut dataset = String::new();
    let mut lines = 0usize;
    while let Some(mut part) = form.next_part().await? {
        match part.name() {
            "dataset" => dataset = part.text().await?,       // small field: buffer it
            "file" => {                                       // the CSV: stream it
                while let Some(chunk) = part.chunk().await? {
                    lines += chunk.iter().filter(|&&b| b == b'\n').count();
                }
            }
            _ => {}                                           // ignore unknown fields
        }
    }
    Ok(Json((dataset, lines)))
}

let t = App::new().route("/import", post(import).stream_body()).into_test();
let res = t.post_multipart("/import", &[
    TestPart::text("dataset", "leads"),
    TestPart::file("file", "rows.csv", "text/csv", b"name,email\nada,a@x\nbob,b@x\n"),
]).await;
assert_eq!(res.json::<(String, usize)>(), ("leads".to_string(), 3)); // header + 2 rows
# }); }
```
Each `Part` exposes its headers before you read its body: `part.name() -> &str`
(the form field name), `part.filename() -> Option<&str>` (the uploaded
filename, if any), and `part.content_type() -> Option<&str>` (the part's
declared `Content-Type`, e.g. `"text/csv"` — branch on it to accept or reject an
upload before draining it).

Rules and limits:
- Wrong content type (not `multipart/form-data` with a boundary) → `415 JC0415`.
- `bytes()`/`text()` buffer a whole part, capped at the per-part cap (default
  8 MiB; override per request with `form.set_part_cap(n)`) → `413 JC0413` over it.
  `chunk()` is NOT subject to this cap — it's bounded only by the route's
  `body_limit`, so stream big files through `chunk()`.
- More than 256 parts, or part headers over 8 KiB → `413 JC0413` (part-count /
  header bombs).
- A malformed body → `400 JC0400`.
- Parts are sequential: `next_part()` discards any unread remainder of the
  current part before yielding the next, and the extractor is single-consumer
  (it owns the body — extracting `Multipart` twice in one handler is an error).

## Errors you'll hit
- `Path<T>` parse failure → `400 JC0400` ("invalid path parameter") automatically.
- Malformed/mistyped JSON body → `422 JC0422` with the serde message.
- Bad query string → `400 JC0400`. You never write these error branches.

## Anti-patterns
- `RequestCtx` does not implement `FromRequest`, so it cannot appear in a handler signature — this is a compile error, not a style rule. If a value isn't expressible as an extractor, define a dependency for it.
- Up to three `{params}` per route: one → `Path<T>`, two/three → tuple form
  `Path<(A, B)>` in route order. More than three params is a design smell —
  split the route or use a subroute.
