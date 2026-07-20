# Greenability + mount-awareness (0.5.2) — #82, #81, #84, #85, #114, #120

**Date:** 2026-07-20
**Status:** Approved design, pre-implementation
**Issues:** #82 (path-FK body-trim mount-blind — also the #125 CREATE security vector), #81 (testgen leaves subroute mount params literal in URLs), #84 (jobs harness never migrates touched tables; realtime TestApp wires no channels), #85 (defaults unsettable on update; non-id path param hardcoded i64; unique-tenant seed collision), #114 (entity name colliding with a prelude identifier is uncompilable, no guard), #120 (scaffold not `cargo fmt --check` clean → JL0003 self-trip)
**Origin:** round-5 eval (faceoff5) — these were the top token-sinks (mount-blindness forced full app re-scaffolds) plus fresh-scaffold compile breaks.
**Ships as:** 0.5.2 (greenability patch; one security completion — the #125 create vector).

## Theme
Generated apps must **compile and go green out of the box**. The eval's biggest cost was builders re-scaffolding entire apps because mount-based nesting (`module mount /clubs/{club_id}`, endpoints at `/books`) is invisible to the DTO body-trim (#82) and the test-URL substitution (#81). The root is the same: several passes inspect only the endpoint's own `ep.path`, blind to the module mount prefix. Fixing that mount-awareness closes a security vector (#125 create), the #82 ergonomic friction, and the #81 red-by-construction tests together.

## Design

### A. Mount-aware path-param resolution (the shared core — #82 + #125-create)
Add a helper that resolves an endpoint's FULL path (accumulated module/subroute mounts + `ep.path`), and switch the path-redundancy check onto it.

- `Design::any_body_endpoint_resolved_path_has(entity, col)` — the recursive `walk` accumulates the mount prefix per level (`module.effective_mount()` + each subroute's `effective_mount()`), and tests `{resolved}.contains("{col}")` instead of `ep.path.contains(...)`.
- `entity_path_fk_columns` uses it → a belongs_to fk that appears anywhere in the resolved path (mount OR ep.path) is **path-redundant** → dropped from the `{Entity}Request` DTO, and the handler injects it from the path param (the existing #53b `// path-owned fk: … inject the _{col} path value` steering).
- **Security effect (#125 create):** the tenant/parent fk is no longer client-controllable on a nested-under-tenant create → a `POST /sites/{site_id}/pages` body cannot carry a foreign `site_id`; the server injects the path value. Combined with 0.5.1's update pin, cross-tenant relocation/injection is closed for both create and update.
- **Byte-identity note:** this changes generated DTOs/handlers/openapi for **nested-mount children** (they lose the now-path-redundant fk from the request body). Apps that already declared the fk in `ep.path` are unchanged. This is a correctness change (path-redundant fks shouldn't be in the body) — documented in the CHANGELOG; conformance/reference fixtures updated where they legitimately change.

### B. testgen mount-aware URLs (#81)
The test generator builds probe URLs from `ep.path` params only; subroute-inherited mount params (`/{workspace_id}/…`) survive as literal `{workspace_id}` → the router 400/404s the whole group.
- Compute each endpoint's resolved path (reuse A's mount accumulation) and substitute EVERY path param — mount-inherited and own — with a seeded/pinned id (the isolation-test generator already does chain-seeding for the tenant fk; generalize the same seed+substitute to all ancestor mount params).
- `param_count`/`regex_free_param`/`collection_url` operate on the resolved path.

### C. Harness migrations (#84)
- **Jobs harness:** the generated `jobs/tests/acceptance.rs` migrates only `JOBS_MIGRATIONS` (jobsgen.rs:242). A job touching a route-module table fails `no such table`. Fix: the jobs harness migrates `JOBS_MIGRATIONS` **plus every route module's migrations** (the same set the app's `App::build` migrates), so a job's data access resolves.
- **Realtime TestApp:** the route TestApp wires the realtime extension with zero declared topics (from prior rounds, #84 twin). Fix: declare the app's topics in the TestApp wiring so realtime tests have channels.

### D. testgen batch (#85)
- **Defaults unsettable on update:** a field with a `default` is dropped from the UPDATE DTO too (#53), so a defaulted lifecycle enum (`status`) can never be changed after create. Fix: keep `default` fields in the UPDATE request body (drop only from CREATE).
- **Non-id path param hardcoded i64:** a non-`{id}` path param is typed `i64` in the generated handler/test even when the referenced entity has a string/uuid pk. Fix: type each path param from its referenced entity's pk type.
- **Unique-on-tenant seed collision:** the isolation/uniqueness probe seeds a value that collides with a `unique` field. Fix: distinct per-probe seed values (the round-4 documented `index` workaround provably doesn't work).

### E. Fresh-scaffold compile guards (#114, #120)
- **#114 prelude-name collision:** an entity named the same as a prelude re-export (`Module`) produces `E0659` glob-import ambiguity. Fix: a **design-time validation error (new `JC0546`)** listing reserved identifiers, rejecting the design before scaffold (fail-loud, like JC0545). (Alternative — qualify generated imports — is larger; the reserved-name guard is the minimal correct fix.)
- **#120 scaffold not fmt-clean:** generated `main.rs` emits `mod` lines out of alphabetical order and no `.rustfmt.toml` ships, so `cargo fmt` reorders them → JL0003 generated-file-drift on an untouched file. Fix: emit `mod` declarations in a stable (alphabetical) order so a fresh scaffold is `cargo fmt --check` clean.

## Non-goals / deferred
- Realtime/storage/migrator transitive tenancy → 0.6.0 (Wave 3).
- The full `write_only`/response-DTO (#112), composite UNIQUE (#115), belongs_to alias (#119) → Wave 4 (expressiveness).
- #116 (steering names methods repo never emits) — **verify first**: 0.5.1's transitive recognition likely emits `create_for_memberships` for nested-subroute grandchildren now; if a fresh scaffold of the livestream shape compiles, close #116; else add a task.

## Success criteria
- A nested-mount app (`/clubs/{club_id}/books`) scaffolds, its `{Entity}Request` omits `club_id`, the create handler injects it from the path, and a body `club_id` cannot write another tenant (the #125 create vector closed — a cross-tenant-create isolation test is red on the unscoped/body-fk form, green on the injected form).
- A generated test suite for a subroute-mounted module has concrete URLs (no literal `{x_id}`) and goes green.
- A job touching a route table migrates + passes; a realtime test has channels.
- A defaulted enum is settable on update; a uuid-pk path param types correctly; a unique-on-tenant probe goes green.
- An entity named `Module` fails `check` with `JC0546`; a fresh scaffold is `cargo fmt --check` clean.
- Direct/flat/non-nested apps: byte-identical except the intended nested-mount DTO change (A), documented.
- `cargo semver-checks` clean; heavy gate green; published 0.5.2.
