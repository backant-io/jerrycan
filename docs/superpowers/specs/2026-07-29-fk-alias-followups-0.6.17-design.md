# fk-alias follow-ups: alias-aware path-param key type + live create-probe (0.6.17) — #178, #179

**Date:** 2026-07-29
**Status:** Approved design, pre-implementation
**Issues:** #178 (a `belongs_to` fk **alias** used as a path param on a String/uuid-pk target types as `i64` → E0308) and #179 (the #119 fk-alias conformance fixture is scaffold + migration-SQL only; the two-aliased-fk **INSERT** path is never exercised by a live create probe). Both are #119 fk-alias follow-ups.
**Ships as:** 0.6.17 — one codegen fix (#178) + one test-coverage extension (#179). Byte-identical except the aliased-fk path-param type (#178), which only changes for a String/uuid-pk aliased fk path param.

## Part A — #178: alias-aware `path_param_key_type`
`Design::path_param_key_type` (design.rs:1535) resolves a path param's Rust key type via `find_name` (design.rs:1536), which matches the param ONLY against each entity's DEFAULT fk column (`Design::fk_column(entity_name)` = `snake(entity)_id`). It never checks `belongs_to` aliases. So an **aliased** fk used as a path param — e.g. `/{from_account_id}` where `Transfer belongs_to Account as from_account` — matches no entity's default fk (`account_id ≠ from_account_id`) → falls through to `"i64"`. If `Account`'s pk is a String/uuid, the generated path-param binding types as `i64` → an **E0308** mismatch (the string-identity landmine).

**Fix:** extend `find_name` so, in addition to matching an entity's default fk column, it also matches any `belongs_to`'s aliased fk column and resolves to that belongs_to's TARGET entity (whose pk type is the answer). Concretely, for each module/subroute entity, also scan `e.belongs_to`: if `b.fk_column() == param`, return `b.entity`. Order: keep the existing entity-default match first (byte-identical for every un-aliased design — `b.fk_column()` for an un-aliased belongs_to equals `snake(entity)_id`, which the default match already covers, so aliases are the only new resolutions). Then `target_key_rust_type(name)` gives the right pk type.

**Unit test:** a design with `X belongs_to Account as from_account` (Account pk = String/uuid) and a route path param `{from_account_id}` → `path_param_key_type("from_account_id") == "String"` (was `"i64"`). A non-aliased design is unchanged (regression: `{account_id}` still resolves to Account's pk type as before).

## Part B — #179: live create-probe for the two-aliased-fk INSERT
`fk_alias_two_refs_and_self_ref_go_green_on_a_correct_scaffold` (conformance.rs:2737) scaffolds the ledger design and asserts the migration SQL (the two aliased fk columns + distinct constraint names), but installs GET-only handlers — the two-aliased-fk INSERT path (seeding both parents under `from_account_id`/`to_account_id`) is only unit-verified in testgen, never run.

**Fix:** extend the existing fixture (or add a sibling `#[ignore]` heavy test modeled on it) to prove the INSERT end-to-end:
- Install a correct `create_transfer` handler that inserts a `Transfer { from_account_id, to_account_id, amount }` (both fks distinct, referencing seeded `Account` rows), and the `create_account` handler.
- Scaffold → `gen-tests` → the generated create acceptance test (or a hand-driven live-serve POST, mirroring the reference-slice battery's raw-HTTP POST) creates two accounts then a transfer referencing both, asserting **201** and that the persisted row carries the two distinct aliased fks.
- Keep it under the existing conformance heavy gate (it already runs `--include-ignored`). Reuse the fixture's `FK_ALIAS_LEDGER` design (add a `create_transfer` POST endpoint if the design lacks one).

Do NOT change the framework for Part B — it is test coverage proving the already-shipped #119 behavior. If the live-serve harness is disproportionately heavy, a `jerrycan check`-green-on-a-correct-insert-handler assertion (compile + a repo-level insert unit) is an acceptable lighter proof — but state which was used.

## Byte-identity + gates
- Part A changes `path_param_key_type` output ONLY for a String/uuid-pk aliased-fk path param (no corpus design has that shape → determinism unchanged; confirm). Part B is test-only.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` (carries Part B) + `eval` `--include-ignored` before done.
- `cargo semver-checks` clean (internal fn + a test — no public API change).

## Success criteria
- #178: an aliased fk path param on a String/uuid-pk target types correctly (unit test RED before, GREEN after); un-aliased designs byte-identical.
- #179: a live/compile create-probe proves the two-aliased-fk INSERT works (201 + two distinct persisted fks).
- Heavy gate green; semver clean; published 0.6.17; #178/#179 closed.

## Non-goals
- Aliasing the tenancy/identity fk (out of scope, #119 non-goal). Reworking `path_param_key_type` beyond the alias match.
