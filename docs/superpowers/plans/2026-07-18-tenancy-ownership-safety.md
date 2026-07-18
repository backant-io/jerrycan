# Ownership safety implementation plan (#78, #79, Supabase-lossless)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make tenancy and per-user scoping membership-verified and correct-by-construction — the cross-tenant leak (#78) and per-user leak (#79) become impossible, and Supabase-migrated many-membership apps become lossless.

**Architecture:** The tenant a request acts on comes from the route (path param, or a flat create's body) and MUST be in the caller's membership set, verified by generated code before scoping. Path-scoped routes verify the path's tenant; flat routes filter by the membership set (RLS-faithful). Per-user entities emit only owner-scoped repo methods. Spec: `docs/superpowers/specs/2026-07-18-tenancy-ownership-safety-design.md`.

**Tech Stack:** Rust; jerrycan generator (`crates/jerrycan/src/platform/{genroute,scaffold,testgen,lints}.rs`), core extractors (`crates/jerrycan-core/src/extract.rs`), docs (`docs/ai/14-tenancy.md`).

## PLAN-REVIEW NOTE (read before executing)

Planning surfaced one refinement to the spec that the reviewer/maintainer should see:

- **The path-aware guard needs a by-name path-param read**, which the framework does not expose today (`RequestCtx.params` is `pub(crate)`; `Path<T>` binds only the *last* param). Task 1 adds a small **additive** framework extractor for a named path param. This is additive-only → `cargo semver-checks` stays green; but it means the spec's "no framework public API change" is more precisely "**additive only**." No breaking change; the 0.5.0 release flag already covers the behavioral shift.
- A considered alternative (embed the membership check inside generated repo methods, drop `Dep<Tenant>` as a trust boundary, no framework API at all) is *also* viable and arguably more bypass-proof, but diverges further from the approved spec. This plan follows the spec's path-aware `Dep<Tenant>` design. If the maintainer prefers the repo-embedded approach, say so at plan review and Task 1/2 are re-scoped.

## Global Constraints

- Commits authored by the repo's git user (Pavel Hegler); NO Co-Authored-By/AI mentions; plain messages; body references issues (one `Fixes #N` per issue).
- **embedded_sync**: `docs/ai/*.md` edits copied byte-identical to `crates/jerrycan/embedded/ai/`; `docs/SKILL.md` to `.claude/skills/jerrycan-backend/SKILL.md`. `cargo test -p jerrycan --test embedded_sync` green.
- **Semver**: before each commit run `cargo semver-checks check-release -p jerrycan-core -p jerrycan-macros -p jerrycan-db -p jerrycan-auth -p jerrycan-validate -p jerrycan-observe -p jerrycan` — must be clean (additive framework API is allowed; a BREAKING change = STOP and report).
- **No-drift**: generator changes must keep output byte-identical for designs NOT exercising the new rule (scaffold both conformance designs base-vs-branch, `diff -r`; every intended hunk justified). Tenancy designs (reference-slice) WILL change — update its fixtures (Task 5) and prove the heavy suite green.
- **Heavy gate**: the reference-slice live battery runs in `heavy.yml`, not the per-PR gate. Tenancy-touching packages (1,2,4,5) must be dispatch-verified on `heavy.yml` GREEN before the release; note this in each PR.
- TDD: failing test first; RED/GREEN evidence in every task report. Full `cargo test -p jerrycan` green; fmt/clippy via pre-commit.
- STOP after each task's local commit — controller reviews, PRs, merges on verified-green gate.

## Key existing code shapes (verified anchors)

- Guard template: `scaffold.rs` ~40-95 — `struct Tenant { id, role }`, `fn tenant(user: CurrentUser, db: Dep<Db>)` doing `SELECT {fk}, role FROM {tenant}_members WHERE user_id = ?`, `None → Error::forbidden()`.
- Guard-vs-user param choice: `genroute.rs:171-177` (`endpoint_is_tenant_owned` → `_tenant: Dep<Tenant>` else `_user: CurrentUser`).
- Scoped repo methods: `genroute.rs` ~849-905 `fn scoped_methods` (`all_for/get_for/remove_for/update_for`, keyed on `fk_col`), gated on `design.tenancy` + `belongs_to == tenancy.entity`.
- Membership table DDL: `genroute.rs:1219-1250` (`{tenant}_members`, `idx_..._user_tenant`).
- Isolation test: `testgen.rs:818-852+` `fn isolation_test` — `return "" if design.tenancy is None`; requires a guarded `POST "/"` creator (nested creators degrade to empty).
- JL0006 lint: `lints.rs:125-140+` `fn lint_unscoped_tenant_queries` — scans handlers.rs for `repo.all()/get(/remove(/update(`, gated on `design.tenant_owned()`.
- Path extractor: `extract.rs:147` `Path<T>` (binds LAST param), `extract.rs:38` `params: Vec<(String,String)> pub(crate)`, `path_param!` macro `extract.rs:193`.
- Migrator: `migrate/rls.rs:16` `TenantMembership`, `migrate/crud.rs` flat routes (`GET /`, `GET /{id}`, ...), `migrate/tenancy.rs` `TableAccess::{Tenant,Owner,...}`.
- Error variants: `jerrycan::Error::not_found()` / `::forbidden()` both exist (used across genroute).

---

### Task 1: Named path-param extractor + route classification helpers

**Files:**
- Modify: `crates/jerrycan-core/src/extract.rs` (add a named path-param read)
- Modify: `crates/jerrycan/src/platform/design.rs` (route-shape classification helpers)
- Test: same-file `#[cfg(test)]` modules

**Interfaces produced (later tasks depend on these):**
- `RequestCtx::param(&self, name: &str) -> Option<&str>` — **pub**, additive. Returns the named captured path param.
- `Design::endpoint_tenant_shape(&self, module, ep) -> TenantShape` where `enum TenantShape { PathScoped { fk_param: String }, MembershipSet, Collection, None }` — pub(crate). `PathScoped` when the endpoint's resolved path (mount + ep.path) contains the tenant fk param; `Collection` for the tenant entity's own `POST "/"` / `GET "/"`; `MembershipSet` for a tenant-owned entity whose route carries no tenant fk param; `None` for non-tenant endpoints.

- [ ] **Step 1: Failing test for `RequestCtx::param`**

In `extract.rs` tests, add:
```rust
#[test]
fn ctx_param_reads_a_named_captured_param() {
    let mut ctx = RequestCtx::test_blank(); // use the existing test constructor pattern (see handler.rs:95 which pushes ctx.params)
    ctx.params.push(("club_id".into(), "42".into()));
    ctx.params.push(("id".into(), "7".into()));
    assert_eq!(ctx.param("club_id"), Some("42"));
    assert_eq!(ctx.param("id"), Some("7"));
    assert_eq!(ctx.param("missing"), None);
}
```
(If `RequestCtx` has no public test constructor, mirror the construction used at `handler.rs:95-112`.)

- [ ] **Step 2: Run it — expect FAIL** (`method param not found`). Run: `cargo test -p jerrycan-core param -- --nocapture`.

- [ ] **Step 3: Implement `RequestCtx::param`** in the `impl RequestCtx` block (`extract.rs:45`):
```rust
    /// The named path parameter captured by the router, if present. Unlike
    /// `Path<T>` (which binds the leaf-most param), a guard can address a specific
    /// mount param by name — e.g. the tenant fk `club_id` under `/clubs/{club_id}`.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
```

- [ ] **Step 4: Run test — expect PASS.**

- [ ] **Step 5: Failing tests for `endpoint_tenant_shape`**

In `design.rs` tests, add cases over a small tenancy design (Club tenant, Book tenant-owned, member_roles):
```rust
#[test]
fn tenant_shape_classifies_by_route() {
    let d = /* tenancy design: tenant Club; module clubs endpoints POST "/", GET "/", GET "/{club_id}", DELETE "/{club_id}"; module books mounted "/clubs/{club_id}" endpoints POST "/", GET "/", GET "/{id}"; flat module customers endpoints GET "/", GET "/{id}" tenant-owned */;
    // Collection:
    assert!(matches!(d.endpoint_tenant_shape(clubs, post_root), TenantShape::Collection));
    assert!(matches!(d.endpoint_tenant_shape(clubs, get_root), TenantShape::Collection));
    // PathScoped (tenant's own detail + nested):
    assert!(matches!(d.endpoint_tenant_shape(clubs, get_id), TenantShape::PathScoped { .. }));
    assert!(matches!(d.endpoint_tenant_shape(books, get_book_id), TenantShape::PathScoped { .. }));
    // MembershipSet (flat tenant-owned, no tenant param in path):
    assert!(matches!(d.endpoint_tenant_shape(customers, get_customer_id), TenantShape::MembershipSet));
}
```
Reuse the design-construction helpers already in `design.rs` tests (e.g. the `V1_FULL`/tenancy fixtures around `design.rs:980-1035`).

- [ ] **Step 6: Run — expect FAIL** (no such method). 

- [ ] **Step 7: Implement `TenantShape` + `endpoint_tenant_shape`** in `design.rs`. The resolved path = `module.effective_mount()` (trim trailing `/`) + `ep.path`. `fk_param = fk_column(tenancy.entity)` (e.g. `club_id`). Classification:
  - non-tenant module/entity → `None`;
  - tenant entity's own module, `ep.path == "/"` (POST or GET) → `Collection`;
  - resolved path contains `{fk_param}` (OR, for the tenant entity's own detail route, `{id}` — treat the tenant's own `/{id}` as PathScoped with `fk_param` = the tenant fk) → `PathScoped { fk_param }`;
  - tenant-owned entity, no tenant param in resolved path → `MembershipSet`.
  Add the doc-comment explaining the classes. (Note the tenant's-own-`/{id}` case here rather than renaming the route param — avoids the normalization ripple flagged in the spec.)

- [ ] **Step 8: Run tests — expect PASS.**

- [ ] **Step 9: Semver + suite + commit**

Run the 7-crate semver command (expect additive `RequestCtx::param` → clean). Run `cargo test -p jerrycan-core -p jerrycan`. Commit:
```bash
git add crates/jerrycan-core/src/extract.rs crates/jerrycan/src/platform/design.rs
git commit -m "core+design: named path-param read + route tenant-shape classification (scaffolding for #78)"
```

---

### Task 2: Path-aware + membership-set scoping engine (the core of #78)

**Files:**
- Modify: `crates/jerrycan/src/platform/scaffold.rs` (guard template → path-aware, 404)
- Modify: `crates/jerrycan/src/platform/genroute.rs` (`scoped_methods` → membership-set variants; guard-param choice per shape)
- Test: `genroute.rs` `#[cfg(test)]` + a `genroute_compile`-style fixture

**Interfaces:**
- Consumes: `Design::endpoint_tenant_shape`, `RequestCtx::param` (Task 1).
- Produces: generated guard `fn tenant(user, db, ctx?)` that verifies membership for the **path** tenant and 404s on miss; scoped repo methods `all_for_memberships`/`get_for_memberships`/... for the flat shape.

- [ ] **Step 1: Failing test — path-aware guard SQL + 404**

In `scaffold.rs`/`genroute.rs` tests assert the emitted guard: (a) resolves the tenant fk from the request path (via `ctx.param(fk_col)`), (b) queries `WHERE user_id = ? AND {fk} = ?`, (c) returns `Error::not_found()` (not `forbidden`) on no membership row. Pin the exact substrings:
```rust
assert!(guard.contains("ctx.param(\"club_id\")"));
assert!(guard.contains("WHERE user_id = ? AND club_id = ?"));
assert!(guard.contains("Error::not_found()"));
```

- [ ] **Step 2: Run — expect FAIL** (current guard is user-only, 403).

- [ ] **Step 3: Rewrite the guard template** (`scaffold.rs` ~68-92). The factory signature gains the request ctx access needed for `param`. Because a DI factory's args are `FromRequest`, add a small `FromRequest` newtype OR have the factory read the tenant fk via a generated `Path`-free path: the cleanest is to make the factory take the ctx. Concretely — the factory reads the fk from the path and verifies membership:
```rust
pub async fn tenant(
    user: CurrentUser,
    db: jerrycan::Dep<jerrycan::db::Db>,
    ctx: &jerrycan::extract::RequestCtx,   // scout the exact factory-ctx access pattern; see dep.rs factory macro
) -> jerrycan::Result<Tenant> {
    let {fk_col} = ctx.param("{fk_col}")
        .ok_or_else(jerrycan::Error::not_found)?;   // no tenant in path on a guarded route → 404
    // SELECT role FROM {tenant}_members WHERE user_id = ? AND {fk_col} = ?
    //   None row → Error::not_found()   (no existence leak)
    //   found    → Tenant { id: {fk_col-parsed}, role }
}
```
**SCOUT FIRST (dep.rs:180-210):** confirm how a DI factory obtains `&RequestCtx` (the factory macro resolves each arg via `FromRequest`; either add a `FromRequest for &RequestCtx`-style accessor, or pass a small `PathParams` `FromRequest` newtype exposing `.get(name)`). Pick the minimal additive shape; document it. Keep `require_role` unchanged (403 path).

- [ ] **Step 4: Run guard test — expect PASS.**

- [ ] **Step 5: Failing test — membership-set scoped methods (flat shape)**

Assert `scoped_methods` (or a new `membership_set_methods`) emits, for a tenant-owned entity on a flat route:
```rust
assert!(src.contains("WHERE"));  // then specifically:
assert!(src.contains("workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id"));
```
for `all_for_memberships(user_id)` (list across the set) and `get_for_memberships(user_id, id)` (adds `AND id = ?`, 404 outside set). Use sea-orm's `Condition`/subquery or a raw `Statement` mirroring the existing guard's raw-SQL style (`db.sql(...)`).

- [ ] **Step 6: Run — expect FAIL.**

- [ ] **Step 7: Implement the flat-shape methods** in `genroute.rs scoped_methods`. Emit BOTH families: keep `all_for(fk)/get_for(fk,id)/...` (used by path-scoped handlers where the tenant is known) AND add `*_for_memberships(user_id[, id])` (used by flat handlers). The membership-set query filters `{fk} IN (SELECT {fk} FROM {tenant}_members WHERE user_id = ?)`.

- [ ] **Step 8: Wire the guard-param choice per shape** (`genroute.rs:171-177`). Using `endpoint_tenant_shape`:
  - `PathScoped` → `_tenant: Dep<Tenant>` (guard verifies the path tenant); handler scopes via `all_for(tenant.id())`.
  - `MembershipSet` → `_user: CurrentUser`; handler uses `*_for_memberships(user.0.id)`; stub comment says so.
  - `Collection` → `_user: CurrentUser` (create/list handled in Task 3).
  Update the generated handler stub comments (`genroute.rs` stub-comment sites) to name the exact scoped method for each shape.

- [ ] **Step 9: Run tests; e2e-lite** — scaffold a path-nested tenancy design (BookClubs shape) + a flat tenancy design into temp dirs (path-dep from crates.io is fine, or local path); `cargo build`; confirm compile. Assert (fixture-level) the two handler shapes reference the right methods.

- [ ] **Step 10: No-drift check** — scaffold both conformance designs base-vs-branch; reference-slice (tenancy) WILL change (guard SQL + methods) — capture the diff for Task 5; todo-api (no tenancy) must be byte-identical.

- [ ] **Step 11: Semver + suite + commit**
```bash
git add crates/jerrycan/src/platform/scaffold.rs crates/jerrycan/src/platform/genroute.rs crates/jerrycan-core/src/*.rs
git commit -m "generator: membership-verified tenant scoping — path-aware guard (404) + membership-set methods (Fixes #78 core)"
```

---

### Task 3: Auto-seeded membership + membership-filtered tenant list

**Files:**
- Modify: `crates/jerrycan/src/platform/genroute.rs` (tenant-create wraps membership insert; tenant-list filters by membership)
- Test: `genroute.rs` tests + e2e-lite

**Interfaces:**
- Consumes: `TenantShape::Collection` (Task 1), the `{tenant}_members` DDL (genroute.rs:1219).
- Produces: generated `POST /clubs/` inserts the club + a `club_members(user, club, first_role)` row in one transaction; `GET /clubs/` returns membership-filtered clubs.

- [ ] **Step 1: Failing test — create seeds membership**

Assert the generated tenant-create path (repo method or handler wrapper) inserts into `{tenant}_members` with the session user id, the new tenant id, and `member_roles[0]`, in one transaction with the tenant insert.
```rust
assert!(src.contains("club_members"));
assert!(src.contains("role")); // first member_role literal, e.g. "organizer"
```

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Implement** a generated `create_with_membership(user_id, item) -> Result<Tenant-id>` on the tenant repo (or emit the transaction in the create handler's generated portion), using `member_roles.first()`. Prefer the repo method (agent-owned handler calls it) so the membership seed can't be dropped. One DB transaction: insert tenant (returning id) → insert membership.

- [ ] **Step 4: Failing test — list filters by membership**

Assert `GET /clubs/`'s generated list method joins `{tenant}_members`:
```rust
assert!(src.contains("JOIN club_members") || src.contains("club_members"));
assert!(src.contains("WHERE") && src.contains("user_id"));
```

- [ ] **Step 5: Run — expect FAIL. Step 6: Implement** the membership-filtered tenant list (`SELECT c.* FROM clubs c JOIN club_members m ON m.{fk}=c.id WHERE m.user_id = ?`).

- [ ] **Step 7: e2e-lite** — scaffold BookClubs shape; drive: create club (201) → immediately GET /clubs/ returns it (membership works without a hand-written insert) → a second user's GET /clubs/ is empty. Log it.

- [ ] **Step 8: No-drift, semver, suite, commit**
```bash
git add crates/jerrycan/src/platform/genroute.rs
git commit -m "generator: auto-seed creator membership on tenant create + membership-filtered tenant list (#78)"
```

---

### Task 4: Per-user make-impossible (#79) + isolation tests for all shapes

**Files:**
- Modify: `crates/jerrycan/src/platform/genroute.rs` (suppress unscoped repo methods for guarded identity-owned entities; emit owner-scoped)
- Modify: `crates/jerrycan/src/platform/testgen.rs` (`isolation_test` generalized to path-scoped nested / membership-set / per-user)
- Modify: `crates/jerrycan/src/platform/lints.rs` (JL0006 generalized or retired as scoping becomes structural)
- Test: genroute + testgen tests + e2e-lite

**Interfaces:**
- Consumes: `endpoint_tenant_shape`, the scoped-method families (Task 2).
- Produces: for a guarded entity `belongs_to` the identity user, the repo emits ONLY `all_for(user_id)/get_for/remove_for/update_for`; the unscoped `all()/get()/...` are absent. Isolation tests for every ownership shape.

- [ ] **Step 1: Failing test — unscoped methods absent for guarded identity-owned entity**

For a design with `Workout belongs_to User` (identity) and guarded endpoints:
```rust
assert!(!repo_src.contains("pub async fn all(&self)"));   // unscoped list NOT emitted
assert!(repo_src.contains("pub async fn all_for(&self, user_id"));
```
And a non-guarded / non-identity entity is unchanged (still has `all()`).

- [ ] **Step 2: Run — expect FAIL** (unscoped `all()` currently always emitted).

- [ ] **Step 3: Implement** the suppression: when `mode.auth && entity is guarded on its routes && entity.belongs_to includes the identity entity`, emit owner-scoped methods keyed on `user_id` and do NOT emit the unscoped variants. Reuse `Design::fk_column(identity_entity)` for the column. Guarded handlers pass `user.0.id`. Update stub comments to show `repo.all_for(_user.0.id)`.

- [ ] **Step 4: Failing tests — isolation tests emitted for each shape**

Generalize `isolation_test` (testgen.rs:824 `return "" if design.tenancy is None`). Assert it now emits:
```rust
// per-user (no tenancy block): two users, user B cannot see user A's row (404/absent)
// path-scoped nested creator: member of club A cannot GET club B's book (404)
// membership-set flat: member of workspace A cannot get/{id} a workspace-B row (404)
```
Pin the presence of each generated test fn name + the cross-owner 404 assertion.

- [ ] **Step 5: Run — expect FAIL. Step 6: Implement** the generalized emitter: drop the `design.tenancy` early-return; branch on ownership shape (per-user identity, path-scoped, membership-set); for the nested-creator case, seed via the parent path (reuse R2's `seed_parents`/`collection_path` machinery). Ensure `expected_failing` accounting stays correct (these tests PASS on correct scaffolds — they're not red-baseline probes; verify against conformance.rs pins).

- [ ] **Step 7: JL0006** — with unscoped methods suppressed (per-user) and membership verification structural (tenant), the lint's role shrinks. Either (a) generalize it to also cover identity-owned modules as a belt-and-suspenders, or (b) narrow/retire it where the method no longer exists to be misused. Decide based on what remains reachable; document in the report. Keep any lint change no-drift for compliant designs.

- [ ] **Step 8: e2e-lite** — scaffold FitnessLog (per-user) + BookClubs (path) + a flat design; run each generated acceptance suite; the isolation tests pass on correct handlers and the unscoped method genuinely doesn't compile if referenced.

- [ ] **Step 9: No-drift, semver, suite, commit**
```bash
git add crates/jerrycan/src/platform/genroute.rs crates/jerrycan/src/platform/testgen.rs crates/jerrycan/src/platform/lints.rs
git commit -m "generator: per-user leak made impossible + isolation tests for every ownership shape (Fixes #79)"
```

---

### Task 5: Docs rewrite, reference-slice fixtures, Supabase-migration test, heavy green

**Files:**
- Modify: `docs/ai/14-tenancy.md` (+ embedded twin), `.claude/skills/jerrycan-backend/SKILL.md` twin if it references the old model
- Modify: `conformance/eval/fixtures/reference/*` handlers (to the membership-verified guard)
- Modify/Add: `crates/jerrycan/tests/migrate_*.rs` or `reference_eval.rs` (Supabase membership-set test)
- Test: heavy conformance

**Interfaces:** consumes everything above; produces green heavy suite + truthful docs.

- [ ] **Step 1: Rewrite `docs/ai/14-tenancy.md`** — remove "`tenant.id()` is trusted"; document: membership-verified scoping, many-membership support, path-scoped vs flat (Supabase) shapes, auto-seeding, 404-vs-403. Copy byte-identical to the embedded twin; update the SKILL twin if it echoes the old model. `cargo test -p jerrycan --test embedded_sync` green.

- [ ] **Step 2: Update reference-slice fixtures** — the hand-written `conformance/eval/fixtures/reference/*` handlers to the new scoped methods/guard (they are the "agent implements" step of the heavy test). Use the Task-2 diff captured in the no-drift step as the guide.

- [ ] **Step 3: Supabase-migration test** — add a test asserting a recognized `TenantMembership` RLS policy (see `migrate/live.rs:404` for the policy shape) → a scaffolded app where a user who is a member of TWO tenants sees BOTH tenants' rows via list, and a `get/{id}` on a row outside their membership set 404s. This is the lossless-migration proof. Mirror the existing `migrate_*.rs` / `reference_eval.rs` harness patterns.

- [ ] **Step 4: Local heavy proof** — run the reference battery single-threaded locally: `cargo test -p jerrycan --all-features --test reference_eval -- --include-ignored --test-threads=1` — GREEN before committing (the harness compiles a scaffolded app; use the shared target dir).

- [ ] **Step 5: Commit**
```bash
git add docs/ai/14-tenancy.md crates/jerrycan/embedded/ai/14-tenancy.md .claude/skills/jerrycan-backend/SKILL.md conformance/eval/fixtures/reference crates/jerrycan/tests
git commit -m "docs+conformance: membership-verified tenancy — 14-tenancy rewrite, reference fixtures, Supabase-lossless test (#78, #79)"
```

- [ ] **Step 6: Post-merge** — after all tasks merge, dispatch `heavy.yml` and require GREEN before any release; flag 0.5.0 for the generated-app behavioral change.

---

## Self-review checklist (controller, before dispatch)
- Spec coverage: path-scoped (T2), membership-set/Supabase (T2 methods + T5 test), auto-seed+list (T3), per-user make-impossible (T4), isolation tests all shapes (T4), docs+fixtures (T5). ✓
- The by-name extractor refinement is surfaced in the PLAN-REVIEW NOTE. ✓
- Types consistent: `TenantShape`, `endpoint_tenant_shape`, `RequestCtx::param`, `all_for`/`*_for_memberships` used consistently across T1→T4. ✓
