# Gate-honesty completion: full 401 coverage + jobs hollow-green (0.6.4) — #153 / #156

**Date:** 2026-07-23
**Status:** Approved design, pre-implementation
**Issues:** #153 (a guarded endpoint silently lacks its `_without_auth_is_401` test in two non-`skip` testgen branches), #156 (a jobs-only design — cron, no endpoints — still reads `ok:true` with zero acceptance tests: the #123a hollow-green hole one surface over).
**Origin:** 0.6.3 whole-branch + T3 reviews (both flagged as fast-follows).
**Ships as:** 0.6.4 — completes the 0.6.3 "gate honesty" coverage: no guarded endpoint silently lacks its 401 test, and no gen-tests-eligible surface reads green with zero tests. Mechanical, reuses 0.6.3's proven patterns. Semver clean.

## The two fixes

### A. #153 — emit the 401 guard test in the two remaining non-`skip` branches
0.6.3 (#123b) made `probe:"skip"` keep the `_without_auth_is_401` test for a guarded endpoint. The same silently-dropped-401 survives in two branches of `testgen.rs` `unit_tests` that are orthogonal to `skip`:
- a **guarded `/{id}` detail endpoint with no seed creator** (~testgen.rs:599-604) → TODO only, no 401 test;
- a **guarded 2+-param endpoint** (~testgen.rs:605-610) → TODO only, no 401 test.

**Fix:** in both branches, when the endpoint `is_guarded()` (`auth_required || !required_roles.is_empty()`) and is not a `public_read` GET (mirror the exact `guarded` predicate #123b uses), emit `push_401_test` with a literal id substitution (`concrete_mount_base` → every `{param}` = `1`) — a 401 rejection needs no seed, so "no creator" / "multi-param" don't block it. Reuse the #123b code path (do not duplicate). `push_401_test` increments `out.count`, so `expected_failing` flows through unchanged.

**Tests:** a guarded `/{id}` endpoint with no creator → `_without_auth_is_401` emitted (literal id); a guarded 2-param endpoint → `_without_auth_is_401` emitted; an UNGUARDED endpoint of each shape → NO 401 test (no false assertion). Red-before/green-after.

**Byte-identity:** only guarded endpoints of these two shapes gain a 401 test. Sweep the conformance/eval/inline designs — confirm which (if any) goldens legitimately gain a test (a guarded no-creator `/{id}` or guarded multi-param endpoint in a fixture) and update those goldens; if none, byte-identity is total. **Behavior change (regen):** such designs gain a 401 test + `expected_failing` +1; passes on a correct app, red only where a guard was hand-weakened.

### B. #156 — `JC0551` for a jobs-only design
0.6.3's `JC0551` requires `crates/routes/{m}/tests/acceptance.rs` for each module **with endpoints**. But `gen-tests` also writes `crates/jobs/tests/acceptance.rs` (`jobsgen::write_jobs_acceptance`), unchecked — so a design with cron jobs but **no endpoint-bearing modules** never trips JC0551, never gets gen-tested, and `check` reads `ok:true` with zero tests.

**Fix:** in the `checkpipe.rs` acceptance-presence step, when `design.jobs` is non-empty, also require `crates/jobs/tests/acceptance.rs` to exist; missing → **`JC0551`** (reuse the code; message adapted, e.g. "no acceptance tests for jobs — run `jerrycan gen-tests`"). Use the same file-existence signal (a gen-tested all-TODO jobs file satisfies it).

**Tests:** a jobs-only design (jobs declared, no endpoint modules) → `check` raises JC0551 before gen-tests, green after; a design with endpoints AND jobs → both the module file(s) and the jobs file are required. Extend the checkpipe unit tests.

**Behavior change (regen):** a jobs-only app that never ran gen-tests flips green→red — same class + fix as #123a, already release-noted; note the jobs extension in the 0.6.4 CHANGELOG.

## Byte-identity, ordering, scope
- Independent fixes (testgen vs checkpipe); either order. Both reuse existing code paths (#123b's `push_401_test`, #123a's JC0551 step) — no new codes, no new pub API, semver clean.
- No emitter/generated-app change except the #153 guarded-shape 401 tests (goldens updated where a fixture legitimately hits the shape) and the #156 check diagnostic.

## Success criteria
- A guarded `/{id}`-no-creator and a guarded 2+-param endpoint each generate `_without_auth_is_401`; unguarded ones do not.
- A jobs-only, never-gen-tested design → `check` raises `JC0551`; gen-tested (even all-TODO) → green.
- Byte-identity except the intended guarded-shape 401 tests; heavy gate green; `cargo semver-checks` clean; published 0.6.4.
