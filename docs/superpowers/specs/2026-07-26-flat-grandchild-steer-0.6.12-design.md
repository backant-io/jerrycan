# Align the flat-membership-method emission gate with the steer (0.6.12) — #116

**Date:** 2026-07-26
**Status:** Approved design, pre-implementation
**Issues:** #116 (HIGH — the framework's OWN generated guidance is a compile error). A tenant-owned handler stub's steer comment (genroute.rs `tenant_scope_comment`, ~:362-368) tells the builder to call `create_for_memberships` / `update_for_memberships` / `remove_for_memberships`, but for a **transitively-owned grandchild whose flat write endpoint lives in an entity-less subroute**, `repo.rs` does not emit those methods — so following the framework's own steer is a `method not found` compile error behind a green `check`.
**Ships as:** 0.6.12 — an internal codegen fix (no contract/API change, no new JC code). Byte-identical for every design that compiles today; the only output delta is the previously-broken flat-grandchild-under-entity-less-subroute shape, which now emits the methods its steer already references.

## Root cause (precise — verified by reading the source)
The steer and the method-emission gate scan **different endpoint sets**:
- **Steer** — `tenant_scope_comment` (genroute.rs:317) fires per-endpoint whenever `design.endpoint_tenant_shape(m, ep) == TenantShape::MembershipSet` for a tenant-owned write, computed with the endpoint's OWN module context `m` (so it fires for endpoints in **subroutes** too, with the subroute as `m`).
- **Emission gate** — `entity_is_flat_tenant_owned` (genroute.rs:1365) decides whether the repo emits `*_for_memberships`. It finds the module that **declares** the entity (`m.entities` contains it) and iterates **only that module's top-level `m.endpoints`** — it never descends into `m.subroutes[].endpoints`, and never looks at other modules. So when the grandchild's flat write lives in an entity-less subroute (or any module other than the declaring one's top level), the gate sees no `MembershipSet` endpoint → returns `false` → methods are NOT emitted, while the steer (per-endpoint) still fires. Mismatch → compile error.

**The methods themselves are already correct for grandchildren.** The transitive write variant (`membership_writes`, genroute.rs:1543-1605, issue #102) resolves the tenant from the body's immediate-parent fk and JOINs up the chain; `all_for_memberships`/`get_for_memberships` likewise have working `path.joins`-non-empty variants. So this is purely a gate/steer **scan-domain** mismatch — no new machinery.

## Fix
### A. Broaden `entity_is_flat_tenant_owned` to the steer's domain (genroute.rs:1365)
Replace the "declaring module's top-level endpoints only" scan with a scan over **every endpoint on the entity across all modules and all (nested) subroutes**, applying the SAME per-endpoint rule with each endpoint's OWN module context:
- for each `(m, ep)` in the recursive walk of `design.modules` + their `subroutes` (all depths): if `endpoint_repo_entity(m, ep) == Some(e.name)`, classify `design.endpoint_tenant_shape(m, ep)` — any `PathScoped` ⇒ return `false` (conservative, unchanged); any `MembershipSet` ⇒ `flat = true`.
- keep the `tenant_path(&e.name).is_none() ⇒ false` short-circuit (unchanged).
Use the same recursion shape as `check_public_on_tenant_owned` (questions.rs:1053-1059) — a helper that recurses `m.subroutes`. **Critical:** call `endpoint_tenant_shape`/`endpoint_repo_entity` with the endpoint's OWN module (the subroute), because the mount path is what makes a nested `/{fk}` route `PathScoped` vs a flat route `MembershipSet` — using the wrong module context would misclassify.

### B. Correct the stale comment (genroute.rs ~1401-1402)
`scoped_methods`' comment says "The JOIN SQL for a grandchild's filter is Tasks 3/4; this only recognizes ownership." The JOIN SQL was implemented in #102 (the `membership_writes` transitive branch + the `all_for_memberships`/`get_for_memberships` non-empty-joins variants). Update the comment to state the transitive JOIN is emitted (do not leave a false "unimplemented" marker). Surgical comment-only edit.

## Reproduction + compile proof (the acceptance criterion IS compilation)
Add a unit test with a **minimal reproduction design**: a tenancy root `Org`, a direct child `Board belongs_to Org`, a transitive grandchild `Card belongs_to Board`, and a flat write endpoint on `Card` (POST/PUT/DELETE) hosted in an **entity-less subroute** (mount with no `entities`, path carries no tenant fk → `MembershipSet`). Assert:
1. `entity_is_flat_tenant_owned(Card, design) == true` (was `false`).
2. The generated `Card` repo CONTAINS `create_for_memberships`/`update_for_memberships`/`remove_for_memberships` (grep the emitted repo string).
3. The steer for the subroute write endpoint references `Card`Repo::create_for_memberships (already fires) — confirm gate ⊇ steer.
4. **Compile proof:** scaffold the reproduction design, write a handler that FOLLOWS the steer (calls `CardRepo::create_for_memberships(_user.0.id, card)`), and `jerrycan check` (or `cargo build`) is GREEN. Model on the existing scaffold-and-compile tests (e.g. `tests/eval.rs` / `tests/conformance.rs` live-scaffold precedents). This is the test that would have caught #116 and must fail before the §A change.

Regression asserts (no false flip): a DIRECT flat child stays `flat == true`; a PATH-SCOPED nested child (subroute `/{fk}/…`) stays `flat == false` (byte-identical); a non-tenant entity stays `false`.

## Byte-identity / gates
- No conformance design has the flat-grandchild-under-entity-less-subroute shape, so `determinism.rs` + the base-vs-HEAD scaffold `diff -r` stay green (the broadened scan changes output ONLY for the new reproduction shape). Confirm.
- **Run the heavy eval gate before calling it done** (0.6.11 lesson): `cargo test -p jerrycan --all-features --test reference_eval -- --include-ignored`, `--test conformance -- --include-ignored`, `--test eval -- --include-ignored`. The per-PR `gate` `#[ignore]`s these.
- `cargo fmt`/`clippy -D warnings` clean; `cargo semver-checks` clean (internal fn, no public-API change).

## Success criteria
- The flat-grandchild-under-entity-less-subroute design: `entity_is_flat_tenant_owned == true`, the repo emits `*_for_memberships`, a steer-following handler COMPILES (was: `method not found`).
- Direct flat child, path-scoped nested child, non-tenant entity: unchanged (byte-identical).
- Heavy eval gate green; determinism green; semver clean; published 0.6.12.

## Non-goals
- Mixed-shape entities (both a PathScoped and a flat route on the same entity) remain out of scope — no such design exists today (the conservative "any PathScoped ⇒ not flat" rule is retained). If one is ever authored, it needs both method sets — a separate decision.
- The genroute residual #171 (anonymous entity-less GET gets a lenient repo) is unrelated.
