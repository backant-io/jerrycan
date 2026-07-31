# Isolate concurrent scaffold builds in the conformance/eval harness (0.6.34) — #118

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation
**Issues:** #118 (HIGH test-infra. The conformance/eval/reference_eval harness scaffolds many apps and builds them all into ONE shared `CARGO_TARGET_DIR` = `target/conformance-apps` (`common::shared_app_target()`). Every scaffolded app is crate `app` with binary `app`, so they all emit the SAME final path `target/conformance-apps/debug/app` (`common/mod.rs:15`). When two builds run CONCURRENTLY into that shared target — a rogue/orphaned `cargo test` alongside a gate, or a parallel harness — one overwrites the other's `debug/app` (and can link a foreign `libroute_*`), so a test serves a STALE binary from another design → phantom `no such column`/404 failures that don't exist in the app's own source. Cost real debug turns on round-5 apps ledger+lms. The official gates dodge it by running SEQUENTIALLY with `--test-threads=1`; the bug bites under any concurrency.)
**Ships as:** 0.6.34 — a TEST-HARNESS change in `crates/jerrycan/tests/common/mod.rs` (+ the per-test build/serve call sites). NO product/codegen change — byte-identical scaffolding; this only changes where the TEST harness builds scaffolded apps. Likely no crate publish (test-only) unless a helper crate's public surface changes.

## The invariant to establish
Two scaffold builds that run at the same time MUST NOT share a mutable artifact path (the `debug/app` binary, or a same-named rlib) — regardless of whether they are two of the three test binaries run concurrently, or a rogue `cargo test` alongside a gate. The fix must ALSO preserve the reason the target is shared: **the framework (jerrycan + ~200 deps) is built ONCE and reused across apps**, or the heavy gate's runtime regresses badly.

## Two candidate fixes — implement the one that MEASURES best against both constraints
Prototype BOTH enough to measure, then ship the winner and record the measured heavy-gate wall-clock delta in the PR:

### Option A — per-app UNIQUE binary name (preserves the single shared framework build)
Keep `shared_app_target()` shared (framework built once), but make each app's FINAL artifact unique so concurrent builds can't collide. After scaffolding, the harness edits the app crate's `Cargo.toml` to set a unique `[[bin]] name = "app_<uid>"` (uid = a per-app nonce: the tempdir's basename, or an atomic counter — NOT `Math.random`/time which are unavailable/nondeterministic; a process-unique atomic is fine in test code). Update the build/serve call sites (`eval.rs`, `conformance.rs`, `reference_eval.rs`) that assume `debug/app` to use the unique name (or `cargo run` in the app dir, which runs the sole `[[bin]]` regardless of name — verify). The route/framework rlibs remain hash-distinguished by source path (already fine); only the final binary needed disambiguating. Cost: ~0 extra framework builds; a small per-app Cargo.toml edit + call-site updates. Fiddlier.

### Option B — per-TEST-BINARY target subdir (simpler; a bounded framework-rebuild cost)
`shared_app_target()` returns `target/conformance-apps/<slot>` where `<slot>` is unique per test binary (e.g. from an env the harness sets, or `std::env::var("CARGO_PKG_NAME")` + a per-binary constant, or a `OnceLock` nonce per process). The three binaries then never share, so running them concurrently is safe; within a binary `--test-threads=1` keeps apps serial (reusing that binary's framework build). Cost: the framework is built once PER TEST BINARY (≈3×) instead of once total — MEASURE the heavy-gate delta; if it is small (framework already cached across runs by cargo's fingerprint on unchanged deps) prefer this for simplicity (Rule 2). Does NOT protect two concurrent runs of the SAME binary (rare); note that limitation if shipping B.

Prefer **A** if the measured framework-rebuild cost of **B** is material (adds minutes to every publish eval gate); prefer **B** if the cost is negligible (cargo reuses the fingerprinted framework build across the sibling target subdirs — verify) and simplicity wins.

## Tests
- **A contamination regression test:** scaffold TWO different designs (distinct schemas — one with a column the other lacks) and build/serve them CONCURRENTLY through the harness; assert each serves ITS OWN app (a request that depends on the distinct column succeeds on the right one and 404/errs on the other) — i.e. no cross-app bleed. This test FAILS on today's shared `debug/app` and passes with the fix. Gate it behind the same `#[ignore]` as the other heavy harness tests, and run it `--include-ignored`.
- Keep `--test-threads=1` where it is (defense in depth); the fix makes the harness correct even WITHOUT it, but don't remove the serialization in the same PR.

## Gates
- `cargo test -p jerrycan` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` all green AND the new contamination regression test green. RECORD the heavy-gate wall-clock before/after in the PR (the perf-constraint evidence).
- `cargo fmt`/`clippy -D warnings`.

## Success criteria
- Two concurrent scaffold builds never share a mutable artifact path; the contamination regression test proves no cross-app bleed.
- The framework is not rebuilt per-app (heavy gate wall-clock not materially regressed — measured + recorded).
- The official gates stay green; #118 closed. Test-harness only — scaffolding byte-identical.

## Non-goals
- Changing the PRODUCT scaffold (the user's app crate stays `app` with binary `app` — the unique naming, if used, is a TEST-HARNESS-only rewrite). A general parallel-test framework. Removing `--test-threads=1` (keep it as defense in depth).
