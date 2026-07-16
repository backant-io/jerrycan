# Designing (the design.json contract)

## Purpose
`design.json` is the single source of truth a jerrycan backend is generated from.
You AUTHOR it first, then `jerrycan new <name> --design design.json` scaffolds a
workspace from it (validate-only happens inside `new`; there is no separate
`validate` subcommand). This page is the complete schema — every field, every
rule, every gotcha — so you can write a valid design without round-tripping
through the validator. The contract is `docs/contracts/design-schema.json`
(JSON Schema 2020-12); the rules below are exactly what the generator enforces.

Validation returns a list of pointed "questions" keyed by JSON pointer (e.g.
`/modules/0/endpoints/1/required_roles`); an empty list means the design is
complete. Fix every question before scaffolding.

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
- `contract_version` (REQUIRED) — an integer, `0`, `1`, or `2`. Use `1` for
  standard apps: v1 unlocks `belongs_to` relations, enum `values`, and real
  `json` columns in db mode. `2` additionally unlocks storage buckets and
  realtime channels (required when the design uses either). `0` is the legacy
  in-memory contract (a `json` field is rejected under db mode on v0). Only
  `> 2` is rejected.
- `description?` — free text.
- `base_path?` — app-level mount prefix applied once to every module and bucket
  mount, e.g. `"/v1"` serves all routes under `/v1`. Health (`/healthz`) and
  metrics (`/metrics`) stay unprefixed. Empty/`/`/absent is a no-op; must be an
  absolute path (leading `/`, no trailing slash).
- `auth?` — `{ "model": "none" | "session" | "jwt", "roles": [string] }`. `model`
  is REQUIRED inside the block. A non-`none` model activates auth-mode generation
  (Session/Bearer guards, `require_role`) and implies the `auth` dependency.
  `roles` is the role NAMESPACE that `required_roles` on endpoints draws from.
- `dependencies?` — array of capability names. The RESERVED names (each toggles
  real generation):
  - `db` — SQL storage (SeaORM entities, migrations, `Db` dependency).
  - `validate` — serves the OpenAPI document at `/openapi.json`.
  - `auth` — session/JWT guards + the `Auth` extension (also implied by a
    non-`none` `auth.model`).
  - `observe` — access-log middleware, `/healthz`, and Prometheus `/metrics`.
  - `oauth` — enables `jerrycan::auth::oauth::{OAuthClient, Provider}`; implies `auth`.

  Any OTHER name is a STUB: the generator reminds you to wire it; it generates
  nothing on its own.
- `tenancy?` — `{ "entity": "Workspace", "member_roles": [string] }`. `entity`
  (REQUIRED in the block) must be a declared entity (PascalCase). See Tenancy.
- `jobs?` — `[{ "name", "schedule"?, "queue"? }]`. Background jobs; require the
  `db` dependency. See Jobs.
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
  slash. A param-carrying mount like `/{ws}` is allowed (it captures a parent
  param for subroutes).
- `description?` — free text.
- `entities?` — the data shapes this module owns (see Entity). A module can have
  zero entities (e.g. a webhook module).
- `endpoints` (REQUIRED) — at least one (see Endpoint).
- `subroutes?` — nested modules, mounted UNDER this module's mount. Every module
  rule applies recursively at any depth.
- `dependencies?` — module-scoped stubs. `db`/`validate` only take effect at the
  TOP level; here they are ordinary stubs.

## Entity
```json
{
  "name": "Lead",
  "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
  "fields": [ /* … at least one … */ ]
}
```
- `name` (REQUIRED) — PascalCase, `^[A-Z][A-Za-z0-9]*$`, and NOT a Rust keyword
  (it becomes a Rust type).
- `belongs_to?` — array of `{ "entity": "<PascalCase>", "on_delete": "cascade" | "set_null" | "restrict" }`.
  - The target entity must exist SOMEWHERE in the design (any module/subroute) —
    cross-module references are allowed.
  - `on_delete` defaults to `"restrict"`. `set_null` makes the fk column nullable.
  - The fk column is DERIVED as `snake_case(entity) + "_id"` (e.g. `Workspace` →
    `workspace_id`). Do NOT also declare an explicit field of that name — it
    collides (db mode).
  - A SAME-module relation gets a real DB `FOREIGN KEY` and the `on_delete` is
    enforced by the database. A CROSS-module relation gets only an indexed fk
    column (no DB constraint); `on_delete` is then YOUR handler's job, not the
    DB's.
- `fields` (REQUIRED) — at least one (see Field).

The SQL **table name** defaults to `snake_case(entity)`, pluralized — `Ticket` →
`tickets`, `Workspace` → `workspaces`, `ApiKey` → `api_keys`, `EnergySummary` →
`energy_summaries`. Pluralization is the ordinary English rule (consonant + `y` →
`ies`; ends in `s`/`x`/`z`/`ch`/`sh` → `es`; else `+s`). A multi-word entity's
table therefore shares the snake_case stem of its fk column (`ApiKey` → table
`api_keys`, fk column `api_key_id`). Set `"table": "…"` on an entity to override
the name verbatim (a frozen external schema). You need the exact table name only
for hand-written cross-module SQL (the generated repo handles intra-module access).

### The `id` field (primary key)
Every entity has an `id` primary key. You usually do NOT declare it:
- **Omit `id`** → the generator synthesizes an auto-increment integer PK
  (`BIGINT AUTOINCREMENT` / `BIGSERIAL`); the Rust key type is `i64`. This is the
  default and what you want most of the time.
- **Declare `id` as `integer`** → same synthetic auto-increment `i64` PK (no
  duplicate column).
- **Declare `id` as `string` or `uuid`** → that becomes the PK column (TEXT, no
  auto-increment) and the Rust key type is `String`. Use this for
  client-supplied or externally-issued ids; your handler must set it.
- A declared `id` of any OTHER type (float/boolean/datetime/json) is REJECTED in
  db mode — a PK must be integer, string, or uuid.

## Field
```json
{ "name": "status", "type": "string", "required": true, "unique": false, "index": false, "values": ["draft", "active"] }
```
- `name` (REQUIRED) — snake_case, `^[a-z][a-z0-9_]*$`. A Rust keyword (`type`,
  `match`, `ref`, …) is allowed: codegen emits it as a raw identifier (`r#type`)
  with a serde rename so the wire and column names stay unchanged. Only
  `self`/`crate`/`super`, which no raw identifier can escape, are rejected.
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

- `required?` — defaults to `true`. `false` makes the column nullable / the Rust
  field `Option<T>`.
- `unique?` — defaults to `false`. Adds a UNIQUE constraint; a duplicate insert
  surfaces as `409 JC0409`.
- `index?` — defaults to `false`. Adds a non-unique index.
- `values?` — the ENUM mechanism. ONLY valid on a `string` field, and must be
  non-empty. A `string` field + `values` becomes a TEXT column with a CHECK
  constraint AND generated request validation: the model rejects an out-of-range
  value at deserialization, so invalid enum input is answered `422 JC0422` before
  it reaches the database — on every write path (create AND update), and the same
  422 in memory mode (which has no DB). There is NO separate `enum` field type —
  enums are always `string` + `values`.

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
- `operation_id` (REQUIRED) — snake_case; becomes the handler fn name; unique
  within its module.
- `method` (REQUIRED) — `GET` | `POST` | `PUT` | `PATCH` | `DELETE` (UPPERCASE).
  The `(method, path)` pair must be unique in the module.
- `path` (REQUIRED) — starts with `/`. Path params are `/{name}`; 1–3 params per
  route, braces balanced. The leaf `/{id}` param is what a `Path<Key>` binds.
- `auth_required?` — defaults to `false`. Requires an active auth model.
- `required_roles?` — array of role names. **CRUCIAL: these are the
  `auth.roles` namespace, NOT `tenancy.member_roles`.** Every role here must be
  declared in top-level `auth.roles`. A non-empty `required_roles` implies
  `auth_required`. (Membership-role gating — owner/member of a tenant — is NOT
  done here; do it in-handler via `tenant.require_role(...)`. See Tenancy.)
- `public?` — defaults to `false`. Marks the route unauthenticated by design
  (exempt from the auth lint and the generated 401 test). It CANNOT combine with
  `auth_required` or `required_roles` (each is a distinct error). It also cannot
  live in a module whose entity is **tenant-owned**: the generator binds an
  endpoint to its module's entity, so a `public` endpoint there (even one with no
  `request_body` of its own) would bypass the Tenant guard and expose one
  tenant's rows. **Put public endpoints — webhooks, an inbound-ingest route, a
  login/register — in their OWN module** that has no tenant-owned entity
  (entity-less is fine).
- `probe?` — `"auto"` (default) or `"skip"`. `skip` tells the generator NOT to
  emit the happy-path 2xx probe for an endpoint whose success needs a credential
  it can't synthesize (login, signed webhook, api-key route) — otherwise that
  probe is un-greenable and `jerrycan check` can never reach `ok:true`. With
  `skip` the generator emits an AGENT TODO; you write the credentialed success +
  rejection tests yourself.
- `request_body?` — `{ "entity": "<Name>" }` ONLY. The body is the named entity;
  the entity must be declared in THIS module. There is no narrower/custom input
  DTO in the design — for an endpoint that takes untrusted public input, defend
  it IN-HANDLER (parse a hand-written input type, validate, then map to the
  entity).
  - **Server-owned fk on guarded endpoints:** if the body entity `belongs_to`
    the auth identity entity (derived fk column `user_id`) AND the endpoint is
    guarded, the generated request body is a `{Entity}Request` DTO WITHOUT
    `user_id` (the OpenAPI request schema omits it too) — the handler injects
    the authenticated session user's id; clients never send it. Every other
    `belongs_to` fk stays required client input, and an unguarded endpoint
    keeps `user_id` (no session to inject).
- `success` (REQUIRED) — `{ "status": <2xx-or-3xx>, "entity"?: "<Name>", "list"?: bool }`.
  - `status` must be in 200–399 (2xx OR 3xx — 3xx is valid for redirects like an
    OAuth connect).
  - `entity?` (must be declared in this module) + `list?` shape the response:
    `entity` + `list:true` → `Json<Vec<Entity>>`; `entity` + `list:false` →
    `Json<Entity>`; `201` + `entity` → `Created<Entity>`; `204` → `NoContent`.
  - A success with NO `entity` (and not 204/201) generates
    `Result<Json<serde_json::Value>>` — a custom shape you HAND-WRITE. (`201`
    with no entity → `Created<serde_json::Value>`.)
- `errors?` — `[{ "status": <4xx-or-5xx>, "code"?: "JC####", "when": "<text>" }]`.
  `status` is 400–599; `code` (optional) must match `^JC[0-9]{4}$`; `when` is
  REQUIRED prose. **Only a `404` on a single-`{id}` path is auto-tested** (the
  generator emits a missing-id test). Every other declared error becomes an
  `// AGENT TODO` in the generated acceptance test for you to encode.

> **Un-greenable success probes (expected — don't fight them).** The generator
> emits one happy-path test per endpoint that posts a MINIMAL body with **no
> credential, no signature, no API key**, and asserts `success.status`. For an
> endpoint whose success genuinely REQUIRES a credential — a `login` that 401s
> bad creds, a signed webhook that 401/400s a bad signature, an API-key-gated
> route — that 2xx probe is **un-satisfiable by construction**, so `jerrycan
> check` will not be fully green for it. That is normal and correct: **do NOT
> weaken the handler to make the probe pass.** Leave the probe red, and cover the
> real behavior (the 200 WITH a valid credential, and the 401/400 without) in
> your own agent-owned test. See `jerrycan docs testing`.
>
> **A hand-written format/`Valid` constraint the generator can't see is the same
> case.** The probe fills each field with a generic fixture — a `string` gets
> `"test-value"`; declared `uuid`/`datetime` get format-valid fixtures and enum
> `values` get a declared value. But the design contract has **no field-format
> declaration** (no email/url/pattern), so a `string` the handler additionally
> requires to be an **email, URL, or other format** — enforced by a hand-written
> `Valid` impl the design can't express — is rejected by a CORRECT handler, and
> that endpoint's 2xx probe is un-greenable too. Mark such an endpoint
> **`probe: "skip"`** and write its success test with a real value yourself.

## Worked examples

### Minimal app
The smallest valid design — one module, one public endpoint, no db:
```json
{
  "name": "hello-api",
  "contract_version": 1,
  "description": "The smallest valid design: one module, one endpoint.",
  "modules": [
    {
      "name": "health",
      "endpoints": [
        {
          "operation_id": "liveness",
          "method": "GET",
          "path": "/",
          "public": true,
          "success": { "status": 200 }
        }
      ]
    }
  ]
}
```

### CRUD module (fields + enum + unique + index)
```json
{
  "name": "catalog-api",
  "contract_version": 1,
  "description": "A CRUD module backed by SQL: fields, an enum, unique + index.",
  "dependencies": ["db"],
  "modules": [
    {
      "name": "products",
      "entities": [
        {
          "name": "Product",
          "fields": [
            { "name": "sku", "type": "string", "unique": true, "index": true },
            { "name": "name", "type": "string" },
            { "name": "price_cents", "type": "integer" },
            { "name": "status", "type": "string", "values": ["draft", "active", "archived"] },
            { "name": "in_stock", "type": "boolean", "required": false },
            { "name": "attributes", "type": "json", "required": false }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_products",
          "method": "GET",
          "path": "/",
          "success": { "status": 200, "entity": "Product", "list": true }
        },
        {
          "operation_id": "create_product",
          "method": "POST",
          "path": "/",
          "request_body": { "entity": "Product" },
          "success": { "status": 201, "entity": "Product" },
          "errors": [{ "status": 409, "code": "JC0409", "when": "sku already exists" }]
        },
        {
          "operation_id": "show_product",
          "method": "GET",
          "path": "/{id}",
          "success": { "status": 200, "entity": "Product" },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
        },
        {
          "operation_id": "delete_product",
          "method": "DELETE",
          "path": "/{id}",
          "success": { "status": 204 },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
        }
      ]
    }
  ]
}
```

### Relations (`belongs_to` + `on_delete`)
A `Comment` belongs to a `Post` in the SAME module, so the fk is a real
DB-enforced `FOREIGN KEY` with `ON DELETE CASCADE`:
```json
{
  "name": "blog-api",
  "contract_version": 1,
  "description": "Relations: a Comment belongs_to a Post (same module, cascade).",
  "dependencies": ["db"],
  "modules": [
    {
      "name": "posts",
      "entities": [
        {
          "name": "Post",
          "fields": [
            { "name": "title", "type": "string" },
            { "name": "body", "type": "string" }
          ]
        },
        {
          "name": "Comment",
          "belongs_to": [{ "entity": "Post", "on_delete": "cascade" }],
          "fields": [
            { "name": "author", "type": "string" },
            { "name": "text", "type": "string" }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_posts",
          "method": "GET",
          "path": "/",
          "success": { "status": 200, "entity": "Post", "list": true }
        },
        {
          "operation_id": "create_post",
          "method": "POST",
          "path": "/",
          "request_body": { "entity": "Post" },
          "success": { "status": 201, "entity": "Post" }
        },
        {
          "operation_id": "create_comment",
          "method": "POST",
          "path": "/{id}/comments",
          "request_body": { "entity": "Comment" },
          "success": { "status": 201, "entity": "Comment" },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown post id" }]
        }
      ]
    }
  ]
}
```

### First-class tenancy
`tenancy.entity` names the tenant; any entity that `belongs_to` it becomes
tenant-owned. The generator emits a `<workspace>_members` membership table,
tenant-SCOPED repositories (`all_for`/`get_for`/…), a Tenant guard, and
cross-tenant ISOLATION tests. Tenancy needs an active auth model
(`session`/`jwt`):
```json
{
  "name": "crm-api",
  "contract_version": 1,
  "description": "First-class tenancy: Leads belong to a Workspace tenant.",
  "auth": { "model": "session", "roles": ["owner", "member"] },
  "dependencies": ["db", "auth"],
  "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
  "modules": [
    {
      "name": "workspaces",
      "entities": [
        {
          "name": "Workspace",
          "fields": [{ "name": "name", "type": "string" }]
        }
      ],
      "endpoints": [
        {
          "operation_id": "create_workspace",
          "method": "POST",
          "path": "/",
          "auth_required": true,
          "request_body": { "entity": "Workspace" },
          "success": { "status": 201, "entity": "Workspace" }
        }
      ]
    },
    {
      "name": "leads",
      "entities": [
        {
          "name": "Lead",
          "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
          "fields": [
            { "name": "email", "type": "string", "unique": true },
            { "name": "stage", "type": "string", "values": ["new", "qualified", "won", "lost"] }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_leads",
          "method": "GET",
          "path": "/",
          "auth_required": true,
          "success": { "status": 200, "entity": "Lead", "list": true }
        },
        {
          "operation_id": "create_lead",
          "method": "POST",
          "path": "/",
          "required_roles": ["owner"],
          "request_body": { "entity": "Lead" },
          "success": { "status": 201, "entity": "Lead" }
        }
      ]
    }
  ]
}
```

### Auth + a public route
A `jwt` model with roles; a guarded `/me`, a role-gated delete, and a `public`
signup (the only unauthenticated route):
```json
{
  "name": "accounts-api",
  "contract_version": 1,
  "description": "Auth with roles, a guarded route, and a public signup route.",
  "auth": { "model": "jwt", "roles": ["admin", "user"] },
  "dependencies": ["db", "auth"],
  "modules": [
    {
      "name": "accounts",
      "entities": [
        {
          "name": "Account",
          "fields": [
            { "name": "email", "type": "string", "unique": true },
            { "name": "password_hash", "type": "string" },
            { "name": "role", "type": "string", "values": ["admin", "user"] }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "signup",
          "method": "POST",
          "path": "/signup",
          "public": true,
          "request_body": { "entity": "Account" },
          "success": { "status": 201, "entity": "Account" }
        },
        {
          "operation_id": "me",
          "method": "GET",
          "path": "/me",
          "auth_required": true,
          "success": { "status": 200, "entity": "Account" }
        },
        {
          "operation_id": "delete_account",
          "method": "DELETE",
          "path": "/{id}",
          "required_roles": ["admin"],
          "success": { "status": 204 },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
        }
      ]
    }
  ]
}
```

### Jobs / cron
Declared at the TOP LEVEL; jobs require the `db` dependency. `schedule` (5-field
cron) makes a job a cron job; a job with no `schedule` is enqueued
programmatically:
```json
{
  "name": "billing-api",
  "contract_version": 1,
  "description": "Background jobs: an hourly cron plus a programmatic queue job.",
  "dependencies": ["db"],
  "jobs": [
    { "name": "expire_trials", "schedule": "0 * * * *", "queue": "billing" },
    { "name": "send_receipt", "queue": "email" }
  ],
  "modules": [
    {
      "name": "invoices",
      "entities": [
        {
          "name": "Invoice",
          "fields": [
            { "name": "amount_cents", "type": "integer" },
            { "name": "paid", "type": "boolean", "required": false }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_invoices",
          "method": "GET",
          "path": "/",
          "success": { "status": 200, "entity": "Invoice", "list": true }
        }
      ]
    }
  ]
}
```

### Signed webhook endpoint
A webhook is an unauthenticated POST whose only proof is an HMAC signature.
Model it as a `public` endpoint with a `signature`-mentioning error case (the
auth lint recognizes signature auth and won't flag it); verify the signature
IN-HANDLER over `RawBody` (see Auth):
```json
{
  "name": "payments-api",
  "contract_version": 1,
  "description": "A signed webhook: a public POST whose proof is an HMAC signature.",
  "modules": [
    {
      "name": "webhooks",
      "endpoints": [
        {
          "operation_id": "stripe_webhook",
          "method": "POST",
          "path": "/stripe",
          "public": true,
          "success": { "status": 204 },
          "errors": [
            { "status": 401, "code": "JC0401", "when": "missing or invalid HMAC signature" }
          ]
        }
      ]
    }
  ]
}
```

## Gotchas (these cost real time)
- **Enums are `string` + `values`, not a type.** There is no `enum` field type.
  `{ "type": "string", "values": ["a", "b"] }` → TEXT + CHECK + a generated request
  validator (an out-of-range value 422s at deserialization, before the DB).
  `values` on a non-string field is rejected; an empty `values` is rejected.
- **`datetime`/`uuid` are `String` at the Rust layer.** There is no native time
  type and no built-in `now() → rfc3339`. Handle time in-handler (format/parse a
  string yourself); the column is TEXT.
- **`required_roles` ≠ membership roles.** `required_roles` draws from
  `auth.roles` ONLY. Tenant membership (owner/member) is checked in-handler via
  the Tenant guard, never through `required_roles`.
- **Cross-module `belongs_to` gets NO scoped `*_for` accessors and NO DB FK.**
  Only an entity that `belongs_to` the tenancy entity gets scoped repos. A
  grandchild reached across modules must be tenant-scoped in-handler, and its
  `on_delete` is enforced by your code, not the DB.
- **`request_body` is entity-only.** No custom input DTO exists in the design.
  For untrusted public input, parse + validate a hand-written type in the
  handler before touching the entity.
- **A non-entity / custom success → hand-written `Json<serde_json::Value>`.**
  Leave `success.entity` out and you own the response type and its body.
- **id/PK rules:** omit `id` (synthetic `i64` autoincrement) for most entities;
  declare `id` as `string`/`uuid` only for client-supplied keys; a declared `id`
  must be integer/string/uuid.
- **`contract_version` is `0`, `1`, or `2`.** Use `1` for relations, enums, and
  json columns; `2` for storage buckets or realtime. v0 rejects `json` in db mode.
- **Jobs require `db`.** A `jobs` array with no `db` dependency is rejected.
- **`public` is exclusive.** It cannot combine with `auth_required` /
  `required_roles`, and not on a tenant-owned entity.

## Next: scaffold + implement
With a valid design, run `jerrycan new <name> --design design.json`, then
implement the generated handler stubs. The per-topic pages:
- `jerrycan docs modules` — module/subroute structure and `belongs_to` derivation.
- `jerrycan docs database` — SeaORM entities, migrations, the `Db` handle.
- `jerrycan docs auth` — guards, sessions/JWT, webhook signatures.
- `jerrycan docs tenancy` — the Tenant guard, scoped repos, isolation tests.
- `jerrycan docs jobs` — task stubs, cron, queues, idempotency.
- `jerrycan docs extractors` / `jerrycan docs response-types` — handler I/O.
- `jerrycan docs errors` / `jerrycan docs error-codes` — the `JC####` set.
