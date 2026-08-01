# Multi-node realtime: propagate changes-source health across nodes (0.7.8) — #232

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 2 follow-up to #228)
**Issue:** #232 — in a multi-node deployment the replication `changes_unavailable` fail-loud (#228, and the #212 trigger-DDL one) is applied ONLY by the elected leader's `run_supervised` (`lib.rs`), because only the leader streams from Postgres and publishes to the bus. Followers deliver from the bus and keep `changes_unavailable = false`, so when the leader can't stream (permanent mis-provisioning), FOLLOWER nodes still admit `changes:` subscribers to a feed that never delivers (the leader never publishes any `Change` to the bus). Single-node is fully covered by #228; this is the multi-node follower case.
**Ships as:** 0.7.8 — a `jerrycan-realtime` runtime change: a new bus control message that carries change-source health, published by the node that marks/lifts `changes_unavailable` and applied by every node. Patch bump 0.7.7 → 0.7.8. Internal (`pub(crate)`) — confirm `cargo semver-checks` requires no update.

## The architecture (confirmed)
- `BusMessage` (`crates/jerrycan-realtime/src/bus.rs:14`) is a `#[serde(tag="kind")]` enum: `Change`, `Broadcast`, `PresenceSet/Clear/Snapshot`, `Resync`. Every node PUBLISHes and every node's subscriber RECEIVEs (`bus_redis.rs:2`); `Hub::deliver` (`lib.rs:400`) dispatches each variant to the local hub.
- `changes_unavailable: Arc<AtomicBool>` is PER-HUB (`lib.rs:234`), checked in `Hub::join` (`lib.rs:323`). The #228/#212 stores are at `lib.rs:654`/`660`/`675`(passed into `run_supervised`)/`705`.

## The change
1. **New bus variant:** add `BusMessage::ChangesHealth { unavailable: bool }` (name to match the enum's snake_case convention → `changes_health`) to `bus.rs`. It must serde round-trip (extend the existing round-trip test in `bus.rs`).
2. **Publish on transition:** wherever a node sets `changes_unavailable` to a NEW value (the #212 trigger-DDL branch, the #228 replication first-connect-failure mark, AND the #228 lift on successful attach), publish `BusMessage::ChangesHealth { unavailable }` on the bus so other nodes learn. Publish ONLY on an actual transition (store returns/compare the prior value, or guard with `swap`) to avoid a publish storm on every `run_supervised` retry — BUT see late-joiner handling below. Prefer: publish on every mark in `run_supervised`'s loop is acceptable IF throttled to the backoff cadence (the loop already backs off), so a follower that joins mid-outage gets the next re-publish within one backoff cycle. State the choice; the requirement is (a) a transition reaches other nodes promptly and (b) no unbounded publish storm.
3. **Apply on receive:** in `Hub::deliver` (`lib.rs:400`), handle `BusMessage::ChangesHealth { unavailable }` → `self.changes_unavailable.store(unavailable, Ordering::Relaxed)`. So a follower's `join` (`lib.rs:323`) now answers JC0530 when the leader reports the source is down, and re-admits when it recovers.
4. **Local-bus / single-node:** publishing `ChangesHealth` on the in-process `LocalBus`/`AnyBus` loops back to the same node (harmless — it re-applies the same value the node already set). Single-node behavior is unchanged (the direct store already covers it; the bus echo is idempotent). Confirm the single-node path stays byte-identical in behavior.

## Late-joiner (state the handling)
Redis pub/sub is ephemeral: a follower that connects AFTER a `ChangesHealth{true}` was published misses it. Handle with the leader's periodic re-publish: `run_supervised` retries the failed stream on a backoff loop and re-marks unavailable each iteration — publish `ChangesHealth{true}` on each such iteration (throttled by the existing backoff), so a late-joining follower converges within one backoff cycle. (A stronger design — followers query current health on connect — is out of scope; document the convergence-within-one-backoff-cycle behavior.)

## Tests
- **bus round-trip:** `ChangesHealth` serde round-trips (extend the `bus.rs` test).
- **deliver applies it:** a unit test that `Hub::deliver(BusMessage::ChangesHealth{unavailable:true})` sets the hub's `changes_unavailable` (and a subsequent `changes:` `join` is refused JC0530); `{unavailable:false}` re-admits. This is a NON-ignored test that turns red if `deliver` ignores the variant.
- **publish on transition:** a test (or factored pure function) that the mark/lift path emits a `ChangesHealth` bus publish on transition (no live PG needed — assert against a fake/local bus).
- Existing realtime + `bus_redis` (heavy) tests green; the #228 non-ignored fail-loud tests still green.

## Gates
- `cargo test -p jerrycan-realtime` + `-p jerrycan` green; the new deliver/round-trip tests green.
- Heavy eval gate (`reference_eval`/`conformance`/`eval`/`genroute_compile --include-ignored`) + the realtime CDC gate (publish.sh, local logical PG) green.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; `cargo semver-checks` (internal — no update expected); determinism + embedded_sync.

## Version + success criteria
0.7.8. When the replication/trigger leader marks the change source unavailable, EVERY node's `changes:` join answers JC0530 (no follower dead feed), converging on recovery too; single-node behavior byte-identical. Published 0.7.8; #232 closed.

## Non-goals
- Followers querying health on connect (periodic re-publish + backoff convergence is the chosen mechanism). Any change to the leader-election or the delivery partitioning. A general node-health/gossip layer.
