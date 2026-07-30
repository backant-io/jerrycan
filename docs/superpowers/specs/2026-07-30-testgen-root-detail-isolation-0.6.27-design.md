# Non-member-404 probe for the tenant ROOT's own detail route (0.6.27) — #172

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #172 (testgen parity gap, green-means-safe. Child/grandchild tenant entities get a cross-tenant **detail-route 404** isolation probe (`tenant_owned_isolation_test`, testgen.rs:1581 — user 2 does `GET /clubs/{id}/books/{id}` on tenant 1's row → 404). The tenant ROOT entity itself (`tenancy.entity`, e.g. `Workspace`) is SKIPPED by that finder because its `design.tenant_path(root)` is `None` (the root does not `belongs_to` itself, testgen.rs:1592-1598). `tenant_collection_isolation_test` (testgen.rs:2077) covers only the root's **collection** (`GET /workspaces/` — user 2's list is empty), NOT its **detail** route. So a regression where the root's `GET /{id}` handler uses the bare `get` instead of `get_for_memberships` — leaking any tenant's root row to a non-member — passes every generated test. The root detail handler already uses `{Entity}Repo::get_for_memberships` (genroute.rs:378); this adds the test that keeps it honest.)
**Ships as:** 0.6.27 — a testgen-only addition: one more generated acceptance probe for a db+auth+tenancy design whose root module exposes a guarded `GET /{id}`. Byte-identical for every other design.

## The gap
For the tenant root module (the module DECLARING `tenancy.entity`):
- `tenant_owned_isolation_test` → skips (root has `tenant_path == None`).
- `tenant_collection_isolation_test` → asserts user 2's LIST is empty (collection), not that `GET /root/{id}` 404s (detail).
- Result: the root's own detail route has NO cross-tenant isolation probe.

## The fix
Emit a probe that, after user 1 creates a root row (id captured), asserts **user 2 (a member of tenant 2 only — the second user `app()` seeds) gets 404 on `GET /{root_base}/{id}`** (tenant 1's row). Model it on the child detail probe (the `foreign` leg, testgen.rs:1693-1697):
```rust
let foreign = t.get_with(&format!("{base}/{{id}}"), &[("{hk}", &test_cookie_for(2))]).await;
assert_eq!(foreign.status().as_u16(), 404, "cross-tenant get on the tenant root must 404 (use get_for_memberships, not get); body: {}", foreign.text());
```
**WHY (Rule 9):** the probe is RED on a fresh scaffold's agent stub (500) AND RED if the handler uses the unscoped `get` (200 — the leak), and turns GREEN only when the detail handler uses `get_for_memberships` (a non-member ⇒ `None` ⇒ 404). It encodes the root-entity tenant-isolation contract that `get_for_memberships` exists to enforce.

### Placement (implementer's choice — pick the cleaner)
- **Preferred — reuse the existing seed:** add the detail-404 leg inside `tenant_collection_isolation_test`, which already creates a tenant-1 root row via user 1 and captures `id_value`, guarded by an ADDITIONAL check that the module has a guarded `GET /{id}` detail route (`ep.method == GET && single `{id}` param && is_guarded()`). If the root module has a guarded detail route, append the `foreign`-style 404 leg (using `id_value` + `test_cookie_for(2)` + the mount base). If it has no detail route, emit nothing new (the existing list assertions are unchanged — byte-identical).
- **Alternative:** a new sibling `tenant_root_detail_isolation_test(design, module)` wired into `isolation_test` (testgen.rs:1556), gated independently on: db+auth+tenancy, the module declares `tenancy.entity`, a guarded creator at `/`, and a guarded `GET /{id}` detail route. Use this if bolting onto `tenant_collection_isolation_test` would entangle the list-gate (which early-returns when there is no guarded list) with the detail probe — the detail probe must emit even for a root module that has a detail route but no list.

Do NOT change `tenant_owned_isolation_test` or the child/grandchild coverage — this is purely the root-entity addition.

## expected_failing registration (MUST — or the conformance gate breaks)
The generated acceptance suite has an `expected_failing` manifest: the conformance gate asserts EXACTLY the stub-probing tests are RED on a fresh scaffold. This new probe hits the root's AGENT-STUB detail handler → it is RED on a fresh scaffold, exactly like `tenant_collection_isolation_test` and `tenant_owned_isolation_test`. Register it in `expected_failing` the SAME way those sibling isolation tests are registered (find how they are listed — grep `expected_failing` in testgen.rs — and add the new test fn name by the same mechanism). If you reuse `tenant_collection_isolation_test` (Preferred), the existing fn is already registered, so confirm the added leg does not create a NEW test fn that needs separate registration (a leg inside the same `#[tokio::test]` needs none; a new fn does). Getting this wrong makes the conformance gate's "these N fail" assertion off-by-one — run the heavy gate to confirm.

## Tests
- **testgen unit/golden:** for a db+auth+tenancy design whose root module has a guarded `GET /{id}`, the generated acceptance file contains the root-detail cross-tenant 404 probe (asserts 404 with `test_cookie_for(2)` against the root base + created id). For a design WITHOUT a root detail route (or no tenancy), NO such probe is emitted (byte-identity witness).
- **Heavy eval gate (the real proof, 0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored`. The reference-slice has a tenancy root (`Workspace`) with a guarded detail route, so: (a) the new probe is emitted, (b) it lands in `expected_failing` on the stub scaffold, (c) once the reference-slice's IMPLEMENTED root handler uses `get_for_memberships`, the probe is GREEN in the live battery. Confirm the `expected_failing` count matches (no off-by-one) and the implemented battery stays green.

## Gates
- `cargo test -p jerrycan` (testgen units) green.
- `reference_eval` + `conformance` + `eval` `--include-ignored` green — including the `expected_failing` count.
- `cargo fmt`/`clippy -D warnings`; determinism (generated test file is a deterministic string); no byte drift for non-tenancy designs.

## Success criteria
- A db+auth+tenancy design whose root module exposes a guarded `GET /{id}` gets a generated probe asserting a non-member (tenant 2's user) receives 404 on the root's own detail route — RED on the stub / on unscoped `get`, GREEN only with `get_for_memberships`.
- `expected_failing` stays exact (conformance gate green); non-tenancy / no-root-detail designs byte-identical; published 0.6.27; #172 closed.

## Non-goals
- Changing child/grandchild isolation coverage or `get_for_memberships` itself. A cross-tenant probe for the root's UPDATE/DELETE (this issue is the read/detail-404 parity; a write probe can be a follow-up if the collection/flat-write tests don't already cover it). Subroute-nested tenant roots (out of scope, as elsewhere).
