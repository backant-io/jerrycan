# Transitive tenancy: close the deep-graph cross-tenant leak (#102, #103)

**Date:** 2026-07-19
**Status:** Approved design, pre-implementation
**Issues:** #102 (transitive tenant children leak — residual #78 on deep graphs), #103 (JL0006 naive substring scan, subroute-path-blind)
**Origin:** round-4 agent eval (`conformance/eval/faceoff4-2026-07-19.md`), fable-audited at file:line. 10 heavy apps on published v0.5.0: the ownership-safety CORE held, but multi-hop tenant graphs (Contact→Account→Org, Message→Channel→Workspace) leak across tenants.
**Ships as:** 0.5.1 (security patch; framework Rust API unchanged/additive, `cargo semver-checks` clean).

## Problem

v0.5.0 closed the cross-tenant leak (#78) for **direct** tenant children only. Every
predicate that decides "is this entity tenant-owned?" tests **direct** `belongs_to`:

- `Design::endpoint_tenant_shape` — `owns_tenant_entity = module.entities.iter().any(|e| e.belongs_to.iter().any(|b| b.entity == tenancy.entity))` (design.rs:762‑768).
- `collect_tenant_owned` — recurses the **subroute tree** but tests `entity.belongs_to.iter().any(|b| b.entity == tenant)` (design.rs:1060‑1073), i.e. depth‑1 only.
- `tenant_owned_isolation_test` — finds the tenant entity by direct `belongs_to`, and its own comment says "subroute-nested tenant entities are out of scope — their isolation is the agent's to test" (testgen.rs:886‑894).

The generator walks one graph (route nesting) but tests ownership on another (the
`belongs_to` / FK chain) at **depth 1**. A **grandchild** — `Contact → Account → Org(tenant)`
— reaches the tenant only *through* `Account`, so its `belongs_to` does not name the tenant.
Consequence for every transitively-owned entity:

1. **No guard / no scoping.** `endpoint_tenant_shape` returns `None` → the route is treated
   as untenanted → the handler is steered to the unscoped `repo.all()/get()` → a member of
   Org A reads Org B's contacts.
2. **No lint.** `tenant_owned()` never lists it, so JL0006 never scans its handler.
3. **No isolation test.** No cross-tenant test is emitted → `jerrycan check` is fully green.

Multi-hop tenant graphs (CRM, chat, project trackers) are the **common** shape, so this is a
live cross-org data read on the published crate: `check` says `ok:true` while Org A reads
Org B's rows.

**#103 is the enabler that let #102 ship undetected.** JL0006 (`scan_unscoped`, lints.rs:196‑243)
reads `crates/routes/{module}/src/handlers.rs`, where `{module}` comes from `tenant_owned()` and
can be a **subroute** name. A subroute's handler lives at a **nested** path, so the read fails and
`return`s — a **silent skip**. The scan is also textual (`line.contains("repo.all()")`), blind to
multi-line chains and repo aliasing. The one guardrail that should have caught a hand-written deep
leak fails closed-but-quiet.

## Goals

1. Tenant-ownership is **transitive**: an entity is tenant-owned iff a `belongs_to` chain
   reaches the tenant entity, at any depth. Guard, scoped repo methods, steering, lint, and
   isolation test all follow from that one predicate.
2. Scoped queries **JOIN up the chain** to apply the existing membership filter — no
   data-model change, no denormalized tenant column on descendants.
3. Ambiguity (a diamond graph with >1 distinct path to the tenant) is a **design-time hard
   error** (`JC0545`), never a silent guess.
4. JL0006 becomes depth- and path-aware and **fail-loud**: it resolves the real nested handler
   file and, if a tenant-owned handler is missing/unreadable/unparseable, emits a loud
   diagnostic instead of skipping.
5. Correct-by-construction: the leaky code is not generated, and a conformance app with a
   3-level graph proves red-on-unscoped / green-on-scoped.

## Non-goals

- **Denormalized tenant FK** on descendants (rejected: mutates the user's schema, can drift).
- **Depth-2-only** support (rejected: re-creates the "fixed depth‑1, depth‑2 still leaks"
  trap one level deeper).
- **Path/row consistency** on nested-grandchild routes (a `/accounts/{account_id}/contacts/{id}`
  where the contact is under a *different* account): NOT a leak (membership join is the boundary),
  so it is a tracked nicety, not part of 0.5.1.
- Membership add/remove surface, realtime/storage tenancy — those are Waves 3/4.

## The unifying rule

> An entity is tenant-owned iff **`tenant_path(entity)` is `Some`** — a unique `belongs_to`
> chain from the entity to the tenant. Direct ownership is the depth‑0 (zero-join) case, so
> this **subsumes** today's behavior for direct children. Every scoping decision keys off
> `tenant_path`; the **security boundary is the membership join, never the URL path.**

## Design

### A. Transitive ownership resolver (design.rs)

New `Design::tenant_path(&self, entity: &str) -> Option<TenantPath>`, a DFS over the
`belongs_to` graph toward `tenancy.entity`:

```rust
pub(crate) struct TenantPath {
    /// Ordered joins from the entity UP to the tenant. Empty for a direct child.
    /// Each: (child_table, child_fk_col, parent_table, parent_pk_col).
    joins: Vec<JoinLink>,
    /// The tenant fk column on the tenant-most table in the chain (the column the
    /// membership subquery filters — e.g. `org_id`).
    tenant_fk: String,
}
```

- **No path** → `None` (entity is not tenant-owned; output stays byte-identical for it).
- **Exactly one path** → `Some(TenantPath{ joins, tenant_fk })`.
- **≥2 distinct paths** to the tenant → push `JC0545` (hard error) and treat as unresolved.
- **Cycle** → visited-set guard; a `belongs_to` cycle does not hang the resolver.

Refactor the direct predicates onto it:
- `tenant_owned()` → every entity in every module/subroute whose `tenant_path` is `Some`,
  returning the **module chain** (top-level module + subroute segments) so the lint and
  testgen can locate the on-disk handler file.
- `endpoint_tenant_shape`'s `owns_tenant_entity` → `self.tenant_path(&e.name).is_some()`.

`collect_tenant_owned` (design.rs:1060) is replaced by the resolver walk.

### B. Route classification (unchanged rule, transitive inputs)

A route is `PathScoped { fk_param }` **iff its resolved path contains the tenant fk token**;
otherwise, if the entity is (transitively) tenant-owned, it is `MembershipSet`.

- Grandchild nested under its **parent** (`/accounts/{account_id}/contacts` — carries
  `account_id`, not `org_id`) → `MembershipSet`, scoped by the join-based membership methods.
- Grandchild mounted directly under the **tenant** (`/orgs/{org_id}/contacts`) → `PathScoped`.

No change to `TenantShape`'s variants; only the ownership input becomes transitive.

### C. Scoped query generation — JOIN up the chain (genroute.rs)

Both accessor families gain a join chain from `TenantPath`. Direct entities (empty `joins`)
emit **byte-identical** SQL to today.

**Membership-set reads** (`all_for_memberships` genroute.rs:1262, `get_for_memberships` :1276):

```sql
-- grandchild Contact → Account → Org
SELECT contact.* FROM contact
  JOIN account ON contact.account_id = account.id
WHERE account.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?)
ORDER BY contact.id
-- get_for_memberships adds:  WHERE contact.id = ? AND account.org_id IN (…)
```

**Path-scoped reads** (`all_for`/`get_for(tenant_id)`): same join, but the tenant predicate is
`WHERE account.org_id = ?` (the path tenant id) instead of `IN (SELECT … memberships)`.

**Membership-set writes** verify the **resolved** tenant, generalizing the #94 body-fk check:
- `create_for_memberships` (genroute.rs:1134): resolve the tenant from the body's **parent** fk
  (`account_id`) via the chain and verify ∈ memberships (403) **before** insert.
- `update_for_memberships` (:1156) / `remove_for_memberships` (:1180): resolve the **existing**
  row's tenant via the chain (WHERE `contact.id = ?`) and verify ∈ memberships (404 outside the
  set); path-id pinning (#92) preserved. If the body can change the parent fk, the **new**
  parent's tenant is verified too → a cross-tenant *move* is 403.

SQL is built with the existing `self.db.sql(...)` raw-statement path so backend quoting is
unchanged; join clauses are assembled from `TenantPath` (identifiers only — no user values in
the string).

### D. Steering comment (genroute.rs:316)

The grandchild handler stub carries no scoping guidance today. Because `endpoint_tenant_shape`
becomes transitive, the stub flows through the existing steering (genroute.rs:341‑367) that
names the scoped accessors — the agent is told to call the join-based `*_for_memberships` /
`*_for` method, never the unscoped repo call.

### E. JL0006 — AST-based, path-aware, fail-loud (lints.rs)

**Path resolution.** `scan_unscoped` takes the **module chain** from `tenant_owned()` and builds
the real nested handler path (top-level module dir + subroute segments), matching how the
scaffold writes nested routes — instead of the flat `crates/routes/{module}/src/handlers.rs`.

**AST detection (2b).** Replace the substring scan with a `syn` parse of the handler file and a
visitor that flags unscoped repo method calls (`.all()/.get()/.update()/.remove()`, plus
`.insert()` on a flat tenant module) on the repo binding, excluding the `*_for*` accessors. This
kills the multi-line-chain and aliasing false-negatives. The `// jerrycan:allow JL0006`
line-hatch is preserved (matched on the call's source span line).

**Fail-loud fallback.** If a handler that the design says is tenant-owned is **missing,
unreadable, or does not parse**, emit a loud diagnostic under a **new code `JL0008`**
("unscannable tenant-owned handler — JL0006 could not verify scoping") naming the file —
never `return` silently. `JL0008` is distinct from `JL0006` (found an unscoped call) because
the condition is different (could not scan at all); `JL0005`/`JL0008` are free, `JL0008` is
next. The guardrail can no longer fail closed-but-quiet, which is exactly what let #102/#103 ship.

### F. `JC0545` — ambiguous tenant path (codes.rs)

New design-time hard error (next free code after JC0544). Message names the entity, the tenant,
and both remedies: "entity `Contact` reaches tenant `Org` through more than one `belongs_to`
path; jerrycan will not guess which defines ownership — collapse to a single path, or split the
entity." Registered in `codes.rs` (~:219 sequence) and surfaced by the P‑A design validator like
JC0542/JC0543/JC0544.

### G. Isolation tests for transitive entities (testgen.rs)

Replace the direct-only bail (testgen.rs:886‑894) with a `tenant_path`-driven generator. For a
grandchild the test seeds the **intermediate chain** in tenant 1 before probing:

- `app()` already seeds Org 1 / Org 2 + memberships (`seed_second_tenant`, testgen.rs:835).
- Extend the raw-SQL seeder to insert each intermediate parent in tenant 1 (`Account(org_id=1)`),
  threading ids down the chain.
- Thread the seeded parent id into the nested mount (`/accounts/{seeded_id}/contacts`), create a
  Contact as user 1, then assert user 2 (Org 2 member, **not** Org 1) gets **404** on GET/DELETE
  and the row is absent from any flat list.

Driven entirely off `tenant_path`, so it works at any depth. WHY (Rule 9): the test encodes the
security contract — red on the unscoped accessor, green only on the join-based scoped accessor.

## Edges / risks (on record)

- **Nested-grandchild path param is advisory.** `/accounts/{account_id}/contacts/{id}` scopes by
  the membership join, not by `account_id`; a mismatched `account_id` is not a leak. Path/row
  consistency tightening is tracked, not in 0.5.1.
- **`JC0545` can reject a design that previously generated** (a silently-leaking diamond). This is
  a fix, not a regression — documented loudly in the CHANGELOG.
- **AST parse cost** on every handler: bounded by handler count; acceptable for `check`.
- **`tenant_path` must stop at the tenant** and not walk *past* it (the tenant's own
  `belongs_to`, if any, is irrelevant) — the DFS target is `tenancy.entity`, first reach wins per
  branch, ambiguity across branches is `JC0545`.

## Rollout

1. Land behind the normal gate; heavy conformance must include a 3-level-graph app.
2. Publish **0.5.1** (all 11 crates), path-dep-free smoke green.
3. CHANGELOG: security fix header — deep tenant graphs now scoped; `JC0545` added; JL0006 now
   AST-based and fail-loud. Note the (rare) previously-generating diamond design now errors.
4. Close #102 and #103; re-verify with a fresh cold-start agent on the CRM/chat shapes that
   round-4 broke.

## Success criteria

- A cold-start agent building an Org→Account→Contact app gets a **guard + join-scoped methods +
  isolation test** for `Contact`; the unscoped handler is **red**, the scoped handler **green**.
- JL0006 fires on a bare `repo.all()` in a **nested** handler and is silent on the scoped call.
- A missing/unparseable tenant-owned handler produces a **loud** diagnostic, never a silent pass.
- A diamond graph fails `jerrycan check` with `JC0545`.
- `cargo semver-checks` clean (framework API unchanged); direct-child apps generate
  byte-identical output.
- Every existing conformance/unit test green; new `tenant_path` unit tests cover
  direct/grandchild/great-grandchild/diamond/cycle.
