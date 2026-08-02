# codegen: flat tenant-owned required_roles checks the membership role + refuse non-canonical tenant mount param (0.7.15) — #247 + #250

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 6 findings, codegen tenant classification/authz)
**Issues:** #247 (flat tenant-owned entity's `required_roles` gates on the SESSION role, not the tenant MEMBERSHIP role → workspace owner locked out — a real functional authz bug, fail-closed) + #250 (a non-canonically-named tenant mount param is silently decorative → entity misclassified flat; safe but a URL-contract lie; the #245 runtime counterpart).
**Ships as:** 0.7.15 — a genroute authz fix (#247) + a design-validation refusal (#250). Patch bump 0.7.14 → 0.7.15. #247 changes generated authz for a flat-tenant-owned `required_roles` route; #250 refuses a mis-specified design.

## Part A (#247) — flat tenant-owned `required_roles` resolves the MEMBERSHIP role
**The bug:** for a FLAT tenant-owned entity (membership-set mount, no `Dep<Tenant>`), a route with `required_roles` generates a steer/check on `user.0.role` — the SESSION/JWT role (`User.role`) — a DIFFERENT dimension from the per-tenant MEMBERSHIP role. A workspace owner (membership role `owner`, session role `user`) is 403'd deleting a lead in their own workspace; the generated test masks it by minting session role `owner`.

**Investigate the mechanism** (read genroute's `require_role` emission + the #107 membership-role machinery: `{tenant}_members`, `MEMBERSHIP_PRINCIPAL_COLUMN`, the membership-mgmt `require_role("admin")` guards which correctly use the membership role). For a flat tenant-owned entity, the required-roles check must resolve the caller's MEMBERSHIP role IN THE ROW'S TENANT: load the row (or its tenant fk), then `SELECT role FROM {tenant}_members WHERE {tenant_fk} = <row's tenant> AND {MEMBERSHIP_PRINCIPAL_COLUMN} = <caller id>` and require that role ∈ `required_roles`. **Pick the correct-and-cleanest of:**
- **(a) Emit a membership-role check** for a flat tenant-owned `required_roles` route (a generated primitive / handler-steer that does the per-row membership-role lookup), so the owner can act. This is the RIGHT fix (it makes the feature work).
- **(b) If (a) is too large for this release,** REFUSE `required_roles` on a flat tenant-owned entity with a clear JC code ("required_roles on a flat tenant-owned entity would check the session role, not the membership role — make the mount path-scoped (`Dep<Tenant>`) or model the check in the handler"), so the mis-generation is loud instead of a silently-wrong 403. This is the make-impossible floor.
Prefer (a) — the whole point is the owner SHOULD be able to act. State which you did and why. Either way: no route where the generated authz silently checks the wrong role dimension, and the generated test must reflect reality (if (a), the test seeds the caller as a member with the required MEMBERSHIP role — not just a session role; if (b), the design is refused).

**Security invariant:** the change must stay FAIL-CLOSED — a caller without the required membership role in the row's tenant is denied; only the caller WITH it is admitted. No cross-tenant escalation (the membership lookup keys on the ROW's tenant, verified against the caller).

## Part B (#250) — refuse a non-canonical tenant mount param
**The bug:** a tenant entity mounted on a param whose name differs from the canonical tenancy fk (e.g. `events` at `/spaces/{ws_id}`, fk `workspace_id`) is silently classified flat/membership-set — the `shared::tenant` resolver only recognizes a param literally named `{canonical_fk}`, so `{ws_id}` is decorative (safe — membership-set enforced — but the URL contract is a lie; `/spaces/1/` and `/spaces/999/` behave identically).
**Fix:** REFUSE at design validation (a new JC code) a tenant-owned entity whose mount carries a path param that is NOT the canonical tenancy fk (nor a recognized join child_fk) — message: "a tenant mount param must be named `{canonical_fk}` to scope by tenant; got `{ws_id}` — rename it or drop it". This makes the mis-specification loud and subsumes the #245 shape at the design layer (a design that would have hit the #245 test-URL bug is now refused before scaffold). Confirm the canonical/nested/grandchild mounts (params named after the tenancy fk / join child_fks) are NOT refused (byte-identical). Coordinate with #245: the #245 `cross_prefix` test fixture used exactly this now-refused shape — update/replace that fixture (it can no longer scaffold; assert the refusal instead), and note #245's test-layer fix becomes belt-and-suspenders.

## Tests
- **#247:** a flat tenant-owned `required_roles` design → (a) the generated route resolves the membership role (a genroute unit + a scaffold that a member with the role is admitted and a non-member/wrong-role is denied — ideally live vs PG: the workspace owner CAN delete, a non-owner member CANNOT); or (b) the design is refused with the new JC code (unit). The generated acceptance test is honest (member-with-role, not session-role).
- **#250:** a non-canonical tenant mount param design → refused with the new JC code (unit); canonical/nested/grandchild mounts NOT refused (byte-identical); the #245 fixture updated to assert refusal.
- Existing conformance/reference_eval/eval byte-identical for unaffected designs.

## Gates
- `cargo test -p jerrycan` + heavy eval gate (`reference_eval`/`conformance`/`eval`/`genroute_compile --include-ignored`) green; if #247 chose (a), a live-PG scaffold proving the owner can act.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; `cargo semver-checks`; determinism + embedded_sync.

## Version + success criteria
0.7.15. A flat tenant-owned `required_roles` route checks the caller's MEMBERSHIP role in the row's tenant (owner can act; wrong-role denied), OR the design is refused loudly (#247, fail-closed either way); a non-canonically-named tenant mount param is refused at validation, not silently decorative (#250). Canonical designs byte-identical; heavy gate green; published 0.7.15; #247 + #250 closed.

## Non-goals
- Changing the membership-role model (#107). The testgen probe-seeding (#248/#249 → 0.7.14). For #250, honoring an arbitrary mount-param name (refusal is the chosen make-impossible).
