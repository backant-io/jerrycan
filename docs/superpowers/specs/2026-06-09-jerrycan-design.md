# jerrycan — Design Specification

**Date:** 2026-06-09
**Status:** Approved (design phase)
**Repo:** `~/github/frask` (directory rename to `jerrycan` pending; contains reference clones of `flask/` and `werkzeug/`)

---

## 1. Vision

**jerrycan** is an AI-native backend platform written in Rust, with two inseparable halves:

1. **The framework** — a ground-up rewrite of the Flask/Werkzeug *concept space* (not the code) in Rust: leanest possible core, trait-based extensions, async-only, secure by default. Goal: replace Flask, FastAPI, and similar frameworks as the standard for AI-generated backends — "the last backend framework."
2. **The platform** — a single `jerrycan` binary that is both a CLI and an MCP server, through which AI agents design, generate (TDD-first), verify, and package complete deployable backends. **Humans do not write the code; agents do**, guided by AI-native documentation and MCP tools.

### Non-goals
- No frontend serving, no HTML templating, no static file hosting. Backends only.
- No runtime plugin loading (dylib/WASM) in v1.
- No deployment *execution* (no kubectl apply, no SSH) in v1 — jerrycan produces proven-deployable artifacts; pushing them is the agent's/user's tooling.
- No runtime capability sandboxing (seccomp/landlock) in v1 — deferred to v2.

### In scope despite "lean"
- Streaming responses (SSE/chunked) in core — AI-era backends proxy LLMs.

---

## 2. Decision log (settled during brainstorming)

| Decision | Choice |
|---|---|
| Core identity | Full ground-up framework (own developer-facing surface, no axum/actix) |
| Dependency floor | tokio + hyper (+ rustls, sqlx, serde, tracing). Like Werkzeug atop Python's socket lib |
| App scope v1 | Pure backend APIs; no frontend serving; leanest core, maximally extensible |
| Extension model | Trait-based extensions in separate crates; compile-time composition |
| Platform scope | Design cycle → TDD generate → verify → package. Workflow enforced by MCP |
| Safety bar | Secure-by-default framework + verified generation pipeline + hardened artifacts |
| v1 extensions | jerrycan-db, jerrycan-auth, jerrycan-validate (+OpenAPI), jerrycan-observe |
| Signature feature | FastAPI-grade dependency injection: async, nested, per-request caching, test overrides — in core |
| Build strategy | Docs & contracts first (docs are the executable spec), with compile-checked examples and a Phase-0 API spike to stay true to Rust ownership |
| License | MIT OR Apache-2.0 dual. Concepts ported from Flask/Werkzeug (BSD-3), no code copied |

---

## 3. Workspace layout

One Cargo monorepo:

```
jerrycan/
├── crates/
│   ├── jerrycan-core       # routing, http types, extractors, DI, middleware, errors, config, test client
│   ├── jerrycan-macros     # #[jerrycan::main], derive sugar — thin and optional
│   ├── jerrycan-db         # sqlx (SQLite + Postgres), migrations, Db dependency
│   ├── jerrycan-auth       # argon2 hashing, signed+encrypted sessions, JWT, CurrentUser, guards
│   ├── jerrycan-validate   # derive(Validate), structured 422s, OpenAPI 3.1 generation
│   ├── jerrycan-observe    # tracing JSON logs, request IDs, /healthz, /metrics (Prometheus)
│   └── jerrycan            # the binary: CLI + `jerrycan mcp` (stdio MCP server)
├── docs/                # AI-native docs — written FIRST; every example is a doc-test
├── templates/           # scaffolding templates used by the generator
└── conformance/         # golden generated apps, built + tested + booted in CI forever
```

`#![forbid(unsafe_code)]` in every crate. Dependencies minimal, pinned, audited.

---

## 4. Core framework (`jerrycan-core`)

### 4.1 The shape of generated code

```rust
use jerrycan::prelude::*;

#[jerrycan::main]
async fn main() -> Result<()> {
    App::new()
        .provide(Db::connect_from_env())          // register a dependency
        .route("/todos", get(list).post(create))
        .serve()                                   // addr/limits/timeouts from layered config
        .await
}

async fn list(db: Dep<Db>) -> Result<Json<Vec<Todo>>> {
    Ok(Json(db.fetch_all("select * from todos").await?))
}

async fn create(db: Dep<Db>, todo: Valid<Json<NewTodo>>) -> Result<Created<Todo>> {
    // ...
}
```

- **Handlers** are plain async fns. Parameters are **extractors** (`Path<T>`, `Query<T>`, `Json<T>`, `Headers`, `Dep<T>`, `CurrentUser`). Return types implement `IntoResponse`. Everything a handler needs is visible in its signature — ideal for AI generation and review.
- **Routing**: trie-based router; typed path params; ambiguous/conflicting routes detected at startup — fail loud before serving.
- **Request/Response**: jerrycan-owned types wrapping hyper's, with Werkzeug-inspired ergonomics (typed headers, content negotiation, cookies).
- **Streaming**: SSE and chunked streaming responses are first-class core response types.
- **Errors**: one `jerrycan::Error` with HTTP status mapping. Production responses never leak internals (no stack traces, no paths, no SQL); full detail goes to structured logs. Dev mode shows everything. Every framework error has a stable error code that maps to a docs anchor.
- **Config**: layered — built-in defaults < `jerrycan.toml` < `JERRYCAN_*` env vars — deserialized into typed structs via serde.
- **Middleware**: own small trait (`async fn handle(&self, req, next) -> Response`); composable; ordering explicit.
- **Test client**: in-memory `TestApp` (no sockets) like Flask's test client, with dependency overrides (see 4.2).

### 4.2 Dependency injection — the signature feature

FastAPI-grade dependencies, done the Rust way. This is the hardest engineering in the project and is validated by the Phase-0 spike before docs freeze.

```rust
// A dependency is (usually) an async fn; dependencies can depend on dependencies.
async fn current_user(session: Dep<Session>, db: Dep<Db>) -> Result<User> { /* ... */ }
async fn admin_only(user: Dep<User>) -> Result<Admin> { /* ... */ }   // guards are just deps

async fn delete_todo(_: Dep<Admin>, db: Dep<Db>, id: Path<i64>) -> Result<NoContent> { /* ... */ }
```

Requirements:
- **Async** dependencies (may await I/O).
- **Nested** dependencies, resolved as a DAG per request.
- **Scopes**: singleton (app lifetime) and per-request, with per-request caching (a dep resolved once per request no matter how many handlers/deps consume it).
- **Overrides for testing**:

```rust
let app = test_app()
    .override_dep::<Db>(fake_db())
    .override_dep::<User>(test_user());
let res = app.get("/todos").await;   // in-memory, no network, no real DB
```

- Auth, db sessions, permissions, rate limiting are all expressed as reusable dependencies.

### 4.3 Secure by default (always on; opting out requires explicit code)

- Request body size limit (default 1 MiB), header count/size limits, request read timeout, handler timeout.
- Security headers on every response.
- Constant-time comparison for all secret material.
- Session cookies signed + encrypted (AEAD), `Secure`/`HttpOnly`/`SameSite` by default.
- Zero internal detail in production error responses.

---

## 5. Extension system

- Core defines small, stable traits: `Extension`, `Middleware`, `Dependency`, `Store`.
- A capability is a separate crate registered in one line: `App::new().extend(Auth::default())`.
- The platform wires extensions mechanically: `jerrycan add auth` edits Cargo.toml + registration code.
- Unused capability = absent from the binary (compile-time composition, zero cost).
- Third parties publish `jerrycan-*` crates against the same traits — the "grows forever" seam. Core stays frozen-small; ambition is absorbed by extensions.

### v1 extension crates
| Crate | Contents |
|---|---|
| `jerrycan-db` | Async SQL (SQLite + Postgres via sqlx), migrations + `jerrycan db migrate`, `Db` dependency, compile-time-checked queries where feasible |
| `jerrycan-auth` | argon2 password hashing, signed+encrypted cookie sessions, JWT/bearer, `CurrentUser` extractor, role/permission guards as dependencies |
| `jerrycan-validate` | `derive(Validate)` declarative validation, automatic structured 422 responses, OpenAPI 3.1 generated from routes + schemas |
| `jerrycan-observe` | tracing-based structured JSON logs, request IDs, `/healthz`, `/metrics` (Prometheus) |

---

## 6. The platform: one `jerrycan` binary (CLI + MCP)

### 6.1 CLI

| Command | Purpose |
|---|---|
| `jerrycan new <name> --design design.json` | Scaffold a project from a validated design |
| `jerrycan add <extension>` | Wire an extension (Cargo.toml + registration code) |
| `jerrycan dev` | Run with auto-reload |
| `jerrycan check` | **Verification gate**: build + clippy (deny warnings) + cargo-audit + cargo-deny + tests + jerrycan lints |
| `jerrycan test` | Run the app's test suite |
| `jerrycan package --docker\|--binary\|--k8s\|--systemd` | Hardened artifacts + SBOM |
| `jerrycan docs <topic>` | AI-native docs, offline, in terminal |
| `jerrycan mcp` | Serve MCP over stdio |

### 6.2 MCP tools — the enforced workflow

The MCP enforces **design-first → TDD → verify → package**:

1. **`jerrycan_design`** — agent submits requirements; tool returns a structured design template that *forces specificity*: entities + fields, endpoints + methods + status codes, auth model, dependencies, error cases. Incomplete designs come back with pointed questions, not code. Output: validated `design.json`.
2. **`jerrycan_scaffold`** — requires a validated design; creates the project.
3. **`jerrycan_gen_tests`** — generates the **failing test suite** from the design (one test per endpoint/error case) *before any handler exists*. The design becomes executable acceptance criteria.
4. *(agent implements handlers, guided by docs tools)*
5. **`jerrycan_check`** — runs the verification gate; returns **structured diagnostics** (JSON: error code, file, span, suggested fix, doc link). Compiler errors become teaching moments.
6. **`jerrycan_package`** — only succeeds after check is green; emits artifacts.

Always available:
- **`jerrycan_docs_search`** / **`jerrycan_docs_get`** — doc lookup.
- **MCP resources**: current app's route map, design spec, migration state — so a resuming agent reads state instead of re-deriving it ("follow-up work must be easy").
- Every tool response includes a `next_step` hint — golden path without hard-blocking.

---

## 7. AI-native documentation — written first, executable always

The docs ARE the spec (docs-first build strategy), kept honest by compilation:

- **Deterministic page shape**, learned once by any agent: Purpose → Signature → Minimal example → Variations → Errors you'll hit → Anti-patterns.
- **Every code example is a doc-test.** CI compiles and runs all of them. Docs cannot drift from the implementation — this is what makes docs-first survivable in Rust.
- **Error-driven docs**: every jerrycan error code maps to a docs anchor; `jerrycan_check` diagnostics deep-link to the fix.
- One source, three outputs: embedded in the binary (`jerrycan docs`), MCP resources, and `llms.txt` / `llms-full.txt` on the web.
- CI gate: every public API item must have a doc-tested example or the build fails.

---

## 8. Safety pipeline (three layers)

1. **Language & framework**: `forbid(unsafe_code)`; minimal audited deps; secure defaults of §4.3.
2. **Verified generation**: nothing is "done" until `jerrycan check` is green — build, clippy deny-warnings, cargo-audit (known CVEs), cargo-deny (license/source policy), full test suite, plus jerrycan-specific lints (raw SQL string concatenation, mutating route without auth guard, secrets committed in source).
3. **Hardened artifacts**: static musl binary; scratch/distroless image, non-root, read-only fs, dropped capabilities; k8s manifests with securityContext + resource limits + NetworkPolicy; systemd unit with hardening directives; CycloneDX SBOM with every build.

v2: runtime capability sandboxing (app declares net/fs/env capabilities; seccomp/landlock enforcement).

---

## 9. Testing strategy — TDD from the ground up

- **The framework itself is built test-first**: doc-tests as the spec layer; unit + integration tests per crate.
- **Conformance suite**: `conformance/` holds golden generated apps; CI builds, tests, packages, and boots them on every commit. The framework can never break its own generated output.
- **Agent evals**: a harness where a real LLM drives the MCP end-to-end to build N reference apps; success rate is a tracked release metric.
- **Fuzzing** (cargo-fuzz) on jerrycan-owned parsing surfaces: router/path decoding, cookie/session decoding, config parsing.

---

## 10. Roadmap

| Phase | Deliverable | Exit criterion |
|---|---|---|
| **0 — Contracts** | Hard-API spike (DI signatures must compile as real Rust); then docs for core surface + MCP tool JSON contracts + CLI UX spec | All doc examples compile against stub crates |
| **1 — Core loop** | jerrycan-core + minimal CLI (new/dev/check) + minimal MCP (docs/design/scaffold/check) | An AI agent generates a working in-memory CRUD service via MCP only |
| **2 — Data & TDD** | DI maturity, jerrycan-db, jerrycan-validate + OpenAPI, jerrycan_gen_tests | Agent builds a Postgres-backed API test-first, all green |
| **3 — Production** | jerrycan-auth, jerrycan-observe, `jerrycan package` (all 4 targets), SBOM | Golden app deploys to Docker + k8s + bare server from one command |
| **4 — Hardening** | Fuzzing, agent evals, diagnostics polish, benchmarks | ≥90% agent-eval success; v0.1.0 public release |
| **v2+** | Sandboxing, WebSockets, jerrycan-queue, multi-service composition | — |

---

## 11. Risks & mitigations

| Risk | Mitigation |
|---|---|
| DI (async + nested + overrides) fights Rust's type system — make-or-break | Phase-0 spike before docs freeze; if a signature can't be elegant, the docs change first |
| Docs-first drift from implementable reality | Every doc example is a compiling doc-test from day one; docs that don't compile don't merge |
| "Replace FastAPI" scope creep | Every phase exits with something an agent can actually build with; core stays frozen-small; extensions absorb ambition |
| AI agents misuse the framework in ways docs didn't anticipate | Agent evals + error-driven docs loop: every eval failure becomes a docs/diagnostic improvement |

---

## 12. Open items (deliberately deferred to the implementation plan)

- Exact `design.json` schema for `jerrycan_design`.
- Choice of trie router implementation details (own vs. adapted matchit-style algorithm — must be ours per ground-up decision, fuzz-tested).
- MSRV and tokio/hyper version pins.
- Docs hosting (static site generator) — only the format and CI gates are specified here.
