# Design-visible rate limiting (0.6.21) — #83

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #83 (rate limiting WORKS at runtime — 429 + Retry-After proven live — but is **design-invisible**: no design.json surface; the only wiring point is tool-owned `main.rs`, which the agent must hand-edit to `.extend(RateLimit::…)`, **permanently tripping JL0003** (the byte-equality drift lint). No supported path exists. Plus a docs bug: `06-middleware.md` claims api-key partitioning is on by default; it is OFF-by-default opt-in.)
**Ships as:** 0.6.21 — an additive design surface + generated wiring + validation + a docs fix. Byte-identical for any design with no `rate_limit` block.

## The JL0003 fix, by construction
JL0003 trips because rate-limit wiring lives in a HAND-EDIT to the tool-owned `main.rs` (drift from what the generator would emit). The fix makes the `.extend(RateLimit::…)` line **generated from the design's `rate_limit` block**, so `main.rs` stays byte-identical to the tool's output — no hand-edit, no drift, no JL0003. This is the whole point: a supported, design-declared wiring path.

## A. Contract: a `rate_limit` block on the design
Add `pub rate_limit: Option<RateLimitDesign>` to `Design` (design.rs, beside `storage`/`realtime`), and:
```rust
#[serde(deny_unknown_fields)]
pub struct RateLimitDesign {
    /// Requests allowed per window per partition key.
    pub limit: u32,
    /// The fixed window, a duration string: "30s", "1m", "1h", "1d".
    pub window: String,
    /// Opt-in api-key partition tier: the header carrying the api key (e.g.
    /// "x-api-key"). Absent ⇒ partition by the authenticated user then client IP
    /// (both unspoofable). NOTE: an UNAUTHENTICATED api-key header is spoofable —
    /// document that caveat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_header: Option<String>,
    /// Trust `X-Forwarded-For` for the client IP (only behind a trusted proxy).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_forwarded_for: bool,
}
```
Add `Design::wants_rate_limit(&self) -> bool = self.rate_limit.is_some()`. Add a `Design::parse_duration(s) -> Option<Duration/secs>` helper mirroring `parse_size` (design.rs:876): `s`/`m`/`h`/`d` suffixes → seconds; a bare number = seconds. Mirror in `docs/contracts/design-schema.json` (pinned by `tests/contracts.rs`).

## B. Facade feature
`Design::facade_features()` (used by scaffold.rs:212 + mounting.rs:15) adds the jerrycan **`rate-limit`** feature when `wants_rate_limit()` (mirror `wants_storage`/`wants_jobs`). Confirm the feature name against `crates/jerrycan/Cargo.toml`'s `rate-limit`/`ratelimit` facade feature and the `jerrycan::ratelimit` re-export path.

## C. Mounting: generate the `.extend(RateLimit::…)` line
In the `.extend(...)` block builder (mounting.rs ~:29-60), when `wants_rate_limit()`, emit:
```rust
.extend(jerrycan::ratelimit::RateLimit::per_window({limit}, std::time::Duration::from_secs({window_secs})){api_key}{fwd})
```
where `{api_key}` = `.api_key_header("{header}")` if set, `{fwd}` = `.trust_forwarded_for(true)` if set. **Order:** rate limiting is identity-aware (partitions by the authenticated user when available), so place it AFTER `Auth` in the extend chain (so a `CurrentUser` partition can resolve) but it needs no db — document the chosen position in the order comment (mounting.rs:29). It is a middleware extension like the others; follow the existing load-bearing-order rationale. The line is TOOL-OWNED (generated), so JL0003 sees no drift.
Confirm the exact `RateLimit` builder API (`per_window(u32, Duration)`, `.api_key_header(impl Into<HeaderName>)`, `.trust_forwarded_for(bool)` — jerrycan-ratelimit lib.rs) and the `jerrycan::ratelimit::RateLimit` re-export.

## D. Validation — `JC0563` (next free after JC0562)
Register **JC0563** (codes.rs + explain + completeness test, mirror JC0562) and refuse a malformed `rate_limit`: `limit == 0` (a 0-limit blocks everything — surely unintended); `window` that does not `parse_duration` to a positive duration; an `api_key_header` that is not a valid HTTP header name (`^[A-Za-z0-9-]+$`). Message cites `jerrycan explain JC0563`.

## E. Docs
- **Fix the existing bug** in `docs/ai/06-middleware.md` (~:93-94, + embedded twin): api-key partitioning is **off by default** (opt-in via the design's `api_key_header` / the runtime `.api_key_header(name)`), NOT on by default; add the **spoofability caveat** (an unauthenticated api-key header is client-controlled — only partition by it when the key is itself authenticated).
- **Document the new `rate_limit` design block** (06-middleware.md + twin, or 00-designing.md): `{ limit, window, api_key_header?, trust_forwarded_for? }`, that the generator wires it into `main.rs` (no hand-edit, no JL0003), the 429 + Retry-After + JC0429 behavior, and the default IP/user partition. Edit BOTH twins identically (embedded_sync gate). **Doc-test note (0.6.18 lesson):** if any doc example is a ```rust block it is compiled — keep it a compiling doc-test or a non-test fence; run `cargo test -p jerrycan --doc`.

## F. testgen / byte-identity
- A design with `rate_limit` should get a generated acceptance test that a burst over `limit` within `window` gets **429** (reuse `TestApp::clock()` for deterministic windows — the runtime already supports it). If a clean 429 probe is disproportionate, at minimum assert the generated `main.rs` contains the `.extend(RateLimit::…)` line (a mounting no-drift unit test) — state which.
- Byte-identity: no `rate_limit` ⇒ no `.extend` line, no feature, no test, no validation — every existing design scaffolds byte-identically (`determinism.rs` + base-vs-HEAD `diff -r`).
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored`. **Fixture proof:** a design with a `rate_limit` block → scaffold: `main.rs` has the generated `.extend(RateLimit::…)` line, the facade `rate-limit` feature is on, `jerrycan check` is green (and, crucially, **JL0003 does NOT trip** — main.rs matches the generator), and a burst test 429s. Add as a conformance/unit fixture.
- `cargo semver-checks`: `Design` gains a serde-default `Option` field (additive — 0.6.1 precedent); scope-allow if the constructible-struct lint fires.

## Success criteria
- A `rate_limit: { limit: 100, window: "1m" }` design → generated `main.rs` wires `RateLimit::per_window(100, Duration::from_secs(60))`; the `rate-limit` facade feature is on; `jerrycan check` green; **JL0003 does NOT trip** (no hand-edit needed); a burst over 100/min gets 429.
- A malformed `rate_limit` (limit 0, bad window, bad header) → JC0563.
- `06-middleware.md` no longer claims api-key partitioning is on by default and states the spoofability caveat.
- No `rate_limit` ⇒ byte-identical; heavy gate green; semver additive; published 0.6.21; #83 closed.

## Non-goals
- Per-ENDPOINT rate limits (per-app only in v1 — a design-wide `rate_limit`; per-route knobs are a follow-up). The Redis store selection (the `rate-limit-redis` feature stays a runtime/build choice; the design block uses the default in-memory store). Custom `user_key` closures (agent code, not design).
