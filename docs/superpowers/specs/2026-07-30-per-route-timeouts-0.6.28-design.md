# Per-route timeout knobs (0.6.28) — #111

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #111 (a slow-but-moving large upload has no working shape. The app-global `handler_timeout`/`body_read_timeout` (both default 30s, `app.rs`) are the ONLY levers, and `body_limit` is the only PER-ROUTE knob. On `.stream_body()` the body drains INSIDE the handler, so the app-global `handler_timeout` fires (503 JC0503) on a slow upload even though the per-frame `body_read_timeout` is satisfied. The only working shape today is raising the app-global budget in TOOL-OWNED `main.rs` — a permanent JL0003 trip. Reproduced with a real dribbling-socket test.)
**Ships as:** 0.6.28 — a `jerrycan-core` runtime API addition: per-route `handler_timeout` and `body_read_timeout` overrides on `MethodRouter`, mirroring the existing per-route `body_limit`. Byte-identical scaffolding — a route that does not set them is unchanged; the app-global default still applies.

## Approach (issue option 1, chosen)
The issue offers two fixes: (1) a per-route `handler_timeout`/`body_read_timeout` knob like `body_limit`; or (2) exclude on-demand body-drain time from the handler budget on `.stream_body()` routes. **Choose (1)** — it is more general (any slow route, not just stream_body), it mirrors the established `body_limit` per-route pattern exactly, and critically it lives on the AGENT-OWNED route registration (`.route(path, post(h).handler_timeout(..))`), NOT tool-owned `main.rs`, so it does NOT trip JL0003. Do NOT implement (2).

## The change — mirror `body_limit` end to end
`body_limit` is the template: `MethodRouter.body_limit: Option<usize>` (`router.rs:19`), a `.body_limit(usize)` builder (`router.rs:59`), threaded to the flattened struct + `Endpoint` (`app.rs:305/479`), applied as `endpoint.body_limit.unwrap_or(BODY_LIMIT)`. Do the same for two `Option<Duration>` fields:

1. **`router.rs`** — add `handler_timeout: Option<std::time::Duration>` and `body_read_timeout: Option<std::time::Duration>` to `MethodRouter` (init `None` in the constructors, `router.rs:45`) AND to the flattened struct (`router.rs:96`, init `None` at `router.rs:303`). Add builders:
   ```rust
   /// Override the app-global handler-time budget for THIS route (issue #111).
   /// A slow-but-moving upload on a `.stream_body()` route drains inside the
   /// handler, so raise this (not the app-global in tool-owned main.rs — that
   /// trips JL0003) to give the drain room. `None` ⇒ the app default applies.
   pub fn handler_timeout(mut self, budget: std::time::Duration) -> Self {
       self.handler_timeout = Some(budget);
       self
   }
   /// Override the app-global per-frame body-read deadline for THIS route (#111).
   pub fn body_read_timeout(mut self, budget: std::time::Duration) -> Self {
       self.body_read_timeout = Some(budget);
       self
   }
   ```
2. **Thread through flatten/module** — wherever `body_limit`/`stream_body` travel from `MethodRouter` → flattened → `Endpoint` (`app.rs:229/289/305`, and the module path `module.rs:78` if modules carry `body_limit`), carry the two new `Option<Duration>` alongside, identically. Add `handler_timeout: Option<Duration>` and `body_read_timeout: Option<Duration>` to `Endpoint` (`app.rs:323`-region is the BuiltApp global; the per-route `Endpoint` struct is where `body_limit`/`stream_body` land — `app.rs:479-480`).
3. **Apply points — per-route overrides the global:**
   - **handler_timeout:** at `app.rs:537` `tokio::time::timeout(self.handler_timeout, run)`, use `endpoint.handler_timeout.unwrap_or(self.handler_timeout)` (the per-route value when set, else the app-global). Confirm `endpoint`/the global are both in scope at that call site; if the timeout wrap does not currently see the endpoint, thread the resolved per-route duration to it.
   - **body_read_timeout:** find where `BuiltApp.body_read_timeout` feeds the per-frame `TimedRecvBody` deadline (grep `TimedRecvBody` / `body_read_timeout` in `extract.rs`/`app.rs`) and use `endpoint.body_read_timeout.unwrap_or(built.body_read_timeout)` at that construction point.

Do NOT add a per-route `write_stall_timeout` (not asked — YAGNI).

## Tests (jerrycan-core, mirror the `body_limit` tests at `extract.rs:687`)
1. **handler_timeout per-route raises the budget:** an app with a SHORT app-global `handler_timeout` (e.g. 50ms) + a handler that sleeps 200ms → 503 by default; the SAME handler on a route with `.handler_timeout(1s)` → 200. Proves the per-route override wins.
2. **handler_timeout per-route lowers the budget:** app-global long, route `.handler_timeout(50ms)` on a 200ms handler → 503. Proves override applies downward too.
3. **The #111 scenario:** a `.stream_body()` upload route with `.handler_timeout(long)` survives a slow (dribbling) body drain that the default 30s… — use a scaled-down analogue (short app-global handler_timeout, a stream_body handler that drains slowly under a raised per-route `.handler_timeout`) → 200, where the default would 503. Model on the existing dribbling-socket / `stream_body` tests (`multipart.rs:857`, `extract.rs:625`).
4. **body_read_timeout per-route** override applies (short global rejects a stalled frame; a route `.body_read_timeout(longer)` tolerates it) — mirror the per-frame deadline test.
5. **Byte-identity/default:** a route with neither knob behaves EXACTLY as before (app-global applies) — an existing test without the knobs still passes unchanged.

## Gates
- `cargo test -p jerrycan-core` (new + existing timeout/body_limit tests) green; `cargo test -p jerrycan` green.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` — this is a jerrycan-core API ADDITION; generated code is unchanged (no route opts in), so the batteries must stay green + byte-identical.
- `cargo fmt`/`clippy -D warnings`; `cargo semver-checks` — the two new `MethodRouter` methods + `Option` fields are ADDITIVE (no breaking change).
- Byte-identity: scaffolding unchanged (`determinism.rs` green) — no generated route sets the new knobs.

## Success criteria
- A route can call `.handler_timeout(dur)` / `.body_read_timeout(dur)` to override the app-global budget for that route only, set on the agent-owned route registration (no JL0003 trip).
- A slow-but-moving `.stream_body()` upload survives with a raised per-route `handler_timeout` where the 30s app-global default would 503.
- Additive/byte-identical for every route that does not opt in; heavy gate + semver green; published 0.6.28; #111 closed.

## Non-goals
- A per-route `write_stall_timeout`. A DESIGN-level (codegen) expression of per-route timeouts (this is the runtime builder API; a design surface can be a follow-up). Changing the app-global defaults or the JC0503 behavior. Excluding drain time from the handler budget (issue option 2 — superseded by the per-route knob).
