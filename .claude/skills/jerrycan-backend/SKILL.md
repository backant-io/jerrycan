---
name: jerrycan-backend
description: Use when building, designing, or scaffolding a backend REST/CRUD API. Guides the whole process step by step — eliciting exactly what the user intends, analyzing an existing frontend to derive the API contract, scoping against what jerrycan can build, authoring design.json, scaffolding, implementing handlers/jobs from the docs, and verifying — asking the user at every decision point so you never guess what to build next.
---

# Building a backend with jerrycan

jerrycan turns a single declarative `design.json` into a working, tested,
multi-tenant REST backend: it generates the data layer (SeaORM models, dual-dialect
migrations, CRUD repos), typed handler stubs, tenant guards + isolation tests,
acceptance tests, OpenAPI, and app wiring. You (the agent) author the design and
fill in the handler bodies; `jerrycan check` is the source of truth for "done."

**This skill is a guided process.** Work through the phases in order. At each
decision point, **ask the user — do not guess.** Checkpoint after every phase so
neither of you loses the thread. The docs are complete and accurate (`jerrycan
docs --list`); read the relevant page before each step rather than guessing an API.

## The golden rules

1. **Never guess what to build.** If you don't know an entity, a field, a status
   value, an endpoint, a role, or a behavior — ask. One question at a time.
2. **`jerrycan check` and the validator are the truth.** Loop them; never claim
   green you haven't seen. Never weaken a generated check to pass it. **But a few
   generated tests are un-greenable BY CONSTRUCTION** (a happy-path probe that
   posts no credential to a login/webhook/API-key endpoint). For those, `check`
   will not be fully green and that is correct — recognize them (see Phase 5),
   leave them, and move on; do NOT thrash trying to green them or weaken the
   handler.
3. **Read the doc page before using a feature.** `jerrycan docs <page>` /
   `jerrycan explain <CODE>`. Start every project by reading `jerrycan docs designing`.
4. **Flag scope walls EARLY** (Phase 2). If the user needs something jerrycan can't
   express, surface it before you design, and decide together how to handle it.
5. **Checkpoint after each phase**: restate what's decided and what's next.

---

## Phase 0 — Orient (once)

- Confirm jerrycan is available: `jerrycan --version` (build it if you're in the
  framework repo: `cargo build -p jerrycan` then use `target/debug/jerrycan`).
- `jerrycan docs --list` to see the page index. **Read `jerrycan docs designing`
  now** — it is the complete `design.json` reference (every field, type,
  constraint, and the gotchas). You will author the design from it.
- Announce: "I'll use the jerrycan-backend process to design and build this step
  by step, checking with you at each decision."

## Phase 1 — Understand exactly what the user intends

Goal: a precise list of **resources, operations, actors, and cross-cutting needs**.
Ask **one question at a time**, prefer multiple-choice. Cover, in roughly this order
(skip what's already obvious from the request):

- **The product in one sentence.** What is it, who uses it?
- **Is there an existing frontend or API consumer?** (If yes → Phase 1b. If no →
  greenfield: derive resources from the domain.)
- **The resources (entities).** What "things" does it store? For each: its fields,
  which are required/unique, any **status-like enums** (and their exact allowed
  values), and how they relate (which belongs to which).
- **Actors & access.** Anonymous vs logged-in? Roles? Is it **multi-tenant**
  (does each customer/workspace/org see only its own data)? — tenancy is a big
  design decision; confirm it explicitly.
- **Auth style.** Session cookies, JWT bearer, OAuth login (Google/GitHub/…),
  programmatic **API keys** with scopes — which, if any?
- **Background / scheduled work.** Any cron jobs, async processing, webhooks from
  third parties (Stripe/Twilio/…)?
- **Operations per resource.** Standard CRUD, or specific actions (e.g.
  `POST /postings/{id}/close`)? Which need auth/roles? Which are public?

**Checkpoint:** write back a structured summary (resources + fields + relations +
auth + tenancy + jobs + endpoints) and get explicit confirmation before designing.

**Decide before you design** (these shape the whole design — settle them with the
user now, don't discover them mid-scaffold):
- **Auth model** — `session`, `jwt`, or `none`? `session` gates REST routes with
  the encrypted `jerrycan_session` cookie; `jwt` gates them with an
  `Authorization: Bearer <jwt>` token (the model a Supabase migration always
  uses). Pick it up front — REST guards, realtime, and the OpenAPI security
  scheme all follow it. And exactly which endpoints are **public**
  (login/register, webhooks, health) vs authenticated?
- **Is there a real org/team tenant entity?** Only use `tenancy` when a distinct
  org/workspace/team owns the data. If rows are just **per-user**, do NOT use
  `tenancy` — the tenant entity must be a SEPARATE entity from the auth identity
  (a user cannot be their own tenant). Instead give the row a `belongs_to` the
  user identity and scope every query by the session user's id (see `jerrycan
  docs tenancy`).
- **Server-owned vs client-supplied fields** — which fields does the server set
  (owner ids, timestamps, status) vs accept from the client? Server-owned fields
  are forced in-handler, never trusted from the request body.

### Phase 1b — Frontend-first (only if a frontend exists)

The frontend already encodes the contract; derive the backend from it instead of
guessing.

- Locate the **API client**: search for `fetch(`, `axios`, `apiClient`, an
  `api/`/`services/`/`hooks/` dir, an OpenAPI/`*.d.ts` types file, or env vars
  like `*_API_URL`/`baseURL`.
- Extract, per call: **method + path** (these become endpoints), the **request
  body shape** and **response shape** (these constrain entities/DTOs), and any
  **auth header** (`Authorization: Bearer …`, an API-key header, a cookie).
- Extract the **data models/types** the frontend expects (TS interfaces, GraphQL
  fragments, store shapes) — these map to entities + fields + enums.
- Note **route guards / login flows** (what's authenticated, what's public).
- Reconcile with the user: "Your frontend calls these N endpoints and expects
  these shapes — here's the backend that serves them. Anything missing/extra?"

> If the frontend expects a shape jerrycan can't express as an entity (composite
> /aggregate/nested payloads), note it now — it becomes a hand-written
> `Json<Value>` handler (see Phase 2 + the gotchas).

## Phase 2 — Scope check against what jerrycan builds (flag walls EARLY)

Before designing, map every requirement onto jerrycan's envelope. **If something
falls outside it, raise it with the user now** and decide: descope, hand-write
inside a handler (within the limits), or use an external service.

**jerrycan builds well (in-scope):** multi-tenant REST/CRUD JSON APIs · relations
(`belongs_to` + `on_delete`) · string enums · session/JWT auth + roles · OAuth2
client + scoped API keys · cron + background jobs (retries, dead-letter,
idempotency) · signed webhooks (`RawBody` + HMAC) · multipart upload parsing +
streaming download · design-modeled object storage (`storage.buckets`, contract
v2) · realtime (Postgres Changes + Broadcast + Presence, contract v2) · CORS +
rate limiting · `/healthz` + Prometheus `/metrics` +
OpenAPI · `jerrycan package` (binaries/containers/k8s/systemd).

**Hard walls (jerrycan will NOT design or scaffold these — decide with the user):**

| Need | Status | Handling |
|---|---|---|
| GraphQL / gRPC / JSON-RPC | **Out of scope** | REST only; remodel as REST or separate service |
| Aggregate / filter / search / pagination / reporting queries | Not design-expressible | Hand-write raw SeaORM in the agent-owned `repo.rs`/handler |
| Composite / nested / computed response shapes | `request_body`/`success` are entity-only | Hand-write a `Json<Value>` handler (declare `success.status` only) |
| Custom middleware / interceptors | Fixed kit only (CORS, rate-limit, access-log) | Not extensible per-route in v2 |
| Multi-step workflows / job chains / priorities | Jobs are single-shot | Out (the jobs contract is capped) |
| WebAuthn / SAML | Out of scope | session/JWT(HS) + OAuth2-client + API keys; native Apple/Google Sign-In via RS256 provider ID-token verify (`jerrycan-auth` `idtoken` feature) |
| `std::process` / `std::fs` / raw sockets in handlers | Forbidden by the JL0007 lint | Go through a framework extension or an allow-hatch (rare) |

**Checkpoint:** confirm the in-scope design and the agreed handling for any wall.

## Phase 3 — Author the design.json (iteratively, with the user)

Build the design **incrementally**, validating as you go. Reference `jerrycan docs
designing` for every construct, and `jerrycan docs designing-examples` for
copy-ready starting points. Work in this order, confirming each:

1. **Skeleton**: `name`, `contract_version: 1`, `dependencies` (pick from `db`,
   `auth`, `validate`, `observe`, `oauth` — `db` switches on SQL mode; `oauth`
   wires the OAuth client and implies `auth`).
2. **Tenancy** (if multi-tenant): `tenancy: { entity, member_roles }`. Every entity
   with a `belongs_to` aimed at the tenant entity becomes tenant-scoped (scoped
   repos + a `Tenant` guard + generated cross-tenant isolation tests).
3. **Auth**: `auth: { model: session|jwt, roles: [...] }`. (`required_roles` on an
   endpoint uses **these** roles, NOT `tenancy.member_roles` — see gotchas.)
4. **Entities** per module: fields with `type`, `required`, `unique`, `index`;
   status fields as `type: "string"` + `values: [...]` (the enum mechanism);
   relations via `belongs_to: [{ entity, on_delete }]`.
5. **Endpoints** per module: `operation_id`, `method`, `path` (incl. `/{id}`),
   `success: { status, entity?, list? }`, `errors`, `auth_required`/`required_roles`,
   and `public: true` for login/register/webhooks.
6. **Jobs**: `jobs: [{ name, schedule: "<5-field cron>", queue? }]` (requires `db`).

**Validate frequently** by scaffolding into a temp dir and reading the validator's
diagnostics — they are precise JSON-pointer-addressed errors. Do not move on with a
red design.

**Checkpoint:** show the user the design (or a plain-English summary of it) before
scaffolding for real.

## Phase 4 — Scaffold

```
JERRYCAN_FRAMEWORK_DEP='jerrycan = { ... }'   # only when testing against a local framework checkout
jerrycan new <app-dir> --design design.json
cd <app-dir>
for m in <each top-level module>; do jerrycan gen-tests --module "$m"; done
```

This emits, per module: `model.rs`, `repo.rs`, `handlers.rs`, and `deps.rs`
(AGENT-owned — edit freely; `repo.rs` is where per-user query scoping lives),
migrations, and `tests/acceptance.rs` (TOOL-owned, currently failing — green is
the goal). Run `jerrycan check` to see the red baseline.

## Phase 5 — Implement (handlers + jobs), loop `jerrycan check` to green

For each module, read the relevant doc page, then implement every handler stub:
- Data access: `jerrycan docs database` (+ `tenancy` for scoped `*_for` accessors;
  another module's table → declare a narrow second entity, same page).
- Auth/guards: `jerrycan docs auth`, `jerrycan docs auth-advanced` (OAuth, API keys,
  token-at-rest).
- Request data: `jerrycan docs extractors` (`Path`/`Query`/`Json`/`Headers`/
  `RawBody`/`Multipart`).
- Responses: `jerrycan docs response-types` (`Json`/`Created`/`NoContent`/
  `Redirect`/`(StatusCode, body)`/streaming).
- Validation: `jerrycan docs validation`. Jobs: `jerrycan docs jobs`. Errors:
  `jerrycan docs error-codes` / `jerrycan explain JCxxxx`.

Apply the **gotchas** (below). Loop `jerrycan --json check` toward `ok: true`
(build + clippy + tests + lints + audit/deny + schema). The JL0006 lint will catch
cross-tenant leaks — fix by using the scoped repo accessors, don't suppress it.

**Expect a few un-greenable generated tests — this is not your bug, and `ok:true`
may not be fully reachable.** The generator emits one happy-path probe per
endpoint that posts a minimal body with **no credential/signature/API key**. For
an endpoint whose success requires one (a `login` that 401s bad creds; a signed
webhook that 401/400s a bad signature; an API-key-gated route), that 2xx probe
**cannot pass** — the handler correctly rejects it. (The missing-id probe now
uses the endpoint's real HTTP method, so it no longer mis-fires a `405` on
POST-only `/{id}` actions.) **Do NOT weaken the handler to make these pass.** Leave
the probe, and prove the REAL behavior (success WITH a valid credential, and the
4xx without) in an **agent-owned test file** you write. Get every test you CAN
green green, then tell the user exactly which generated probes are un-satisfiable
and why. A **hand-written format/`Valid` constraint the generator can't see** is
the same case: the probe fills a `string` field with `"test-value"`, but if the
handler requires that field to be an **email, URL, or other format** (the design
contract has no field-format declaration, so this lives in a hand-written `Valid`
impl), a CORRECT handler rejects the fixture and the 2xx probe is un-greenable —
mark that endpoint **`probe: "skip"`** and write its success test with a real
value yourself. (Declared `uuid`/`datetime` fields already get format-valid
fixtures; only constraints invisible to the design need `skip`.) **Two more
un-greenable shapes:** (1) **role-mismatch** — the generated credential carries ONE
role (the design's role gate: an endpoint's first `required_roles`, else the first
declared `auth.roles`); if two guarded endpoints in one module require **different**
roles, the credential satisfies only one and a correct `require_role` handler 403s
the other's probe — cover it with your own credential minting that role. (2)
**cross-module `belongs_to` parent-existence** — a body fk pointing at a parent in
another module (no DB FK, no auto-seed) fills with `1`, but a handler that checks the
parent exists rejects it — mark `probe: "skip"`, seed the parent yourself, and test
it. (One further known generator rough edge: a `unique` non-PK field on the tenant
entity collides in the two-tenant isolation seed — make it a plain `index` instead.
Enum `values` fields now get a generated "out-of-range → 422" reject test that passes
on stubs — the request validator refuses the bad value before the handler; keep it.)

## Phase 6 — Verify

- The generated acceptance suite (incl. `tenant_a_cannot_read_tenant_b_*`) must
  pass — that's the cross-tenant isolation guarantee.
- For critical flows, do a **live HTTP smoke**: serve the app on a free port
  (`JERRYCAN_ADDR=127.0.0.1:<port> ... cargo run -p app`, with `JERRYCAN_SECRET`
  and a DB URL set) and drive register/login, a tenant-scoped create+read, any
  webhook/OAuth/api-key flow with real signatures/credentials.
- For declared error-cases that aren't auto-tested (the `// AGENT TODO`s — e.g.
  webhook bad-signature → 400, scope → 403), write your own tests in an
  agent-owned test file.

## Phase 7 — Hand off / iterate

Tell the user what was built: the endpoints, how to run it (`jerrycan dev`,
env vars), how to test (`jerrycan test`), and how to package (`jerrycan package`).
To ship it: `jerrycan deploy render` generates `deploy/render/deploy.sh`; run it
with `RENDER_API_KEY` for a live, secure URL (see `jerrycan docs packaging`).
Get feedback and iterate from the relevant phase. To change the data model or
endpoints, edit `design.json` and regenerate (tool-owned files refresh;
agent-owned handlers are untouched — re-implement only new stubs).

---

## Gotchas (these cost real time if you don't know them)

- **Enums are `string` + `values: [...]`** — there is no `enum` field type. It
  becomes a TEXT column with a `CHECK` constraint AND a generated request validator
  (an out-of-range value 422s at deserialization, before the DB — on create AND
  update, and the same 422 in memory mode); the Rust field is `String`.
  **Server-assign** privileged enum/role fields on create (e.g.
  registration sets `role = "user"`, never trusts the client) — generated success
  probes post the first declared value, but untrusted input must be defended.
- **`datetime`/`uuid` are `String`** at the Rust layer (no native time/uuid type,
  no built-in `now()→rfc3339`). Format/compare time yourself in handlers.
- **`required_roles` ≠ membership roles.** `required_roles` is the `auth.roles`
  namespace. To gate on a tenancy **membership** role (e.g. workspace "owner"),
  call `tenant.require_role("owner")?` IN the handler.
- **Cross-module `belongs_to` gets no scoped accessors.** A grandchild
  (`Application` → `Posting` → `Org`) isn't auto tenant-scoped; scope it by joining
  through the parent in your handler/repo.
- **Table name = `snake_case(entity)` pluralized** (`Ticket` → `tickets`, `ApiKey`
  → `api_keys`, `EnergySummary` → `energy_summaries`); override verbatim with
  `"table": "…"`. It shares the fk column's snake stem (`ApiKey` table `api_keys`,
  fk `api_key_id`). You need the exact table name only for hand-written cross-module SQL.
- **`public` endpoints can't live in a module that owns a tenant-owned entity**
  (the generator binds the endpoint to that entity → guard bypass). Put webhooks /
  login / inbound-ingest routes in their OWN module (entity-less is fine).
- **`request_body` is entity-only.** A public endpoint receives `Json<Entity>`
  (all fields), so the untrusted client can send server-controlled fields — force
  them in-handler (take the id from the path, fix the status, etc.).
- **Non-entity / custom success** → declare `success.status` only; the stub is
  `Result<Json<serde_json::Value>>` you hand-write.
- **Webhook stubs generate with no extractors** — add `Headers` + `RawBody`
  yourself and verify the HMAC (`jerrycan docs auth`).
- **`Db::from_env()` defaults to `sqlite::memory:`** when `JERRYCAN_DATABASE_URL`
  is unset (dev/test). Set a real URL for persistence.
- **OAuth needs the `oauth` dependency** in the design (auto-wires the feature);
  the in-process `MockIdp` (for hermetic tests) additionally needs the `mock-idp`
  facade feature.

## Red flags — stop and ask / fix

- About to invent an entity, field, status value, role, or endpoint → **ask**.
- A requirement maps to a Phase-2 wall → **surface it, decide together**.
- `jerrycan check` is red and you're tempted to delete/skip a generated assertion
  → don't; fix the handler or the design, or report a genuine framework bug.
- You can't find how to do something in the docs → `jerrycan docs --search <term>`
  / `jerrycan explain <code>`; if it's genuinely undocumented, say so rather than
  guessing an API.
