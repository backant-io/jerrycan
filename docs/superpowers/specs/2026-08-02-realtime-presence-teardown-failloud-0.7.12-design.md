# Realtime: presence cleanup on slow-consumer drop + fail-loud on sustained post-connect outage (0.7.12) — #241 + #242

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 4 findings, realtime)
**Issues:** #241 (MEDIUM — a queue-full WS drop never publishes `PresenceClear` → permanent phantom online member), #242 (LOW — a permanent post-first-connect source death leaves `changes:` subscribers on a silent dead feed; #228/#234 residual).
**Ships as:** 0.7.12 — a `jerrycan-realtime` runtime fix. Patch bump 0.7.11 → 0.7.12. Internal (`pub(crate)`) — confirm `cargo semver-checks` needs no update.

## Part A (#241) — a dropped connection always runs presence teardown
A WS conn's outbound queue is bounded (`mpsc::channel(CONN_QUEUE=128)`, `lib.rs:210,244`). When it fills, `try_send` fails and the conn is removed DIRECTLY from `conns` — `send_to` (`lib.rs:268`) and `broadcast_presence_diff` → `drop_list` (`presence.rs:391-393`) — WITHOUT presence cleanup. The per-connection loop later calls `hub.disconnect(conn)` (`ws.rs:204`), but `presence_disconnect` (`presence.rs:272-274`) starts `let Some(sub) = conns.remove(&conn) else { return }` — the conn is already gone → early-return → the `PresenceClear` leaves for that conn's presence keys are NEVER published. `sweep` is node-granularity (the local node never expires — `touch_node` every tick), so the phantom persists until node restart / same-key LWW reconnect.

**Fix:** make every path that drops a connection publish its presence leaves. Prefer a single teardown funnel: instead of a bare `conns.remove(&cid)` in `send_to`/`broadcast_presence_diff`'s drop path, route through the SAME presence-clearing logic `presence_disconnect` uses (capture the removed `sub`'s owned presence keys and publish `PresenceClear` for each). Simplest robust shape: have the drop paths call a shared helper `drop_connection(cid)` that (a) removes from `conns` AND (b) publishes the presence leaves for the removed sub's keys (idempotent if later `disconnect` also runs — a second `PresenceClear` for an already-cleared key is a no-op). Ensure `presence_disconnect`/`disconnect` remain correct if the conn was already dropped (they already early-return safely). No double-panic, no lock re-entrancy.

**Test (NON-ignored):** a unit test that a connection owning a presence key, when dropped via the queue-full path (`send_to`/`broadcast_presence_diff` drop), results in a `PresenceClear` for its key (assert against the bus / presence map — the key is gone / a leave was published). RED if the drop path skips presence teardown.

## Part B (#242) — a SUSTAINED post-connect outage fails loud (bounded consecutive-failure threshold)
After a successful first connect, a PERMANENT mid-stream source death (slot dropped + unrecreatable, DB decommissioned) leaves `connected_ever = true` forever, so `run_supervised` retries every backoff iteration but NEVER re-sets `changes_unavailable` (`replication.rs:459-474`) → `changes:` joins are admitted to a silent dead feed indefinitely. Ordinary blips must STAY transient (a successful reconnect is common) — so:

**Fix:** track CONSECUTIVE `stream_once` failures since the last successful attach. After `MAX_CONSECUTIVE_FAILURES` (a bounded const — pick a value that tolerates ordinary reconnect blips but catches a sustained outage, e.g. ~5, over the backoff schedule = tens of seconds to a couple minutes) consecutive failures with NO successful reconnect, re-mark `changes_unavailable` (and publish `ChangesHealth{true}` so followers learn — #232 path) → `changes:` joins answer JC0530 instead of a silent dead feed. A subsequent successful attach resets the counter AND lifts the flag (the existing #234 lift-on-connect + heartbeat already re-admit). Keep retrying (don't stop the supervisor — the source may come back). This closes the gap without failing loud on ordinary network blips.

**Test (NON-ignored, no live PG):** a unit/factored-function test that N consecutive first-`stream_once`-failures-after-connect (inject a failing source, or test the counter logic in isolation) sets `changes_unavailable`; a success before N resets the counter (no false fail-loud). RED if the threshold logic is removed.

## Gates
- `cargo test -p jerrycan-realtime` + `-p jerrycan` green; the new presence + threshold tests green (and RED if their fix is reverted).
- Heavy eval gate + realtime CDC gate (publish.sh, local logical PG — the `stream_once` change must not break the replication/trigger tests; the threshold const must be large enough that the brief live test never trips it) green.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; `cargo semver-checks` (internal); determinism + embedded_sync.

## Version + success criteria
0.7.12. A WS connection dropped for a full outbound queue publishes its `PresenceClear` leaves (no phantom online member) (#241); a sustained post-connect change-source outage fails loud with JC0530 after a bounded consecutive-failure threshold instead of a silent dead feed, while ordinary reconnect blips stay transient (#242). Onset/recovery (#228/#234) + single-node unchanged; published 0.7.12; #241 + #242 closed. **Board → 0 → audit Round 5.**

## Non-goals
- Enlarging the outbound queue or changing backpressure policy (the fix is teardown-on-drop, not avoiding the drop). Node-level presence gossip. Changing the transient-reconnect behavior for blips under the threshold.
