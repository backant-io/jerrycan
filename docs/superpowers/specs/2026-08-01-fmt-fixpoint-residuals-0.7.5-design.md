# fmt-fixpoint residuals: dbgen/jobs collapse + cron Box::pin wrap (0.7.5) — #221

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation (AUDIT round 1 follow-up to #218)
**Issue:** #221 — three residual `cargo fmt` fixpoint gaps found during the 0.7.4 review. A freshly-scaffolded app of the relevant SHAPE fails `cargo fmt --check` out of the box (the exact papercut #218 set out to kill), but for design shapes the #218 guard did not exercise. All are WHITESPACE-only — generated code compiles/runs identically; this is fixpoint completeness, not correctness.
**Ships as:** 0.7.5 — codegen fmt-fixpoint corrections in `jobsgen.rs` (D, F) + the db-repo/dbgen emitter (E) + an extended `scaffold_is_a_rustfmt_fixpoint` guard. No public API change → patch bump 0.7.4 → 0.7.5. All output must be byte-identical to the pinned rustfmt (1.97 / rustfmt 1.9.0, edition 2024, max_width 100) as ORACLE — diff against it, never guess.

## The three residuals (all confirmed during the #218 review by scaffolding + `cargo fmt --check`)

### D — single-element jobs migration array (`jobsgen.rs`, jobs `tests/acceptance.rs` emitter)
The jobs acceptance emitter writes `db.migrate(&[ jerrycan::db::Migration { … } ]).await…` MULTI-line. For a **single-route-module** jobs design the array has ONE element, which rustfmt COLLAPSES to one line (per max_width). The reference-slice/queue guards have ≥2 route modules so the array stays multi-line and this hid. **Fix:** emit the one-line collapsed form when the single-element array fits max_width (mirror rustfmt); keep multi-line when it doesn't or when >1 element.

### E — single-field `ActiveModel` in the db-repo `insert` (`crates/jerrycan/src/platform/dbgen.rs` or the repo emitter)
The db-repo `insert` emitter writes `ActiveModel { id: Set(…) }` MULTI-line. For an **id-only / single-field** entity (only the pk on the wire) rustfmt collapses it to one line. Guard entities are multi-field so this hid. **Fix:** emit the one-line collapsed struct-literal form when it fits max_width; keep multi-line otherwise. Grep the repo/dbgen emitters for SIBLING single-field/single-element struct or array literals with the same collapse behavior and fix them consistently (there may be more than the `insert` site).

### F — cron `Box::pin({name}::{name}(ctx))` non-monotonic wrap (`jobsgen.rs`, jobs registry cron closure)
The cron registry closure's `Box::pin({name}::{name}(ctx))` line is a fixpoint for a cron name ≤26 cols, but rustfmt's wrap is NON-MONOTONIC in name length (measured during review: one line ≤26, breaks the inner call at 27–28, one line AGAIN at 29–31, breaks at 32+). A naive "break when the inner call exceeds `fn_call_width` (60)" would REGRESS the 29–31 range. **Fix approach:** build a tiny oracle harness (emit the exact cron-closure line at name lengths ~20–40, run the pinned rustfmt, record the true wrap at each length) and make the emitter reproduce rustfmt's actual output across that range. If the boundary is genuinely non-monotonic, an EMPIRICAL boundary check derived from the oracle (with a loud comment citing the measured lengths, mirroring the `success_body`/`payload_bind` width-sensitivity precedent) is acceptable — the requirement is that a realistic cron name (up to ~40 chars) is a fixpoint. Verify the cron `Box::pin` for BOTH the one-line and each broken regime.

## The guard MUST cover each shape (this is what locks the fix — the #218 lesson)
Extend `scaffold_is_a_rustfmt_fixpoint` (`crates/jerrycan/tests/conformance.rs`) with design(s) that trigger D, E, and F — the guard is worthless for a shape it never scaffolds (that is exactly why #218's queue blind spot shipped):
- **D:** a jobs design with a SINGLE route module (one entity/module) so the acceptance migration array has one element.
- **E:** a design with an ID-ONLY (single-field) entity so the repo `insert` emits a single-field `ActiveModel`.
- **F:** a cron job with a name ≥27 chars (and ideally one in each measured regime, e.g. 27, 30, 33 chars) so the non-monotonic wrap is exercised.
Either add focused `conformance/designs/*.json` fixtures or build them inline. Each must be RED before the corresponding emitter fix and GREEN after — SANITY-CHECK by reverting the emitter and confirming the guard fails (as the #218 fix did).

## Tests
- The extended `scaffold_is_a_rustfmt_fixpoint` is GREEN after all three fixes and RED before each (verify per-residual by reverting that emitter).
- `conformance` / `reference_eval` / `eval` / `genroute_compile` byte-output ripples (D/E/F change generated whitespace for the affected shapes) — regenerate/verify inline string-assertion unit tests; no compile/behavior change.
- Existing jobsgen/dbgen unit tests updated to the collapsed/wrapped forms where they assert the affected lines.

## Gates
- `cargo test -p jerrycan` green; the extended guard green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (local PG available).
- `cargo fmt`/`clippy -D warnings`; `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps --all-features` (CI-only trap); `cargo semver-checks` (no public API change); determinism + embedded_sync.

## Version + success criteria
0.7.5. A freshly-scaffolded app of ANY shape — single-route-module jobs (D), id-only entity (E), long cron name (F) — is `cargo fmt --check` clean, and the guard locks each shape. Multi-module/multi-field/short-cron designs byte-identical; heavy gate + determinism + cargo-doc green; published 0.7.5; #221 closed. **This empties the board again → AUDIT ROUND 2.**

## Non-goals
- Running rustfmt at scaffold time (the deliberate no-rustfmt design stays). Any behavior/validator/OpenAPI change. A full audit of EVERY conceivable collapse shape beyond D/E/F (fix the confirmed three + obvious siblings; a broader sweep, if warranted, is a separate issue).
