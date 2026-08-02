# testgen: greenable happy-path probes for same-module FK + tenancy-entity create/reserve (0.7.14) — #248 + #249

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 6 findings, testgen green-means-safe)
**Issues:** #248 (CREATE success probe seeds no belongs_to parents → same-module enforced FK 500, un-greenable) + #249 (tenancy entity's own create probe PK-collides with the auto-seeded tenant → 409; reserve probe seeds the counter at capacity → 409). Both: the app/codegen is correct; the GENERATED happy-path acceptance test can never go green, wedging the agent (a green-means-safe honesty defect — a `_returns_201`/`_returns_200` test that can never pass).
**Ships as:** 0.7.14 — a testgen fix (parent-seeding for the create probe + tenancy-create id + reserve counter default). Patch bump 0.7.13 → 0.7.14. Suites for shapes NOT hitting these cases are byte-identical.

## Part A (#248) — the create success probe seeds enforced same-module belongs_to parents
`seed_for_id_probe`/`seed_parents` (`crates/jerrycan/src/platform/testgen.rs`) already seed `belongs_to` parents before a GET `/{id}` probe "so an enforced intra-module FK resolves". The POST create success probe (`create_{entity}_returns_201`) does NOT. For a same-module enforced FK — which an fk-alias (#119) virtually always is — the create body posts `{from_account_id:1,to_account_id:1,…}` with no parent `Account` row → DDL FK violation → 500 (asserts 201).
**Fix:** before the create success probe's POST, seed the entity's enforced same-module `belongs_to` parents (reuse `seed_parents`, exactly as the `/{id}` probe does). Skip the identity fk (server-injected) and the tenancy entity (seeded by `tenant_seed`) — mirror `seed_parents`'s existing exclusions. A cross-module (unenforced) fk needs no seed (its `1` fixture points at no enforced row) — match the existing `/{id}`-probe behavior. Aliased fks (`from_account_id`/`to_account_id`) both seed the SAME parent entity (`Account`) — seed it once (id 1) and both fks point at it (or seed distinct rows if the probe needs distinct — a single `Account id=1` satisfies both `from=1,to=1`).
**Test:** an fk-alias same-module design (`Transfer belongs_to Account as from/to`) → the generated `create_transfer_returns_201` seeds an `Account` first; verify it would reach 201 (the create body's fks resolve). A design with no same-module enforced fk → byte-identical.

## Part B (#249) — the tenancy entity's create + reserve probes are greenable
In a module hosting the TENANCY entity, `app()` seeds a tenant `id=1` (for subroutes / membership mgmt). Then:
- **create:** `create_{tenant}_returns_201` reuses fixture `id=1` → PK collision with the seeded tenant → 409. **Fix:** the tenancy-entity create probe must POST a body whose pk does NOT collide with the auto-seeded tenant (use a distinct id, e.g. `2`, for the create probe's body / omit the pk so it autoincrements past the seed) — so the create reaches 201.
- **reserve:** the reserve success probe seeds the counter field with the generic integer fixture `1` instead of the field's declared `default` (e.g. `seats_used` should seed `0` per `default:0`, but seeds `1` = capacity) → reserve → 409. **Fix:** when seeding a row for a reserve probe (and generally when a field has a `default`), the seed must honor the field's `default` value, not the generic `1` fixture — so a counter with `default:0` is seeded `0` (not born at capacity) and the reserve reaches 200.
**Test:** a tenancy entity with a create route + a reserve route (counter `default:0`, `reserve_against` a capacity field) → `create_{tenant}_returns_201` uses a non-colliding pk, `reserve_..._returns_200` seeds the counter at `0` and reaches 200. Non-tenancy / no-reserve designs byte-identical.

## The invariant
Every generated `_returns_2xx` happy-path acceptance probe is GREENABLE by a correct handler — never blocked by a missing FK parent, a pk collision with an auto-seed, or a counter seeded at capacity. (A `_returns_2xx` test that can never pass is the green-means-safe inverse — the agent is wedged with a red suite that no handler work can fix.)

## Tests + Gates
- testgen units for both shapes (fk-alias same-module create; tenancy create + reserve). Existing conformance/reference_eval/eval isolation+happy-path tests still green (byte-identical for unaffected shapes — verify the reference-slice suites don't drift, or update the golden if a reference entity legitimately hits these cases).
- **Heavy eval gate** (`reference_eval`/`conformance`/`eval`/`genroute_compile --include-ignored`) + a scaffold-and-run check if feasible: build the #248/#249 shapes, migrate against PG, and confirm the previously-red create/reserve probes now pass with a correct (or trivially-correct) handler.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; determinism + embedded_sync.

## Version + success criteria
0.7.14. A same-module enforced-FK (fk-alias) create probe seeds its parents and can reach 201; a tenancy entity's create probe doesn't collide with the auto-seeded tenant, and its reserve probe seeds the counter at its `default` (not capacity) so it reaches 200. Unaffected shapes byte-identical; heavy gate green; published 0.7.14; #248 + #249 closed.

## Non-goals
- The flat-tenant-owned required_roles membership-role bug (#247) and the non-canonical mount refusal (#250) → 0.7.15. Any runtime/handler change (the handlers are correct — this is TEST seeding only).
