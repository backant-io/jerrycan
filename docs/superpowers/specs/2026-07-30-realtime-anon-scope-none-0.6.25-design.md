# Anonymous clients can reach scope-`none` realtime topics (0.6.25) — #117

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #117 (HIGH — when an auth model exists, `WsStart::from_request` (ws.rs:68) runs the principal resolver with `?`, so an ANONYMOUS client is 401'd at the WebSocket UPGRADE, before any per-topic scope check. This contradicts `scope_allows(TopicScope::None, None) => Ok` (channel.rs:87) and the resolver's own doc ("an absent resolver ⇒ anonymous connections join scope-none"). A public (scope-`none`) topic — e.g. an auction's price feed — is unreachable by anonymous clients the moment the app has any auth. Found: round-5 app `auction`.)
**Ships as:** 0.6.25 — a realtime WS-upgrade auth fix in `jerrycan-realtime`. Additive/permissive (anon gains access to scope-`none` topics that were wrongly blocked); scope-`auth`/`tenant` topics are unaffected. No design/codegen change; byte-identical scaffolding.

## Root cause + fix
`from_request` (crates/jerrycan-realtime/src/ws.rs:67-70):
```rust
let principal = match handle.resolver.as_ref() {
    Some(r) => Some(r(ctx).await?),   // ← `?` 401s an anonymous client at the upgrade
    None => None,
};
```
When a resolver is installed, an anonymous (or bad-credential) client makes `r(ctx)` return an auth error (401), and `?` aborts the upgrade — so the connection never reaches `scope_allows`, which would have allowed a scope-`none` join. An ABSENT resolver already yields `None` (anonymous) and works; the fix makes a resolver auth-FAILURE behave the same.

**Fix:** treat a resolver **authentication failure (401)** as an ANONYMOUS connection (`principal = None`) rather than a hard upgrade 401; propagate only genuine non-auth errors (e.g. a 5xx backend failure):
```rust
let principal = match handle.resolver.as_ref() {
    Some(r) => match r(ctx).await {
        Ok(p) => Some(p),
        // #117: no/invalid credential ⇒ an ANONYMOUS connection, not a hard 401 at
        // the upgrade. Per-topic `scope_allows` enforces access: scope-`none` is
        // public (joinable), scope-`auth`/`tenant` reject a `None` principal at JOIN.
        // A NON-auth error (backend failure) still propagates.
        Err(e) if e.status().as_u16() == 401 => None,
        Err(e) => return Err(e),
    },
    None => None,
};
```
Confirm `Error::status()` (jerrycan-core) is available on the resolver's error and that the resolver returns a 401-status error for a missing/invalid credential (grep the generated/wired resolver + `JC0401`, lib.rs:295-region). If the error type doesn't expose a status cleanly, use the resolver's documented auth-failure signal — do NOT swallow ALL errors indiscriminately (a genuine server error must not silently degrade to anonymous).

## Security analysis (must hold)
Treating a bad/expired credential as anonymous (`None`) does NOT escalate: `scope_allows` (channel.rs:85) rejects a `None` principal from every scope-`auth` topic ("authentication required") and every scope-`tenant` topic ("tenant membership required"). So a `None` principal can reach ONLY scope-`none` topics — which are public by definition. A bad credential therefore accesses nothing an anonymous client couldn't. The change is strictly: anon/bad-cred can now reach PUBLIC topics (the intended behavior), and gains nothing else.

## Tests (in jerrycan-realtime)
Add WS/hub tests (model on the existing `ws.rs`/`channel.rs` tests):
1. **The bug fix:** a resolver is installed (auth model present); an anonymous client (no credential) can JOIN a scope-`none` topic (was: 401 at upgrade). Assert the join succeeds and the client receives a scope-`none` broadcast.
2. **Scope-auth still enforced:** the same anonymous (None-principal) client is REJECTED joining a scope-`auth` topic ("authentication required") and a scope-`tenant` topic.
3. **Valid credential unchanged:** a resolver returning `Ok(principal)` still yields `Some(principal)` → scope-`auth`/`tenant` joins work as before.
4. **Non-auth error propagates:** a resolver returning a non-401 error (e.g. 500) still aborts the upgrade (does NOT silently become anonymous).
If a 401-vs-non-401 distinction can't be unit-tested at the `from_request` layer, at minimum test the hub/`scope_allows` path with a `None` principal + a resolver-error→None mapping unit.

## Gates
- `cargo test -p jerrycan-realtime` (the new WS tests) + `cargo test -p jerrycan` green.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` — reference-slice has realtime; confirm its realtime battery stays green (and that a scope-`none`/broadcast topic there is still reachable). Local PG container (`wal_level=logical`) available for the realtime changes leg.
- `cargo fmt`/`clippy -D warnings`; `cargo semver-checks` (internal realtime logic — no public API change).
- Byte-identity: this is a runtime-crate logic change only; the generated code (realtimegen wiring) is unchanged → every scaffolded app is byte-identical.

## Success criteria
- With an auth model installed, an anonymous WebSocket client can join a scope-`none` topic and receive its broadcasts (was: 401 at upgrade).
- Scope-`auth`/`tenant` topics still reject a `None`/anonymous principal (no escalation).
- A valid credential still authenticates; a non-auth server error still fails the upgrade.
- Heavy gate green; published 0.6.25; #117 closed.

## Non-goals
- Changing `scope_allows` or the topic-scope model. Re-authenticating mid-connection. A distinct client-facing signal for bad-vs-missing credential at the upgrade (both map to anonymous; the scope check gives the per-topic rejection).
