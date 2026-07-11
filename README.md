<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/jerrycan-icon-dark.svg">
  <img src="docs/assets/jerrycan-icon.svg" alt="jerrycan icon" width="96">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/jerrycan-wordmark-dark.svg">
  <img src="docs/assets/jerrycan-wordmark.svg" alt="jerrycan" width="280">
</picture>

<br><br>

**Humanity's last backend. Built for AI agents.**

The AI-native Rust backend framework + generation platform.
Agents design, generate, verify and package complete backends. You never write the code.

[![CI](https://github.com/backant-io/jerrycan/actions/workflows/ci.yml/badge.svg)](https://github.com/backant-io/jerrycan/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/jerrycan.svg)](https://crates.io/crates/jerrycan)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
[![rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

[jerrycan.cc](https://jerrycan.cc) · [AI-native docs](docs/ai) · [Why jerrycan exists](https://jerrycan.cc/blog/humanitys-last-backend-framework) · [llms.txt](https://jerrycan.cc/llms.txt)

</div>

---

```console
$ jerrycan new --design bookmarks.json       # describe it once
  ✓ scaffolded a crate-per-module workspace

$ jerrycan gen-tests --module bookmarks      # the test suite is generated for you
  ✓ 8 acceptance tests written

  …an agent fills in ~12 lines of obvious handler glue…

$ jerrycan --json check                      # build · clippy · audit · tests · lints
  {"ok":true,"diagnostics":[]}               # all green: safe + tested, no internals leaked
```

> **0.2.0, published on [crates.io](https://crates.io/crates/jerrycan). Early but real.** Expect rough edges as it grows.

## Key features

* **Agents build it, you don't.** Describe the API once; jerrycan generates the workspace, a working data layer, and the tests. Handlers come out as a few lines of obvious glue.
* **Secure by default.** Secure response headers, body limits, strict input handling, **no internals leaked** in errors, `#![forbid(unsafe_code)]` everywhere, and stable `JC####` codes that deep-link into the docs.
* **Tested before it's "done".** jerrycan *generates* the acceptance suite test-first; `jerrycan check` won't go green until it passes. What the generator can't derive from the contract becomes an explicit `AGENT TODO` in the test file, so the gaps are named instead of silent.
* **Fail loud.** Conflicting routes are build-time errors *before* serving; missing dependencies and cycles are coded errors, not mysteries.
* **Multi-agent ready.** Generated apps are crate-per-module workspaces with compiler-enforced boundaries, so parallel agents merge without conflicts.
* **Deploy anywhere, deployed by the agent.** `jerrycan package` produces a static binary, a hardened container image, k8s manifests, or a systemd unit, with an SBOM. `jerrycan deploy render` writes a deploy kit the agent executes with an API key: design file to live URL, no human in the loop.
* **Docs that can't lie.** Every example in the docs is a doctest executed in CI.

## Quickstart

### For your agent (the intended path)

```bash
cargo install jerrycan          # the CLI + MCP server
```

Wire the MCP server into your agent, then ask for a backend in one prompt:

```bash
# Claude Code
claude mcp add jerrycan -- jerrycan mcp
```

```jsonc
// Cursor / any stdio MCP client
{ "mcpServers": { "jerrycan": { "command": "jerrycan", "args": ["mcp"] } } }
```

The agent drives the whole loop: design → scaffold → gen-tests → implement → check → package → deploy. Claude Code users also get the bundled [`jerrycan-backend` skill](.claude/skills/jerrycan-backend) that guides the process end to end. Point any other agent at [docs/ai](docs/ai) or [jerrycan.cc/llms.txt](https://jerrycan.cc/llms.txt); the docs are written to be sufficient on their own.

### For humans

```bash
# In your app: the framework, with the extensions you need
cargo add jerrycan --features db,auth,validate,observe
```

A route module is Flask's Blueprints, reborn with compiler-enforced boundaries. Everything a handler needs is visible in its signature, and **guards are just dependencies**:

```rust
use jerrycan::prelude::*;

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

async fn remove(_: Dep<Admin>, repo: Dep<TodoRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    repo.delete(id).await?;            // `Dep<Admin>` is the guard: a dependency that must resolve
    Ok(NoContent)
}
```

Testing runs real requests in memory, no sockets, and **any dependency can be faked in one line**:

```rust
let t = app().into_test().override_dep(Db::fake());
assert_eq!(t.get("/todos/").await.status(), jerrycan::http::StatusCode::OK);
```

## How it works

The agent drives one fixed loop; jerrycan does the generation and the gating:

```
jerrycan_design    → requirements become a validated design.json (pointed questions, not guesses)
jerrycan_scaffold  → a crate-per-module workspace, one route crate per module
jerrycan_gen_tests → failing acceptance tests, generated from the design
   (the agent implements the handler bodies, guided by the docs tools)
jerrycan_check     → build + clippy + audit + tests + jerrycan lints, machine-readable diagnostics
jerrycan_package   → hardened artifacts + SBOM, only when everything is green
jerrycan_deploy    → a deploy kit for the target platform (Render first), run by the agent
```

<details>
<summary><b>Project layout of the framework itself</b></summary>

```
crates/
├── jerrycan          # facade + the CLI/MCP binary, apps depend on this
├── jerrycan-core     # routing, extractors, DI, modules, middleware, errors, test client
├── jerrycan-macros   # #[jerrycan::main]
├── jerrycan-db       # data layer + migrations (SeaORM)
├── jerrycan-auth     # sessions, JWT, OAuth2, guards
├── jerrycan-validate  # validation + OpenAPI
├── jerrycan-observe   # logs, /healthz, /metrics
├── jerrycan-ratelimit # rate limiting (429 JC0429)
└── jerrycan-jobs      # background jobs, cron, retries (Postgres / Redis)
docs/
├── ai/               # the AI-native docs, every example is a CI-run doc-test
└── contracts/        # MCP tool schemas, design.json schema, CLI UX spec
```
</details>

## Does it actually work?

Yes, and it's *measured*, not asserted. A **docs-only** agent (given only `jerrycan docs`, no framework source, no fixtures) builds real backends that pass `jerrycan check` and serve real HTTP:

* **5/5** of the reference CRUD apps: green on the first run, zero doc gaps.
* The full **multi-tenant SaaS slice**: green across 6 modules + 2 background jobs, driven **live over HTTP**. Auth, per-tenant isolation, signed webhooks, CSV import, scoped API keys, OAuth. A **negative control** (breaking tenant scoping) correctly turns the gate **red**, so the green isn't hollow.

It's wired as an **un-skippable release gate** (CI + a fail-fast pre-publish block), so it can't silently regress. Full write-up: [`conformance/eval/results.md`](conformance/eval/results.md).

## What it's for, and what it's not

**For:** CRUD-shaped, multi-tenant REST APIs. The backbone of most SaaS.

**Not (yet):** realtime / WebSockets, GraphQL / gRPC, blob storage, edge / serverless. jerrycan runs as a normal long-lived service. We'd rather name the edges than oversell the middle.

## Built with

jerrycan stands on the Rust ecosystem you already trust, and emits plain Rust you own:

[Rust](https://www.rust-lang.org) · [Tokio](https://tokio.rs) · [hyper](https://hyper.rs) · [SeaORM](https://www.sea-ql.org/SeaORM/) · [serde](https://serde.rs) · [clippy](https://github.com/rust-lang/rust-clippy) · [cargo-audit](https://github.com/rustsec/rustsec)

## Roadmap

<details>
<summary><b>Phases 0-4 + the full v2 cycle, all complete (click to expand)</b></summary>

| Phase | Scope | Status |
|---|---|---|
| **0 - Contracts** | Core API spike (DI, modules, routing, serving) + AI docs + MCP/CLI contracts | ✅ complete |
| **1 - Core loop** | `jerrycan` CLI (new/generate/dev/check) + MCP server | ✅ complete (incl. 1b hardening) |
| **2 - Data & TDD** | jerrycan-db, jerrycan-validate + OpenAPI, per-module test generation | ✅ complete |
| **3 - Production** | jerrycan-auth, jerrycan-observe, `jerrycan package` (Docker/k8s/binary/systemd) | ✅ complete |
| **4 - Hardening** | Fuzzing, agent evals, diagnostics polish → v0.1.0 | ✅ complete |
| **v0.1.0** | First release, crates published on crates.io | 🚀 released |
| **v2.0 - Data foundation** | Contract v1 (relations + `on_delete`, unique/index, enums, json, **tenancy**, jobs shape), SeaORM data layer, `schema.json` contract + `jerrycan_schema` tool, generated isolation tests | ✅ complete |
| **v2.0b - Core readiness** | Dual-lane body + per-route limits, param-carrying mounts, task-scoped DI, extension lifecycle, mockable `Clock` | ✅ complete |
| **v2.1 - Protocol surface** | `Multipart` / `RawBody` (webhook signatures) / `StreamBody` extractors | ✅ complete |
| **v2.2 - Middleware kit** | CORS in core; rate limiting as an extension (`429 JC0429`) | ✅ complete |
| **v2.3 - jerrycan-jobs** | `JobStore` (Postgres / Redis), retries + dead-letter, named queues, cron, idempotency, `run_at` | ✅ complete (incl. v2.3b Redis Streams) |
| **v2.4 - Auth expansion** | OAuth2 client, encrypted token storage + key rotation, scoped API keys, mock IdP harness | ✅ complete |
| **v2.5 - Eval gate → v0.2.0** | Reference slice rebuilt on jerrycan, served live, every v2 feature driven over real HTTP, wired as a permanent, un-skippable CI + publish gate | ✅ complete |

The v1 plan is in the [v1 design spec](docs/superpowers/specs/2026-06-09-jerrycan-design.md); the v2 roadmap is in the [v2 design spec](docs/superpowers/specs/2026-06-11-jerrycan-v2-design.md); deferred items are in the [backlog](docs/phase1-backlog.md).
</details>

## Development

<details>
<summary><b>Build · test · lint · bench · fuzz</b></summary>

```bash
cargo test --workspace --all-features   # CI runs this, every docs example is a doc-test
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo bench                             # criterion benches (routing, extraction)
cargo +nightly fuzz run <target>        # fuzz targets live in fuzz/ (outside the workspace)
```

The project is built docs-first and test-first: documentation examples are the executable specification.
</details>

## Sponsors

jerrycan is built by one developer and a fleet of agents. Sponsorship pays for the eval infrastructure, the deploy targets, and the time it takes to keep the gate honest.

| Tier | Monthly | What you get |
|---|---|---|
| 💎 **Diamond** | $500 | Your logo at the top of this README and on [jerrycan.cc](https://jerrycan.cc), priority issue triage, and a monthly 1:1 call on your agent-backend setup |
| 🥇 **Gold** | $250 | Your logo in this README, priority issue triage |
| 🥈 **Silver** | $100 | Your name in this README, sponsor badge |

<a href="https://github.com/sponsors/Sorcecoder"><img src="https://img.shields.io/badge/GitHub%20Sponsors-sponsor%20jerrycan-EA4AAA?logo=githubsponsors&logoColor=white" alt="Sponsor on GitHub"></a>
<a href="https://buymeacoffee.com/sorcecoder"><img src="https://img.shields.io/badge/Buy%20me%20a%20coffee-sorcecoder-FFDD00?logo=buymeacoffee&logoColor=black" alt="Buy Me a Coffee"></a>

## License

Licensed under the [MIT License](LICENSE-MIT).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be licensed under the MIT License, without any additional terms or conditions.

---

<div align="center">

[jerrycan.cc](https://jerrycan.cc) &nbsp;·&nbsp; [AI-native docs](docs/ai) &nbsp;·&nbsp; GitHub [@backant-io](https://github.com/backant-io)

</div>
