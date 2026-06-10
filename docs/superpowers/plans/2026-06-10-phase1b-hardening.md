# jerrycan Phase 1b — Framework Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the spec §4.4 secure defaults that Phase 0/1 deferred — security headers, timeouts, panic containment, accept-loop resilience, graceful shutdown, percent-decoding — plus multi-param `Path`, macro span preservation, dep-version hygiene, and two generator UX fixes, clearing the Phase 1 backlog.

**Architecture:** Almost everything lands in `jerrycan-core` (app.rs dispatch/serve path, router, extract) following the existing patterns: secure defaults applied at the single dispatch exit so TestApp sees them too; serve-path-only concerns (panic catch, shutdown, accept resilience) stay in `serve_with_shutdown`, which `serve`/`serve_with` now delegate to. Multi-param Path ripples deterministically through questions → genroute → docs. Fuzzing explicitly stays Phase 4 (roadmap row owns it); `jerrycan dev` SIGTERM process-grouping stays in the backlog (needs unsafe/libc, conflicts with forbid(unsafe_code) — accepted v0 limitation).

**Tech Stack:** No new dependencies. Workspace tokio gains the `signal` feature (graceful shutdown). Everything else is std + existing deps.

**Source authority:** spec §4.4 (`docs/superpowers/specs/2026-06-09-jerrycan-design.md`), `docs/phase1-backlog.md`, error-code conventions in `crates/jerrycan-core/src/error.rs`.

**New stable error codes introduced here:** `JC0503` (handler timeout, HTTP 503). Percent-decoding failures reuse `JC0400`; panics reuse `JC0500`.

---

## File Structure

```
Cargo.toml                                  # MODIFY: tokio +signal; [workspace.dependencies] internal crates
crates/jerrycan-core/src/
├── error.rs                                # MODIFY: handler_timeout() JC0503 + test line
├── app.rs                                  # MODIFY: security headers, handler timeout, panic catch,
│                                           #   accept-error classification, serve_with_shutdown
├── router.rs                               # MODIFY: percent-decoding + RouteMatch::Malformed
├── extract.rs                              # MODIFY: Path<(A,B)> and Path<(A,B,C)> impls
crates/jerrycan-core/tests/
├── hardening.rs                            # CREATE: headers/timeout/panic/shutdown/decoding (mixed in-process + TCP)
├── e2e.rs                                  # (unchanged; existing tests must stay green)
crates/jerrycan-macros/src/lib.rs           # MODIFY: span-preserving token re-emit
crates/jerrycan/Cargo.toml                  # MODIFY: internal deps via workspace
crates/jerrycan/src/platform/
├── questions.rs                            # MODIFY: param limit 1 → 3
├── genroute.rs                             # MODIFY: multi-param handler mapping; route-count helper
├── mcp_dispatch.rs                         # MODIFY: warn-on-route-reduction + name-mismatch hint
├── main.rs (crates/jerrycan/src/main.rs)   # MODIFY: warn-on-route-reduction in cmd_generate_route
docs/ai/01-app.md                           # MODIFY: secure-defaults section (doc-tested)
docs/ai/03-extractors.md                    # MODIFY: multi-param Path (replaces the v0 limitation bullet)
docs/ai/05-errors.md                        # MODIFY: JC0503 row; JC0400 decoding note
docs/phase1-backlog.md                      # MODIFY: clear completed items; annotate deferrals
README.md                                   # MODIFY: Phase 1 row → fully complete
```

**Conventions (unchanged from Phase 0/1 execution):** repo root; before EVERY commit `cargo fmt --all` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace` green; plain commit messages (no Co-Authored-By/Claude); `#![forbid(unsafe_code)]`; heavy tests `#[ignore]`; if plan code fails to compile fix minimally and record; design-level failure → BLOCKED.

---

### Task 1: `JC0503` handler-timeout error code

**Files:**
- Modify: `crates/jerrycan-core/src/error.rs`

- [ ] **Step 1: Write the failing assertions** — in the existing `errors_carry_status_and_stable_code` test add:

```rust
        assert_eq!(Error::handler_timeout().code(), "JC0503");
        assert_eq!(Error::handler_timeout().status(), StatusCode::SERVICE_UNAVAILABLE);
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan-core error` → compile FAIL (no `handler_timeout`).

- [ ] **Step 3: Implement** — after `unprocessable`, add:

```rust
    /// The handler exceeded the configured time budget (spec §4.4 timeouts).
    pub fn handler_timeout() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "JC0503",
            "handler timed out",
        )
    }
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan-core error` → green. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/error.rs
git commit -m "Add JC0503 handler timeout error code"
```

---

### Task 2: Security headers — on by default, explicit opt-out

Spec §4.4: "Security headers on every response… opting out requires explicit code." Applied at the single dispatch exit, so `TestApp`, `serve`, 404s, and error responses ALL carry them; handler-set headers win (we never overwrite).

**Files:**
- Modify: `crates/jerrycan-core/src/app.rs`
- Create: `crates/jerrycan-core/tests/hardening.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan-core/tests/hardening.rs`:

```rust
//! Spec §4.4 secure defaults: headers, timeouts, panic containment, shutdown.

use jerrycan_core::{get, App, Json, NoContent};

#[tokio::test]
async fn security_headers_are_on_every_response_including_errors() {
    let t = App::new()
        .route("/ok", get(|| async { Json(1) }))
        .into_test();

    for path in ["/ok", "/missing"] {
        let res = t.get(path).await;
        let h = res.headers();
        assert_eq!(h["x-content-type-options"], "nosniff", "{path}");
        assert_eq!(h["x-frame-options"], "DENY", "{path}");
        assert_eq!(h["referrer-policy"], "no-referrer", "{path}");
        assert_eq!(h["content-security-policy"], "default-src 'none'", "{path}");
        assert_eq!(h["cache-control"], "no-store", "{path}");
    }
}

#[tokio::test]
async fn handler_set_headers_win_over_defaults() {
    async fn cached() -> jerrycan_core::Response {
        let mut res = jerrycan_core::IntoResponse::into_response("ok");
        res.headers_mut().insert(
            jerrycan_core::http::header::CACHE_CONTROL,
            jerrycan_core::http::HeaderValue::from_static("max-age=60"),
        );
        res
    }
    let t = App::new().route("/", get(cached)).into_test();
    let res = t.get("/").await;
    assert_eq!(res.headers()["cache-control"], "max-age=60", "handler wins");
    assert_eq!(res.headers()["x-frame-options"], "DENY", "others still applied");
}

#[tokio::test]
async fn security_headers_can_be_explicitly_disabled() {
    let t = App::new()
        .route("/", get(|| async { NoContent }))
        .security_headers(false)
        .into_test();
    let res = t.get("/").await;
    assert!(res.headers().get("x-frame-options").is_none());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan-core --test hardening` → compile FAIL (`security_headers` method missing) / header assertions fail.

- [ ] **Step 3: Implement in `app.rs`**

(a) Add a field to `App` (and default `true`) and to `BuiltApp`:

```rust
// In struct App { ... } add:
    security_headers: bool,
```

`App` derives `Default` — `bool::default()` is `false`, but the contract is default ON. Replace the derive-based `App::new()` with an explicit constructor (keep `#[derive(Default)]` REMOVED if it conflicts; the explicit impl is the source of truth):

```rust
impl Default for App {
    fn default() -> Self {
        Self {
            routes: Vec::new(),
            mounts: Vec::new(),
            env: DepEnv::default(),
            middleware: Vec::new(),
            security_headers: true,
        }
    }
}
```

(Remove the `#[derive(Default)]` attribute from `App` and keep `pub fn new() -> Self { Self::default() }`.)

(b) Builder method on `App`:

```rust
    /// Secure-by-default headers on every response (spec §4.4). Opting out
    /// must be explicit — that is the contract.
    pub fn security_headers(mut self, on: bool) -> Self {
        self.security_headers = on;
        self
    }
```

(c) `BuiltApp` gains `pub(crate) security_headers: bool`, set in `build()` from `self.security_headers`.

(d) The header applicator + use at dispatch exit:

```rust
/// Defaults chosen for API-only services; handler-set values always win.
pub(crate) fn apply_security_headers(res: &mut Response) {
    const DEFAULTS: [(&str, &str); 5] = [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'none'"),
        ("cache-control", "no-store"),
    ];
    for (name, value) in DEFAULTS {
        let header_name = http::HeaderName::from_static(name);
        if !res.headers().contains_key(&header_name) {
            res.headers_mut()
                .insert(header_name, http::HeaderValue::from_static(value));
        }
    }
}
```

Restructure `dispatch` so every path flows through one exit:

```rust
    pub(crate) async fn dispatch(&self, parts: http::request::Parts, body: Bytes) -> Response {
        let mut response = self.dispatch_inner(parts, body).await;
        if self.security_headers {
            apply_security_headers(&mut response);
        }
        response
    }

    async fn dispatch_inner(&self, parts: http::request::Parts, body: Bytes) -> Response {
        // ... the ENTIRE previous dispatch body moves here unchanged ...
    }
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan-core --test hardening` → 3 green; `cargo test --workspace` all green (the 07-testing doc-test asserting `content-type` is unaffected — we never overwrite existing headers and never set content-type). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/app.rs crates/jerrycan-core/tests/hardening.rs
git commit -m "Apply security headers to every response by default with explicit opt-out"
```

---

### Task 3: Handler timeout (default 30s, configurable)

In `dispatch`, so TestApp and serve both enforce it. Read-path timeouts (header read, body collect) land in Task 6 alongside the serve_with restructure.

**Files:**
- Modify: `crates/jerrycan-core/src/app.rs`
- Modify: `crates/jerrycan-core/tests/hardening.rs`

- [ ] **Step 1: Write the failing tests (append to hardening.rs)**

```rust
use std::time::Duration;

#[tokio::test]
async fn slow_handlers_hit_the_timeout_with_jc0503() {
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_secs(5)).await;
        "too late"
    }
    let t = App::new()
        .route("/slow", get(slow))
        .handler_timeout(Duration::from_millis(50))
        .into_test();
    let res = t.get("/slow").await;
    assert_eq!(res.status(), jerrycan_core::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(res.text().contains("JC0503"), "{}", res.text());
}

#[tokio::test]
async fn fast_handlers_are_unaffected_by_the_default_timeout() {
    let t = App::new().route("/", get(|| async { "quick" })).into_test();
    assert_eq!(t.get("/").await.text(), "quick");
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL (`handler_timeout` missing).

- [ ] **Step 3: Implement in app.rs**

`App` field + Default arm + builder:

```rust
// field:
    handler_timeout: std::time::Duration,
// in Default::default():
    handler_timeout: std::time::Duration::from_secs(30),
// builder:
    /// Per-request handler time budget (default 30s — spec §4.4). Exceeding it
    /// returns 503 JC0503 without killing the connection or the server.
    pub fn handler_timeout(mut self, budget: std::time::Duration) -> Self {
        self.handler_timeout = budget;
        self
    }
```

`BuiltApp` gains `pub(crate) handler_timeout: Duration` (set in build()). Wrap the routed-handler execution inside `dispatch_inner`'s `Found` arm:

```rust
            RouteMatch::Found { endpoint, params } => {
                let mut ctx = RequestCtx::new(
                    parts,
                    body,
                    DepResolver::new(endpoint.env.clone(), self.overrides.clone()),
                );
                ctx.params = params;
                let handler: &BoxHandlerFn =
                    endpoint.methods.get(&method).expect("find() checked the method");
                let run = Next { chain: &endpoint.middleware, endpoint: handler }.run(&mut ctx);
                match tokio::time::timeout(self.handler_timeout, run).await {
                    Ok(response) => response,
                    Err(_) => Error::handler_timeout().into_response(),
                }
            }
```

- [ ] **Step 4: Run to verify pass** — hardening tests green; full workspace green (default 30s touches nothing). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/app.rs crates/jerrycan-core/tests/hardening.rs
git commit -m "Enforce 30s default handler timeout returning 503 JC0503"
```

---
### Task 4: Panic → 500 containment on the serve path

A panicking handler must cost ONE response, not the connection or the server. Catch via `tokio::spawn`'s JoinError (no unsafe, no new deps). Deliberately NOT in `dispatch`: TestApp keeps propagating panics so unit tests fail loudly — document that.

**Files:**
- Modify: `crates/jerrycan-core/src/app.rs`
- Modify: `crates/jerrycan-core/tests/hardening.rs`

- [ ] **Step 1: Write the failing TCP test (append to hardening.rs)**

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn raw_get(addr: &str, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn handler_panics_become_500_and_the_server_survives() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new()
        .route("/boom", get(|| async { if true { panic!("kaboom") } "x" }))
        .route("/fine", get(|| async { "still here" }));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    let res = raw_get(&addr, "/boom").await;
    assert!(res.starts_with("HTTP/1.1 500"), "panic must become a 500: {res}");
    assert!(res.contains("JC0500"), "{res}");

    let res = raw_get(&addr, "/fine").await;
    assert!(res.starts_with("HTTP/1.1 200") && res.ends_with("still here"), "server must survive: {res}");
    server.abort();
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan-core --test hardening handler_panics` — today the panicking task drops the connection: the first assertion fails (empty/reset response).

- [ ] **Step 3: Implement** — in `serve_with`'s connection service (the closure currently doing `app.dispatch(parts, collected.to_bytes()).await`), route through a spawned task:

```rust
                        let response = match limited.collect().await {
                            Ok(collected) => {
                                let app = app.clone();
                                let body = collected.to_bytes();
                                match tokio::spawn(async move { app.dispatch(parts, body).await }).await {
                                    Ok(response) => response,
                                    Err(_join_error) => {
                                        // A panic in agent-written handler code costs one
                                        // response, never the connection or the server.
                                        let mut response =
                                            Error::internal("handler panicked").into_response();
                                        apply_security_headers(&mut response);
                                        response
                                    }
                                }
                            }
                            Err(_) => Error::payload_too_large().into_response(),
                        };
```

Also apply `apply_security_headers` to the 413 arm's response (it bypasses dispatch):

```rust
                            Err(_) => {
                                let mut response = Error::payload_too_large().into_response();
                                apply_security_headers(&mut response);
                                response
                            }
```

(If `serve_with` cannot see a `BuiltApp.security_headers` flag at that point, apply unconditionally on these two bypass paths — they are framework-error responses, not handler output; note the choice in the report.)

Doc note on `TestApp` (test_client.rs doc comment, one line): "Panics in handlers propagate in tests by design — the serve path converts them to 500 JC0500."

- [ ] **Step 4: Run to verify pass** — hardening + e2e + full workspace green. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/app.rs crates/jerrycan-core/src/test_client.rs crates/jerrycan-core/tests/hardening.rs
git commit -m "Contain handler panics as 500 responses on the serve path"
```

---

### Task 5: Accept-loop resilience (transient errors don't kill the server)

Closes the `TODO(phase1)` in app.rs: EMFILE/ENFILE/ECONNABORTED/ECONNRESET/EINTR are turbulence, not death.

**Files:**
- Modify: `crates/jerrycan-core/src/app.rs`

- [ ] **Step 1: Write the failing unit test (in app.rs `mod tests`)**

```rust
    #[test]
    fn accept_error_classification_matches_unix_reality() {
        use std::io::{Error as IoError, ErrorKind};
        for transient in [
            IoError::from(ErrorKind::ConnectionAborted),
            IoError::from(ErrorKind::ConnectionReset),
            IoError::from(ErrorKind::Interrupted),
            IoError::from_raw_os_error(24), // EMFILE
            IoError::from_raw_os_error(23), // ENFILE
        ] {
            assert!(is_transient_accept_error(&transient), "{transient:?}");
        }
        assert!(!is_transient_accept_error(&IoError::from(ErrorKind::InvalidInput)));
        assert!(!is_transient_accept_error(&IoError::from(ErrorKind::PermissionDenied)));
    }
```

- [ ] **Step 2: Run to verify failure** — compile FAIL.

- [ ] **Step 3: Implement**

```rust
/// Accept errors that mean "back off and keep serving", not "die":
/// aborted/reset handshakes, signal interruptions, and fd exhaustion
/// (EMFILE/ENFILE — kind-mapping varies by platform, so match raw errno too).
fn is_transient_accept_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    ) || matches!(e.raw_os_error(), Some(23) | Some(24))
}
```

Replace the accept site (currently `.map_err(...)?` with the TODO comment) with:

```rust
            let (stream, _) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) if is_transient_accept_error(&e) => {
                    eprintln!("jerrycan: transient accept error ({e}); backing off 50ms");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                Err(e) => return Err(Error::internal(format!("accept failed fatally: {e}"))),
            };
```

Delete the `// TODO(phase1): tolerate transient accept() errors...` comment.

- [ ] **Step 4: Run to verify pass** — unit + e2e green. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/app.rs
git commit -m "Back off on transient accept errors instead of killing the server"
```

---

### Task 6: Graceful shutdown + read-path timeouts (`serve_with_shutdown`)

One restructure delivers both backlog items: `serve_with_shutdown(listener, shutdown_future)` is the real engine (select accept vs shutdown; drain in-flight with a 10s cap); `serve()` feeds it `tokio::signal::ctrl_c()`; `serve_with` feeds it `pending()` (behavior-compatible with every existing test). Header-read timeout via hyper's http1 builder; body-collect timeout wraps the Limited read.

**Files:**
- Modify: `Cargo.toml` (workspace tokio features: add `signal`)
- Modify: `crates/jerrycan-core/src/app.rs`
- Modify: `crates/jerrycan-core/tests/hardening.rs`

- [ ] **Step 1: Write the failing tests (append to hardening.rs)**

```rust
#[tokio::test]
async fn graceful_shutdown_drains_in_flight_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (trigger, shutdown) = tokio::sync::oneshot::channel::<()>();

    async fn slow_ok() -> &'static str {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "drained"
    }
    let app = App::new().route("/slow", get(slow_ok));
    let server = tokio::spawn(async move {
        app.serve_with_shutdown(listener, async {
            let _ = shutdown.await;
        })
        .await
    });

    // Start an in-flight request, then trigger shutdown mid-handler.
    let addr2 = addr.clone();
    let inflight = tokio::spawn(async move { raw_get(&addr2, "/slow").await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    trigger.send(()).unwrap();

    let res = inflight.await.unwrap();
    assert!(res.starts_with("HTTP/1.1 200") && res.ends_with("drained"), "in-flight must complete: {res}");

    // serve_with_shutdown returns Ok after draining…
    let served = tokio::time::timeout(Duration::from_secs(5), server).await
        .expect("server drains within the cap")
        .unwrap();
    assert!(served.is_ok());

    // …and the listener is gone.
    assert!(tokio::net::TcpStream::connect(&addr).await.is_err(), "no new connections after shutdown");
}

#[tokio::test]
async fn glacial_request_bodies_are_cut_off() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = App::new()
        .route("/echo", jerrycan_core::post(|b: Json<String>| async move { b }))
        .body_read_timeout(Duration::from_millis(200));
    let server = tokio::spawn(async move { app.serve_with(listener).await });

    // Send headers claiming a body, then stall forever.
    let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
    s.write_all(b"POST /echo HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: 5\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    // No body bytes sent. Server must answer (408-class via JC0400 family) or close within ~1s, not hang.
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    assert!(read.is_ok(), "server must not hang on a stalled body");
    let text = String::from_utf8_lossy(&buf);
    // Either an explicit 408 response or a clean close are acceptable cut-offs:
    assert!(text.is_empty() || text.contains("408"), "got: {text}");
    server.abort();
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL (`serve_with_shutdown`, `body_read_timeout` missing).

- [ ] **Step 3: Implement**

(a) Workspace `Cargo.toml`: tokio features become `["macros", "rt-multi-thread", "net", "time", "sync", "signal"]`.

(b) `App` fields + Default + builder (alongside handler_timeout):

```rust
// field:
    body_read_timeout: std::time::Duration,
// Default::default():
    body_read_timeout: std::time::Duration::from_secs(30),
// builder:
    /// Time budget for reading a request body (default 30s — spec §4.4).
    pub fn body_read_timeout(mut self, budget: std::time::Duration) -> Self {
        self.body_read_timeout = budget;
        self
    }
```

`BuiltApp` carries `pub(crate) body_read_timeout: Duration` (set in build()).

(c) Restructure the serve path:

```rust
    /// Bind from config and serve until Ctrl-C, then drain gracefully.
    pub async fn serve(self) -> Result<()> {
        let addr = std::env::var("JERRYCAN_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("failed to bind {addr}: {e}")))?;
        self.serve_with_shutdown(listener, async {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("jerrycan: shutdown signal received — draining");
        })
        .await
    }

    /// Serve on an existing listener forever (tests, port 0, socket activation).
    pub async fn serve_with(self, listener: tokio::net::TcpListener) -> Result<()> {
        self.serve_with_shutdown(listener, std::future::pending()).await
    }

    /// The serve engine: accept until `shutdown` resolves, then stop accepting,
    /// drain in-flight connections (10s cap), and return.
    pub async fn serve_with_shutdown(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<()> {
        const BODY_LIMIT: usize = 1024 * 1024; // 1 MiB — spec §4.4
        const DRAIN_CAP: std::time::Duration = std::time::Duration::from_secs(10);
        const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

        let built = Arc::new(self.build()?);
        let mut connections = tokio::task::JoinSet::new();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(pair) => pair,
                        Err(e) if is_transient_accept_error(&e) => {
                            eprintln!("jerrycan: transient accept error ({e}); backing off 50ms");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            continue;
                        }
                        Err(e) => return Err(Error::internal(format!("accept failed fatally: {e}"))),
                    };
                    let app = built.clone();
                    connections.spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let app = app.clone();
                            async move {
                                let (parts, body) = req.into_parts();
                                use http_body_util::BodyExt;
                                let limited = http_body_util::Limited::new(body, BODY_LIMIT);
                                let collected =
                                    tokio::time::timeout(app.body_read_timeout, limited.collect()).await;
                                let response = match collected {
                                    Ok(Ok(collected)) => {
                                        let body = collected.to_bytes();
                                        let app2 = app.clone();
                                        match tokio::spawn(async move { app2.dispatch(parts, body).await }).await {
                                            Ok(response) => response,
                                            Err(_join_error) => {
                                                let mut response =
                                                    Error::internal("handler panicked").into_response();
                                                apply_security_headers(&mut response);
                                                response
                                            }
                                        }
                                    }
                                    Ok(Err(_)) => {
                                        let mut response = Error::payload_too_large().into_response();
                                        apply_security_headers(&mut response);
                                        response
                                    }
                                    Err(_) => {
                                        let mut response = Error::new(
                                            http::StatusCode::REQUEST_TIMEOUT,
                                            "JC0408",
                                            "timed out reading the request body",
                                        )
                                        .into_response();
                                        apply_security_headers(&mut response);
                                        response
                                    }
                                };
                                Ok::<_, std::convert::Infallible>(response)
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .header_read_timeout(HEADER_READ_TIMEOUT)
                            .serve_connection(io, service)
                            .await;
                    });
                }
            }
        }

        drop(listener); // stop accepting immediately
        let drain = async {
            while connections.join_next().await.is_some() {}
        };
        if tokio::time::timeout(DRAIN_CAP, drain).await.is_err() {
            eprintln!("jerrycan: drain cap reached — aborting remaining connections");
            connections.abort_all();
        }
        Ok(())
    }
```

This SUPERSEDES Task 4/5's edits to the old `serve_with` (they evolve into this engine — Tasks 4 and 5 land first so their tests exist; this task relocates their logic verbatim into the new shape). Delete the old `serve_with` body entirely. NOTE: `JC0408` joins the stable code table — add `Error` constructor? No: it's constructed inline here once; ADD it to the docs table in Task 12 regardless (every code maps to docs).

- [ ] **Step 4: Run to verify pass** — hardening (all), e2e (all old tests must still pass — `serve_with` is behavior-compatible), full workspace. Heavy conformance: `cargo test -p jerrycan --test conformance -- --include-ignored` (generated apps gained headers; assertions check status lines + body tails — must stay green; if an assertion trips on headers, the TEST is wrong only if it assumed no extra headers — inspect before touching, and record). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jerrycan-core/src/app.rs crates/jerrycan-core/tests/hardening.rs
git commit -m "Add graceful shutdown engine with drain cap and read-path timeouts"
```

---
### Task 7: Percent-decoding of path segments

Decoding happens per segment AFTER splitting on `/` (so `%2F` inside a param can never create new segments), statics match against decoded text, params capture decoded values, and malformed encodings are a clean `400 JC0400` — never a panic, never a silent mismatch.

**Files:**
- Modify: `crates/jerrycan-core/src/router.rs`
- Modify: `crates/jerrycan-core/src/app.rs` (Malformed arm in dispatch_inner)
- Modify: `crates/jerrycan-core/tests/hardening.rs`

- [ ] **Step 1: Write the failing tests**

Router unit tests (append in router.rs `mod tests`):

```rust
    #[test]
    fn percent_encoded_segments_decode_for_statics_and_params() {
        let mut t = Trie::default();
        t.insert("/caf\u{e9}/menu", endpoint(&[Method::GET])).unwrap();
        t.insert("/todos/{id}", endpoint(&[Method::GET])).unwrap();

        // %C3%A9 = é in a STATIC segment
        assert!(matches!(t.find("/caf%C3%A9/menu", &Method::GET), RouteMatch::Found { .. }));

        // %2F decodes INSIDE the param value without creating a new segment
        match t.find("/todos/a%2Fb", &Method::GET) {
            RouteMatch::Found { params, .. } => assert_eq!(params[0].1, "a/b"),
            other => panic!("expected param capture, got no match ({})", matches!(other, RouteMatch::NotFound)),
        }

        // %20 decodes to a space
        match t.find("/todos/hello%20world", &Method::GET) {
            RouteMatch::Found { params, .. } => assert_eq!(params[0].1, "hello world"),
            _ => panic!("expected match"),
        }
    }

    #[test]
    fn malformed_percent_encodings_are_flagged_not_matched() {
        let mut t = Trie::default();
        t.insert("/todos/{id}", endpoint(&[Method::GET])).unwrap();
        assert!(matches!(t.find("/todos/%zz", &Method::GET), RouteMatch::Malformed));
        assert!(matches!(t.find("/todos/%2", &Method::GET), RouteMatch::Malformed)); // truncated
        assert!(matches!(t.find("/todos/%FF", &Method::GET), RouteMatch::Malformed)); // invalid UTF-8
    }
```

End-to-end (append to hardening.rs):

```rust
#[tokio::test]
async fn malformed_path_encoding_is_400_jc0400() {
    use jerrycan_core::Path;
    async fn show(Path(id): Path<String>) -> String { id }
    let t = App::new().route("/items/{id}", get(show)).into_test();

    assert_eq!(t.get("/items/ok%20name").await.text(), "ok name");
    let res = t.get("/items/%zz").await;
    assert_eq!(res.status(), jerrycan_core::http::StatusCode::BAD_REQUEST);
    assert!(res.text().contains("JC0400"));
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL (`RouteMatch::Malformed` missing).

- [ ] **Step 3: Implement**

(a) router.rs — the decoder + enum variant:

```rust
/// Decode %XX sequences in ONE path segment. `None` = malformed (bad hex,
/// truncated escape, or non-UTF-8 result) — the caller answers 400.
/// Runs after '/'-splitting, so an encoded slash cannot create segments.
fn decode_segment(seg: &str) -> Option<String> {
    if !seg.contains('%') {
        return Some(seg.to_string());
    }
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = seg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // `get` returns None on truncated escapes; `hex` on bad digits.
            let high = hex(*bytes.get(i + 1)?)?;
            let low = hex(*bytes.get(i + 2)?)?;
            out.push(high * 16 + low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
```

(b) `RouteMatch` gains a variant:

```rust
pub(crate) enum RouteMatch<'a> {
    Found { endpoint: &'a Endpoint, params: Vec<(String, String)> },
    MethodMissing,
    Malformed,
    NotFound,
}
```

(c) `find` / `find_node`: decode each segment up front; bail Malformed on failure:

```rust
    pub(crate) fn find<'a>(&'a self, path: &str, method: &Method) -> RouteMatch<'a> {
        let mut segs: Vec<String> = Vec::new();
        for raw in segments(path) {
            match decode_segment(raw) {
                Some(decoded) => segs.push(decoded),
                None => return RouteMatch::Malformed,
            }
        }
        let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
        let mut params: Vec<(String, String)> = Vec::new();
        match find_node(&self.root, &segs, &mut params) {
            Some(node) => {
                let ep = node.endpoint.as_ref().expect("find_node only returns endpoint nodes");
                if ep.methods.contains_key(method) {
                    RouteMatch::Found { endpoint: ep, params }
                } else {
                    RouteMatch::MethodMissing
                }
            }
            None => RouteMatch::NotFound,
        }
    }
```

(`find_node` is unchanged — it already works over `&[&str]`.)

(d) app.rs `dispatch_inner` match gains:

```rust
            RouteMatch::Malformed => {
                Error::bad_request("malformed percent-encoding in path").into_response()
            }
```

- [ ] **Step 4: Run to verify pass** — router + hardening + full workspace green (existing router tests unaffected: undecoded paths contain no `%`). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/router.rs crates/jerrycan-core/src/app.rs crates/jerrycan-core/tests/hardening.rs
git commit -m "Decode percent-encoded path segments and reject malformed encodings with 400"
```

---

### Task 8: Multi-param `Path` tuples (2 and 3) + the platform ripple

The docs promised "multi-param Path arrives in Phase 1". Deliver: `Path<(A, B)>` / `Path<(A, B, C)>` in core, validator limit 1 → 3, genroute mapping, docs update.

**Files:**
- Modify: `crates/jerrycan-core/src/extract.rs`
- Modify: `crates/jerrycan/src/platform/questions.rs`
- Modify: `crates/jerrycan/src/platform/genroute.rs`
- Modify: `docs/ai/03-extractors.md`
- Modify: `crates/jerrycan-core/tests/hardening.rs`

- [ ] **Step 1: Write the failing tests**

Append to hardening.rs:

```rust
#[tokio::test]
async fn multi_param_paths_extract_in_route_order() {
    use jerrycan_core::Path;
    async fn pair(Path((a, b)): Path<(i64, String)>) -> String {
        format!("{a}:{b}")
    }
    async fn triple(Path((a, b, c)): Path<(i64, i64, i64)>) -> String {
        format!("{}", a + b + c)
    }
    let t = App::new()
        .route("/pair/{a}/{b}", get(pair))
        .route("/sum/{x}/{y}/{z}", get(triple))
        .into_test();

    assert_eq!(t.get("/pair/7/seven").await.text(), "7:seven");
    assert_eq!(t.get("/sum/1/2/3").await.text(), "6");

    let res = t.get("/pair/notanumber/x").await;
    assert_eq!(res.status(), jerrycan_core::http::StatusCode::BAD_REQUEST);
}
```

questions.rs: UPDATE the existing `v0_limits_one_path_param_and_validates_mount_prefix` test — the two-param case is now legal; four params are not:

```rust
    #[test]
    fn paths_allow_up_to_three_params_and_validate_mount_prefix() {
        // Two params: now legal (multi-param Path landed in core).
        let d = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{id}/tags/{tag}\""));
        assert!(
            !validate(&d).iter().any(|q| q.question.contains("path parameter")),
            "two params must be accepted now"
        );
        // Four params: rejected.
        let d4 = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{a}/{b}/{c}/{d}\""));
        assert!(validate(&d4).iter().any(|q| q.question.contains("three path parameters")));

        let d2 = design(&MINIMAL.replace("\"name\": \"comments\",", "\"name\": \"comments\", \"mount\": \"comments\","));
        assert!(validate(&d2).iter().any(|q| q.id.contains("/mount") && q.question.contains("start with '/'")));
    }
```

genroute.rs: extend `handler_signatures_follow_the_mapping_rules`-adjacent coverage with a new unit test:

```rust
    #[test]
    fn multi_param_endpoints_map_to_path_tuples() {
        let mut m = todos();
        m.endpoints.push(Endpoint {
            operation_id: "move_todo".into(),
            method: HttpMethod::POST,
            path: "/{id}/position/{slot}".into(),
            auth_required: false,
            required_roles: vec![],
            request_body: None,
            success: Success { status: 204, entity: None, list: false },
            errors: vec![],
        });
        let h = handlers_rs(&m);
        assert!(
            h.contains("pub(crate) async fn move_todo(_repo: Dep<TodoRepo>, Path((_id, _slot)): Path<(i64, i64)>) -> Result<NoContent>"),
            "{h}"
        );
    }
```

- [ ] **Step 2: Run to verify failure** — compile FAIL / assertion failures across all three crates' tests.

- [ ] **Step 3: Implement**

(a) extract.rs — tuple impls after the existing single-param impl:

```rust
impl<A, B> FromRequest for Path<(A, B)>
where
    A: FromStr + Send,
    B: FromStr + Send,
    A::Err: std::fmt::Display,
    B::Err: std::fmt::Display,
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let [a, b] = take_params::<2>(ctx)?;
        Ok(Path((parse_param(&a.0, &a.1)?, parse_param(&b.0, &b.1)?)))
    }
}

impl<A, B, C> FromRequest for Path<(A, B, C)>
where
    A: FromStr + Send,
    B: FromStr + Send,
    C: FromStr + Send,
    A::Err: std::fmt::Display,
    B::Err: std::fmt::Display,
    C::Err: std::fmt::Display,
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let [a, b, c] = take_params::<3>(ctx)?;
        Ok(Path((
            parse_param(&a.0, &a.1)?,
            parse_param(&b.0, &b.1)?,
            parse_param(&c.0, &c.1)?,
        )))
    }
}

/// First N captured params, cloned in route order. Fewer than N is a routing
/// bug (the route declared fewer `{params}` than the handler expects) — 500.
fn take_params<const N: usize>(ctx: &RequestCtx) -> Result<[(String, String); N]> {
    if ctx.params.len() < N {
        return Err(Error::internal(format!(
            "route captures {} path parameter(s) but the handler expects {N}",
            ctx.params.len()
        )));
    }
    Ok(std::array::from_fn(|i| ctx.params[i].clone()))
}

fn parse_param<T: FromStr>(name: &str, raw: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    raw.parse::<T>()
        .map_err(|e| Error::bad_request(format!("invalid path parameter `{name}`: {e}")))
}
```

Refactor the EXISTING single-param `Path<T>` impl to reuse `parse_param` (same messages, less duplication):

```rust
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
        parse_param(name, raw).map(Path)
    }
}
```

(If the blanket `Path<T>` impl and the tuple impls conflict — tuples also satisfy `T: FromStr`? They do NOT (tuples don't implement FromStr), so there is no overlap; if rustc disagrees, report BLOCKED with the error.)

(b) questions.rs — the limit check becomes:

```rust
        let param_count = ep.path.matches('{').count();
        if param_count > 3 {
            qs.push(q(format!("{eptr}/path"), format!("Path `{}` has {param_count} parameters — at most three path parameters per endpoint are supported. Split the route or use a subroute.", ep.path)));
        }
```

(c) genroute.rs — `path_param` generalizes to all params; `handler_params` maps 1 → single, 2-3 → tuple:

```rust
fn path_params(ep: &Endpoint) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = ep.path.as_str();
    while let Some(start) = rest.find('{') {
        let Some(end_rel) = rest[start..].find('}') else { break };
        out.push(rest[start + 1..start + end_rel].to_string());
        rest = &rest[start + end_rel + 1..];
    }
    out
}
```

In `handler_params`, replace the single-param block with:

```rust
    let params_in_path = path_params(ep);
    match params_in_path.len() {
        0 => {}
        1 => params.push(format!("Path(_{}): Path<i64>", params_in_path[0])),
        n => {
            let names: Vec<String> = params_in_path.iter().map(|p| format!("_{p}")).collect();
            let types = vec!["i64"; n].join(", ");
            params.push(format!("Path(({})): Path<({})>", names.join(", "), types));
        }
    }
```

Delete the old `path_param` fn (now unused).

(d) docs/ai/03-extractors.md — REPLACE the second Anti-patterns bullet (the one beginning "One `Path<T>` per route in v0…") with:

```markdown
- Up to three `{params}` per route: one → `Path<T>`, two/three → tuple form
  `Path<(A, B)>` in route order. More than three params is a design smell —
  split the route or use a subroute.
```

And append to the Variations section:

````markdown
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
````

- [ ] **Step 4: Run to verify pass** — hardening, jerrycan unit tests (questions/genroute), doc-tests (`cargo test --doc -p jerrycan` — one NEW doc-test, expect 27), full workspace. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/extract.rs crates/jerrycan-core/tests/hardening.rs crates/jerrycan/src/platform/questions.rs crates/jerrycan/src/platform/genroute.rs docs/ai/03-extractors.md
git commit -m "Add multi-param Path tuples with validator, generator, and docs support"
```

---

### Task 9: Span-preserving `#[jerrycan::main]`

The current macro stringifies the whole item (`format!` → re-parse), collapsing every user span to 1:1 — so a type error inside `main` points at the attribute, not the offending line. Fix: parse ONLY the static attribute tokens; pass the user's item TokenStream through untouched.

**Files:**
- Modify: `crates/jerrycan-macros/src/lib.rs`

- [ ] **Step 1: Implement (3 lines of substance; behavior is pinned by the existing facade test)**

Replace the `main` fn body:

```rust
/// `#[jerrycan::main]` — boots the async runtime around `async fn main`.
/// Delegates to `#[tokio::main]`; the app must (and generated apps do) depend
/// on tokio directly. The user's tokens pass through UNCHANGED, preserving
/// their spans so compiler diagnostics point at the user's code, not at this
/// attribute.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut out: TokenStream = "#[::tokio::main]"
        .parse()
        .expect("static attribute tokens always parse");
    out.extend(item); // original tokens, original spans
    out
}
```

- [ ] **Step 2: Verify compile-success behavior is unchanged**

Run: `cargo test -p jerrycan --test facade` → PASS (the `#[jerrycan::main]` + cast test).

- [ ] **Step 3: Verify the span improvement MANUALLY and record it**

```bash
cd $(mktemp -d) && cargo init --name spanprobe
# point it at the local workspace:
cat >> Cargo.toml <<'EOF'
jerrycan = { path = "REPO/crates/jerrycan", default-features = false }
jerrycan-macros = { path = "REPO/crates/jerrycan-macros" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
EOF
cat > src/main.rs <<'EOF'
#[jerrycan_macros::main]
async fn main() {
    let _x: i32 = "not a number";
}
EOF
cargo build 2>&1 | grep "main.rs"
```

(Replace REPO with the absolute repo path.) Expected: the E0308 error points at `src/main.rs:3` (the bad line), NOT `1:1`. Record the observed line numbers in your report, then delete the temp dir.

- [ ] **Step 4: Gates + commit**

Full gate green (the macro change is exercised by every facade doc-test and the conformance apps).

```bash
git add crates/jerrycan-macros/src/lib.rs
git commit -m "Preserve user token spans in jerrycan::main expansion"
```

---
### Task 10: Workspace dep-version hygiene

The facade's internal deps carry literal `version = "0.0.0"` strings that will silently NOT track the 0.1.0 workspace bump. Centralize them.

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/jerrycan/Cargo.toml`

- [ ] **Step 1: Centralize**

Root `Cargo.toml`, in `[workspace.dependencies]`, add:

```toml
jerrycan-core = { path = "crates/jerrycan-core", version = "0.0.0" }
jerrycan-macros = { path = "crates/jerrycan-macros", version = "0.0.0" }
```

`crates/jerrycan/Cargo.toml` `[dependencies]` becomes:

```toml
jerrycan-core.workspace = true
jerrycan-macros.workspace = true
```

(One place now controls the internal version pins at the 0.1.0 cut.)

- [ ] **Step 2: Verify** — `cargo check --workspace` + full gate green (pure manifest refactor; lockfile may update).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jerrycan/Cargo.toml
git commit -m "Centralize internal crate versions in workspace dependencies"
```

---

### Task 11: Generator UX — warn on route reduction, hint on name mismatch

**Files:**
- Modify: `crates/jerrycan/src/platform/mcp_dispatch.rs`
- Modify: `crates/jerrycan/src/main.rs` (cmd_generate_route)
- Modify: `crates/jerrycan/tests/mcp.rs`

- [ ] **Step 1: Write the failing tests (append to tests/mcp.rs)**

```rust
#[test]
fn partial_slice_replacement_warns_about_dropped_routes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let app_dir = tmp.path().join("todo-api");
    let (err, _) = c.call_tool("jerrycan_scaffold", serde_json::json!({
        "design_path": tmp.path().join("design.json").to_str().unwrap(),
        "directory": app_dir.to_str().unwrap(),
    }));
    assert!(!err);

    // Replace todos with a ONE-endpoint slice: routes drop 8 -> 3 (comments subroute included).
    let (err, payload) = c.call_tool("jerrycan_generate", serde_json::json!({
        "kind": "route",
        "path": "todos",
        "directory": app_dir.to_str().unwrap(),
        "design_slice": { "name": "todos", "endpoints": [
            { "operation_id": "list_todos", "method": "GET", "path": "/", "success": { "status": 200 } }
        ]},
    }));
    assert!(!err, "{payload}");
    let next = payload["next_step"].as_str().unwrap();
    assert!(next.contains("warning") && next.contains("route count"), "{next}");
    c.shutdown();
}

#[test]
fn slice_name_path_mismatch_gets_a_pointed_hint() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let app_dir = tmp.path().join("todo-api");
    let (err, _) = c.call_tool("jerrycan_scaffold", serde_json::json!({
        "design_path": tmp.path().join("design.json").to_str().unwrap(),
        "directory": app_dir.to_str().unwrap(),
    }));
    assert!(!err);

    let (err, payload) = c.call_tool("jerrycan_generate", serde_json::json!({
        "kind": "route",
        "path": "widgets",
        "directory": app_dir.to_str().unwrap(),
        "design_slice": { "name": "gadgets", "endpoints": [
            { "operation_id": "list_gadgets", "method": "GET", "path": "/", "success": { "status": 200 } }
        ]},
    }));
    assert!(err);
    let msg = payload["error"].as_str().unwrap();
    assert!(msg.contains("gadgets") && msg.contains("widgets"), "must name both sides: {msg}");
    c.shutdown();
}
```

- [ ] **Step 2: Run to verify failure** — both new tests FAIL.

- [ ] **Step 3: Implement**

(a) mcp_dispatch.rs, "route" | "subroute" arm — right after parsing the slice and BEFORE the merge, add the mismatch guard (route kind only):

```rust
                        if kind == "route" && module.name != path {
                            return err_payload(format!(
                                "design_slice.name `{}` does not match path `{path}` — set path to the module the slice replaces (slices replace the WHOLE module)",
                                module.name
                            ));
                        }
```

(b) Route-reduction warning — capture the count before mutating, compare after:

```rust
                    let routes_before = genroute::route_map(&design).len();
                    // ... existing slice merge + validate + write ...
                    let routes_after = genroute::route_map(&design).len();
                    let mut next_step = format!(
                        "implement crates/routes/{top_name}/src/handlers.rs, then jerrycan_check"
                    );
                    if routes_after < routes_before {
                        next_step.push_str(&format!(
                            " — warning: route count dropped {routes_before} → {routes_after}; a partial design_slice REPLACES the whole module (stale agent files are not deleted)"
                        ));
                    }
```

(use `next_step` in the success json).

(c) main.rs `cmd_generate_route` — same warning for the CLI twin (design.json was edited by the agent BEFORE running the command, so compare against the route table derivable from DISK BEFORE regeneration is impossible there; instead compare against the EXISTING generated crate: count `.route(`/`.mount(` lines? NO — keep it honest and simple: the CLI path regenerates from the design as-is and has no "before" to compare; add the static reminder instead). Append to the CLI's next_step string:

```rust
        "next_step": format!("implement crates/routes/{top}/src/handlers.rs, then jerrycan check --module {top} — note: regeneration mirrors design.json exactly; routes removed there are removed here (stale agent files are not deleted)"),
```

- [ ] **Step 4: Run to verify pass** — mcp tests green; cli tests green (no assertion pins the old next_step text — verify, and if one does, update it to match). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/mcp_dispatch.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/mcp.rs
git commit -m "Warn on route reduction and hint on slice name mismatch in generate"
```

---

### Task 12: Docs, backlog, README sweep + Phase 1b exit gate

**Files:**
- Modify: `docs/ai/01-app.md`, `docs/ai/05-errors.md`
- Modify: `docs/phase1-backlog.md`
- Modify: `README.md`

- [ ] **Step 1: Document the secure defaults (01-app.md — append to Variations)**

````markdown
Secure defaults are ON for every response — security headers
(`x-content-type-options`, `x-frame-options`, `referrer-policy`,
`content-security-policy`, `cache-control: no-store`), a 30s handler timeout
(`503 JC0503`), a 30s body-read timeout, a 1 MiB body cap, and graceful
shutdown on Ctrl-C. Opting out is explicit:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let t = App::new().route("/", get(|| async { "ok" })).into_test();
assert_eq!(t.get("/").await.headers()["x-content-type-options"], "nosniff");

let bare = App::new()
    .route("/", get(|| async { "ok" }))
    .security_headers(false)                                  // explicit opt-out
    .handler_timeout(std::time::Duration::from_secs(120))     // explicit re-budget
    .into_test();
assert!(bare.get("/").await.headers().get("x-frame-options").is_none());
# }); }
```
````

- [ ] **Step 2: Extend the 05-errors code table** — add rows (keep table formatting):

```markdown
| JC0408 | 408 | Request body wasn't received within the read budget (default 30s) |
| JC0503 | 503 | Handler exceeded its time budget (default 30s) |
```

And extend the JC0400 row's "Produced when" to: `Bad path param / query string / malformed percent-encoding in path`.

- [ ] **Step 3: Backlog + README**

`docs/phase1-backlog.md` — remove every line item this plan completed (accept loop, panic→500, graceful shutdown/timeouts/security headers/percent-decoding, macro spans, facade version literals, multi-param Path, the two generator-UX bullets). What REMAINS (verbatim, reorganized under clear headings):

```markdown
# Phase backlog

## Phase 4 (per roadmap)

- Router + percent-decoder fuzzing (cargo-fuzz; roadmap Phase 4 owns fuzzing)

## Contract v1 candidates (deliberately deferred from v0)

- design-schema: middleware (module- and app-scoped) as first-class design objects; jerrycan_generate kind "middleware" returns then too
- design-schema: structured rate-limit config (v0: rate limits ride as opaque dependency names)
- jerrycan_check diagnostics: span (line+column ranges) — macro spans are preserved as of Phase 1b; wiring spans through diagnostics remains
- design-schema: path parameter types (v0 generates i64; string ids need a type field on params)

## Accepted v0 limitations

- `jerrycan dev`: directed SIGTERM orphans the cargo/app child (Ctrl-C is fine). Process-group handling needs libc/unsafe — conflicts with forbid(unsafe_code); revisit if it bites.
- write_subroutes does not prune subroute directories removed from the design; generate warns on route reduction instead (stale agent-owned files are never deleted by the tool).
```

`README.md` — Phase 1 roadmap row becomes `| **1 — Core loop** | \`jerrycan\` CLI (new/generate/dev/check) + MCP server | ✅ complete (incl. 1b hardening) |`.

- [ ] **Step 4: Run the Phase 1b exit gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p jerrycan --test conformance -- --include-ignored
cargo test -p jerrycan --test genroute_compile -- --include-ignored
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

ALL green (the YAML check is the Phase-1 lesson — run it even though ci.yml is untouched here).

- [ ] **Step 5: Commit**

```bash
git add docs/ai docs/phase1-backlog.md README.md
git commit -m "Document secure defaults and clear the hardening backlog"
```

---

## Execution notes

- **Order:** 1 → 12 strictly. Tasks 4 and 5 edit the OLD serve_with; Task 6 then restructures it into `serve_with_shutdown` carrying their logic forward — their tests are the safety net for the move.
- **Heavy tests:** run the conformance suite after Task 6 (serve path changed) and at the exit gate. Generated-app responses now carry security headers; conformance assertions are status-line/body-tail based and must stay green — investigate before touching any assertion.
- **Pre-solved traps:** `App` loses `#[derive(Default)]` for an explicit `Default` impl (defaults are ON, bool::default() is false); tuple `Path` impls don't overlap the single-param impl (tuples aren't FromStr); `decode_segment` runs AFTER '/'-splitting by construction; MCP responses stay single-line.
- **Out of scope (tracked):** fuzzing (Phase 4 roadmap row), `dev` SIGTERM process-grouping (accepted v0 limitation — unsafe/libc conflict), subroute pruning (warn-instead — accepted), span-through-diagnostics (contract v1).
