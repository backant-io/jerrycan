<div align="center">

# jerrycan

**The AI-native Rust backend platform.**

Backends designed, generated, verified, and packaged by AI agents —
on a framework built from the ground up for exactly that.

[![CI](https://github.com/backant-io/jerrycan/actions/workflows/ci.yml/badge.svg)](https://github.com/backant-io/jerrycan/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/jerrycan.svg)](https://crates.io/crates/jerrycan)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

[jerrycan.cc](https://jerrycan.cc) · [AI-native docs](docs/ai) · [Platform contracts](docs/contracts) · [Design spec](docs/superpowers/specs/2026-06-09-jerrycan-design.md) · [Roadmap](#roadmap)

</div>

---

## What is jerrycan?

jerrycan is two inseparable halves:

1. **A backend framework** — a ground-up rewrite of the Flask/Werkzeug *concept space* in Rust: leanest possible core, async-only on tokio + hyper, trait-based extensions, secure by default.
2. **A generation platform** — one `jerrycan` binary that is both a CLI and an MCP server, through which AI agents **design → generate test-first → verify → package** complete, deployable backends.

**Humans don't write the code — agents do**, guided by documentation where every example is a compiling, *running* doc-test, and by machine-readable contracts for every tool.

> **Status: 0.1.0 released; 0.2.0 in development on `main`.** Phases 0 (core API + frozen contracts), 1 (CLI + MCP core loop), 2 (data & test-first generation), 3 (auth, observability, `jerrycan package`), and 4 (fuzzing, agent evals, diagnostics polish) are complete and fully tested. The crates are published at `0.1.0` on [crates.io](https://crates.io/crates/jerrycan). The full v2 cycle toward `0.2.0` has landed on `main` — the data foundation (relations, constraints, first-class tenancy, SeaORM), the protocol surface (multipart, raw-body webhooks, streaming), the middleware kit (CORS, rate limiting), jerrycan-jobs (Postgres + Redis stores, cron, retries), auth expansion (OAuth2, encrypted tokens, scoped API keys), and the v2.5 eval gate (the Kolli reference slice driven live over HTTP as a permanent release gate); see the [v2 design spec](docs/superpowers/specs/2026-06-11-jerrycan-v2-design.md). Early but real; expect rough edges as it grows.

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
| **Deploy anywhere** | `jerrycan package` produces static binaries, hardened container images, k8s manifests, or systemd units — with an SBOM. |

## Architecture

```
crates/
├── jerrycan          # facade + the CLI/MCP binary — apps depend on this
├── jerrycan-core     # routing, extractors, DI, modules, middleware, errors, test client
├── jerrycan-macros   # #[jerrycan::main]
├── jerrycan-db       # SQL + migrations
├── jerrycan-auth     # sessions, JWT, guards
├── jerrycan-validate # validation + OpenAPI
└── jerrycan-observe  # logs, /healthz, /metrics
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
| **1 — Core loop** | `jerrycan` CLI (new/generate/dev/check) + MCP server | ✅ complete (incl. 1b hardening) |
| **2 — Data & TDD** | jerrycan-db, jerrycan-validate + OpenAPI, per-module test generation | ✅ complete |
| **3 — Production** | jerrycan-auth, jerrycan-observe, `jerrycan package` (Docker/k8s/binary/systemd) | ✅ complete |
| **4 — Hardening** | Fuzzing, agent evals, diagnostics polish → v0.1.0 | ✅ complete |
| **v0.1.0** | First release — crates published on [crates.io](https://crates.io/crates/jerrycan) | 🚀 released |
| **v2.0 — Data foundation** | Contract v1 (relations + `on_delete`, unique/index, enums, json, **tenancy**, jobs shape), SeaORM data layer, `schema.json` contract + `jerrycan_schema` tool, generated isolation tests | ✅ complete |
| **v2.0b — Core readiness** | Dual-lane body + per-route limits, param-carrying mounts, task-scoped DI, extension lifecycle, mockable `Clock` | ✅ complete |
| **v2.1 — Protocol surface** | `Multipart` / `RawBody` (webhook signatures) / `StreamBody` extractors | ✅ complete |
| **v2.2 — Middleware kit** | CORS in core; rate limiting as an extension (`429 JC0429`) | ✅ complete |
| **v2.3 — jerrycan-jobs** | `JobStore` (Postgres / Redis), retries + dead-letter, named queues, cron, idempotency, `run_at` | ✅ complete (incl. v2.3b Redis Streams) |
| **v2.4 — Auth expansion** | OAuth2 client, encrypted token storage + key rotation, scoped API keys, mock IdP harness | ✅ complete |
| **v2.5 — Eval gate → v0.2.0** | Kolli reference slice rebuilt on jerrycan, served live, every v2 feature driven over real HTTP — wired as a permanent, un-skippable CI + publish gate | ✅ complete |

The original plan lives in the [v1 design spec](docs/superpowers/specs/2026-06-09-jerrycan-design.md); the v2 roadmap is in the [v2 design spec](docs/superpowers/specs/2026-06-11-jerrycan-v2-design.md); deferred items are tracked in the [backlog](docs/phase1-backlog.md).

## Install

> `0.1.0` is live on crates.io — these work today.

```bash
# In an app — the framework facade with the extensions you need:
cargo add jerrycan --features db,auth,validate,observe

# The CLI + MCP server (the generation platform):
cargo install jerrycan
```

## Agent eval

The headline metric: an opus agent, given **only** the published docs surface
(`jerrycan docs …` — no framework source, no test fixtures), scaffolds and
fully implements backends that build, pass `jerrycan check`, and serve real
CRUD over HTTP. The v2.5 north star — a **docs-only rebuild of the full Kolli
slice** — is **GREEN**: `jerrycan check` passes, all **37/37** generated
acceptance tests pass across 6 modules + 2 cron jobs, live HTTP round-trips
verified, and a negative control (unscoping a tenant query) correctly turns the
gate red. The five simpler reference apps stand at **5/5 (100%)**. See
[`conformance/eval/results.md`](conformance/eval/results.md) (floor 4/5,
target ≥ 90%).

The v2.5 release gate adds a **deterministic Kolli battery**
([`crates/jerrycan/tests/kolli_eval.rs`](crates/jerrycan/tests/kolli_eval.rs)):
it scaffolds the Kolli reference slice on jerrycan, gets `jerrycan check` green,
runs the generated acceptance suite (including cross-tenant isolation), serves
the app **live** and drives every v2 feature over real HTTP — register/login
(JWT sessions), live cross-tenant isolation, webhook signature verification
(200/400), multipart CSV import (202), scoped API keys (200/403/401), and OAuth
connect (302) + callback against an in-process mock IdP — plus both crons firing
under a controlled clock and `schema.json` data-structure questions answered from
the published `SchemaContract` alone. It runs in CI (the `--include-ignored`
heavy step) and is a fail-fast pre-publish block in `scripts/publish.sh`, so a
release can't ship if it's red. Cold-build time (the SeaORM compile-tax baseline)
is measured in CI by the kolli conformance test, which prints
`kolli-slice cold build: …` to the log.

## Development

```bash
cargo test --workspace --all-features   # CI runs this — every docs example is a doc-test
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo bench                             # criterion benches (routing, extraction)
cargo +nightly fuzz run <target>        # fuzz targets live in fuzz/ (outside the workspace)
```

The project is built docs-first and test-first: documentation examples are the executable specification.

## Support

If jerrycan's direction resonates with you, you can fuel it:

<a href="https://buymeacoffee.com/sorcecoder"><img src="https://img.shields.io/badge/Buy%20me%20a%20coffee-sorcecoder-FFDD00?logo=buymeacoffee&logoColor=black" alt="Buy Me a Coffee"></a>

## License

Licensed under the [MIT License](LICENSE-MIT).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be licensed under the MIT License, without any additional terms or conditions.
