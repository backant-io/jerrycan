# Realtime: change-source recovery converges on every node (0.7.9) — #234

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 3 finding, #232 follow-up)
**Issue:** #234 — the #232 (0.7.8) multi-node health propagation is ASYMMETRIC. Outage onset (`ChangesHealth{true}`) is re-published every backoff iteration of the leader's `run_supervised` (`replication.rs:399-401`), so it survives ephemeral pub/sub loss. But recovery (`ChangesHealth{false}`) is published EXACTLY ONCE on the `true→false` swap transition (`replication.rs:361-363`) and never again — no healthy heartbeat, no snapshot-on-join. A follower's `changes_unavailable` is driven SOLELY by bus `ChangesHealth` (followers block in `LeaderGate::acquire`, never run the streaming path). A follower that misses the one-shot `false` — via a Redis pump reconnect (`bus_redis.rs`, pub/sub has no replay) or a fan-in `broadcast` `Lagged` drop (`lib.rs:539-541`, cap 1024, MOST likely at recovery when the leader floods backlogged `Change` events) — stays `changes_unavailable=true` FOREVER, answering JC0530 on `changes:` joins though the source is healthy and `Change` events flow on that node. Never self-heals.
**Ships as:** 0.7.9 — a `jerrycan-realtime` runtime change making recovery convergence match onset. Patch bump 0.7.8 → 0.7.9. Internal (`pub(crate)`) — confirm `cargo semver-checks` needs no update.

## The fix — bounded recovery convergence on every node
Two mechanisms; **(1) is REQUIRED** (the reliable floor, covers an idle-but-healthy source); (2) is a RECOMMENDED optimization (instant heal on activity). Implement (1), and (2) if it's clean.

### (1) REQUIRED — a healthy heartbeat re-publishes `ChangesHealth{false}`
While the leader is CONNECTED and streaming (the source is demonstrably healthy), periodically re-publish `ChangesHealth{false}` on a bounded interval (e.g. 30s — match the max backoff), so a follower that missed the one-shot `false` converges to admitting within one heartbeat interval. Implementation: the streaming happens inside `stream_once`'s event loop (`replication.rs`); add a `tokio::time::interval(HEARTBEAT)` branch to its `select!` that sends `false` on the `health` channel (which `lib.rs:676`/the forwarding task publishes as `ChangesHealth`). The heartbeat fires ONLY while connected (inside `stream_once`), so a down source never heartbeats "healthy". Keep it bounded (one message per interval per leader — no storm; only the leader streams). State the interval chosen.
- Symmetry: onset re-publishes `true` each backoff iteration (down source); recovery re-publishes `false` each heartbeat (healthy source). A follower converges to the leader's true state within `max(backoff, heartbeat)` regardless of which messages it missed.

### (2) RECOMMENDED — clear `changes_unavailable` on `Change` delivery
In `Hub::deliver` (`lib.rs:400`), on `BusMessage::Change`, also `self.changes_unavailable.store(false, Ordering::Relaxed)`: a delivered `Change` PROVES the leader read it from a healthy source, so any node receiving one is safe to admit. This heals a stuck follower on the next `Change` (instant on an active source), complementing the heartbeat (which covers an idle one). Zero extra bus traffic. Confirm this cannot wrongly clear a genuinely-unavailable state (a `Change` only exists if the source produced it — it cannot arrive while the source is down).

## Tests (NON-ignored)
- **Heartbeat re-publishes:** a test (or factored function) that while connected, `run_supervised`/`stream_once` emits `ChangesHealth{false}` on the heartbeat tick (not just once) — assert against a fake/local bus or the `health` channel receiver; drive at least two ticks. No live PG (fake the source or test the timer-branch logic in isolation).
- **(2) if implemented:** a NON-ignored test that `Hub::deliver(BusMessage::Change(ev))` clears a previously-set `changes_unavailable` (and a subsequent `changes:` join is admitted). RED if the clear is removed.
- **Convergence regression:** the #228 fail-loud (onset) tests + the #232 `ChangesHealth` deliver/round-trip tests still green; the recovery path still publishes on the real transition too (don't remove the swap-transition publish — the heartbeat is ADDITIONAL).
- **No storm:** confirm the heartbeat interval bounds recovery traffic (a comment + the interval constant; a healthy source publishes at most 1 `false`/interval).

## Gates
- `cargo test -p jerrycan-realtime` + `-p jerrycan` green; new heartbeat/deliver tests green.
- Heavy eval gate + the realtime CDC gate (publish.sh, local logical PG — the `replication`/`triggers` tests must still pass; the added heartbeat branch must not break them) green.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; `cargo semver-checks` (internal); determinism + embedded_sync.

## Version + success criteria
0.7.9. After a change-source recovery, EVERY node (leader + followers) converges to admitting `changes:` joins within a bounded interval, whether or not `Change` events flow — a follower that missed the one-shot `false` no longer stays stuck forever. Onset fail-loud (#228/#232) unchanged; no publish storm; single-node byte-identical. Published 0.7.9; #234 closed.

## Non-goals
- Followers querying health synchronously on connect (the heartbeat + optional clear-on-Change is the chosen mechanism). Changing the leader election, delivery partitioning, or the onset re-publish. A general node-health/gossip layer.
