# Changelog

## 0.7.1 — 2026-08-01

### Added
- **Opt-in `auth.identity` generalizes per-user owner-scoping (#150).** Per-user owner-scoping,
  the #34 server-injected fk, and `public_read` (#105) all detected ownership by the LITERAL
  derived column `user_id`, so an identity entity named anything but `User` (e.g. `Account`, fk
  `account_id`) SILENTLY got no owner-scoping AND kept its fk client-writable — spoofable ownership
  behind a green `check`. `Auth` now carries an opt-in `identity: Option<String>` (default `"User"`);
  the identity-fk DETECTION column is resolved per-design as `snake(auth.identity)_id` through the
  single `Design::identity_fk_column()`, threaded through every consumer (owner-scoped repo methods,
  the request-DTO fk omission + server-inject steer, testgen seeds/probes, the OpenAPI request
  schema, and the JC0540/JC0549/JC0560 validators + the JL0006 classifier). The membership-table
  PRINCIPAL column stays the fixed `user_id` (it stores the session principal, not the identity
  entity's fk). A non-existent `auth.identity` is refused with **JC0540** (identity == tenant org)
  or the new **JC0566** (identity names no declared entity). Additive on the now-`#[non_exhaustive]`
  `Auth` (0.7.0/#145): the default `"User"` resolves to `user_id`, so every existing design is
  byte-identical — the new behavior is opt-in only.

## 0.7.0 — 2026-08-01

The first release of the 0.7 major line. Groundwork only — no new design surface.

### Changed (breaking)
- **Platform config structs are now `#[non_exhaustive]`; the semver lint is re-enabled (#145).**
  0.6.1 (#105, `public_read`) scope-allowed `constructible_struct_adds_field` crate-wide because
  adding an opt-in field to a `pub` serde-deserialized `platform` config struct tripped the lint
  and, for a 0.x crate, forced a spurious major bump. That crate-wide allow also HID a genuinely
  breaking field-add to any OTHER `platform` struct, narrowing the semver gate. Every public
  serde-config struct in `platform::design` (`Design`, `CorsDesign`, `RateLimitDesign`,
  `RealtimeDesign`, `RealtimeTopic`, `Auth`, `ModuleDesign`, `Entity`, `Field`, `BelongsTo`,
  `Tenancy`, `JobDesign`, `StorageDesign`, `BucketDesign`, `Endpoint`, `RequestBody`, `Success`,
  `ErrorCase`) is now `#[non_exhaustive]`, so a downstream crate can no longer construct it with a
  struct literal — it must go through serde / the design contract. `#[non_exhaustive]` blocks only
  DOWNSTREAM literals; the defining crate still constructs its own structs by literal, so all
  in-crate construction (the migrator, defaults, tests) is unchanged and every design.json
  round-trips byte-identically. With the structs sealed, the `constructible_struct_adds_field =
  "allow"` is removed from both `crates/jerrycan/Cargo.toml` and `crates/jerrycan-realtime/Cargo.toml`
  (realtime needed no struct marked — its constructible specs are literal-built by generated code,
  and no field is being added, so the lint passes clean). Adding a field to any of these structs is
  now a clean non-breaking minor, and a genuinely breaking change is caught by the gate again.

## 0.6.35 — 2026-08-01

### Fixed
- **Nested-mount handlers auto-bind mount-inherited path params (#127).** A handler under a
  parameterized mount (`/accounts/{account_id}/…`) read only its OWN path (`{id}`), not the
  mount-inherited tokens, so a tenant grandchild's parent fk (`account_id`) — dropped from the
  request body (#82) but not bound as a `Path` — had to be hand-injected, and a NON-tenant
  param-mount child had no way to inject the dropped fk at all (uncompilable, a latent shape).
  `path_params`/`handler_params` now bind every RESOLVED-path param (`effective_mount + ep.path`)
  as a `Path`, EXCEPT the tenant fk that `Dep<Tenant>` already resolves (no double-bind). The
  steering comments are reconciled — a param-mount grandchild's parent fk comes from the PATH, the
  tenant from `Dep<Tenant>`. The previously-uncompilable non-tenant param-mount child now compiles
  under strict clippy (proven in genroute_compile).

Byte-identical for flat/top-level modules and direct tenant children (their resolved path adds no
new token, or the only token is the tenant fk already resolved by the guard).

## 0.6.34 — 2026-08-01

### Fixed (security)
- **An owner-scoped storage bucket now requires `owner_prefix` — JC0565 (#133).** A bucket
  WITHOUT `owner_prefix` shares ONE global key namespace across all owners, so `put_object`'s
  duplicate check leaks a cross-owner existence oracle (owner B uploading owner A's key gets a
  `409`, learning A has it) and lets owners squat each other's keys. `owner_prefix: true` is
  immune (keys are `{owner}/…` — path, unique index, and check are all naturally per-owner).
  The naive runtime fix corrupts data (the blob write lands at the shared path first) and the
  full path/index-scoping fix breaks the Supabase-parity global namespace, so the design-time
  refusal is the non-corrupting, non-breaking fix: **JC0565 refuses an owned bucket that lacks
  `owner_prefix`** — set `owner_prefix: true` for per-owner isolation, or drop the `owner` for
  an intentionally shared (Unowned) bucket. An Unowned bucket is unaffected. Requiring
  `owner_prefix` changes only the key layout, never read-visibility.

Byte-identical for any design whose scoped buckets already set `owner_prefix` (or have no owner).

## 0.6.33 — 2026-08-01

### Fixed
- **db tenant/owner repo templates are now `cargo fmt` fixpoints (#201, follow-up to #165).**
  #165 made `handlers.rs` + the memory `repo.rs` rustfmt fixpoints but scoped out the db
  tenant/owner membership-repo templates, so a fresh db-tenant scaffold's agent-owned
  `repo.rs` still failed `cargo fmt --check`: rustfmt greedy-fills a long
  `use jerrycan::db::sea_orm::{…}` import and wraps long membership/scoped method
  signatures (`create_for_memberships`, `update_for`, …). Those templates now pre-wrap
  exactly as the pinned rustfmt formats them (reusing #165's width-regime helper —
  width-gated, so short-name repos stay byte-identical). The #165 rustfmt round-trip test
  now covers db tenant/owner shapes (tenant-child + per-user owner, short and long names).

A fresh scaffold of any design — memory or db, tenant or owner — now survives
`cargo fmt --check` untouched.

## 0.6.32 — 2026-07-31

### Fixed
- **A fresh scaffold's agent-owned stubs are now `cargo fmt` fixpoints (#165).** The
  generated `handlers.rs`/`repo.rs` stubs were not rustfmt-clean, so `cargo fmt --check`
  (and the new app's first `jerrycan check`/CI fmt step) failed before the agent wrote a
  line — and JL0003-style drift would blame the agent for a file they never touched. The
  stub templates now pre-wrap exactly as the pinned toolchain's rustfmt formats them (the
  #128 convention — no runtime `cargo fmt` pass), covering the width-dependent cases (a
  short entity/op keeps its signature and error body on one line; a long one wraps). A
  rustfmt round-trip test proves fixpoint-ness across memory and db modes and both width
  regimes.

The generated stubs change (that is the fix); a re-`cargo fmt` of any fresh scaffold is
now a no-op.

## 0.6.31 — 2026-07-31

### Fixed
- **The Supabase migrator's public-read downgrade is now transitive (#144).** When a
  source table carries a public-SELECT policy, the migrator strips the public reads for
  a *tenant-owned* table (a public read would leak across tenants) and emits an advisory.
  That tenant-owned test checked only a **direct** `belongs_to` the tenant, while the
  framework classifies ownership **transitively** (`Design::tenant_path`, since #102) — so
  a *transitively* tenant-owned source (a grandchild) with a public-SELECT policy could
  slip past the check. The migrator now walks the `belongs_to` chain (cycle-guarded, the
  tenant root excluded) so its ownership view is as deep as the framework's. Currently
  unreachable via the released migration path (a latent inconsistency), so this is
  hardening; direct-child and unowned inputs are byte-identical.

## 0.6.30 — 2026-07-31

### Fixed
- **`jerrycan onboard` hardening (#136).** Two small robustness gaps from the onboard
  CLI review: (1) `upsert_block` (the AGENTS.md marker updater) fell through to its
  append arm on a *corrupted* marker pair — an orphan start/end, reversed order, or a
  duplicate pair — which could let a later run pair a stray marker with the new block's
  end and swallow the content between them; it now refuses with an actionable error
  instead of compounding a hand-corruption (the tool's own writes are always balanced,
  so this only guards a hand-edited file). (2) The `embedded_sync` CI tripwire guarded
  `docs/SKILL.md` against its `embedded/` copy but not against the Claude Code skill
  twin at `.claude/skills/jerrycan-backend/SKILL.md`; that pair is now guarded too, so
  editing one and not the other fails CI.

Tooling/test only — every generated app is byte-identical.

## 0.6.29 — 2026-07-31

### Fixed
- **JL0006's tenant-detail exemption is now signature-aware (#147).** `jerrycan check`
  exempts the tenant's own detail handlers from the unscoped-repo lint because their
  `Dep<Tenant>` guard already verified membership. That exemption was keyed on the
  handler's name alone, so two hand-edits slipped through green: dropping the
  `_tenant: Dep<Tenant>` guard from an exempt handler that still calls `repo.get(id)`,
  or binding a *child* repo as `repo` inside it. The exemption now also inspects the
  signature — it holds only when the handler binds BOTH a `Dep<Tenant>` guard AND a
  `repo: Dep<{Tenant}Repo>` (the tenant's own repo); otherwise JL0006 fires. `all()`
  stays armed regardless. The check only ever withdraws an exemption (never adds one),
  so a correctly written handler is unaffected. (Its acceptance-suite counterpart — a
  non-member-404 probe for the tenant root's own detail route — shipped in 0.6.27.)

Lint-only — every generated app is byte-identical.

## 0.6.28 — 2026-07-30

### Added
- **Per-route timeout overrides — `.handler_timeout()` / `.body_read_timeout()` (#111).**
  A route can now override the app-global handler-time budget and per-frame body-read
  deadline for itself, mirroring the existing per-route `.body_limit()`:
  `.route("/upload", post(h).stream_body().handler_timeout(Duration::from_secs(120)))`.
  A slow-but-moving large upload drains inside the handler, so the app-global
  `handler_timeout` (default 30s) used to `503` it (JC0503) — and the only escape was
  raising the budget app-wide in tool-owned `main.rs`, a permanent JL0003 trip. The
  per-route knob lives on the agent-owned route registration, so it does not trip
  JL0003. `None` (the default) keeps the app-global budget, so every route that does not
  opt in is unchanged.

Additive, byte-identical scaffolding — no generated route sets the new knobs.

## 0.6.27 — 2026-07-30

### Fixed
- **The tenant root's own detail route now gets a cross-tenant isolation probe (#172).**
  Child and grandchild tenant entities already got a generated acceptance probe proving
  a non-member gets `404` on another tenant's row by id. The tenant ROOT entity itself
  (e.g. `Workspace`) was skipped — the finder keys on `tenant_path`, which is `None` for
  the root — and the collection isolation test covers only the root's list, not its
  detail route. So a regression that dropped the membership guard on a **guarded**
  `GET /{root}/{id}` — leaking any tenant's root row to a non-member — passed every
  generated test. testgen now emits `a_non_member_cannot_read_the_{root}_detail` for a
  db+auth+tenancy design whose root module exposes a guarded `GET /{id}`: user 2 (a
  member of a different tenant) must get `404`. It is RED on the stub and on an unscoped
  `get`, green only when the route keeps its membership-checked `Dep<Tenant>` guard
  (which `404`s a non-member). A public root detail route is correctly skipped (it 200s
  everyone by design).

Byte-identical for every design without a guarded tenant-root detail route.

## 0.6.26 — 2026-07-30

### Added
- **Type-safe atomic capacity reservations — `reserve_against` (#187).** A field can
  declare `reserve_against: "<capacity_field>"` — the field is a counter, the named
  field its ceiling — and the generated `{Entity}Repo` gains a
  **`reserve(id, n) -> Result<bool>`** method that performs the reservation in ONE
  atomic conditional `UPDATE … SET used = used + n WHERE id = ? AND used + n <= capacity`
  (`Ok(true)` reserved, `Ok(false)` at capacity or no such row). This is the
  make-impossible successor to #108: an agent no longer hand-writes the reservation, so
  the read-capacity-then-write slip that silently oversells on Postgres cannot be
  written. Correct on SQLite **and** Postgres — all callers for a row contend on the
  same primary-key row, so the row lock plus the `WHERE` guard serialize them (proven by
  a live-Postgres concurrency test: a naive read-then-write oversells; `reserve` holds
  `used == capacity`). Identifiers in the generated SQL are quoted, so a counter or
  capacity named after a SQL keyword (`limit`, `order`) is safe.
- **`JC0564`** refuses a malformed `reserve_against`: a non-existent or non-integer
  capacity, a non-integer counter, either leg being the pk `id` or a nullable
  (`required: false`) column (a NULL makes the guard NULL — `reserve` would never
  succeed), a self-reference, more than one per entity, or a design without a database.

Byte-identical for every design that does not declare `reserve_against` (no method
emitted, no schema change).

## 0.6.25 — 2026-07-30

### Fixed
- **Anonymous clients can now reach public (scope-`none`) realtime topics when an
  auth model exists (#117).** The WebSocket upgrade ran the principal resolver with
  `?`, so a client with no/invalid credential was `401`'d at the *upgrade* — before
  any per-topic scope check. That made a public topic (e.g. an auction's price feed)
  unreachable by anonymous clients the moment the app had any auth, contradicting the
  scope model (`scope_allows(None, None) => Ok`). A resolver **authentication failure
  (401)** is now treated as an **anonymous connection** (`principal = None`) rather
  than a hard upgrade `401`; per-topic `scope_allows` still enforces access. A `None`
  principal reaches **only** scope-`none` topics — every scope-`auth`/`tenant` topic
  (and every changes/CDC topic) still rejects it at JOIN — so a bad credential
  accesses nothing an anonymous client couldn't. A genuine non-auth resolver error
  (e.g. a 5xx backend failure) still aborts the upgrade (no fail-open).

Byte-identical scaffolding — this is a `jerrycan-realtime` runtime fix; generated code
is unchanged.

## 0.6.24 — 2026-07-30

### Fixed
- **The last-admin guard is now race-free (#138).** Removing or demoting a tenant's
  sole admin was blocked by a count-then-act sequence (read the admin count, then a
  separate `DELETE`/`UPDATE`) — so two concurrent admin-gated writes could both pass
  the check and leave the tenant with **zero admins** (locked out of member
  management). On Postgres this was a write-skew race (the two writes touch different
  admin rows and the count read takes no lock); SQLite's single writer hid it. The
  generated `remove_member`/`set_member_role` now run one transaction that first locks
  the tenant's admin set (`SELECT … FOR UPDATE`, Postgres only — SQLite serializes on
  its single writer) and then the guarded write. Concurrent admin-gated writes now
  serialize; the sole admin can never be removed or demoted (still `409`), and a
  normal remove/demote/404/re-affirm are unchanged.

Byte-identical for any design without tenancy (no member surface).

## 0.6.23 — 2026-07-30

### Docs
- **Auth + tenancy doc improvements (#86).** `10-auth.md` gains a worked
  **session-login** example (matching the existing JWT one) and now states the
  **identity-fk convention** explicitly — the framework keys owner auto-omission on
  the literal `user_id` column, and an aliased `belongs_to` the identity entity is
  not the owner fk. `14-tenancy.md` mirrors the **unique-field-on-a-tenant-entity
  gotcha** (a `unique` field needs enough distinct values for the generated
  per-tenant seeds + create probe, or the acceptance suite is un-greenable — JC0552)
  that previously lived only in the skill docs.

## 0.6.22 — 2026-07-30

### Security
- **Flat tenant writes are now make-impossible (#97).** For a flat (Supabase-shape)
  tenant-owned entity, the unchecked bare repo methods (`insert`/`update`/`remove`/
  `all`/`get`) are no longer generated — only the membership-checked
  `*_for_memberships` accessors remain. Previously the bar was steer + lint (`JL0006`
  flagged a bare `repo.insert(body)` but the method still existed), so an agent could
  write an unchecked cross-tenant write. Now the leaky call cannot be written at all —
  parity with how per-user entities (#79) already close the leak by construction. A
  flat create reads its tenant fk from the request body, so it must go through
  `create_for_memberships` (which verifies the fk against the caller's membership set);
  the bare `insert` is suppressed.
- **A generated write-side isolation test (#96)** proves it: a member of tenant A gets
  **403** creating a row into tenant B.

Byte-identical for every non-flat entity (per-user, path-scoped, tenant root, non-tenant).

## 0.6.21 — 2026-07-30

### New
- **Rate limiting is now design-visible (#83).** A design may declare a
  `rate_limit` block — `{ "limit": 100, "window": "1m", "api_key_header"?, "trust_forwarded_for"? }`
  — and the generator wires the limiter into `main.rs`
  (`.extend(RateLimit::per_window(…))`) and enables the `rate-limit` facade
  feature. Previously rate limiting worked at runtime but had no contract surface:
  the only wiring path was hand-editing the tool-owned `main.rs`, which
  **permanently tripped `JL0003`** (the drift lint). Because the wiring is now
  generated, `main.rs` stays byte-identical and `JL0003` no longer trips. Over-limit
  requests get 429 + Retry-After; the partition defaults to the authenticated user
  then client IP (both unspoofable).
- **`JC0563`** refuses a malformed `rate_limit` — a zero `limit`, an unparseable
  `window`, or an invalid `api_key_header` name.

### Docs
- Corrected `06-middleware.md`: api-key partitioning is **off by default** (opt-in),
  not on, and an unauthenticated api-key header is client-spoofable — only partition
  by it when the key is validated before the limiter runs.

## 0.6.20 — 2026-07-30

### Fixed
- **A migrated jwt login can return its bearer token (#106).** The Supabase
  migrator pinned the login's success to `entity: User` (a `Json<User>` return),
  leaving nowhere for the token the login must mint. The login success now carries
  no entity (a bare 200), so the handler returns its own token response — the
  reference-slice login shape. (Register keeps its `User` success.)

### Notes
- The other half of #106 — the `workspace_members` write surface — was resolved by
  the #107 membership surface (0.6.0): a migrated tenancy now auto-generates the
  add/remove/list/set-role member-management endpoints at `/{tenant}/members`.

## 0.6.19 — 2026-07-30

### Docs
- **Concurrency & atomic reservations (#108).** Documented the per-backend write
  concurrency and the safe way to reserve a limited resource. The SQLite pool caps
  at 1 connection (single writer — writes serialize), so a *read-capacity-then-insert*
  reservation is accidentally race-free; on Postgres (real pool) the identical code
  **silently oversells**. The new section documents the pool sizing, warns about the
  read-then-insert trap, and gives the correct cross-backend pattern — a single
  atomic conditional `UPDATE … WHERE used + n <= capacity` (check the affected-row
  count: 1 = reserved, 0 = at capacity → 409). Backed by a concurrency test that
  proves the naive pattern oversells on Postgres while the atomic one reserves
  exactly capacity under load.

## 0.6.18 — 2026-07-30

Realtime security.

### Security
- **The realtime `changes` broadcast no longer leaks `write_only`/secret columns
  (#167).** The `changes` channel delivers the raw DB row, so `#[serde(skip_serializing)]`
  (the #112 REST-response mechanism) never applied — a `write_only`/`password_hash`
  column on a `changes` entity was broadcast to every WebSocket subscriber. The
  engine now **projects those columns out** of the broadcast row (at the single
  `deliver_change` delivery seam, covering both the WAL and trigger paths); the
  column is still stored and returned by nothing, and never reaches a subscriber.
- **`JC0555` is lifted.** The 0.6.8 interim refused a `write_only`/`password_hash`
  column on a `changes` entity by construction; with projection the combination is
  safe, so the restriction is removed — a `changes` entity may carry a `write_only`
  column again (it is simply never broadcast).

Byte-identical broadcast for any entity with no `write_only` column.

## 0.6.17 — 2026-07-30

### Fixed
- **An aliased `belongs_to` fk used as a path param types correctly (#178).**
  `path_param_key_type` matched a path param only against an entity's default fk
  column, so an aliased fk path param (`/{from_account_id}` for
  `belongs_to Account as from_account`) on a String/uuid-pk target fell through to
  `i64` → an E0308 mismatch. It now also matches the aliased fk column and
  resolves to the target entity's pk type. Byte-identical for un-aliased designs.

### Tests
- The two-references-to-one-entity fk-alias path now has a live create-probe
  proving the aliased-fk INSERT works end-to-end (two accounts + a transfer
  referencing both `from_account_id`/`to_account_id`), not just the migration SQL
  (#179).

## 0.6.16 — 2026-07-27

Classification-honesty cleanup — four review-filed residuals where the
validator/lint/steer classified an endpoint or entity differently from what
codegen targets, so a broken stub could ship behind a green `check`.

### Fixed
- **A public/unguarded write on a `public_read` entity is refused even when the
  entity isn't first in its module (#143).** The `JC0549(b)` write-gate resolved
  the endpoint's entity leniently (first-entity fallback), so a bodyless
  `public` `DELETE /{id}` on a non-first `public_read` entity mis-attributed to
  the first entity and escaped. It now fires when either the lenient or the
  strict (#56 collection-creator) resolution lands on the `public_read` entity.
- **An anonymous custom handler on a tenant-owned module no longer emits a
  broken scope comment (#171).** The membership-set steer referenced
  `_user`/`_id` params that an unguarded handler doesn't have; it's replaced by
  an honest TODO (the repo binding is unchanged).
- **`JC0562`** refuses a tenant entity reachable by **both** a flat (body-fk)
  write and a path-scoped route (#175) — the generator emits only one scoping
  shape, so the mixed shape would steer to a `*_for_memberships` method it never
  emits. Give the entity a single shape.

### Docs
- Note that in memory mode an absent optional field reads back as its type
  default (`0`/`""`), which may fall outside a `min`/`max` bound; db mode stores
  NULL (#161).

## 0.6.15 — 2026-07-27

### New
- **Non-entity (inline-DTO) request body (#122).** A custom-action endpoint whose
  body is not a table row can now declare it inline:
  `"request_body": { "fields": [ {"name":"coupon","type":"string"}, {"name":"total","type":"integer"} ] }`
  on `POST /checkout` generates a plain `CheckoutRequest` struct (named from the
  `operation_id`), the handler takes `Json<CheckoutRequest>`, and the shape flows
  into OpenAPI — instead of forcing a `probe: skip` and a hand-written DTO
  invisible to the generated contract. Inline fields honor the field constraints
  (#80). `request_body` now accepts an entity reference **or** inline `fields`
  (exactly one).
- **`JC0561`** refuses an unbuildable `request_body`: both an entity and inline
  `fields` (or neither), an inline body on an endpoint with no `operation_id`, an
  invalid inline field, or an inline `{Pascal(operation_id)}Request` name that
  collides with an entity's generated DTO or another inline body's (which would
  emit a duplicate struct / clobber the OpenAPI schema).

### Fixed
- **`probe: skip` TODOs are auth-aware.** In a design with no auth model, the
  skipped-endpoint TODO no longer emits credential/401 wording that doesn't apply.

Byte-identical for any `request_body` that references an entity.

## 0.6.14 — 2026-07-26

### New
- **`belongs_to` fk alias — two references to the same entity (#119).** A
  `belongs_to` may declare `"as": "from_account"`, making its fk column
  `from_account_id` instead of the hardcoded `account_id`. Two references to the
  same target now coexist — a ledger's `Transfer { from_account, to_account }`, a
  self-referential `Comment { parent }` — each with its own FK, `ON DELETE`, and
  scoped accessors, instead of falling back to a plain `i64` field that loses the
  integrity + scoping machinery. Each aliased fk gets a distinct DDL constraint
  name. `Design::fk_column` (the tenancy/identity fk) is unchanged.
- **`JC0560`** refuses an unbuildable alias: two `belongs_to` deriving the same fk
  column, an `{as}_id` colliding with a field or the pk `id`, a malformed `as`, or
  an `as` that lands on the **reserved** identity (`user_id`) or tenancy fk that
  the target doesn't own (which would hijack per-user/tenant scoping).

Byte-identical for any `belongs_to` that declares no `as`.

## 0.6.13 — 2026-07-26

### New
- **Composite / multi-column `UNIQUE` (#115).** An entity may declare
  `"unique": [["user_id", "post_id"]]` — a table-level composite unique over ≥2
  columns (each a field or a `belongs_to` fk column), emitted as a
  `CREATE UNIQUE INDEX`. A "one row per (a,b)" invariant — a like per (user,
  post), an enrollment per (user, course) — is now a **DB constraint**: a
  duplicate is a **409** (via the existing unique-violation mapping), not a racy
  SELECT-then-INSERT with a TOCTOU window. Single-column uniqueness stays
  `Field.unique`. The generated acceptance suite gets a composite-conflict 409
  test (isolated so only the composite index can trip it), and OpenAPI documents
  the 409.
- **`JC0559`** refuses an unbuildable composite `unique` group: fewer than 2
  **distinct** columns (use `Field.unique` for one column), a column that is
  neither a field nor a `belongs_to` fk column, or a duplicate group.

Byte-identical for any entity that declares no composite `unique`.

## 0.6.12 — 2026-07-26

### Fixed
- **Following the framework's own steer no longer produces a compile error (#116).**
  A tenant-owned handler stub steers the builder to call
  `create_for_memberships` / `update_for_memberships` / `remove_for_memberships`,
  but for a **flat tenant grandchild declared in a nested subroute** the repo
  emitted none of those methods — so following the generated guidance was a
  `method not found` behind a green `jerrycan check`. The emission gate
  (`entity_is_flat_tenant_owned`) scanned only the declaring module's top-level
  endpoints; it now walks all modules and nested subroutes with each endpoint's
  own module context, matching the steer's domain, so the (already-correct,
  #102 transitive-JOIN) membership methods are emitted wherever the steer
  references them. Byte-identical for every design without this shape.

Follow-up: a single entity carrying **both** a flat write and a path-scoped
route is a documented non-goal and now fails as a hidden method-not-found — a
loud refusal is tracked in #175 (no such design exists today).

## 0.6.11 — 2026-07-26

Auth authorization.

### Security
- **Anonymous reads on a tenant / tenant-owned entity are refused (#148, `JC0558`).**
  In an auth design, an endpoint on the tenant entity or a tenant-owned entity
  that is neither `auth_required` nor `public` was generated with **no**
  `Dep<Tenant>` guard and **no** `CurrentUser` — a fully anonymous handler — yet
  `jerrycan check` was green, so any caller could read (or, with a `public`
  mutation, write) **any tenant's rows by id**. Validation now refuses this shape
  (`JC0558`): set `auth_required: true` so the membership guard scopes it. This is
  the tenant twin of the per-user `JC0549(c)` check. Signature-authenticated
  webhooks (the Stripe pattern) and genuinely `public` routes are exempt. The
  refusal covers the tenant root's own reads and directly/transitively
  tenant-owned entities.

Follow-ups: an anonymous entity-less custom GET in a tenant-owned module still
receives a lenient tenant-owned repo binding (read-only, writes already blocked by
JL0004 — #171); a tenant-root non-member-404 acceptance probe (#172).

## 0.6.10 — 2026-07-26

### New
- **`"default": "now"` for server-set timestamps (#110).** A `datetime` field may
  declare `"default": "now"` — the server sets it to the current time (RFC3339
  UTC) on create; the field is omitted from **both** the create and update request
  bodies (server-owned and immutable after create) and is returned in responses.
  Previously a server timestamp (`created_at`, `applied_at`, `sent_at`) had no
  in-contract expression and was forced into a lossy `required: false` workaround.
- **`jerrycan::now_rfc3339() -> String`** — a dependency-free (no `chrono`)
  current-UTC RFC3339 helper, prelude-exported; the generated `default: "now"`
  handler steer points at it.
- **`JC0557`** refuses `"now"` on a non-`datetime` field (or a mis-cased near-miss
  like `"NOW"` on a datetime field).

Deferred: relative offsets (`"now+7d"`) and now-on-update (`updated_at`).

## 0.6.9 — 2026-07-26

Storage authorization.

### Security
- **Blob writes are role-gated (#132).** A tenant-scoped bucket can declare
  `"write_roles": ["admin", …]` — a member holding a role **not** in the set gets
  **403** on `upload`/`remove`. Previously the blob write handlers took a bare
  tenant guard with no role check, and a tenant-scoped bucket stamps every member
  as the owner, so a **read-only-role member could upload bytes and delete
  others' uploads**. `sign` (a signed *download* URL) and reads are unaffected.
  Backward-compatible: a bucket with no `write_roles` keeps today's behavior (any
  member may write).
- **`JC0556`** refuses a `write_roles` entry that isn't a declared member role,
  or `write_roles` on a non-tenant-scoped bucket (where the gate would be silently
  inert).

### Docs
- Storage: an **owned** bucket shares one key namespace across owners unless
  `owner_prefix: true` is set — use `owner_prefix` for per-owner key isolation
  (the #133 cross-owner key-collision mitigation; a runtime key-namespacing fix
  is tracked in #133).

## 0.6.8 — 2026-07-26

Security: response-hidden fields.

### Security
- **`write_only` fields (#112) — secrets no longer serialize by default.** A
  field marked `"write_only": true` — and any field named `password_hash`, which
  is auto-hidden (secure by default) — is accepted on create/update and stored,
  but **never appears in an API response**: the generated `Model` gets
  `#[serde(skip_serializing)]` (input and the DB layer are unaffected), and the
  OpenAPI schema marks it `writeOnly`. Previously every column serialized, so the
  docs' own accounts-api example and the Supabase migrator both shipped
  `password_hash` in responses. **Behavior change:** an existing design with a
  `password_hash` column (or a newly-`write_only` field) stops returning it on
  regeneration.
- **`JC0554`** refuses `write_only` on the primary-key `id` (the id must be
  returned).
- **`JC0555`** refuses a `write_only`/`password_hash` column on a realtime
  `changes` entity — the changes broadcast delivers the whole row, so the column
  would leak to WebSocket subscribers; the combination is refused until
  per-column projection lands (#167).

### Docs
- The accounts-api example marks `password_hash` `write_only` (and notes that
  responses omit it); the field reference documents `write_only`, the
  `password_hash` auto-hide, and `JC0554`/`JC0555`.

## 0.6.7 — 2026-07-26

Papercuts — four small correctness/consistency fixes surfaced by prior
whole-branch reviews.

### Fixed
- **Scaffold output is `cargo fmt`-clean for every app shape (#128).** The
  tool-owned `main.rs` (the OpenApi builder line, module/bucket mounts, and the
  CORS layer) and `migrations.rs` (`include_str!` paths) are now emitted as
  rustfmt fixpoints, so a later `cargo fmt` no longer rewrites a generated file
  the agent never touched and re-fires `JL0003` on it. (The CORS layer is emitted
  as a `let cors = …` builder preamble so it stays stable under any config.)
- **`JC0553` — an entity that collides with the generated membership surface is
  refused at `check` (#141).** With `tenancy`, an entity named `{Tenant}Member`
  or whose table resolves to `{tenant}_members` (e.g. `ClubMember` under tenant
  `Club`) now fails validation with a rename suggestion — previously it passed
  `check` and aborted mid-scaffold with an opaque `table already exists`.
- **`jerrycan_gen_tests` (MCP) no longer requires `module` (#159).** The bare MCP
  call now generates every endpoint module's acceptance suite plus jobs,
  mirroring the 0.6.4 CLI `gen-tests`; the two now share one code path.

### Internal
- **Drift tripwire (#129):** a test asserts `RESERVED_PRELUDE_IDENTS` stays a
  superset of `jerrycan::prelude`'s re-exports, so adding a prelude export without
  updating the reserved set now fails CI (guards `JC0546` against silent drift).

### Notes
- Follow-up: #165 (agent-owned route stubs aren't `cargo fmt`-clean on a pristine
  scaffold — cosmetic; outside `JL0003`'s tool-owned scope).

## 0.6.6 — 2026-07-24

Security.

### Security
- **jsonwebtoken 9 → 10.4.0 (#162, GHSA-h395-gr6q-cpjc).** Upgrades the JWT
  library past a type-confusion advisory that can lead to an authorization
  bypass. jsonwebtoken 10 removed its built-in `ring` backend, so jerrycan-auth's
  `idtoken` (OAuth id-token / JWKS) verifier now installs its own **RS256-only
  crypto provider backed by `ring`** — the crypto backend already in the tree via
  rustls — which keeps the pure-Rust `rsa` crate (and the RUSTSEC-2023-0071
  Marvin timing advisory) off the compiled path. RS256 stays pinned in three
  independent layers (the JWS header `alg`, the `Validation` algorithm allowlist,
  and the provider itself); non-RS256, `alg:none`, and an HMAC alg keyed with the
  RSA public key are all rejected.
- **The id-token `aud` claim is now required to be present** (previously it was
  only checked when present) — a validly-signed token that omits its audience is
  rejected (401).

### Note for embedders
- jerrycan-auth installs jsonwebtoken 10's process-global crypto provider on the
  first use of the `idtoken` verifier. An application that *also* uses the
  `jsonwebtoken` 10 crate directly and calls `decode`/`encode` **before** any
  jerrycan verifier exists — with neither the `aws_lc_rs` nor `rust_crypto`
  backend feature enabled — will seed jsonwebtoken's provider with a panicking
  placeholder (a loud crash, never an unsafe accept). Construct a jerrycan
  verifier, or enable a jsonwebtoken backend feature, before such direct use.

## 0.6.5 — 2026-07-24

The #1 contract gap by eval demand: **field length/range constraints**.

### New: `min` / `max` / `min_len` / `max_len` field keys (#80)
A field may now declare validation bounds:
- `min` / `max` — inclusive integer range (integer fields).
- `min_len` / `max_len` — inclusive string length in **Unicode code points**
  (string fields).

The generator emits a deserialize-time validator that rejects an out-of-range
value with **422**, carries the bound into the OpenAPI schema
(`minimum`/`maximum`/`minLength`/`maxLength`) and the migration `CHECK`, and
testgen derives an in-range fixture **plus** an out-of-range 422 probe — so a
constrained design is **green end-to-end on correct handlers with zero
hand-written validation**. Previously every such field forced a hand-written
`Valid` impl (and a `probe:"skip"` that dropped sibling probes — the #1 repeated
hand-work across the eval program). Opt-in and additive: absent the keys, every
existing design generates byte-identical output.

### Validation
- **`JC0552`** refuses malformed constraints: a bound on the wrong field type,
  `min > max` / `min_len > max_len`, length keys combined with `values`, any
  constraint on the pk `id`, `max_len: 0` on a required field, an out-of-bounds
  `default`, and a `unique` field whose range admits fewer than 3 distinct values
  (the test harness seeds up to 3 distinct rows — widen the range or drop
  `unique`).

### Docs
- `docs/ai/00-designing.md` documents the four keys and rewrites the
  `probe:"skip"` guidance — length/range are now declarable (email/url/regex
  remain the documented `skip` case).

### Notes
- v1 covers integer + string scalar bounds. Deferred (tracked): `float` ranges,
  regex/pattern + email/url formats, and structured multi-violation error
  details (activating the `Valid<T>` machinery). Follow-up #161 (memory-mode
  optional-default divergence).

## 0.6.4 — 2026-07-23

Completes the 0.6.3 "gate honesty" coverage.

### Security / correctness
- **Full 401-guard coverage (#153).** A guarded endpoint now *always* generates
  its `<op>_without_auth_is_401` test. 0.6.3 fixed the `probe:"skip"` case; this
  closes the two remaining branches that silently dropped it — a `/{id}` detail
  route with no seed creator, and a multi-parameter endpoint. **Behavior change:**
  such designs gain a 401 test on regen (`expected_failing` +1); it passes on a
  correct app and turns red only where a guard was hand-weakened.
- **Jobs hollow-green closed (#156).** `JC0551` now also fires when a design
  declares cron jobs but has no `crates/jobs/tests/acceptance.rs` — a jobs-only
  app used to read `ok:true` with zero tests. Same file-existence signal and fix
  (`jerrycan gen-tests`) as the endpoint-module check.

### CLI
- **`jerrycan gen-tests` no longer requires `--module`.** The bare command now
  generates the acceptance suite for every endpoint-bearing module *plus* the
  jobs suite; `--module <name>` still targets a single module (byte-identically).
  This also makes the `JC0551` jobs diagnostic's suggested fix runnable for a
  module-less, jobs-only design.

### Notes
- Follow-up: #159 (the MCP `jerrycan_gen_tests` twin should also accept an
  optional module).

## 0.6.3 — 2026-07-23

Gate honesty: every change makes a guarantee the framework *claims* actually
hold or be proven. "Green means safe."

### Security / correctness
- **Honest `check` (#123a) — `JC0551`.** `jerrycan check` (and the MCP `check`
  tool and the `package` gate) now refuses with **`JC0551`** when a module that
  has endpoints has no generated acceptance tests. A freshly-scaffolded app that
  never ran `gen-tests` used to read `ok:true` with **zero tests** — a hollow
  green. **Behavior change:** such apps now flip green→red; the fix is the
  already-documented `jerrycan gen-tests` step (the scaffold's `next_step`
  already orders it before `check`). A gen-tested app — even one whose endpoints
  are all TODO stubs — stays green.
- **`probe:"skip"` keeps the auth-guard test (#123b).** Marking an endpoint
  `probe:"skip"` no longer also deletes its `<op>_without_auth_is_401` negative
  test for a **guarded** endpoint — a passing security assertion was being
  silently dropped along with the skipped happy-path probe. **Behavior change:**
  a guarded `probe:"skip"` endpoint gains a `_without_auth_is_401` test on regen
  (its `expected_failing` count rises by one); on a correct app the new test
  passes, and it turns red only where a guard was hand-weakened.
- **SQLite FK enforcement pinned & proven (#121).** `Db::connect` now sets
  SQLite `foreign_keys=ON` explicitly (via sea-orm's `map_sqlx_sqlite_opts`)
  instead of relying on the sqlx default, and a new test proves foreign-key
  rejection + `ON DELETE CASCADE` through the connection pool. (Enforcement
  already worked via sqlx's default — this makes the guarantee explicit and
  CI-proven, so a future upstream default change can't silently disable it.)

### Docs
- **The auth identity entity must be named `User` (#123c).** Owner-scoping, the
  server-injected identity fk, and `public_read` all key on the literal derived
  column `user_id`; `docs/ai/10-auth.md` and `14-tenancy.md` now state this
  plainly and drop the misleading "typically a `User`/`Account`" phrasing — an
  identity named `Account` (fk `account_id`) silently gets **no** owner-scoping
  and a client-writable fk. A future opt-in `auth.identity` is tracked (#150).

### Dependencies
- `sea-orm` requirement floor raised to **`1.1.17`** (the version that introduced
  `map_sqlx_sqlite_opts`, used for the explicit SQLite FK pin). A consumer that
  co-pins `sea-orm` below 1.1.17 (e.g. `=1.0.x`) will no longer resolve with
  jerrycan-db 0.6.3.

### Notes
- Follow-ups filed: #150 (generalize owner-scoping via an opt-in `auth.identity`),
  #152 (Supabase-migrate capstone hollow-green), #153 (complete the 401-guard
  test for the non-`skip` no-creator/`{id}` and multi-param branches), #156
  (a `JC0551` sibling for jobs-only designs).

## 0.6.2 — 2026-07-22

Closes the residual tenant-guard gaps left by the #78 ownership-safety effort
(the core membership-query tenant filter landed in 0.5.0). A
correctness/security patch — every existing design generates byte-identical
output.

### Security / correctness (#88, #89, #124)
- **`JC0550`** (#88) refuses a tenant entity's own detail route addressed by a
  non-pk param (e.g. `/{slug}`) instead of silently generating it with **no
  membership check**. The guard verifies the tenant named by the path fk, so a
  non-pk param cannot be membership-checked; the conventional `/{id}` still
  auto-normalizes to `/{fk}`, and an explicit `/{fk}` passes. (Renaming a
  slug→fk was rejected as unsafe — the guard parses the path value as the pk
  type.) The fk is matched against the mount-resolved path, so a mount-carried
  fk is not falsely refused.
- **Detail-route normalization** (#89) now targets only the tenant entity's own
  routes — a sibling entity's `/{id}` in the same module is no longer
  mis-renamed to the tenant fk (resolved in an immutable pre-pass).
- **JL0006** (#124) attributes each unscoped-repo-call flag to its enclosing
  handler and exempts the tenant's own path-verified, **guarded** detail
  handlers (which legitimately call unscoped methods on the
  already-membership-verified tenant repo), while keeping `repo.all()` armed
  everywhere and Collection, child, and **unguarded** handlers fully armed. A
  false positive on correct code becomes silent; a real leak stays flagged. The
  exemption uses the strict repo-entity resolver and requires the endpoint to be
  guarded — it under-exempts by design (residual false positives keep the
  `// jerrycan:allow JL0006` hatch; over-exempting would silence a leak).

### Notes
- Follow-ups filed: #147 (make the JL0006 exemption signature-aware), #148
  (require the guard on tenant / tenant-owned GETs in auth designs).

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
