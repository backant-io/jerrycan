# testgen: isolation test for a read-leg-less module compiles and actually asserts (0.7.11) — #240

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 4 finding, HIGH)
**Issue:** #240 — `tenant_owned_isolation_test` (`crates/jerrycan/src/platform/testgen.rs:1768`), `per_user_isolation_test` (`:1995`), and `public_read_isolation_test` (`:2108`) bind `row` and `cookie2` UNCONDITIONALLY but consume them only inside the OPTIONAL get/list/delete probe legs. For a module whose isolation probe has NO read leg — a **nested** tenant module (mount carries the tenant fk, e.g. `/orgs/{org_id}/events`) with a creator + list but no `GET /{id}` (the list leg is suppressed for nested mounts via `is_nested`, `~1851`, so all three legs are `None`), or a per-user create/update-only module — the generated isolation test is (1) **setup-only, asserting NOTHING** about cross-tenant/cross-user isolation (a security-critical negative control silently vacuous), and (2) leaves `row`/`cookie2` unused → `jerrycan check`'s `cargo clippy --all-targets -- -D warnings` FAILS in a tool-owned `acceptance.rs` (the agent is wedged; regenerating reproduces it). The author already guarded `id`/`id_value` bindings (`~1832-1834`) — the `row`/`cookie2` SETUP was left unconditional. Not caught by CI: every isolation unit test in `tests/testgen.rs` uses a module WITH a `GET /{id}` (`get_one = Some`).
**Ships as:** 0.7.11 — a testgen fix (two parts: stop the vacuous/uncompilable emission, AND give a nested list read leg a real isolation probe). Patch bump 0.7.10 → 0.7.11. Non-isolation-test output byte-identical.

## Part A — a nested tenant module's LIST read leg gets a cross-tenant isolation probe
The deeper defect: a nested tenant module with a collection LIST (`GET /orgs/{org_id}/events`) but no `GET /{id}` has ZERO generated isolation coverage today, because the list leg is suppressed for nested mounts (`is_nested`, `~1851`). The RUNTIME is safe — the `Dep<Tenant>` membership guard (#78/#102) scopes/denies a foreign-tenant list — but the negative CONTROL is missing. Emit a LIST isolation probe for a nested tenant (and per-user) module: seed a row as tenant-1 (user-1), then as a tenant-2 member (`cookie2`) GET the LIST at tenant-1's concrete mount path (`/orgs/{org1_id}/events`) and assert the guard denies it — a **404** (the `Dep<Tenant>` guard 404s a non-member of `org1`), OR an empty/own-only body if the framework returns 200 with a scoped-empty list (match the actual guard behavior — verify by scaffolding + serving, or by reading the generated handler/guard; the existing nested `GET /{id}` isolation probe already encodes the correct expected status — mirror it for the list). This consumes `row`/`cookie2`, fixing the unused-var break for this shape AND closing the coverage gap. The per-user list read leg gets the analogous cross-user probe.

## Part B — a genuinely read-less module emits a clean no-op isolation test
For a module with NO read endpoint at all (create-only, or create + update-only — no list, no `GET /{id}`, no delete), there is nothing to isolation-probe (no way to READ another tenant's/user's row), so the isolation test must be a clean NO-OP: emit an empty/trivial test body (or skip emitting the fn) with NO unbound `row`/`cookie2` and NO false "asserts isolation" claim. Guard the `row`/`cookie2` SETUP bindings on at least one probe leg being present (mirror the existing `id`/`id_value` guard at `~1832-1834`), and — if the whole body would be empty — either omit the isolation fn or emit a documented no-op (a comment: "no read endpoint to isolation-test; the guard/owner-scope is the enforcement"). It MUST compile under `-D warnings` and MUST NOT masquerade as an assertion-bearing test.

## The invariant (MUST hold)
Every generated isolation test either (a) contains a REAL cross-tenant/cross-user assertion (a probe leg exists — including the new nested list leg), or (b) is a clean no-op for a module with no readable endpoint — NEVER a setup-only body that asserts nothing yet is named `tenant_a_cannot_read_tenant_b_*`. And every generated `acceptance.rs` compiles under `jerrycan check`'s `-D warnings`.

## Tests (add — CI missed this because every isolation unit test used a module WITH `GET /{id}`)
- **Compile+assert:** a testgen unit test for a NESTED tenant module with `POST "/"` + list but NO `GET /{id}` → the generated isolation test compiles (no unused vars) AND contains the list cross-tenant probe with its assertion. Model on `tests/testgen.rs` (`:444`/`:840`/`:572`).
- **Per-user create/update-only:** a per-user module with create + `PUT /{id}` only (no read leg) → the generated isolation test is a clean no-op that compiles (or the fn is omitted); no unused `row`/`cookie2`.
- **genroute_compile / a compile fixture:** add a nested-tenant list-only design (and/or a per-user create-only design) to the compile gate so a future regression (unused-var / assertion-less) turns it RED. This is the make-impossible half — the exact shapes the audit found, now compiled by CI.
- Existing isolation tests (modules WITH `GET /{id}`) byte-identical.

## Gates
- `cargo test -p jerrycan` green; the new testgen unit tests + compile fixture green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (the new nested-list probe changes generated isolation tests for any nested-tenant-list design; regenerate/verify).
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; determinism + embedded_sync.

## Version + success criteria
0.7.11. A nested tenant module with a list-but-no-detail read leg, and a per-user create/update-only module, both generate an `acceptance.rs` that COMPILES under `jerrycan check -D warnings` (no wedged agent) AND whose isolation test either carries a real cross-tenant/cross-user assertion (the new list probe) or is an honest no-op — never a silently-vacuous security negative control. A compile fixture locks both shapes. Modules with a `GET /{id}` byte-identical; heavy gate green; published 0.7.11; #240 closed.

## Non-goals
- Refusing the read-leg-less shape (it's a valid, secure design — the guard enforces isolation; the fix is TEST coverage + compilability, not a refusal). Changing the runtime guard/scoping. The presence + post-connect realtime findings (#241/#242 → 0.7.12).
