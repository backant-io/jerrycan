<div align="center">

# jerrycan

**The AI-native Rust backend platform.**

Backends designed, generated, verified, and packaged by AI agents —
on a framework built from the ground up for exactly that.

[![CI](https://github.com/backant-io/jerrycan/actions/workflows/ci.yml/badge.svg)](https://github.com/backant-io/jerrycan/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/jerrycan.svg)](https://crates.io/crates/jerrycan)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

[jerrycan.cc](https://jerrycan.cc) · [AI-native docs](docs/ai) · [Platform contracts](docs/contracts) · [Design spec](docs/superpowers/specs/2026-06-09-jerrycan-design.md) · [Roadmap](#roadmap)

</div>

---

## What is jerrycan?

jerrycan is two inseparable halves:

1. **A backend framework** — a ground-up rewrite of the Flask/Werkzeug *concept space* in Rust: leanest possible core, async-only on tokio + hyper, trait-based extensions, secure by default.
2. **A generation platform** — one `jerrycan` binary that is both a CLI and an MCP server, through which AI agents **design → generate test-first → verify → package** complete, deployable backends.

**Humans don't write the code — agents do**, guided by documentation where every example is a compiling, *running* doc-test, and by machine-readable contracts for every tool.

> **Status: early development.** Phases 0 (core API + frozen contracts) and 1 (CLI + MCP core loop) are complete and fully tested. The crates on crates.io are `0.0.0` name reservations — the first usable release will be `0.1.0`. Don't build on it yet; watch it grow.

## A taste

```rust
use jerrycan::prelude::*;

// A route module — Flask's Blueprints, reborn with compiler-enforced boundaries.
pub fn module() -> Module {
    Module::new("todos")
        .route("/", get(list).post(create))
        .route("/{id}", get(show).delete(remove))
        .mount("/{id}/comments", comments::module()) // subroutes nest arbitrarily
        .provide(TodoRepo::new())                    // module-scoped dependency
}

async fn list(repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>> {
    Ok(Json(repo.all().await?))
}

#[jerrycan::main]
async fn main() -> Result<()> {
    App::new().mount("/todos", todos::module()).serve().await
}
```

Dependencies are jerrycan's signature feature — async, nested, memoized per request, and **guards are just dependencies**:

```rust
async fn current_user(session: Dep<Session>, db: Dep<Db>) -> Result<User> { /* ... */ }
async fn admin_only(user: Dep<User>) -> Result<Admin> { /* ... */ }

async fn remove(_: Dep<Admin>, repo: Dep<TodoRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    repo.delete(id).await?;
    Ok(NoContent)
}
```

Testing runs real requests in memory — no sockets, and **any dependency can be faked in one line**:

```rust
let t = app().into_test().override_dep(Db::fake());
assert_eq!(t.get("/todos/").await.status(), jerrycan::http::StatusCode::OK);
```

## Why jerrycan

| | |
|---|---|
| **Built for AI generation** | Everything a handler needs is visible in its signature. One fractal project shape, learned once. Docs follow a fixed, machine-friendly structure — and every example is executed in CI, so docs can never lie. |
| **Secure by default** | Body limits, strict input handling, no internals leaked in errors. Every framework error carries a stable `JC####` code that deep-links into the docs. `#![forbid(unsafe_code)]` everywhere. |
| **Fail loud** | Conflicting routes are build-time errors, before serving. Missing dependencies and cycles are coded errors, not mysteries. |
| **TDD as a workflow, not a virtue** | The MCP tools enforce design-first → failing acceptance tests → implement → verify → package. The design becomes executable acceptance criteria. |
| **Multi-agent ready** | Generated apps are crate-per-module workspaces: one agent owns one route crate, the compiler enforces the boundaries, shared files are generator-owned. Parallel agents merge without conflicts. |
| **Deploy anywhere** | `jerrycan package` (Phase 3) produces static binaries, hardened container images, k8s manifests, or systemd units — with an SBOM. |

## Architecture

```
crates/
├── jerrycan          # facade + the CLI/MCP binary — apps depend on this
├── jerrycan-core     # routing, extractors, DI, modules, middleware, errors, test client
├── jerrycan-macros   # #[jerrycan::main]
├── jerrycan-db       # SQL + migrations            (Phase 2)
├── jerrycan-auth     # sessions, JWT, guards       (Phase 3)
├── jerrycan-validate # validation + OpenAPI        (Phase 2)
└── jerrycan-observe  # logs, /healthz, /metrics    (Phase 3)
docs/
├── ai/               # the AI-native docs — every example is a CI-run doc-test
└── contracts/        # MCP tool schemas, design.json schema, CLI UX spec
```

## The agent workflow

```
jerrycan_design   → requirements become a validated design.json (pointed questions, not guesses)
jerrycan_scaffold → workspace shell + one route-module crate per design module
jerrycan_gen_tests→ failing acceptance tests, generated from the design
   (agent implements handlers, guided by the docs tools)
jerrycan_check    → build + clippy + audit + tests + jerrycan lints, machine-readable diagnostics
jerrycan_package  → hardened artifacts + SBOM, only when everything is green
```

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| **0 — Contracts** | Core API spike (DI, modules, routing, serving) + AI docs + MCP/CLI contracts | ✅ complete |
| **1 — Core loop** | `jerrycan` CLI (new/generate/dev/check) + MCP server | ✅ core loop (framework hardening → Phase 1b) |
| **2 — Data & TDD** | jerrycan-db, jerrycan-validate + OpenAPI, per-module test generation | next |
| **3 — Production** | jerrycan-auth, jerrycan-observe, `jerrycan package` (Docker/k8s/binary/systemd) | |
| **4 — Hardening** | Fuzzing, agent evals, diagnostics polish → v0.1.0 | |

The full plan lives in the [design spec](docs/superpowers/specs/2026-06-09-jerrycan-design.md); deferred items are tracked in the [phase 1 backlog](docs/phase1-backlog.md).

## Development

```bash
cargo test --workspace        # 124 tests, including every docs example as a doc-test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The project is built docs-first and test-first: documentation examples are the executable specification.

## Support

If jerrycan's direction resonates with you, you can fuel it:

<a href="https://buymeacoffee.com/sorcecoder"><img src="https://img.shields.io/badge/Buy%20me%20a%20coffee-sorcecoder-FFDD00?logo=buymeacoffee&logoColor=black" alt="Buy Me a Coffee"></a>

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
