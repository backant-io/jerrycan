# jerrycan v2 — "Reference-complete" (framework 0.2.0, design contract v1)

Validated 2026-06-11 with the founder. Successor to the
[v0 design spec](2026-06-09-jerrycan-design.md); informed by the v0.1.0
end-to-end audit and the gap analysis against a
production Flask sales-engagement backend (~213 endpoints, 33 tables, 97 FKs,
Celery worker, Stripe/Twilio/CRM integrations).

## North star and definition of done

**v2 is done when Reference's REST API service could genuinely be rebuilt on
jerrycan**, proven by an agent building a representative slice docs-only via
MCP (the v2.5 eval gate). Every feature is built generically; Reference is the
acceptance test, not the customer. The realtime voice-copilot service
(WebSockets, Twilio Media Streams, Deepgram) is explicitly out of scope — it
remains a separate service in any stack.

## Decisions log

| Decision | Choice | Key rationale |
|---|---|---|
| North star | Reference-complete | Sharp, real acceptance test; guards against speculative features |
| Data layer | **Adopt SeaORM 1.x now** (on the existing sea-query + sqlx stack) | Relations arrived (97 FKs in the target); transactions + eager loading come with the library; founder decision after trade-off review |
| Jobs engine | **Own minimal queue** (`JobStore` trait; Postgres default, Redis optional) | Generator controls all call sites, so a general-purpose jobs API solves problems we do not have; semantics capped by contract (below) |
| Sequencing | Data-first ladder, eval design written day one | The schema/SeaORM change is the most invasive; break the contract once, first |
| DB contract | **jerrycan-owned `schema.json`** (no DBML, no third-party format) | Joins the existing contract family; one payload across file/CLI/MCP |
| Tenancy | **First-class design concept** | Target scopes every query by workspace; generated isolation tests are a differentiator |
| Versioning | **0.2.0 now; 1.0 = contracts frozen** after eval + bake | Semver honesty; 1.0 is a promise of stable machine contracts |
| Eval scope | OAuth included, **vs a mock IdP** in the test harness | Hermetic gate; OAuth is where docs-only generation most often goes wrong |

## Out of scope for v2 (named, deliberately)

WebSockets/SSE, WebAuthn passkeys, SAML SSO, RabbitMQ/external brokers,
RS256/asymmetric JWT, live-database drift introspection (`sea-schema` later),
a separate worker binary (in-process workers first), `jerrycan upgrade`
(post-v2 longevity feature), DBML in any form.

---

## Phase ladder

### v2.0 — Data foundation (the breaking change, done once, first)

**Design schema → contract_version 1** (additive; every v0 design remains
valid and generates identically). Contract v1 defines the **entire v2
surface in one bump** — including the `jobs` object shape implemented later
in v2.3 — so the contract breaks exactly once:

- `belongs_to` relations on entities with `on_delete: cascade | set_null |
  restrict`; `has_many` is implied by the inverse.
- Field flags: `unique: true`, `index: true`; composite uniques at entity
  level.
- `json` fields allowed in db mode (TEXT/JSONB storage,
  `serde_json::Value` in models).
- String enums: `values: [...]` on string fields (CHECK constraint in DDL,
  validated in `Valid<T>`).
- **Tenancy**: top-level `"tenancy": { "entity": "...", "member_roles":
  [...] }` generates the M2M membership table, a `Tenant` guard extractor
  (membership-checked, exposes tenant id + role), tenant-scoped repo methods
  (`all_for`, `get_for`, `remove_for`) on every entity that `belongs_to` the
  tenant, and **generated cross-tenant isolation acceptance tests** (tenant A
  must not read tenant B). A jerrycan lint flags unscoped queries against
  tenant-owned tables.

**SeaORM adoption** (jerrycan-db v2):

- `Db` wraps `sea_orm::DatabaseConnection`; `db.transaction(|txn| ...)` is the
  sanctioned multi-statement idiom. JC0409/JC0510 mapping preserved.
- Generated `model.rs` = SeaORM entities (`DeriveEntityModel`, `Relation`);
  `repo.rs` remains the agent-owned seam wrapping SeaORM queries — the
  "db access only in repos" lint keeps its meaning.
- Accepted deviation: generated route crates gain a direct `sea-orm`
  dependency (its derives emit `::sea_orm` paths). Pinned to SeaORM 1.x;
  2.x is mid-transition upstream.
- Migrations stay module-owned dual-dialect `.sql` with the existing runner;
  new `jerrycan generate migration <name> --module <m>` emits numbered pairs
  and rewires `migrations.rs`. sea-orm-migration is not adopted.

**The DB contract — `schema.json`** (project root, beside `design.json`):

- Governed by a published JSON Schema (`jerrycan.cc/schemas/db-schema-v1.json`).
- Content: tables with owning module, columns (design-language types:
  `string`/`integer`/`float`/`boolean`/`datetime`/`uuid`/`json`, nullability,
  pk), foreign keys with `on_delete`, unique constraints, indexes, enums.
- Derivation: apply the module migrations to a throwaway `sqlite::memory:`
  and introspect — no SQL parsing; hand-written follow-on migrations are
  reflected automatically. `jerrycan check` regenerates and fails with a JC
  code if the committed file is stale (same tripwire pattern as the embedded
  docs).
- One payload, three surfaces: the committed file, `jerrycan schema --json`,
  and the new MCP tool `jerrycan_schema` return identical JSON
  (mcp-tools.json grows 9 → 10 tools, additive).

**Day one of this phase:** write `conformance/designs/reference-slice.design.json`
(workspaces + members tenancy, leads with relations + CSV import, API keys, a
Stripe-style signed webhook, two cron jobs, an OAuth connect flow). Every
later phase builds against it.

### v2.0b — Core readiness (the substrate the feature phases assume)

1. **Body architecture**: dual-lane — buffered lane (Json/RawBody, capped)
   and streaming lane (multipart parts, response streams); **per-route body
   limit overrides**. This is the riskiest core surgery in v2 and is done
   here deliberately, not discovered mid-feature.
2. **Router**: param-carrying mount prefixes become fully supported (removes
   the documented v0 limitation; required by `/workspaces/{id}/...` shapes
   and tenancy); path-param typing opens beyond the sealed set (custom id
   newtypes — promoted from backlog).
3. **Task-scoped DI**: `Dep<T>` resolution outside HTTP requests (same
   providers, defined memoization, test overrides) so job handlers keep the
   signature-visible DI model.
4. **Extension lifecycle**: `on_start` / background-task spawning / graceful
   drain participation for extensions, integrated with existing
   SIGINT/SIGTERM handling (needed by jobs workers and the rate-limit
   sweeper).
5. **Clock abstraction**: a mockable `Clock` dependency (real by default,
   controllable in `TestApp`) — rate-limit windows, cron, `run_at`, and
   backoff become deterministic to test.
6. Minor: new JC codes (429 and multipart variants), TestApp multipart +
   time-travel helpers, cap the MCP stdio line length (open finding from the
   0.1.0 audit).

### v2.1 — Protocol surface (jerrycan-core)

`Multipart` extractor (streaming parts; per-part size and part-count caps),
`RawBody` extractor (webhook signature verification over exact bytes; docs
recipe for Stripe/Twilio), `StreamBody` responses (downloads, CSV export)
with write timeouts. Every new parser gets a fuzz target (Phase-4
discipline).

### v2.2 — Middleware kit

- **CORS** in core: allowlist, credentials-safe (refuses `*` with
  credentials), preflight handling.
- **Rate limiting** as an extension: fixed-window; in-memory store default,
  Redis store behind a feature; identity-aware partition key (api-key → user
  → IP); `429 JC0429`; OPTIONS exempt.

### v2.3 — jerrycan-jobs (new crate)

- **`JobStore` trait** (enqueue / lease / ack / retry / dead-letter):
  Postgres impl via `SELECT ... FOR UPDATE SKIP LOCKED` (default, zero extra
  infra); Redis impl via the `redis` crate (Streams + consumer groups,
  `XAUTOCLAIM` for crashed-worker reclaim) behind a `jobs-redis` feature.
- **Capped semantics — this list is the contract**: at-least-once delivery;
  N retries with exponential backoff then a dead-letter table (inspectable,
  requeueable); named queues with per-queue worker concurrency; cron with
  skip-missed semantics and a Postgres-advisory-lock leader; idempotency key
  (unique index, conflict = no-op); `run_at` delayed jobs. Explicitly not
  built: priorities, workflows/chains, rate-limited queues, delayed fan-out.
- Jobs are design-schema objects (`"jobs": [{ "name": ..., "schedule": ...,
  "queue": ... }]` — the shape is defined by contract v1 in v2.0; this phase
  implements the engine); the generator emits typed task stubs and failing
  acceptance tests, exactly like handlers. Workers run in-process with the
  app (lean two-tier deploys).

### v2.4 — Auth expansion (jerrycan-auth)

- **OAuth2 authorization-code client** with refresh; providers as *config
  presets* (google, github, hubspot, salesforce), not code; linked-identities
  pattern documented.
- **Token storage encrypted at rest** (ChaCha20-Poly1305, already in stack);
  `zeroize` on key material; documented key-rotation story (multi-key decrypt
  so rotating `JERRYCAN_SECRET` does not invalidate every session).
- **Scoped API keys**: hashed at rest, prefixed, `ApiKey` guard extractor +
  `require_scope`.
- **Mock IdP test harness**: a tiny in-process authorization-code + refresh
  server used by generated tests and the v2.5 eval.

### v2.5 — The eval gate (release condition for 0.2.0)

An agent rebuilds the Reference slice via MCP, docs-only. Pass requires:
`jerrycan check` green; generated acceptance tests green (including
cross-tenant isolation); a live HTTP battery (tenant isolation, webhook
signature rejection, multipart CSV import, API-key scopes, both crons firing
under the test clock, OAuth connect against the mock IdP); and the agent
answering data-structure questions from `schema.json` alone. Pass rate is
recorded in the README. Generated-app cold-build time is measured here — the
SeaORM compile-tax check, with feature-trimming as the lever if it regresses.

---

## Foundation layer (cross-cutting commitments)

1. **Threat model** — `docs/contracts/threat-model.md`, covering classic
   layers plus the AI-native attacker class: **the agent itself as untrusted
   input**. Documents the existing mitigations as intentional architecture
   (tool-owned/agent-owned boundary, `forbid(unsafe_code)` in generated
   crates, SQL-outside-repos lint, check gate) and adds lints flagging
   `std::process`, raw sockets, and filesystem access in handler code.
   Lands in v2.0b.
2. **Crypto review as a 1.0 gate** — independent external review of the
   hand-rolled session codec and JWT envelope before contracts freeze.
3. **Supply-chain policy** — every new dependency requires written
   justification in the spec that introduces it; `cargo-semver-checks` in CI;
   MSRV declared and tested; dependency count tracked per release. Audit/deny
   gates remain mandatory.
4. **Tested invariants** — (a) determinism: same design.json → byte-identical
   generated output (golden-output corpus in CI); (b) compatibility: every
   contract_version 0 design validates and generates under v1 (compat suite).
5. **The agent eval is a permanent release gate** — from v2.5 onward, no
   release ships if the eval pass rate drops. Un-skippable, like clippy.
6. **Named for later**: `jerrycan upgrade` (cross-version regeneration of
   tool-owned files + report of required agent-owned changes), live-db drift
   introspection, separate worker binary, connection back-pressure tuning.

## Risks accepted

- SeaORM dependency in generated route crates (purity rule relaxed by
  decision); SeaORM pinned to 1.x against upstream 2.x churn.
- Jobs semantics creep — the capped list above is the contract; additions
  require a design revision.
- Contract 0/1 coexistence — mitigated by additive-only schema changes plus
  the compat suite.
- Upgrade story for existing 0.1.0 apps is documentation only (regenerate
  tool-owned files; agent-owned files untouched) — acceptable with zero
  external users.

## Testing strategy

Each phase keeps the full estate green (workspace tests, doc-tests, heavy
conformance, fuzz). New parsers (multipart, cron expressions) get fuzz
targets. Jobs and rate limiting test deterministically via the Clock
dependency. Tenancy ships with generated isolation tests. The reference-slice
eval is the integration test of record.

## Housekeeping

README: remove "publish-pending" language (0.1.0 is live on crates.io);
roadmap table gains the v2 phases.
