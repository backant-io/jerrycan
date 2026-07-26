# Refuse anonymous reads on the tenant / tenant-owned entity (0.6.11) — #148

**Date:** 2026-07-26
**Status:** Approved design, pre-implementation
**Issues:** #148 (SECURITY — "green means safe" violated: in an auth design, a GET on the tenant entity or a tenant-owned entity that omits `auth_required` (serde default `false`) and is not `public` is generated with **no `Dep<Tenant>` guard and no `CurrentUser`** — a fully anonymous handler. `genroute.rs:196` emits the guard only under `mode.auth && ep.is_guarded()`. Nothing refuses the unguarded-but-not-`public` case: `check_public_on_tenant_owned` keys on `ep.public`; JC0549(c) is per-user-only; JC0550 is about the detail *param*; JL0004 covers mutations only; and in a **childless** tenant module `handlers.rs` is never scanned by JL0006. So an anonymous internet user can read any tenant's row by id with a green `jerrycan check`.)
**Ships as:** 0.6.11 — a validation-only security fix (correct-by-construction refusal). Additive: no generated-code change for any design that doesn't already contain the anonymous shape (those designs never compiled a *safe* app — they compiled an *unsafe* one; 0.6.11 refuses them at `check`). Byte-identical for every safe design.

## Root cause (precise)
`Endpoint::is_guarded()` (design.rs:517) = `auth_required || !required_roles.is_empty()`. An endpoint that is neither guarded nor `public` gets **no session param at all** (genroute.rs:196-201) → anonymous. The per-user twin of this hole is already refused: `check_public_read` / JC0549(c) (questions.rs:1343) refuses an unguarded read on an owner-scoped (per-user) entity unless `public_read: true` or `auth_required: true`. **#148 is the missing tenant twin of that exact check.**

## Design — JC0558, the tenant twin of JC0549(c)

### A. The refusal (questions.rs)
Add a validation check that mirrors `check_public_on_tenant_owned` (questions.rs:1034) **exactly**, flipping the predicate from `ep.public` to "anonymous". The existing `check_public_on_tenant_owned` walks every module/subroute endpoint and flags one whose repo entity satisfies `d.tenant_path(name).is_some()` — and `tenant_path` returns `Some` for **both** the tenant root entity (design.rs:970, `entity == tenancy.entity` → empty chain) **and** every directly/transitively tenant-owned entity. That predicate is precisely #148's domain ("the tenant entity or a tenant-owned entity"). Reuse it.

New check (place beside `check_public_on_tenant_owned`, gated on an active auth model + declared tenancy — the same guard the surrounding block already computes):
```rust
// #148 (JC0558): in an auth design, an endpoint on the tenant entity or a
// tenant-owned entity that is neither guarded nor `public` is ANONYMOUS — it
// emits no Dep<Tenant> and no CurrentUser, so any caller reads/writes any
// tenant's rows. Refuse it (correct-by-construction, the JC0549(c) tenant twin).
if !ep.public
    && !ep.is_guarded()
    && !ep.declares_signature_auth()   // Stripe-webhook shape is auth'd by signature (mirror JL0004)
    && endpoint_repo_entity(m, ep).is_some_and(|name| d.tenant_path(name).is_some())
{
    qs.push(q(
        format!("{ptr}/endpoints/{i}"),
        format!(
            "Endpoint `{}` ({:?} {}) is on the tenant-scoped entity `{}` but is neither authenticated nor `public` — it emits no `Dep<Tenant>` guard and no `CurrentUser`, so an anonymous caller could read or write any tenant's rows. Set `auth_required: true` so the membership guard scopes it. See `jerrycan explain JC0558`.",
            ep.name, ep.method, ep.path, /* repo entity name */,
        ),
    ));
}
```
Walk modules + subroutes recursively (mirror `check_public_on_tenant_owned`'s recursion at :1053-1059).

**Exemptions (must NOT fire — verify each in a test):**
- `ep.public` — a genuinely open route (login/register). (For a *tenant-owned* entity, `public` is separately refused by `check_public_on_tenant_owned`; for the tenant *root*, `tenant_path` is also `Some`, so `public` there is already refused too. JC0558 only concerns the anonymous-non-public case, so the `!ep.public` guard just avoids a duplicate on the same endpoint.)
- `ep.is_guarded()` — `auth_required` or `required_roles` present → guard emitted.
- `ep.declares_signature_auth()` — Stripe-style webhook, intentionally unguarded, proves itself by signature (JL0004 exempts it; JC0558 must too).
- `endpoint_repo_entity` is `None` (entity-less subroute — the join/leave membership escape hatch) OR resolves to a non-tenant-owned entity (per-user entities are JC0549's domain). `tenant_path(name).is_none()` → no fire.

### B. JC0558 registry + explain + completeness test (codes.rs)
Register **JC0558** (next free after JC0557) in `codes.rs` with `cause`/`fix`, following the JC0557 precedent (codes.rs, most recent entry). Add the completeness/`lookup("JC0558")` test mirroring the JC0557 test (codes.rs tests ~:641). The `explain` output must name: the anonymous-read cause (no guard, no session param), the tenant/tenant-owned domain, and the fix (`auth_required: true`; note that tenant-owned entities have no public-read in v1 — that's #105's per-user-only gap).

### C. testgen — tenant own-detail non-member 404 probe (parity with child isolation)
The issue asks for a testgen probe so the acceptance suite catches a guard that is *present but not enforcing* (a different failure mode than JC0558's make-impossible). Today child-isolation emits a non-member-404 probe for tenant-*owned* detail routes; the tenant entity's **own** detail route (`GET /clubs/{id}`) has no such parity probe. Emit `{ident}_detail_by_non_member_is_404` for the guarded tenant-root detail route: a `CurrentUser` who is NOT a member of the path tenant gets 404 (not 200) on the tenant's own detail. Model on the existing child non-member-404 probe (locate via `grep -n "non_member" crates/jerrycan/src/platform/testgen.rs`). Seed a second principal (non-member) reusing the existing acceptance-seed pattern.

**Scope note:** §A+§B (the refusal) is the security fix and is complete on its own — it closes the hole by construction. §C is defense-in-depth parity. If §C's byte-identity/seed cost balloons, ship §A+§B as 0.6.11 and file §C as a scoped follow-up; do NOT let §C block the security refusal.

### D. Docs
Document in the auth doc (`docs/ai/` — the tenancy/auth reference; find via `grep -rln "auth_required" docs/ai/`) + the embedded twin (embedded_sync byte-identity gate — edit BOTH twins identically): in an auth design, every endpoint on the tenant or a tenant-owned entity must be authenticated (`auth_required: true`) — an unguarded, non-`public` tenant read is refused (JC0558). Note there is no public-read for tenant-owned entities in v1.

## Byte-identity / no-drift
- §A+§B change validation only — a design that never contained the anonymous shape scaffolds byte-identically. Prove with the base-vs-HEAD scaffold `diff -r` on an existing conformance app (unchanged) + the determinism test.
- §C changes generated tests **only** for the tenant-root detail route — update the affected golden/no-drift expectations; every non-tenant-detail test stays byte-identical.
- `cargo semver-checks` clean (no public-API change; `Endpoint`/`Design` methods unchanged).

## Success criteria
- A design with a GET on the tenant (or a tenant-owned) entity that omits `auth_required` and is not `public` → `jerrycan check` **refuses** with JC0558 (was: green). Setting `auth_required: true` (or `public: true` where the shape allows) clears it.
- A signature-authed webhook, an entity-less join/leave subroute, a `public` login route, and a guarded tenant read do **not** trip JC0558 (regression tests for each).
- §C: a guarded tenant-root detail route's acceptance suite includes a non-member-404 probe.
- A safe design is byte-identical; `jerrycan explain JC0558` is complete; heavy gate green; `cargo semver-checks` clean; published 0.6.11.

## Non-goals
- Public-read for tenant-owned entities (#105 is per-user-only — a separate contract surface). Forcing the guard on instead of refusing (option (b) — rejected: refusal is correct-by-construction and matches the JC0549/JC0550 lineage). #147 (signature-aware JL0006 exemption) — coordinated but separate.
