# Gate honesty: pin & prove, honest check, guard-preserving skip, identity docs (0.6.3) — #121 / #123

**Date:** 2026-07-22
**Status:** Approved design, pre-implementation
**Issues:** #121 (SQLite FK enforcement rests on an unpinned upstream default, unproven), #123 (a: `check` ok:true with zero acceptance tests; b: `probe:"skip"` also deletes the `_without_auth_is_401` guard test; c: owner-scoping keys only on the literal `user_id` fk).
**Origin:** round-5 eval (faceoff5-2026-07-20.md).
**Ships as:** 0.6.3 — a gate-honesty patch: every fix makes a guarantee the framework *claims* actually hold or be proven, per the project's "green means safe" bar. Semver clean (no new public Rust API; JC0551 is additive design-time diagnostic). One behavior change is intentional and loud (#123a); one dependency-floor bump (#121).

## Premise corrections (carry into release notes — do not overstate)
- **#121 is pin-and-prove, NOT a bug fix.** SQLite FK enforcement already WORKS: sqlx-sqlite 0.8.6 enables `foreign_keys=ON` by default on every connection (overriding SQLite's off-by-default), sea-orm routes through it, and `Db::connect` leaves it intact — empirically verified (`PRAGMA foreign_keys=1`, orphan rejected, `ON DELETE CASCADE` fires). The gap is that the guarantee is **implicit (an upstream default) and unproven (no test)**. Do NOT claim "FKs were unenforced."
- **#123(c) generalization needs new contract surface** (there is no auth identity entity to derive from) — deferred to 0.7 as #150. 0.6.3 documents the requirement.

## The fixes

### A. #121 — pin FK enforcement explicitly + prove it
- **Pin** (`crates/jerrycan-db/src/lib.rs:62-64`, in `Db::connect`): set `foreign_keys(true)` explicitly via sea-orm's `ConnectOptions::map_sqlx_sqlite_opts(|o| o.foreign_keys(true))` (runs on every pooled connection by construction — a post-connect `execute("PRAGMA …")` is the wrong shape, lost on pool reconnect). This removes the reliance on sqlx's default. **Dependency floor:** `map_sqlx_sqlite_opts` was added in the sea-orm 1.1.x line; the workspace pins `sea-orm = "1"` (Cargo.toml) — raise the floor to the exact minor that introduced it (verify at implementation; published consumers resolving old 1.0.x would otherwise fail to compile). Gate this on the SQLite backend only; Postgres is untouched (enforces natively).
- **Prove** (new test in the `#[cfg(test)]` mod of `crates/jerrycan-db/src/lib.rs`, near `unique_violations_map_to_409_conflict`): through `Db::connect("sqlite::memory:")`, assert `PRAGMA foreign_keys` returns 1, an orphan insert is rejected, and `ON DELETE CASCADE` removes children. This is the real deliverable — it converts an assumed guarantee into a CI-proven one (a future sqlx default flip turns it red).
- Fix the now-stale comment at `crates/jerrycan/tests/dbgen.rs:302-303` ("sqlite ignores FKs by default" — its manual `PRAGMA foreign_keys=ON` is redundant; keep or drop the manual pragma but correct the comment).
- **schema.json unchanged** (`enforced:true` matches verified reality). Byte-identity: none. Semver: none (internal; only the dep-floor bump).

### B. #123(a) — honest check: distinguish "no acceptance tests yet" from "green"
`checkpipe.rs` `run_all` folds a zero-test `cargo test` (exit 0) into ok:true, so a scaffold that never ran `gen-tests` reads green. Add a step (before the "jerrycan lints" step, shared by CLI `cmd_check`, the MCP twin, and the `package` gate): for each top-level `design.modules` entry with ≥1 endpoint, require `crates/routes/{name}/tests/acceptance.rs` to exist; a missing file raises new **`JC0551`** ("no acceptance tests for module `{m}` — run `jerrycan gen-tests --module {m}`"). Register `JC0551` in codes.rs (+ `jerrycan explain`).
- **File-existence is the correct signal, not test count:** `gen-tests` writes the acceptance file even when every endpoint is an all-TODO banner (a design with legitimately nothing greenable), so a gen-tested design never false-alarms; only a *never-gen-tested* scaffold trips it.
- **Behavior change — LOUD, intended:** `check` and `package` flip green→red for any app that never ran `gen-tests`. In-repo tests that assert exactly this hollow green (`conformance.rs` `fresh_scaffold_passes_jerrycan_check` and its db/auth siblings, `eval.rs` fixture harness, `package.rs`/`deploy.rs` gate callers) must be updated in the same PR to run `gen-tests` before `check` — verify each check-caller. The scaffold's own `next_step` already orders `gen-tests` before `check`, so the change enforces the documented workflow. Release-note it.
- Extend the `checkpipe.rs` report-serialization unit test; add: missing-acceptance-file → JC0551; banner-only (all-TODO) file → still green.

### C. #123(b) — `probe:"skip"` must keep the auth-guard test
In `testgen.rs`, the `gated` branch (~:481-490, `gated = endpoint_is_credential_gated(ep) || probe_skip`) emits only an agent TODO and returns before `push_401_test`, so a session-guarded endpoint marked `probe:"skip"` loses its `_without_auth_is_401` negative test along with the happy-path probe — a passing security assertion silently dropped.
- **Fix:** when the endpoint `is_guarded()` (the same predicate genroute keys the real handler guard on), still emit `push_401_test` even under `probe:"skip"`; for param paths substitute a literal id (as the 404 probe does via `regex_free_param`) — a 401 rejection needs no seed. Apply the same to the skipped-*creator* sibling branch (~:521-532), where a guarded `/{id}` sibling loses its 401 test too. Reword the TODO (it currently tells the agent to write the rejection test themselves → success-test-only). `push_401_test` already increments `out.count`, so `expected_failing` flows through unchanged — no special-casing.
- Byte-identity: **no in-repo golden uses guarded+skip** (reference-slice/todo-api carry no `probe` key; the inline skip tests are `public`/unguarded), so no golden changes. New tests: guarded + `probe:"skip"` → 401 emitted + TODO retained + count correct; guarded sibling of a skipped creator → 401 emitted.
- Docs: `docs/ai/00-designing.md:237-241` (promises "you write the rejection tests yourself") must be reworded (the 401 guard test is now generated) **plus its byte-identical embedded twin** `crates/jerrycan/embedded/ai/00-designing.md` (embedded_sync gate — edit both identically).
- **Behavior change on regen:** an external guarded+`skip` design gains a 401 test and a shifted `expected_failing`; on a correct app the new test passes (the handler guard is real), red only where a guard was hand-weakened — the point. Release-note it.

### D. #123(c) — document the `user_id` owner-scoping requirement (docs-only; generalize = #150)
Per-user owner-scoping, the #34 server-injected fk, and `public_read` all key on the literal derived column `user_id`; an identity entity named `Account` (fk `account_id`) silently gets none, and its fk stays client-writable. Generalizing needs a new `auth.identity` contract field (#150, 0.7). For 0.6.3, document it:
- Add an explicit section to `docs/ai/10-auth.md` (which today says nothing about identity/owner-scoping): the auth identity entity must be named **`User`**; owner-scoping, the server-injected fk (#34), and `public_read` key on the literal derived column `user_id`; an identity named `Account`/`Member` gets **no** owner-scoping and its fk stays client-writable — name it `User` (or track #150). Sharpen the parenthetical at `docs/ai/00-designing.md:251-252`.
- Update the **embedded twins** `crates/jerrycan/embedded/ai/10-auth.md` and `.../00-designing.md` identically (embedded_sync gate).
- No code, no byte-identity, no semver. (Advisory `validate` question deliberately NOT added — the heuristic ("auth + guarded belongs_to entities + no identity fk anywhere") false-warns on designs with non-ownership `belongs_to`.)

## Byte-identity & ordering
- #121: no generated output. #123(a): CheckReport gains a diagnostic only in the previously-hollow case. #123(b): no golden changes (no guarded+skip fixture). #123(c): docs + embedded twins only. The only test churn is the intended hollow-green test updates (#123a) and the new tests.
- Independent tasks; suggested order T1 (#121) → T2 (#123a) → T3 (#123b + #123c). T3's two docs edits touch `00-designing.md` — do them in one task to avoid a twin-sync race.

## Success criteria
- `Db::connect` SQLite pins `foreign_keys(true)` and a test proves FK rejection + cascade through it; Postgres unchanged.
- `jerrycan check` on a freshly-scaffolded, never-gen-tested app raises `JC0551` (was: ok:true); a gen-tested all-TODO app stays green.
- A guarded endpoint with `probe:"skip"` still emits `_without_auth_is_401`; the docs no longer claim otherwise.
- `docs/ai/10-auth.md` (+ twin) states the `user_id` identity requirement.
- Every in-repo design byte-identical (except the intended hollow-green test updates); heavy gate green; `cargo semver-checks` clean; published 0.6.3.
