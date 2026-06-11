# Changelog

## 0.2.0 — unreleased

The v2 data foundation: a single contract bump that lands relations, constraints,
and first-class multi-tenancy, and moves the data layer onto SeaORM.

### Design contract (contract_version 1, additive)
- `belongs_to` relations with `on_delete: cascade | set_null | restrict`
  (`has_many` implied); field flags `unique` / `index` plus entity-level
  composite uniques; `json` fields (`serde_json::Value`, TEXT/JSONB storage);
  string enums via `values: [...]` (CHECK constraint + `Valid<T>`).
- **Tenancy**: a top-level `tenancy` block generates the M2M membership table, a
  membership-checked `Tenant` guard (`Dep<Tenant>`, `id()`/`require_role()`),
  tenant-scoped repo methods (`all_for`/`get_for`/`remove_for`) on every entity
  that `belongs_to` the tenant, and cross-tenant isolation acceptance tests.
- **Jobs**: the `jobs` object *shape* is defined now (contract breaks once); the
  engine arrives in a later v2 phase.

### Data layer (jerrycan-db on SeaORM)
- `Db` wraps `sea_orm::DatabaseConnection`; generated `model.rs` is SeaORM
  entities, `repo.rs` stays the agent-owned query seam. JC0409/JC0510 preserved.
- `jerrycan generate migration <name> --module <m>` emits numbered dual-dialect
  pairs and rewires `migrations.rs`.

### Schema contract
- `schema.json` (beside `design.json`) — tables, columns, foreign keys, uniques,
  indexes, enums — derived by introspecting the applied migrations. Surfaced
  three ways from one payload: the committed file, `jerrycan schema --json`, and
  the new MCP tool `jerrycan_schema` (mcp-tools.json grows 9 → 10, additive).
- `jerrycan check` regenerates and fails `JC0520` if the committed file is stale.

### Isolation & lints
- Generated cross-tenant isolation tests (tenant A must not read tenant B).
- `JL0006` flags unscoped repo queries on tenant-owned tables.

### Invariants
- Determinism: the same `design.json` produces byte-identical generated output
  (golden-output corpus in CI).
- Compatibility: every contract_version 0 design validates and generates under
  v1 (compat suite).

### Core readiness (v2.0b)
The serving core grown up to carry real apps: streaming-shaped responses,
per-route limits, task-scoped DI, an extension lifecycle, and injectable time.
- **Two-phase read**: routing decides BEFORE the body is read, so an unknown
  path (`404`) or wrong method (`405`) never drains an oversized body. Each
  route sets its own ceiling with `.body_limit(n)`; the 1 MiB cap is the default.
- **Boxed response bodies** (`JcBody`) — responses carry a boxed body, the seam
  streaming will plug into. The serve engine moved into its own `serve.rs`.
- **Leaf-most path binding**: a single `Path<T>` binds the LAST captured param,
  so a route under a param-carrying mount (`/ws/{ws}` + `/leads/{id}`) addresses
  its own `{id}`; tuples still read root→leaf. Custom id newtypes opt in through
  the new `jerrycan::path_param!` macro.
- **Task-scoped DI**: `BuiltApp`/`TestApp::task_context()` resolves app-level
  deps OUTSIDE a request — startup wiring, background jobs, CLI. HTTP extractors
  (`Json`/`Path`/`Query`/`Headers`) reject there with `JC1003`.
- **`App::on_serve`**: background tasks that run for the lifetime of `serve`,
  receive a `TaskContext` + shutdown watch, and drain under the same 10s budget;
  `into_test` deliberately does not run them.
- **Injectable `Clock`**: handlers/tasks take `Dep<Clock>` and call `now()` for
  domain time (rate windows, schedules, expiry); tests move it with
  `TestApp::clock().advance(..)`/`.set(..)`. Transport timeouts stay on real time.
- **Public endpoints**: routes can be marked `public: true` (a `JL0004` carve-out
  for credential-issuing routes); the `kolli` users module gains `register`/`login`.
- **Cross-module relations** are unenforced fk columns by default; `schema.json`
  reports the enforcement state per relation.
- **MCP stdio**: lines are capped at 16 MiB — an overlong line fails loud with
  JSON-RPC `-32600` and the reader recovers rather than wedging.
- **Threat model** published (`docs/threat-model.md`); `JL0007` flags handler
  code that escapes the request boundary.

### Breaking
- `Db::pool()` is removed — use `Db::conn()` (a `sea_orm::DatabaseConnection`).
- Generated apps must regenerate tool-owned files (`model.rs`, `lib.rs`,
  migrations runner, tests) to pick up the SeaORM layer and tenancy wiring.
- MSRV raised to Rust 1.88 (the SeaORM data layer pulls `time` 0.3.47, which
  requires it; resolves RUSTSEC-2026-0009).

## 0.1.0 — first release

The first public release of jerrycan — the AI-native Rust backend platform.

### Framework (jerrycan-core)
- App/Module routing with a backtracking trie, typed extractors, FastAPI-grade
  dependency injection (async, nested, per-request cached, test-overridable),
  middleware, in-memory TestApp.
- Secure by default: security headers, body/read/handler timeouts, panic→500
  containment, graceful shutdown (SIGINT/SIGTERM), percent-decoding, 1 MiB body
  cap. `#![forbid(unsafe_code)]`.
- Stable `JC####` error codes mapped to docs anchors.

### Extensions
- `jerrycan-db` — SQLite + Postgres: sea-query builds all SQL (dialect-correct
  placeholders, quoting, RETURNING via `Db::query_builder()`), sqlx executes;
  module-owned dual-dialect migrations; unique-key violations map to
  `409 JC0409`, other db failures to `500 JC0510` — neither leaks internals.
- `jerrycan-auth` — argon2 password hashing, ChaCha20-Poly1305 sessions, HS256
  JWT, `Session`/`Bearer` extractors, role guards.
- `jerrycan-validate` — `Valid<T>` extractor, structured 422s, OpenAPI 3.1.
- `jerrycan-observe` — request IDs, JSON access logs, `/healthz`, Prometheus
  `/metrics`.

### Platform (the `jerrycan` binary: CLI + MCP)
- Design-first, TDD generation of crate-per-module apps; `new`, `generate`,
  `gen-tests`, `check`, `test`, `dev`, `list`, `docs`, `explain`, `add`,
  `db migrate`, `package`, `mcp`.
- `jerrycan package` → hardened Docker/k8s/systemd + static binary + CycloneDX SBOM.
- AI-native docs (every example a doc-test) + the MCP tool contracts.

### Hardening
- Fuzz-smoke + cargo-fuzz targets on all parsers; criterion benchmarks; an
  agent-eval harness (reference apps + recorded pass rate).
- Full from-scratch E2E audit (CLI + MCP + live HTTP + load + fuzz): generated
  migrations no longer duplicate a declared `id` pk; creates return the real
  id via `INSERT … RETURNING` on both backends; text (uuid/string) pks key
  repos, extractors, and generated tests consistently; CI runs cargo-audit and
  cargo-deny with documented policies.
