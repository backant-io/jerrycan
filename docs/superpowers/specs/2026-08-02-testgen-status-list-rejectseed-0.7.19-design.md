# codegen: 201-list responder + id-echo bodyless-status + reject-probe membership seed (0.7.19) — #265 + #266 + #267

**Date:** 2026-08-02
**Status:** Approved design (AUDIT round 10, test-generator family — all green-means-safe, no runtime-app defects)
**Issues:** #265 (201+list:true create emits Created<Item> single, violating the array contract — silent false green), #266 (create id-echo panics on a bodyless 204/3xx success — un-greenable), #267 (reject probes on a tenant-entity's own path-scoped route seed no membership → 404 not 422 — un-greenable, #236/#248/#260 family).
**Ships as:** 0.7.19 — genroute (#265) + testgen (#266, #267). Patch bump 0.7.18 → 0.7.19. Unaffected shapes byte-identical.
**MANDATORY:** run `reference_slice_live_battery` (--include-ignored, live PG) — the reference-slice has tenant-entity routes + import_leads; the #265/#267 changes could touch it.

## Part A (#265) — a 201 + list:true create emits a list responder
`genroute::return_type` (`genroute.rs:52`): `(201, Some(e), _) => Result<Created<{e}>>` ignores `success.list`. Branch it on `ep.success.list`: a list → `Created<Vec<{e}>>` (confirm jerrycan-core `Created<T>` accepts `T = Vec<X>` — it should, `Created<T: Serialize>`; if `Created` is entity-specialized, use the same list responder the 200/202 arms use with the 201 status). Byte-identical for a non-list 201 create. Confirm OpenAPI (already array) now matches the handler type + the #263 testgen probe (asserts status only for a list) stays green.

## Part B (#266) — id-echo only for a body-bearing success status
`testgen.rs:966` id-echo gate is `(ep.method == POST && !ep.success.list)`. Add a body-bearing-status check: only emit the id-echo when `success.status` returns a JSON body — i.e. a 2xx that is NOT 204 and is < 300 (200/201/... but not 204, not 3xx). A 204 (`NoContent`) or 3xx (`Redirect`) has an empty body, so `from_str(&res.text())` would panic. (Composes with the #263 `!list` gate.) Unaffected statuses byte-identical.

## Part C (#267) — reject probes seed the tenant membership on a path-scoped tenant route
The reject-probe emitters `push_enum_reject_test` / `push_constraint_reject_test` / `push_inline_reject_test` (`testgen.rs` ~1393/1420/1462) thread the credential but seed no membership. On a PATH-SCOPED route on a tenant / tenant-owned entity, the membership-verified `Tenant` guard extractor runs BEFORE body deserialization → 404 for a non-member → the 422 validator is never reached. Fix: when the reject-probe's route is path-scoped on a tenant/tenant-owned entity (the same predicate the corresponding 2xx probe uses to seed), PREPEND the same tenant-membership seed the sibling 2xx probe emits (reuse the existing seed helper — the tenant-entity's `app()` doesn't pre-seed, but the child-entity modules' 2xx probes already seed; mirror that). So the reject body reaches the validator → 422. A NON-tenant / non-path-scoped reject probe is byte-identical.
- Investigate the exact seed the 2xx probe uses for this shape (a `POST /` create of the tenant entity + membership, or the isolation-test seed) and reuse it, so the reject probe is greenable exactly as the 2xx probe is.

## Tests + Gates
- #265: a 201-list create → handler `Created<Vec<X>>` + OpenAPI array + probe green (unit + scaffold). #266: a 204/3xx create with entity+id → NO id-echo (unit); a 200/201 create → id-echo present (byte-identity). #267: a tenant-entity path-scoped constrained/enum route → the reject probe seeds membership + reaches 422 (unit + scaffold, proven greenable); a child-entity or non-tenant reject probe byte-identical.
- **reference_slice_live_battery + conformance/eval/genroute_compile --include-ignored + lib + testgen green** (canary); determinism + embedded_sync; fmt/clippy/doc -D warnings.

## Version + success criteria
0.7.19. A 201-list create serves + documents an array; a 204/3xx create's id-echo doesn't panic; a tenant-entity path-scoped route's reject probes are greenable (seed membership → 422). Unaffected shapes byte-identical; reference-slice + heavy gate green; published 0.7.19; #265 + #266 + #267 closed.

## Non-goals
- Refusing 204/3xx+entity (gating the probe is enough; a JC lint is optional). Changing the runtime guard order. The accepted residuals.
