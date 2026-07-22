# Tenant-guard residuals: precise normalization + honest refusal + lint attribution (0.6.2) — #88 / #89 / #124

**Date:** 2026-07-21
**Status:** Approved design, pre-implementation
**Issues:** #88 (non-`{id}` tenant detail param escapes normalization → silent no-membership-check), #89 (normalization over-applies — a sibling entity's `/{id}` in the tenant module is mis-renamed to the tenant fk), #124 (JL0006 false-positive on the tenant's own detail route when the module also owns a tenant-owned child). All three are residuals of the #78 tenant-guard effort.
**Origin:** T2/T6 re-reviews of the ownership-safety work (#78) + round-5 eval.
**Ships as:** 0.6.2 — a codegen-correctness / security patch. The #78 **core** leak (membership query lacked the tenant filter) was closed in 0.5.0; the guard now verifies `WHERE user_id = ? AND {fk} = ?` for path-scoped routes (scaffold.rs:109). This closes the three residual gaps around it. Purely additive-correctness: **every in-repo design generates byte-identical output** (the investigation swept all 9 JSON + 30 inline designs — zero carry any of the three trigger shapes), so the changes are new validation + new lint precision + new fixtures only. One new `platform::Entity`-independent design-time error code — no `constructible_struct_adds_field` interaction.

## Background: how the tenant guard resolves the addressed tenant
A tenant-owned route's guard (`Dep<Tenant>`) verifies membership in the tenant **named by the path fk**: for a path-scoped route it runs `SELECT role FROM {tenant}_members WHERE user_id = ? AND {fk} = ?` and 404s a non-member (scaffold.rs:102-121). To make that path branch fire on the tenant entity's **own** detail route, `Design::normalize_tenant_detail_routes` (design.rs:837) rewrites the tenant module's own `/{id}` → `/{fk}` at design load. A route the guard cannot bind to an fk falls back to the caller's **first** membership (scaffold.rs:122-139) — correct only for the tenant's own collection and storage buckets, a leak-shaped default anywhere else.

## The three residuals

### A. #89 — normalization must target the tenant entity, not the whole module
`normalize_own_detail_routes` (design.rs:852-863) renames **every** endpoint in the tenant-declaring module whose path contains `{id}`. A tenant module that also hosts a second entity with its own `/{id}` detail route gets that sibling's `{id}` wrongly renamed to `{fk}`, mis-scoping the sibling's detail route to the tenant guard.

**Fix:** rename only endpoints whose resolved repo entity **is** the tenant entity. Resolve with the existing lenient `Design::endpoint_repo_entity(m, ep)` (design.rs:1430) — it distinguishes a sibling (`success.entity`/`request_body` = the sibling) from the tenant, and its first-entity fallback preserves today's normalization of a bodyless tenant detail route (e.g. a `DELETE /{id}` with no `success.entity`) in a single-entity tenant module. Because `endpoint_repo_entity` reads sibling endpoints' paths (collection-creator resolution), resolve **all** target entities in an immutable pre-pass (collect the endpoint indices to rewrite) **before** mutating any path — never resolve mid-rename against half-rewritten collection paths. Entity-less custom endpoints in a *multi-entity* tenant module remain a documented residual of `endpoint_repo_entity`'s first-entity fallback (tracked under #143); no in-repo design exercises it.

### B. #88 — refuse a non-pk tenant detail param instead of silently skipping the check
`normalize_own_detail_routes` matches only the literal `{id}` token, so a tenant entity's own detail route using a different param name (`/{slug}`, `/by-slug/{slug}`, a multi-param custom) is not normalized. The guard then cannot bind an fk → `endpoint_tenant_shape` returns `None` → the handler is generated with a bare `CurrentUser` and **no `Dep<Tenant>` at all** (genroute.rs:196) — the tenant's own detail route runs with **no membership check**, silently. (In a `db`+`auth` design JC0542 already rejects the common first-position `/{slug}` collision with the implicit member routes; the reachable holes are non-first-position params, `db`-less/memory-mode tenancy, and multi-param customs.)

Renaming `/{slug}` → `/{fk}` is **not** a safe fix: the guard parses the path value as the tenant **pk** type (scaffold.rs:103-110), so silently renaming a slug reinterprets a non-pk lookup as a pk lookup — a semantic change, not a normalization. Membership-by-slug would require the guard to resolve slug→pk first (a larger feature, out of scope; see Non-goals).

**Fix — a new design-time error `JC0550`** (next free; codes.rs registry): after normalization, if the **tenant entity's own** detail route (an endpoint whose `endpoint_repo_entity == tenancy.entity` that carries at least one path param) has a trailing/sole path param that is **not** the tenant fk, refuse it:

> `JC0550`: Endpoint `{op}` (`{METHOD} {path}`) is the tenant `{Tenant}`'s own detail route but addresses it by `{param}`, not its pk `{fk}` — the membership guard verifies the tenant named by the path fk, so a non-pk param (e.g. a slug) cannot be membership-checked. Use `/{id}` (auto-normalized to `/{fk}`) or `/{fk}` directly. (Slug-based tenant addressing is not yet supported.)

`{id}` still auto-normalizes to `{fk}` (so the conventional shape is unaffected), and a route already spelled `/{fk}` passes. Only a genuinely non-pk param trips `JC0550`. This turns a silent no-check into a loud, correct-by-construction refusal — the JC0549 pattern (a latent unimplementable shape becomes a clear fork).

### C. #124 — JL0006 must attribute a hit to its handler and exempt the tenant's own path-verified detail route
`collect_owned_handlers` (design.rs:589-614) scans a module's **entire** `handlers.rs` when **any** of its entities is tenant-owned (`tenant_path(..).is_some()`). A tenant module that hosts one tenant-owned child therefore drags the tenant's OWN detail handlers into the scan — and `UnscopedVisitor` (lints.rs:309-413) flags every `repo.get/update/remove(` with no attribution to the enclosing function. But the tenant's own `PathScoped` detail handler legitimately calls unscoped `repo.get/update/remove` on the **tenant** repo: membership in the path tenant was already verified by the guard, and the tenant repo intentionally keeps its unscoped methods (per-user suppression only, genroute.rs:2006). Result: a false JL0006 on correct code.

**Fix:** attribute each JL0006 hit to its enclosing `fn` (add an `ItemFn` frame to the visitor; handler fn names are `operation_id`s by the JL0002 contract, lints.rs:502). Compute, in `collect_owned_handlers` (where `&self` + `&ModuleDesign` are in scope), the set of exempt handler names = endpoints where `endpoint_repo_entity(m, ep) == Some(tenancy.entity)` **and** `endpoint_tenant_shape(m, ep) == PathScoped`; carry it on `HandlerRef` (design.rs:550). Suppress `get`/`update`/`remove`/`insert` hits **only** inside an exempt fn.

**Errata (as implemented):** the shipped exemption deliberately uses the **strict** resolver (`endpoint_repo_entity_strict`), not the lenient `endpoint_repo_entity` named above — for a security lint the safe direction is to UNDER-exempt (a residual false positive has the line-scoped `// jerrycan:allow JL0006` hatch), whereas the lenient first-entity fallback would over-exempt an entity-less custom endpoint and silence a real leak. **The exempt set also carries a third conjunct `ep.is_guarded()`** (added after the whole-branch review): the exemption's premise — "membership was already verified by the `Dep<Tenant>` guard" — holds only for guarded endpoints, since genroute emits the guard only under `mode.auth && ep.is_guarded()`. Without it, an unguarded tenant detail route (no `auth_required`) would be exempted while running with no membership check at all — an anonymous read flipped red→green. (The broader "an auth design should not allow an unguarded, non-public tenant/tenant-owned GET at all" gap is tracked #148.)

**Stays armed (must not be suppressed):**
- `repo.all()` **everywhere**, including inside an exempt detail fn — fn-level suppression cannot see which repo the `repo` binding holds, so keeping `all()` armed cheaply bounds the "agent bound a child repo as `repo` in the tenant's detail handler" residual. (A correct tenant detail handler calls `get`, not `all`.)
- All hits in the tenant's **Collection** handlers (`repo.all()` in `list_clubs` must still steer to `all_for_member` — a real leak otherwise).
- All hits in **child-entity** handlers (the actual tenant-owned children — the whole point of JL0006).

## Byte-identity & ordering
- **No golden changes.** Every existing design's generated output is unchanged: no in-repo tenant module carries a non-`{id}` detail param (A/B), hosts a sibling `/{id}` (A), or runs the lint over implemented handlers of a child-hosting tenant module (C). Prove it: base-vs-HEAD scaffold `diff -r` of the reference-slice (a tenancy design) stays identical, and the full suite stays green.
- The one test that legitimately changes is the JC0542 `club_tenancy("/{slug}")` case (questions.rs:3147) — a `db`+`auth` `/{slug}` now also trips `JC0550`; update its expectation (both codes are legitimate; assert `JC0550` is present).
- A precedes B (B's validation reads the normalized paths). C is independent.

## Tests
- **A (#89):** a tenant module with a second entity that has its own `GET/PUT/DELETE /{id}` — after `normalize_tenant_detail_routes`, the tenant entity's `/{id}` → `/{fk}` but the sibling's `/{id}` is **unchanged**. Extend `normalize_renames_only_the_tenant_module_own_detail_route` (design.rs:2212).
- **B (#88):** a tenant entity's own detail route `/{slug}` (repo entity = tenant) → `validate()` raises `JC0550` with the entity/param/fk in the message; a conventional `/{id}` (→ `/{fk}`) and an explicit `/{fk}` both pass; a **child** entity's `/{slug}` detail route does **not** trip `JC0550` (only the tenant's own). Registry completeness test covers `JC0550`.
- **C (#124):** a tenant module hosting a tenant-owned child, with implemented handlers where the tenant's own `get_club` calls `repo.get(id)` (unscoped, legitimate) and the child's `list_books` calls `repo.all()` (unscoped, illegitimate) → JL0006 is **silent on `get_club`** and **fires on the child's `repo.all()`**; a `repo.all()` inside `get_club` still fires. New lint test beside the JL0006 suite (lints.rs:568+). An end-to-end `jerrycan check`-goes-green proof (mirror `public_read_feed_goes_green`, conformance.rs:717) for a child-hosting tenant app with correct handlers.

## Non-goals / documented
- **Slug-based tenant addressing** (membership-checked `/clubs/{slug}`) is not supported — `JC0550` refuses it with guidance. A future feature would resolve slug→pk in the guard before the membership query; file as a tracked enhancement if demand appears.
- **Flat tenant write make-impossible** (#96/#97/#116 — suppress the bare tenant write methods, wire creates through `create_for_memberships`) is a larger parity-with-#79 change, its own release.
- The `endpoint_tenant_shape` `ep.path.contains("{id}")` shortcut (design.rs:1013) and the questions.rs:146 resolver's missing collection-creator arm are pre-existing (tracked: #143) — not widened here.

## Success criteria
- A tenant module with a sibling `/{id}` entity: the sibling's detail route is untouched by normalization; only the tenant's own `/{id}` → `/{fk}`.
- A tenant's own detail route with a non-pk param → `JC0550` (was: silent no-membership-check); `/{id}` and `/{fk}` still pass.
- JL0006 is silent on a tenant's own path-verified detail handler in a child-hosting module but still fires on the child's unscoped calls and on any `repo.all()`.
- Every in-repo design byte-identical; heavy gate green; `cargo semver-checks` clean; published 0.6.2.
