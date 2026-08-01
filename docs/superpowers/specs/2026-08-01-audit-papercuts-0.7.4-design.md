# Audit papercuts (0.7.4) — inline-DTO reject probe + scaffold fmt fixpoints (#217 + #218)

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation (AUDIT round 1 findings)
**Issues:**
- **#217 (testgen coverage gap).** A custom action with an INLINE request body (`request_body: {fields: [...]}`, issue #122) gets a happy-path test (`inline_fixture_json`, `testgen.rs:254`) but NO 422 reject probe: every reject-probe helper keys on `rb.entity` (`constraint_reject_literal` / `first_enum_field` are only reached on the entity-body path). So a `min`/`max`/`min_len`/`max_len` (#80) or enum (`values`, #47) constraint DECLARED on an inline field — and enforced by the generated `{Op}Request` validator + advertised in OpenAPI — is UNVERIFIED by `check`: the boundary could stop answering 422 and the suite stays green. The entity-body path proves 422; the inline-DTO path must too.
- **#218 (scaffold is not a `cargo fmt` fixpoint).** A fresh scaffold of a jobs+realtime design (`conformance/designs/reference-slice.design.json`) is not `cargo fmt --check` clean — 24 drift hunks. The `#128`/`#165`/`#201` discipline (emitters produce output byte-identical to the pinned rustfmt so scaffolding needs no rustfmt) was never extended to these emitters. Confirmed sites: `jobsgen` task stub (`task_rs` unimpl `Err(...)`), jobs registry closure (`crates/jobs/src/lib.rs` `Arc::new(|ctx, _payload| -> JobFuture {...}`), jobs acceptance (`crates/jobs/tests/acceptance.rs` `Db::connect(...).await.expect(...)` chains), realtime acceptance (`crates/realtime/tests/acceptance.rs`), genroute route `lib.rs` + `model.rs` enum validator (`crates/routes/*/src/{lib,model}.rs`), and `crates/shared/src/lib.rs`. All exceed rustfmt's `max_width` (100) and rustfmt would wrap them.
**Ships as:** 0.7.4 — a testgen ADDITION (#217, inline reject probe) + codegen fmt-fixpoint corrections (#218, no behavior change — the emitted code is byte-for-byte what rustfmt would produce, so it still compiles and runs identically; only whitespace/wrapping changes). No public API change → MINOR/patch (bump 0.7.3 → 0.7.4).

## Part A (#217) — inline-DTO custom action gets a 422 reject probe

The entity-body reject machinery already exists and is constraint-shape-complete; reuse it for inline fields (an inline field is a `Field` with the same `#80` constraints + enum `values`):
1. At the site that emits the inline happy-path test (the caller of `inline_fixture_json`, `testgen.rs:254`), ALSO emit a reject probe when an inline field is rejectable:
   - **Constraint reject (#80):** the FIRST inline field for which `constraint_reject_literal(f)` (`testgen.rs:516`) returns `Some(lit)` → send a body identical to the happy-path body but with that field replaced by `lit` (an out-of-range value / `"a".repeat(max_len+1)` expression), assert the response is **422**. Mirror the entity path's "corrupt exactly one field so the ONLY reason for a 422 is that field" discipline (`testgen.rs:233`).
   - **Enum reject (#47):** if an inline field has `values: Some` AND `default: None` (mirror `first_enum_field`'s "present on the wire" gate — a defaulted enum field is omitted from the DTO so a bad value is ignored, not rejected), send `ENUM_REJECT_SENTINEL` in it, assert **422**.
2. **Count it in `reject`** (`TestOut.reject`, `testgen.rs:467`): like the entity enum/constraint rejects, an inline reject 422s at deserialization BEFORE the handler runs, so it PASSES on a stub and must be subtracted from the RED-on-stubs `expected_failing` baseline (else the honest-red count is wrong — the #47/#156 discipline).
3. **Auth:** if the inline-DTO endpoint is guarded, thread the test credential on the reject probe exactly as the happy-path inline test does (same `test_cookie()` / `post_json_with` shape) — the probe must reach the validator, not 401 first.
4. **Skip when nothing is rejectable:** an inline body whose fields declare NO `#80` constraint and NO non-defaulted enum emits NO reject probe (there is nothing the boundary would 422 — a probe would assert a 422 the validator never produces, the #80 T1-review-b discipline). Byte-identical output for such designs.

### #217 invariant (verify + test)
For an inline-DTO custom action with a constrained/enum inline field, `gen-tests` emits a probe that sends an out-of-range value in that field and asserts 422 — so a regression that drops the inline validator turns the suite RED. An inline body with no rejectable field is byte-identical to today. The entity-body path is unchanged.

## Part B (#218) — the scaffold is a `cargo fmt` fixpoint

Extend the `#128`/`#165`/`#201` manual-pre-wrap discipline to every drifting emitter so a fresh scaffold is `cargo fmt --check` clean. The ORACLE is the pinned toolchain's rustfmt (1.97 / rustfmt 1.9.0, edition 2024, `max_width` 100): scaffold `reference-slice.design.json`, run `cargo fmt`, and make each emitter produce EXACTLY rustfmt's output. Sites (each an emitter in `crates/jerrycan/src/platform/`):
- **`jobsgen.rs` `task_rs`** — the `unimpl` `Err(jerrycan::Error::internal("<name> not implemented — replace this stub"))` line exceeds 100 cols for realistic names → rustfmt breaks the `Err(` call arg onto its own indented line. Pre-wrap it (mirror the genroute stub-body wrap, `genroute.rs:514`).
- **`jobsgen.rs` jobs registry (`lib.rs`/`registry`)** — the `std::sync::Arc::new(|ctx: jerrycan::TaskContext, _payload: serde_json::Value| -> jerrycan::jobs::JobFuture<'static, ()> { ... })` closure exceeds 100 → rustfmt opens the `Arc::new(` call and breaks the closure params one-per-line. Pre-wrap to match.
- **`jobsgen.rs` jobs acceptance (`tests/acceptance.rs`)** — `Db::connect("sqlite::memory:").await.expect("test db")` and `db.migrate(...).await.expect(...)` method chains exceed 100 → rustfmt breaks each `.await`/`.expect(...)` onto its own line. Pre-wrap.
- **`realtimegen.rs` realtime acceptance (`tests/acceptance.rs:42`)** — same chain/width class. Pre-wrap the drifting line(s).
- **`genroute.rs` route `lib.rs` + `model.rs` enum validator** — the `crates/routes/*/src/lib.rs:*` and `model.rs:*` (enum validator) lines that exceed 100. Use `wrap_signature` (`genroute.rs:349`) where the drift is a signature; otherwise pre-wrap the specific expression exactly as rustfmt does.
- **`shared/src/lib.rs:94`** — the one drifting line in the shared crate emitter. Pre-wrap.

**Method (mandatory):** for EACH site, the target output is literally what `cargo fmt` produces on the scaffold — diff the emitter output against rustfmt and make them equal. Do not guess the wrap; use rustfmt as the oracle. Every prefix is ASCII (byte == char width), consistent with `wrap_signature`.

### #218 guard test (add — so drift can't silently return)
Add a conformance test (`crates/jerrycan/tests/conformance.rs`, mirroring the existing `rustfmt` fixpoint test at `conformance.rs:214` / the `Command::new("rustfmt")` pattern) that scaffolds the `reference-slice` (jobs+realtime) design to a tempdir and asserts EVERY generated `.rs` is a `rustfmt` fixpoint (running `rustfmt --check` / `cargo fmt --check` yields no diff). This is the make-impossible half: a future emitter that drifts turns this test RED. Gate it so it runs in the per-PR `gate` (it needs only `rustfmt`, already present in CI) — it must not be `#[ignore]`d, so the fast gate catches the regression the audit found.

## Tests
- **#217 testgen unit:** a design with an inline-DTO custom action carrying a `min/max` (or `max_len`, or enum) inline field → the generated module test contains a reject probe sending the out-of-range value and asserting 422, and `reject` count incremented; an inline-DTO action with NO constrained field → byte-identical (no probe). An entity-body endpoint → unchanged.
- **#217 e2e (conformance/reference_eval):** if the reference-slice / a conformance design has an inline-DTO constrained action, its suite gains the reject probe and stays honest-green (the reject subtracts from expected_failing). If none exists, add a minimal inline-DTO constrained action to a genroute_compile/conformance fixture to exercise the path end to end.
- **#218 guard:** the new scaffold-fmt-fixpoint conformance test is GREEN after the fix, and would be RED against the pre-fix emitters (verify by running it before/after).
- **#218 regression:** `conformance` / `reference_eval` / `eval` / `genroute_compile` byte-output snapshots update to the rustfmt-clean form (expected — the generated bytes change); no compile/behavior change.

## Gates
- `cargo test -p jerrycan` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (the #218 byte-output changes ripple into these snapshots — regenerate/verify; #217 adds a probe to any inline-DTO fixture).
- `cargo fmt`/`clippy -D warnings`; `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps --all-features` (CI-only trap); `cargo semver-checks` (no public API change); determinism + embedded_sync.
- **The new #218 guard test is GREEN** (a fresh reference-slice scaffold is a `cargo fmt` fixpoint).

## Version + success criteria
0.7.4. A fresh jobs+realtime scaffold is `cargo fmt --check` clean (24 drift hunks → 0) and a guard test locks it; an inline-DTO custom action with a constrained/enum field gets a 422 reject probe (unverified-constraint gap closed). Entity-body suites + unconstrained-inline designs byte-identical; heavy gate + determinism + cargo-doc green; published 0.7.4; #217 + #218 closed. **This empties the board again.**

## Non-goals
- Running rustfmt on generated code at scaffold time (the deliberate no-rustfmt-at-scaffold design stays — emitters remain manual fixpoints). Changing the inline-DTO happy path or the entity reject machinery. Any validator/OpenAPI behavior change (the constraints already exist — #217 only ADDS the test that proves them).
