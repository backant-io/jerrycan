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

### Protocol surface (v2.1)
The HTTP edge grown out to real protocol work: streaming bodies in and out,
multipart uploads, and raw-bytes webhook verification.
- **`Multipart` extractor** — streaming `multipart/form-data`: parts in wire
  order via `next_part()`, `chunk()` for unbuffered file reads and
  `bytes()`/`text()` for fields, with attacker-surface caps (8 MiB per-part
  buffer / `set_part_cap`, 256 parts, 8 KiB part headers → `413`; wrong content
  type → `415 JC0415`; malformed → `400`).
- **`RawBody`** — the exact request bytes on either lane (buffered or
  `stream_body()`), the extractor webhook signing needs.
- **`StreamBody` + `BodySender`** (plus the `JcBody` `BodyError` channel) —
  incremental downloads/exports via `channel()`/`new()`, with `content_type`,
  `attachment`, and a per-frame `frame_timeout` (default 30s). A mid-stream
  failure aborts the connection so truncation is always detectable, never a
  silently-incomplete body.
- **`.stream_body()` route marker** — the body is not buffered before dispatch;
  `body_limit` becomes a cumulative cap and `body_read_timeout` a per-frame
  read deadline (`408 JC0408`).
- **`App::write_stall_timeout`** (default 30s) — drops slow-reader clients so a
  stalled download can't pin a connection.
- **`JC0415`** unsupported-media-type added (e.g. `Multipart` without
  `multipart/form-data`).
- **`jerrycan::auth::webhook`** — constant-time `verify_sha256_hex`/
  `sign_sha256_hex` (Stripe/GitHub) and `verify_sha1_base64`/`sign_sha1_base64`
  (Twilio) over the raw request bytes; `serde_urlencoded` re-exported for signed
  form bodies.
- **Fuzzing**: a `multipart_parse` cargo-fuzz target plus a stable-toolchain
  fuzz-smoke run in CI.
- **TestApp**: `post_multipart`/`post_multipart_with` with `TestPart::text`/
  `TestPart::file`, and `TestResponse::bytes()` for raw download bodies.

### Middleware kit (v2.2)
The cross-origin and abuse-control edge: CORS in the core, a peer address on the
request context, and an identity-aware rate limiter as an extension.
- **CORS in core** (`App::cors(CorsConfig::new(CorsOrigins::list([..])))` /
  `CorsOrigins::any()`) — an exact-match origin allowlist, `allow_credentials`
  (with `any()`+credentials refused at build time), `max_age`, `allow_headers`,
  `expose_headers`. Preflight `OPTIONS` is answered BEFORE routing (`204` with
  reflected methods, not `405`), and the actual response is decorated afterward —
  including error responses (`404`/`413`/`500`) so the browser surfaces the real
  status. Same-origin and disallowed-origin responses are left undecorated;
  `Vary: Origin` rides every decorated response.
- **`RequestCtx::peer_addr()`** — the raw socket peer, the source the rate-limit
  IP tier partitions on.
- **`jerrycan-ratelimit`** (the `rate-limit` feature) — a fixed-window,
  identity-aware limiter installed with
  `app.extend(RateLimit::per_window(n, dur))`. Partition precedence is
  api-key header → `user_key` closure → client IP; `OPTIONS` is exempt;
  over-limit is `429 JC0429` + `Retry-After`. Builders: `api_key_header`,
  `user_key`, `trust_forwarded_for` (off by default — `X-Forwarded-For` is
  client-spoofable), `store`. The default store is in-memory; `RedisStore`
  (behind `rate-limit-redis`) shares one window across replicas. Windows are
  deterministic under `TestApp::clock().advance(..)`.
- **`JC0429`** too-many-requests added (the rate-limit extension; carries
  `Retry-After`).
- **TestApp**: `options_with` (preflight), `request`/`request_from` (arbitrary
  method + headers, with/without a peer), and `get_from`/`request_from` (drive a
  request from a chosen socket peer for the IP tier).

### Job engine (v2.3)
Background work off the request path: declared cron schedules and
programmatically-enqueued queue jobs, generated as typed task stubs.
- **`jerrycan-jobs`** (the `jobs` feature) — at-least-once queues over a durable
  Postgres `SELECT … FOR UPDATE SKIP LOCKED` store (always compiled,
  `Jobs::postgres(db)`) plus an in-memory test store (`Jobs::in_memory()`) and,
  behind `jobs-redis`, a Redis Streams store (`Jobs::redis(store)`, see below).
  **Jobs run at LEAST once** (a crashed worker's lease expires and the job
  re-runs), so task handlers MUST be idempotent — exactly-once is impossible
  across crashes.
- **Retries → dead-letter** — a failing (or timed-out) task is retried with
  exponential backoff; after `max_attempts` (default 5) it moves to the
  dead-letter set, inspectable (`list_dead`) and requeueable (`requeue_dead`),
  never silently dropped.
- **Cron** — a `schedule` (5-field cron) fires a job each tick with skip-missed
  semantics (a downtime backlog fires the most recent tick once, no backfill)
  under a single leader: on Postgres a `pg_advisory_xact_lock` leader (one node
  fires each tick, lock + enqueue + last-fired in one transaction);
  single-process deploys are the trivial leader.
- **`run_at`** delayed jobs, **idempotency keys** (a duplicate enqueue is a
  no-op reporting the existing id), and **per-queue worker concurrency** (one
  worker pool per declared queue).
- **Generation** — `design.jobs` emits a typed task stub per job
  (`crates/jobs/src/{name}.rs`: cron `async fn name(ctx)`, queue `(ctx,
  payload: {Name}Payload)`), a wired `Jobs` extension, and a failing acceptance
  test (red until the task is implemented).
- **`JobsHandle`** — app handlers resolve `Dep<JobsHandle>` and
  `enqueue(NewJob, now)` with the clock explicit, so tests drive time
  deterministically; **`TaskContext::fork`** gives each job a fresh
  dependency-resolution cache (DI isolation between jobs).
- **`JC0521`** job-failed/dead-lettered added.

### Redis Streams job store (v2.3b)
- **`Jobs::redis(RedisStore::connect(url).await?)`** (the `jobs-redis` feature, a
  facade `jerrycan/jobs-redis` feature too) — a durable, multi-node `JobStore`
  over Redis Streams + consumer groups + `XAUTOCLAIM`, satisfying the existing
  `JobStore` contract with no trait or generator change. It matches the
  in-memory/Postgres reference semantics exactly: at-least-once lease/reclaim,
  retries → dead-letter, `run_at` delays, permanent idempotency dedup,
  id-ordered `list_dead`, attempts-reset `requeue_dead`.
- **Atomic, cross-node enqueue idempotency** — the idempotency key is a `SET NX`
  inside the enqueue Lua script, so duplicate cross-node enqueues (e.g. cron
  ticks under the single-process leader) collapse to one job.
- **Crashed-worker reclaim** is `XAUTOCLAIM` keyed on the Redis-server idle time
  (= the lease) — the one place the store uses wall-clock rather than the
  injected `now`; a still-running worker is never stolen.
- `redis` stays rustls-only (no openssl); no new dependencies.

### Auth expansion (v2.4)
The auth surface beyond sessions and JWTs — all in `jerrycan-auth`, no generator
or design-contract change. Reuses the existing `JC0400`/`JC0401`/`JC0403` codes.
- **Key rotation + encrypted token-at-rest** — `Auth::with_secrets(primary,
  retired)` and `JERRYCAN_SECRET` / `JERRYCAN_SECRET_OLD` (comma-separated)
  do multi-key decrypt: the primary encrypts, retired secrets only decrypt, so
  rotating the master secret never logs users out until the old key is dropped.
  `Auth::tokens()` is a rotation-aware ChaCha20-Poly1305 codec keyed under a
  distinct `"oauth-token"` label (non-cross-decryptable with sessions) — encrypt
  a provider `TokenResponse` at rest with `auth.tokens().encode(&t)?`. Derived
  key bytes are `zeroize`d on drop.
- **Scoped API keys** — `mint(prefix)` draws 32 CSPRNG bytes, shows the plaintext
  once, and stores only its hex SHA-256 `hash`. `verify` is a constant-time digest
  compare (never `==` on the hex string). The `ApiKey` extractor reads
  `Authorization: Bearer` or `X-API-Key`, resolves the `ApiKeys` store
  (`InMemoryApiKeyStore` for tests/small deploys, a DB-backed `ApiKeyStore` in
  prod), and `require_scope` gates the handler (wildcard `"*"` is an admin grant).
- **OAuth2 authorization-code client** (the `oauth` feature; facade
  `jerrycan/oauth = ["auth", "jerrycan-auth/oauth"]`) — `Provider` presets
  (`google`/`github`/`hubspot`/`salesforce`) as config not code, `authorize_url`
  (+ S256 PKCE), `exchange_code`, and `refresh` over a `TokenTransport` seam
  (production `HttpTransport` is hyper + hyper-rustls, rustls-only). The
  `client_secret` lives in a non-`Debug` `Secret` and is never logged; a provider
  error surfaces as a non-500 `JC0400` naming the reason, never the secret.
- **Mock IdP harness** — `MockIdp` (deterministic, counter-driven) exposes both an
  in-process `token_transport()` for hermetic `OAuthClient` tests and an
  `into_app()` real `App` (`GET /authorize` 302, `POST /token`) sharing one core,
  so the wire path and the in-process path can't diverge.
- **Docs** — new `docs/ai/16-auth-advanced.md` (OAuth connect/refresh, the
  linked-identities table pattern, the token-at-rest rotation runbook, scoped API
  keys) with compiling doctests; the threat model gains an advanced-auth section.

### Eval gate (v2.5)
0.2.0's release condition: the Kolli reference slice — the full v2 showcase —
rebuilt on jerrycan, served live, with every v2 feature exercised over real HTTP,
wired as a permanent gate.
- **Kolli reference backend** — `conformance/eval/fixtures/kolli` implements the
  slice (tenancy + JWT/session auth, tenant-scoped CRUD, multipart CSV import,
  raw-body webhook verification, scoped API keys, OAuth connect+callback against a
  mock IdP, two cron jobs) so a fresh scaffold of
  `conformance/designs/kolli-slice.design.json` is `jerrycan check`-green.
- **Live HTTP battery** — `crates/jerrycan/tests/kolli_eval.rs`
  (`kolli_slice_live_battery`) scaffolds the slice, gets `check` green, runs the
  generated acceptance suite (incl. cross-tenant isolation), serves the app live
  (sqlite), and drives every feature over a real `TcpStream`: register/login,
  live cross-tenant isolation (`404` on another tenant's row), webhook signature
  `200`/`400`, multipart import `202`, scoped API keys `200`/`403`/`401`, OAuth
  connect `302` + callback `200`/`400` — plus both crons firing under a controlled
  clock and `schema.json` data-structure questions answered from `SchemaContract`
  alone.
- **Permanent, un-skippable gate** — the battery runs in CI's `--include-ignored`
  heavy step and is a fail-fast pre-publish block in `scripts/publish.sh`
  (alongside the scripted conformance/eval reference apps), with a documented
  `SKIP_EVAL_GATE=1` emergency escape. A release can't ship if the eval is red.
- **Additive design support** — a 3xx success status is allowed for endpoints
  like OAuth `connect` (`success: 302`), a tenant-scoped `update_for` repo
  accessor is generated alongside `all_for`/`get_for`/`remove_for`, and
  `Multipart::from_buffered` lets one route accept multipart or another content
  type. No contract change.

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
