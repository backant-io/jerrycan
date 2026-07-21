# Changelog

## 0.6.1 — 2026-07-21

The public-read / owner-write ownership shape (#105) — the mainstream feed /
post / listing / job-board model: **anyone reads, only the owner writes**.

### New: `public_read` on an entity (#105)
An identity-owned (per-user, #79) entity may set `"public_read": true`. Its reads
become public while its writes stay owner-scoped:
- **Reads** (GET list + detail) are unguarded and unscoped — a public list
  returns every owner's rows (the feed intent). No session required.
- **Writes** (POST/PUT/PATCH/DELETE) are unchanged from #79: authenticated,
  server-injected `user_id` on create, `update_for`/`remove_for` keyed on the
  caller (404 for a non-owner — existence hidden), unscoped `update`/`remove`
  never emitted.

Opt-in and additive: absent the flag, every existing design generates
byte-identical output. The precedent is the storage `visibility: public` bucket
(open reads + owner-stamped writes), lifted to HTTP entities.

### Validation
- **`JC0549`** refuses the unsafe spellings: a `public_read` write that is
  `public`/unguarded (writes must stay owner-gated); `public_read` on a
  non-identity-owned or tenant-owned entity (identity-owned-only in v1);
  `public_read` with no auth model. It also closes a latent trap — an unguarded
  GET on a per-user entity that had *not* opted into `public_read` used to
  generate an unimplementable stub; now it's a clear fork ("set `public_read:
  true` to make reads public, or keep the GET authenticated").

### Behavior
- One shared guarding predicate now drives the generator, the OpenAPI, and
  testgen: a `public_read` GET drops its OpenAPI `security` stanza and generates
  no `*_without_auth_is_401` probe, so a correct public-read feed's acceptance
  suite is green (previously the GET ran unguarded but the suite still asserted a
  401 — a red-when-correct test).
- JL0006 stays silent on a public_read module's now-legitimate unscoped reads but
  still fires on an unscoped write; JL0004 (unguarded mutation) still fires on any
  unguarded write.
- The Supabase migrator routes a public-SELECT + owner-write source table to the
  `public_read` entity shape instead of the previous silently-unimplementable
  `public: true` GET stub.

### Docs
- `docs/ai/00-designing.md` and `14-tenancy.md` (with their embedded twins) teach
  the public-read/owner-write shape.

### Packaging
- The `platform` module is internal codegen, not a stable Rust API;
  `constructible_struct_adds_field` is scope-allowed for the `jerrycan` package
  (`[package.metadata.cargo-semver-checks.lints]`) so an additive `platform::Entity`
  field doesn't force a spurious 0.x-major bump (tracked: #145).

## 0.6.0 — 2026-07-21

First 0.6 minor — a new generated capability: **membership management**. Every
many-membership tenancy app used to hand-seed `{tenant}_members` via raw SQL
(the top eval friction); now the framework generates a full member surface.

### New: member-management surface (#107)
When a design has `tenancy`, jerrycan generates, on the tenant module, a real
(not stub) member surface:
- `GET  /{tenant}/{fk}/members` — list the roster (any member).
- `POST /{tenant}/{fk}/members` `{user_id, role}` — add (admin-gated).
- `PATCH /{tenant}/{fk}/members/{user_id}` `{role}` — change role (admin-gated).
- `DELETE /{tenant}/{fk}/members/{user_id}` — remove; **self-removal** ("leave")
  is allowed without the admin role.

Authorization is correct-by-construction: writes require the **admin role**
(`member_roles[0]`); the `Dep<Tenant>` guard 404s non-members of the addressed
tenant (no cross-tenant management); **last-admin lockout** is refused (409 on
removing/demoting the sole admin, including self); an out-of-set `role` is 422;
a duplicate member is 409. The routes appear in the generated OpenAPI, and
acceptance tests cover every gate.

### Validation
- **`JC0548`** — `member_roles` must be non-empty, duplicate-free, and
  identifier-shaped (role names are interpolated into generated code).
- **JC0542 hardened** — the implicit member routes now participate in
  design-time conflict detection, and the comparison is segment-normalized, so a
  trailing-slash or `//` collision (e.g. a hand-rolled `GET /{fk}/members/`) is
  caught by `check` instead of panicking at `App::build`. Closes #140 (and the
  pre-existing `/x` vs `/x/` design-route class).
- **Supabase migrator** synthesizes a default `member_roles = ["admin","member"]`
  when the source membership table has no role constraint (#139).

### Behavior notes
- Every tenancy app gains the member surface (additive routes; existing routes
  unchanged). Non-tenancy apps are byte-identical to 0.5.4.
- A design with empty/duplicated `member_roles` — including a pre-0.6.0 app
  regenerated on 0.6.0 — now fails `check` with `JC0548`; add a valid roles list.

### Deferred (tracked)
- Last-admin guard is check-then-act (a concurrency race, #138); an entity named
  `{Tenant}Member` collides with the members table (#141); storage `write_roles`
  is now unblocked (#132); no live realtime-socket revocation on remove/re-role;
  `require_role` stays single-role exact-match.

### Onboarding & distribution
- **`jerrycan onboard`** — prints the guided build runbook; `--emit-skill --agent <id>` installs the jerrycan-backend skill for claude-code / cursor / codex / windsurf / generic.
- Skill: explicit entry-path branching (existing project / from scratch / migrate from Supabase) + a Phase 1c Supabase-migration runbook.
- Release: tag-triggered **prebuilt binaries** (macOS arm64/x64, Linux x64/arm64 musl) + `cargo binstall jerrycan`.

Framework Rust API additive (`cargo semver-checks` clean); the surface is
generated-app code.

## 0.5.4 — 2026-07-21

Storage-tenancy security patch — continues the transitive-tenancy work into the
storage subsystem (after HTTP routes in 0.5.1 and realtime in 0.5.3).

### Security
- **Transitive bucket owners — closed.** A storage bucket whose `owner` was a
  *transitively* tenant-owned (grandchild) entity was mis-classified as
  user-scoped: no `Dep<Tenant>` guard and no `tenant_id` stamping, so any
  authenticated user could read/write it. Bucket ownership now uses the
  transitive `tenant_path` resolver — a grandchild-owned bucket is membership-
  guarded and tenant-stamped like a direct-owned one. Direct-owned buckets are
  byte-identical.
- **`JC0545` for ambiguous bucket owners.** A bucket owner that reaches the
  tenant through more than one `belongs_to` path (a diamond) is refused at design
  time instead of silently degrading to per-user scope.
- **#109 — honest status.** A private tenant bucket's `download` accepts a
  session OR a signed URL; the `Option<Dep<Tenant>>` extractor discarded the
  guard's real status, so an authenticated **non-member** was reported **401**
  instead of **403**. A new error-preserving `Result<T, Error>` request extractor
  (`jerrycan-core`) keeps the guard's status: missing session → 401, non-member →
  403, foreign-tenant member → 404, valid signed URL → works.

### Deferred (tracked)
- Owner-write / shared-read role split (#132, gated on the #107 membership
  surface); cross-scope key-existence oracle (#133); storage's first-membership
  arbitrariness (facet of #104) → 0.6.0.

Framework Rust API additive only (`jerrycan-core` gains the `Result<T,Error>`
extractor; `cargo semver-checks` clean). Non-storage apps and direct-owned
buckets are byte-identical to 0.5.3.

## 0.5.3 — 2026-07-20

Security patch — closes a **critical realtime broadcast leak**. A `changes`
channel on the **tenancy entity itself** (`changes: ["Workspace"]` where
`Workspace` is the tenant) derived `tenant_column: None`, so `change_visible`'s
`(None, _) => true` broadcast **every row to every authenticated principal**,
member or not — a cross-tenant data leak with `check` green. Found by the
round-5 eval (collab app).

### Security
- **#113 — closed.** When the `changes` entity is the tenancy entity, its own
  primary key is now its tenant key (`tenant_column: Some("id")`), so a member of
  tenant T receives only tenant-T's own row and non-members receive nothing. The
  runtime CDC path was already correct once the column is populated — codegen-only
  fix, zero runtime change.
- **`JC0547` — realtime `changes` on a transitively tenant-owned entity (a
  grandchild) is now refused at design time** instead of silently leaking (its
  tenant key lives on an ancestor table that CDC can't read from the row image).
  The changes entity must be the tenant itself or a direct child. Full transitive
  realtime is a 0.6.0 capability.

### Deferred (tracked)
- #117 (anonymous clients can't reach `scope:"none"` realtime topics) →
  0.6.0, alongside the #104 many-membership realtime rework (same resolver path).
- A generated live-WS cross-tenant negative-control test (the current realtime
  acceptance tests are `#[ignore]`d stubs) — needs an in-memory WS test client.

Realtime apps whose `changes` entity is a **direct child** of the tenant are
byte-identical to 0.5.2. Framework Rust API additive only (`cargo semver-checks`
clean).

## 0.5.2 — 2026-07-20

Greenability patch. The round-5 eval's biggest cost was builders re-scaffolding
whole apps because generated code and tests didn't compile or go green out of the
box — mostly **mount-blindness** (a module mounted at `/clubs/{club_id}` with a
child at `/books`). This makes the generator mount-aware end-to-end, and along
the way **completes the #125 cross-tenant write closure** (the create side).

### Security
- **#125 (create) — closed.** Path-fk detection is now mount-aware
  (`any_body_endpoint_resolved_path_has`): a child under a param mount drops the
  mount-carried tenant/parent fk from its request DTO, so a `POST
  /clubs/{club_id}/books` can no longer carry a foreign `club_id` — the handler's
  only tenant source is the membership-verified `Dep<Tenant>`. With 0.5.1's
  update-pin, cross-tenant relocation/injection is now closed for **both** create
  and update. (JC0544 was made mount-aware in tandem so it stops false-positiving
  a valid nested create.)

### Greenability
- **#81** — the test generator substitutes mount-inherited path params in
  acceptance URLs (`/workspaces/1/channels`, not a literal `/workspaces/{workspace_id}/channels`)
  and seeds the referenced parents, so subroute-mounted modules go green.
- **#84** — the jobs test harness migrates the full app schema (not just
  `JOBS_MIGRATIONS`), so a job touching a route table no longer fails
  `no such table`; the route TestApp declares the app's realtime topics.
- **#85** — a field's `default` value is now settable on **update** (a new
  `{Entity}UpdateRequest` DTO keeps it; create still omits it); a non-`{id}` path
  param types from its referenced entity's pk (uuid/string, not hardcoded `i64`);
  unique-field probe seeds are distinct so a create probe no longer 409s.
- **#114** — an entity named after a prelude identifier (e.g. `Module`) is
  rejected at design time with a new **`JC0546`** instead of scaffolding an
  uncompilable app.
- **#120** — generated `main.rs` module declarations are emitted in sorted order,
  so a fresh scaffold is `cargo fmt --check` clean (no JL0003 self-trip).

### Behavior notes
- A **nested-mount child's request DTO** loses the now-path-redundant fk (a
  correctness change — it was demanding a value already in the URL).
- A **default-bearing entity** gains a generated `{Entity}UpdateRequest` DTO.
- An app **generated by 0.5.1** will show a one-time `main.rs` mod-sort drift
  under a 0.5.2 `check` (JL0003) until regenerated — regenerate to clear it.

### Deferred (tracked)
- Subroute tenant child under an ancestor param mount is mis-scoped (#116); the
  handler doesn't auto-bind the mount parent fk (#127); `cargo fmt` still rewraps
  long `include_str!` lines for heavier apps (#128); a reserved-prelude drift
  tripwire (#129); OpenAPI path-param schema still `int64` for string-pk referents.

Framework Rust API additive only (`cargo semver-checks` clean). Direct / flat /
non-nested / no-default apps are byte-identical to 0.5.1.

## 0.5.1 — 2026-07-20

Security patch. v0.5.0 made tenancy correct-by-construction for **direct** tenant
children only; multi-hop tenant graphs (`Contact → Account → Org`, `Message →
Channel → Workspace` — the common CRM/chat shape) silently escaped every defense.
0.5.1 makes tenant ownership **transitive**: an entity is tenant-owned iff a
`belongs_to` chain reaches the tenant, at any depth. The framework's Rust API is
additive only; apps with no transitive tenant graph generate **byte-identical**
output.

### Security & scoping
- **Transitive tenant ownership (closes #102).** A new resolver walks the
  `belongs_to` chain to the tenant; guard, scoped repo methods, lint, and
  isolation test all key off it. Grandchild+ reads/writes **JOIN up the chain**
  and apply the same membership filter, so a member of Org A can no longer read
  or write Org B's contacts/deals. Previously `jerrycan check` was green while the
  data leaked.
- **Path-scoped writes pin the tenant fk to the route (closes #125, update half).**
  A path-scoped `update_for` now writes the tenant/parent fk from the **path**,
  never the request body, so a body fk cannot relocate a row into another tenant.
  Transitive writes verify the resolved tenant ∈ the caller's memberships (403).
- **Ambiguous ownership is a hard error (`JC0545`).** An entity that reaches the
  tenant through more than one `belongs_to` path (a diamond) fails design
  validation before any code is generated — jerrycan will not guess which chain
  defines ownership. This is the one behavior change beyond the fix: a design that
  previously generated (and silently leaked on the ambiguous path) now fails
  `check` and must collapse to a single path.

### Tooling
- **JL0006 is now AST-based (closes #103).** The unscoped-repo-call lint parses
  handlers with `syn` instead of a substring scan, and resolves the **real nested
  handler paths** — previously it silently skipped subroute/grandchild handlers,
  which is how the #102 leak shipped past `check`. It also catches unscoped calls
  inside macro bodies (e.g. `json!`).
- **`JL0008` — fail-loud lint.** A tenant-owned handler that is missing,
  unreadable, or unparseable now produces a loud diagnostic instead of a silent
  skip, so the guardrail can never again fail closed-but-quiet.
- Cross-tenant **isolation tests are generated for grandchild** entities (seed the
  intermediate chain, assert a non-member gets 404).

### Deferred (tracked, not in 0.5.1)
- Transitive tenancy in **realtime** (`changes`/broadcast), **storage** buckets,
  and the **Supabase migrator** is still direct-only → 0.6.0 (#104/#113, #108/#109,
  #106). These are not the generated REST surface.
- **Path-scoped CREATE** parent-fk verification (a nested-under-tenant create can
  take an unverified parent fk from the body when the mount carries the fk) →
  0.5.2 with the mount-aware body-trim fix (#125 create vector, #82).
- JL0006 false-*positive* on a tenancy entity's own detail route (#124).

Framework Rust API additive only (`cargo semver-checks` clean; new `pub` item
`Design::tenant_owned_handlers`). Direct-child / per-user / non-tenant apps are
byte-identical to 0.5.0.

## 0.5.0 — 2026-07-19

The ownership-safety release. Tenancy and per-user scoping are now
**membership-verified and correct-by-construction** — the tenant a request acts
on comes from the route and must be in the caller's membership set, verified by
generated code before the handler scopes. Closes two cross-tenant/cross-user
leak classes surfaced by the agent evals. The framework's Rust API is additive
only (no breaking change), but generated apps behave materially differently, so
this is a 0.x-major bump.

### Security & scoping
- **Path-scoped tenant guard** reads the tenant fk from the route and verifies
  membership for *that* tenant — a non-member gets `404` (no existence leak),
  a wrong role `403`. Many-tenants-per-user (Slack/GitHub-org shape) is now a
  first-class, safe capability. Closes the cross-tenant read leak (#78).
- **Membership-set (RLS-faithful) reads** for flat routes (`{fk} IN (SELECT …
  WHERE user_id=?)`) — the **Supabase migration is lossless** for multi-workspace
  users; the migrator normalizes tenant-own detail routes so they're verified too.
- **Per-user scoping is make-impossible** (#79): a guarded identity-owned entity
  emits only owner-scoped repo methods — the unscoped leak won't compile.
- **Membership-checked flat writes** (#94): create/update/delete verify the body
  tenant fk against the set (`403` on a non-member tenant); JL0006 flags a bare
  unchecked write.
- **Scoped updates pin the ownership-checked path id** (#92) — a client-supplied
  body `id` can no longer redirect a write to another scope's row.
- Membership is **auto-seeded** on tenant create (one transaction); the tenant
  list is membership-filtered; isolation tests are generated for every shape.
- `docs/ai/14-tenancy.md` rewritten — the "`tenant.id()` is trusted" model that
  taught the leak is gone.

Known limitation (tracked, #97): the reference-slice example still uses the
single-membership fallback for its flat modules, so heavy conformance proves the
membership-set path at the unit level only. Safe (no leak), but lossy for a
multi-workspace user in that specific example.

## 0.4.1 — 2026-07-17

The agent-eval release: two 10-agent evaluation rounds (vs FastAPI, paired
apps, adversarially audited) drove three fix waves. First publish of
jerrycan-jobs, jerrycan-ratelimit, jerrycan-storage, and jerrycan-realtime.

### Fail-loud (round 1, issues #27–#35)
- `--json` failures emit one machine envelope (`{ok:false, code, error, hint}`).
- Tenancy=identity designs rejected at validation (JC0540) instead of dying at
  migrate with a raw SQLite error. `{X}Request` name collisions rejected (JC0541).
- Carrying the envelope on `Failure` broke 0.3.0's struct-literal contract —
  hence 0.4.x; `Failure` is `#[non_exhaustive]` so future fields stay minor.

### Generator & contract correctness (rounds 1–2, issues #42–#53)
- `auth.model: "jwt"` now generates real `Bearer` guards for REST routes
  (Supabase-migrated apps regression-tested); OpenAPI carries `securitySchemes`.
- Enum `values` reject out-of-range input with 422 at extraction, not 500 at
  the DB; enum value content constrained to identifier shape (JC0543).
- Request bodies: server-owned identity FKs, design-defaulted fields (new
  `default` key), and path-redundant parent FKs are omitted and injected
  server-side; schema allows 3xx successes; 3xx endpoints get Redirect stubs.
- Cron fires on SQLite via the in-process leader (silent no-op path removed);
  realtime gains a server-side `RealtimeHandle::publish` for REST handlers.
- gen-tests: entity-aware `/{id}` seeding, format-valid fixtures, design-derived
  credential roles, probe:"skip"-aware seeding, extension-wired TestApp harness.
- Router param-name conflicts and dual-create path-FK shapes are caught at
  validation (JC0542/JC0544) instead of panicking at runtime.
- `add`/`generate route` (CLI and MCP) warn loudly when regeneration drops
  agent-added lines from tool-owned files; no-body stubs bind the route's
  entity; mandated first-read docs split (−32% tokens); cross-module data
  access documented.

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
  for credential-issuing routes); the `reference` users module gains `register`/`login`.
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
0.2.0's release condition: the reference slice — the full v2 showcase —
rebuilt on jerrycan, served live, with every v2 feature exercised over real HTTP,
wired as a permanent gate.
- **Reference backend** — `conformance/eval/fixtures/reference` implements the
  slice (tenancy + JWT/session auth, tenant-scoped CRUD, multipart CSV import,
  raw-body webhook verification, scoped API keys, OAuth connect+callback against a
  mock IdP, two cron jobs) so a fresh scaffold of
  `conformance/designs/reference-slice.design.json` is `jerrycan check`-green.
- **Live HTTP battery** — `crates/jerrycan/tests/reference_eval.rs`
  (`reference_slice_live_battery`) scaffolds the slice, gets `check` green, runs the
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

### Deploy (Render)
- **Zero-touch deploy**: `jerrycan deploy render` generates an idempotent,
  secret-safe Render deploy kit (pure HTTP API) — a self-contained `deploy/render/`
  (`deploy.sh`, `teardown.sh`, `render.yaml`, `README.md`) an agent runs with only
  `RENDER_API_KEY` to stand the app up on Render: hardened image, managed Postgres,
  secrets generated into Render's store (never printed), `JERRYCAN_ENV=prod`
  fail-closed, TLS, health-checked, and a live HTTPS URL. Re-runs update in place
  (find-or-create); resource ids land in a gitignored `.deploy-state.json` (no
  secrets); `teardown.sh` removes the service + database. Private-registry pulls
  are wired via `JERRYCAN_DEPLOY_REGISTRY_USER`/`_TOKEN` (GHCR defaults to the
  image owner + `GITHUB_TOKEN`).

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
