# Designing (the design.json contract)

## Purpose
`design.json` is the single source of truth a jerrycan backend is generated from.
You AUTHOR it first, then `jerrycan new <name> --design design.json` scaffolds a
workspace from it (validate-only runs inside `new`; there is no separate `validate`
subcommand). This page is the complete schema — every field, rule, and gotcha — so
you can write a valid design without round-tripping through the validator. Worked,
copy-ready designs live in `jerrycan docs designing-examples`. The formal contract is
`docs/contracts/design-schema.json` (JSON Schema 2020-12); the rules below are exactly
what the generator enforces.

Validation returns a list of pointed "questions" keyed by JSON pointer (e.g.
`/modules/0/endpoints/1/required_roles`); an empty list means the design is complete.
Fix every question before scaffolding.

## Top-level
```json
{
  "name": "my-api",
  "contract_version": 1,
  "description": "optional one-liner",
  "auth": { "model": "session", "roles": ["admin"] },
  "dependencies": ["db", "auth"],
  "tenancy": { "entity": "Workspace", "member_roles": ["owner"] },
  "jobs": [{ "name": "expire_trials", "schedule": "0 * * * *", "queue": "billing" }],
  "modules": [ /* … */ ]
}
```
- `name` (REQUIRED) — kebab-case, `^[a-z][a-z0-9-]*$`.
- `contract_version` (REQUIRED) — an integer, `0`, `1`, or `2`. Use `1` for standard
  apps: v1 unlocks `belongs_to` relations, enum `values`, and real `json` columns in
  db mode. `2` additionally unlocks storage buckets and realtime channels (required
  when the design uses either). `0` is the legacy in-memory contract (a `json` field
  is rejected under db mode on v0). Only `> 2` is rejected.
- `description?` — free text.
- `base_path?` — app-level mount prefix applied once to every module and bucket mount,
  e.g. `"/v1"` serves all routes under `/v1`. Health (`/healthz`) and metrics
  (`/metrics`) stay unprefixed. Empty/`/`/absent is a no-op; must be an absolute path
  (leading `/`, no trailing slash).
- `auth?` — `{ "model": "none" | "session" | "jwt", "roles": [string] }`. `model` is
  REQUIRED inside the block. A non-`none` model activates auth-mode generation
  (Session/Bearer guards, `require_role`) and implies the `auth` dependency. `roles` is
  the role NAMESPACE that endpoint `required_roles` draws from.
- `dependencies?` — array of capability names. The RESERVED names (each toggles real
  generation):
  - `db` — SQL storage (SeaORM entities, migrations, `Db` dependency).
  - `validate` — serves the OpenAPI document at `/openapi.json`.
  - `auth` — session/JWT guards + the `Auth` extension (also implied by a non-`none`
    `auth.model`).
  - `observe` — access-log middleware, `/healthz`, and Prometheus `/metrics`.
  - `oauth` — enables `jerrycan::auth::oauth::{OAuthClient, Provider}`; implies `auth`.

  Any OTHER name is a STUB: the generator reminds you to wire it; it generates nothing
  on its own.
- `tenancy?` — `{ "entity": "Workspace", "member_roles": [string] }`. `entity`
  (REQUIRED in the block) must be a declared entity (PascalCase). See Tenancy.
- `jobs?` — `[{ "name", "schedule"?, "queue"? }]`. Background jobs; require the `db`
  dependency. See Jobs.
- `modules` (REQUIRED) — at least one. The resource areas of the backend.

## Module
```json
{
  "name": "leads",
  "mount": "/leads",
  "description": "optional",
  "entities": [ /* … */ ],
  "endpoints": [ /* … */ ],
  "subroutes": [ /* nested modules … */ ],
  "dependencies": ["something"]
}
```
- `name` (REQUIRED) — kebab-case, unique among top-level modules.
- `mount?` — defaults to `"/" + name`. Must start with `/`, no `//`, no trailing
  slash. A param-carrying mount like `/{ws}` is allowed (it captures a parent param for
  subroutes).
- `description?` — free text.
- `entities?` — the data shapes this module owns (see Entity). A module can have zero
  entities (e.g. a webhook module).
- `endpoints` (REQUIRED) — at least one (see Endpoint).
- `subroutes?` — nested modules, mounted UNDER this module's mount. Every module rule
  applies recursively at any depth.
- `dependencies?` — module-scoped stubs. `db`/`validate` only take effect at the TOP
  level; here they are ordinary stubs.

## Entity
```json
{
  "name": "Lead",
  "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
  "fields": [ /* … at least one … */ ]
}
```
- `name` (REQUIRED) — PascalCase, `^[A-Z][A-Za-z0-9]*$`, and NOT a Rust keyword (it
  becomes a Rust type).
- `belongs_to?` — array of `{ "entity": "<PascalCase>", "on_delete": "cascade" | "set_null" | "restrict" }`.
  - The target entity must exist SOMEWHERE in the design (any module/subroute) —
    cross-module references are allowed.
  - `on_delete` defaults to `"restrict"`. `set_null` makes the fk column nullable.
  - The fk column is DERIVED as `snake_case(entity) + "_id"` (`Workspace` →
    `workspace_id`). Do NOT also declare an explicit field of that name — it collides
    (db mode).
  - A SAME-module relation gets a real DB `FOREIGN KEY` and the `on_delete` is enforced
    by the database. A CROSS-module relation gets only an indexed fk column (no DB
    constraint); `on_delete` is then YOUR handler's job, not the DB's.
- `public_read?` — defaults to `false`. The **public-read / owner-write** switch — the
  feed/blog/listing/job-board shape: anyone (even anonymous) reads, only the owner
  writes. Valid ONLY on a per-user identity-owned entity (a `belongs_to` the auth
  identity, NOT tenant-owned, in an auth design) — anything else is rejected with
  `JC0549`. When `true`:
  - **Reads (GET list + detail) are PUBLIC.** The generated handler takes no
    `CurrentUser` — even if the endpoint declares `auth_required` (the entity flag
    drives the guard split) — the repo keeps the unscoped `all()`/`get()`, the OpenAPI
    operation carries no `security` stanza, and no 401 test is generated for the reads.
    A public list returns the **whole collection** — every owner's rows.
  - **Writes stay owner-scoped exactly as without the flag:** guarded, server-injected
    `user_id` on create, `update_for`/`remove_for` keyed on the session user (a
    non-owner's update/delete → 404, hiding existence). A write endpoint that is
    `public` or unguarded is rejected (`JC0549`). The generated isolation test proves
    the whole contract: anon read 200 (containing another user's row), anon write 401,
    non-owner write 404 with the row surviving, owner write 200.
  - A GET with `required_roles` **keeps its guard** — an explicit role demand outranks
    the entity-level read-open default.

  Without the flag, an unguarded (or `public: true`) GET on a per-user entity is
  rejected as unimplementable (`JC0549`): the owner-scoped repo has no unscoped read
  and the handler no session user. The fork is: set `public_read: true`, or keep the
  GET authenticated.
- `fields` (REQUIRED) — at least one (see Field).

The SQL **table name** defaults to `snake_case(entity)`, pluralized — `Ticket` →
`tickets`, `Workspace` → `workspaces`, `ApiKey` → `api_keys`, `EnergySummary` →
`energy_summaries` (ordinary English: consonant + `y` → `ies`; ends in
`s`/`x`/`z`/`ch`/`sh` → `es`; else `+s`). A multi-word entity's table shares the
snake_case stem of its fk column (`ApiKey` → table `api_keys`, fk column `api_key_id`).
Set `"table": "…"` on an entity to override the name verbatim (a frozen external
schema). You need the exact table name only for hand-written cross-module SQL (the
generated repo handles intra-module access).

### The `id` field (primary key)
Every entity has an `id` primary key; you usually do NOT declare it:
- **Omit `id`, or declare it as `integer`** → the generator synthesizes an
  auto-increment integer PK (`BIGINT AUTOINCREMENT` / `BIGSERIAL`), no duplicate
  column; the Rust key type is `i64`. This is the default and what you want most of the
  time.
- **Declare `id` as `string` or `uuid`** → that becomes the PK column (TEXT, no
  auto-increment) and the Rust key type is `String`. Use this for client-supplied or
  externally-issued ids; your handler must set it.
- A declared `id` of any OTHER type (float/boolean/datetime/json) is REJECTED in db
  mode — a PK must be integer, string, or uuid.

## Field
```json
{ "name": "status", "type": "string", "required": true, "unique": false, "index": false, "values": ["draft", "active"] }
```
- `name` (REQUIRED) — snake_case, `^[a-z][a-z0-9_]*$`. A Rust keyword (`type`, `match`,
  `ref`, …) is allowed: codegen emits it as a raw identifier (`r#type`) with a serde
  rename so the wire and column names stay unchanged. Only `self`/`crate`/`super`,
  which no raw identifier can escape, are rejected.
- `type` (REQUIRED) — one of seven: `string`, `integer`, `float`, `boolean`,
  `datetime`, `uuid`, `json`. Their Rust types:

  | `type` | Rust type |
  |---|---|
  | `string` | `String` |
  | `integer` | `i64` |
  | `float` | `f64` |
  | `boolean` | `bool` |
  | `datetime` | `String` (no native time type yet) |
  | `uuid` | `String` (no native uuid type yet) |
  | `json` | `serde_json::Value` |

- `required?` — defaults to `true`. `false` makes the column nullable / the Rust field
  `Option<T>`.
- `unique?` — defaults to `false`. Adds a UNIQUE constraint; a duplicate insert
  surfaces as `409 JC0409`.
- `index?` — defaults to `false`. Adds a non-unique index.
- `values?` — the ENUM mechanism. ONLY valid on a `string` field, and must be
  non-empty. A `string` field + `values` becomes a TEXT column with a CHECK constraint
  AND generated request validation: the model rejects an out-of-range value at
  deserialization, so invalid enum input is answered `422 JC0422` before it reaches the
  database — on every write path (create AND update), and the same 422 in memory mode
  (which has no DB). There is NO separate `enum` field type — enums are always `string`
  + `values`.
- `min?` / `max?` — integers; an INCLUSIVE integer range; `integer` fields ONLY (use
  `min_len`/`max_len` to bound a string). Either bound may stand alone. Declaring one
  generates the full stack: request validation at deserialization (an out-of-range
  value is answered `422 JC0422` before the handler and the database, on create AND
  update, and the same 422 in memory mode), OpenAPI `minimum`/`maximum`, a db-mode
  `CHECK` constraint, an IN-RANGE happy-path fixture in the generated tests, and a
  `{op}_rejects_out_of_range_{field}` 422 probe. An empty range (`min > max`) is
  rejected (`JC0552`).
- `min_len?` / `max_len?` — non-negative integers; an INCLUSIVE string-LENGTH range
  counted in Unicode CODE POINTS (`chars().count()`, matching JSON Schema
  `minLength`/`maxLength` — NOT bytes); `string` fields ONLY, and NOT combinable with
  `values` (the enum already fixes the exact allowed strings — `JC0552`). Generates
  the same stack as `min`/`max`: the 422 at the boundary, OpenAPI
  `minLength`/`maxLength`, a db-mode `CHECK` on `length(col)`, the in-range fixture,
  and the 422 reject probe. `min_len` is capped at 4096 (generated fixtures
  materialize a minimum-length value); `min_len > max_len` and `max_len: 0` on a
  required field are rejected (`JC0552`).
- Constraint rules (each refused with `JC0552` at design time): no constraint key on
  the pk `id` (ids are server-assigned; the generated probes and seeds assume them
  free); a `default` must satisfy its own field's constraints; and a `unique` field
  must leave ROOM — the generated test harness seeds up to 3 DISTINCT in-range values
  per field (the request fixture plus two seeds), so a `unique` integer whose range
  admits fewer than 3 values is refused. An ABSENT bound counts as its i64 extreme,
  so a lone `min: 9223372036854775806` is refused too; a `unique` string with
  `max_len: 0` is the same case. Widen the range or drop `unique`.
- `default?` — a SERVER-OWNED default. A field with a `default` is dropped from the
  generated request DTO / OpenAPI request schema / happy-path probe body — the client
  never sends it, the SERVER supplies it — yet it stays **required NOT-NULL** in the
  entity and the table (the default is a wire concern, not a schema-nullability one), so
  the create handler must write the declared value (the generated stub names each one).
  This expresses `confirmed` (`boolean`, default `false`) or `status` (`string` enum,
  default `"active"`) WITHOUT forcing a clean client to POST a server-controlled key.
  The value must type-check against `type` (and be a member of `values` when present),
  and the design must depend on `db` — the default is applied through the db-mode
  request DTO, so it is inert (a validation error) in memory mode. It is NOT
  `required: false`, which would make the column nullable (`Option<T>`) with value
  `null`, not the default; `default` keeps a solid NOT-NULL column with a server-chosen
  fallback.
- `write_only?` — defaults to `false`. A RESPONSE-HIDDEN field: it is accepted on
  create/update input and stored, but emitted with `#[serde(skip_serializing)]` on the
  generated model so it NEVER appears in an API response (the request DTO and OpenAPI
  request schema KEEP it — input is unaffected; OpenAPI marks the property `writeOnly`).
  A `password_hash` column is AUTO-hidden even without the flag — secure-by-default,
  fail-closed, since a password hash must never be in a response; the broad
  `*_hash`/`token`/`secret` name heuristic is deliberately NOT applied (a `share_token`
  may legitimately be returned), so mark those `write_only` explicitly. `write_only` on
  the pk `id` is refused (`JC0554` — the id must be echoed in every response). A
  `write_only`/`password_hash` column may NOT be on a realtime `changes` entity
  (`JC0555`): the changes broadcast ships the raw row over the WebSocket, so the column
  would leak to subscribers despite the response hide — remove it from the entity or
  drop the entity from `changes` (lifted once column projection lands, #167).

## Endpoint
```json
{
  "operation_id": "create_lead",
  "method": "POST",
  "path": "/{id}/leads",
  "auth_required": true,
  "required_roles": ["admin"],
  "public": false,
  "request_body": { "entity": "Lead" },
  "success": { "status": 201, "entity": "Lead", "list": false },
  "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
}
```
- `operation_id` (REQUIRED) — snake_case; becomes the handler fn name; unique within
  its module.
- `method` (REQUIRED) — `GET` | `POST` | `PUT` | `PATCH` | `DELETE` (UPPERCASE). The
  `(method, path)` pair must be unique in the module.
- `path` (REQUIRED) — starts with `/`. Path params are `/{name}`; 1–3 params per route,
  braces balanced. The leaf `/{id}` param is what a `Path<Key>` binds.
- `auth_required?` — defaults to `false`. Requires an active auth model.
- `required_roles?` — array of role names. **CRUCIAL: these are the `auth.roles`
  namespace, NOT `tenancy.member_roles`.** Every role here must be declared in
  top-level `auth.roles`. A non-empty `required_roles` implies `auth_required`.
  (Membership-role gating — owner/member of a tenant — is NOT done here; do it
  in-handler via `tenant.require_role(...)`. See Tenancy.)
- `public?` — defaults to `false`. Marks the route unauthenticated by design (exempt
  from the auth lint and the generated 401 test). It CANNOT combine with `auth_required`
  or `required_roles` (each is a distinct error). It also cannot live in a module whose
  entity is **tenant-owned**: the generator binds an endpoint to its module's entity, so
  a `public` endpoint there (even one with no `request_body` of its own) would bypass
  the Tenant guard and expose one tenant's rows. **Put public endpoints — webhooks, an
  inbound-ingest route, a login/register — in their OWN module** that has no
  tenant-owned entity (entity-less is fine). For "anyone READS this entity, only its
  owner writes" (feeds, posts, listings), do NOT mark the GETs of an owner-scoped
  entity `public` — that is rejected as unimplementable (`JC0549`); set
  `public_read: true` on the entity instead (see Entity above).
- `probe?` — `"auto"` (default) or `"skip"`. `skip` tells the generator NOT to emit the
  happy-path 2xx probe for an endpoint whose success needs a credential it can't
  synthesize (login, signed webhook, api-key route) — otherwise that probe is
  un-greenable and `jerrycan check` can never reach `ok:true`. With `skip` the generator
  emits an AGENT TODO; you write the credentialed success test yourself. A guarded
  endpoint (`auth_required`/`required_roles`) STILL gets its generated
  `_without_auth_is_401` test — `skip` drops only the success probe, never the
  auth-guard assertion. An unguarded `skip` endpoint (a login, a signed webhook) has no
  generated guard test, so write its 401/403 rejection test yourself too.
- `request_body?` — `{ "entity": "<Name>" }` ONLY. The body is the named entity; the
  entity must be declared in THIS module. There is no narrower/custom input DTO in the
  design — for an endpoint that takes untrusted public input, defend it IN-HANDLER
  (parse a hand-written input type, validate, then map to the entity).
  - **Server-owned fields → a `{Entity}Request` DTO.** When the body entity has a field
    the SERVER owns, the generated body type is a trimmed `{Entity}Request` DTO (the
    OpenAPI request schema and the happy-path probe body drop the same fields); the
    entity RESPONSE shape is unchanged. Three drop reasons — a body can hit several at
    once:
    1. **Identity fk (guarded):** the body `belongs_to` the auth identity entity —
       which MUST be named literally `User`, so the derived fk is `user_id` (see
       10-auth.md: an identity named anything else gets NO owner-scoping and its fk
       stays client-writable) — AND the endpoint is guarded → `user_id` is omitted; the
       handler injects the session user's id. An unguarded endpoint keeps it (no session
       to inject).
    2. **`default` field:** any field with a `default` is omitted; the server applies
       the declared value. Works on unguarded/public creates too — this is what lets
       `POST /subscribers { "email": … }` succeed while `confirmed` and `status` default
       server-side.
    3. **Path-redundant parent fk:** if the entity `belongs_to` a parent and the create
       route already carries that parent's id as a path param whose name equals the fk
       column (`Checkin belongs_to Habit` + `POST /{habit_id}/checkins`), `habit_id` is
       omitted; the handler injects the path value, so the row attaches to the path's
       parent. Every other `belongs_to` fk stays required client input.
- `success` (REQUIRED) — `{ "status": <2xx-or-3xx>, "entity"?: "<Name>", "list"?: bool }`.
  - `status` must be in 200–399 (2xx OR 3xx — 3xx is valid for redirects like an OAuth
    connect).
  - `entity?` (must be declared in this module) + `list?` shape the response: `entity` +
    `list:true` → `Json<Vec<Entity>>`; `entity` + `list:false` → `Json<Entity>`; `201` +
    `entity` → `Created<Entity>`; `204` → `NoContent`.
  - A success with NO `entity` (and not 204/201) generates
    `Result<Json<serde_json::Value>>` — a custom shape you HAND-WRITE. (`201` with no
    entity → `Created<serde_json::Value>`.)
- `errors?` — `[{ "status": <4xx-or-5xx>, "code"?: "JC####", "when": "<text>" }]`.
  `status` is 400–599; `code` (optional) must match `^JC[0-9]{4}$`; `when` is REQUIRED
  prose. **Only a `404` on a single-`{id}` path is auto-tested** (the generator emits a
  missing-id test). Every other declared error becomes an `// AGENT TODO` in the
  generated acceptance test for you to encode.

> **Un-greenable success probes (expected — don't fight them).** The generator emits
> one happy-path test per endpoint that posts a MINIMAL body with **no credential, no
> signature, no API key**, and asserts `success.status`. For an endpoint whose success
> genuinely REQUIRES a credential — a `login` that 401s bad creds, a signed webhook that
> 401/400s a bad signature, an API-key-gated route — that 2xx probe is **un-satisfiable
> by construction**, so `jerrycan check` will not be fully green for it. That is normal
> and correct: **do NOT weaken the handler to make the probe pass.** Leave the probe
> red, and cover the real behavior (the 200 WITH a valid credential, and the 401/400
> without) in your own agent-owned test. See `jerrycan docs testing`.
>
> **A length or range rule is NOT this case — declare it, don't skip.** The contract
> expresses bounds directly: `min`/`max` on an integer field, `min_len`/`max_len` on a
> string field (see Field above). Declare them and the generator does the rest — it
> derives an IN-RANGE happy-path fixture (a `string` gets `"test-value"` fitted into
> the declared length window, an integer is clamped into `[min, max]`) plus a
> `{op}_rejects_out_of_range_{field}` 422 probe, so the endpoint stays fully greenable
> with ZERO hand-written validation. Never hand-write a `Valid` impl (or reach for
> `probe: "skip"`) for a plain length/range bound.
>
> **A hand-written FORMAT constraint the generator can't see is still the skip case.**
> The probe fills each field with a generic fixture — a `string` gets `"test-value"`;
> declared `uuid`/`datetime` get format-valid fixtures and enum `values` get a declared
> value. But the design contract has **no field-format declaration** (no email/url/
> regex/pattern), so a `string` the handler additionally requires to be an **email,
> URL, or other format** — enforced by a hand-written `Valid` impl the design can't
> express — is rejected by a CORRECT handler, and that endpoint's 2xx probe is
> un-greenable too. Mark such an endpoint **`probe: "skip"`** and write its success
> test with a real value yourself.
>
> **A module with endpoints requiring DIFFERENT roles is a partial case.** The generated
> credential carries ONE role, drawn from the design's role gate (the first
> `required_roles` a handler will demand, else the first declared `auth.roles`). If two
> guarded endpoints in the SAME module require **different** roles, that single credential
> satisfies only one — a CORRECT `require_role` handler 403s the other's happy-path probe.
> Cover the un-satisfied one in your own test with a credential minting that role.
>
> **A cross-module `belongs_to` parent that a handler checks exists is the same case.**
> A create/update body carries an fk to a parent in **another module** (a cross-module
> `belongs_to` — no DB FK, so the generator can't auto-seed it). The fixture uses `1`, but
> a CORRECT handler that verifies the parent exists rejects it (that parent row was never
> seeded). Mark the endpoint **`probe: "skip"`**, seed the parent yourself, and test it.

## Worked examples
`jerrycan docs designing-examples` ships seven complete, copy-ready designs — each
validated and scaffolded by the test suite: a minimal app · a CRUD module (fields +
enum + unique + index) · relations (`belongs_to` + `on_delete`) · first-class tenancy ·
auth + a public route · jobs/cron · a signed webhook. Lift one as a starting point.

## Gotchas (these cost real time)
- **Enums are `string` + `values`, not a type.** There is no `enum` field type. `{
  "type": "string", "values": ["a", "b"] }` → TEXT + CHECK + a generated request
  validator (an out-of-range value 422s at deserialization, before the DB). `values` on
  a non-string field is rejected; an empty `values` is rejected.
- **`datetime`/`uuid` are `String` at the Rust layer.** There is no native time type and
  no built-in `now() → rfc3339`. Handle time in-handler (format/parse a string
  yourself); the column is TEXT.
- **`required_roles` ≠ membership roles.** `required_roles` draws from `auth.roles`
  ONLY. Tenant membership (owner/member) is checked in-handler via the Tenant guard,
  never through `required_roles`.
- **Cross-module `belongs_to` gets NO scoped `*_for` accessors and NO DB FK.** Only an
  entity that `belongs_to` the tenancy entity gets scoped repos. A grandchild reached
  across modules must be tenant-scoped in-handler, and its `on_delete` is enforced by
  your code, not the DB.
- **`request_body` is entity-only.** No custom input DTO exists in the design. For
  untrusted public input, parse + validate a hand-written type in the handler before
  touching the entity.
- **A non-entity / custom success → hand-written `Json<serde_json::Value>`.** Leave
  `success.entity` out and you own the response type and its body.
- **id/PK rules:** omit `id` (synthetic `i64` autoincrement) for most entities; declare
  `id` as `string`/`uuid` only for client-supplied keys; a declared `id` must be
  integer/string/uuid.
- **`contract_version` is `0`, `1`, or `2`.** Use `1` for relations, enums, and json
  columns; `2` for storage buckets or realtime. v0 rejects `json` in db mode.
- **Jobs require `db`.** A `jobs` array with no `db` dependency is rejected.
- **`public` is exclusive.** It cannot combine with `auth_required` / `required_roles`,
  and not on a tenant-owned entity. And it is NOT how you open reads on an owner-scoped
  entity — that's the entity-level `public_read: true` (public reads, owner-only
  writes); a `public`/unguarded GET there is rejected as unimplementable.

## Next: scaffold + implement
With a valid design, run `jerrycan new <name> --design design.json`, then implement the
generated handler stubs. The per-topic pages:
- `jerrycan docs designing-examples` — seven complete, copy-ready designs.
- `jerrycan docs modules` — module/subroute structure and `belongs_to` derivation.
- `jerrycan docs database` — SeaORM entities, migrations, the `Db` handle.
- `jerrycan docs auth` — guards, sessions/JWT, webhook signatures.
- `jerrycan docs tenancy` — the Tenant guard, scoped repos, isolation tests.
- `jerrycan docs jobs` — task stubs, cron, queues, idempotency.
- `jerrycan docs extractors` / `jerrycan docs response-types` — handler I/O.
- `jerrycan docs errors` / `jerrycan docs error-codes` — the `JC####` set.
