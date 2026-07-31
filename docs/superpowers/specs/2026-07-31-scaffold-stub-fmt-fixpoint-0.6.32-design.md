# Scaffold agent-owned stubs are `cargo fmt` fixpoints (0.6.32) — #165

**Date:** 2026-07-31
**Status:** Approved design, pre-implementation
**Issues:** #165 (a FRESH scaffold's agent-owned stubs — `crates/routes/<mod>/src/handlers.rs` and `repo.rs` — are NOT `cargo fmt`-clean, so `cargo fmt --check` (and any first `jerrycan check`/CI fmt step in the new app) fails before the agent has written a line. The tool-owned files (`main.rs`, `migrations.rs`) were made rustfmt fixpoints in #128; the agent-owned stubs were not.)
**Ships as:** 0.6.32 — a codegen change to the stub templates in `crates/jerrycan/src/platform/genroute.rs` (the `handlers.rs` + `repo.rs` emitters) so their output is a `cargo fmt` fixpoint. Follows the established #128 convention: **pre-wrap generated code EXACTLY as the pinned toolchain's rustfmt (1.97, default config, edition 2024) formats it** — do NOT add a runtime `cargo fmt` pass (the project deliberately hand-pre-wraps so it needn't shell out to rustfmt and needn't depend on the user's rustfmt version; see `mounting.rs:30-45`). The generated app is byte-DIFFERENT from today (the stubs change) but that IS the fix; every scaffold is deterministic and a fmt fixpoint.

## The observed dirt (reproduced isolated, default rustfmt, max_width=100, fn_call_width=60)
1. **handlers.rs import order** — emitted `use jerrycan::prelude::*;` then `use super::model::*;` / `use super::repo::*;`. rustfmt `reorder_imports` sorts them: `super::model`, `super::repo`, then `jerrycan::prelude`. **Deterministic** → emit in that order.
2. **`Err(Error::internal("<op> not implemented — replace this stub"))`** — the inner `Error::internal("…")` call exceeds `fn_call_width` (60) for every realistic op name, so rustfmt breaks the string arg onto its own line:
   ```rust
   Err(Error::internal(
       "<op> not implemented — replace this stub",
   ))
   ```
   Confirm the threshold: the one-line `Error::internal("<op> not implemented — replace this stub")` call body is `16 + op_len + 37 + 3 = 56 + op_len` columns; > 60 for `op_len ≥ 5` (every generated handler name — `list_x`, `get_x`, `create_x`, …). Emit the WRAPPED form. (If a pathologically short op could stay ≤60, gate the wrap on the computed width like #128/mounting.rs does — but verify no generated op name is short enough to matter and state so.)
3. **create/update handler fn signature** — `pub(crate) async fn create_<snake>(_repo: Dep<{E}Repo>, Json(_body): Json<{E}>) -> Result<Created<{E}>>` exceeds max_width (100) for typical names → rustfmt breaks each PARAM onto its own line:
   ```rust
   pub(crate) async fn create_<snake>(
       _repo: Dep<{E}Repo>,
       Json(_body): Json<{E}>,
   ) -> Result<Created<{E}>> {
   ```
   This is **WIDTH-DEPENDENT** (a short entity name fits one line; a long one wraps). Reproduce rustfmt's decision the SAME way `mounting.rs` does for the rate-limit line: compute the one-line signature width and emit the multi-line form iff it exceeds max_width (100). Do this for EVERY handler shape whose one-line signature can exceed 100 (create/update carry a body param, so they're the long ones; `list`/`get`/`delete` may or may not — compute each). The boundary is empirical against the pinned rustfmt 1.97 — mirror `mounting.rs`'s approach (a helper that picks the regime by width).
4. **repo.rs `use` order** — `use std::sync::Mutex;` then `use std::sync::atomic::{...}`. rustfmt sorts `atomic` before `Mutex`. **Deterministic** → emit `atomic` first.
5. **repo.rs `Self { items: …, next_id: … }`** — the struct literal exceeds fn_call_width → rustfmt breaks each field onto its own line. This is FIXED-WIDTH (no entity name in the literal) → emit the wrapped form always:
   ```rust
   Self {
       items: Mutex::new(BTreeMap::new()),
       next_id: AtomicI64::new(1),
   }
   ```
6. **repo.rs trailing blank line** — the file ends with an extra blank line rustfmt strips; trim it.

## Fix
Edit the `handlers.rs` and `repo.rs` stub emitters in `genroute.rs` so each of (1)–(6) is emitted exactly as rustfmt 1.97 produces. For the width-dependent (3), add/reuse a width-regime helper (model on `mounting.rs`'s rate-limit regime selection) that picks one-line vs per-param-wrapped by the computed one-line width vs 100. Keep the doc-comment header lines (`//! Handlers for …`) unchanged (they're already clean). Every other generated file is untouched.

## Tests
- **The fixpoint test (the #165 proof):** a test that scaffolds a fixture design (reuse the conformance `scaffold_golden` path or `jerrycan new` into a tempdir) and runs the pinned toolchain's `rustfmt --check` (or `--emit stdout` and compares) on EVERY agent-owned `handlers.rs` + `repo.rs`, asserting NO diff. Model on `dbgen.rs`'s `rustfmt` helper (it already pipes a file through the pinned rustfmt). Cover BOTH a short-entity design (signature fits one line) AND a long-entity design (signature wraps) so the width-regime (3) is exercised in both directions.
- **Determinism/byte-identity:** the generated stubs change once (that IS the fix), but generation stays deterministic (`determinism.rs` green) and a re-`cargo fmt` of the scaffold is a no-op (the fixpoint property).
- Existing conformance `scaffolded_app_builds_with_zero_warnings` + golden tests updated to the new stub bytes (they build the scaffold — the new fmt-clean stubs must still compile + build clean).

## Gates
- `cargo test -p jerrycan` (genroute stub tests + the new fixpoint test) green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` `--include-ignored` — the scaffold-build conformance tests exercise the new stubs; they must build + the golden output matches the new fmt-clean bytes.
- `cargo fmt`/`clippy -D warnings`; the fixpoint test IS the byte-identity guard for the scaffold.

## Success criteria
- `jerrycan new` on ANY design produces `handlers.rs`/`repo.rs` stubs that are a `cargo fmt` fixpoint (fmt is a no-op; `cargo fmt --check` passes on a fresh scaffold), for both short- and long-name entities.
- No runtime rustfmt/`cargo fmt` pass added (stays in the #128 pre-wrap convention); determinism + heavy gate green; published 0.6.32; #165 closed.

## Non-goals
- Adding a `cargo fmt` pass to `jerrycan new` (rejected — the project pre-wraps to avoid a rustfmt-version/availability dependency; `mounting.rs:30-45`). Reformatting tool-owned files (already fixpoints since #128). Any non-stub generated file.
