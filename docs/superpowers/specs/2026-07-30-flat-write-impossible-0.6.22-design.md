# Flat tenant writes: make-impossible + isolation test (0.6.22) — #97, #96

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #97 (SECURITY — for FLAT (Supabase-shape) tenant-owned entities, the security bar was steer+lint, NOT make-impossible: the bare unchecked `insert`/`update`/`remove`/`all`/`get` are still generated alongside the membership-checked `*_for_memberships`, so an agent CAN write a bare `repo.insert(body)` that skips the membership check — JL0006 only *flags* it. Per-user entities (#79) close this by CONSTRUCTION — the unscoped methods aren't generated. Bring flat tenant to parity.) + #96 (testgen: emit the flat-write ISOLATION test — a member of w1 gets 403 creating/moving a row into w2 — proven behaviorally in `migrate_membership_lossless.rs` but not in the per-app generated suite).
**Ships as:** 0.6.22 — a security make-impossible codegen change + a generated isolation test. Byte-identical for every non-flat entity; a FLAT tenant entity's repo loses its bare unscoped methods (the intended change).

## Root: the gating at genroute.rs:2501-2508
The repo emits unscoped `all`/`get` (`unscoped_reads`), `update`/`remove` (`unscoped_writes`), and the bare `insert` (`insert_body`, ALWAYS today). `per_user`/`public_read` already suppress these for #79/#105. FLAT tenant is not in the match, so a flat entity emits the full bare surface. The membership-scoped replacements — `all_for_memberships`, `get_for_memberships`, `create_for_memberships`, `update_for_memberships`, `remove_for_memberships` (both direct and transitive variants, genroute.rs:1593-1876) — are ALREADY emitted for a flat entity (`entity_is_flat_tenant_owned`), so the scoped set is COMPLETE. Suppressing the bare set leaves a safe, complete surface.

## A. #97 — suppress the bare methods for a flat tenant entity
At the repo emission (genroute.rs:2440-2508):
- `let flat_tenant = mode.db && mode.auth && entity_is_flat_tenant_owned(e, design);`
- **Suppress the bare `insert`** for a flat entity (UNLIKE per-user, which keeps `insert` — a per-user create is scoped by the server-INJECTED owner fk, safe; a flat create reads the tenant fk from the client BODY, so the bare `insert` is the leak — the create MUST go through `create_for_memberships`). So: `let insert = if flat_tenant { String::new() } else { insert_body };`
- **Suppress the unscoped reads + writes** for a flat entity (parity with per-user): extend the `(reads, writes)` decision so `flat_tenant ⇒ (String::new(), String::new())`. Keep the existing per_user/public_read arms unchanged.
- Result: a flat tenant entity's repo emits ONLY the `*_for_memberships` set (+ any non-suppressed shared methods). Non-flat entities are byte-identical.

Update the doc comment (genroute.rs:2441-2494) to state that flat tenant entities also suppress the bare surface (make-impossible, parity with #79) and WHY the bare `insert` is suppressed for flat but kept for per-user.

## B. THE CRITICAL RISK — reference-slice fixtures + the heavy gate (0.6.11 lesson)
reference-slice's `Lead` and `ApiKey` are FLAT tenant entities (direct-child MembershipSet). Their committed reference-fixture handlers (`conformance/eval/fixtures/reference/leads_handlers.rs`, `api-keys_handlers.rs`) MUST already call the `*_for_memberships` methods (the steer points there). **If any fixture handler calls a bare `repo.insert`/`update`/`remove`/`all`/`get` on a flat entity, suppressing it breaks the reference battery (a compile error in the migrated/scaffolded app) — exactly the 0.6.11 failure mode.** Before/while implementing:
1. Grep the reference fixtures for bare `repo.insert(`/`.update(`/`.remove(`/`.all(`/`.get(` on `Lead`/`ApiKey`; if found, the fixture RELIED on the bare method — update it to the scoped `*_for_memberships` (the correct, membership-checked call) as part of this change.
2. **Run the FULL heavy eval gate** (`reference_eval` especially — it scaffolds + serves reference-slice live) to catch any remaining bare-method reliance. The per-PR gate does NOT run reference_eval; it MUST be run locally before shipping.
Also grep genroute/testgen/dbgen TESTS for assertions that a flat entity emits `insert`/`update`/`remove`/`all`/`get` (e.g. genroute.rs:4333-4401 style) and update them to assert those are now SUPPRESSED for a flat entity (and still present for non-flat).

## C. #96 — the flat-write isolation test (testgen)
Extend the isolation-test emitter (testgen.rs) to emit, for a membership-set (flat) tenant-owned entity with a create endpoint, a WRITE-side isolation test proving a member of tenant A gets **403** creating (and, if an update route exists, moving) a row into tenant B. Model on the existing per-user/#79 isolation test and the membership seeds the acceptance suite already builds (the #107 member_app / the two-tenant seed). Name it `{ident}_flat_write_into_foreign_tenant_is_403` (or mirror the existing isolation-test naming). It POSTs a create with a body tenant fk NOT in the caller's membership set and asserts 403 (create_for_memberships's WITH-CHECK). Counted toward expected_failing per the isolation-test convention. **PG note:** the 403 is membership-check behavior; if a live PG proof is disproportionate, the generated-test-string assertion + the reference_eval battery (which drives cross-tenant 403/404 live) suffice — state which.

## D. Byte-identity + gates
- Non-flat entities (per-user, path-scoped-nested, non-tenant, the tenant root itself): byte-identical repo output. Prove with `determinism.rs` + base-vs-HEAD scaffold `diff -r` on a non-flat design.
- A flat entity's repo changes (bare methods removed) — the conformance/reference fixtures + any golden expectations update accordingly (that IS the feature).
- **Heavy eval gate (MANDATORY, 0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored`. Local PG container available for any migrate/realtime leg.
- `cargo fmt`/`clippy -D warnings`; `cargo semver-checks` (internal codegen — no public API change).

## Success criteria
- A flat tenant entity's generated repo emits NO bare `insert`/`update`/`remove`/`all`/`get` — ONLY the `*_for_memberships` set. An agent CANNOT write the unchecked cross-tenant write (make-impossible, parity with #79).
- The generated acceptance suite for a flat entity includes the write-side 403 isolation test (#96).
- Every non-flat entity is byte-identical; reference-slice still scaffolds + serves green (reference_eval); heavy gate green; published 0.6.22; #97 + #96 closed.

## Non-goals
- Changing per-user (#79) or path-scoped emission (unchanged). Suppressing reads on the tenant ROOT or non-tenant entities. The transitive-grandchild flat shape's method SQL (already correct, #102/#116) — this only changes which methods are EXPOSED.
