# Inline-DTO completeness: entity-less compile, reject-probe parity, schema (0.7.6) — #224 + #225 + #226

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation (AUDIT round 2 findings, inline-DTO / #122 family)
**Issues:** three gaps in the #122 inline-DTO (`request_body: {fields:[…]}`) surface, found by the Round-2 audit:
- **#224 (HIGH — won't compile).** An entity-less module whose only endpoint uses an inline-DTO body does not compile from a clean `jerrycan new`: `genroute.rs:3833` (`mod_decls`) gates `mod model;` on `!m.entities.is_empty()`, but `model.rs` IS emitted for such a module (`genroute.rs:975`/`:1292`) and `handlers.rs` does `use super::model::*;` → `E0432 unresolved import super::model`. `check`/`cargo build` can never be green.
- **#225 (MEDIUM — green-means-safe).** The #217 inline reject probe (`push_inline_reject_test`, `testgen.rs:1212-1236`) emits at most ONE reject and only for REQUIRED fields — strictly weaker than the entity probe. Gap A (XOR): `if let <constraint> … else if let <enum>` skips the enum reject when a constrained field exists. Gap B: the `f.required` gate + `inline_fixture_json` omitting optional fields means an optional constrained/enum inline field gets no probe. The generated validator exists (`genroute.rs:1603-1637`) but is untested → a regression stays green.
- **#226 (MINOR — contract drift).** `docs/contracts/design-schema.json` (`request_body` `additionalProperties:false`, only `entity`) and `docs/ai/00-designing.md` ("request_body is entity-only") are stale vs the validator, which accepts `fields`.
**Ships as:** 0.7.6 — a genroute emission fix (#224), a testgen probe fix (#225), and a docs/schema correction (#226). No public API change → patch bump 0.7.5 → 0.7.6. All generated-byte changes are additive to the inline-DTO shape; entity-body + non-inline designs must be byte-identical.

## Part A (#224) — declare `mod model;` whenever `model.rs` is emitted
In `crates/jerrycan/src/platform/genroute.rs` `mod_decls` (~line 3833): the `mod model;` declaration currently gates on `!m.entities.is_empty()`. Change the gate so `mod model;` is emitted when the module has entities OR any endpoint carries an inline `request_body.fields` (i.e. the SAME condition under which `model.rs`/`model_rs`/`model_rs_db` is emitted — find that exact predicate and reuse it so the declaration and the file can never disagree). Keep `mod repo;` entity-gated (an entity-less inline-DTO module has no repo). Verify: an entity-less inline-DTO module now emits `mod model;` and compiles.

**Fixture (mandatory — CI missed this):** add an ENTITY-LESS inline-DTO module design to `crates/jerrycan/tests/genroute_compile.rs` (the existing `inline-db` fixture at ~line 1614 co-locates an `Order` entity, which masks the bug). The new fixture must have a module with NO entities and one endpoint using `request_body:{fields:[…]}`, and it must COMPILE under the strict-clippy compile gate. This is the make-impossible half.

## Part B (#225) — inline reject probe reaches parity with the entity probe
In `crates/jerrycan/src/platform/testgen.rs`, rework `push_inline_reject_test` (and its call site) to mirror the entity-body path (`testgen.rs:787-806`/`853-878`, which uses `first_enum_field` + `first_constraint_reject`, both gating only on `default.is_none()`):
1. **Gap A — emit BOTH independently:** stop the `if constraint … else if enum` XOR. Emit a CONSTRAINT reject (first inline field with `constraint_reject_literal` Some) AND, separately, an ENUM reject (first inline field with `values: Some` && `default: None`) — exactly as the entity path emits both. If a single field is both, follow the entity path's behavior (it picks per-category independently). Count each in `TestOut.reject`.
2. **Gap B — drop the `f.required` gate:** probe optional-but-present constrained/enum fields too. This requires the corrupted body to INCLUDE the optional field: extend `inline_fixture_json` (or the reject-body builder) so the reject probe's body carries the corrupted optional field at a valid-then-overridden value — mirror how `fixture_json` includes optional non-defaulted fields for the entity path. (The happy-path inline body stays minimal/required-only — byte-identical; only the REJECT body gains the optional field it corrupts.)
3. Preserve the #217 invariants the Round-1 review confirmed: the corrupted field is present on the wire; vacuous bounds are excluded (via `constraint_reject_literal`/`first_enum_field`'s existing gates); the reject 422s before the handler so it PASSES on a stub and subtracts from `expected_failing`; guarded endpoints thread the credential.

**Tests:** an inline-DTO action with BOTH a constrained field and an enum field → the generated suite has a constraint reject AND an enum reject (Gap A); an inline-DTO action with an OPTIONAL constrained/enum field → a reject probe exists for it (Gap B); an inline action with a single required constrained field → unchanged (the #217 case); entity-body path byte-identical. Model on the Round-2 finding's repro designs (`scratchpad/inline-both.design.json`, `inline-opt.design.json`).

## Part C (#226) — schema + docs allow the inline-DTO shape
- `docs/contracts/design-schema.json` (+ embedded twin if the codebase carries one): change the `request_body` schema to allow `fields` — express `entity` XOR `fields` (mirroring the validator's JC0561), and relax `additionalProperties` so `fields` is accepted. Do NOT loosen it into accepting both at once (keep the XOR the validator enforces).
- `docs/ai/00-designing.md` (+ embedded twin): correct the "request_body is entity-only" prose to document the inline-DTO shape (`request_body: {fields:[…]}`) alongside the entity shape, matching #122.
- If a determinism/embedded-sync twin test guards these docs, update the twin so it stays a fixpoint.

## Gates
- `cargo test -p jerrycan` green; the new entity-less inline-DTO compile fixture green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (local PG available). The #224 fix changes `lib.rs` for inline-DTO modules only; #225 adds reject tests to inline-DTO suites; entity-body + non-inline designs byte-identical.
- `cargo fmt`/`clippy -D warnings`; `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps --all-features` (CI-only trap); `cargo semver-checks` (no public API change); determinism + embedded_sync (the #226 docs twins).

## Version + success criteria
0.7.6. An entity-less inline-DTO module compiles from a clean scaffold (#224, locked by a compile fixture); an inline-DTO action's declared constraint AND enum on required OR optional fields each get a 422 reject probe (#225); the published schema + docs accept the inline-DTO shape (#226). Entity-body + non-inline designs byte-identical; heavy gate + determinism + cargo-doc green; published 0.7.6; #224 + #225 + #226 closed.

## Non-goals
- The pre-existing symmetric gap that a multi-param-path (`param_count>=2`) custom action gets no reject probe at all (inline OR entity) — a broader coverage limitation, separate issue if pursued. Any validator/OpenAPI behavior change (the validators already exist — #225 only ADDS tests). The realtime findings (#227/#228/#229 → 0.7.7).
