# jerrycan-realtime — Design Spec

**Date:** 2026-07-10
**Status:** Design **approved** (all review questions resolved 2026-07-10). Second of three: storage → **realtime** → migrator.
**Contract impact:** extends `design.json` **contract_version 2** (adds the top-level `realtime` block, alongside `storage`).
**Part of:** lossless Supabase migration program (see `jerrycan-supabase-migration-roadmap` memory). Designed with the migrator's needs as a hard input.

---

## Goal

A first-class realtime extension covering **all three** Supabase Realtime features — **Postgres Changes**, **Broadcast**, **Presence** — over WebSockets, modeled in `design.json`, generated + eval-gated, with **scope-filtered delivery** (RLS parity) as a hard security pillar. End state: a Supabase Realtime channel migrates into a jerrycan realtime channel that behaves the same and is tested.

## Non-goals (this spec)

- The migrator itself (separate spec).
- Realtime for SQLite deployments — realtime targets Postgres; a SQLite app that requests realtime gets a coded diagnostic (Postgres required).
- Guaranteed at-least-once delivery / full event replay in v1 (live-UI semantics; durability is a documented future enhancement — see Postgres Changes).

## Architecture overview

Same extension-crate shape as the rest:

- New crate `crates/jerrycan-realtime/` — modules: `lib.rs` (the realtime hub + `Realtime` dependency), `ws.rs` (upgrade + connection loop), `channel.rs` (subscription registry + scope filter), `changes/` (the two CDC source adapters), `broadcast.rs`, `presence.rs`, `bus.rs` (multi-node fan-out).
- Facade feature `realtime = ["dep:jerrycan-realtime", "db"]` — **realtime implies db** (Changes needs it). Multi-node behind `realtime-redis` (like `jobs-redis` / `rate-limit-redis`).
- Reserved `realtime` dependency recognized in `design.rs` (`has_realtime()`), surfaced in `facade_features()`, added to the reserved-name filter in `mounting.rs`.
- New generator module `realtimegen.rs` emits the publication DDL, `REPLICA IDENTITY` DDL, the NOTIFY triggers (fallback path), channel wiring, and tests.

**New third-party crates (honest count):** two — **`tokio-tungstenite`** (WS transport) and **`tokio-postgres`** (+rustls; the WAL replication socket only — sqlx stays the sole data-layer client, see Resolved #7). `redis` is already in the workspace.

## `design.json` contract (v2 addition): the `realtime` block

```json
"realtime": {
  "changes":   ["Message", "Order"],
  "broadcast": [ { "name": "room",    "scope": "tenant" } ],
  "presence":  [ { "name": "editors", "scope": "tenant" } ]
}
```

- `changes` — entity names whose row changes are subscribable. The generator marks their tables for publication + `REPLICA IDENTITY FULL`, and delivery is **scope-filtered** by the subscriber's owner/tenant (reuses tenancy). A change the subscriber couldn't `GET` is never delivered.
- `broadcast` — named ephemeral pub/sub topics; `scope` (`none`|`tenant`|`auth`) gates who may publish/subscribe.
- `presence` — named presence topics (online-state sync); same `scope`.

Validation (`questions.rs`, `contract_version >= 2`): `changes` entities must exist and imply `db`; any realtime block with a scoped channel implies `auth`; realtime + sqlite-only ⇒ diagnostic.

## WebSocket transport + channel protocol

- **Transport is ours** — hyper's native upgrade (HTTP 101) + **`tokio-tungstenite`** (the conservative standard; rides hyper's upgrade, rustls-friendly). No new HTTP stack — consistent with the storage/OAuth decision.
- One WS endpoint (`GET /realtime` upgrade) multiplexes all channels. A connection is authenticated at upgrade (session/JWT/api-key via the existing extractors), then carries a principal used for every scope check.
- **Channel protocol (decided): jerrycan-native** `join`/`leave`/`event`/`heartbeat` envelope, driven by a jerrycan realtime client the migrator repoints the frontend to (consistent with how REST migrates — jerrycan endpoints aren't PostgREST-wire-compatible either). A Supabase Phoenix-Channels **wire-compat adapter** (an unchanged `@supabase/realtime-js` connects directly) is a possible fast-follow if migration friction demands it.

## Postgres Changes — detect-replication-else-triggers

Two source adapters behind one `ChangeSource` trait; both feed the same scope-filtered fan-out. **The client sees identical behavior; only the source differs.** Selected automatically at startup.

### Primary: logical replication (self-maintaining)

Exact Supabase-style mechanism — logical decoding of the WAL via the **built-in `pgoutput` plugin** (no server-side plugin install, unlike `wal2json`). The WAL socket is opened with **`tokio-postgres`** (+rustls) — the one place sqlx can't reach; it carries only the replication stream, never the app's queries (sqlx stays the data-layer client, see Resolved #7).

- **Detection:** at startup, `SHOW wal_level`. If `logical` and the role can replicate → use this adapter. Else → fall back to triggers.
- **Self-maintaining, by construction:**
  - Slot + publication created **idempotently** (publication is generated DDL); `REPLICA IDENTITY FULL` on published tables (generated) so updates/deletes carry the *old* row — required to decide who was allowed to see a row whose tenant/owner just changed.
  - **`max_slot_wal_keep_size`** bounds WAL retention: Postgres invalidates the slot instead of filling the disk. Combined with continuous **LSN confirmation** (standby status updates), retained WAL stays minimal under normal operation.
  - A **supervised task** reconnects with backoff and resumes from the stored LSN; if the slot was invalidated (prolonged downtime), it **auto-recreates + reconciles** so subscribers converge. No operator involvement.
- **Honest residual:** under prolonged downtime the slot may invalidate → a resync gap where some changes are missed and subscribers refetch current state on reconnect (same failure mode Supabase has). Live-UI-acceptable.
- **Multi-node:** a replication slot is single-consumer, so exactly one node owns it — elected via a **Postgres advisory lock** (`pg_try_advisory_lock`); on that node's death the lock releases and another takes over (self-maintaining, no new infra). The leader publishes decoded changes to the **bus** (`realtime-redis`); all nodes scope-filter and deliver to their own WS clients.

### Fallback: triggers + LISTEN/NOTIFY

When `wal_level != logical` (or replication privilege is absent):

- Generated `AFTER INSERT/UPDATE/DELETE` trigger per published table → `pg_notify('jc_changes', table|pk|op|tenant_id)`. The app holds a `LISTEN` connection (sqlx `PgListener` — already in the tree, no new dep).
- **8KB NOTIFY limit** → the payload is table + pk + op (+ scope keys); the app refetches the row when the subscriber needs the body.
- **Multi-node comes free** — every node `LISTEN`s, Postgres is the bus, no Redis needed for Changes on this path.
- Weaker guarantee (fire-and-forget: lost only if *zero* instances are listening). Documented; the optional durability enhancement is a NOTIFY-on-outbox variant, deferred.

### Detection outcome is surfaced

`jerrycan check` / startup logs report which backend is active, and if replication was *expected* but unavailable, a **coded diagnostic** names the exact one-time host fix (set `wal_level=logical`) — so the operator can opt into full fidelity, but the app runs correctly either way.

## Broadcast

Ephemeral client-to-client pub/sub, no DB. The hub routes a message published to topic `T` to all subscribers of `T`, gated by the topic's `scope`. **Multi-node:** messages traverse the `realtime-redis` bus so a publisher on node A reaches subscribers on node B. Single-node needs no Redis.

## Presence

Per-topic online-state: each client sets a presence key + metadata; the hub maintains merged state and broadcasts join/leave **diffs** to subscribers. State is ephemeral (in-memory single-node; merged across nodes over the `realtime-redis` bus, keyed per topic, with last-writer-wins on a client's own key). The fiddliest feature; kept to Supabase's observable join/sync/leave semantics.

## Multi-node fan-out (the asymmetry, made explicit)

| Path | Single-node | Multi-node |
|---|---|---|
| Changes (replication) | leader = the one node | advisory-lock leader → Redis bus → all nodes |
| Changes (triggers) | node LISTENs | **every node LISTENs — Postgres is the bus, no Redis** |
| Broadcast / Presence | in-process | Redis bus |

`realtime-redis` is the single feature that lights up cross-node delivery, mirroring `jobs-redis`.

## Security pillar — scope-filtered delivery (mandatory)

Every event — Changes, Broadcast, Presence — is filtered by the subscriber's principal (owner/tenant) **before** it leaves the server, reusing the existing tenancy/guard model. Supabase applies RLS to realtime; so do we. A change to another tenant's row must never reach a subscriber. This is eval-gated with a **negative control** (a cross-tenant change that must *not* arrive turns the gate red).

## Supabase migration mapping (input to the migrator spec)

| Supabase | jerrycan-realtime |
|---|---|
| `postgres_changes` subscription on a table | entity in `realtime.changes` (published + scope-filtered) |
| Realtime RLS (row visibility) | scope-filtered delivery (owner/tenant) |
| `broadcast` topic | `realtime.broadcast[]` entry |
| `presence` topic | `realtime.presence[]` entry |
| logical-replication under the hood | replication adapter (or trigger fallback) — same client behavior |
| Phoenix-Channels client wire protocol | jerrycan client (default) **or** wire-compat adapter (open question) |

## Eval gate

The reference Supabase export gains realtime channels. The eval drives a real WS client: subscribe → mutate over HTTP → assert the scoped event arrives; **negative control** — a change to another tenant's row must not arrive. Broadcast + Presence round-trips asserted. Un-skippable in CI + pre-publish.

## Testing strategy

- Unit: `pgoutput` message decode against captured WAL frames; NOTIFY payload parse; scope filter (owner/tenant/prefix); presence diff merge.
- Integration: replication adapter against a `wal_level=logical` Postgres container; trigger adapter against a default Postgres; advisory-lock leader failover; `realtime-redis` two-node fan-out.
- Generated-app: per-channel subscribe/receive + the cross-tenant negative control.
- Docs: realtime `docs/ai` examples run in CI.

## Resolved decisions (review 2026-07-10)

1. **Scope:** all three features (Postgres Changes + Broadcast + Presence) in v1.
2. **Postgres Changes source:** **detect-replication-else-triggers** — logical replication (`pgoutput`) as the self-maintaining primary, triggers+`LISTEN/NOTIFY` as the automatic fallback. Identical client behavior.
3. **Self-maintaining replication:** idempotent slot/publication, `REPLICA IDENTITY FULL`, `max_slot_wal_keep_size` + continuous LSN confirmation, supervised reconnect + auto-recreate/reconcile, advisory-lock leader election.
4. **Transport:** hyper-native WS upgrade; no new HTTP stack.
5. **Multi-node:** `realtime-redis` bus; free on the trigger path (Postgres is the bus).
6. **Security:** scope-filtered delivery is mandatory and negative-control eval-gated.
7. **Replication client:** `tokio-postgres` (+rustls), confined to the replication adapter. **We keep sqlx** as the sole data-layer client — switching off sqlx would drop SQLite (`tokio-postgres` is Postgres-only), abandon the sea-orm/sea-query-binder layer, and break the generated-code contract (`jerrycan::db::sqlx::…`); not worth it to avoid one narrow dependency.
8. **Transport lib:** `tokio-tungstenite`.
9. **Channel protocol:** jerrycan-native; Supabase wire-compat adapter deferred to a fast-follow.
10. **Delivery:** match Supabase — no replay in v1 (at-most-once; client refetches on reconnect). Replay / guaranteed at-least-once is the first fast-follow.

## Deferred to fast-follow (not v1)

- **Supabase Phoenix-Channels wire-compat adapter** — lets an unchanged `@supabase/realtime-js` connect directly; scoped in if migration friction demands it.
- **Replay / guaranteed at-least-once delivery** — durable event-log + per-subscriber cursors; the first place jerrycan pulls ahead of Supabase on delivery.

All review questions resolved 2026-07-10.
