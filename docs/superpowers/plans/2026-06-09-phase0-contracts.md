# jerrycan Phase 0 — Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove jerrycan's hardest API signatures (DI + Module) compile and run as real Rust on tokio+hyper, then freeze the core contracts: AI-native docs whose every example is a compiling doc-test, MCP tool JSON contracts, and the CLI UX spec.

**Architecture:** `jerrycan-core` gets a minimal-but-real implementation of the full public API surface (App, Module, routing, extractors, DI with async/nested/cached/overridable dependencies, middleware, TestApp, serve-on-hyper). The docs in `docs/ai/*.md` are `include_str!`-mounted into the crate so `cargo test --doc` compiles every example — docs are the executable spec. MCP/CLI contracts are JSON Schema + markdown, syntax-gated by tests in the `jerrycan` binary crate.

**Tech Stack:** Rust edition 2024 (MSRV = stable), tokio 1, hyper 1 (+hyper-util), http 1, http-body-util, serde/serde_json/serde_urlencoded, bytes. No tower, no axum — the developer-facing surface is 100% jerrycan's (spec §2 "ground-up on tokio+hyper").

**Spec:** `docs/superpowers/specs/2026-06-09-jerrycan-design.md` (Phase 0 row of §11; core API of §4; generated-shape of §5; platform contracts of §7).

---

## File Structure

```
Cargo.toml                                  # MODIFY: [workspace.dependencies]
crates/jerrycan-core/
├── Cargo.toml                              # MODIFY: real deps
└── src/
    ├── lib.rs                              # MODIFY: modules + prelude + doc mounts
    ├── error.rs                            # CREATE: Error/Result, stable JC#### codes
    ├── response.rs                         # CREATE: Response, IntoResponse, Json/Created/NoContent
    ├── extract.rs                          # CREATE: RequestCtx, FromRequest, Path/Query/Json
    ├── dep.rs                              # CREATE: Dep<T>, DepEnv, resolver, DepFactory macro, overrides
    ├── handler.rs                          # CREATE: Handler trait + arity macro → BoxHandlerFn
    ├── router.rs                           # CREATE: MethodRouter, get/post/…, segment trie, conflicts
    ├── middleware.rs                       # CREATE: Middleware trait + Next chain
    ├── module.rs                           # CREATE: Module builder (routes/mount/provide/middleware)
    ├── app.rs                              # CREATE: App builder, build()→BuiltApp dispatch, serve()
    ├── test_client.rs                      # CREATE: TestApp + override_dep + TestResponse
    └── docs.rs                             # CREATE: #[doc=include_str!] mounts for docs/ai/*.md
crates/jerrycan-core/tests/
    ├── di.rs                               # CREATE: nested/cached/overridden dependency tests
    ├── module.rs                           # CREATE: nesting, prefixes, scoped deps/middleware
    └── e2e.rs                              # CREATE: in-process CRUD + real-socket smoke test
docs/ai/
    ├── 01-app.md                           # CREATE: App + routing page (doc-tested)
    ├── 02-modules.md                       # CREATE: Module + subroutes page
    ├── 03-extractors.md                    # CREATE: Path/Query/Json page
    ├── 04-dependencies.md                  # CREATE: DI page (the signature feature)
    ├── 05-errors.md                        # CREATE: Error/codes page
    ├── 06-middleware.md                    # CREATE: Middleware page
    └── 07-testing.md                       # CREATE: TestApp page
docs/contracts/
    ├── mcp-tools.json                      # CREATE: 9 MCP tool contracts (JSON Schema)
    ├── design-schema.json                  # CREATE: design.json schema (module-grouped)
    └── cli-ux.md                           # CREATE: CLI UX spec
crates/jerrycan/tests/contracts.rs          # CREATE: contracts parse + invariant tests
.github/workflows/ci.yml                    # CREATE: fmt + clippy + tests (incl. doc-tests)
```

**Conventions for every task:** run commands from the repo root. Every commit message is plain "what changed" (no Claude/co-author lines — repo rule). `#![forbid(unsafe_code)]` stays in every crate. All public items get doc comments with runnable examples by Task 19 — write them as you go.

---

### Task 1: Workspace dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/jerrycan-core/Cargo.toml`

- [ ] **Step 1: Add `[workspace.dependencies]` to the root `Cargo.toml`**

Append to `Cargo.toml` (keep the existing `[workspace]` and `[workspace.package]` tables):

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread", "net", "time", "sync"] }
hyper = { version = "1", features = ["http1", "server"] }
hyper-util = { version = "0.1", features = ["tokio", "server", "http1"] }
http = "1"
http-body-util = "0.1"
bytes = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_urlencoded = "0.7"
```

- [ ] **Step 2: Give `jerrycan-core` its dependencies**

Replace `crates/jerrycan-core/Cargo.toml` contents with:

```toml
[package]
name = "jerrycan-core"
description = "Core framework of the jerrycan platform: routing, extractors, dependency injection, middleware. Name reservation; real releases begin at 0.1.0. https://jerrycan.cc"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true

[dependencies]
tokio.workspace = true
hyper.workspace = true
hyper-util.workspace = true
http.workspace = true
http-body-util.workspace = true
bytes.workspace = true
serde.workspace = true
serde_json.workspace = true
serde_urlencoded.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "net", "time", "sync", "io-util"] }
```

- [ ] **Step 3: Verify the workspace still builds**

Run: `cargo check --workspace`
Expected: `Finished` with no errors (placeholder lib.rs files still compile).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jerrycan-core/Cargo.toml
git commit -m "Add tokio/hyper/serde dependency floor to jerrycan-core"
```

---

### Task 2: Error type with stable error codes

**Files:**
- Create: `crates/jerrycan-core/src/error.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing test (inside `error.rs` as a unit test)**

Create `crates/jerrycan-core/src/error.rs`:

```rust
//! jerrycan's single error type. Every error carries a stable `code` (JC####)
//! that maps to a documentation anchor — the error-driven-docs contract (spec §8).

use http::StatusCode;
use std::fmt;

/// Convenience alias used across jerrycan and generated apps.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_status_and_stable_code() {
        assert_eq!(Error::not_found().status(), StatusCode::NOT_FOUND);
        assert_eq!(Error::not_found().code(), "JC0404");
        assert_eq!(Error::method_not_allowed().code(), "JC0405");
        assert_eq!(Error::bad_request("nope").status(), StatusCode::BAD_REQUEST);
        assert_eq!(Error::payload_too_large().code(), "JC0413");
        assert_eq!(Error::unprocessable("bad field").code(), "JC0422");
        assert_eq!(Error::internal("boom").status(), StatusCode::INTERNAL_SERVER_ERROR);
        let e = Error::missing_dependency("app::Db");
        assert_eq!(e.code(), "JC1001");
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(e.message().contains("app::Db"));
    }

    #[test]
    fn display_includes_code_and_message() {
        let e = Error::bad_request("missing body");
        assert_eq!(format!("{e}"), "JC0400: missing body");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p jerrycan-core` (after adding `mod error;` — see Step 3's lib.rs edit; the failure is a compile error: `Error` not defined). Expected: compilation FAILS mentioning `cannot find type Error`.

- [ ] **Step 3: Implement `Error` (same file, above the tests)**

Add between the `pub type Result` line and `#[cfg(test)]`:

```rust
/// The one error type of the framework (spec §4.1 "Errors").
///
/// Production responses render only `code` + `message` as JSON; internals
/// (sources, backtraces) are for logs — enforced in Phase 1's observe layer.
#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl Error {
    /// Build an error with an explicit status and stable code.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "JC0400", message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "JC0404", "not found")
    }
    pub fn method_not_allowed() -> Self {
        Self::new(StatusCode::METHOD_NOT_ALLOWED, "JC0405", "method not allowed")
    }
    pub fn payload_too_large() -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "JC0413", "payload too large")
    }
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "JC0422", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "JC0500", message)
    }
    /// A handler or dependency asked for a type no provider supplies (spec §4.3).
    pub fn missing_dependency(type_name: &str) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "JC1001",
            format!("no provider registered for dependency `{type_name}`"),
        )
    }

    pub fn status(&self) -> StatusCode { self.status }
    pub fn code(&self) -> &'static str { self.code }
    pub fn message(&self) -> &str { &self.message }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}
```

Replace `crates/jerrycan-core/src/lib.rs` contents with:

```rust
//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. See https://jerrycan.cc
#![forbid(unsafe_code)]

pub mod error;

pub use error::{Error, Result};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p jerrycan-core`
Expected: `test error::tests::errors_carry_status_and_stable_code ... ok`, `test error::tests::display_includes_code_and_message ... ok` — 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/error.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add Error type with stable JC#### error codes"
```

---
### Task 3: Response model — `IntoResponse`, `Json`, `Created`, `NoContent`

**Files:**
- Create: `crates/jerrycan-core/src/response.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan-core/src/response.rs`:

```rust
//! Response model. Handlers return anything implementing [`IntoResponse`];
//! `Result<T, Error>` renders errors as `{"code","message"}` JSON (spec §4.1).

use crate::error::Error;
use bytes::Bytes;
use http::{header, HeaderValue, StatusCode};
use http_body_util::Full;
use serde::Serialize;

/// The concrete response type of the spike. Streaming bodies arrive in Phase 1
/// behind the same `IntoResponse` seam, so handler signatures won't change.
pub type Response = http::Response<Full<Bytes>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(r: &Response) -> String {
        // Full<Bytes> exposes its data without polling via a clone of the inner frame.
        let bytes = r.body().clone();
        let collected = futures_executor_lite(bytes);
        String::from_utf8(collected.to_vec()).unwrap()
    }

    /// Minimal "block on a Full body" helper so unit tests need no runtime.
    fn futures_executor_lite(full: Full<Bytes>) -> Bytes {
        use http_body_util::BodyExt;
        let fut = full.collect();
        // Full's collect future is immediately ready; poll it once by hand.
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(Ok(c)) => c.to_bytes(),
            _ => panic!("Full body was not immediately ready"),
        }
    }

    #[test]
    fn str_becomes_200_text() {
        let r = "hello".into_response();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(body_of(&r), "hello");
    }

    #[test]
    fn json_wrapper_sets_content_type() {
        #[derive(Serialize)]
        struct Todo { id: u32 }
        let r = Json(Todo { id: 7 }).into_response();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(body_of(&r), r#"{"id":7}"#);
    }

    #[test]
    fn created_is_201_and_no_content_is_204() {
        #[derive(Serialize)]
        struct T { ok: bool }
        assert_eq!(Created(T { ok: true }).into_response().status(), StatusCode::CREATED);
        let r = NoContent.into_response();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(body_of(&r), "");
    }

    #[test]
    fn errors_render_code_and_message_json() {
        let r = Error::not_found().into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_of(&r), r#"{"code":"JC0404","message":"not found"}"#);
    }

    #[test]
    fn result_renders_ok_or_err() {
        let ok: crate::Result<&'static str> = Ok("fine");
        assert_eq!(ok.into_response().status(), StatusCode::OK);
        let err: crate::Result<&'static str> = Err(Error::bad_request("x"));
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core response`
Expected: compile FAILURE — `IntoResponse`, `Json`, `Created`, `NoContent` not found.

- [ ] **Step 3: Implement (insert above `#[cfg(test)]`)**

```rust
/// Conversion of handler return values into HTTP responses.
pub trait IntoResponse {
    fn into_response(self) -> Response;
}

/// JSON body wrapper: `Json(value)` serializes with `application/json`.
pub struct Json<T>(pub T);

/// 201 Created with a JSON body.
pub struct Created<T>(pub T);

/// 204 No Content.
pub struct NoContent;

fn full(status: StatusCode, content_type: &'static str, body: impl Into<Bytes>) -> Response {
    let mut r = http::Response::new(Full::new(body.into()));
    *r.status_mut() = status;
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r
}

fn json_body<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => full(status, "application/json", bytes),
        Err(e) => Error::internal(format!("response serialization failed: {e}")).into_response(),
    }
}

impl IntoResponse for Response {
    fn into_response(self) -> Response { self }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        full(StatusCode::OK, "text/plain; charset=utf-8", self.as_bytes().to_vec())
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        full(StatusCode::OK, "text/plain; charset=utf-8", self.into_bytes())
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        let mut r = http::Response::new(Full::new(Bytes::new()));
        *r.status_mut() = self;
        r
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response { json_body(StatusCode::OK, &self.0) }
}

impl<T: Serialize> IntoResponse for Created<T> {
    fn into_response(self) -> Response { json_body(StatusCode::CREATED, &self.0) }
}

impl IntoResponse for NoContent {
    fn into_response(self) -> Response {
        let mut r = http::Response::new(Full::new(Bytes::new()));
        *r.status_mut() = StatusCode::NO_CONTENT;
        r
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        json_body(self.status(), &ErrorBody { code: self.code(), message: self.message() })
    }
}

impl<T: IntoResponse> IntoResponse for crate::Result<T> {
    fn into_response(self) -> Response {
        match self {
            Ok(v) => v.into_response(),
            Err(e) => e.into_response(),
        }
    }
}
```

In `crates/jerrycan-core/src/lib.rs` add below `pub mod error;`:

```rust
pub mod response;

pub use response::{Created, IntoResponse, Json, NoContent, Response};
```

(Keep the existing `pub use error::…` line.)

NOTE: `std::task::Waker::noop()` is stable since Rust 1.85. If the build complains, replace the helper with `futures_executor_lite` using a manual no-op `RawWaker` — but prefer the stdlib call.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core response`
Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/response.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add IntoResponse model with Json, Created, NoContent and error JSON rendering"
```

---

### Task 4: `RequestCtx`, `FromRequest`, and the `Path`/`Query`/`Json` extractors

**Files:**
- Create: `crates/jerrycan-core/src/extract.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

Context for the engineer: `RequestCtx` is the single mutable view of an in-flight request. Extractors pull typed values out of it; the DI resolver (Task 5) also lives behind it. Handlers never see hyper types.

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan-core/src/extract.rs`:

```rust
//! Request context and extractors (spec §4.1). Everything a handler needs is
//! visible in its signature; each parameter implements [`FromRequest`].

use crate::dep::DepResolver;
use crate::error::{Error, Result};
use crate::response::Json;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::str::FromStr;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep::DepEnv;
    use std::sync::Arc;

    fn ctx(uri: &str, body: &str) -> RequestCtx {
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(())
            .unwrap();
        let (parts, ()) = req.into_parts();
        RequestCtx::new(parts, Bytes::from(body.to_string()), DepResolver::new(Arc::new(DepEnv::default()), Default::default()))
    }

    #[tokio::test]
    async fn path_extracts_typed_param() {
        let mut c = ctx("/todos/42", "");
        c.params.push(("id".into(), "42".into()));
        let Path(id): Path<i64> = Path::from_request(&mut c).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn path_with_wrong_type_is_400() {
        let mut c = ctx("/todos/abc", "");
        c.params.push(("id".into(), "abc".into()));
        let err = Path::<i64>::from_request(&mut c).await.unwrap_err();
        assert_eq!(err.code(), "JC0400");
    }

    #[tokio::test]
    async fn query_deserializes_struct() {
        #[derive(serde::Deserialize)]
        struct Page { limit: u32, offset: u32 }
        let mut c = ctx("/todos?limit=10&offset=20", "");
        let Query(p): Query<Page> = Query::from_request(&mut c).await.unwrap();
        assert_eq!((p.limit, p.offset), (10, 20));
    }

    #[tokio::test]
    async fn json_body_deserializes_and_bad_json_is_422() {
        #[derive(serde::Deserialize)]
        struct NewTodo { title: String }
        let mut c = ctx("/todos", r#"{"title":"x"}"#);
        let Json(t): Json<NewTodo> = Json::from_request(&mut c).await.unwrap();
        assert_eq!(t.title, "x");

        let mut bad = ctx("/todos", r#"{"title":"#);
        let err = Json::<NewTodo>::from_request(&mut bad).await.unwrap_err();
        assert_eq!(err.code(), "JC0422");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core extract`
Expected: compile FAILURE — `RequestCtx`, `Path`, `Query`, `crate::dep` not found. (Two modules are created across Tasks 4–5; the tests go green at the end of Task 5.)

- [ ] **Step 3: Implement `RequestCtx` + extractors (insert above `#[cfg(test)]`)**

```rust
/// The mutable view of one in-flight request. Handlers receive extractors,
/// not this type; middleware and the DI resolver work through it.
pub struct RequestCtx {
    pub(crate) parts: http::request::Parts,
    pub(crate) body: Bytes,
    /// Path parameters captured by the router, in route order.
    pub(crate) params: Vec<(String, String)>,
    pub(crate) deps: DepResolver,
}

impl RequestCtx {
    pub(crate) fn new(parts: http::request::Parts, body: Bytes, deps: DepResolver) -> Self {
        Self { parts, body, params: Vec::new(), deps }
    }

    pub fn method(&self) -> &http::Method { &self.parts.method }
    pub fn uri(&self) -> &http::Uri { &self.parts.uri }
    pub fn headers(&self) -> &http::HeaderMap { &self.parts.headers }
}

/// Types that can be produced from the request. Implemented by all extractors
/// and by `Dep<T>` (see `dep` module).
pub trait FromRequest: Sized + Send {
    fn from_request(ctx: &mut RequestCtx) -> impl Future<Output = Result<Self>> + Send;
}

/// Typed path parameter: `Path<i64>` grabs the first `{param}` in the route.
/// (Multi-param `Path<(A, B)>` lands in Phase 1; one param per route segment
/// covers the spike and the docs say so.)
pub struct Path<T>(pub T);

impl<T> FromRequest for Path<T>
where
    T: FromStr + Send,
    T::Err: std::fmt::Display,
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let (name, raw) = ctx
            .params
            .first()
            .ok_or_else(|| Error::internal("route has no path parameters"))?;
        raw.parse::<T>()
            .map(Path)
            .map_err(|e| Error::bad_request(format!("invalid path parameter `{name}`: {e}")))
    }
}

/// Typed query string: `Query<MyParams>` via serde.
pub struct Query<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Query<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let q = ctx.parts.uri.query().unwrap_or("");
        serde_urlencoded::from_str::<T>(q)
            .map(Query)
            .map_err(|e| Error::bad_request(format!("invalid query string: {e}")))
    }
}

impl<T: DeserializeOwned + Send> FromRequest for Json<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        serde_json::from_slice::<T>(&ctx.body)
            .map(Json)
            .map_err(|e| Error::unprocessable(format!("invalid JSON body: {e}")))
    }
}
```

In `crates/jerrycan-core/src/lib.rs` add:

```rust
pub mod extract;

pub use extract::{FromRequest, Path, Query, RequestCtx};
```

- [ ] **Step 4: Do NOT expect green yet**

Run: `cargo check -p jerrycan-core`
Expected: FAILURE only about the missing `crate::dep` module — everything else resolves. That is the cue to start Task 5.

- [ ] **Step 5: Commit (joint commit happens at the end of Task 5 when the build is green)**

No commit yet — broken builds are never committed.

---
### Task 5: Dependency injection core — `DepEnv`, `Dep<T>`, per-request resolver

**Files:**
- Create: `crates/jerrycan-core/src/dep.rs`
- Modify: `crates/jerrycan-core/src/error.rs` (one new constructor)
- Modify: `crates/jerrycan-core/src/lib.rs`

Context: this is the spec's signature feature (§4.3). Resolution order per request: **request-cache → overrides → singletons → factories**. Factories may themselves extract `Dep<_>` — nested DI (Task 6). Everything is memoized per request.

- [ ] **Step 1: Add the cycle-guard error constructor**

In `crates/jerrycan-core/src/error.rs`, after `missing_dependency`, add:

```rust
    /// Dependency factories recursed past the depth limit (cycle, or absurd chain).
    pub fn dependency_cycle() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "JC1002",
            "dependency cycle or chain deeper than 32",
        )
    }
```

And extend the unit test `errors_carry_status_and_stable_code` with:

```rust
        assert_eq!(Error::dependency_cycle().code(), "JC1002");
```

- [ ] **Step 2: Write the failing tests for value providers + memoization**

Create `crates/jerrycan-core/src/dep.rs`:

```rust
//! Dependency injection (spec §4.3) — async, nested, per-request memoized,
//! override-able in tests. Resolution order: cache → overrides → singletons → factories.

use crate::error::{Error, Result};
use crate::extract::{FromRequest, RequestCtx};
use std::any::{type_name, Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    pub(crate) fn test_ctx(env: DepEnv) -> RequestCtx {
        let req = http::Request::builder().uri("/").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        RequestCtx::new(
            parts,
            Bytes::new(),
            DepResolver::new(Arc::new(env), Arc::new(HashMap::new())),
        )
    }

    struct Config { name: &'static str }

    #[tokio::test]
    async fn value_provider_resolves_and_derefs() {
        let mut env = DepEnv::default();
        env.insert_value(Config { name: "prod" });
        let mut ctx = test_ctx(env);
        let cfg: Dep<Config> = Dep::from_request(&mut ctx).await.unwrap();
        assert_eq!(cfg.name, "prod"); // Deref<Target = Config>
    }

    #[tokio::test]
    async fn missing_provider_is_jc1001() {
        let mut ctx = test_ctx(DepEnv::default());
        let err = Dep::<Config>::from_request(&mut ctx).await.unwrap_err();
        assert_eq!(err.code(), "JC1001");
        assert!(err.message().contains("Config"));
    }

    #[tokio::test]
    async fn same_request_yields_same_arc() {
        let mut env = DepEnv::default();
        env.insert_value(Config { name: "x" });
        let mut ctx = test_ctx(env);
        let a = ctx.resolve::<Config>().await.unwrap();
        let b = ctx.resolve::<Config>().await.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p jerrycan-core dep` (add `pub mod dep;` + `pub use dep::Dep;` to `lib.rs` first).
Expected: compile FAILURE — `DepEnv`, `DepResolver`, `Dep` not defined.

- [ ] **Step 4: Implement the machinery (insert above `#[cfg(test)]`)**

```rust
pub(crate) type AnyArc = Arc<dyn Any + Send + Sync>;
pub(crate) type ProviderFut<'a> = Pin<Box<dyn Future<Output = Result<AnyArc>> + Send + 'a>>;
pub(crate) type ProviderFn =
    Arc<dyn for<'a> Fn(&'a mut RequestCtx) -> ProviderFut<'a> + Send + Sync>;

/// The provider set effective for a route: app providers merged with the
/// route's module chain — inner module wins (spec §4.2 scoping).
#[derive(Default, Clone)]
pub struct DepEnv {
    pub(crate) singletons: HashMap<TypeId, AnyArc>,
    pub(crate) factories: HashMap<TypeId, ProviderFn>,
}

impl DepEnv {
    /// Register an already-built value; shared by every request (singleton scope).
    pub(crate) fn insert_value<T: Send + Sync + 'static>(&mut self, value: T) {
        let id = TypeId::of::<T>();
        self.singletons.insert(id, Arc::new(value));
        self.factories.remove(&id);
    }

    /// Later entries shadow earlier ones — used to layer module envs over the app env.
    pub(crate) fn merge_from(&mut self, inner: &DepEnv) {
        for (k, v) in &inner.singletons {
            self.singletons.insert(*k, v.clone());
            self.factories.remove(k);
        }
        for (k, f) in &inner.factories {
            self.factories.insert(*k, f.clone());
            self.singletons.remove(k);
        }
    }
}

/// Per-request resolution state. Cheap to create; memoizes by `TypeId`.
pub struct DepResolver {
    pub(crate) env: Arc<DepEnv>,
    pub(crate) overrides: Arc<HashMap<TypeId, AnyArc>>,
    pub(crate) cache: HashMap<TypeId, AnyArc>,
    pub(crate) depth: u8,
}

impl DepResolver {
    pub(crate) fn new(env: Arc<DepEnv>, overrides: Arc<HashMap<TypeId, AnyArc>>) -> Self {
        Self { env, overrides, cache: HashMap::new(), depth: 0 }
    }
}

const MAX_RESOLVE_DEPTH: u8 = 32;

impl RequestCtx {
    /// Resolve a dependency by type, memoized for this request (spec §4.3).
    pub async fn resolve<T: Send + Sync + 'static>(&mut self) -> Result<Arc<T>> {
        let id = TypeId::of::<T>();
        if let Some(v) = self.deps.cache.get(&id) {
            return downcast::<T>(v.clone());
        }
        if let Some(v) = self.deps.overrides.get(&id).cloned() {
            self.deps.cache.insert(id, v.clone());
            return downcast::<T>(v);
        }
        if let Some(v) = self.deps.env.singletons.get(&id).cloned() {
            self.deps.cache.insert(id, v.clone());
            return downcast::<T>(v);
        }
        let factory = match self.deps.env.factories.get(&id) {
            Some(f) => f.clone(),
            None => return Err(Error::missing_dependency(type_name::<T>())),
        };
        self.deps.depth += 1;
        if self.deps.depth > MAX_RESOLVE_DEPTH {
            self.deps.depth -= 1;
            return Err(Error::dependency_cycle());
        }
        let produced = (*factory)(self).await;
        self.deps.depth -= 1;
        let v = produced?;
        self.deps.cache.insert(id, v.clone());
        downcast::<T>(v)
    }
}

fn downcast<T: Send + Sync + 'static>(v: AnyArc) -> Result<Arc<T>> {
    v.downcast::<T>()
        .map_err(|_| Error::internal("dependency type mismatch (provider/consumer disagree)"))
}

/// A resolved dependency. Derefs to `T`; cloning is `Arc`-cheap.
pub struct Dep<T: ?Sized>(pub(crate) Arc<T>);

impl<T: ?Sized> Deref for Dep<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.0 }
}

impl<T: ?Sized> Clone for Dep<T> {
    fn clone(&self) -> Self { Dep(self.0.clone()) }
}

impl<T: Send + Sync + 'static> FromRequest for Dep<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        ctx.resolve::<T>().await.map(Dep)
    }
}
```

In `crates/jerrycan-core/src/lib.rs` add:

```rust
pub mod dep;

pub use dep::Dep;
```

- [ ] **Step 5: Run to verify everything so far passes**

Run: `cargo test -p jerrycan-core`
Expected: all `error`, `response`, `extract` and the 3 new `dep` tests PASS (extract's tests compile now that `crate::dep` exists).

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan-core/src/dep.rs crates/jerrycan-core/src/extract.rs crates/jerrycan-core/src/error.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add DI core: DepEnv, per-request resolver with memoization, Dep extractor"
```

---

### Task 6: Nested async dependency factories (`DepFactory`)

**Files:**
- Modify: `crates/jerrycan-core/src/dep.rs`

Context: `async fn current_user(session: Dep<Session>, db: Dep<Db>) -> Result<User>` must register as a provider for `User`. Arguments are extractors, so factories nest arbitrarily. This is the exact FastAPI `Depends` ergonomic, statically typed.

- [ ] **Step 1: Write the failing tests (append inside `mod tests`)**

```rust
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)] struct Db { url: &'static str }
    struct Session { token: String }
    struct User { name: String }

    static SESSION_RESOLVES: AtomicUsize = AtomicUsize::new(0);

    async fn make_session() -> crate::Result<Session> {
        SESSION_RESOLVES.fetch_add(1, Ordering::SeqCst);
        Ok(Session { token: "t-1".into() })
    }

    async fn current_user(session: Dep<Session>, db: Dep<Db>) -> crate::Result<User> {
        Ok(User { name: format!("{}@{}", session.token, db.url) })
    }

    fn nested_env() -> DepEnv {
        let mut env = DepEnv::default();
        env.insert_value(Db { url: "pg://prod" });
        env.insert_factory(make_session);
        env.insert_factory(current_user);
        env
    }

    #[tokio::test]
    async fn factories_nest_and_resolve_async() {
        let mut ctx = test_ctx(nested_env());
        let user = ctx.resolve::<User>().await.unwrap();
        assert_eq!(user.name, "t-1@pg://prod");
    }

    #[tokio::test]
    async fn nested_deps_are_memoized_once_per_request() {
        SESSION_RESOLVES.store(0, Ordering::SeqCst);
        let mut ctx = test_ctx(nested_env());
        // Both of these need Session (one directly, one through current_user).
        let _s = ctx.resolve::<Session>().await.unwrap();
        let _u = ctx.resolve::<User>().await.unwrap();
        assert_eq!(SESSION_RESOLVES.load(Ordering::SeqCst), 1, "memoized within request");

        // A new request resolves afresh — request scope, not singleton.
        let mut ctx2 = test_ctx(nested_env());
        let _u2 = ctx2.resolve::<User>().await.unwrap();
        assert_eq!(SESSION_RESOLVES.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn self_referential_factory_hits_cycle_guard() {
        struct Loopy;
        async fn loopy(_again: Dep<Loopy>) -> crate::Result<Loopy> { Ok(Loopy) }
        let mut env = DepEnv::default();
        env.insert_factory(loopy);
        let mut ctx = test_ctx(env);
        let err = ctx.resolve::<Loopy>().await.unwrap_err();
        assert_eq!(err.code(), "JC1002");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core dep`
Expected: compile FAILURE — `insert_factory` and `DepFactory` not defined.

- [ ] **Step 3: Implement `DepFactory` + the arity macro (insert after the `Dep` impls)**

```rust
/// Async functions registrable as providers. `Args` is the tuple of extractor
/// parameters; `T` the produced dependency. Implemented for arities 0..=8.
pub trait DepFactory<Args, T>: Send + Sync + 'static {
    fn into_provider(self) -> ProviderFn;
}

macro_rules! impl_dep_factory {
    ($($A:ident),*) => {
        impl<F, Fut, T, $($A,)*> DepFactory<($($A,)*), T> for F
        where
            F: Fn($($A),*) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = Result<T>> + Send,
            T: Send + Sync + 'static,
            $($A: FromRequest + 'static,)*
        {
            fn into_provider(self) -> ProviderFn {
                Arc::new(move |ctx: &mut RequestCtx| {
                    let f = self.clone();
                    Box::pin(async move {
                        #[allow(non_snake_case, unused_variables)]
                        {
                            $(let $A = <$A as FromRequest>::from_request(ctx).await?;)*
                            let value = f($($A),*).await?;
                            Ok(Arc::new(value) as AnyArc)
                        }
                    })
                })
            }
        }
    };
}

impl_dep_factory!();
impl_dep_factory!(A1);
impl_dep_factory!(A1, A2);
impl_dep_factory!(A1, A2, A3);
impl_dep_factory!(A1, A2, A3, A4);
impl_dep_factory!(A1, A2, A3, A4, A5);
impl_dep_factory!(A1, A2, A3, A4, A5, A6);
impl_dep_factory!(A1, A2, A3, A4, A5, A6, A7);
impl_dep_factory!(A1, A2, A3, A4, A5, A6, A7, A8);
```

And add to `DepEnv` (next to `insert_value`):

```rust
    /// Register an async factory; runs at most once per request (request scope).
    pub(crate) fn insert_factory<F, Args, T>(&mut self, factory: F)
    where
        F: DepFactory<Args, T>,
        T: Send + Sync + 'static,
    {
        let id = TypeId::of::<T>();
        self.factories.insert(id, factory.into_provider());
        self.singletons.remove(&id);
    }
```

Compiler-fight notes for the engineer (these are the known sharp edges, pre-solved):
- The closure MUST be written `move |ctx: &mut RequestCtx|` with the explicit type so it satisfies the higher-ranked `for<'a> Fn(&'a mut RequestCtx) -> ProviderFut<'a>` — without the annotation, inference picks a single lifetime and the `Arc::new` coercion fails.
- Inside the macro body, type idents double as variable bindings (`let $A = …`) — that's what the `#[allow(non_snake_case)]` is for. Standard tuple-arity trick.
- Passing `ctx` to each `from_request` works via implicit reborrow; do not `clone` or restructure it.
- `self_referential_factory_hits_cycle_guard`: the recursion happens through the boxed `ProviderFut`, so there's no infinite compile-time type — the runtime depth guard (JC1002) breaks the loop.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core dep`
Expected: all 6 dep tests PASS (3 from Task 5 + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/dep.rs
git commit -m "Add nested async dependency factories with arity macro and cycle guard"
```

---

### Task 7: Dependency overrides (the testing seam)

**Files:**
- Modify: `crates/jerrycan-core/src/dep.rs`

- [ ] **Step 1: Write the failing test (append inside `mod tests`)**

```rust
    #[tokio::test]
    async fn overrides_shadow_both_values_and_factories() {
        // Real env: Db value + Session factory.
        let mut env = nested_env();
        env.insert_value(Db { url: "pg://prod" });

        // Overrides replace them without touching the env.
        let mut overrides: HashMap<TypeId, AnyArc> = HashMap::new();
        overrides.insert(TypeId::of::<Db>(), Arc::new(Db { url: "sqlite::memory:" }));
        overrides.insert(
            TypeId::of::<Session>(),
            Arc::new(Session { token: "fake".into() }),
        );

        let req = http::Request::builder().uri("/").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(Arc::new(env), Arc::new(overrides)),
        );

        let user = ctx.resolve::<User>().await.unwrap();
        assert_eq!(user.name, "fake@sqlite::memory:");
    }
```

- [ ] **Step 2: Run to verify it passes ALREADY**

Run: `cargo test -p jerrycan-core dep`
Expected: PASS — Task 5's resolver already checks `overrides` before `env`. This test pins the contract so it can never regress; the public `TestApp::override_dep` API arrives in Task 13.

(If it fails, the resolver's lookup order is wrong — fix `resolve` to check `overrides` after `cache` and before `singletons`.)

- [ ] **Step 3: Commit**

```bash
git add crates/jerrycan-core/src/dep.rs
git commit -m "Pin dependency override resolution order with regression test"
```

---
### Task 8: `Handler` trait — plain async fns become routable

**Files:**
- Create: `crates/jerrycan-core/src/handler.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/jerrycan-core/src/handler.rs`:

```rust
//! Handler abstraction (spec §4.1): a handler is a plain async fn whose
//! parameters implement [`FromRequest`] and whose return implements
//! [`IntoResponse`]. Extraction failures short-circuit into error responses.

use crate::extract::{FromRequest, RequestCtx};
use crate::response::{IntoResponse, Response};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep::{Dep, DepEnv, DepResolver};
    use crate::response::Json;
    use crate::Path;
    use std::collections::HashMap;

    struct Greeting { word: &'static str }

    async fn greet(g: Dep<Greeting>, Path(id): Path<u32>) -> crate::Result<Json<String>> {
        Ok(Json(format!("{} #{id}", g.word)))
    }

    #[tokio::test]
    async fn handler_extracts_runs_and_responds() {
        let mut env = DepEnv::default();
        env.insert_value(Greeting { word: "hi" });
        let req = http::Request::builder().uri("/greet/7").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(std::sync::Arc::new(env), std::sync::Arc::new(HashMap::new())),
        );
        ctx.params.push(("id".into(), "7".into()));

        let h = greet.into_handler_fn();
        let res = (*h)(&mut ctx).await; // explicit deref: Arc<dyn Fn> is not directly callable
        assert_eq!(res.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn extraction_failure_short_circuits_to_error_response() {
        // No Greeting provider registered → Dep extraction fails → JC1001 → 500.
        let req = http::Request::builder().uri("/greet/7").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(Default::default(), std::sync::Arc::new(HashMap::new())),
        );
        ctx.params.push(("id".into(), "7".into()));

        let h = greet.into_handler_fn();
        let res = (*h)(&mut ctx).await;
        assert_eq!(res.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core handler` (add `pub mod handler;` to lib.rs plus `pub use handler::Handler;`).
Expected: compile FAILURE — `into_handler_fn` not defined.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// Type-erased handler as stored by the router.
pub(crate) type BoxHandlerFn = Arc<
    dyn for<'a> Fn(&'a mut RequestCtx) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>>
        + Send
        + Sync,
>;

/// Implemented for async fns of arity 0..=8 over [`FromRequest`] parameters.
pub trait Handler<Args>: Send + Sync + 'static {
    #[doc(hidden)]
    fn into_handler_fn(self) -> BoxHandlerFn;
}

macro_rules! impl_handler {
    ($($A:ident),*) => {
        impl<F, Fut, R, $($A,)*> Handler<($($A,)*)> for F
        where
            F: Fn($($A),*) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = R> + Send,
            R: IntoResponse,
            $($A: FromRequest + 'static,)*
        {
            fn into_handler_fn(self) -> BoxHandlerFn {
                Arc::new(move |ctx: &mut RequestCtx| {
                    let f = self.clone();
                    Box::pin(async move {
                        #[allow(non_snake_case, unused_variables)]
                        {
                            $(
                                let $A = match <$A as FromRequest>::from_request(ctx).await {
                                    Ok(v) => v,
                                    Err(e) => return e.into_response(),
                                };
                            )*
                            f($($A),*).await.into_response()
                        }
                    })
                })
            }
        }
    };
}

impl_handler!();
impl_handler!(A1);
impl_handler!(A1, A2);
impl_handler!(A1, A2, A3);
impl_handler!(A1, A2, A3, A4);
impl_handler!(A1, A2, A3, A4, A5);
impl_handler!(A1, A2, A3, A4, A5, A6);
impl_handler!(A1, A2, A3, A4, A5, A6, A7);
impl_handler!(A1, A2, A3, A4, A5, A6, A7, A8);
```

(Test note: `handler_extracts_runs_and_responds` destructures `Path(id)` in the fn signature — that pattern must keep working; it's in the docs.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core handler`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/handler.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add Handler trait turning plain async fns into routable handlers"
```

---

### Task 10: Router — `MethodRouter`, segment trie, startup conflict detection

> **Numbering note:** Task 9 (Middleware) executes BEFORE this task but appears after it in this document — the router stores middleware chains, so `middleware.rs` must exist first. Follow the numbers, not the page order.

**Files:**
- Create: `crates/jerrycan-core/src/router.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan-core/src/router.rs`:

```rust
//! Method routing + segment trie with `{param}` captures (spec §4.1).
//! Conflicting routes are detected at build time — fail loud before serving.
//! NOTE: percent-decoding of path segments is deliberately Phase 1 (with fuzzing).

use crate::dep::DepEnv;
use crate::error::{Error, Result};
use crate::handler::{BoxHandlerFn, Handler};
use crate::middleware::Middleware;
use http::Method;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::IntoResponse;

    fn dummy_handler() -> BoxHandlerFn {
        Arc::new(move |_ctx: &mut crate::RequestCtx| Box::pin(async move { "ok".into_response() }))
    }

    fn endpoint(methods: &[Method]) -> Endpoint {
        let mut map = HashMap::new();
        for m in methods {
            map.insert(m.clone(), dummy_handler());
        }
        Endpoint { methods: map, env: Arc::new(DepEnv::default()), middleware: Arc::from(vec![]) }
    }

    #[test]
    fn static_and_param_segments_match() {
        let mut t = Trie::default();
        t.insert("/todos", endpoint(&[Method::GET])).unwrap();
        t.insert("/todos/{id}", endpoint(&[Method::GET, Method::DELETE])).unwrap();
        t.insert("/todos/{id}/comments", endpoint(&[Method::GET])).unwrap();

        match t.find("/todos/42/comments", &Method::GET) {
            RouteMatch::Found { params, .. } => assert_eq!(params, vec![("id".to_string(), "42".to_string())]),
            _ => panic!("expected match"),
        }
        assert!(matches!(t.find("/todos/42", &Method::DELETE), RouteMatch::Found { .. }));
    }

    #[test]
    fn unknown_path_is_not_found_and_wrong_method_is_method_missing() {
        let mut t = Trie::default();
        t.insert("/todos", endpoint(&[Method::GET])).unwrap();
        assert!(matches!(t.find("/nope", &Method::GET), RouteMatch::NotFound));
        assert!(matches!(t.find("/todos", &Method::POST), RouteMatch::MethodMissing));
    }

    #[test]
    fn duplicate_path_registration_is_a_build_error() {
        let mut t = Trie::default();
        t.insert("/todos", endpoint(&[Method::GET])).unwrap();
        let err = t.insert("/todos", endpoint(&[Method::POST])).unwrap_err();
        assert!(err.message().contains("/todos"));
    }

    #[test]
    fn conflicting_param_names_are_a_build_error() {
        let mut t = Trie::default();
        t.insert("/todos/{id}", endpoint(&[Method::GET])).unwrap();
        let err = t.insert("/todos/{todo_id}", endpoint(&[Method::DELETE])).unwrap_err();
        assert!(err.message().contains("id"));
    }

    #[test]
    fn method_router_builder_collects_methods() {
        let mr = get(|| async { "a" }).post(|| async { "b" });
        let methods: Vec<_> = mr.handlers.iter().map(|(m, _)| m.clone()).collect();
        assert_eq!(methods, vec![Method::GET, Method::POST]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core router` (add `pub mod router;` and `pub use router::{delete, get, patch, post, put, MethodRouter};` to lib.rs. `middleware.rs` already exists — Task 9 ran first.)
Expected: compile FAILURE — `Trie`, `Endpoint`, `RouteMatch`, `get` not defined.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// Per-path method table: `get(list).post(create)` (spec §4.1).
pub struct MethodRouter {
    pub(crate) handlers: Vec<(Method, BoxHandlerFn)>,
}

pub fn get<H: Handler<A>, A>(h: H) -> MethodRouter { MethodRouter::new().on(Method::GET, h) }
pub fn post<H: Handler<A>, A>(h: H) -> MethodRouter { MethodRouter::new().on(Method::POST, h) }
pub fn put<H: Handler<A>, A>(h: H) -> MethodRouter { MethodRouter::new().on(Method::PUT, h) }
pub fn patch<H: Handler<A>, A>(h: H) -> MethodRouter { MethodRouter::new().on(Method::PATCH, h) }
pub fn delete<H: Handler<A>, A>(h: H) -> MethodRouter { MethodRouter::new().on(Method::DELETE, h) }

impl MethodRouter {
    fn new() -> Self { Self { handlers: Vec::new() } }

    pub fn on<H: Handler<A>, A>(mut self, method: Method, h: H) -> Self {
        self.handlers.push((method, h.into_handler_fn()));
        self
    }
    pub fn get<H: Handler<A>, A>(self, h: H) -> Self { self.on(Method::GET, h) }
    pub fn post<H: Handler<A>, A>(self, h: H) -> Self { self.on(Method::POST, h) }
    pub fn put<H: Handler<A>, A>(self, h: H) -> Self { self.on(Method::PUT, h) }
    pub fn patch<H: Handler<A>, A>(self, h: H) -> Self { self.on(Method::PATCH, h) }
    pub fn delete<H: Handler<A>, A>(self, h: H) -> Self { self.on(Method::DELETE, h) }
}

/// A flattened route: method table + the effective dependency environment and
/// middleware chain for this path (computed at build time, spec §4.2).
pub(crate) struct Endpoint {
    pub(crate) methods: HashMap<Method, BoxHandlerFn>,
    pub(crate) env: Arc<DepEnv>,
    pub(crate) middleware: Arc<[Arc<dyn Middleware>]>,
}

#[derive(Default)]
pub(crate) struct Trie {
    root: Node,
}

#[derive(Default)]
struct Node {
    statics: HashMap<String, Node>,
    param: Option<(String, Box<Node>)>,
    endpoint: Option<Endpoint>,
}

pub(crate) enum RouteMatch<'a> {
    Found { endpoint: &'a Endpoint, params: Vec<(String, String)> },
    MethodMissing,
    NotFound,
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

impl Trie {
    pub(crate) fn insert(&mut self, path: &str, endpoint: Endpoint) -> Result<()> {
        let mut node = &mut self.root;
        for seg in segments(path) {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                if node.param.is_none() {
                    node.param = Some((name.to_string(), Box::default()));
                }
                let (existing, child) = node.param.as_mut().expect("just ensured");
                if existing != name {
                    return Err(Error::internal(format!(
                        "conflicting path parameters `{{{existing}}}` vs `{{{name}}}` in `{path}`"
                    )));
                }
                node = child;
            } else {
                node = node.statics.entry(seg.to_string()).or_default();
            }
        }
        if node.endpoint.is_some() {
            return Err(Error::internal(format!("duplicate route registration for `{path}`")));
        }
        node.endpoint = Some(endpoint);
        Ok(())
    }

    pub(crate) fn find<'a>(&'a self, path: &str, method: &Method) -> RouteMatch<'a> {
        let mut node = &self.root;
        let mut params: Vec<(String, String)> = Vec::new();
        for seg in segments(path) {
            if let Some(next) = node.statics.get(seg) {
                node = next;
            } else if let Some((name, child)) = &node.param {
                params.push((name.clone(), seg.to_string()));
                node = child;
            } else {
                return RouteMatch::NotFound;
            }
        }
        match &node.endpoint {
            Some(ep) if ep.methods.contains_key(method) => RouteMatch::Found { endpoint: ep, params },
            Some(_) => RouteMatch::MethodMissing,
            None => RouteMatch::NotFound,
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core router`
Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/router.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add segment-trie router with typed params and build-time conflict detection"
```

---

### Task 9: Middleware — one trait, explicit chain (executes BEFORE Task 10)

**Files:**
- Create: `crates/jerrycan-core/src/middleware.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/jerrycan-core/src/middleware.rs`:

```rust
//! Middleware (spec §4.1): `async fn handle(&self, ctx, next)`. Composable,
//! ordering explicit, no tower, no magic.

use crate::extract::RequestCtx;
use crate::handler::BoxHandlerFn;
use crate::response::Response;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep::{DepEnv, DepResolver};
    use crate::response::IntoResponse;
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<&'static str>>>;

    struct Tag { name_in: &'static str, name_out: &'static str, log: Log }

    impl Middleware for Tag {
        fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
            Box::pin(async move {
                self.log.lock().unwrap().push(self.name_in);
                let res = next.run(&mut *ctx).await;
                self.log.lock().unwrap().push(self.name_out);
                res
            })
        }
    }

    #[tokio::test]
    async fn chain_runs_outside_in_then_inside_out() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let endpoint: BoxHandlerFn = Arc::new(move |_ctx: &mut RequestCtx| {
            let l = l.clone();
            Box::pin(async move {
                l.lock().unwrap().push("handler");
                "ok".into_response()
            })
        });

        let chain: Vec<Arc<dyn Middleware>> = vec![
            Arc::new(Tag { name_in: "outer-in", name_out: "outer-out", log: log.clone() }),
            Arc::new(Tag { name_in: "inner-in", name_out: "inner-out", log: log.clone() }),
        ];

        let req = http::Request::builder().uri("/").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(Arc::new(DepEnv::default()), Arc::new(HashMap::new())),
        );

        let res = Next { chain: &chain, endpoint: &endpoint }.run(&mut ctx).await;
        assert_eq!(res.status(), http::StatusCode::OK);
        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer-in", "inner-in", "handler", "inner-out", "outer-out"]
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core middleware` (add `pub mod middleware;` and `pub use middleware::{Middleware, MiddlewareFuture, Next};` to lib.rs).
Expected: compile FAILURE — `Middleware`, `Next` not defined.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// Boxed future returned by middleware. The lifetime ties to the request.
pub type MiddlewareFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;

/// Wraps request handling. Call `next.run(&mut *ctx).await` to continue;
/// return early to short-circuit (auth rejections, rate limits, …).
pub trait Middleware: Send + Sync + 'static {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a>;
}

/// The remainder of the middleware chain plus the endpoint handler.
pub struct Next<'a> {
    pub(crate) chain: &'a [Arc<dyn Middleware>],
    pub(crate) endpoint: &'a BoxHandlerFn,
}

impl<'a> Next<'a> {
    /// Run the rest of the chain. Takes a reborrow (`&mut *ctx`) so the caller
    /// keeps using `ctx` after awaiting.
    pub fn run<'b>(self, ctx: &'b mut RequestCtx) -> MiddlewareFuture<'b>
    where
        'a: 'b,
    {
        let Next { chain, endpoint } = self;
        match chain.split_first() {
            Some((head, rest)) => head.handle(ctx, Next { chain: rest, endpoint }),
            None => (**endpoint)(ctx), // &BoxHandlerFn → Arc<dyn Fn> → dyn Fn: explicit double deref
        }
    }
}
```

Pre-solved sharp edge: `Next::run` is generic over `'b` (with `'a: 'b`) precisely so middleware can keep using `ctx` after `next.run(&mut *ctx).await` — if you make `run` take `&'a mut`, the borrow checker rejects every middleware that touches the response afterward.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core middleware`
Expected: 1 test PASSES (the ordering assertion is the contract).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/middleware.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add Middleware trait with explicit Next chain"
```

---
### Task 11: `Module` — Blueprint reborn (routes, subroutes, scoped deps/middleware)

**Files:**
- Create: `crates/jerrycan-core/src/module.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan-core/src/module.rs`:

```rust
//! `Module` (spec §4.2): the unit of routing, packaging, and ownership.
//! Bundles routes, nested subroutes, module-scoped dependencies and middleware.
//! Flattening composes URL prefixes and layers environments (inner wins).

use crate::dep::{DepEnv, DepFactory};
use crate::middleware::Middleware;
use crate::router::MethodRouter;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::get;

    struct Cfg { tag: &'static str }

    fn leaf_paths(routes: &[FlatRoute]) -> Vec<String> {
        routes.iter().map(|r| r.path.clone()).collect()
    }

    #[test]
    fn nesting_composes_prefixes() {
        let comments = Module::new("comments").route("/", get(|| async { "list" }));
        let todos = Module::new("todos")
            .route("/", get(|| async { "list" }))
            .route("/{id}", get(|| async { "one" }))
            .mount("/{id}/comments", comments);

        let flat = todos.flatten("/todos", &DepEnv::default(), &[]);
        assert_eq!(
            leaf_paths(&flat),
            vec!["/todos", "/todos/{id}", "/todos/{id}/comments"]
        );
    }

    #[test]
    fn module_env_shadows_parent_env() {
        let parent = {
            let mut e = DepEnv::default();
            e.insert_value(Cfg { tag: "app" });
            e
        };
        let child = Module::new("sub").provide(Cfg { tag: "module" }).route("/", get(|| async { "x" }));
        let flat = child.flatten("/sub", &parent, &[]);
        let env = &flat[0].env;
        let got = env
            .singletons
            .get(&std::any::TypeId::of::<Cfg>())
            .and_then(|v| v.clone().downcast::<Cfg>().ok())
            .unwrap();
        assert_eq!(got.tag, "module");
    }

    #[test]
    fn middleware_chains_accumulate_parent_first() {
        struct Named(&'static str);
        impl Middleware for Named {
            fn handle<'a>(
                &'a self,
                ctx: &'a mut crate::RequestCtx,
                next: crate::middleware::Next<'a>,
            ) -> crate::middleware::MiddlewareFuture<'a> {
                next.run(ctx)
            }
        }
        let inner = Module::new("inner").middleware(Named("inner")).route("/", get(|| async { "x" }));
        let outer = Module::new("outer").middleware(Named("outer")).mount("/inner", inner);
        let flat = outer.flatten("/outer", &DepEnv::default(), &[]);
        assert_eq!(flat[0].middleware.len(), 2, "outer then inner");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core module` (add `pub mod module;` and `pub use module::Module;` to lib.rs).
Expected: compile FAILURE — `Module`, `FlatRoute` not defined.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// Flask's Blueprint, Rust-grade. Built by route crates' `pub fn module()`.
pub struct Module {
    pub(crate) name: String,
    pub(crate) routes: Vec<(String, MethodRouter)>,
    pub(crate) mounts: Vec<(String, Module)>,
    pub(crate) env: DepEnv,
    pub(crate) middleware: Vec<Arc<dyn Middleware>>,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            routes: Vec::new(),
            mounts: Vec::new(),
            env: DepEnv::default(),
            middleware: Vec::new(),
        }
    }

    /// Register a path relative to the module's mount point.
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_string(), methods));
        self
    }

    /// Mount a child module (subroute) under a relative prefix. Nests arbitrarily.
    pub fn mount(mut self, prefix: &str, child: Module) -> Self {
        self.mounts.push((prefix.to_string(), child));
        self
    }

    /// Module-scoped singleton value; shadows any parent provider of the same type.
    pub fn provide<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.env.insert_value(value);
        self
    }

    /// Module-scoped async factory (request scope); shadows parents likewise.
    pub fn provide_dep<F, Args, T>(mut self, factory: F) -> Self
    where
        F: DepFactory<Args, T>,
        T: Send + Sync + 'static,
    {
        self.env.insert_factory(factory);
        self
    }

    /// Module-scoped middleware; runs after the app's and parents' middleware.
    pub fn middleware<M: Middleware>(mut self, mw: M) -> Self {
        self.middleware.push(Arc::new(mw));
        self
    }

    pub fn name(&self) -> &str { &self.name }
}

/// One route after flattening: absolute path + effective env + middleware chain.
pub(crate) struct FlatRoute {
    pub(crate) path: String,
    pub(crate) methods: MethodRouter,
    pub(crate) env: Arc<DepEnv>,
    pub(crate) middleware: Arc<[Arc<dyn Middleware>]>,
}

pub(crate) fn join_paths(prefix: &str, rel: &str) -> String {
    let a = prefix.trim_end_matches('/');
    let b = rel.trim_start_matches('/');
    match (a.is_empty(), b.is_empty()) {
        (true, true) => "/".to_string(),
        (false, true) => a.to_string(),
        (true, false) => format!("/{b}"),
        (false, false) => format!("{a}/{b}"),
    }
}

impl Module {
    /// Resolution order baked at build time: app env ← parent modules ← this
    /// module (inner wins); middleware: app's, then parents', then this module's.
    pub(crate) fn flatten(
        self,
        prefix: &str,
        parent_env: &DepEnv,
        parent_mw: &[Arc<dyn Middleware>],
    ) -> Vec<FlatRoute> {
        let mut merged = parent_env.clone();
        merged.merge_from(&self.env);

        let mut mw: Vec<Arc<dyn Middleware>> = parent_mw.to_vec();
        mw.extend(self.middleware);

        let env = Arc::new(merged.clone());
        let mw_arc: Arc<[Arc<dyn Middleware>]> = Arc::from(mw.clone());

        let mut out = Vec::new();
        for (path, methods) in self.routes {
            out.push(FlatRoute {
                path: join_paths(prefix, &path),
                methods,
                env: env.clone(),
                middleware: mw_arc.clone(),
            });
        }
        for (sub_prefix, child) in self.mounts {
            let child_prefix = join_paths(prefix, &sub_prefix);
            out.extend(child.flatten(&child_prefix, &merged, &mw));
        }
        out
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core module`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/module.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add Module with nested subroutes and scoped dependency/middleware layering"
```

---

### Task 12: `App` — assembly, build-time validation, dispatch

**Files:**
- Create: `crates/jerrycan-core/src/app.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan-core/src/app.rs`:

```rust
//! `App` (spec §4.1): assembles mounted modules + app-level routes, validates
//! the route table at build time (fail loud), and dispatches requests.

use crate::dep::{AnyArc, DepEnv, DepFactory, DepResolver};
use crate::error::{Error, Result};
use crate::extract::RequestCtx;
use crate::handler::BoxHandlerFn;
use crate::middleware::{Middleware, Next};
use crate::module::{FlatRoute, Module};
use crate::response::{IntoResponse, Response};
use crate::router::{Endpoint, MethodRouter, RouteMatch, Trie};
use bytes::Bytes;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Json;
    use crate::router::{get, post};
    use crate::{Dep, Path};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Store { items: Mutex<Vec<String>> }

    async fn list(store: Dep<Store>) -> Json<Vec<String>> {
        Json(store.items.lock().unwrap().clone())
    }

    async fn create(store: Dep<Store>, Json(item): Json<String>) -> crate::Result<Json<usize>> {
        let mut items = store.items.lock().unwrap();
        items.push(item);
        Ok(Json(items.len()))
    }

    async fn show(store: Dep<Store>, Path(ix): Path<usize>) -> crate::Result<Json<String>> {
        store.items.lock().unwrap().get(ix).cloned().map(Json).ok_or_else(Error::not_found)
    }

    fn crud_app() -> App {
        App::new()
            .provide(Store::default())
            .mount(
                "/todos",
                Module::new("todos")
                    .route("/", get(list).post(create))
                    .route("/{ix}", get(show)),
            )
    }

    async fn dispatch(built: &BuiltApp, method: http::Method, path: &str, body: &str) -> Response {
        let req = http::Request::builder().method(method).uri(path).body(()).unwrap();
        let (parts, ()) = req.into_parts();
        built.dispatch(parts, Bytes::from(body.to_string())).await
    }

    #[tokio::test]
    async fn crud_round_trip_in_process() {
        let built = crud_app().build().unwrap();
        let r = dispatch(&built, http::Method::POST, "/todos/", r#""write spike""#).await;
        assert_eq!(r.status(), http::StatusCode::OK);
        let r = dispatch(&built, http::Method::GET, "/todos/0", "").await;
        assert_eq!(r.status(), http::StatusCode::OK);
        let r = dispatch(&built, http::Method::GET, "/todos/9", "").await;
        assert_eq!(r.status(), http::StatusCode::NOT_FOUND);
        let r = dispatch(&built, http::Method::PATCH, "/todos/", "").await;
        assert_eq!(r.status(), http::StatusCode::METHOD_NOT_ALLOWED);
        let r = dispatch(&built, http::Method::GET, "/nope", "").await;
        assert_eq!(r.status(), http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn conflicting_routes_fail_at_build_not_at_request_time() {
        let app = App::new()
            .route("/x", get(|| async { "a" }))
            .route("/x", get(|| async { "b" }));
        let err = app.build().unwrap_err();
        assert!(err.message().contains("/x"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core app` (add `pub mod app;` and `pub use app::{App, BuiltApp};` to lib.rs).
Expected: compile FAILURE — `App`, `BuiltApp` not defined.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// The application builder. Generated `app/src/main.rs` is exactly this:
/// provide app-level deps, mount modules, serve.
#[derive(Default)]
pub struct App {
    routes: Vec<(String, MethodRouter)>,
    mounts: Vec<(String, Module)>,
    env: DepEnv,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl App {
    pub fn new() -> Self { Self::default() }

    /// App-level route (prefer modules; this exists for tiny services and tests).
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_string(), methods));
        self
    }

    /// Mount a module at a prefix (spec §4.2).
    pub fn mount(mut self, prefix: &str, module: Module) -> Self {
        self.mounts.push((prefix.to_string(), module));
        self
    }

    /// App-level singleton value dependency.
    pub fn provide<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.env.insert_value(value);
        self
    }

    /// App-level async factory dependency (request scope).
    pub fn provide_dep<F, Args, T>(mut self, factory: F) -> Self
    where
        F: DepFactory<Args, T>,
        T: Send + Sync + 'static,
    {
        self.env.insert_factory(factory);
        self
    }

    /// App-level middleware — outermost ring of every route's chain.
    pub fn middleware<M: Middleware>(mut self, mw: M) -> Self {
        self.middleware.push(Arc::new(mw));
        self
    }

    /// Flatten modules, validate the route table, freeze the dispatch trie.
    /// All conflicts surface HERE — before serving (spec §4.1 "fail loud").
    pub fn build(self) -> Result<BuiltApp> {
        let mut trie = Trie::default();
        let app_env = Arc::new(self.env.clone());
        let app_mw: Arc<[Arc<dyn Middleware>]> = Arc::from(self.middleware.clone());

        for (path, methods) in self.routes {
            insert_flat(&mut trie, FlatRoute {
                path,
                methods,
                env: app_env.clone(),
                middleware: app_mw.clone(),
            })?;
        }
        for (prefix, module) in self.mounts {
            for flat in module.flatten(&prefix, &self.env, &self.middleware) {
                insert_flat(&mut trie, flat)?;
            }
        }
        Ok(BuiltApp { trie, overrides: Arc::new(HashMap::new()) })
    }
}

fn insert_flat(trie: &mut Trie, flat: FlatRoute) -> Result<()> {
    let mut methods = HashMap::new();
    for (m, h) in flat.methods.handlers {
        if methods.insert(m.clone(), h).is_some() {
            return Err(Error::internal(format!("duplicate method {m} for `{}`", flat.path)));
        }
    }
    trie.insert(&flat.path, Endpoint { methods, env: flat.env, middleware: flat.middleware })
}

/// The frozen, immutable runtime form. Cheap to share across connections.
pub struct BuiltApp {
    pub(crate) trie: Trie,
    pub(crate) overrides: Arc<HashMap<TypeId, AnyArc>>,
}

impl BuiltApp {
    /// Route + run middleware chain + handler for one request.
    pub(crate) async fn dispatch(&self, parts: http::request::Parts, body: Bytes) -> Response {
        let method = parts.method.clone();
        let path = parts.uri.path().to_string();
        match self.trie.find(&path, &method) {
            RouteMatch::NotFound => Error::not_found().into_response(),
            RouteMatch::MethodMissing => Error::method_not_allowed().into_response(),
            RouteMatch::Found { endpoint, params } => {
                let mut ctx = RequestCtx::new(
                    parts,
                    body,
                    DepResolver::new(endpoint.env.clone(), self.overrides.clone()),
                );
                ctx.params = params;
                let handler: &BoxHandlerFn =
                    endpoint.methods.get(&method).expect("find() checked the method");
                Next { chain: &endpoint.middleware, endpoint: handler }.run(&mut ctx).await
            }
        }
    }
}
```

(`Trie::find` borrows `endpoint` from `self.trie` while `ctx` borrows nothing from it — the handler runs against cloned `Arc`s, so there's no borrow tangle. If the compiler complains about `parts.uri.path()` being used after `parts` moves into `RequestCtx::new`, note the `path` is cloned to a `String` first — that's why.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core app`
Expected: 2 tests PASS — the CRUD round trip and build-time conflict detection.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/app.rs crates/jerrycan-core/src/lib.rs
git commit -m "Add App assembly with build-time route validation and request dispatch"
```

---

### Task 13: `TestApp` — the public testing story (overrides included)

**Files:**
- Create: `crates/jerrycan-core/src/test_client.rs`
- Create: `crates/jerrycan-core/tests/di.rs`
- Create: `crates/jerrycan-core/tests/module.rs`
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the failing integration tests (public API only — this is the spike's proof)**

Create `crates/jerrycan-core/tests/di.rs`:

```rust
//! Spec §4.3 acceptance: nested async deps + per-request caching + overrides,
//! exercised through the public TestApp API exactly as generated tests will.

use jerrycan_core::{get, App, Dep, Json, Module};

struct Db { url: String }
struct CurrentUser { name: String }

async fn current_user(db: Dep<Db>) -> jerrycan_core::Result<CurrentUser> {
    Ok(CurrentUser { name: format!("user@{}", db.url) })
}

async fn whoami(user: Dep<CurrentUser>) -> Json<String> {
    Json(user.name.clone())
}

fn app() -> App {
    App::new()
        .provide(Db { url: "pg://prod".into() })
        .provide_dep(current_user)
        .mount("/me", Module::new("me").route("/", get(whoami)))
}

#[tokio::test]
async fn nested_deps_resolve_through_real_requests() {
    let t = app().into_test();
    let res = t.get("/me/").await;
    assert_eq!(res.status(), http::StatusCode::OK);
    assert_eq!(res.json::<String>(), "user@pg://prod");
}

#[tokio::test]
async fn override_dep_swaps_the_database_for_tests() {
    let t = app().into_test().override_dep(Db { url: "sqlite::memory:".into() });
    let res = t.get("/me/").await;
    assert_eq!(res.json::<String>(), "user@sqlite::memory:");
}

#[tokio::test]
async fn override_can_replace_a_factory_product_directly() {
    let t = app().into_test().override_dep(CurrentUser { name: "fake".into() });
    let res = t.get("/me/").await;
    assert_eq!(res.json::<String>(), "fake");
}
```

Create `crates/jerrycan-core/tests/module.rs`:

```rust
//! Spec §4.2 acceptance: nested subroutes, module-scoped deps shadowing,
//! module-scoped middleware short-circuiting.

use jerrycan_core::{
    get, App, Dep, IntoResponse, Json, Middleware, MiddlewareFuture, Module, Next, RequestCtx,
};

struct Flavor(&'static str);

async fn flavor(f: Dep<Flavor>) -> Json<String> {
    Json(f.0.to_string())
}

#[tokio::test]
async fn subroutes_nest_and_module_deps_shadow_app_deps() {
    let comments = Module::new("comments")
        .provide(Flavor("comment-scope"))
        .route("/", get(flavor));
    let todos = Module::new("todos")
        .route("/flavor", get(flavor))
        .mount("/{id}/comments", comments);
    let t = App::new()
        .provide(Flavor("app-scope"))
        .mount("/todos", todos)
        .into_test();

    assert_eq!(t.get("/todos/flavor").await.json::<String>(), "app-scope");
    assert_eq!(t.get("/todos/7/comments/").await.json::<String>(), "comment-scope");
}

struct Deny;
impl Middleware for Deny {
    fn handle<'a>(&'a self, _ctx: &'a mut RequestCtx, _next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async { jerrycan_core::Error::new(http::StatusCode::FORBIDDEN, "JC0403", "denied").into_response() })
    }
}

#[tokio::test]
async fn module_middleware_short_circuits_only_its_subtree() {
    let locked = Module::new("locked").middleware(Deny).route("/", get(|| async { "secret" }));
    let t = App::new()
        .route("/open", get(|| async { "open" }))
        .mount("/locked", locked)
        .into_test();

    assert_eq!(t.get("/open").await.status(), http::StatusCode::OK);
    assert_eq!(t.get("/locked/").await.status(), http::StatusCode::FORBIDDEN);
}
```

Note: `use http::…` resolves in these integration tests because `http` is a direct dependency of the package (all test targets see it); additionally add `pub use http;` to lib.rs so generated apps never declare `http` themselves. `IntoResponse` is imported at the top because `Deny::handle`'s body calls `.into_response()` — a trait method needs the trait in scope at the call site's module level.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core --test di`
Expected: compile FAILURE — `into_test`, `TestApp` missing.

- [ ] **Step 3: Implement the test client**

Create `crates/jerrycan-core/src/test_client.rs`:

```rust
//! In-memory test client (spec §4.1 "Test client"): no sockets, no network.
//! `override_dep` is THE testing seam — fake any dependency, run real requests.

use crate::app::{App, BuiltApp};
use crate::response::Response;
use bytes::Bytes;
use http::{header, Method, StatusCode};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::any::TypeId;
use std::sync::Arc;

impl App {
    /// Build for testing. Panics on build errors — a test should fail loudly.
    pub fn into_test(self) -> TestApp {
        TestApp { built: self.build().expect("app failed to build") }
    }
}

pub struct TestApp {
    built: BuiltApp,
}

impl TestApp {
    /// Replace the provider for `T` everywhere (values AND factories) for all
    /// subsequent requests. Chainable.
    pub fn override_dep<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        let mut map = (*self.built.overrides).clone();
        map.insert(TypeId::of::<T>(), Arc::new(value) as crate::dep::AnyArc);
        self.built.overrides = Arc::new(map);
        self
    }

    pub async fn get(&self, path: &str) -> TestResponse {
        self.request(Method::GET, path, None).await
    }
    pub async fn delete(&self, path: &str) -> TestResponse {
        self.request(Method::DELETE, path, None).await
    }
    pub async fn post_json<B: Serialize>(&self, path: &str, body: &B) -> TestResponse {
        self.request(Method::POST, path, Some(serde_json::to_vec(body).expect("serialize"))).await
    }
    pub async fn put_json<B: Serialize>(&self, path: &str, body: &B) -> TestResponse {
        self.request(Method::PUT, path, Some(serde_json::to_vec(body).expect("serialize"))).await
    }
    pub async fn patch_json<B: Serialize>(&self, path: &str, body: &B) -> TestResponse {
        self.request(Method::PATCH, path, Some(serde_json::to_vec(body).expect("serialize"))).await
    }

    async fn request(&self, method: Method, path: &str, json: Option<Vec<u8>>) -> TestResponse {
        let mut builder = http::Request::builder().method(method).uri(path);
        if json.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let req = builder.body(()).expect("test request build");
        let (parts, ()) = req.into_parts();
        let body = Bytes::from(json.unwrap_or_default());
        TestResponse::collect(self.built.dispatch(parts, body).await).await
    }
}

pub struct TestResponse {
    status: StatusCode,
    headers: http::HeaderMap,
    body: Bytes,
}

impl TestResponse {
    async fn collect(res: Response) -> Self {
        let (parts, body) = res.into_parts();
        let body = body.collect().await.expect("collect response body").to_bytes();
        Self { status: parts.status, headers: parts.headers, body }
    }

    pub fn status(&self) -> StatusCode { self.status }
    pub fn headers(&self) -> &http::HeaderMap { &self.headers }
    pub fn text(&self) -> String { String::from_utf8_lossy(&self.body).into_owned() }

    /// Deserialize the JSON body, with a readable panic on mismatch.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("response body is not the expected JSON shape: {e}\nbody: {}", self.text())
        })
    }
}
```

In `crates/jerrycan-core/src/lib.rs`: add `pub mod test_client;`, `pub use test_client::{TestApp, TestResponse};`, and `pub use http;`. Also make `dep::AnyArc` visible inside the crate (it already is `pub(crate)` — the `crate::dep::AnyArc` path in `test_client.rs` works as-is).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core`
Expected: ALL tests green — unit tests + `tests/di.rs` (3) + `tests/module.rs` (2). **This moment is the Phase 0 spike exit: DI and Module signatures are proven as real, running Rust.**

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/test_client.rs crates/jerrycan-core/tests crates/jerrycan-core/src/lib.rs
git commit -m "Add TestApp with dependency overrides; DI and Module acceptance tests pass"
```

---
### Task 14: `serve()` on hyper — secure defaults at the edge

**Files:**
- Modify: `crates/jerrycan-core/src/app.rs`
- Create: `crates/jerrycan-core/tests/e2e.rs`

- [ ] **Step 1: Write the failing smoke test**

Create `crates/jerrycan-core/tests/e2e.rs`:

```rust
//! Real-socket smoke test: hyper serves a built app over actual TCP.

use jerrycan_core::{get, App};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn serves_over_real_tcp() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = App::new().route("/ping", get(|| async { "pong" }));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);

    assert!(text.starts_with("HTTP/1.1 200 OK"), "got: {text}");
    assert!(text.ends_with("pong"), "got: {text}");

    server.abort();
}

#[tokio::test]
async fn oversized_bodies_are_rejected_with_413() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = App::new().route("/echo", jerrycan_core::post(|b: jerrycan_core::Json<String>| async move { b }));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let huge = "x".repeat(2 * 1024 * 1024); // 2 MiB > 1 MiB default limit
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /echo HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        huge.len() + 2
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(format!("\"{huge}\"").as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await; // server may reset after responding
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("413"), "got: {text}");
    server.abort();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan-core --test e2e`
Expected: compile FAILURE — `serve_with` not defined.

- [ ] **Step 3: Implement `serve` / `serve_with` (append to the `impl App` block in `app.rs`)**

```rust
    /// Bind from config and serve forever. Address: `JERRYCAN_ADDR` env var,
    /// default `127.0.0.1:8000`. (Full layered config lands in Phase 1; the
    /// env-var layer is the contract that already works.)
    pub async fn serve(self) -> Result<()> {
        let addr = std::env::var("JERRYCAN_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("failed to bind {addr}: {e}")))?;
        self.serve_with(listener).await
    }

    /// Serve on an existing listener (tests, socket activation, port 0).
    pub async fn serve_with(self, listener: tokio::net::TcpListener) -> Result<()> {
        const BODY_LIMIT: usize = 1024 * 1024; // 1 MiB — spec §4.4 secure default

        let built = Arc::new(self.build()?);
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| Error::internal(format!("accept failed: {e}")))?;
            let app = built.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let app = app.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        use http_body_util::BodyExt;
                        let limited = http_body_util::Limited::new(body, BODY_LIMIT);
                        let response = match limited.collect().await {
                            Ok(collected) => app.dispatch(parts, collected.to_bytes()).await,
                            Err(_) => Error::payload_too_large().into_response(),
                        };
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                // Connection errors (resets, parse failures) are per-connection
                // noise, not app failures; hyper already responded 4xx where it could.
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    }
```

(Request read/handler timeouts are Phase 1 alongside `jerrycan-observe` — the body limit proves where secure defaults live.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan-core --test e2e`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/app.rs crates/jerrycan-core/tests/e2e.rs
git commit -m "Add hyper serving with 1MiB body limit secure default"
```

---

### Task 15: Prelude, lint gate, and a tidy `lib.rs`

**Files:**
- Modify: `crates/jerrycan-core/src/lib.rs`

- [ ] **Step 1: Write the final `lib.rs`**

Replace `crates/jerrycan-core/src/lib.rs` with:

```rust
//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. Generated apps import this through the `jerrycan`
//! facade crate — see https://jerrycan.cc
#![forbid(unsafe_code)]

pub mod app;
pub mod dep;
pub mod error;
pub mod extract;
pub mod handler;
pub mod middleware;
pub mod module;
pub mod response;
pub mod router;
pub mod test_client;

pub use app::{App, BuiltApp};
pub use dep::Dep;
pub use error::{Error, Result};
pub use extract::{FromRequest, Path, Query, RequestCtx};
pub use handler::Handler;
pub use middleware::{Middleware, MiddlewareFuture, Next};
pub use module::Module;
pub use response::{Created, IntoResponse, Json, NoContent, Response};
pub use router::{delete, get, patch, post, put, MethodRouter};
pub use test_client::{TestApp, TestResponse};

/// Re-exported so apps and tests never add `http` to their own Cargo.toml.
pub use http;

/// One import for generated code: `use jerrycan::prelude::*;`
pub mod prelude {
    pub use crate::{
        delete, get, patch, post, put, App, Created, Dep, Error, IntoResponse, Json, Middleware,
        MiddlewareFuture, Module, NoContent, Next, Path, Query, RequestCtx, Result, TestApp,
    };
}
```

- [ ] **Step 2: Run the full local gate**

Run: `cargo fmt --all && cargo clippy -p jerrycan-core --all-targets -- -D warnings && cargo test -p jerrycan-core`
Expected: fmt clean, clippy ZERO warnings, all tests pass. Fix any clippy findings now (typical ones: `len() == 0` → `is_empty()`, redundant clones in tests). Do not `#[allow]` anything except the documented `non_snake_case` in the two arity macros.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "Add prelude and pass fmt/clippy gates on jerrycan-core"
```

---

### Task 16: The `jerrycan` facade crate + `#[jerrycan::main]` + doc-test harness

**Files:**
- Modify: `crates/jerrycan-macros/Cargo.toml`
- Modify: `crates/jerrycan-macros/src/lib.rs`
- Modify: `crates/jerrycan/Cargo.toml`
- Modify: `crates/jerrycan/src/lib.rs`

Context: generated apps write `use jerrycan::prelude::*;` and `#[jerrycan::main]` (spec §4.1). So the `jerrycan` package is a **facade library** re-exporting core + macros; the CLI binary is added to the same package in Phase 1 behind a feature flag. The docs (Task 17–19) mount into THIS crate so every example compiles against the exact paths generated apps use.

- [ ] **Step 1: Make `jerrycan-macros` a real proc-macro crate**

Replace `crates/jerrycan-macros/Cargo.toml` with:

```toml
[package]
name = "jerrycan-macros"
description = "Proc-macro sugar for the jerrycan framework. Name reservation; real releases begin at 0.1.0. https://jerrycan.cc"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true

[lib]
proc-macro = true

[dependencies]
```

Replace `crates/jerrycan-macros/src/lib.rs` with:

```rust
//! Proc-macro sugar for the jerrycan framework.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;

/// `#[jerrycan::main]` — boots the async runtime around `async fn main`.
/// Today it delegates to `#[tokio::main]`; the app must (and generated apps
/// do) depend on tokio directly.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let wrapped = format!("#[::tokio::main]\n{item}");
    wrapped.parse().expect("jerrycan::main: item must be a valid async fn")
}
```

- [ ] **Step 2: Turn `crates/jerrycan` into the facade**

Replace `crates/jerrycan/Cargo.toml` with:

```toml
[package]
name = "jerrycan"
description = "The AI-native Rust backend platform: framework, CLI, and MCP server. Name reservation; development at https://jerrycan.cc — real releases begin at 0.1.0."
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true

[dependencies]
jerrycan-core = { path = "../jerrycan-core", version = "0.0.0" }
jerrycan-macros = { path = "../jerrycan-macros", version = "0.0.0" }

[dev-dependencies]
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
```

Replace `crates/jerrycan/src/lib.rs` with:

```rust
//! The AI-native Rust backend platform. Generated apps depend on this one
//! crate (plus tokio) and write `use jerrycan::prelude::*;`.
//! The CLI/MCP binary joins this package in Phase 1 behind a `cli` feature.
#![forbid(unsafe_code)]

pub use jerrycan_core::*;
pub use jerrycan_macros::main;

pub mod prelude {
    pub use jerrycan_core::prelude::*;
    pub use jerrycan_macros::main;
}
```

- [ ] **Step 3: Verify the facade compiles and the macro expands**

Create `crates/jerrycan/tests/facade.rs`:

```rust
//! The facade must expose exactly the paths generated code uses.

use jerrycan::prelude::*;

#[jerrycan::main]
async fn demo_main() -> Result<()> {
    // Never actually served; this test proves the attribute + paths compile.
    let _app = App::new().route("/ping", get(|| async { "pong" }));
    Ok(())
}

#[test]
fn facade_paths_compile() {
    // demo_main is intentionally unused at runtime; its existence is the test.
    let _ = demo_main as fn() -> Result<()>;
}
```

Run: `cargo test -p jerrycan`
Expected: PASS. (`#[tokio::main]` rewrites `async fn` into `fn`, hence the cast type.)

- [ ] **Step 4: Commit**

```bash
git add crates/jerrycan crates/jerrycan-macros Cargo.lock
git commit -m "Turn jerrycan into facade crate with jerrycan::main macro"
```

---
### Task 17: Doc-test harness + docs pages 01 (App) and 02 (Modules)

**Files:**
- Modify: `crates/jerrycan/src/lib.rs` (doc mounts)
- Create: `docs/ai/01-app.md`
- Create: `docs/ai/02-modules.md`

Context: every fenced ```rust block in `docs/ai/*.md` becomes a doc-test of the `jerrycan` facade — **docs that don't compile don't merge** (spec §8). Lines starting with `# ` inside examples are hidden scaffolding (runtime boot); readers see clean code, CI runs the whole thing. `rust,no_run` compiles but skips execution — used only for examples that bind sockets.

- [ ] **Step 1: Mount the docs as doc-tests**

Append to `crates/jerrycan/src/lib.rs`:

```rust
/// Compile-checks every example in docs/ai/*.md (spec §8: executable docs).
#[cfg(doctest)]
mod doc_tests {
    macro_rules! doc_page {
        ($name:ident, $path:literal) => {
            #[doc = include_str!($path)]
            mod $name {}
        };
    }
    doc_page!(page_01_app, "../../../docs/ai/01-app.md");
    doc_page!(page_02_modules, "../../../docs/ai/02-modules.md");
}
```

(Each later docs task appends its `doc_page!` lines here.)

- [ ] **Step 2: Write `docs/ai/01-app.md`**

````markdown
# App

## Purpose
`App` assembles a backend: register app-level dependencies, mount modules, serve.
In generated projects this file is machine-written (`crates/app/src/main.rs`) — you
rarely edit it; you generate modules instead (see 02-modules).

## Signature
```rust,no_run
# use jerrycan::prelude::*;
# async fn noop() -> Result<()> {
App::new()
    .provide(())                 // .provide(value) — app-wide singleton dependency
    .route("/ping", get(|| async { "pong" }))   // app-level route (prefer modules)
    .serve()                     // binds JERRYCAN_ADDR (default 127.0.0.1:8000)
    .await
# }
```

## Minimal example
```rust,no_run
use jerrycan::prelude::*;

#[jerrycan::main]
async fn main() -> Result<()> {
    App::new()
        .route("/ping", get(|| async { "pong" }))
        .serve()
        .await
}
```

## Variations
Mount modules (the normal shape of a generated app):
```rust
# use jerrycan::prelude::*;
let app = App::new()
    .mount("/todos", Module::new("todos").route("/", get(|| async { "list" })));
# let _ = app.into_test();
```

App-level middleware wraps every route (outermost ring):
```rust
# use jerrycan::prelude::*;
# struct Noop;
# impl Middleware for Noop {
#     fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
#         next.run(ctx)
#     }
# }
let app = App::new().middleware(Noop).route("/x", get(|| async { "x" }));
# let _ = app.into_test();
```

## Errors you'll hit
- Duplicate or ambiguous routes fail at **build/serve time**, not request time —
  `serve()` returns `Err` describing the conflicting path. Fix the route table;
  never work around it with ordering.
- `JC0404`/`JC0405` are produced automatically for unknown paths / known path,
  wrong method. You don't write those handlers.

## Anti-patterns
- Don't hand-edit a generated `main.rs` to add routes — generate a module
  (`jerrycan generate route <name>`); mounting is regenerated deterministically.
- Don't register many app-level routes; modules are the unit of structure,
  testing, and ownership.
````

- [ ] **Step 3: Write `docs/ai/02-modules.md`**

````markdown
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
````

- [ ] **Step 4: Run the doc-tests**

Run: `cargo test --doc -p jerrycan`
Expected: every ```rust block above runs (or compiles for `no_run`) and PASSES. Failures here mean the docs and the implementation disagree — fix whichever is wrong; never delete the example.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/lib.rs docs/ai/01-app.md docs/ai/02-modules.md
git commit -m "Add doc-test harness and App/Module docs pages"
```

---

### Task 18: Docs pages 03 (Extractors) and 04 (Dependencies)

**Files:**
- Create: `docs/ai/03-extractors.md`
- Create: `docs/ai/04-dependencies.md`
- Modify: `crates/jerrycan/src/lib.rs` (two `doc_page!` lines)

- [ ] **Step 1: Add the mounts**

In the `doc_tests` module of `crates/jerrycan/src/lib.rs`, append:

```rust
    doc_page!(page_03_extractors, "../../../docs/ai/03-extractors.md");
    doc_page!(page_04_dependencies, "../../../docs/ai/04-dependencies.md");
```

- [ ] **Step 2: Write `docs/ai/03-extractors.md`**

````markdown
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
- Don't take `RequestCtx` in handlers to "grab things manually" — if a value
  isn't expressible as an extractor, define a dependency for it.
- One `Path<T>` per route in v0 (one `{param}`-typed extractor); multi-param
  tuples arrive in Phase 1 — until then design routes with one variable segment
  per handler or read both via two nested modules.
````

- [ ] **Step 3: Write `docs/ai/04-dependencies.md`**

````markdown
# Dependencies

## Purpose
`Dep<T>` is jerrycan's dependency injection — the signature feature. Auth,
database handles, permissions, rate limits: all reusable values resolved per
request, async, nested, memoized, and replaceable in tests.

## Signature
```rust
# use jerrycan::prelude::*;
# struct Db; struct Session; struct User { name: String }
// Register on the app (or module):
//   .provide(value)        — singleton, shared by all requests
//   .provide_dep(factory)  — async fn run at most once per request
async fn current_user(session: Dep<Session>, db: Dep<Db>) -> Result<User> {
    # let _ = (session, db);
    Ok(User { name: "ada".into() })  // factories can await I/O
}

// Consume anywhere — handlers or other factories:
async fn whoami(user: Dep<User>) -> String { user.name.clone() }
# let _ = (current_user, whoami);
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Greeting(&'static str);
async fn greet(g: Dep<Greeting>) -> String { g.0.to_string() }

let t = App::new()
    .provide(Greeting("hello"))
    .route("/", get(greet))
    .into_test();
assert_eq!(t.get("/").await.text(), "hello");
# }); }
```

## Variations
Nested factories — guards are just dependencies:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
struct Db { url: &'static str }
struct User { name: String }
struct Admin;

async fn current_user(db: Dep<Db>) -> Result<User> {
    Ok(User { name: format!("ada@{}", db.url) })
}
async fn require_admin(user: Dep<User>) -> Result<Admin> {
    if user.name.starts_with("ada") { Ok(Admin) } else { Err(Error::new(jerrycan::http::StatusCode::FORBIDDEN, "JC0403", "admins only")) }
}
async fn dashboard(_: Dep<Admin>, user: Dep<User>) -> String { user.name.clone() }

let t = App::new()
    .provide(Db { url: "pg://prod" })
    .provide_dep(current_user)
    .provide_dep(require_admin)
    .route("/admin", get(dashboard))
    .into_test();
assert_eq!(t.get("/admin").await.text(), "ada@pg://prod");
# }); }
```

Per-request memoization: a factory runs at most once per request, no matter how
many handlers/factories ask for its type; a fresh request resolves afresh.

## Errors you'll hit
- Consuming an unregistered type → `500 JC1001` naming the missing type. Fix:
  `.provide`/`.provide_dep` it on the app or the module.
- A factory chain deeper than 32 (or cyclic) → `500 JC1002`. Break the cycle.

## Anti-patterns
- Don't pass `Dep<T>` values into helper functions by cloning everywhere —
  factories compose; make the helper a dependency.
- Don't use a singleton `.provide(value)` for per-request state (sessions,
  transactions) — that's what `.provide_dep` request scope is for.

## Testing
See 07-testing: `TestApp::override_dep` replaces ANY dependency — value or
factory product — without touching app code.
````

- [ ] **Step 4: Run the doc-tests**

Run: `cargo test --doc -p jerrycan`
Expected: all pages' examples PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/ai/03-extractors.md docs/ai/04-dependencies.md crates/jerrycan/src/lib.rs
git commit -m "Add extractor and dependency-injection docs pages"
```

---
### Task 19: Docs pages 05 (Errors), 06 (Middleware), 07 (Testing)

**Files:**
- Create: `docs/ai/05-errors.md`
- Create: `docs/ai/06-middleware.md`
- Create: `docs/ai/07-testing.md`
- Modify: `crates/jerrycan/src/lib.rs` (three `doc_page!` lines)

- [ ] **Step 1: Add the mounts**

```rust
    doc_page!(page_05_errors, "../../../docs/ai/05-errors.md");
    doc_page!(page_06_middleware, "../../../docs/ai/06-middleware.md");
    doc_page!(page_07_testing, "../../../docs/ai/07-testing.md");
```

- [ ] **Step 2: Write `docs/ai/05-errors.md`**

````markdown
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

## Errors you'll hit (the built-in code table)
| Code | Status | Produced when |
|---|---|---|
| JC0400 | 400 | Bad path param / query string |
| JC0404 | 404 | No route matched, or `Error::not_found()` |
| JC0405 | 405 | Path exists, method doesn't |
| JC0413 | 413 | Body over the limit (default 1 MiB) |
| JC0422 | 422 | JSON body failed to parse/validate |
| JC0500 | 500 | `Error::internal` / response serialization failure |
| JC1001 | 500 | Dependency type has no provider |
| JC1002 | 500 | Dependency cycle / chain > 32 |

## Anti-patterns
- Don't `panic!`/`unwrap()` in handlers for expected failures — return `Err`.
- Don't put internal detail (queries, file paths) in `message` — it goes to the
  client. Log internals; respond with intent.
````

- [ ] **Step 3: Write `docs/ai/06-middleware.md`**

````markdown
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
````

- [ ] **Step 4: Write `docs/ai/07-testing.md`**

````markdown
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

## Errors you'll hit
- `panic: app failed to build` — your route table has a conflict; the message
  names the path. This is the same failure `serve()` would return.
- `response body is not the expected JSON shape` — `res.json::<T>()` panics
  with the body text included; read it, fix the handler or the test type.

## Anti-patterns
- Don't boot real servers/sockets in tests — `TestApp` is the contract.
- Don't build special "test mode" branches into handlers — if a handler needs
  faking, model the thing being faked as a dependency and override it.
````

- [ ] **Step 5: Run the doc-tests, then commit**

Run: `cargo test --doc -p jerrycan`
Expected: ALL pages PASS.

```bash
git add docs/ai crates/jerrycan/src/lib.rs
git commit -m "Add errors, middleware, and testing docs pages"
```

---

### Task 20: MCP tool contracts + design.json schema (machine-readable, test-gated)

**Files:**
- Create: `docs/contracts/mcp-tools.json`
- Create: `docs/contracts/design-schema.json`
- Create: `crates/jerrycan/tests/contracts.rs`

- [ ] **Step 1: Write `docs/contracts/mcp-tools.json`**

The 9 tools of spec §7.2. Workflow tools always return `next_step` — the golden-path hint.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://jerrycan.cc/schemas/mcp-tools-v0.json",
  "contract_version": 0,
  "tools": [
    {
      "name": "jerrycan_design",
      "description": "Turn requirements into a validated design.json. Incomplete designs return pointed questions, not code. Call repeatedly with answers until status=complete.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "requirements": { "type": "string", "description": "What the backend must do, in natural language." },
          "answers": { "type": "object", "description": "Answers to a previous call's questions, keyed by question id." },
          "revision_of": { "type": "string", "description": "Path to an existing design.json being revised." }
        },
        "required": ["requirements"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "status": { "enum": ["complete", "questions"] },
          "design": { "$ref": "https://jerrycan.cc/schemas/design-v0.json" },
          "questions": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" }, "question": { "type": "string" } }, "required": ["id", "question"] } },
          "next_step": { "type": "string" }
        },
        "required": ["status", "next_step"]
      }
    },
    {
      "name": "jerrycan_scaffold",
      "description": "Create a new app workspace from a complete design.json: app/ shell, shared/ crate, one route-module crate per design module.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "design_path": { "type": "string" },
          "directory": { "type": "string", "description": "Target directory for the new workspace." }
        },
        "required": ["design_path", "directory"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "created": { "type": "array", "items": { "type": "string" } },
          "next_step": { "type": "string" }
        },
        "required": ["created", "next_step"]
      }
    },
    {
      "name": "jerrycan_generate",
      "description": "Incremental generator (Angular-style): add a route module, subroute, or dependency to an existing app. Regenerates mounting and workspace members deterministically.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "kind": { "enum": ["route", "subroute", "dependency", "middleware"] },
          "path": { "type": "string", "description": "route: module name (todos); subroute: parent/child (todos/comments)." },
          "module": { "type": "string", "description": "Owning module for kind=dependency|middleware." },
          "design_slice": { "type": "object", "description": "The design.json fragment for the new unit." }
        },
        "required": ["kind", "path"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "created": { "type": "array", "items": { "type": "string" } },
          "modified": { "type": "array", "items": { "type": "string" } },
          "next_step": { "type": "string" }
        },
        "required": ["created", "modified", "next_step"]
      }
    },
    {
      "name": "jerrycan_gen_tests",
      "description": "Generate the failing acceptance test suite for a module from its design slice — one test per endpoint and error case. TDD: run before implementing handlers.",
      "inputSchema": {
        "type": "object",
        "properties": { "module": { "type": "string" } },
        "required": ["module"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "tests_created": { "type": "array", "items": { "type": "string" } },
          "expected_failing": { "type": "integer" },
          "next_step": { "type": "string" }
        },
        "required": ["tests_created", "expected_failing", "next_step"]
      }
    },
    {
      "name": "jerrycan_check",
      "description": "The verification gate: build + clippy(deny) + cargo-audit + cargo-deny + tests + jerrycan lints. Scope to one module for fast iteration. Diagnostics are machine-readable with doc links.",
      "inputSchema": {
        "type": "object",
        "properties": { "module": { "type": "string", "description": "Omit for the full workspace." } },
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "ok": { "type": "boolean" },
          "diagnostics": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "code": { "type": "string" },
                "file": { "type": "string" },
                "line": { "type": "integer" },
                "message": { "type": "string" },
                "suggestion": { "type": "string" },
                "doc_url": { "type": "string" }
              },
              "required": ["code", "message"]
            }
          },
          "next_step": { "type": "string" }
        },
        "required": ["ok", "diagnostics", "next_step"]
      }
    },
    {
      "name": "jerrycan_package",
      "description": "Produce hardened deploy artifacts (+ CycloneDX SBOM). Refuses to run unless the full-workspace check is green.",
      "inputSchema": {
        "type": "object",
        "properties": { "target": { "enum": ["docker", "binary", "k8s", "systemd"] } },
        "required": ["target"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "artifacts": { "type": "array", "items": { "type": "string" } },
          "sbom": { "type": "string" },
          "next_step": { "type": "string" }
        },
        "required": ["artifacts", "sbom", "next_step"]
      }
    },
    {
      "name": "jerrycan_docs_search",
      "description": "Search the AI-native docs. Returns page+anchor+snippet hits.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "query": { "type": "string" },
          "limit": { "type": "integer", "default": 5 }
        },
        "required": ["query"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": {
          "results": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": { "page": { "type": "string" }, "anchor": { "type": "string" }, "snippet": { "type": "string" } },
              "required": ["page", "snippet"]
            }
          }
        },
        "required": ["results"]
      }
    },
    {
      "name": "jerrycan_docs_get",
      "description": "Fetch one docs page (optionally one anchored section) as markdown. Pages follow a fixed shape: Purpose, Signature, Minimal example, Variations, Errors you'll hit, Anti-patterns.",
      "inputSchema": {
        "type": "object",
        "properties": { "page": { "type": "string" }, "anchor": { "type": "string" } },
        "required": ["page"],
        "additionalProperties": false
      },
      "outputSchema": {
        "type": "object",
        "properties": { "markdown": { "type": "string" } },
        "required": ["markdown"]
      }
    },
    {
      "name": "jerrycan_list_routes",
      "description": "The live route tree of the current app: method, path, owning module, handler. The resume-work map for agents.",
      "inputSchema": { "type": "object", "additionalProperties": false },
      "outputSchema": {
        "type": "object",
        "properties": {
          "routes": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": { "method": { "type": "string" }, "path": { "type": "string" }, "module": { "type": "string" }, "handler": { "type": "string" } },
              "required": ["method", "path", "module", "handler"]
            }
          }
        },
        "required": ["routes"]
      }
    }
  ]
}
```

- [ ] **Step 2: Write `docs/contracts/design-schema.json`**

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://jerrycan.cc/schemas/design-v0.json",
  "title": "jerrycan design.json",
  "description": "The validated design an app is generated from. Module-grouped with nested subroutes (spec §5). Produced by jerrycan_design; consumed by scaffold/generate/gen_tests.",
  "type": "object",
  "required": ["name", "contract_version", "modules"],
  "additionalProperties": false,
  "properties": {
    "name": { "type": "string", "pattern": "^[a-z][a-z0-9-]*$" },
    "contract_version": { "const": 0 },
    "description": { "type": "string" },
    "auth": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "model": { "enum": ["none", "session", "jwt"] },
        "roles": { "type": "array", "items": { "type": "string" } }
      },
      "required": ["model"]
    },
    "modules": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/module" } }
  },
  "$defs": {
    "module": {
      "type": "object",
      "required": ["name", "endpoints"],
      "additionalProperties": false,
      "properties": {
        "name": { "type": "string", "pattern": "^[a-z][a-z0-9-]*$" },
        "description": { "type": "string" },
        "entities": { "type": "array", "items": { "$ref": "#/$defs/entity" } },
        "endpoints": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/endpoint" } },
        "subroutes": { "type": "array", "items": { "$ref": "#/$defs/module" } },
        "dependencies": { "type": "array", "items": { "type": "string" }, "description": "Module-scoped dependency names (factories or values) the generator must stub." }
      }
    },
    "entity": {
      "type": "object",
      "required": ["name", "fields"],
      "additionalProperties": false,
      "properties": {
        "name": { "type": "string", "pattern": "^[A-Z][A-Za-z0-9]*$" },
        "fields": {
          "type": "array",
          "minItems": 1,
          "items": {
            "type": "object",
            "required": ["name", "type"],
            "additionalProperties": false,
            "properties": {
              "name": { "type": "string", "pattern": "^[a-z][a-z0-9_]*$" },
              "type": { "enum": ["string", "integer", "float", "boolean", "datetime", "uuid", "json"] },
              "required": { "type": "boolean", "default": true }
            }
          }
        }
      }
    },
    "endpoint": {
      "type": "object",
      "required": ["operation_id", "method", "path", "success"],
      "additionalProperties": false,
      "properties": {
        "operation_id": { "type": "string", "pattern": "^[a-z][a-z0-9_]*$", "description": "Becomes the handler fn name — lint-enforced (spec §5.3)." },
        "method": { "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"] },
        "path": { "type": "string", "pattern": "^/" },
        "auth_required": { "type": "boolean", "default": false },
        "request_body": {
          "type": "object",
          "additionalProperties": false,
          "properties": { "entity": { "type": "string" } }
        },
        "success": {
          "type": "object",
          "required": ["status"],
          "additionalProperties": false,
          "properties": {
            "status": { "type": "integer", "minimum": 200, "maximum": 299 },
            "entity": { "type": "string" },
            "list": { "type": "boolean", "default": false }
          }
        },
        "errors": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["status", "when"],
            "additionalProperties": false,
            "properties": {
              "status": { "type": "integer", "minimum": 400, "maximum": 599 },
              "code": { "type": "string", "pattern": "^JC[0-9]{4}$" },
              "when": { "type": "string" }
            }
          }
        }
      }
    }
  }
}
```

- [ ] **Step 3: Write the failing contract tests**

Create `crates/jerrycan/tests/contracts.rs`:

```rust
//! Syntax + invariant gate for the platform contracts (full JSON-Schema
//! validation arrives with the MCP implementation in Phase 1).

use serde_json::Value;

fn load(rel: &str) -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/contracts/");
    let raw = std::fs::read_to_string(format!("{path}{rel}"))
        .unwrap_or_else(|e| panic!("missing contract file {rel}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{rel} is not valid JSON: {e}"))
}

#[test]
fn mcp_tools_contract_holds_its_invariants() {
    let doc = load("mcp-tools.json");
    let tools = doc["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 9, "spec §7.2 defines exactly 9 tools");

    let mut names = std::collections::HashSet::new();
    let workflow = [
        "jerrycan_design", "jerrycan_scaffold", "jerrycan_generate",
        "jerrycan_gen_tests", "jerrycan_check", "jerrycan_package",
    ];
    for t in tools {
        let name = t["name"].as_str().expect("tool name");
        assert!(name.starts_with("jerrycan_"), "{name}: tools are jerrycan_-prefixed");
        assert!(names.insert(name.to_string()), "{name}: duplicate tool");
        assert!(t["description"].as_str().is_some_and(|d| d.len() > 20), "{name}: real description");
        assert_eq!(t["inputSchema"]["type"], "object", "{name}: object input");
        if workflow.contains(&name) {
            let required: Vec<_> = t["outputSchema"]["required"]
                .as_array().expect("required").iter().filter_map(|v| v.as_str()).collect();
            assert!(required.contains(&"next_step"), "{name}: workflow tools must return next_step");
        }
    }
}

#[test]
fn design_schema_is_module_grouped_and_recursive() {
    let doc = load("design-schema.json");
    assert_eq!(doc["$id"], "https://jerrycan.cc/schemas/design-v0.json");
    assert_eq!(doc["properties"]["modules"]["items"]["$ref"], "#/$defs/module");
    // Subroutes recurse into the same module definition (spec §5.1 "fractal").
    assert_eq!(doc["$defs"]["module"]["properties"]["subroutes"]["items"]["$ref"], "#/$defs/module");
    // operation_id is the handler-name contract used by the §5.3 naming lint.
    assert!(doc["$defs"]["endpoint"]["properties"]["operation_id"]["pattern"].is_string());
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan --test contracts`
Expected: 2 tests PASS (they fail loudly if the JSON drifts).

- [ ] **Step 5: Commit**

```bash
git add docs/contracts/mcp-tools.json docs/contracts/design-schema.json crates/jerrycan/tests/contracts.rs
git commit -m "Add MCP tool contracts and design.json schema with invariant tests"
```

---
### Task 21: CLI UX specification

**Files:**
- Create: `docs/contracts/cli-ux.md`

- [ ] **Step 1: Write `docs/contracts/cli-ux.md`**

````markdown
# jerrycan CLI — UX Specification (contract v0)

One binary, two audiences: humans debugging, agents working. Every command has
a `--json` mode whose output is the same data the MCP tool returns.

## Global conventions

- **Output:** human-readable progress → stderr; results → stdout.
  With `--json`: stdout is exactly one JSON document matching the MCP tool's
  outputSchema (docs/contracts/mcp-tools.json); stderr stays human.
- **`next_step`:** every workflow command's JSON output includes `next_step`,
  the golden-path hint (e.g. after `new` → "run jerrycan gen-tests --module todos").
- **Exit codes:** `0` success · `1` the gate failed (check/test failures, conflicts) ·
  `2` usage error (unknown flag, missing arg) · `3` environment error (no cargo, no git).
- **Color:** auto (TTY only); `NO_COLOR` honored.
- **Env:** `JERRYCAN_ADDR` (serve bind), `JERRYCAN_ENV=dev|prod` (error verbosity; prod is the default when packaged).

## Commands (v0 surface — mirrors spec §7.1)

| Command | Args/Flags | Behavior | MCP twin |
|---|---|---|---|
| `jerrycan new <name>` | `--design <file>` (required) | Scaffold workspace from validated design: `app/`, `shared/`, one route crate per module | jerrycan_scaffold |
| `jerrycan generate route <path>` | alias `g`; `<path>`=`todos` or `todos/comments` | New module crate or subroute; rewires mounting + workspace deterministically; emits failing tests | jerrycan_generate |
| `jerrycan generate dep <name>` | `--module <m>` (required) | Module-scoped dependency stub (factory fn + registration) | jerrycan_generate |
| `jerrycan gen-tests` | `--module <m>` (required) | Failing acceptance tests from the module's design slice | jerrycan_gen_tests |
| `jerrycan list routes` | `--json` | Route tree: METHOD path → module::handler | jerrycan_list_routes |
| `jerrycan dev` | `--addr <a>` | Run with auto-reload (debounced rebuild) | — |
| `jerrycan check` | `--module <m>` | build → clippy(-D warnings) → cargo-audit → cargo-deny → tests → jerrycan lints; first failure class reported, all diagnostics collected | jerrycan_check |
| `jerrycan test` | `--module <m>` | The app's test suite only (subset of check) | — |
| `jerrycan package` | `--docker\|--binary\|--k8s\|--systemd` | Hardened artifact + CycloneDX SBOM; refuses unless full check is green | jerrycan_package |
| `jerrycan docs <topic>` | `--search <q>` | Render docs page in terminal / search | jerrycan_docs_get / _search |
| `jerrycan add <extension>` | | Wire a jerrycan-* extension crate (Phase 2+) | — |
| `jerrycan mcp` | | Serve MCP over stdio (Phase 1) | — |

## Diagnostics format (check, human mode)

```
error[JC0405]: mutating route without auth guard
  --> crates/routes/todos/src/lib.rs:14
   = note: POST /todos has no Dep<…> guard and auth.model = "session"
   = help: add a guard dependency, e.g. `_user: Dep<CurrentUser>`
   = docs: jerrycan docs dependencies#guards
```

Same payload in `--json`: `{code, file, line, message, suggestion, doc_url}` —
identical to the MCP outputSchema. One diagnostics pipeline, two renderings.

## Non-goals (v0)

- No interactive prompts, ever — agents can't answer TTY prompts. Missing
  input = exit 2 with the exact flag to provide.
- No telemetry.
- No deploy execution (`kubectl`, ssh) — `package` ends at artifacts + SBOM.
````

- [ ] **Step 2: Sanity-check the table against the MCP contract**

Run: `grep -o 'jerrycan_[a-z_]*' docs/contracts/cli-ux.md | sort -u`
Expected: every name listed exists in `docs/contracts/mcp-tools.json` (eyeball or `grep -c` each). Mismatch = fix whichever file is wrong.

- [ ] **Step 3: Commit**

```bash
git add docs/contracts/cli-ux.md
git commit -m "Add CLI UX specification"
```

---

### Task 22: CI gate + Phase 0 exit verification

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/ci.yml`:

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always
  RUSTDOCFLAGS: -D warnings

jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy (deny warnings)
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Tests (unit + integration + doc-tests)
        run: cargo test --workspace
      - name: Docs build (broken intra-doc links are errors)
        run: cargo doc --workspace --no-deps
```

(cargo-audit/cargo-deny join the workflow in Phase 1 when the `check` command wires them; the dependency tree today is the floor we already audited by choosing it.)

- [ ] **Step 2: Run the full Phase 0 exit criterion locally**

Run, in order:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # includes every docs/ai example as a doc-test
cargo doc --workspace --no-deps
```

Expected: ALL green. That is spec §11 Phase 0's exit: **"All doc examples compile against stub crates"** — they don't just compile; they run.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "Add CI workflow: fmt, clippy, tests with doc-tests, docs build"
```

---

## Execution notes

- **Order:** 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 (middleware) → 10 (router) → 11 → … → 22. Tasks 4+5 share one commit (Task 4 ends red by design; Task 5's Step 5 runs both modules' tests).
- **Out of scope for Phase 0** (deliberately, per spec): the CLI/MCP *implementations* (Phase 1), security-header middleware + timeouts (Phase 1), percent-decoding + router fuzzing (Phase 1), `jerrycan-db/auth/validate/observe` content (Phases 2–3), publishing 0.1.0 (Phase 4).
- **If a signature can't be made to compile elegantly** (the spike's whole purpose): stop, document the friction in the plan file under a "## Spike findings" section, adjust the docs page FIRST, then the implementation — docs are the contract (spec §2 build strategy).
- **Phase 0 exit deliverables checklist:** `jerrycan-core` with passing DI/Module/e2e tests · 7 doc-tested docs pages · `mcp-tools.json` + `design-schema.json` + invariant tests · `cli-ux.md` · CI green.
