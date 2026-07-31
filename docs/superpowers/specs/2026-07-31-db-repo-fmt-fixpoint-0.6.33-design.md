# db tenant/owner repo templates are `cargo fmt` fixpoints (0.6.33) — #201

**Date:** 2026-07-31
**Status:** Approved design, pre-implementation
**Issues:** #201 (follow-up to #165. #165 made the SIMPLE stub templates — `handlers.rs` + the memory `repo.rs` — `cargo fmt` fixpoints, but SCOPED OUT the db TENANT/OWNER membership-repo templates (`reference-slice` shape): rustfmt's greedy-fill of a long `use jerrycan::db::sea_orm::{…}` import (7+ traits) and width-dependent wrapping of long membership/scoped method signatures (`update_for_memberships`, `create_for_memberships`, `update_for`, …) mean a fresh db-tenant scaffold's agent-owned `repo.rs` is NOT a `cargo fmt` fixpoint. Non-breaking today — the app still builds and only the tool-owned `main.rs` is fmt-gated by CI, and agent stubs are JL0003-exempt — but `cargo fmt --check` on a fresh db-tenant scaffold fails, the same first-run papercut #165 fixed for the simple shapes.)
**Ships as:** 0.6.33 — a codegen change to the db repo-method templates in `crates/jerrycan/src/platform/genroute.rs`, completing #165's fixpoint work for the tenant/owner/membership repo shapes. Same convention: pre-wrap exactly as the pinned toolchain's rustfmt formats it (NO runtime `cargo fmt` pass). Reuse the width-regime helper #165 added.

## The dirt (db tenant/owner repo.rs; reproduce isolated with the pinned rustfmt, like #165)
1. **The `sea_orm` import** (`genroute.rs:2802` `use jerrycan::db::sea_orm::{{{filter_imports}}};`, and the concrete list e.g. `genroute.rs:6400` — `ActiveModelTrait, ActiveValue::Set, ConnectionTrait, EntityTrait, FromQueryResult, QueryOrder` and wider variants): when `filter_imports` is long enough that the one-line `use …::{…};` exceeds `max_width` (100), rustfmt GREEDY-FILLS the braces across multiple lines (as many idents per line as fit, indented). Emit the exact greedy-filled form when it exceeds 100; keep one line when it fits. (Confirm rustfmt's exact greedy-fill layout for the pinned version — width-gate it, do not unconditionally wrap.)
2. **The membership/scoped method signatures** (`create_for_memberships` ~1654/1762, `update_for_memberships` ~1676/1784, `remove_for_memberships` ~1700/1807, and the `*_for`/`update_for`/`all_for`/`get_for` family): `pub async fn update_for_memberships(&self, user_id: String, id: {key}, item: {entity}) -> Result<bool>` exceeds `max_width` for longer `{entity}`/`{key}` → rustfmt breaks each PARAM onto its own line. WIDTH-DEPENDENT — reproduce with the SAME width-regime helper #165 added (compute the one-line signature width, emit per-param-wrapped iff > 100). Apply to EVERY db repo-method template whose one-line signature can exceed 100.
3. Any other rustfmt drift the fixpoint test surfaces for the db-tenant/owner shapes (e.g. long conditional-SQL string wrapping, `Self`/struct-literal fills) — fix each to the pinned rustfmt's output, same as #165's "3 extras beyond the spec's 6".

## Fix
Extend #165's width-regime treatment to the db repo-method emitters. Reuse the helper #165 added (grep the genroute width-regime fn from #165 / commit 0628a9c) — do not add a second one. For the `sea_orm` import, emit rustfmt's greedy-fill layout when the one-line import exceeds 100.

## Tests
- **Extend the #165 fixpoint test** (`scaffold_stub_handlers_and_repos_are_rustfmt_fixpoints`, `crates/jerrycan/tests/dbgen.rs`): add the DB TENANT/OWNER shapes to its `cases` — the `reference-slice` design (tenant membership surface) and an owner-scoped (`user_id`) db design — so every agent-owned `repo.rs` (including the membership methods) is round-tripped through the pinned rustfmt with zero diff. Cover a LONG-entity-name tenant design so the width regime (2) is exercised in the wrapping direction. Remove the scope-note carve-out from that test's doc comment once covered.
- The scaffold must still BUILD (the `scaffolded_app_builds_with_zero_warnings` / conformance scaffold-build tests) with the reflowed templates.

## Gates
- `cargo test -p jerrycan` (genroute + the extended fixpoint test) green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` `--include-ignored` — the reference-slice (a db tenant design) is scaffolded + built by these; the reflowed repo templates must build + its acceptance battery stay green. Local PG container available; reset schema + libpq psql on PATH if a PG test needs it. Re-run a suspicious unrelated conformance flake alone (#118).
- `cargo fmt`/`clippy -D warnings`; determinism green (the db repo templates change once — that IS the fix — generation stays deterministic; the fixpoint test is the guard).

## Success criteria
- A fresh scaffold of ANY design — including a db tenant/owner membership design — has agent-owned `repo.rs` (and `handlers.rs`) that survive `cargo fmt --check` untouched, for both short- and long-name entities.
- The #165 fixpoint test covers the db tenant/owner shapes (carve-out removed); heavy gate + determinism green; published 0.6.33; #201 closed. #165's `handlers.rs`/memory-repo fixpoint property is preserved.

## Non-goals
- A runtime `cargo fmt`/rustfmt pass (rejected — same convention as #165/#128). Any non-repo generated file. Changing the repo methods' behavior — this is purely their FORMATTING.
