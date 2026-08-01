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
        // A tenant-owned entity is scoped to the tenant; a per-user entity
        // (`.owner_column(Some("user_id".to_string()))`) is scoped to its owner —
        // you only receive a change you could have GET'd.
        .changes(
            ChangeChannelSpec::new("Lead", "leads", "id")
                .tenant_column(Some("workspace_id".to_string())),
        )
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

`publish` enforces the same gate as the client `publish` op: `topic` must name a **declared** broadcast topic, so an unknown name or a `changes`/`presence` channel returns a clear `Err` (**JC0404**), never a silent drop. A server publish carries no connection identity, so it is un-partitioned and reaches **every** subscriber of the topic — which means publishing to a `tenant`-scoped topic via `publish` is a **JC0403** `Err` (delivering to all tenants would break the isolation the scope promises). Declare the topic scope `none` or `auth` to publish it with `publish`.

### Publishing to one tenant (`publish_to`)

For a `tenant`-scoped topic — where "workspace A's members see A's messages, not B's" — use `publish_to(tenant_id, topic, payload)`. It is the partitioned twin of `publish`: it stamps the event with the tenant, so delivery reaches **only that tenant's** sockets (a socket receives it exactly when its verified `principal.tenant_id` equals `tenant_id`). Another tenant can never receive it.

```rust
use jerrycan::prelude::*;
use jerrycan::realtime::RealtimeHandle;

// The generator adds `_rt: Dep<RealtimeHandle>` and a `publish_to` stub comment to a
// PATH-SCOPED tenant write handler (one that already takes `Dep<Tenant>`) whenever the
// design declares a `tenant`-scoped broadcast topic. In a real handler the tenant id is
// `_tenant.id()` (the membership-verified tenant the write acted on); here it is a param.
async fn after_write(rt: Dep<RealtimeHandle>, tenant_id: String) -> Result<()> {
    // ... the handler just wrote a Lead in this tenant ...
    rt.publish_to(&tenant_id, "deal_room", serde_json::json!({ "type": "created" }))
        .await?;
    Ok(())
}
# let _ = after_write;
```

The two methods are a clean duality: `publish` for `none`/`auth` topics, `publish_to` for `tenant` topics. `publish_to` on a `none`/`auth` topic is a **JC0403** `Err` (those are un-partitioned, so the tenant argument would be silently ignored — use `publish`), and an unknown topic is **JC0404** just like `publish`.

## Choosing a tenant on the WebSocket (`?tenant=`)

The socket's principal carries a **verified** tenant, and it is chosen at connect time:

- A user who belongs to **exactly one** tenant gets that tenant automatically — nothing to pass.
- A user in **several** tenants passes `?tenant=<id>` on the connect URL to pick which one the socket scopes to. The membership is **verified**: if the user is not a member of that tenant, the upgrade is refused (**403**) — a socket can never scope to a tenant the user is not in.
- A user in **no** tenant (or one who omits `?tenant=` while in several) connects with **no tenant**. They can join `none`/`auth` topics but not `tenant` topics (a `tenant` topic rejects a tenant-less principal at JOIN). This is why an account with zero memberships is no longer refused off `/realtime`.

`?tenant=` travels on the same query string as `?token=` (browsers cannot set headers on a WebSocket): `wss://…/realtime?token=<jwt>&tenant=<workspace-id>`.

## Delivery is scope-filtered (you only receive what you could GET)

Every event passes the same tenant/owner check your REST endpoints use, **before** it leaves the server. If a row moves from tenant A to tenant B, the old tenant receives a `delete`-shaped event and the new tenant an `update` — nobody else sees anything. This is the security pillar; the generated acceptance tests include a cross-tenant negative control that fails if a change ever leaks.

## Per-room isolation *within* a tenant

The security boundary is the **tenant**: a `tenant`-scoped topic plus `publish_to(tenant, …)` isolates one workspace's messages from another's, and that is enforced server-side. Splitting a tenant into finer per-room channels (a topic per chat thread, per document, per board) is **not** a security boundary — every recipient is already a member of the same tenant. So there is no dynamic-topic primitive; do per-room fan-out with a room tag in the payload and a client-side filter:

```text
// publish (server or client): tag the room
{"op":"publish","channel":"broadcast:deal_room","payload":{"room":"lead-42","msg":"hi"}}

// each client joins broadcast:deal_room once and ignores payloads whose room isn't theirs
```

Everyone in the tenant receives the frame; each client keeps only the rooms it cares about. True dynamic per-entity topics are a future ergonomic enhancement — they would not change what any tenant member is allowed to see.

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
