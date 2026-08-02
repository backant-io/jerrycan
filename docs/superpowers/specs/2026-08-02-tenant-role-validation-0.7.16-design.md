# codegen: validate flat-tenant required_roles vs member_roles + refuse aliased tenant-anchor (0.7.16) — #256 + #257

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 7, tenant validation gaps at the #247/#119 seam)
**Issues:** #256 (flat tenant-owned `required_roles` validated against `auth.roles` not `member_roles` → a vacuous always-403 gate scaffolds green, AND a valid membership-role is falsely refused) + #257 (aliasing the tenant-ANCHOR's `belongs_to` breaks all tenant scoping; JC0560 doesn't refuse — grandchild compiles green then runtime `no such column`). Both fail-closed green-means-safe validation gaps.
**Ships as:** 0.7.16 — two design-validation fixes in `questions.rs`/`design.rs`. Patch bump 0.7.15 → 0.7.16. Byte-identical for designs not hitting these shapes.
**MANDATORY:** run `reference_slice_live_battery` (--include-ignored, live PG) locally BEFORE shipping — the reference-slice has flat tenant-owned `required_roles` entities (Lead + ApiKey), so this touches its canary path.

## Part A (#256) — flat-tenant required_roles validates against member_roles
`questions.rs:929-933` sets `declared_roles = auth.roles`; `:2804-2815` validates every endpoint's `required_roles` against it. But #247 makes a FLAT tenant-owned (`MembershipSet` + tenant-owned) route's `required_roles` a MEMBERSHIP-role check (`require_membership_role` reads `{tenant}_members.role`, domain = `tenancy.member_roles`). **Fix:** for an endpoint whose entity is flat tenant-owned with a role route (the same predicate #247's `entity_has_required_roles_route` / the steer uses), validate its `required_roles` against `tenancy.member_roles`; for session-role / non-tenant routes keep validating against `auth.roles`. Refuse a flat-tenant `required_roles` value not in `member_roles` (a clear message: "`required_roles` on a tenant-owned route checks the MEMBERSHIP role — `X` is not in `tenancy.member_roles`"); accept a `member_roles` value even if it's not in `auth.roles` (fixes the false refusal). Confirm the reference-slice (which sets `auth.roles == member_roles`, so `["owner"]` is valid under both) stays green.

## Part B (#257) — refuse aliasing the tenancy-anchor's belongs_to
`Design::tenant_path` (`design.rs:1210`) hardcodes `tenant_fk = Self::fk_column(&tenancy.entity)` (canonical), ignoring an aliased anchor→tenant `belongs_to`. **Fix (make-impossible, primary):** extend JC0560 (`questions.rs:2023-2034`) to REFUSE a design where the tenancy-anchor entity's `belongs_to` the tenancy entity is aliased (its `fk_column()` != the canonical `fk_column(tenancy.entity)`) — message: "the belongs_to that anchors an entity to the tenancy entity `{tent}` must not be aliased (its fk must be the canonical `{canonical_fk}`); rename or drop the `as`". This catches BOTH the grandchild (silent) and direct-child (compile) cases loudly at design time. Confirm an aliased INTERMEDIATE link (non-anchor) is NOT refused (that already generates correct SQL — the aliased-intermediate case must stay valid). Confirm canonical (unaliased anchor) designs byte-identical.
- (If deriving the fk from the anchor's actual `belongs_to(tenant).fk_column()` and threading it through `tenant_path` + all members-JOINs is cleaner and you're confident it's complete, that's acceptable instead — but the refusal is the smaller, safer make-impossible; prefer it unless deriving is clearly better. State which.)

## Tests
- #256: flat-tenant `required_roles` in `member_roles` accepted; not in `member_roles` refused (unit); a `member_roles`-only role (not in `auth.roles`) accepted (the false-refusal fix); a session-role/non-tenant `required_roles` still validated vs `auth.roles`. reference-slice green.
- #257: aliased tenancy-anchor refused (unit + JC code); aliased intermediate link NOT refused; canonical byte-identical.
- **reference_slice_live_battery + conformance/eval/genroute_compile --include-ignored green** (the reference-slice canary).

## Gates
- `cargo test -p jerrycan` + heavy eval gate (incl reference_slice_live_battery, local PG) green; `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; determinism + embedded_sync.

## Version + success criteria
0.7.16. A flat tenant-owned `required_roles` is validated against `member_roles` (no vacuous always-403 gate, no false refusal of a membership role); an aliased tenancy-anchor `belongs_to` is refused loudly (no silent scoping break). Canonical designs byte-identical; reference-slice + heavy gate green; published 0.7.16; #256 + #257 closed.

## Non-goals
- Changing #247's runtime primitive (correct). Supporting an aliased tenancy-anchor fk (refuse it). The accepted JC0568 intermediate-subroute residual.
