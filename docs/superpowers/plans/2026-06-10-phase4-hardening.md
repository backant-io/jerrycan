# jerrycan Phase 4 — Hardening → v0.1.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden the parsing surfaces (fuzzing), prove the platform's purpose (an agent driving the MCP builds working apps — the ≥90% eval metric), polish diagnostics, add benchmarks, and prepare the first real **v0.1.0** release of the reserved crates.io names.

**Architecture:** Five independent workstreams, each shippable on its own. Fuzzing adds a nightly-only `fuzz/` crate (outside the stable workspace) PLUS a deterministic stable "fuzz smoke" test that throws thousands of adversarial inputs at the four jerrycan-owned parsers and asserts no panic — the CI-gating signal. Diagnostics gain a single code registry (every `JC####`/`JL####` in one place) behind a `jerrycan explain <code>` command, with a completeness test. Benchmarks are criterion targets on the hot paths. The agent eval ships reference-app specs + a deterministic scripted runner (CI signal) AND a real agent-driven eval (a dispatched agent uses ONLY the docs + MCP/CLI to build the apps — no model API in jerrycan's code, per the cost constraint); the recorded success rate is the release metric. Release prep bumps `0.0.0 → 0.1.0` everywhere — including the generated-app template default — fills crate metadata, dry-runs every publish, and emits an ordered publish script the maintainer runs.

**Tech Stack:** `libfuzzer-sys`/`arbitrary` (fuzz crate only, nightly, excluded from the workspace), `criterion` (dev-dep, bench targets). No new runtime deps. No external model API. The actual `cargo publish` is performed by the maintainer with their authenticated account.

**Pinned design decisions (the architect's calls — do not relitigate):**
1. **Two-layer fuzzing.** The real `cargo-fuzz` targets live in a `fuzz/` crate excluded from `[workspace]` (nightly + libfuzzer; that crate does NOT carry `forbid(unsafe_code)` — the harness macro needs it, but jerrycan's own crates stay clean). The CI gate is a STABLE randomized smoke test (`tests/fuzz_smoke.rs` in jerrycan-core/auth) with a fixed PRNG seed, thousands of adversarial byte strings, asserting the parsers never panic. Deep fuzzing is a documented local/scheduled command; CI does not run libfuzzer.
2. **Parsers under fuzz:** router `decode_segment` (percent-decoding), `Trie::find` path matching, session-cookie `decode`, JWT `decode`, and `Design`/JSON parsing. The invariant for all: arbitrary bytes → `Ok`/`Err`/clean result, NEVER a panic, hang, or OOM.
3. **Agent eval = the querying agent's job, not ours.** jerrycan ships reference specs + a scorer + a deterministic scripted runner; the "real LLM" half is a dispatched agent that reads `jerrycan docs`, drives `jerrycan` MCP/CLI, and writes handlers from scratch — no Anthropic/OpenAI calls in jerrycan's code or tests. `conformance/eval/results.md` records the latest real run; ≥90% pass is the exit metric.
4. **Diagnostics registry is the single source of truth.** One `codes.rs` table maps every `JC####`/`JL####` → title, cause, fix, doc anchor. `jerrycan explain <code>` reads it. A completeness test greps the source for emitted codes and fails if any is missing from the registry (no orphan codes).
5. **v0.1.0 bumps the template default too.** `templates::jerrycan_dep_spec_from`'s default becomes `version = "0.1.0"`, so newly generated apps depend on the PUBLISHED framework — this closes both the Phase-0 version-literal backlog item AND the Phase-3 in-container-Dockerfile-build gap (the emitted Dockerfile builds once 0.1.0 is on crates.io). Conformance keeps overriding via `JERRYCAN_FRAMEWORK_DEP` (path) so pre-publish gates still pass.
6. **The maintainer publishes.** This plan produces version bumps, metadata, CHANGELOG, green `cargo publish --dry-run` for all 7 crates, and `scripts/publish.sh` (correct dependency order, rate-limit-tolerant). It does NOT run `cargo publish`. The crates are published by the maintainer's authenticated, email-verified account.
7. **Benchmarks are informational, not gated.** Criterion benches build in CI (`cargo build --benches`) but are not run as a pass/fail gate (timing is machine-dependent). Baseline numbers recorded in `docs/benchmarks.md` from a local run.

---

## File Structure

```
Cargo.toml                                  # MODIFY: criterion dev-dep; (fuzz/ is its OWN workspace, excluded)
fuzz/                                       # CREATE: nightly cargo-fuzz crate (excluded from [workspace])
├── Cargo.toml
├── fuzz_targets/{decode_segment,route_find,session_decode,jwt_decode,design_parse}.rs
└── corpus/…                                # seed inputs
crates/jerrycan-core/
├── tests/fuzz_smoke.rs                      # CREATE: stable randomized no-panic smoke (router/decode/design)
├── benches/core_bench.rs                    # CREATE: router match + dispatch criterion benches
└── Cargo.toml                              # MODIFY: [dev-dependencies] criterion; [[bench]]
crates/jerrycan-auth/
├── tests/fuzz_smoke.rs                      # CREATE: stable smoke for session/jwt decode
├── benches/auth_bench.rs                    # CREATE: session encode/decode + jwt criterion benches
└── Cargo.toml                              # MODIFY: criterion dev-dep + [[bench]]
crates/jerrycan/src/platform/
├── codes.rs                                # CREATE: code registry (JC/JL → title/cause/fix/doc)
├── docsidx.rs                              # MODIFY: (codes feed explain; no change unless needed)
crates/jerrycan/src/main.rs                  # MODIFY: `explain <code>` command
crates/jerrycan/tests/explain.rs             # CREATE: explain output + registry completeness
conformance/eval/
├── specs/{blog,tasks,shortener,inventory,notes}.design.json  # CREATE: 5 reference designs
├── fixtures/…                              # CREATE: reference handler impls per spec (the scorer's known-good)
└── results.md                              # CREATE: recorded real-agent eval run + score
crates/jerrycan/tests/eval.rs                # CREATE: deterministic scripted eval runner (#[ignore] heavy)
docs/benchmarks.md                           # CREATE: baseline numbers
docs/ai/13-error-codes.md                    # CREATE: doc-tested? (prose: the code table) — UNGATED page
scripts/publish.sh                           # CREATE: ordered, rate-limit-tolerant publish script (maintainer runs)
CHANGELOG.md                                 # CREATE
README.md                                    # MODIFY: roadmap → complete, install, eval badge, v0.1.0
docs/phase1-backlog.md                       # MODIFY: clear resolved items
.github/workflows/ci.yml                     # MODIFY: build benches + fuzz crate (nightly, non-blocking); eval heavy
[all 7 crate Cargo.toml + workspace]         # MODIFY (release task): 0.0.0 → 0.1.0 + metadata
```

**Conventions (as Phase 2/3):** gates are `cargo fmt --all` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace --all-features` before EVERY commit. The `fuzz/` crate is OUTSIDE the workspace so these gates ignore it. Plain commit messages; `#![forbid(unsafe_code)]` in every jerrycan crate (fuzz crate exempt); heavy tests `#[ignore]`; plan-code compile failures fixed minimally + recorded; design-level walls → BLOCKED.

---
### Task 1: Stable fuzz-smoke tests (the CI no-panic gate)

**Files:**
- Create: `crates/jerrycan-core/tests/fuzz_smoke.rs`, `crates/jerrycan-auth/tests/fuzz_smoke.rs`

A deterministic PRNG generates thousands of adversarial inputs; the only assertion is "no panic / returns". This is the always-green CI signal; deep libfuzzer fuzzing (Task 2) is the nightly/local layer.

- [ ] **Step 1: Write the core smoke test**

`crates/jerrycan-core/tests/fuzz_smoke.rs`:

```rust
//! Deterministic randomized smoke: jerrycan-owned parsers must NEVER panic on
//! adversarial input. Fixed seed → reproducible. Deep fuzzing lives in fuzz/.

use jerrycan_core::http::Method;

/// xorshift64* — tiny deterministic PRNG (no rand dep in core tests).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn byte(&mut self) -> u8 {
        (self.next() & 0xff) as u8
    }
    /// A messy path-ish string: %-escapes, slashes, unicode, control bytes.
    fn messy_path(&mut self) -> String {
        let alphabet = b"/abc{}%0123456789ZZ%2%C3%A9%%/../\xff \t";
        let len = (self.next() % 40) as usize;
        let mut s = String::from("/");
        for _ in 0..len {
            let c = alphabet[(self.next() as usize) % alphabet.len()];
            s.push(c as char);
        }
        s
    }
}

#[test]
fn router_matching_never_panics_on_adversarial_paths() {
    use jerrycan_core::{get, App};
    // A built app with a mix of static + param + nested routes.
    let app = App::new()
        .route("/", get(|| async { "root" }))
        .route("/items/{id}", get(|| async { "item" }))
        .route("/a/b/c", get(|| async { "abc" }))
        .route("/a/{x}/d", get(|| async { "axd" }))
        .into_test();
    // Drive 20k adversarial GETs through real dispatch (router + decode).
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        for _ in 0..20_000 {
            let path = {
                let mut r = Rng(rng.next());
                r.messy_path()
            };
            // Any status is fine; the contract is "does not panic / hang".
            let _ = app.get(&path).await;
        }
    });
    let _ = Method::GET; // import anchor
}
```

NOTE: the core smoke covers only router/decode (core-owned). `Design` lives in the `jerrycan` crate (platform), so its parse-smoke is a SEPARATE file written in Step 3 (`crates/jerrycan/tests/fuzz_smoke.rs`) — do NOT import `jerrycan::platform` from a core test (core does not depend on the facade).

- [ ] **Step 2: Write the auth smoke test**

`crates/jerrycan-auth/tests/fuzz_smoke.rs`:

```rust
//! Session/JWT decoders must never panic on attacker-controlled bytes.

use jerrycan_auth::{jwt, SessionStore};
use serde::Deserialize;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12; x ^= x << 25; x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    /// A token-ish string: base64 alphabet, dots, padding, junk.
    fn token(&mut self) -> String {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.=%\xff";
        let len = (self.next() % 120) as usize;
        let mut s = String::new();
        for _ in 0..len {
            s.push(alphabet[(self.next() as usize) % alphabet.len()] as char);
        }
        s
    }
}

#[derive(Deserialize)]
struct AnyClaims {
    #[allow(dead_code)]
    sub: Option<String>,
}

#[test]
fn session_and_jwt_decode_never_panic() {
    let key = [7u8; 32];
    let store = SessionStore::new(&key);
    let mut rng = Rng(0xDEADBEEFCAFEF00D);
    for _ in 0..50_000 {
        let token = rng.token();
        let _ = store.decode::<AnyClaims>(&token); // must Err, never panic
        let _ = jwt::decode::<AnyClaims>(&token, &key);
    }
    // Also fuzz the cookie-header parser path with junk cookie strings.
    for _ in 0..10_000 {
        let header = rng.token();
        let _ = store.read_cookie(&header);
    }
}
```

NOTE: `read_cookie` is `pub(crate)` — for this sibling-crate test it must be reachable. Either make `read_cookie` `pub` (it's a harmless parser) or drop that loop. Prefer making it `pub` and record it.

- [ ] **Step 3: Create the jerrycan-crate design-parse smoke**

`crates/jerrycan/tests/fuzz_smoke.rs`:

```rust
//! design.json parsing must never panic on garbage (it's agent/file input).

use jerrycan::platform::design::Design;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0; x ^= x >> 12; x ^= x << 25; x ^= x >> 27; self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

#[test]
fn design_parse_never_panics_on_corrupted_golden() {
    let bytes = GOLDEN.as_bytes();
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for _ in 0..20_000 {
        let mut corrupted = bytes.to_vec();
        // Flip / truncate / inject random bytes.
        let ops = (rng.next() % 8) as usize;
        for _ in 0..ops {
            if corrupted.is_empty() { break; }
            let i = (rng.next() as usize) % corrupted.len();
            corrupted[i] = (rng.next() & 0xff) as u8;
        }
        if rng.next() % 2 == 0 && !corrupted.is_empty() {
            corrupted.truncate((rng.next() as usize) % corrupted.len());
        }
        // serde_json::from_slice into Design must Err, never panic.
        let _ = serde_json::from_slice::<Design>(&corrupted);
    }
    let _ = Design::from_path; // anchor
}
```

- [ ] **Step 4: Run to verify** — `cargo test -p jerrycan-core --test fuzz_smoke && cargo test -p jerrycan-auth --test fuzz_smoke && cargo test -p jerrycan --test fuzz_smoke`. All pass (each runs 20k–60k cases in well under a second; if any panics, that's a real parser bug — FIX the parser, never the test). Full `--all-features` gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/tests/fuzz_smoke.rs crates/jerrycan-auth/tests/fuzz_smoke.rs crates/jerrycan/tests/fuzz_smoke.rs crates/jerrycan-auth/src/session.rs
git commit -m "Add deterministic fuzz-smoke tests asserting parsers never panic"
```

---

### Task 2: cargo-fuzz crate (deep fuzzing, nightly, out of workspace)

**Files:**
- Create: `fuzz/Cargo.toml`, `fuzz/fuzz_targets/*.rs`, `fuzz/corpus/*` seeds
- Modify: root `Cargo.toml` (exclude `fuzz` from members if globbed — verify it's not pulled in)

- [ ] **Step 1: Confirm fuzz/ is outside the workspace**

The workspace `[workspace] members = [...]` lists crates explicitly (not a glob), so `fuzz/` is naturally excluded. Add `exclude = ["fuzz"]` to `[workspace]` defensively. Run `cargo check --workspace` — fuzz must NOT appear.

- [ ] **Step 2: Write `fuzz/Cargo.toml`**

```toml
[package]
name = "jerrycan-fuzz"
version = "0.0.0"
edition = "2021"
publish = false

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
jerrycan-core = { path = "../crates/jerrycan-core" }
jerrycan-auth = { path = "../crates/jerrycan-auth" }
jerrycan = { path = "../crates/jerrycan", default-features = false }
serde_json = "1"
serde = { version = "1", features = ["derive"] }

[[bin]]
name = "decode_segment"
path = "fuzz_targets/decode_segment.rs"
test = false
doc = false

[[bin]]
name = "session_decode"
path = "fuzz_targets/session_decode.rs"
test = false
doc = false

[[bin]]
name = "jwt_decode"
path = "fuzz_targets/jwt_decode.rs"
test = false
doc = false

[[bin]]
name = "design_parse"
path = "fuzz_targets/design_parse.rs"
test = false
doc = false
```

NOTE: `decode_segment`/`Trie` are `pub(crate)` in core's router. The fuzz target can't reach them directly. Two options: (a) fuzz the PUBLIC surface that exercises them — `App::into_test().get(path)` drives the router+decoder end to end; (b) add a `#[doc(hidden)] pub fn __fuzz_decode_segment` shim in core. Prefer (a): the `decode_segment` target fuzzes the decoder+router via a built `TestApp` (same surface as the smoke); keep the bin/file named `decode_segment` to match the Cargo.toml and README. Record the choice.

- [ ] **Step 3: Write the fuzz targets**

`fuzz/fuzz_targets/decode_segment.rs` (drives router+decoder via the public TestApp):

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(path) = std::str::from_utf8(data) {
        use jerrycan_core::{get, App};
        let app = App::new()
            .route("/items/{id}", get(|| async { "x" }))
            .route("/a/b/c", get(|| async { "y" }))
            .into_test();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async { let _ = app.get(path).await; });
    }
});
```

(NOTE: building a TestApp per input is slow for libfuzzer but correct; deep fuzzing is a soak activity. If perf matters, lazily build once with `std::sync::OnceLock`. Record if you optimize.)

`fuzz/fuzz_targets/session_decode.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Deserialize)]
struct C { #[allow(dead_code)] sub: Option<String> }

fuzz_target!(|data: &[u8]| {
    if let Ok(token) = std::str::from_utf8(data) {
        let store = jerrycan_auth::SessionStore::new(&[7u8; 32]);
        let _ = store.decode::<C>(token);
    }
});
```

`fuzz/fuzz_targets/jwt_decode.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Deserialize)]
struct C { #[allow(dead_code)] sub: Option<String> }

fuzz_target!(|data: &[u8]| {
    if let Ok(token) = std::str::from_utf8(data) {
        let _ = jerrycan_auth::jwt::decode::<C>(token, &[7u8; 32]);
    }
});
```

`fuzz/fuzz_targets/design_parse.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<jerrycan::platform::design::Design>(data);
});
```

- [ ] **Step 4: Seed corpus + README**

Create `fuzz/corpus/design_parse/golden` = a copy of the golden design; `fuzz/corpus/session_decode/sample` = a real token (any base64 string); etc. Add `fuzz/README.md`:

```markdown
# Deep fuzzing (nightly)

```
rustup toolchain install nightly
cargo install cargo-fuzz
cargo +nightly fuzz run decode_segment -- -max_total_time=120
cargo +nightly fuzz run session_decode -- -max_total_time=120
cargo +nightly fuzz run jwt_decode    -- -max_total_time=120
cargo +nightly fuzz run design_parse  -- -max_total_time=120
```

The stable `tests/fuzz_smoke.rs` suites run the same surfaces continuously in CI; this crate is for deeper soak runs. Any crash found here = a parser bug; reproduce, fix the parser, commit the crashing input to the corpus.
```

- [ ] **Step 5: Verify it builds (no nightly needed to type-check the targets via the harness? It needs nightly.)** If nightly is available: `cargo +nightly fuzz build` (compiles all targets). If not, at least `cd fuzz && cargo +nightly check` — record whether nightly was present. The stable workspace gate (`cargo check --workspace`) must still NOT include fuzz.

- [ ] **Step 6: Commit**

```bash
git add fuzz Cargo.toml
git commit -m "Add cargo-fuzz targets for the router, session, JWT, and design parsers"
```

---
### Task 3: Diagnostics — code registry + `jerrycan explain`

**Files:**
- Create: `crates/jerrycan/src/platform/codes.rs` (+ `pub mod codes;` in mod.rs)
- Modify: `crates/jerrycan/src/main.rs`
- Create: `crates/jerrycan/tests/explain.rs`

- [ ] **Step 1: Write the failing tests** (`crates/jerrycan/tests/explain.rs`)

```rust
//! `jerrycan explain <code>` + registry completeness.

use std::process::Command;

fn jerrycan() -> Command { Command::new(env!("CARGO_BIN_EXE_jerrycan")) }

#[test]
fn explain_prints_title_cause_fix_and_doc() {
    let out = jerrycan().args(["explain", "JC0404"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("JC0404"));
    assert!(text.to_lowercase().contains("not found"));
    assert!(text.contains("docs:") || text.contains("jerrycan docs"));
}

#[test]
fn explain_works_for_a_lint_code_and_is_case_insensitive() {
    let out = jerrycan().args(["explain", "jl0004"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("JL0004"));
}

#[test]
fn explain_unknown_code_is_usage_error() {
    let out = jerrycan().args(["explain", "JC9999"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn json_mode_explain_emits_structured_record() {
    let out = jerrycan().args(["--json", "explain", "JC0510"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["code"], "JC0510");
    assert!(v["title"].is_string() && v["fix"].is_string() && v["doc"].is_string());
}
```

Add a completeness test (in `codes.rs` as a unit test, since it greps source paths relative to the crate):

```rust
    #[test]
    fn every_emitted_code_is_in_the_registry() {
        // Grep the workspace source for JC####/JL#### string literals and assert
        // each is registered. This is the "no orphan codes" guard.
        use std::collections::BTreeSet;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap();
        let mut found = BTreeSet::new();
        fn walk(dir: &std::path::Path, found: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if matches!(name, "target" | ".git" | "fuzz" | "flask" | "werkzeug") { continue; }
                    walk(&p, found);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    if let Ok(s) = std::fs::read_to_string(&p) {
                        for cap in find_codes(&s) {
                            found.insert(cap);
                        }
                    }
                }
            }
        }
        walk(&root.join("crates"), &mut found);
        let registered: BTreeSet<String> = REGISTRY.iter().map(|c| c.code.to_string()).collect();
        let orphans: Vec<&String> = found.iter().filter(|c| !registered.contains(*c)).collect();
        assert!(orphans.is_empty(), "codes emitted in source but missing from the registry: {orphans:?}");
    }

    /// Extract `JC####` / `JL####` tokens from a source string.
    fn find_codes(s: &str) -> Vec<String> {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 6 <= bytes.len() {
            let w = &bytes[i..i + 6];
            let is_code = (w[0] == b'J') && (w[1] == b'C' || w[1] == b'L') && w[2..].iter().all(u8::is_ascii_digit);
            if is_code {
                // ensure not part of a longer alnum run
                let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                let after_ok = i + 6 == bytes.len() || !bytes[i + 6].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    out.push(String::from_utf8_lossy(w).to_string());
                }
            }
            i += 1;
        }
        out
    }
```

- [ ] **Step 2: Run to verify failure** — compile FAIL.

- [ ] **Step 3: Implement `codes.rs`**

```rust
//! The single registry of stable diagnostic codes. `jerrycan explain` reads it;
//! a completeness test fails if any code emitted in source is missing here.

/// One diagnostic code's human explanation.
pub struct CodeInfo {
    pub code: &'static str,
    pub title: &'static str,
    pub cause: &'static str,
    pub fix: &'static str,
    pub doc: &'static str,
}

/// Every JC#### (framework runtime) and JL#### (jerrycan generation lint) code.
pub const REGISTRY: &[CodeInfo] = &[
    CodeInfo { code: "JC0400", title: "bad request", cause: "a path parameter or query string failed to parse, or the path had a malformed percent-encoding", fix: "send well-formed input; check the route's parameter types", doc: "jerrycan docs errors" },
    CodeInfo { code: "JC0401", title: "authentication required", cause: "no valid session cookie or bearer token was presented", fix: "log in (Session) or send Authorization: Bearer <jwt>", doc: "jerrycan docs auth" },
    CodeInfo { code: "JC0403", title: "forbidden", cause: "authenticated, but require_role rejected the user's role", fix: "use an account with the required role", doc: "jerrycan docs auth" },
    CodeInfo { code: "JC0404", title: "not found", cause: "no route matched the path, or a handler returned Error::not_found()", fix: "check the path; confirm the resource exists", doc: "jerrycan docs app" },
    CodeInfo { code: "JC0405", title: "method not allowed", cause: "the path exists but not for this HTTP method", fix: "use a method the route defines", doc: "jerrycan docs app" },
    CodeInfo { code: "JC0408", title: "request timeout", cause: "the request body was not received within the read budget", fix: "send the body promptly; raise body_read_timeout if legitimate", doc: "jerrycan docs app" },
    CodeInfo { code: "JC0413", title: "payload too large", cause: "the request body exceeded the size limit (default 1 MiB)", fix: "send a smaller body; raise the limit explicitly if needed", doc: "jerrycan docs app" },
    CodeInfo { code: "JC0422", title: "unprocessable entity", cause: "the JSON body failed to parse, or Valid<T> found violations", fix: "fix the body to match the schema; read the details array", doc: "jerrycan docs validation" },
    CodeInfo { code: "JC0500", title: "internal error", cause: "an unexpected server-side failure (or a handler panicked)", fix: "check server logs; the cause is logged, never sent to the client", doc: "jerrycan docs errors" },
    CodeInfo { code: "JC0503", title: "handler timeout", cause: "the request exceeded the per-request handler budget (default 30s)", fix: "make the handler faster or raise handler_timeout", doc: "jerrycan docs app" },
    CodeInfo { code: "JC0510", title: "database error", cause: "a jerrycan-db query/connection failed", fix: "check JERRYCAN_DATABASE_URL and migrations; the sqlx detail is on stderr", doc: "jerrycan docs database" },
    CodeInfo { code: "JC1001", title: "missing dependency", cause: "a handler asked for a Dep<T> with no registered provider", fix: "provide(value) or provide_dep(factory) on the app or module", doc: "jerrycan docs dependencies" },
    CodeInfo { code: "JC1002", title: "dependency cycle", cause: "dependency factories recursed past the depth limit (cycle or absurd chain)", fix: "break the cycle in your provide_dep graph", doc: "jerrycan docs dependencies" },
    CodeInfo { code: "JL0001", title: "leaky route crate", cause: "a route crate's lib.rs exports more than module()", fix: "make it pub(crate), or move shared types to the shared crate", doc: "jerrycan docs modules" },
    CodeInfo { code: "JL0002", title: "missing handler", cause: "a design endpoint has no matching handler fn", fix: "add the handler with the operation_id name, or fix the design", doc: "jerrycan docs modules" },
    CodeInfo { code: "JL0003", title: "generated drift", cause: "a tool-owned generated file was hand-edited or the design changed without regenerating", fix: "re-run jerrycan generate; never hand-edit GENERATED files", doc: "jerrycan docs app" },
    CodeInfo { code: "JL0004", title: "unguarded mutation", cause: "an auth design has a mutating route with no auth guard", fix: "set auth_required: true or required_roles on the endpoint", doc: "jerrycan docs auth" },
];

/// Look up a code, case-insensitively.
pub fn lookup(code: &str) -> Option<&'static CodeInfo> {
    let upper = code.to_uppercase();
    REGISTRY.iter().find(|c| c.code == upper)
}
```

NOTE: the completeness test will also flag codes used ONLY as example text in docs/tests (e.g. a `JC0409` doc example). Codes appear in `.rs` source — `JC0409` only lives in a markdown doc example and an `Error::new(.., "JC0409", ..)` doc-test snippet inside a .md (not .rs), so the .rs grep won't see it. If the grep DOES surface a code that's a deliberate user-example (not framework-emitted), add it to the registry OR refine `find_codes` to skip test files — prefer adding any genuinely-emitted code; if a pure example leaks, scope the walk to `src/` only (not `tests/`). Record what you did.

- [ ] **Step 4: Implement the `explain` command** (main.rs)

Clap: `Cmd::Explain { code: String }`. Arm:

```rust
        Cmd::Explain { code } => cmd_explain(&code, cli.json),
```

```rust
fn cmd_explain(code: &str, json_mode: bool) -> Result<(), Failure> {
    let info = jerrycan::platform::codes::lookup(code)
        .ok_or_else(|| Failure::usage(format!("unknown code `{code}` — see `jerrycan explain JC0404` for the format")))?;
    if json_mode {
        println!("{}", serde_json::json!({
            "code": info.code, "title": info.title, "cause": info.cause, "fix": info.fix, "doc": info.doc,
        }));
    } else {
        println!("{} — {}", info.code, info.title);
        println!("\ncause: {}", info.cause);
        println!("fix:   {}", info.fix);
        println!("docs:  {}", info.doc);
    }
    Ok(())
}
```

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan --test explain` + the codes completeness unit test green. Full gate green. (If completeness flags an orphan, register it — that's the test doing its job.)

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/src/platform/codes.rs crates/jerrycan/src/platform/mod.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/explain.rs
git commit -m "Add diagnostic code registry and jerrycan explain command"
```

---

### Task 4: Benchmarks (criterion, informational)

**Files:**
- Modify: `crates/jerrycan-core/Cargo.toml`, `crates/jerrycan-auth/Cargo.toml`, root `Cargo.toml`
- Create: `crates/jerrycan-core/benches/core_bench.rs`, `crates/jerrycan-auth/benches/auth_bench.rs`
- Create: `docs/benchmarks.md`

- [ ] **Step 1: Add criterion**

Root `[workspace.dependencies]`: `criterion = { version = "0.5", features = ["html_reports"] }`.

`crates/jerrycan-core/Cargo.toml`:

```toml
[dev-dependencies]
# ... existing ...
criterion = { workspace = true }

[[bench]]
name = "core_bench"
harness = false
```

Same `[[bench]]` block (name `auth_bench`) + criterion dev-dep in `crates/jerrycan-auth/Cargo.toml`.

- [ ] **Step 2: Write `core_bench.rs`**

```rust
//! Hot-path benchmarks: router matching and full in-memory dispatch.
use criterion::{criterion_group, criterion_main, Criterion};
use jerrycan_core::{get, App, TestApp};

fn build_app() -> TestApp {
    let mut app = App::new();
    for i in 0..50 {
        let path = format!("/resource{i}/{{id}}");
        app = app.route(&path, get(|| async { "x" })); // route() stores an owned String; &str is fine
    }
    app.into_test()
}

fn bench_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let app = build_app();
    c.bench_function("dispatch_param_route", |b| {
        b.iter(|| rt.block_on(async { app.get("/resource25/42").await }));
    });
    c.bench_function("dispatch_404", |b| {
        b.iter(|| rt.block_on(async { app.get("/nope/nope").await }));
    });
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
```

- [ ] **Step 3: Write `auth_bench.rs`**

```rust
//! Crypto hot paths: session encode/decode and JWT encode/decode.
use criterion::{criterion_group, criterion_main, Criterion};
use jerrycan_auth::{jwt, SessionStore};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Sess { id: i64, role: String }

fn bench_session(c: &mut Criterion) {
    let store = SessionStore::new(&[9u8; 32]);
    let token = store.encode(&Sess { id: 1, role: "admin".into() }).unwrap();
    c.bench_function("session_encode", |b| {
        b.iter(|| store.encode(&Sess { id: 1, role: "admin".into() }).unwrap());
    });
    c.bench_function("session_decode", |b| {
        b.iter(|| store.decode::<Sess>(&token).unwrap());
    });
}

fn bench_jwt(c: &mut Criterion) {
    let key = [9u8; 32];
    let token = jwt::encode(&Sess { id: 1, role: "admin".into() }, &key).unwrap();
    c.bench_function("jwt_encode", |b| b.iter(|| jwt::encode(&Sess { id: 1, role: "admin".into() }, &key).unwrap()));
    c.bench_function("jwt_decode", |b| b.iter(|| jwt::decode::<Sess>(&token, &key).unwrap()));
}

criterion_group!(benches, bench_session, bench_jwt);
criterion_main!(benches);
```

(NOTE: `Sess` needs `exp`? jwt::decode only enforces exp when present — omitting it means the bench token never expires, fine for a benchmark. Keep it simple.)

- [ ] **Step 4: Build + run locally; record baselines**

`cargo build --benches --workspace` must succeed (this is what CI checks). Then `cargo bench -p jerrycan-core && cargo bench -p jerrycan-auth` locally (NOT in CI). Capture the median times into `docs/benchmarks.md`:

```markdown
# Benchmarks

Informational baselines (criterion, local run on <machine>, release). Not a CI gate.

| Benchmark | Median |
|---|---|
| dispatch_param_route | <fill from run> |
| dispatch_404 | <fill> |
| session_encode | <fill> |
| session_decode | <fill> |
| jwt_encode | <fill> |
| jwt_decode | <fill> |

Run locally: `cargo bench -p jerrycan-core -p jerrycan-auth`.
```

Fill the table with the REAL numbers from your run (record the machine). If `argon2` hashing is benched anywhere it'll dominate — we deliberately bench only the per-request crypto (session/jwt), not password hashing (which is intentionally slow).

- [ ] **Step 5: Run gates** — `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo build --benches --workspace && cargo test --workspace --all-features`. Benches build clean; clippy covers bench targets (`--all-targets`).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jerrycan-core/Cargo.toml crates/jerrycan-core/benches crates/jerrycan-auth/Cargo.toml crates/jerrycan-auth/benches docs/benchmarks.md
git commit -m "Add criterion benchmarks for dispatch and auth hot paths"
```

---
### Task 5: Agent-eval reference specs + deterministic scripted runner (CI)

**Files:**
- Create: `conformance/eval/specs/{blog,tasks,shortener,inventory,notes}.design.json`
- Create: `conformance/eval/fixtures/<spec>/…` (reference handler impls per spec — the known-good the scripted runner copies)
- Create: `crates/jerrycan/tests/eval.rs` (deterministic scripted runner, `#[ignore]` heavy)

The scripted runner is the CI signal that the WHOLE loop (design→scaffold→gen-tests→implement→check→serve) works across diverse designs. The "real LLM" half is Task 6 (a dispatched agent). Both share the same specs + pass criterion.

- [ ] **Step 1: Write 5 reference designs** (varied shape; all memory-mode for fast CI)

Each is a valid `design.json` (passes `questions::validate`). Keep them small but distinct:
- `blog.design.json` — modules: posts (CRUD + comments subroute), authors. Entities Post{title,body}, Comment{text}, Author{name}.
- `tasks.design.json` — modules: tasks (CRUD, done toggle via PUT), projects. Entities Task{title,done:bool}, Project{name}.
- `shortener.design.json` — modules: links (POST create, GET /{id} resolve, GET / list, DELETE). Entity Link{slug,target}.
- `inventory.design.json` — modules: items (CRUD), categories. Entities Item{sku,name,quantity:integer}, Category{name}.
- `notes.design.json` — modules: notes (CRUD), with a tags subroute. Entities Note{title,content}, Tag{label}.

Each MUST validate (kebab module names, snake operation_ids, PascalCase entities, snake fields, 2xx success, ≤3 path params, balanced braces). Author them by hand and verify each with `jerrycan` (see Step 3). Provide the full JSON for all five — write them out completely; do not abbreviate.

- [ ] **Step 2: Write reference fixtures**

For each spec, the known-good handler implementations (memory-mode, mirroring `conformance/fixtures/*_handlers.rs` patterns: `repo.all()`/`get`/`insert`/`remove`, `Error::not_found()` on miss). One file per route crate per spec under `conformance/eval/fixtures/<spec>/<module>_handlers.rs`. These let the SCRIPTED runner produce a passing app deterministically (the real-agent eval in Task 6 writes its own).

- [ ] **Step 3: Write the scripted runner** (`crates/jerrycan/tests/eval.rs`)

```rust
//! Deterministic agent-loop eval: for each reference spec, scaffold → gen-tests →
//! apply the reference fixtures → check → serve → smoke a CRUD request. Scores
//! pass/fail per spec. This is the CI signal that the loop works across designs;
//! the real-LLM eval (conformance/eval/results.md) is a dispatched agent.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}
fn jc() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_jerrycan")) }
fn framework_dep() -> String {
    format!("jerrycan = {{ path = \"{}\", default-features = false }}", repo_root().join("crates/jerrycan").display())
}

const SPECS: &[&str] = &["blog", "tasks", "shortener", "inventory", "notes"];

#[test]
#[ignore = "heavy: scaffolds, builds, checks, and serves 5 reference apps"]
fn scripted_agent_loop_builds_all_reference_apps() {
    let mut passed = 0;
    let mut report = String::new();
    for spec in SPECS {
        match run_one(spec) {
            Ok(()) => { passed += 1; report.push_str(&format!("PASS {spec}\n")); }
            Err(e) => report.push_str(&format!("FAIL {spec}: {e}\n")),
        }
    }
    eprintln!("scripted eval:\n{report}");
    assert_eq!(passed, SPECS.len(), "all reference apps must build+check+serve:\n{report}");
}

fn run_one(spec: &str) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let app = tmp.path().join(spec);
    let design = repo_root().join(format!("conformance/eval/specs/{spec}.design.json"));

    // scaffold
    let st = Command::new(jc()).env("JERRYCAN_FRAMEWORK_DEP", framework_dep())
        .arg("new").arg(&app).arg("--design").arg(&design).status().map_err(|e| e.to_string())?;
    if !st.success() { return Err("scaffold failed".into()); }

    // apply reference fixtures: copy each <module>_handlers.rs to its route crate
    let fixtures = repo_root().join(format!("conformance/eval/fixtures/{spec}"));
    for entry in std::fs::read_dir(&fixtures).map_err(|e| format!("fixtures dir: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let fname = entry.file_name().to_string_lossy().to_string(); // e.g. posts_handlers.rs OR posts__comments_handlers.rs
        // map "<module>_handlers.rs" → crates/routes/<module>/src/handlers.rs
        // map "<module>__<sub>_handlers.rs" → crates/routes/<module>/src/subroutes/<sub>/handlers.rs
        let target = handler_target(&app, &fname)?;
        std::fs::create_dir_all(target.parent().unwrap()).ok();
        std::fs::copy(entry.path(), &target).map_err(|e| format!("copy {fname}: {e}"))?;
    }

    // check (full gate)
    let out = Command::new(jc()).current_dir(&app).args(["--json", "check"]).output().map_err(|e| e.to_string())?;
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("check json: {e}"))?;
    if payload["ok"] != true { return Err(format!("check failed: {}", payload["diagnostics"])); }

    // serve + smoke one request to the first listed route
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo").current_dir(&app).env("JERRYCAN_ADDR", &addr)
        .args(["run", "-p", "app"]).spawn().map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut up = false;
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(&addr).is_ok() { up = true; break; }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let result = if up {
        let routes = Command::new(jc()).current_dir(&app).args(["--json", "list", "routes"]).output().unwrap();
        let rv: serde_json::Value = serde_json::from_slice(&routes.stdout).unwrap();
        let first = rv["routes"][0]["path"].as_str().unwrap_or("/").to_string();
        let body = http_get(&addr, &first);
        if body.starts_with("HTTP/1.1 2") || body.starts_with("HTTP/1.1 404") { Ok(()) } else { Err(format!("serve smoke bad status: {body}")) }
    } else {
        Err("app did not start".into())
    };
    let _ = server.kill();
    let _ = server.wait();
    result
}

fn handler_target(app: &Path, fixture_name: &str) -> Result<PathBuf, String> {
    let stem = fixture_name.strip_suffix("_handlers.rs").ok_or("bad fixture name")?;
    let base = app.join("crates/routes");
    if let Some((module, sub)) = stem.split_once("__") {
        Ok(base.join(module).join("src/subroutes").join(sub).join("handlers.rs"))
    } else {
        Ok(base.join(stem).join("src/handlers.rs"))
    }
}

fn http_get(addr: &str, path: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
```

- [ ] **Step 4: Run it** — `cargo test -p jerrycan --test eval -- --include-ignored` (budget 10–20 min cold; 5 apps each built). All 5 PASS. If a spec fails, the design or its fixtures are wrong — fix them (the runner is correct). Record any spec/fixture fixes.

- [ ] **Step 5: Commit**

```bash
git add conformance/eval/specs conformance/eval/fixtures crates/jerrycan/tests/eval.rs
git commit -m "Add reference designs and a deterministic scripted agent-loop eval"
```

---

### Task 6: Real agent-driven eval (the ≥90% metric)

**Files:**
- Create: `conformance/eval/results.md`
- Create: `conformance/eval/PROTOCOL.md`

This task is performed BY a dispatched agent acting as a jerrycan USER — it reads ONLY `jerrycan docs` + drives the MCP/CLI, writing handlers from scratch (NOT copying the reference fixtures). No external model API; the agent is the one already in the loop. The pass rate it achieves is the release metric.

- [ ] **Step 1: Write `conformance/eval/PROTOCOL.md`** — the eval procedure:

````markdown
# jerrycan agent eval protocol

**Goal:** measure how often a fresh agent, using ONLY `jerrycan docs` and the
`jerrycan` MCP/CLI, turns a reference design into an app that passes
`jerrycan check` and serves a correct CRUD round-trip.

**Per spec (5 total in conformance/eval/specs/):**
1. `jerrycan new <app> --design <spec>` (with JERRYCAN_FRAMEWORK_DEP=path for pre-publish).
2. `jerrycan gen-tests --module <m>` for each module.
3. Read the relevant `jerrycan docs` pages (database/auth/etc. as the design needs).
4. Implement every generated handler stub FROM SCRATCH — no copying the
   conformance reference fixtures.
5. `jerrycan check` until green (iterate on diagnostics; each failure that the
   docs didn't prevent is a docs/diagnostics bug to log).
6. Run the app; verify one create→read→delete round-trip per top module.

**Scoring:** a spec PASSES if check is green AND the round-trip behaves. Record
pass/fail per spec and the overall rate. Target: ≥ 90% (≥ 5/5 ideally; ≥ 4/5 acceptable
with the failure root-caused and ticketed). Every failure feeds the
error-driven-docs loop (spec §8): file what doc/diagnostic would have prevented it.
````

- [ ] **Step 2: A dispatched agent performs the eval**

(Orchestrator: dispatch a fresh subagent whose ONLY knowledge of jerrycan is the published docs surface — `jerrycan docs <page>` + the MCP/CLI. It must NOT read jerrycan's source or the conformance reference fixtures. It builds each of the 5 reference apps from scratch, runs `jerrycan check` to green, and serves a round-trip. It returns a pass/fail table + any docs/diagnostics gaps it hit.)

The orchestrator records the result verbatim into `conformance/eval/results.md`:

```markdown
# Agent eval results

Run: <date> · agent: <model/agent id> · jerrycan @ <commit>

| Spec | check green | serve round-trip | result |
|---|---|---|---|
| blog | ✅ | ✅ | PASS |
| tasks | … | … | … |
| shortener | … | … | … |
| inventory | … | … | … |
| notes | … | … | … |

**Pass rate: N/5 (NN%)** — target ≥ 90%.

## Docs/diagnostics gaps surfaced (error-driven-docs loop)
- <none, or a list of what would have prevented each failure>
```

- [ ] **Step 3: If the rate is < 90%**, root-cause each failure: improve the relevant `docs/ai` page or the diagnostic message/`jerrycan explain` text (NOT the eval), commit those improvements, and re-run the eval. Loop until ≥ 90% or the remaining failures are genuinely out-of-scope (ticket them in the backlog). Record the final rate.

- [ ] **Step 4: Commit**

```bash
git add conformance/eval/PROTOCOL.md conformance/eval/results.md docs/ai
git commit -m "Add agent eval protocol and record the reference-app pass rate"
```

---
### Task 7: v0.1.0 — version bump + crate metadata

**Files:**
- Modify: root `Cargo.toml` (`[workspace.package] version`, internal `[workspace.dependencies]` version pins)
- Modify: every crate's `Cargo.toml` (metadata)
- Modify: `crates/jerrycan/src/platform/templates.rs` (template default dep version)

- [ ] **Step 1: Bump the workspace version + internal pins**

Root `Cargo.toml`:
- `[workspace.package] version = "0.0.0"` → `version = "0.1.0"`.
- In `[workspace.dependencies]`, every internal entry's explicit `version = "0.0.0"` → `"0.1.0"`: `jerrycan-core`, `jerrycan-macros`, `jerrycan-db`, `jerrycan-auth`, `jerrycan-validate`, `jerrycan-observe`. (These are the `{ path = "...", version = "..." }` entries; the `version` is what crates.io consumers resolve.)
- Any per-crate `Cargo.toml` that still hardcodes a path-dep `version = "0.0.0"` (e.g. `jerrycan-db = { path = "../jerrycan-core", version = "0.0.0" }` lines inside crates) → `"0.1.0"`. Grep: `grep -rn 'version = "0.0.0"' crates/` and bump every internal dep pin. (Crates inherit `version.workspace = true` for their OWN version — that's already 0.1.0 via the bump; only the explicit dep-pin literals need editing.)

- [ ] **Step 2: Add release metadata to `[workspace.package]`**

```toml
[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
homepage = "https://jerrycan.cc"
repository = "https://github.com/backant-io/jerrycan"
authors = ["Pavel Hegler"]
rust-version = "1.85"
```

Each crate's `[package]` adds `repository.workspace = true`, `authors.workspace = true`, `rust-version.workspace = true`, plus crate-specific `keywords`/`categories` (these can't be workspace-inherited cleanly across differing crates, so set per-crate):
- `jerrycan` (facade/CLI/MCP): `keywords = ["web", "framework", "backend", "ai", "mcp"]`, `categories = ["web-programming::http-server", "command-line-utilities"]`, `readme = "README.md"` (point at the repo README via a symlink or a short crate README — simplest: add `crates/jerrycan/README.md` = a short pointer + the facade taste; set `readme = "README.md"`).
- `jerrycan-core`: `keywords = ["web","framework","async","http"]`, `categories = ["web-programming::http-server"]`.
- `jerrycan-db`: `keywords = ["sql","sqlx","migrations"]`, `categories = ["database"]`.
- `jerrycan-auth`: `keywords = ["auth","jwt","session","argon2"]`, `categories = ["authentication"]`.
- `jerrycan-validate`: `keywords = ["validation","openapi"]`, `categories = ["web-programming"]`.
- `jerrycan-observe`: `keywords = ["observability","metrics","logging"]`, `categories = ["web-programming"]`.
- `jerrycan-macros`: `keywords = ["macro","async"]`, `categories = ["development-tools"]`.

crates.io requires `description` (present), `license` (present), and at most 5 keywords each (≤ 20 chars each). Verify counts.

- [ ] **Step 3: Bump the generated-app template default** (closes Phase-0 backlog + Phase-3 Dockerfile gap)

`crates/jerrycan/src/platform/templates.rs` — `jerrycan_dep_spec_from`'s default literal:

```rust
pub(crate) fn jerrycan_dep_spec_from(env: Option<String>) -> String {
    env.unwrap_or_else(|| "jerrycan = { version = \"0.1.0\", default-features = false }".to_string())
}
```

Update the `features_inject_into_the_dep_line` test's expected base string to `version = "0.1.0"`. NOTE: conformance tests set `JERRYCAN_FRAMEWORK_DEP` (path), so they still build pre-publish; only the DEFAULT (what a real user gets) changes to the published version.

- [ ] **Step 4: Verify the workspace still builds + gates green**

`cargo build --workspace --all-features` (version bump is pure metadata; nothing should break). Update any test that asserted `version = "0.0.0"` in generated output (the templates test from Step 3; grep tests for `0.0.0`). Full `--all-features` gate green. `cargo test -p jerrycan` generation/dbgen/cli tests that pin the dep line must be updated to `0.1.0` — find and fix them.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/*/Cargo.toml crates/jerrycan/README.md crates/jerrycan/src/platform/templates.rs crates/jerrycan/tests
git commit -m "Bump to 0.1.0 with release metadata and published-version app template"
```

---

### Task 8: v0.1.0 — dry-run, CHANGELOG, publish script

**Files:**
- Create: `CHANGELOG.md`, `scripts/publish.sh`

- [ ] **Step 1: `cargo publish --dry-run` every crate, in dependency order**

The crates form a DAG: `jerrycan-core` and `jerrycan-macros` have no internal deps; `jerrycan-db`/`jerrycan-auth`/`jerrycan-validate`/`jerrycan-observe` depend on core; `jerrycan` (facade) depends on all. Dry-run order:

```bash
for c in jerrycan-core jerrycan-macros jerrycan-db jerrycan-auth jerrycan-validate jerrycan-observe jerrycan; do
  echo "=== dry-run $c ==="
  cargo publish -p "$c" --dry-run --allow-dirty 2>&1 | tail -8
done
```

Expected: each PACKAGES + VERIFIES (compiles the packaged tarball). **Known wrinkle:** `cargo publish --dry-run` resolves path deps against the registry version — since core/etc. aren't published yet at 0.1.0, a dry-run of `jerrycan-db` may fail to find `jerrycan-core 0.1.0` on crates.io. cargo's `--dry-run` uses the LOCAL path dep for verification in a workspace, so it should compile; if it complains about the unpublished dep, that's expected and resolves itself during the REAL ordered publish (each crate is on crates.io before the next dry-runs against it). Record which crates dry-ran clean vs which only validate post-publish. The FACADE (`jerrycan`) with optional features: dry-run with `--all-features` too so db/auth/validate/observe optional deps are checked.

Fix any real packaging errors (missing `description`/`license`, files excluded by `.gitignore` that the package needs, `readme` path). Common fix: ensure no crate references a path that escapes its own dir except via the workspace deps (cargo packages each crate standalone).

- [ ] **Step 2: Write `CHANGELOG.md`**

```markdown
# Changelog

## 0.1.0 — first release

The first public release of jerrycan — the AI-native Rust backend platform.

### Framework (jerrycan-core)
- App/Module routing with a backtracking trie, typed extractors, FastAPI-grade
  dependency injection (async, nested, per-request cached, test-overridable),
  middleware, in-memory TestApp.
- Secure by default: security headers, body/read/handler timeouts, panic→500
  containment, graceful shutdown (SIGINT/SIGTERM), percent-decoding, 1 MiB body
  cap. `#![forbid(unsafe_code)]`.
- Stable `JC####` error codes mapped to docs anchors.

### Extensions
- `jerrycan-db` — SQLite + Postgres via sqlx, module-owned dual-dialect
  migrations, `?`→`$n` translation.
- `jerrycan-auth` — argon2 password hashing, ChaCha20-Poly1305 sessions, HS256
  JWT, `Session`/`Bearer` extractors, role guards.
- `jerrycan-validate` — `Valid<T>` extractor, structured 422s, OpenAPI 3.1.
- `jerrycan-observe` — request IDs, JSON access logs, `/healthz`, Prometheus
  `/metrics`.

### Platform (the `jerrycan` binary: CLI + MCP)
- Design-first, TDD generation of crate-per-module apps; `new`, `generate`,
  `gen-tests`, `check`, `test`, `dev`, `list`, `docs`, `explain`, `add`,
  `db migrate`, `package`, `mcp`.
- `jerrycan package` → hardened Docker/k8s/systemd + static binary + CycloneDX SBOM.
- AI-native docs (every example a doc-test) + the MCP tool contracts.

### Hardening
- Fuzz-smoke + cargo-fuzz targets on all parsers; criterion benchmarks; an
  agent-eval harness (reference apps + recorded pass rate).
```

- [ ] **Step 3: Write `scripts/publish.sh`** (the maintainer runs this)

```bash
#!/usr/bin/env bash
# Publish jerrycan v0.1.0 to crates.io in dependency order.
# Prerequisites: `cargo login` with a publish-scoped token; verified email.
# Run from the repo root. Rate-limit tolerant (waits between new-crate pushes).
set -euo pipefail

CRATES=(jerrycan-core jerrycan-macros jerrycan-db jerrycan-auth jerrycan-validate jerrycan-observe jerrycan)

for c in "${CRATES[@]}"; do
  echo "=== publishing $c ==="
  tries=0
  until cargo publish -p "$c"; do
    rc=$?
    out=$(cargo publish -p "$c" 2>&1 || true)
    if echo "$out" | grep -qi "already.*uploaded\|already exists"; then
      echo "SKIP $c (already published)"; break
    fi
    tries=$((tries+1))
    if [ "$tries" -ge 10 ]; then echo "GIVING UP on $c after $tries tries"; exit 1; fi
    echo "retry $c in 60s (rate limit or index lag)…"; sleep 60
  done
  # Let the index update so the next crate resolves this one.
  echo "waiting 30s for crates.io index…"; sleep 30
done
echo "All crates published at 0.1.0."
```

`chmod +x scripts/publish.sh`. Add a header note: the facade `jerrycan` publishes last because it depends on all the others; each crate must be indexed before the next resolves it (hence the inter-publish wait).

- [ ] **Step 4: Final dry-run sanity** — re-run the Step 1 loop; record the outcome (which crates fully verify pre-publish vs which only resolve during the real ordered run). Confirm `scripts/publish.sh` is syntactically valid: `bash -n scripts/publish.sh`.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md scripts/publish.sh
git commit -m "Add CHANGELOG and ordered publish script for v0.1.0"
```

---

### Task 9: CI, docs, README, roadmap + Phase 4 exit gate

**Files:**
- Modify: `.github/workflows/ci.yml`, `README.md`, `docs/phase1-backlog.md`
- Create: `docs/ai/13-error-codes.md`

- [ ] **Step 1: CI — build benches, build fuzz on nightly (non-blocking), run the scripted eval**

In `.github/workflows/ci.yml`, in the `gate` job after the existing test step, add a bench build (cheap, stable):

```yaml
      - name: Build benches
        run: cargo build --workspace --benches --all-features
```

Add the scripted eval to the heavy conformance step:

```yaml
      - name: "Conformance (heavy)"
        run: |
          cargo test -p jerrycan --test conformance -- --include-ignored
          cargo test -p jerrycan --test genroute_compile -- --include-ignored
          cargo test -p jerrycan --test eval -- --include-ignored
```

Add a SEPARATE non-blocking nightly fuzz-build job (does not gate merges):

```yaml
  fuzz-build:
    runs-on: ubuntu-latest
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - name: Build fuzz targets
        run: cargo install cargo-fuzz && cargo +nightly fuzz build
```

Validate the YAML: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`.

- [ ] **Step 2: Write `docs/ai/13-error-codes.md`** (prose page; the human-readable code table — mirror the registry)

````markdown
# Error & lint codes

Every jerrycan diagnostic carries a stable code. `jerrycan explain <code>` prints
the cause + fix for any of them.

## Runtime errors (`JC####`)
| Code | Meaning |
|---|---|
| JC0400 | Bad request — malformed path param / query / percent-encoding |
| JC0401 | Authentication required or failed |
| JC0403 | Forbidden — role check failed |
| JC0404 | Not found |
| JC0405 | Method not allowed |
| JC0408 | Request body read timeout |
| JC0413 | Payload too large (default 1 MiB) |
| JC0422 | Unprocessable — bad JSON or validation violations |
| JC0500 | Internal error (or handler panic) |
| JC0503 | Handler timeout (default 30s) |
| JC0510 | Database error (jerrycan-db) |
| JC1001 | Missing dependency provider |
| JC1002 | Dependency cycle |

## Generation lints (`JL####`)
| Code | Meaning |
|---|---|
| JL0001 | Route crate exports more than `module()` |
| JL0002 | Design endpoint has no matching handler |
| JL0003 | Generated file drifted from the design |
| JL0004 | Mutating route unguarded in an auth design |
````

Mount it ungated in `docsidx.rs` PAGES as `error-codes` and add a `#[cfg(doctest)]` doc_page only if it has runnable rust fences — it has none (pure tables), so add it to PAGES (for `jerrycan docs error-codes`) but NOT to the doctest harness.

- [ ] **Step 3: README + roadmap + backlog**

`README.md`:
- Roadmap: Phase 4 → `✅ complete`; add a final line "v0.1.0 — first release" once published (note it's publish-pending until the maintainer runs `scripts/publish.sh`).
- Status blurb: update from "early development" to reflect a 0.1.0 release candidate; keep honest ("0.1.0, first release").
- Add an **Install** section: `cargo add jerrycan --features db,auth,validate,observe` (apps) and `cargo install jerrycan` (the CLI/MCP) — note these work once 0.1.0 is published.
- Add an **Agent eval** line citing `conformance/eval/results.md` and the recorded pass rate.
- Development section: `cargo test --workspace --all-features`; mention `cargo bench` and `fuzz/`.

`docs/phase1-backlog.md`: remove now-resolved items — the facade version-literal item (done: bumped to 0.1.0 + template default), percent-decoder/router fuzzing (done: fuzz targets + smoke). Keep genuinely-deferred items (OAuth/OIDC, RS256, rate-limiting, multi-arch images, derive(Validate) rules, checked SQL, json db columns, span diagnostics, pg advisory lock, OpenAPI dup-status merge). Rename the file's scope note if it still says "phase 1".

- [ ] **Step 4: The Phase 4 exit gate (run ALL, report truthfully)**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --benches --all-features
cargo test -p jerrycan --test conformance -- --include-ignored
cargo test -p jerrycan --test genroute_compile -- --include-ignored
cargo test -p jerrycan --test eval -- --include-ignored
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
bash -n scripts/publish.sh
# release readiness:
for c in jerrycan-core jerrycan-macros jerrycan-db jerrycan-auth jerrycan-validate jerrycan-observe jerrycan; do cargo publish -p "$c" --dry-run --allow-dirty >/dev/null 2>&1 && echo "dry-run OK $c" || echo "dry-run NEEDS-PUBLISHED-DEPS $c (expected pre-publish)"; done
```

Report each. The agent-eval pass rate (Task 6) is recorded and ≥ 90%.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml docs README.md crates/jerrycan/src/platform/docsidx.rs
git commit -m "Wire CI bench/fuzz/eval, document codes, and mark Phase 4 complete"
```

---

## Execution notes

- **Order:** 1 → 9 strictly. Tasks 5/6 (eval) are the spec's headline metric; Task 6 dispatches an agent that uses ONLY the published docs surface (no jerrycan source, no reference fixtures) — that is the genuine eval. Tasks 7/8 prepare the release; the maintainer runs `scripts/publish.sh` (NOT this plan).
- **Gates carry `--all-features`** everywhere; `fuzz/` is OUTSIDE the workspace so gates ignore it.
- **Heavy tests:** Task 5 eval (5 apps), plus the existing Phase 1/2/3 conformance; CI runs all with `--include-ignored`. The nightly fuzz-build job is `continue-on-error` (non-gating).
- **Pre-solved traps:** `read_cookie` → `pub` for the auth smoke; `decode_segment`/`Trie` are `pub(crate)` so fuzz/smoke exercise them via the public `TestApp.get` (not directly); criterion benches need `harness = false`; the template default bump to `0.1.0` changes generated apps but conformance overrides via `JERRYCAN_FRAMEWORK_DEP`; `cargo publish --dry-run` of an unpublished-dep crate may only fully verify during the real ordered publish — that's expected, not a failure.
- **The maintainer publishes.** This plan ends at a green dry-run + `scripts/publish.sh`; the actual `cargo publish` (irreversible) is the maintainer's, with their authenticated account.
- **Out of scope (tracked):** everything still in `docs/phase1-backlog.md` after Step 3's trim (OAuth/OIDC, RS256, rate-limiting, multi-arch images, derive(Validate) rules, checked SQL, json db columns, span diagnostics, pg advisory lock).
