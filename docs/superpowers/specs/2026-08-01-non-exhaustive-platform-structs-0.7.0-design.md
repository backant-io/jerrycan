# Mark platform config structs `#[non_exhaustive]` + re-enable the semver lint (0.7.0) — #145

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation
**Issues:** #145 (0.6.1's #105 added `public_read` to `platform::Entity` — additive for design-json authors, but `platform` is `pub`, so cargo-semver-checks flagged `constructible_struct_adds_field`, which for a 0.x crate forces a spurious major bump. 0.6.1 scope-allowed that lint crate-wide in `crates/jerrycan/Cargo.toml` (+ `jerrycan-realtime/Cargo.toml`). The DEBT: the crate-wide allow now also HIDES a genuinely-breaking field-add to ANOTHER `platform`/realtime constructible struct — the gate's semver coverage of those crates is narrowed until this is undone.)
**Ships as:** 0.7.0 — the FIRST release of the 0.7 major line. Marking the serde-deserialized config structs `#[non_exhaustive]` is itself a ONE-TIME 0.x-MAJOR break (downstream crates can no longer construct them with a struct literal — they must go through `serde` / the crate's constructors), so it lands in a release that is already major. This unblocks the rest of the 0.7 line: once the structs are `#[non_exhaustive]`, adding a field (as #150's `auth.identity` and #104's realtime fields will) is a clean non-breaking MINOR.

## What `#[non_exhaustive]` does (and does not) break
- It blocks DOWNSTREAM crates from: (a) constructing the struct with a literal `Foo { .. }`, and (b) exhaustive `match`/destructuring without a `..` arm. Downstream code that DESERIALIZES (serde) or reads fields is unaffected.
- The DEFINING crate (jerrycan / jerrycan-realtime) can STILL construct its own `#[non_exhaustive]` structs with literals — so every in-crate literal (migrate `authmap.rs`/`entities.rs`, test fixtures, defaults) keeps compiling. Verify: `cargo build`/`cargo test` stay green with zero changes to construction sites.

## The change
1. **`crates/jerrycan/src/platform/design.rs`** — add `#[non_exhaustive]` to every PUBLIC serde-deserialized config struct (the design contract surface). From the 19 `pub struct`s, the config structs are: `Design`, `CorsDesign`, `RateLimitDesign`, `RealtimeDesign`, `RealtimeTopic`, `Auth`, `ModuleDesign`, `Entity`, `Field`, `BelongsTo`, `Tenancy`, `JobDesign`, `StorageDesign`, `BucketDesign`, `Endpoint`, `RequestBody`, `Success`, `ErrorCase`. Do NOT mark structs that are NOT part of the external contract if marking them breaks an intended downstream literal-construction API — but `platform` is tool-internal (shared with the `jerrycan` bin + MCP, not a downstream literal-construction API), so mark all the config structs. `HandlerRef` and any purely-internal (non-serde, `pub(crate)`) struct: mark only if it is `pub` AND trips the lint; leave `pub(crate)` structs alone (the lint only sees `pub`).
   - **DO NOT** add `#[non_exhaustive]` to enums here unless one trips the lint — the issue is about *constructible structs*. Keep scope to structs.
2. **`crates/jerrycan-realtime/src/…`** — the realtime crate has its OWN `constructible_struct_adds_field = "allow"` (`Cargo.toml:56`). Find which of ITS public structs trip the lint (run `cargo semver-checks` with the allow removed — see step 3) and mark those `#[non_exhaustive]` too. (Likely the runtime config/spec structs, e.g. anything a downstream would construct — verify empirically; do not guess.)
3. **Remove the allow from BOTH manifests:** delete `constructible_struct_adds_field = "allow"` (and the now-stale explanatory comment) from `crates/jerrycan/Cargo.toml` (~100-106) and `crates/jerrycan-realtime/Cargo.toml` (~50-56). If removing the `[package.metadata.cargo-semver-checks.lints]` table leaves it empty, remove the table.

## The verify-loop (this IS the method)
Iterate until clean: mark the structs → remove the allow → run `cargo semver-checks` for the crate. Because 0.7.0 is a MAJOR bump, the baseline is 0.6.35; the tool WILL report the `#[non_exhaustive]`-additions as breaking (expected for a major) — that is FINE for a major release. The GOAL is: with the allow removed, semver-checks reports NO UNEXPECTED breaking change beyond the intended `#[non_exhaustive]` markers, and it now WOULD catch a future field-add to a still-exhaustive struct. Run `cargo semver-checks check-release -p jerrycan` and `-p jerrycan-realtime`; confirm the only breaking findings are the deliberate `#[non_exhaustive]` markings, and that no `constructible_struct_adds_field` allow remains.

## Tests
- `cargo build`/`cargo test -p jerrycan` + `-p jerrycan-realtime` green with ZERO changes to in-crate construction sites (proves `#[non_exhaustive]` doesn't break own-crate literals).
- A comment/doc note on each marked struct is NOT required, but keep the design.rs struct docs intact.
- No new unit test is strictly needed (this is a semver-surface change); the semver gate IS the test. Optionally add a doc-comment noting these are `#[non_exhaustive]` (extend via the design contract, not literals).

## Gates
- `cargo test -p jerrycan` + `-p jerrycan-realtime` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (generated code is unchanged — `#[non_exhaustive]` is a source annotation with no effect on generation; scaffolded apps consume the design via serde, unaffected). Byte-identical scaffolding (`determinism.rs` green).
- `cargo fmt`/`clippy -D warnings`.
- `cargo semver-checks` for jerrycan + jerrycan-realtime: the allow is GONE and the only breaking findings are the intended `#[non_exhaustive]` markers (a major release accepts them).

## Version
This is the **0.7.0** major bump (0.6.35 → 0.7.0). Bump all 11 workspace crates to 0.7.0; `cargo update -p jerrycan --precise 0.7.0 --offline`. (The publish.sh semver step tolerates a major bump — a 0.7.0 tag leads the 0.6 baseline.)

## Success criteria
- Every public serde-config struct in `platform` (and the flagged jerrycan-realtime structs) is `#[non_exhaustive]`; the `constructible_struct_adds_field = "allow"` is removed from both manifests; own-crate construction unchanged; heavy gate + determinism green; a future field-add to any of these structs is now a clean non-breaking minor (and a genuinely-breaking change is caught again). Published 0.7.0; #145 closed.

## Non-goals
- Adding any new field or contract surface (that is #150/#104). Changing serialization. Marking `pub(crate)`/private structs (the lint only sees `pub`). Enums (unless one trips the lint).
