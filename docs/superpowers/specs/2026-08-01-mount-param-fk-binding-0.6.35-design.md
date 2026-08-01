# Handler auto-binds mount-inherited path params (0.6.35) — #127

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation
**Issues:** #127 (two follow-ups from the 0.5.2 mount-awareness work. `handler_params`/`path_params` (genroute.rs:60/193) read `ep.path` ONLY, so a nested-mount handler binds only its OWN params (`{id}`), not the mount-INHERITED ones (`{club_id}` from the mount prefix). (1) **Tenant grandchild:** a grandchild under `/accounts/{account_id}` has its immediate PARENT fk (`account_id`) dropped from the body (correct, #82) but NOT bound as a `Path` param — it is neither the tenant (that comes from `Dep<Tenant>`) nor a bound path param, so the agent must hand-add `Path(account_id)` to inject the dropped fk; the generated stub compiles but the steering points at a param the framework never generated. (2) **Non-tenant param-mount child (LATENT):** no `Dep<Tenant>`, no `Path` binding → the dropped fk is un-injectable → uncompilable. No such design exists in-repo (grep-verified) and the harness runs SQLite FK-off, so nothing is broken today — but it would break if such a shape ships. Plus a MUDDLED steering pair: `tenant_scope_comment` (~365) says the flat create reads the tenant fk from the BODY while `server_owned_fk_comment` (~553) says the fk is dropped and injected from the path.)
**Ships as:** 0.6.35 — a genroute change to `handler_params`/`path_params` (bind mount-inherited path params) + the two steering comments. The generated handler for a PARAM-MOUNT CHILD changes (it gains the mount-fk `Path` binding); byte-identical for every FLAT/top-level module (no mount param) and every non-param-mount design.

## The resolved path is the source of truth
An endpoint's RESOLVED path = `module.effective_mount()` + `ep.path` (see `design.rs:1280` `let resolved = format!("{mount}{}", ep.path)`). `path_params(ep)` today scans `ep.path` only; make it scan the RESOLVED path so mount-inherited tokens (`{club_id}`, `{account_id}`) are included. `handler_params`/`path_params` already receive `m: &ModuleDesign` (or thread it), so `m.effective_mount()` is available.

## Fix
1. **`path_params`** (genroute.rs:60): scan the RESOLVED path (`effective_mount + ep.path`) instead of `ep.path`, so it returns mount-inherited params + own params, in path order. (A flat module's `effective_mount` carries no `{token}`, so its result is byte-identical.)
2. **`handler_params`** (genroute.rs:193): bind each resolved-path param as a `Path` param, EXCEPT the tenant fk when the endpoint takes `Dep<Tenant>` (which already resolves the tenant — do NOT double-bind it). So:
   - tenant fk token + `endpoint_uses_tenant_guard` ⇒ resolved by `Dep<Tenant>`, NOT a `Path` param (byte-identical to today for a direct tenant child).
   - every OTHER resolved-path token (a parent fk on a grandchild, or any non-tenant mount param) ⇒ a `Path` binding, so the handler can inject the dropped fk. Preserve the existing single-`{id}` binding (the endpoint's own leaf) — the change is ADDITIVE mount tokens for nested mounts.
   - Order: repo, guard, then Path params in resolved-path order, then body (match the existing param order convention).
   Confirm the `Path<...>` type for a multi-token nested mount: multiple path params bind as a tuple `Path((a, b))` or separate `Path` extractors per the framework's convention (grep how a two-token route binds today — genroute.rs:6945 asserts `!h.contains("Path((_")` for the current single-leaf case; the nested-mount case needs the correct multi-token form). Use the framework's supported multi-path-param extractor.
3. **Steering reconciliation:** make `tenant_scope_comment` (~365) and `server_owned_fk_comment` (~553) COHERENT for the MembershipSet param-mount grandchild: the tenant comes from `Dep<Tenant>`; the PARENT fk is dropped from the body and injected from the now-generated `Path` param. Remove the contradiction (one says body, the other says path) — the path is correct.

## Byte-identity + ripple
- FLAT/top-level modules and direct tenant children: byte-identical (their `effective_mount` has no extra token, or the only token is the tenant fk already handled by `Dep<Tenant>`).
- TENANT GRANDCHILD designs (a nested `/accounts/{account_id}/…` mount): the generated handler GAINS the `Path(account_id)` binding — that IS the fix. If the reference-slice or a conformance fixture has a tenant grandchild, its generated handlers change → update that battery's expectations (the handler stub + any golden). Grep the fixtures for a nested param mount (`{…_id}` in a mount prefix) and handle the ripple.
- Add a design/fixture exercising a NON-TENANT param-mount child (the latent uncompilable case) so the fix is proven: its handler now binds the mount fk as `Path` and COMPILES (was un-injectable). Run it through genroute_compile (which builds a real crate under strict clippy) to prove compilation.

## Tests
- genroute unit: a nested-mount handler's params include the mount-inherited token(s) as `Path` bindings (tenant grandchild: parent fk bound, tenant fk NOT double-bound; non-tenant param-mount: mount fk bound). A flat/top-level handler is unchanged (byte-identity witness).
- **genroute_compile:** add the non-tenant param-mount design so a real crate compiles under strict clippy (proves the latent case is fixed).
- Steering: the two comments are coherent (no body-vs-path contradiction) for the MembershipSet param-mount grandchild.

## Gates
- `cargo test -p jerrycan` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (a tenant-grandchild fixture's handlers change → update expectations; the new non-tenant param-mount compiles). Local PG available.
- `cargo fmt`/`clippy -D warnings`; determinism (regenerated handlers deterministic).

## Success criteria
- A nested-mount handler auto-binds every mount-inherited path param as a `Path` (except the tenant fk `Dep<Tenant>` resolves), so a tenant grandchild's parent fk and a non-tenant param-mount child's mount fk are injectable WITHOUT hand-editing; the non-tenant param-mount design COMPILES (genroute_compile proof).
- The steering comments are coherent (parent fk from the path, tenant from `Dep<Tenant>`).
- Flat/top-level/direct-child designs byte-identical; heavy gate green; published 0.6.35; #127 closed.

## Non-goals
- Changing the body-trim (#82, already correct). The tenant guard SHAPE (`endpoint_uses_tenant_guard`, unchanged). Subroute-nested tenant roots beyond the param-mount case. Removing SQLite FK-off in the harness (separate).
