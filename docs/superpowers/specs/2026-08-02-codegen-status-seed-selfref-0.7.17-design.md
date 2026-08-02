# codegen: 202 status + grandchild create-seed + JC0560 self-ref (0.7.17) — #259 + #260 + #261

**Date:** 2026-08-02
**Status:** Approved design (AUDIT round 8)
**Issues:** #259 (declared 202/non-standard-2xx served as 200; probe asserts declared status → un-greenable + contract violation; latent on reference-slice import_leads:202), #260 (transitive-grandchild flat-create probe seeds no intermediate parent → 403≠201, #248 sibling), #261 (JC0560 #257 false-refuses a self-ref tenant entity's aliased belongs_to — LOW, my 0.7.16 regression).
**Ships as:** 0.7.17 — genroute + testgen + questions fixes. Patch bump 0.7.16 → 0.7.17. Unaffected shapes byte-identical.
**MANDATORY:** run `reference_slice_live_battery` (--include-ignored, live PG) — import_leads declares 202 (the #259 canary).

## Part A (#259) — a declared non-200/201/204 2xx status is served with that status
`genroute::return_type` (`genroute.rs:45-58`) special-cases 204/3xx/201 → everything else funnels to `Json<…>`/`Created<…>` (fixed 200). Fix: for a route whose `success.status` is a 2xx OTHER than 200/201/204 (e.g. 202), emit a return type that sets THAT status — jerrycan-core supports the tuple `(StatusCode, Json<…>)` (or an equivalent typed responder). The generated handler stub returns the declared status; testgen's `_returns_{status}` probe (which already asserts the declared status) now matches. 200/201/204/3xx unchanged (byte-identical). Confirm the reference-slice `import_leads` (202) now serves 202 and its probe passes — and if its hand-authored reference handler hardcodes 202 to paper over the bug, reconcile so the reference handler follows the new generated signature (update `conformance/eval/fixtures/reference/*_handlers.rs` if the signature changes; run reference_slice_live_battery).

## Part B (#260) — transitive-grandchild flat-create probe seeds the intermediate parent
For a transitive-grandchild flat tenant-owned entity (`Planting belongs_to Bed`, `Bed belongs_to Farm`[tenancy]), the create success probe seeds the tenant + membership but not the intermediate `Bed`, so `create_for_memberships`' parent-existence check (`SELECT 1 FROM beds WHERE beds.id=? AND farm_id IN(memberships)`) can't resolve → 403≠201. Fix: extend the create-probe parent seeding (the #248 mechanism, `create_probe_parent_seed`/`seed_parents` in testgen) to seed the INTERMEDIATE belongs_to parent row(s) up the chain to the tenancy entity, reachable under the caller's membership — so the parent-existence check passes and the probe reaches 201. Direct-child flat creates (whose fk is the seeded tenant) stay byte-identical.

## Part C (#261) — JC0560 excludes the tenant entity's own self-ref
`questions.rs:2049` check (5) refuses an aliased anchor→tenant belongs_to but omits `&& e.name != *tent`, so the tenant entity's OWN self-referential aliased belongs_to (`Org belongs_to Org as "parent"`) is wrongly refused (`tenant_path` is `None` for the tenant entity itself → never a tenancy anchor). Fix: add `&& e.name != *tent` to the check; give the self-ref a correct (non-anchor) message if it hits another check, or simply allow it. Unit test: a self-ref tenant entity's aliased belongs_to is NOT refused; the aliased anchor on a NON-tenant entity IS still refused.

## Tests + Gates
- #259: a 202 (and a 200/201/204) route generate the correct return type + status; the `_returns_202` probe passes; reference-slice import_leads:202 green. #260: a grandchild flat-create probe seeds the intermediate parent + reaches 201 (unit + ideally live). #261: self-ref tenant entity aliased bt accepted; anchor alias still refused.
- **reference_slice_live_battery + conformance/eval/genroute_compile --include-ignored + lib + testgen green**; determinism + embedded_sync; fmt/clippy/doc -D warnings.

## Version + success criteria
0.7.17. A declared 202 serves 202 (contract honored, probe greenable); a transitive-grandchild flat-create probe is greenable; a self-ref tenant entity's aliased belongs_to is not falsely refused. Unaffected shapes byte-identical; reference-slice + heavy gate green; published 0.7.17; #259 + #260 + #261 closed.

## Non-goals
- Non-2xx declared statuses (a declared error status is a different path). Changing the tenancy/membership model. The accepted residuals.
