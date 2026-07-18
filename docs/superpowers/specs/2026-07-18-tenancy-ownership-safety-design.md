# Ownership safety: membership-verified tenancy + per-user scoping (#78, #79)

**Date:** 2026-07-18
**Status:** Approved design, pre-implementation
**Issues:** #78 (tenant guard cross-tenant leak), #79 (per-user no backstop)
**Origin:** round-3 agent eval (BookClubs J6, FitnessLog J2); Supabase-migration compatibility raised during design.

## Problem

jerrycan's tenancy has an **unstated single-tenant-per-user assumption**, and the docs
enshrine it: *"the guard is the gate, `tenant.id()` is trusted"* (14-tenancy.md).
The generated `Tenant` guard resolves the tenant from the **user**, not the request:

```sql
SELECT {fk}, role FROM {tenant}_members WHERE user_id = ?   -- first membership row
```

Three failures follow:

1. **#78 — cross-tenant leak (path-nested designs).** For a many-membership app
   (BookClubs: a user is in many clubs), `tenant.id()` is an *arbitrary* membership,
   so the natural nested route `/clubs/{club_id}/books` scopes by the **path** `club_id`
   — and **nothing verified the caller is a member of *that* club.** The guard only
   checked "member of something." A member of club A reads club B's books, passing the
   guard, the JL0006 lint (which never inspects the scope argument), and the test suite
   (no isolation test is emitted for nested creators). Trap-shaped — stubs 500 and the
   documented pattern mis-scopes rather than leaks — but the natural implementation leaks.

2. **#79 — per-user has no backstop.** A `belongs_to`-identity entity (Workout→User) —
   the exact shape JC0540 steers agents toward — gets **no** isolation test and **no**
   lint: both are gated on `design.tenancy`. An unscoped `repo.all()` leaks every user's
   rows with `jerrycan check` fully green.

3. **Supabase migration is lossy, not lossless.** The migrator recognizes the canonical
   many-membership RLS policy `fk IN (SELECT fk FROM {members} WHERE user_id = auth.uid())`
   (`rls.rs:16` `TenantMembership`, `live.rs:404`) and maps it to a `tenancy` block, but
   generates **flat** routes (`crud.rs`: `GET /`, `GET /{id}`, `PATCH /{id}` — no tenant
   in the path). Supabase RLS scopes **per row** (a row is visible iff *its* fk ∈ the
   membership set); jerrycan's guard flattens that to one arbitrary tenant. A migrated
   user in two workspaces silently sees only the first — violating the lossless-migration
   promise.

## Goals

- **Membership-verified, never single-tenant-trusted.** The tenant a request acts on is
  derived from the *route*, and the caller's membership in *that* tenant is verified —
  for both nested and flat routes, both hand-authored and Supabase-migrated designs.
- **Many-tenants-per-user is a first-class, safe capability** (Slack/GitHub-org shape).
- **#78 and #79 leaks become impossible by construction**, with generated isolation
  tests proving it for every ownership shape.
- **Supabase migration becomes genuinely lossless** for multi-membership users — with no
  migrator change: the fix lives in the generator, so migrated *and* authored designs
  benefit.

## Non-goals

- A "guarded but intentionally cross-user" mode (admin lists all users' rows). Owner-scoped
  is the safe default; an explicit escape hatch (e.g. a `shared`/`unscoped` marker) is a
  tracked follow-up, not built here.
- Changing the framework's public Rust API. All changes are in generated-code templates,
  testgen, lints, and docs (see Semver).

## The unifying rule

> The tenant id for any operation comes from the **route** — a path param, or (on a flat
> create) the request body — and it **MUST be in the caller's membership set**, verified by
> generated code before the handler scopes to it. List operations with no specific tenant
> return the whole membership set. A membership miss on a **read** is `404` (no existence
> leak); a **write** to a non-member tenant, or a role the membership lacks, is `403`.

Everything below is that rule specialized to route shape.

## Design

### A. Route classification (generator-side)

For a tenant-owned endpoint the generator already knows the path. Classify:

- **Path-scoped** — the path carries the tenant fk param: nested `/clubs/{club_id}/...`,
  or the tenant entity's own detail route. The tenant entity's own `/{id}` detail routes
  are **normalized** to `/{club_id}` so every tenant-addressing route names the fk
  uniformly (reinforces JC0542 param-consistency and R5 path-fk). *Fallback if this
  normalization ripples through testgen/#56/R5 keying: the guard extracts `club_id` OR
  `id` on the tenant module's own route — an implementation-plan detail.*
- **Membership-set (flat)** — a tenant-owned entity whose routes carry no tenant fk param
  (the Supabase-migrated shape, and any authored flat design). Scope by the membership set.
- **Collection routes** — `POST /clubs/` (create tenant) and `GET /clubs/` (list tenants):
  no `Dep<Tenant>`; special-cased below.

### B. Path-scoped guard

`Dep<Tenant>` extracts the tenant fk from the path and verifies membership for *that* tenant:

```
SELECT role FROM club_members WHERE user_id = ? AND club_id = ?    -- ? = path club_id
  no row → Error::not_found()             (404, no existence leak)
  found  → Tenant { id: club_id, role }   -- id GUARANTEED == the addressed tenant
```

Handlers keep `tenant.id()` for scoping (now provably the path tenant) and
`tenant.require_role(...)` for role gates (403 on wrong role). The leak is gone: a club's
rows are unreachable without a membership row for that club.

### C. Membership-set (flat) scoping — RLS-faithful, Supabase's model

No tenant in the path. Generated scoped repo methods filter by the membership set:

```sql
-- list:      SELECT * FROM customers
--            WHERE workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?)
-- get/{id}:  ... AND id = ?                       → 404 if the row's tenant ∉ set
-- update/{id}, delete/{id}: same IN(...) guard     → 404 outside the set
-- create:    workspace_id comes from the body; verified ∈ set (else 403), like RLS WITH CHECK
```

This is exactly the RLS the migrator already recognizes, restored faithfully: a
multi-workspace user sees all their workspaces' rows, and nothing outside the set. The
migrator needs **no change** — it emits the `tenancy` block + flat routes; the generator's
tenancy scoping now does the right thing.

### D. Auto-seeded membership + membership-filtered tenant list

- **`POST /clubs/`** (generated, one transaction): insert the club, then insert
  `club_members(user_id=session, club_id=new_id, role=<first member_role>)`. "Creator
  becomes organizer" is guaranteed, not an agent TODO; the guard works on the next request.
- **`GET /clubs/`**: generated membership-filtered list —
  `SELECT c.* FROM clubs c JOIN club_members m ON m.club_id = c.id WHERE m.user_id = ?`.

### E. Per-user (#79): make the leak not exist

For a **guarded** entity that `belongs_to` the **identity** user (no tenancy block):

- The generated repo emits **only** owner-scoped `all_for(user_id)/get_for/remove_for/
  update_for`; the unscoped `all()/get()/remove()/update()` are **not generated** — the
  leaky call isn't reachable. The handler passes `user.0.id`.
- The isolation-test emitter (today `return "" if design.tenancy is None`) is generalized
  to cover identity-owned guarded entities.

Default is owner-scoped (safe). The rare intentional cross-user case is the tracked
escape-hatch follow-up (Non-goals).

### F. Tests (part of the fix)

Isolation tests are emitted for **every** ownership shape, including the cases that today
degrade to empty:

- path-scoped **nested creator** (the exact BookClubs shape with no coverage today);
- membership-set **flat** routes (member of A cannot see B's rows via `get/{id}`; can see
  all their memberships' rows via list);
- per-user identity-owned (user B cannot see user A's rows);
- a **migration** test asserting a recognized `TenantMembership` RLS policy → a scaffolded
  app where a two-workspace user sees both workspaces' rows and nothing else.

### G. Docs (part of the fix)

Rewrite `docs/ai/14-tenancy.md` (+ embedded twin): remove the "`tenant.id()` is trusted"
framing; document membership-verified scoping, many-membership support, the path-scoped vs
flat shapes, auto-seeding, 404-vs-403, and the Supabase-flat model. This doc *taught* the
false-trust mental model — leaving it leaves the trap.

## Edges / risks (on record)

- **Escape hatch** for guarded-but-cross-user (admin list): follow-up issue, not built.
- **No-drift:** this changes generated output for tenancy designs — `reference-slice` and
  its hand-written fixtures (`conformance/eval/fixtures/reference/*`) must be updated to the
  membership-verified guard; the heavy conformance suite is the proof gate.
- **Semver:** no framework public Rust API changes (the guard/`Tenant` are generated-code
  templates; `require_role`, `Error::not_found/forbidden`, `Dep`, `db.sql` already exist).
  `cargo semver-checks` stays clean at the current baseline. **But** the behavioral change
  to *generated apps* is large; flag **0.5.0** at release for honesty, not 0.4.x —
  a release-time call.
- **Param normalization ripple** (`/{id}`→`/{club_id}`): may touch testgen seeding, #56,
  R5 keying; fallback is the guard-branch approach in §A. Resolve in the plan.
- **Create on flat model needs a body tenant fk** validated ∈ set — the one place a flat
  create reads the tenant id from the body (RLS `WITH CHECK` parity); path-scoped creates
  take it from the path.

## Rollout

Package boundaries for the implementation plan (each its own PR, gated + heavy-verified):

1. **Scoping engine** — genroute repo methods + guard template for path-scoped and
   membership-set shapes; route classification; 404/403 split.
2. **Membership lifecycle** — auto-seed on create, membership-filtered tenant list.
3. **Per-user (#79)** — suppress unscoped methods for guarded identity-owned entities.
4. **Testgen** — isolation tests for all shapes + the nested-creator and Supabase-migration
   cases; JL0006 generalized/retired as scoping becomes structural.
5. **Docs + fixtures** — 14-tenancy rewrite; reference-slice fixtures updated; heavy green.

Order matters: land the scoping engine (1) before testgen (4) so the new tests assert the
new behavior.

## Success criteria

- BookClubs J6 shape: a non-member of club B gets 404 on club B's resources; a member sees
  only their clubs; generated isolation tests prove it.
- Supabase-migrated `TenantMembership` app: a two-workspace user sees both workspaces' rows
  and nothing outside their set — proven by a migration test.
- FitnessLog J2 shape: the unscoped repo method does not exist; user B cannot reach user A's
  rows; isolation test proves it.
- No framework public-API semver break; heavy conformance green with updated fixtures.
