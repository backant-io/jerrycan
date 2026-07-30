# Realtime

Realtime gives your app three things over one WebSocket connection at `/realtime`:

- **Changes** — clients get a live message whenever a row they are allowed to see is inserted, updated, or deleted.
- **Broadcast** — clients send short messages to each other on a named topic (a chat room, a "someone is typing" ping). Nothing is stored.
- **Presence** — a live "who is here" list per topic, with each client's own bit of state (a cursor position, a status).

You declare channels in `design.json`; the generator wires them and writes acceptance tests. Delivery is **scope-filtered**: a client only ever receives a change it could have fetched with a normal `GET`. A change to another tenant's row never reaches it.

## The `design.json` block (contract version 2)

```json
"realtime": {
  "changes": ["Lead"],
  "broadcast": [{ "name": "deal_room", "scope": "tenant" }],
  "presence":  [{ "name": "editors",   "scope": "tenant" }]
}
```

- `changes` — entity names whose row changes are subscribable. The generator marks their tables for change capture and filters delivery by the subscriber's tenant/owner. Requires a database and an active auth model.
- `broadcast` / `presence` — named topics. `scope` decides who may join and publish: `none` (anyone), `auth` (any signed-in user), `tenant` (members of a tenant; delivery is partitioned so one tenant never sees another's messages).

## What the wired extension looks like

The generated `crates/realtime` crate builds this for you; you never write it by hand. The shape:

```rust
use jerrycan::realtime::{ChangeChannelSpec, Realtime, TopicScope};

fn wire(db: jerrycan::db::Db) -> Realtime {
    Realtime::new(db)
        .changes(ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
            hidden_columns: vec![],
        })
        .broadcast("deal_room", TopicScope::Tenant)
        .presence("editors", TopicScope::Tenant)
}
# let _ = wire;
```

## The wire protocol

Every frame is one JSON object tagged by `op`. Channels are namespaced strings: `changes:{Entity}`, `broadcast:{name}`, `presence:{name}`. A join → event transcript:

```text
-> {"op":"join","channel":"changes:Lead","ref":1}
<- {"op":"joined","channel":"changes:Lead","ref":1}
   ... someone inserts a Lead in your tenant ...
<- {"op":"event","channel":"changes:Lead","payload":{"type":"insert","pk":"42","row":{"id":42,"phone":"..."}}}

-> {"op":"join","channel":"broadcast:deal_room","ref":2}
<- {"op":"joined","channel":"broadcast:deal_room","ref":2}
-> {"op":"publish","channel":"broadcast:deal_room","payload":{"msg":"hi"}}
   ... every OTHER member of deal_room in your tenant receives ...
<- {"op":"event","channel":"broadcast:deal_room","payload":{"msg":"hi"}}

-> {"op":"join","channel":"presence:editors","ref":3}
<- {"op":"joined","channel":"presence:editors","ref":3}
<- {"op":"presence_state","channel":"presence:editors","state":{"alice":{"cursor":1}}}
-> {"op":"track","channel":"presence:editors","state":{"cursor":7}}
<- {"op":"presence_diff","channel":"presence:editors","joins":{"you":{"cursor":7}},"leaves":{}}

-> {"op":"heartbeat","ref":9}
<- {"op":"heartbeat_ack","ref":9}
```

Errors never drop the connection — they come back as `{"op":"error","code":"JC0403",...}` with the offending `channel`/`ref`.

## Publishing from a REST handler

The common pattern is server-driven: a REST handler creates a row and wants every subscriber to see it immediately. Resolve `RealtimeHandle` as a dependency and call `publish(topic, payload)` — no WebSocket client, no round-trip.

```rust
use jerrycan::prelude::*;
use jerrycan::realtime::RealtimeHandle;

// The generator adds `_rt: Dep<RealtimeHandle>` to write handlers (and a stub
// comment with this one-liner) whenever the design declares a broadcast topic
// with scope `none` or `auth`.
async fn create_note(rt: Dep<RealtimeHandle>) -> Result<NoContent> {
    // ... the handler just wrote a Note ...
    rt.publish("events", serde_json::json!({ "type": "created", "id": 7 }))
        .await?;
    Ok(NoContent)
}
# let _ = create_note;
```

`publish` enforces the same gate as the client `publish` op: `topic` must name a **declared** broadcast topic, so an unknown name or a `changes`/`presence` channel returns a clear `Err` (**JC0404**), never a silent drop. A server publish carries no connection identity, so it is un-partitioned and reaches **every** subscriber of the topic — which means publishing to a `tenant`-scoped topic is a **JC0403** `Err` (delivering to all tenants would break the isolation the scope promises). Declare the topic scope `none` or `auth` to publish it from a handler. (Per-tenant server publishing lands with dynamic topics — tracked in the realtime roadmap.)

## Delivery is scope-filtered (you only receive what you could GET)

Every event passes the same tenant/owner check your REST endpoints use, **before** it leaves the server. If a row moves from tenant A to tenant B, the old tenant receives a `delete`-shaped event and the new tenant an `update` — nobody else sees anything. This is the security pillar; the generated acceptance tests include a cross-tenant negative control that fails if a change ever leaks.

## Two change sources, identical behavior

Realtime picks its change source automatically at startup and logs which one is active:

- **Logical replication** (preferred) — reads the database's write-ahead log directly. Self-maintaining and lowest-overhead. Needs `wal_level = logical` and a role that may replicate.
- **Triggers + LISTEN/NOTIFY** (automatic fallback) — works on a stock Postgres with no special settings. Slightly weaker delivery guarantee, same client behavior.

If replication was possible but the server isn't configured for it, you get diagnostic **JC0531** naming the one-time host fix: set `wal_level = logical` and grant `REPLICATION` to the app role, then restart Postgres. The app runs correctly either way.

## Requirements and limits

- **Changes need Postgres.** On a SQLite deployment a client joining a `changes:` channel gets **JC0530**. Broadcast and presence work without a database.
- **Browser auth uses `?token=`.** Browsers cannot set an `Authorization` header on a WebSocket, so JWT apps also accept the token as a `?token=` query parameter (it can appear in access logs — the same trade-off Supabase makes). Session-cookie apps need nothing special; the browser sends the cookie on the upgrade.
- **Multi-node** delivery lights up with the `realtime-redis` feature (mirrors `jobs-redis`): broadcast, presence, and replication-path changes fan out across nodes over Redis. The trigger path needs no Redis — every node listens and Postgres is the bus.
- **Delivery is live-UI (at-most-once).** A client that misses messages while disconnected refetches current state on reconnect; after a replication gap the server sends `{"op":"resync",...}` telling it to do so. Guaranteed replay is a planned enhancement.
