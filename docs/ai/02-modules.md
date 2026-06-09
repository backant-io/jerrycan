# Modules

## Purpose
A `Module` is jerrycan's unit of routing, packaging, and ownership — routes,
nested subroutes, module-scoped dependencies and middleware, in one value.
Every route crate exposes exactly one public item: `pub fn module() -> Module`.

## Signature
```rust
# use jerrycan::prelude::*;
# async fn list() -> &'static str { "l" }
# async fn create() -> &'static str { "c" }
# async fn show() -> &'static str { "s" }
# fn comments_module() -> Module { Module::new("comments") }
# struct TodoRepo;
pub fn module() -> Module {
    Module::new("todos")
        .route("/", get(list).post(create))      // relative to the mount prefix
        .route("/{id}", get(show))               // {param} captures a segment
        .mount("/{id}/comments", comments_module()) // subroutes nest arbitrarily
        .provide(TodoRepo)                       // module-scoped dependency
}
# let _ = module();
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
async fn hello() -> &'static str { "hello from a module" }

let todos = Module::new("todos").route("/", get(hello));
let t = App::new().mount("/todos", todos).into_test();

assert_eq!(t.get("/todos/").await.text(), "hello from a module");
# }); }
```

## Variations
Module-scoped dependencies shadow app-scoped ones for that subtree only:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Flavor(&'static str);
async fn which(f: Dep<Flavor>) -> String { f.0.to_string() }

let special = Module::new("special").provide(Flavor("module")).route("/", get(which));
let t = App::new()
    .provide(Flavor("app"))
    .route("/plain", get(which))
    .mount("/special", special)
    .into_test();

assert_eq!(t.get("/plain").await.text(), "app");
assert_eq!(t.get("/special/").await.text(), "module");
# }); }
```

## Errors you'll hit
- Mounting two routes onto the same final path → build-time conflict error
  naming the path. Rename one route or move the mount prefix.
- Two different `{param}` names at the same position (`/{id}` vs `/{todo_id}`)
  → build-time conflict; pick one name.

## Anti-patterns
- Don't reach into another module's internals — route crates expose `module()`
  and nothing else; shared types live in the app's `shared` crate.
- Don't use module middleware for cross-cutting concerns that belong app-level
  (logging, request IDs); module middleware is for subtree policy (auth zones,
  rate limits).
