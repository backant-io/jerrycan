# JL0006 tenant-own-detail exemption is signature-aware (0.6.29) — #147

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #147 (JL0006 lint hardening. 0.6.2 #124 exempts the tenant's own PathScoped **detail** handlers from JL0006 — they legitimately call unscoped `repo.get/update/remove` on the *tenant* repo because the `Dep<Tenant>` guard already verified membership. The exemption is **name-keyed** (operation_id in `HandlerRef::exempt_fns`) and does NOT inspect the handler's signature, so: (1) an agent that DROPS `_tenant: Dep<Tenant>` from `get_club` and calls `repo.get(id)` ships green — the exemption fires on the name alone; (2) an agent that binds a CHILD repo as the literal `repo` inside the exempt `get_club` and calls `repo.get/update/remove` is silenced. The 0.6.2 exemption is deliberately conservative (`all()` always armed, strict resolver), so this is hardening, not a live leak.)
**Ships as:** 0.6.29 — a `jerrycan check` lint-precision change in `crates/jerrycan/src/platform/lints.rs` (+ `design.rs` HandlerRef). No codegen change; every generated app is byte-identical. The change only affects what `jerrycan check` FLAGS on a HAND-EDITED handler.

## Note: the testgen half of #147 is already shipped
#147 suggested "also a testgen non-member-404 probe for the tenant's own detail route (parity with child isolation)" — that shipped in **0.6.27 (#172)** (`tenant_root_detail_isolation_test`). This spec covers the remaining LINT half only.

## Root cause + fix
`UnscopedVisitor` (lints.rs:314) frames each fn by NAME (`fn_stack: Vec<String>`, pushed at `visit_item_fn`/`visit_impl_item_fn`, lints.rs:337/342) and `armed_in_current_fn` (lints.rs:404) suppresses `get/update/remove/insert` when the innermost frame name is in `exempt_fns`. Make the exemption **signature-aware**: an exempt-named fn is honored ONLY if its signature binds BOTH
- a `Dep<Tenant>`-typed parameter (the membership guard — any binding name, e.g. `_tenant`), AND
- a parameter bound `repo` whose type is `Dep<{Tenant}Repo>` (the tenant's OWN repo — the receiver the unscoped calls use).

An exempt-named fn missing the guard (residual 1) or whose `repo` is a different repo type (residual 2) is NOT exempt → JL0006 fires. `all()` stays armed regardless, unchanged.

### Changes
1. **`design.rs` (`HandlerRef`, ~702):** carry the two signature markers the visitor needs, derived from `tenancy.entity`:
   - the tenant repo type name — `{Tenant}Repo` (e.g. `WorkspaceRepo`), and
   - the guard type marker — the last path segment `Tenant` (the generated guard is `Dep<Tenant>`; match on the `Tenant` segment inside `Dep<…>`, lenient to a `shared::Tenant` path).
   Populate them where `exempt_fns` is built (design.rs:754) — they are constant per tenant module. A non-tenancy design has no `exempt_fns`, so these are only meaningful when the set is non-empty.
2. **`lints.rs`:**
   - `fn_stack: Vec<(String, bool)>` — name + `exempt_qualified` (the signature check result). Push at `visit_item_fn`/`visit_impl_item_fn` after inspecting `node.sig.inputs`.
   - Add a helper `fn signature_qualifies_for_exempt(inputs: &Punctuated<FnArg, Comma>) -> bool`: returns true iff inputs contain a param whose type is `Dep<…Tenant>` (the guard) AND a param whose PAT is the ident `repo` whose type is `Dep<{Tenant}Repo>`. Match `Dep<T>` by the outer type being `Dep` with a single generic arg, and the inner last path segment equal to `Tenant` / `{Tenant}Repo` respectively (lenient on the leading path, exact on the final ident). Thread the expected `{Tenant}Repo` string + the `Tenant` marker into the visitor (from HandlerRef).
   - `armed_in_current_fn`: honor the exemption only when the innermost frame is exempt-named **AND** its `exempt_qualified` flag is true:
     ```rust
     display == "all()"
         || !self.fn_stack.last().is_some_and(|(name, qualified)| {
             *qualified && self.exempt_fns.contains(name)
         })
     ```
   - `scan_macro` uses `armed_in_current_fn` too (lints.rs:441) — it inherits the fix automatically once `fn_stack` carries the flag.

Keep the guiding invariant (lints.rs:746 comment): the lint must UNDER-exempt (a residual false positive has the `// jerrycan:allow JL0006` hatch) and NEVER over-exempt. The signature check tightens (removes) exemptions, so it only ever moves toward flagging — safe direction.

## Tests (lints.rs unit tests — mirror the existing JL0006 #124 tests)
Add unit tests over hand-authored handler source (the lint parses a `&str` of Rust) for a tenancy design whose exempt set contains `get_workspace` (tenant = `Workspace`, repo `WorkspaceRepo`):
1. **Qualified exempt fn is still exempt:** `async fn get_workspace(_tenant: Dep<Tenant>, repo: Dep<WorkspaceRepo>, ...) { repo.get(id)... }` → NO JL0006 hit (unchanged behavior — the legitimate case).
2. **Residual 1 — dropped guard fires:** the SAME fn WITHOUT the `Dep<Tenant>` param, `repo: Dep<WorkspaceRepo>`, calling `repo.get(id)` → JL0006 FIRES (was silently green).
3. **Residual 2 — mis-bound child repo fires:** `get_workspace(_tenant: Dep<Tenant>, repo: Dep<MemberRepo>, ...) { repo.get(id) }` (a NON-tenant repo bound as `repo`) → JL0006 FIRES.
4. **`all()` still armed** inside a fully-qualified exempt fn (unchanged): `get_workspace(_tenant: Dep<Tenant>, repo: Dep<WorkspaceRepo>) { repo.all() }` → FIRES (all() never exempt).
5. **Non-exempt handler unchanged:** a child handler calling `repo.get` without being exempt-named still fires (baseline).
Also confirm the existing #124 JL0006 tests still pass (the qualified case must match their expectation).

## Gates
- `cargo test -p jerrycan` (lints unit tests + existing JL0006/#124 tests) green.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` — the reference-slice/conformance apps whose IMPLEMENTED tenant detail handlers ARE fully qualified must stay JL0006-clean under `jerrycan check` (confirm the signature check does not newly flag a correctly-written handler). NOTE: conformance can flake on the #118 shared-target contention — re-run a suspicious non-related failure in isolation.
- `cargo fmt`/`clippy -D warnings`; byte-identity (`determinism.rs`) — generated code is unchanged (lint-only), every scaffold byte-identical.

## Success criteria
- An exempt-named tenant detail handler that DROPS `Dep<Tenant>` or binds a non-`{Tenant}Repo` as `repo` is FLAGGED by JL0006 (residuals 1 + 2 closed); a correctly-qualified handler stays exempt; `all()` stays armed.
- Byte-identical scaffolding; heavy gate + existing #124 tests green; published 0.6.29; #147 closed.

## Non-goals
- The evasion-only residual 3 (a nested fn deliberately named after an exempt operation_id) — a deliberate-evasion path with the `// jerrycan:allow` hatch already available; not worth the added frame-provenance complexity. Changing `all()`'s always-armed status. The testgen probe (already shipped in #172). Any codegen change.
