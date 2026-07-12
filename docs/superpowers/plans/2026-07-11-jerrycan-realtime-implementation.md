# jerrycan-realtime Implementation Plan

**Goal:** a first-class realtime extension — Postgres Changes (logical replication primary, trigger/LISTEN-NOTIFY fallback), Broadcast, and Presence over hyper-native WebSockets — modeled in `design.json` contract v2, generated + eval-gated, with mandatory scope-filtered delivery.

**Architecture:** a new extension crate `crates/jerrycan-realtime` (same shape as `jerrycan-ratelimit`/`jerrycan-jobs`) hosts a connection hub behind one `GET /realtime` upgrade endpoint; two CDC source adapters feed one scope-filtered fan-out through a `Bus` (in-process default, Redis behind `realtime-redis`). The platform side adds the `realtime` block to contract v2 (extending what jerrycan-storage established), a `realtimegen.rs` generator emitting a tool-owned `crates/realtime/` wiring crate, and validation/mounting/facade plumbing mirroring `jobs`.

**Tech stack:** hyper HTTP/1 upgrade (`.with_upgrades()`, new in core) + `tokio-tungstenite 0.30` (no TLS features — it rides hyper's already-established socket); the **`pgwire-replication`** crate (dedicated logical-replication client: `ReplicationClient`, rustls TLS + SCRAM, pgoutput) for the WAL socket; sqlx `PgListener` (already in tree) for the trigger fallback; the existing workspace `redis` for the multi-node bus. sqlx/sea-orm stays the sole data-layer client.

> **⚠ REPLICATION-CLIENT OVERRIDE — AUTHORITATIVE (supersedes any `postgres-protocol` hand-roll or `src/changes/wire.rs` described in the tasks below, incl. Resolved-#1 and Tasks 14–15):** Use the **`pgwire-replication`** crate as the dedicated replication client. Replace any hand-rolled replication socket with `ReplicationClient::connect(ReplicationConfig { host, port, user, password, database, slot, publication, start_lsn, tls })` (enable its `tls-rustls` + `scram` features), stream changes via `client.recv().await → ReplicationEvent` (pgoutput-decoded: `XLogData` / `Begin` / `Commit` / `KeepAlive`), and advance the slot with `client.update_applied_lsn(wal_end)`. Do **NOT** create `wire.rs` or depend on `postgres-protocol` — pgwire-replication owns the socket / startup / SCRAM / TLS / CopyBoth framing, so that work simply disappears. sqlx stays the data-layer client. Everything else in this plan (triggers+`LISTEN/NOTIFY` fallback, scope-filtered delivery, Broadcast, Presence, `realtime-redis` bus, generator, tests) is unchanged.

> **For agentic workers:** this is both the plan and the spec companion (spec:
> `docs/superpowers/specs/2026-07-10-jerrycan-realtime-design.md`). Implement
> strictly TDD, plain commit messages (what changed — never any AI/Claude/
> Co-Authored-By lines), workspace green after every task. Live-service tests
> are `#[ignore]`d with the exact run command in the file header, exactly like
> `jerrycan-jobs/tests/redis_store.rs`.

---

## COORDINATION — build-order AFTER jerrycan-storage

This plan **extends** the contract_version 2 + reserved-dependency plumbing that
the jerrycan-storage plan establishes. **Do NOT re-introduce contract_version 2.**
Preconditions this plan assumes are already on `main` (verify before Task 20;
if any is missing, STOP and surface — do not re-implement them here):

1. `questions.rs` accepts `contract_version == 2` (the `> 1` guard became `> 2`).
2. `docs/contracts/design-schema.json` `contract_version.enum` is `[0, 1, 2]`
   and the `design.rs` test `published_schema_accepts_v1_constructs` asserts it.
3. `design.rs` carries `storage: Option<...>` + `wants_storage()` and
   `facade_features()` appends `"storage"` after `"oauth"`.
4. `mounting.rs`'s reserved-name filter includes `"storage"`.

The `realtime` reserved dep is added **alongside** `storage` using the identical
pattern: `design.rs` `wants_realtime()` + `facade_features()`, `mounting.rs`
reserved-name filter + main-wiring, `questions.rs` v2-gated validation.

---

## Resolved ambiguities (decided here, with reasons)

1. **`tokio-postgres` cannot open a replication session — spec corrected.**
   Verified against docs.rs on 2026-07-11: mainline `tokio-postgres` 0.7.18 has
   **no** `Config::replication_mode`, **no** generic `Config::param`, and **no**
   `Client::copy_both_simple` — the CopyBoth/replication PR was never merged
   upstream, and every project that does this (Materialize, Supabase etl) pins a
   **git fork**, which jerrycan cannot do (crates.io publishing forbids git
   deps). Resolution: keep the spec's *intent* (a narrow rust-postgres-family
   dep confined to the replication adapter; sqlx stays the data layer) but use
   **`postgres-protocol 0.6`** (same sfackler/rust-postgres repo, published,
   maintained) and hand-roll the one replication socket: TCP + optional rustls
   (SSLRequest preamble), `startup_message` with `replication=database`
   (postgres-protocol accepts arbitrary startup params), SCRAM/MD5/cleartext
   auth via `postgres_protocol::authentication`, simple-query, and CopyBoth
   framing (~400 lines, unit-tested; the framing is `tag(1) + len(4) + body`).
   This mirrors the storage decision to hand-roll SigV4 over hyper instead of
   pulling aws-sdk. Honest new-crate count stays **two**: `tokio-tungstenite`
   and `postgres-protocol`.
2. **Realtime DDL is applied idempotently at startup, not via migrations.**
   Module migrations require sqlite twins and the DDL set (publication +
   REPLICA IDENTITY vs triggers) depends on the source adapter chosen at
   runtime. The adapters reconcile their own DDL through the `Db` handle on
   every startup (`CREATE OR REPLACE FUNCTION`, `DROP TRIGGER IF EXISTS` +
   `CREATE TRIGGER`, publication create-or-`SET TABLE`). This is the literal
   reading of "self-maintaining, by construction". `realtimegen.rs` still emits
   the DDL *inputs* (table/pk/tenant-column specs) into the generated wiring.
3. **`Db` gains a `url()` accessor** (Task 3). The realtime extension needs the
   database URL for three extension-held sessions the sea-orm pool cannot
   provide (sqlx `PgListener`, the replication socket, the advisory-lock leader
   session); `Db` currently discards it.
4. **Principal is normalized to strings** (`user_id: String`,
   `tenant_id: Option<String>`, `role: Option<String>`). Tenant pks vary
   (i64/String) per design; change events extract scope keys as text
   (`::text` in triggers, text-format pgoutput tuples), so string comparison is
   the one uniform, allocation-cheap filter. Principal *resolution* is a
   generated closure (only the generated app knows `shared::SessionUser` /
   `shared::Tenant`), passed to `Realtime::principal(...)`.
5. **Browser JWT connections authenticate via `?token=`** — browsers cannot set
   an `Authorization` header on a WebSocket. The generated jwt-model resolver
   tries the Bearer header first (non-browser clients), then the `token` query
   parameter (`jerrycan::auth::jwt::decode` + `Auth::jwt_key()` are public).
   Session-cookie designs need nothing special (browsers send cookies on
   upgrade). Documented in `docs/ai/18-realtime.md` (query tokens can appear in
   access logs — same trade-off Supabase makes).
6. **`wants_realtime()` gates on the `realtime` block** (like `wants_jobs()` gates
   on `jobs`), not on a `dependencies` entry. The *name* `realtime` still joins
   the reserved-name filter in `mounting.rs` so a stray `dependencies:
   ["realtime"]` never generates a stub comment.
7. **`max_slot_wal_keep_size` is detected and surfaced, never set.** Setting it
   requires `ALTER SYSTEM` (superuser). At startup the replication adapter runs
   `SHOW max_slot_wal_keep_size`; `-1` (unbounded) logs the JC0531 diagnostic
   naming the exact fix. Slot invalidation under a bounded setting is handled
   (auto-recreate + `resync` envelope).
8. **Changes scope rule:** every `changes` subscription requires an
   authenticated principal (validation enforces an active auth model). When the
   entity is tenant-owned (`belongs_to` the tenancy entity), delivery is
   filtered on the tenant fk column; an update that *moves* a row across
   tenants delivers an `update` to the new tenant and a `delete` to the old
   (this is exactly why REPLICA IDENTITY FULL / OLD-row triggers exist). A
   non-tenant-owned entity delivers to all authenticated subscribers.
9. **Broadcast/Presence run without Postgres; Changes require it.** JC0530
   (realtime requires Postgres) is emitted at startup *and* as the error
   envelope when a client joins a `changes:` channel on a sqlite deployment.
   This keeps the loopback WS tests (broadcast/presence) free of any external
   service.
10. **Trigger-path delivery bypasses the bus** (every node LISTENs — Postgres
    is the bus); replication-path delivery always goes leader → bus → all nodes
    (including the leader itself, for one uniform delivery path). Broadcast/
    presence always go through the bus (LocalBus in-process). This prevents
    N-times duplication if `realtime-redis` is on while the trigger path runs.
11. **Wire protocol pinned concretely** (spec only named the ops) — see
    `protocol.rs` in Task 4. Channels are namespaced strings:
    `changes:{Entity}`, `broadcast:{name}`, `presence:{name}`.
12. **Slow consumers are disconnected** (bounded per-connection queue of 128;
    `try_send` full ⇒ drop the connection). Matches at-most-once/live-UI
    semantics; a reconnecting client refetches.
13. **No local LSN persistence.** `START_REPLICATION` at `0/0` resumes from the
    slot's server-side `confirmed_flush_lsn` (Postgres takes the max), which the
    adapter advances via standby status updates. Restart-safe with zero state.
14. **TLS on the replication socket:** if the server declines SSL (`N`) →
    plaintext; if it accepts (`S`) → rustls with webpki roots, and a chain
    failure is a hard error (no silent downgrade). Private-CA servers are a
    documented v1 limitation (same stance as the oauth client).
15. **Generated realtime acceptance tests are `#[ignore]`d live-Postgres
    tests** (a sqlite `TestApp` cannot run Changes). The eval battery runs them
    against a `wal_level=logical` container; the cross-tenant negative control
    lives there AND as service-free unit tests in `channel.rs`.

---

## File Structure

```
Cargo.toml                                    — workspace: + member, + jerrycan-realtime/tokio-tungstenite/postgres-protocol deps
crates/jerrycan-realtime/
  Cargo.toml                                  — deps + `realtime-redis` feature
  src/lib.rs                                  — Realtime builder/extension, RealtimeHandle, Principal, supervisor (bus pump, source select, presence sweep)
  src/protocol.rs                             — the jerrycan-native WS envelope (ClientMsg/ServerMsg, serde)
  src/ws.rs                                   — handshake validation + Sec-WebSocket-Accept, WsStart extractor, per-connection loop
  src/channel.rs                              — ChannelId, subscription registry, MANDATORY scope filter (join + delivery)
  src/bus.rs                                  — BusMessage, LocalBus, AnyBus (+ RedisBus behind `realtime-redis`)
  src/broadcast.rs                            — ephemeral pub/sub semantics (publish gate, tenant partitioning)
  src/presence.rs                             — presence map, join/sync/leave diffs, cross-node merge + node-expiry sweep
  src/changes/mod.rs                          — ChangeEvent/ChangeOp/ChangeSource, detection queries, DDL templates, NOTIFY payload
  src/changes/pgoutput.rs                     — Lsn, XLogData/keepalive frames, standby-status encode, pgoutput message decode (pure, DB-free)
  src/changes/wire.rs                         — the hand-rolled replication socket (framing, startup, auth, TLS preamble, simple query, CopyBoth)
  src/changes/replication.rs                  — logical-replication adapter: slot mgmt, stream loop, LSN confirm, supervisor, advisory-lock leader
  src/changes/triggers.rs                     — trigger fallback: DDL reconcile, PgListener loop, row refetch
  tests/ws_live.rs                            — loopback WS integration (join/heartbeat/broadcast/presence — NO external services)
  tests/changes_pg.rs                         — trigger-path live tests (#[ignore], any Postgres)
  tests/replication_pg.rs                     — replication live tests (#[ignore], wal_level=logical Postgres)
  tests/redis_bus.rs                          — Redis bus live tests (#[ignore], feature realtime-redis)
crates/jerrycan-core/src/serve.rs             — `.with_upgrades()` on the hyper connection
crates/jerrycan-core/src/extract.rs           — `RequestCtx::take_extension<T>()`
crates/jerrycan-core/tests/upgrade.rs         — raw HTTP/1 upgrade round-trip test
crates/jerrycan-db/src/lib.rs                 — `Db::url()`
crates/jerrycan/Cargo.toml                    — `realtime` / `realtime-redis` facade features
crates/jerrycan/src/lib.rs                    — `pub use jerrycan_realtime as realtime` + doc_page gate
crates/jerrycan/src/platform/design.rs        — RealtimeDesign model, wants_realtime(), facade_features()
crates/jerrycan/src/platform/questions.rs     — v2-gated realtime validation
crates/jerrycan/src/platform/codes.rs         — JC0530, JC0531
crates/jerrycan/src/platform/mounting.rs      — reserved filter, main wiring, members/route-deps, regenerate hook
crates/jerrycan/src/platform/realtimegen.rs   — the generated tool-owned crates/realtime/ wiring crate + acceptance tests
crates/jerrycan/src/platform/mod.rs           — `pub mod realtimegen;`
docs/contracts/design-schema.json             — the `realtime` block
docs/ai/18-realtime.md (+ crates/jerrycan/embedded/ai/18-realtime.md)
docs/ai/13-error-codes.md (+ embedded twin)   — JC0530/JC0531 rows
crates/jerrycan/src/platform/docsidx.rs       — ("realtime", 18-realtime.md) page
conformance/designs/reference-slice.design.json — realtime block for the eval gate
conformance/eval/PROTOCOL.md + specs          — WS eval steps incl. the negative control
```

---

## Task 1: workspace deps + `jerrycan-realtime` crate skeleton

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/Cargo.toml`
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/Cargo.toml`
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs`
- Test: inline `#[cfg(test)]` in `src/lib.rs`

- [ ] 1. Write the failing test first — the builder must hold the design-shaped config. In the (not-yet-existing) `crates/jerrycan-realtime/src/lib.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The builder collects the generated wiring's channel specs verbatim —
    /// realtimegen (platform side) emits exactly these calls, so the shape is
    /// the crate's public contract.
    #[test]
    fn builder_collects_channel_specs() {
        let rt = Realtime::builder()
            .changes(ChangeChannelSpec {
                entity: "Lead".into(),
                table: "lead".into(),
                pk_column: "id".into(),
                tenant_column: Some("workspace_id".into()),
            })
            .broadcast("room", TopicScope::Tenant)
            .presence("editors", TopicScope::Tenant)
            .mount("/rt");
        assert_eq!(rt.config.changes.len(), 1);
        assert_eq!(rt.config.changes[0].entity, "Lead");
        assert_eq!(rt.config.broadcast, vec![("room".to_string(), TopicScope::Tenant)]);
        assert_eq!(rt.config.presence, vec![("editors".to_string(), TopicScope::Tenant)]);
        assert_eq!(rt.mount, "/rt");
    }

    /// The default mount is /realtime (one endpoint multiplexes all channels).
    #[test]
    fn default_mount_is_realtime() {
        assert_eq!(Realtime::builder().mount, "/realtime");
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime` — expected FAIL: package does not exist / nothing compiles.
- [ ] 3. Implement. Workspace `Cargo.toml`: add `"crates/jerrycan-realtime"` to `members` (after `jerrycan-jobs`) and to `[workspace.dependencies]`:

```toml
jerrycan-realtime = { path = "crates/jerrycan-realtime", version = "0.2.0" }
# Realtime WS transport. No TLS features: server-side it rides hyper's already-
# accepted socket; the test client connects plain ws:// on loopback. Verified
# 0.30.0 current on docs.rs 2026-07-11 (from_raw_socket + inherent send +
# handshake::derive_accept_key are the APIs we use, stable since 0.24).
tokio-tungstenite = { version = "0.30", default-features = false, features = ["handshake", "connect"] }
# The replication-socket protocol crate (rust-postgres family). Mainline
# tokio-postgres has NO replication support (verified 0.7.18); we speak the
# wire protocol ourselves over tokio TCP + rustls — see the realtime plan.
postgres-protocol = "0.6"
```

`crates/jerrycan-realtime/Cargo.toml` (mirrors jerrycan-jobs' shape):

```toml
[package]
name = "jerrycan-realtime"
description = "Realtime extension for the jerrycan framework: WebSocket channels for Postgres Changes (logical replication with trigger fallback), Broadcast, and Presence, with scope-filtered delivery. https://jerrycan.cc"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true
repository.workspace = true
authors.workspace = true
rust-version.workspace = true
keywords = ["realtime", "websocket", "cdc", "pubsub", "presence"]
categories = ["web-programming", "asynchronous"]

[dependencies]
jerrycan-core = { workspace = true }
jerrycan-db = { workspace = true }
serde.workspace = true
serde_json.workspace = true
bytes.workspace = true
http.workspace = true
hyper = { workspace = true }
hyper-util = { workspace = true }
futures-core.workspace = true
tokio = { workspace = true, features = ["time", "sync", "rt", "net", "io-util"] }
tokio-tungstenite = { workspace = true }
# Replication socket only — sqlx/sea-orm stays the sole data-layer client.
postgres-protocol = { workspace = true }
rustls = { workspace = true }
webpki-roots = { workspace = true }
tokio-rustls = { version = "0.26", default-features = false, features = ["ring"] }
# PgListener (trigger fallback) + the dedicated advisory-lock session.
sqlx = { workspace = true }
# The multi-node fan-out bus (behind `realtime-redis`, like jobs-redis).
redis = { workspace = true, optional = true }

[features]
realtime-redis = ["dep:redis"]

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync", "net", "io-util"] }
```

(`tokio-rustls` is the async wrapper rustls itself recommends; pin it in the
workspace `[workspace.dependencies]` as `tokio-rustls = { version = "0.26",
default-features = false, features = ["ring"] }` and reference it here as
`workspace = true` — it is glue over the already-vendored rustls, not a new
stack.)

`src/lib.rs` initial content:

```rust
//! Realtime extension for jerrycan: Postgres Changes + Broadcast + Presence
//! over one WebSocket endpoint, with mandatory scope-filtered delivery.
//! <https://jerrycan.cc>
#![forbid(unsafe_code)]

/// The authenticated identity a connection carries for every scope check.
/// All keys are strings: tenant pks vary per design (i64/uuid/text), and both
/// CDC paths extract scope columns as text, so string equality is the one
/// uniform filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub user_id: String,
    pub tenant_id: Option<String>,
    pub role: Option<String>,
}

/// Who may join/publish on a broadcast/presence topic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopicScope {
    /// No principal required (public topic).
    None,
    /// Any authenticated principal.
    Auth,
    /// Principals with a tenant; delivery is partitioned per tenant.
    Tenant,
}

/// One subscribable entity: the generated wiring supplies table/pk/tenant
/// column so the adapters can build DDL and extract scope keys.
#[derive(Clone, Debug)]
pub struct ChangeChannelSpec {
    pub entity: String,
    pub table: String,
    pub pk_column: String,
    /// The tenant fk column when the entity is tenant-owned; None ⇒ delivery
    /// to every authenticated subscriber.
    pub tenant_column: Option<String>,
}

/// The hub's static channel configuration (from the design, via realtimegen).
#[derive(Clone, Debug, Default)]
pub struct RealtimeConfig {
    pub changes: Vec<ChangeChannelSpec>,
    pub broadcast: Vec<(String, TopicScope)>,
    pub presence: Vec<(String, TopicScope)>,
}

/// The realtime extension builder. `Realtime::new(db)` in real wiring;
/// `Realtime::builder()` builds config without a database (unit tests).
pub struct Realtime {
    pub(crate) db: Option<jerrycan_db::Db>,
    pub(crate) mount: String,
    pub(crate) config: RealtimeConfig,
}

impl Realtime {
    pub fn new(db: jerrycan_db::Db) -> Self {
        Self { db: Some(db), ..Self::builder() }
    }

    pub fn builder() -> Self {
        Self { db: None, mount: "/realtime".into(), config: RealtimeConfig::default() }
    }

    pub fn mount(mut self, path: &str) -> Self {
        self.mount = path.to_string();
        self
    }

    pub fn changes(mut self, spec: ChangeChannelSpec) -> Self {
        self.config.changes.push(spec);
        self
    }

    pub fn broadcast(mut self, name: &str, scope: TopicScope) -> Self {
        self.config.broadcast.push((name.to_string(), scope));
        self
    }

    pub fn presence(mut self, name: &str, scope: TopicScope) -> Self {
        self.config.presence.push((name.to_string(), scope));
        self
    }
}
```

- [ ] 4. Run `cargo test -p jerrycan-realtime` — expected PASS. Run `cargo check --workspace` — green.
- [ ] 5. Commit: `Add jerrycan-realtime crate skeleton and workspace deps (tokio-tungstenite, postgres-protocol)`

---

## Task 2: jerrycan-core HTTP/1 upgrade support

The serve engine currently builds `serve_connection(io, service)` without
`.with_upgrades()`, so hyper never performs the 101 protocol switch, and
`RequestCtx` gives handlers no way to take the `hyper::upgrade::OnUpgrade`
handle out of the request extensions. Both are prerequisites for any WS work.

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-core/src/serve.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-core/src/extract.rs`
- Test: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-core/tests/upgrade.rs` (new)

- [ ] 1. Write the failing integration test:

```rust
//! HTTP/1 upgrade support: a handler can take hyper's OnUpgrade out of the
//! request, reply 101, and speak a raw protocol on the upgraded socket.
//! jerrycan-realtime's WebSocket transport rides exactly this seam, so this
//! test is the core-level contract (no tungstenite here — raw bytes).
use jerrycan_core::{App, Error, FromRequest, IntoResponse, RequestCtx, Response, Result, get};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Extractor that claims the upgrade handle (it is single-use and !Clone,
/// hence take, not get).
struct TakeUpgrade(hyper::upgrade::OnUpgrade);

impl FromRequest for TakeUpgrade {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        ctx.take_extension::<hyper::upgrade::OnUpgrade>()
            .map(TakeUpgrade)
            .ok_or_else(|| Error::internal("connection does not support upgrades"))
    }
}

async fn upgrade_echo(up: TakeUpgrade) -> Result<Response> {
    tokio::spawn(async move {
        if let Ok(upgraded) = up.0.await {
            let mut io = hyper_util::rt::TokioIo::new(upgraded);
            let mut buf = [0u8; 5];
            if io.read_exact(&mut buf).await.is_ok() {
                let _ = io.write_all(&buf).await;
            }
        }
    });
    let mut res = "".into_response();
    *res.status_mut() = jerrycan_core::http::StatusCode::SWITCHING_PROTOCOLS;
    res.headers_mut().insert(
        jerrycan_core::http::header::CONNECTION,
        jerrycan_core::http::HeaderValue::from_static("upgrade"),
    );
    res.headers_mut().insert(
        jerrycan_core::http::header::UPGRADE,
        jerrycan_core::http::HeaderValue::from_static("echo"),
    );
    Ok(res)
}

#[tokio::test]
async fn handler_upgrades_and_speaks_raw_bytes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = App::new().route("/up", get(upgrade_echo));
    let server = tokio::spawn(app.serve_with_shutdown(listener, async {
        let _ = rx.await;
    }));

    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /up HTTP/1.1\r\nHost: t\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n")
        .await
        .unwrap();
    // Read the response head (headers end at CRLFCRLF).
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
        assert!(head.len() < 4096, "response head too large");
    }
    let head = String::from_utf8_lossy(&head);
    assert!(head.starts_with("HTTP/1.1 101"), "expected 101, got: {head}");

    // The socket now speaks the raw echo protocol.
    s.write_all(b"hello").await.unwrap();
    let mut echo = [0u8; 5];
    s.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"hello");

    let _ = tx.send(());
    let _ = server.await;
}
```

- [ ] 2. Run `cargo test -p jerrycan-core --test upgrade` — expected FAIL: no method `take_extension` on `RequestCtx` (compile error).
- [ ] 3. Implement. In `extract.rs`, on `impl RequestCtx` (next to `peer_addr`):

```rust
/// Remove a typed extension from the request parts. jerrycan-realtime takes
/// hyper's `OnUpgrade` handle this way to run a WebSocket after replying 101.
/// Remove-not-get: the handle is single-use and `!Clone`.
pub fn take_extension<T: Send + Sync + 'static>(&mut self) -> Option<T> {
    self.parts.extensions.remove::<T>()
}
```

In `serve.rs`, chain `.with_upgrades()` onto the connection (hyper only
performs the 101 switch on an `UpgradeableConnection`; its
`graceful_shutdown` has the same signature, so the drain loop is unchanged):

```rust
let conn = hyper::server::conn::http1::Builder::new()
    .timer(hyper_util::rt::TokioTimer::new())
    .header_read_timeout(HEADER_READ_TIMEOUT)
    .serve_connection(io, service)
    .with_upgrades();
```

- [ ] 4. Run `cargo test -p jerrycan-core --test upgrade` — expected PASS. Run `cargo test -p jerrycan-core` — all prior tests still green (the upgrade wrapper is behavior-transparent for non-upgrade responses).
- [ ] 5. Commit: `core: enable HTTP/1 upgrades and add RequestCtx::take_extension`

---

## Task 3: `Db::url()` accessor

The realtime extension holds three sessions the sea-orm pool cannot provide
(PgListener, the replication socket, the advisory-lock leader connection) and
`Db` currently discards the URL it connected with.

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-db/src/lib.rs`

- [ ] 1. Failing test (in jerrycan-db's existing tests module):

```rust
#[tokio::test]
async fn db_exposes_its_connection_url() {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    assert_eq!(db.url(), "sqlite::memory:");
}
```

- [ ] 2. Run `cargo test -p jerrycan-db db_exposes_its_connection_url` — expected FAIL: no method `url`.
- [ ] 3. Implement: add `url: String` to `struct Db`, set it in `connect` (`url: url.to_string()` — note `Db` derives `Clone`; `String` clones fine), and:

```rust
/// The URL this handle connected with. Extension crates (jerrycan-realtime)
/// use it to open sessions the pool cannot serve: LISTEN connections, the
/// replication socket, and long-held advisory-lock sessions.
pub fn url(&self) -> &str {
    &self.url
}
```

- [ ] 4. Run `cargo test -p jerrycan-db` — expected PASS.
- [ ] 5. Commit: `db: expose the connection url for extension-held sessions`

---

## Task 4: the wire protocol (`protocol.rs`)

The jerrycan-native envelope (spec decision #9). Concrete shape, pinned by
serde round-trip tests — the realtime client and the migrator both build
against these exact bytes.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/protocol.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (`pub mod protocol;`)

- [ ] 1. Failing tests (in `protocol.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_join_parses() {
        let m: ClientMsg =
            serde_json::from_str(r#"{"op":"join","channel":"broadcast:room","ref":1}"#).unwrap();
        assert_eq!(m, ClientMsg::Join { channel: "broadcast:room".into(), r#ref: Some(1) });
    }

    #[test]
    fn client_publish_carries_arbitrary_payload() {
        let m: ClientMsg = serde_json::from_str(
            r#"{"op":"publish","channel":"broadcast:room","payload":{"x":1}}"#,
        )
        .unwrap();
        let ClientMsg::Publish { payload, .. } = m else { panic!("wrong variant") };
        assert_eq!(payload["x"], 1);
    }

    #[test]
    fn unknown_op_is_a_parse_error_not_a_panic() {
        assert!(serde_json::from_str::<ClientMsg>(r#"{"op":"hack"}"#).is_err());
    }

    #[test]
    fn server_event_serializes_with_op_tag() {
        let m = ServerMsg::Event {
            channel: "changes:Lead".into(),
            payload: serde_json::json!({"type":"insert","pk":"1","row":{"id":1}}),
        };
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert_eq!(v["op"], "event");
        assert_eq!(v["channel"], "changes:Lead");
        assert_eq!(v["payload"]["type"], "insert");
    }

    #[test]
    fn server_error_round_trips() {
        let m = ServerMsg::Error {
            code: "JC0403".into(),
            message: "forbidden".into(),
            channel: Some("broadcast:room".into()),
            r#ref: Some(2),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: ServerMsg = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime protocol` — expected FAIL (module missing).
- [ ] 3. Implement:

```rust
//! The jerrycan-native realtime envelope (spec decision #9). One WS endpoint
//! multiplexes all channels; every frame is a JSON object tagged by `op`.
//! Channels are namespaced strings: `changes:{Entity}` / `broadcast:{name}` /
//! `presence:{name}`.

use serde::{Deserialize, Serialize};

/// Client → server frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMsg {
    Join { channel: String, #[serde(default)] r#ref: Option<u64> },
    Leave { channel: String, #[serde(default)] r#ref: Option<u64> },
    /// Broadcast publish (ephemeral, client-to-client).
    Publish { channel: String, payload: serde_json::Value, #[serde(default)] r#ref: Option<u64> },
    /// Presence: set/replace this connection's state on the topic.
    Track { channel: String, state: serde_json::Value, #[serde(default)] r#ref: Option<u64> },
    /// Presence: clear this connection's state on the topic.
    Untrack { channel: String, #[serde(default)] r#ref: Option<u64> },
    Heartbeat { #[serde(default)] r#ref: Option<u64> },
}

/// Server → client frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ServerMsg {
    Joined { channel: String, r#ref: Option<u64> },
    Left { channel: String, r#ref: Option<u64> },
    /// A delivery: changes payloads are `{"type","pk","row"?,"old_pk"?}`;
    /// broadcast payloads are the publisher's JSON verbatim.
    Event { channel: String, payload: serde_json::Value },
    /// Full presence state, sent on join to a presence channel.
    PresenceState { channel: String, state: serde_json::Value },
    /// Incremental joins/leaves after the initial state.
    PresenceDiff { channel: String, joins: serde_json::Value, leaves: serde_json::Value },
    HeartbeatAck { r#ref: Option<u64> },
    /// The replication slot was recreated after a gap — refetch and resubscribe.
    Resync { channel: String },
    Error { code: String, message: String, channel: Option<String>, r#ref: Option<u64> },
}
```

- [ ] 4. Run `cargo test -p jerrycan-realtime protocol` — expected PASS.
- [ ] 5. Commit: `realtime: pin the jerrycan-native WS envelope (protocol.rs)`

---

## Task 5: channel registry + the MANDATORY scope filter (`channel.rs`)

The security pillar, unit-tested with no services: cross-tenant events must
never pass the filter, and joins are gated by scope. Everything here is pure
or mutex-local state.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/channel.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (`pub(crate) mod channel;` + re-export `ChangeEvent`/`ChangeOp` types come in Task 11; for now channel.rs owns a minimal local `ChangeEventView` — see step 3)

- [ ] 1. Failing tests (in `channel.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChangeChannelSpec, Principal, RealtimeConfig, TopicScope};

    fn cfg() -> RealtimeConfig {
        RealtimeConfig {
            changes: vec![ChangeChannelSpec {
                entity: "Lead".into(),
                table: "lead".into(),
                pk_column: "id".into(),
                tenant_column: Some("workspace_id".into()),
            }],
            broadcast: vec![
                ("room".into(), TopicScope::Tenant),
                ("lobby".into(), TopicScope::None),
            ],
            presence: vec![("editors".into(), TopicScope::Auth)],
        }
    }

    fn principal(tenant: &str) -> Principal {
        Principal { user_id: "u1".into(), tenant_id: Some(tenant.into()), role: None }
    }

    #[test]
    fn channel_ids_parse_and_reject_unknown() {
        assert!(matches!(ChannelId::parse("changes:Lead"), Some(ChannelId::Changes(e)) if e == "Lead"));
        assert!(matches!(ChannelId::parse("broadcast:room"), Some(ChannelId::Broadcast(_))));
        assert!(matches!(ChannelId::parse("presence:editors"), Some(ChannelId::Presence(_))));
        assert!(ChannelId::parse("nope").is_none());
        assert!(ChannelId::parse("changes:").is_none());
    }

    #[test]
    fn join_requires_auth_for_changes_and_scoped_topics() {
        let c = cfg();
        // changes: never joinable anonymously (mandatory scope filter).
        assert!(may_join(&ChannelId::parse("changes:Lead").unwrap(), &c, None).is_err());
        assert!(may_join(&ChannelId::parse("changes:Lead").unwrap(), &c, Some(&principal("t1"))).is_ok());
        // tenant-scoped broadcast: needs a principal WITH a tenant.
        assert!(may_join(&ChannelId::parse("broadcast:room").unwrap(), &c, None).is_err());
        let no_tenant = Principal { user_id: "u".into(), tenant_id: None, role: None };
        assert!(may_join(&ChannelId::parse("broadcast:room").unwrap(), &c, Some(&no_tenant)).is_err());
        // scope none: anonymous ok.
        assert!(may_join(&ChannelId::parse("broadcast:lobby").unwrap(), &c, None).is_ok());
        // auth-scoped presence: any principal.
        assert!(may_join(&ChannelId::parse("presence:editors").unwrap(), &c, Some(&no_tenant)).is_ok());
        // unknown channel names are rejected (not silently created).
        assert!(may_join(&ChannelId::Broadcast("ghost".into()), &c, Some(&principal("t1"))).is_err());
        assert!(may_join(&ChannelId::Changes("Ghost".into()), &c, Some(&principal("t1"))).is_err());
    }

    /// THE negative control (spec: security pillar). A change in tenant t2
    /// must never be visible to a t1 subscriber; breaking this filter must
    /// turn this test red.
    #[test]
    fn cross_tenant_change_is_never_visible() {
        let c = cfg();
        let spec = &c.changes[0];
        let ev = ChangeEventView { tenant_id: Some("t2".into()), old_tenant_id: None };
        assert!(!change_visible(spec, &ev, Some(&principal("t1"))));
        assert!(change_visible(spec, &ev, Some(&principal("t2"))));
        // Anonymous NEVER sees a change, scoped or not.
        assert!(!change_visible(spec, &ev, None));
        let unscoped = ChangeChannelSpec { tenant_column: None, ..spec.clone() };
        assert!(!change_visible(&unscoped, &ev, None));
        assert!(change_visible(&unscoped, &ev, Some(&principal("t1"))));
    }

    /// A row moving across tenants: the OLD tenant gets a delete-shaped view,
    /// the NEW tenant the update — nobody else sees anything. This is the
    /// REPLICA IDENTITY FULL / OLD-row rationale, encoded as delivery routing.
    #[test]
    fn tenant_move_routes_delete_to_old_and_update_to_new() {
        let c = cfg();
        let spec = &c.changes[0];
        let ev = ChangeEventView { tenant_id: Some("t2".into()), old_tenant_id: Some("t1".into()) };
        assert!(change_visible(spec, &ev, Some(&principal("t2"))), "new tenant sees it");
        assert!(delete_view_for_old_tenant(spec, &ev, Some(&principal("t1"))), "old tenant gets the delete view");
        assert!(!change_visible(spec, &ev, Some(&principal("t1"))), "old tenant must NOT get the row body");
        assert!(!delete_view_for_old_tenant(spec, &ev, Some(&principal("t3"))), "third tenant sees nothing");
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime channel` — expected FAIL (module missing).
- [ ] 3. Implement `channel.rs`:

```rust
//! Channel identity, join gating, and the MANDATORY scope filter. Every event
//! (Changes, Broadcast, Presence) passes these functions BEFORE it leaves the
//! server (spec: security pillar). Pure functions — the negative controls run
//! with zero services.

use crate::{ChangeChannelSpec, Principal, RealtimeConfig, TopicScope};

/// A parsed channel name: `changes:{Entity}` / `broadcast:{name}` / `presence:{name}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChannelId {
    Changes(String),
    Broadcast(String),
    Presence(String),
}

impl ChannelId {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        let (kind, name) = s.split_once(':')?;
        if name.is_empty() {
            return None;
        }
        match kind {
            "changes" => Some(Self::Changes(name.to_string())),
            "broadcast" => Some(Self::Broadcast(name.to_string())),
            "presence" => Some(Self::Presence(name.to_string())),
            _ => None,
        }
    }

    pub(crate) fn as_string(&self) -> String {
        match self {
            Self::Changes(e) => format!("changes:{e}"),
            Self::Broadcast(n) => format!("broadcast:{n}"),
            Self::Presence(n) => format!("presence:{n}"),
        }
    }
}

/// The scope keys of one change event, as the filter sees them.
/// (`ChangeEvent` proper, with op/pk/row, lands in changes/mod.rs — the filter
/// deliberately depends only on this narrow view.)
#[derive(Clone, Debug, Default)]
pub(crate) struct ChangeEventView {
    pub(crate) tenant_id: Option<String>,
    pub(crate) old_tenant_id: Option<String>,
}

/// May this principal join this channel? Err carries the protocol error text.
pub(crate) fn may_join(
    id: &ChannelId,
    cfg: &RealtimeConfig,
    principal: Option<&Principal>,
) -> Result<(), &'static str> {
    match id {
        ChannelId::Changes(entity) => {
            if !cfg.changes.iter().any(|c| &c.entity == entity) {
                return Err("unknown channel");
            }
            if principal.is_none() {
                return Err("authentication required");
            }
            Ok(())
        }
        ChannelId::Broadcast(name) => {
            let scope = cfg
                .broadcast
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| *s)
                .ok_or("unknown channel")?;
            scope_allows(scope, principal)
        }
        ChannelId::Presence(name) => {
            let scope = cfg
                .presence
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| *s)
                .ok_or("unknown channel")?;
            scope_allows(scope, principal)
        }
    }
}

fn scope_allows(scope: TopicScope, principal: Option<&Principal>) -> Result<(), &'static str> {
    match scope {
        TopicScope::None => Ok(()),
        TopicScope::Auth => principal.map(|_| ()).ok_or("authentication required"),
        TopicScope::Tenant => principal
            .and_then(|p| p.tenant_id.as_ref())
            .map(|_| ())
            .ok_or("tenant membership required"),
    }
}

/// Is the (new-row) view of this change visible to the subscriber?
/// A change the subscriber couldn't GET is never delivered.
pub(crate) fn change_visible(
    spec: &ChangeChannelSpec,
    ev: &ChangeEventView,
    principal: Option<&Principal>,
) -> bool {
    let Some(p) = principal else { return false };
    match (&spec.tenant_column, &ev.tenant_id) {
        (None, _) => true, // authenticated-only entity: any principal
        (Some(_), Some(t)) => p.tenant_id.as_deref() == Some(t.as_str()),
        // A scoped entity with no extractable tenant key: fail CLOSED.
        (Some(_), None) => false,
    }
}

/// A row that MOVED tenant delivers a delete-shaped view to the OLD tenant.
pub(crate) fn delete_view_for_old_tenant(
    spec: &ChangeChannelSpec,
    ev: &ChangeEventView,
    principal: Option<&Principal>,
) -> bool {
    let Some(p) = principal else { return false };
    if spec.tenant_column.is_none() {
        return false; // unscoped entities have no tenant partitions to move between
    }
    match (&ev.old_tenant_id, &ev.tenant_id) {
        (Some(old), new) if Some(old) != new.as_ref() => p.tenant_id.as_deref() == Some(old.as_str()),
        _ => false,
    }
}
```

Add `pub(crate) mod channel;` to `lib.rs`.

- [ ] 4. Run `cargo test -p jerrycan-realtime channel` — expected PASS.
- [ ] 5. Commit: `realtime: channel ids + mandatory scope filter with cross-tenant negative controls`

---

## Task 6: the fan-out bus (`bus.rs`) — `BusMessage`, `LocalBus`, `AnyBus`

One uniform delivery rule (Resolved #10): broadcast/presence and
replication-path changes go hub → bus → every node's hub (including the
publisher's own — self-delivery comes back over the bus so single-node and
multi-node share one code path). The trigger path bypasses the bus.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/bus.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (`pub(crate) mod bus;`)

- [ ] 1. Failing tests (in `bus.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_bus_echoes_to_all_subscribers_including_publisher() {
        let bus = LocalBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(BusMessage::Broadcast {
            topic: "room".into(),
            tenant_id: Some("t1".into()),
            payload: serde_json::json!({"x": 1}),
            origin: Some((7, 42)),
        })
        .await
        .unwrap();
        for rx in [&mut a, &mut b] {
            let BusMessage::Broadcast { topic, tenant_id, origin, .. } = rx.recv().await.unwrap()
            else {
                panic!("wrong message kind")
            };
            assert_eq!(topic, "room");
            assert_eq!(tenant_id.as_deref(), Some("t1"));
            assert_eq!(origin, Some((7, 42)));
        }
    }

    /// BusMessage must serde round-trip — the Redis bus (Task 18) ships these
    /// exact bytes between nodes.
    #[test]
    fn bus_message_round_trips_as_json() {
        let m = BusMessage::PresenceSet {
            topic: "editors".into(),
            tenant_id: None,
            key: "u1".into(),
            node: 3,
            meta: serde_json::json!({"cursor": 4}),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: BusMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime bus` — expected FAIL.
- [ ] 3. Implement:

```rust
//! The multi-node fan-out bus. `LocalBus` (in-process, tokio broadcast) is the
//! default; `RedisBus` (Task 18, behind `realtime-redis`) carries the same
//! serde-encoded messages over Redis pub/sub. Delivery rule: broadcast,
//! presence, and replication-path changes always travel hub → bus → hubs
//! (self included); the trigger path delivers hub-locally (Postgres is the
//! bus there — publishing again would double-deliver under Redis).

use serde::{Deserialize, Serialize};

/// Everything nodes exchange. `origin` on Broadcast is `(node_id, conn_id)` so
/// the publishing connection is excluded from its own delivery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum BusMessage {
    Change(crate::changes::ChangeEvent),
    Broadcast {
        topic: String,
        tenant_id: Option<String>,
        payload: serde_json::Value,
        origin: Option<(u64, u64)>,
    },
    PresenceSet {
        topic: String,
        tenant_id: Option<String>,
        key: String,
        node: u64,
        meta: serde_json::Value,
    },
    PresenceClear {
        topic: String,
        tenant_id: Option<String>,
        key: String,
        node: u64,
    },
    /// Periodic liveness: the full key set a node currently tracks, so peers
    /// can expire entries from dead nodes (Task 10 sweep).
    PresenceSnapshot {
        node: u64,
        entries: Vec<(String, Option<String>, String)>, // (topic, tenant, key)
    },
    /// Replication gap (slot recreated): subscribers must refetch.
    Resync { entity: Option<String> },
}

const BUS_CAPACITY: usize = 1024;

/// In-process bus: a tokio broadcast channel.
pub(crate) struct LocalBus {
    tx: tokio::sync::broadcast::Sender<BusMessage>,
}

impl LocalBus {
    pub(crate) fn new() -> Self {
        Self { tx: tokio::sync::broadcast::channel(BUS_CAPACITY).0 }
    }

    pub(crate) async fn publish(&self, msg: BusMessage) -> jerrycan_core::Result<()> {
        // No receivers is fine (nothing subscribed yet during startup).
        let _ = self.tx.send(msg);
        Ok(())
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BusMessage> {
        self.tx.subscribe()
    }
}

/// The bus the hub actually holds. Constructed synchronously at extension
/// registration; the Redis variant connects lazily inside the supervisor.
pub(crate) enum AnyBus {
    Local(LocalBus),
    #[cfg(feature = "realtime-redis")]
    Redis(crate::bus_redis::RedisBus),
}

impl AnyBus {
    pub(crate) async fn publish(&self, msg: BusMessage) -> jerrycan_core::Result<()> {
        match self {
            AnyBus::Local(b) => b.publish(msg).await,
            #[cfg(feature = "realtime-redis")]
            AnyBus::Redis(b) => b.publish(msg).await,
        }
    }

    /// The local fan-in every node consumes. For Redis, the pump task (run in
    /// the supervisor) forwards Redis pub/sub into this same channel.
    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BusMessage> {
        match self {
            AnyBus::Local(b) => b.subscribe(),
            #[cfg(feature = "realtime-redis")]
            AnyBus::Redis(b) => b.subscribe(),
        }
    }
}
```

Note: `crate::changes::ChangeEvent` does not exist until Task 11 — for this
task, declare a minimal placeholder-free version by CREATING
`src/changes/mod.rs` now with just the event type Task 11 will grow:

```rust
//! Postgres Changes: the shared event model (adapters land in later tasks).
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOp {
    Insert,
    Update,
    Delete,
}

/// One decoded row change, scope keys pre-extracted (all text — see
/// Principal's string rationale).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangeEvent {
    pub entity: String,
    pub op: ChangeOp,
    pub pk: String,
    /// The new row (insert/update). None for delete and for trigger-path
    /// events whose refetch found the row already gone.
    pub row: Option<serde_json::Value>,
    pub tenant_id: Option<String>,
    /// The OLD row's tenant (update/delete) — drives the tenant-move routing.
    pub old_tenant_id: Option<String>,
}
```

Add `pub mod changes;` to lib.rs.

- [ ] 4. Run `cargo test -p jerrycan-realtime bus` — expected PASS; `cargo check --workspace` green.
- [ ] 5. Commit: `realtime: bus message model + in-process LocalBus`

---

## Task 7: WS handshake functions (`ws.rs`, pure part)

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/ws.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (`pub(crate) mod ws;`)

- [ ] 1. Failing tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use jerrycan_core::http::{HeaderMap, HeaderValue};

    fn ws_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("connection", HeaderValue::from_static("Upgrade"));
        h.insert("upgrade", HeaderValue::from_static("websocket"));
        h.insert("sec-websocket-version", HeaderValue::from_static("13"));
        h.insert("sec-websocket-key", HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="));
        h
    }

    /// RFC 6455 §1.3 sample nonce → the exact accept key.
    #[test]
    fn accept_key_matches_rfc_6455_vector() {
        assert_eq!(
            handshake_accept(&ws_headers()).unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn missing_or_wrong_headers_are_rejected() {
        let mut no_key = ws_headers();
        no_key.remove("sec-websocket-key");
        assert!(handshake_accept(&no_key).is_err());

        let mut wrong_version = ws_headers();
        wrong_version.insert("sec-websocket-version", HeaderValue::from_static("8"));
        assert!(handshake_accept(&wrong_version).is_err());

        let mut not_ws = ws_headers();
        not_ws.insert("upgrade", HeaderValue::from_static("h2c"));
        assert!(handshake_accept(&not_ws).is_err());

        // Connection may be a list: "keep-alive, Upgrade" must pass.
        let mut list = ws_headers();
        list.insert("connection", HeaderValue::from_static("keep-alive, Upgrade"));
        assert!(handshake_accept(&list).is_ok());
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime ws::` — expected FAIL.
- [ ] 3. Implement (top of `ws.rs`; the connection loop joins in Task 8):

```rust
//! WebSocket transport: RFC 6455 handshake over hyper's HTTP/1 upgrade, then
//! tokio-tungstenite (Role::Server) over the upgraded socket.

use jerrycan_core::http::HeaderMap;
use jerrycan_core::{Error, Result};

/// Validate the upgrade request headers and derive Sec-WebSocket-Accept.
/// 400-class errors — the connection never upgrades on failure.
pub(crate) fn handshake_accept(headers: &HeaderMap) -> Result<String> {
    let connection_ok = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")));
    let upgrade_ok = headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if !connection_ok || !upgrade_ok {
        return Err(Error::new(
            jerrycan_core::http::StatusCode::UPGRADE_REQUIRED,
            "JC0400",
            "this endpoint speaks WebSocket — send Connection: Upgrade / Upgrade: websocket",
        ));
    }
    let version_ok = headers
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "13");
    if !version_ok {
        return Err(Error::bad_request("unsupported Sec-WebSocket-Version (need 13)"));
    }
    let key = headers
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::bad_request("missing Sec-WebSocket-Key"))?;
    Ok(tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes()))
}
```

(If `Error::new`/`Error::bad_request` differ from core's actual constructors,
match the constructors `error.rs` really exports — `Error::bad_request` exists
for JC0400; check `crates/jerrycan-core/src/error.rs` and keep the status
codes: 426 for a non-upgrade request, 400 for a malformed one.)

- [ ] 4. Run `cargo test -p jerrycan-realtime ws::` — expected PASS.
- [ ] 5. Commit: `realtime: WS handshake validation + RFC 6455 accept key`

---

## Task 8: the hub, the Extension, and the connection loop (loopback-tested)

The heart of the crate. `Realtime` becomes a real `Extension`: it provides a
`RealtimeHandle`, mounts `GET {mount}` with the upgrade handler, and registers
the `on_serve` supervisor (bus pump; source adapters join in Task 17).
Everything is tested over a real loopback socket with tokio-tungstenite's
client — no external services.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/hub_state.rs` — NO. Keep the hub in `lib.rs` per the spec's module list; `lib.rs` gains `Hub`, `RealtimeHandle`, the `Extension` impl, and the supervisor.
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/ws.rs` (WsStart extractor + handler + connection loop)
- Test: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/tests/ws_live.rs` (new)

- [ ] 1. Failing integration test:

```rust
//! Loopback WS integration: a real serve on 127.0.0.1, a real
//! tokio-tungstenite client, zero external services. Broadcast/presence run
//! without Postgres (resolved decision #9), so `Db` here is sqlite::memory:.
use jerrycan_realtime::{Realtime, TopicScope};
use tokio_tungstenite::tungstenite::Message;

/// Serve an app with the given Realtime extension on an ephemeral port;
/// returns (port, shutdown sender, server task).
async fn serve(rt: Realtime) -> (u16, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let db = jerrycan::db::Db::connect("sqlite::memory:").await.unwrap();
    let _ = &db; // rt was built with its own db handle by the caller
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = jerrycan::App::new().extend(rt);
    let task = tokio::spawn(async move {
        let _ = app.serve_with_shutdown(listener, async { let _ = rx.await; }).await;
    });
    // Let the accept loop come up.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (port, tx, task)
}

async fn connect(port: u16) -> tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
> {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/realtime"))
        .await
        .expect("ws connect");
    ws
}

async fn recv_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    use futures_core::Stream;
    loop {
        let msg = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut *ws).poll_next(cx)),
        )
        .await
        .expect("timed out waiting for a frame")
        .expect("stream ended")
        .expect("ws error");
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).expect("server frames are JSON");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn join_heartbeat_and_error_envelopes_round_trip() {
    let db = jerrycan::db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;

    let mut ws = connect(port).await;
    ws.send(Message::Text(r#"{"op":"join","channel":"broadcast:lobby","ref":1}"#.into()))
        .await
        .unwrap();
    let joined = recv_json(&mut ws).await;
    assert_eq!(joined["op"], "joined");
    assert_eq!(joined["channel"], "broadcast:lobby");
    assert_eq!(joined["ref"], 1);

    ws.send(Message::Text(r#"{"op":"heartbeat","ref":2}"#.into())).await.unwrap();
    let ack = recv_json(&mut ws).await;
    assert_eq!(ack["op"], "heartbeat_ack");

    // Unknown channel ⇒ error envelope, connection stays up.
    ws.send(Message::Text(r#"{"op":"join","channel":"broadcast:ghost","ref":3}"#.into()))
        .await
        .unwrap();
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["ref"], 3);

    // Malformed JSON ⇒ error envelope with JC0422.
    ws.send(Message::Text("not json".into())).await.unwrap();
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["code"], "JC0422");

    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn non_websocket_get_is_rejected_without_upgrade() {
    let db = jerrycan::db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;
    let res = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"GET /realtime HTTP/1.1\r\nHost: t\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 512];
        let n = s.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    };
    assert!(res.starts_with("HTTP/1.1 426"), "expected 426 Upgrade Required: {res}");
    let _ = shutdown.send(());
    let _ = task.await;
}
```

The dev-dependency on the `jerrycan` facade creates a cycle (`jerrycan` ←
`jerrycan-realtime`), so the test must use `jerrycan_core::App` and
`jerrycan_db::Db` directly — add them (plus `serde_json`) to
`[dev-dependencies]` instead of the facade, and write `jerrycan_core::App` /
`jerrycan_db::Db::connect(...)` in the test (mirrors how
`jerrycan-ratelimit`'s tests use `jerrycan_core` directly).

- [ ] 2. Run `cargo test -p jerrycan-realtime --test ws_live` — expected FAIL (no Extension impl, no ws route).
- [ ] 3. Implement, three pieces.

**(a) lib.rs — hub + handle + Extension.** Add to `Realtime` the resolver field
and to lib.rs:

```rust
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Resolves the connection's principal at upgrade time from the request
/// (session cookie / bearer / ?token=). Generated wiring supplies it; absent
/// resolver ⇒ anonymous connections (only scope-none channels joinable).
pub type PrincipalResolver = Arc<
    dyn for<'a> Fn(
            &'a mut jerrycan_core::RequestCtx,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = jerrycan_core::Result<Principal>> + Send + 'a>,
        > + Send
        + Sync,
>;

impl Realtime {
    pub fn principal(mut self, resolver: PrincipalResolver) -> Self {
        self.resolver = Some(resolver);
        self
    }
}

/// Per-connection outbound queue capacity; a full queue disconnects the
/// client (at-most-once live-UI semantics — resolved decision #12).
const CONN_QUEUE: usize = 128;

pub(crate) struct Subscriber {
    pub(crate) principal: Option<Principal>,
    pub(crate) tx: tokio::sync::mpsc::Sender<crate::protocol::ServerMsg>,
    pub(crate) channels: std::collections::HashSet<crate::channel::ChannelId>,
}

/// The connection hub: registry + delivery. One per app.
pub struct Hub {
    pub(crate) config: RealtimeConfig,
    pub(crate) node_id: u64,
    pub(crate) bus: bus::AnyBus,
    pub(crate) db: Option<jerrycan_db::Db>,
    pub(crate) conns: Mutex<HashMap<u64, Subscriber>>,
    pub(crate) presence: Mutex<presence::PresenceMap>, // lands in Task 10; until then a unit struct
    next_conn: AtomicU64,
}

impl Hub {
    pub(crate) fn connect(
        self: &Arc<Self>,
        principal: Option<Principal>,
    ) -> (u64, tokio::sync::mpsc::Receiver<crate::protocol::ServerMsg>) {
        let id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::channel(CONN_QUEUE);
        self.conns.lock().expect("hub mutex").insert(
            id,
            Subscriber { principal, tx, channels: Default::default() },
        );
        (id, rx)
    }

    pub(crate) fn disconnect(self: &Arc<Self>, conn: u64) {
        // Presence leaves broadcast in Task 10; for now just drop the entry.
        self.conns.lock().expect("hub mutex").remove(&conn);
    }

    /// Send to one connection; a full/closed queue drops the connection.
    pub(crate) fn send_to(&self, conn: u64, msg: crate::protocol::ServerMsg) {
        let mut conns = self.conns.lock().expect("hub mutex");
        if let Some(sub) = conns.get(&conn) {
            if sub.tx.try_send(msg).is_err() {
                conns.remove(&conn); // slow consumer: rx closes, loop ends
            }
        }
    }

    /// One client frame: parse, dispatch, reply via the conn's own queue.
    pub(crate) async fn handle_client(self: &Arc<Self>, conn: u64, text: &str) {
        use crate::protocol::{ClientMsg, ServerMsg};
        let msg: ClientMsg = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                return self.send_to(conn, ServerMsg::Error {
                    code: "JC0422".into(),
                    message: format!("unparseable frame: {e}"),
                    channel: None,
                    r#ref: None,
                });
            }
        };
        match msg {
            ClientMsg::Heartbeat { r#ref } => self.send_to(conn, ServerMsg::HeartbeatAck { r#ref }),
            ClientMsg::Join { channel, r#ref } => self.join(conn, &channel, r#ref),
            ClientMsg::Leave { channel, r#ref } => self.leave(conn, &channel, r#ref),
            // Publish lands in Task 9, Track/Untrack in Task 10 — until then
            // they answer with a not-implemented error envelope so the match
            // stays exhaustive and honest.
            other => {
                let (channel, r#ref) = match other {
                    ClientMsg::Publish { channel, r#ref, .. }
                    | ClientMsg::Track { channel, r#ref, .. }
                    | ClientMsg::Untrack { channel, r#ref } => (Some(channel), r#ref),
                    _ => (None, None),
                };
                self.send_to(conn, ServerMsg::Error {
                    code: "JC0500".into(),
                    message: "not implemented yet".into(),
                    channel,
                    r#ref,
                });
            }
        }
    }

    fn join(self: &Arc<Self>, conn: u64, channel: &str, r#ref: Option<u64>) {
        use crate::protocol::ServerMsg;
        let Some(id) = crate::channel::ChannelId::parse(channel) else {
            return self.send_to(conn, ServerMsg::Error {
                code: "JC0404".into(),
                message: "unknown channel namespace".into(),
                channel: Some(channel.to_string()),
                r#ref,
            });
        };
        let allowed = {
            let conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.get(&conn) else { return };
            crate::channel::may_join(&id, &self.config, sub.principal.as_ref())
        };
        match allowed {
            Err("unknown channel") => self.send_to(conn, ServerMsg::Error {
                code: "JC0404".into(),
                message: "unknown channel".into(),
                channel: Some(channel.to_string()),
                r#ref,
            }),
            Err(reason) => self.send_to(conn, ServerMsg::Error {
                code: if reason.contains("authentication") { "JC0401" } else { "JC0403" }.into(),
                message: reason.into(),
                channel: Some(channel.to_string()),
                r#ref,
            }),
            Ok(()) => {
                if let Some(sub) = self.conns.lock().expect("hub mutex").get_mut(&conn) {
                    sub.channels.insert(id);
                }
                self.send_to(conn, ServerMsg::Joined { channel: channel.to_string(), r#ref });
                // Presence initial state joins in Task 10.
            }
        }
    }

    fn leave(self: &Arc<Self>, conn: u64, channel: &str, r#ref: Option<u64>) {
        use crate::protocol::ServerMsg;
        if let Some(id) = crate::channel::ChannelId::parse(channel) {
            if let Some(sub) = self.conns.lock().expect("hub mutex").get_mut(&conn) {
                sub.channels.remove(&id);
            }
        }
        self.send_to(conn, ServerMsg::Left { channel: channel.to_string(), r#ref });
    }
}

/// The app-provided dependency: handlers (and the ws extractor) resolve this.
#[derive(Clone)]
pub struct RealtimeHandle {
    pub(crate) hub: Arc<Hub>,
    pub(crate) resolver: Option<PrincipalResolver>,
}

impl jerrycan_core::Extension for Realtime {
    fn register(self, app: jerrycan_core::App) -> jerrycan_core::App {
        let bus = bus::AnyBus::Local(bus::LocalBus::new()); // Redis selection: Task 18 supervisor
        let hub = Arc::new(Hub {
            config: self.config,
            node_id: rand_node_id(),
            bus,
            db: self.db,
            conns: Mutex::new(HashMap::new()),
            presence: Mutex::new(presence::PresenceMap::default()),
            next_conn: AtomicU64::new(1),
        });
        let handle = RealtimeHandle { hub: hub.clone(), resolver: self.resolver };
        app.provide(handle)
            .route(&self.mount, jerrycan_core::get(ws::ws_handler))
            .on_serve("realtime", move |_ctx, mut shutdown| async move {
                // Bus pump: everything on the bus is delivered locally.
                let mut rx = hub.bus.subscribe();
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        msg = rx.recv() => match msg {
                            Ok(m) => hub.deliver(m),
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                eprintln!("jerrycan-realtime: bus lagged, dropped {n} messages");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        },
                    }
                }
            })
    }
}

/// A random node id (presence/broadcast origin tagging across nodes). Uses
/// the std hasher over a fresh allocation address + time — no rand dep.
fn rand_node_id() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish()
}
```

`Hub::deliver(BusMessage)` for this task only needs the Broadcast arm stubbed
to a no-op match that Tasks 9/10/17 fill — implement it as an exhaustive match
with the arms landing in their tasks (`BusMessage::Change(_) => {}` etc. each
carrying a `// Task N` comment). Also add an empty
`pub(crate) mod presence { #[derive(Default)] pub(crate) struct PresenceMap; }`
inline until Task 10 replaces it with the real module file.

**(b) ws.rs — WsStart extractor + handler + connection loop:**

```rust
pub(crate) struct WsStart {
    hub: std::sync::Arc<crate::Hub>,
    principal: Option<crate::Principal>,
    accept: String,
    on_upgrade: hyper::upgrade::OnUpgrade,
}

impl jerrycan_core::FromRequest for WsStart {
    async fn from_request(ctx: &mut jerrycan_core::RequestCtx) -> Result<Self> {
        let handle = ctx.resolve::<crate::RealtimeHandle>().await?;
        let accept = handshake_accept(ctx.headers())?;
        // Auth BEFORE upgrade: a bad credential is a plain 401 response.
        let principal = match handle.resolver.as_ref() {
            Some(r) => Some(r(ctx).await?),
            None => None,
        };
        let on_upgrade = ctx
            .take_extension::<hyper::upgrade::OnUpgrade>()
            .ok_or_else(|| Error::internal("connection does not support upgrades"))?;
        Ok(WsStart { hub: handle.hub.clone(), principal, accept, on_upgrade })
    }
}

pub(crate) async fn ws_handler(start: WsStart) -> Result<jerrycan_core::Response> {
    let WsStart { hub, principal, accept, on_upgrade } = start;
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let io = hyper_util::rt::TokioIo::new(upgraded);
                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    io,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    None,
                )
                .await;
                run_connection(ws, hub, principal).await;
            }
            Err(e) => eprintln!("jerrycan-realtime: upgrade failed: {e}"),
        }
    });
    use jerrycan_core::IntoResponse;
    let mut res = "".into_response();
    *res.status_mut() = jerrycan_core::http::StatusCode::SWITCHING_PROTOCOLS;
    let headers = res.headers_mut();
    headers.insert(
        jerrycan_core::http::header::CONNECTION,
        jerrycan_core::http::HeaderValue::from_static("upgrade"),
    );
    headers.insert(
        jerrycan_core::http::header::UPGRADE,
        jerrycan_core::http::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        jerrycan_core::http::header::SEC_WEBSOCKET_ACCEPT,
        jerrycan_core::http::HeaderValue::from_str(&accept)
            .map_err(|_| Error::internal("accept key is always a valid header"))?,
    );
    Ok(res)
}

/// The per-connection loop. Single task, no socket split: select over the
/// outbound queue and the inbound stream, then act with the exclusive borrow
/// (the select arms only *return* the step; both futures are cancel-safe).
pub(crate) async fn run_connection<S>(
    mut ws: tokio_tungstenite::WebSocketStream<S>,
    hub: std::sync::Arc<crate::Hub>,
    principal: Option<crate::Principal>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_core::Stream;
    use tokio_tungstenite::tungstenite::Message;

    /// Server-side idle cutoff: a client that sends nothing (not even a
    /// heartbeat) for this long is disconnected.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(60);

    let (conn, mut rx) = hub.connect(principal);
    let mut deadline = tokio::time::Instant::now() + IDLE;

    enum Step {
        Out(Option<crate::protocol::ServerMsg>),
        In(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
        Idle,
    }

    loop {
        let step = tokio::select! {
            m = rx.recv() => Step::Out(m),
            r = std::future::poll_fn(|cx| std::pin::Pin::new(&mut ws).poll_next(cx)) => Step::In(r),
            _ = tokio::time::sleep_until(deadline) => Step::Idle,
        };
        match step {
            Step::Out(Some(msg)) => {
                let text = serde_json::to_string(&msg).expect("server frames serialize");
                if ws.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            Step::Out(None) => break, // hub dropped us (slow consumer / shutdown)
            Step::In(Some(Ok(Message::Text(t)))) => {
                deadline = tokio::time::Instant::now() + IDLE;
                hub.handle_client(conn, t.as_str()).await;
            }
            Step::In(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {
                deadline = tokio::time::Instant::now() + IDLE;
                // tungstenite auto-answers pings on the next send/flush.
            }
            Step::In(Some(Ok(Message::Close(_))) | None) => break,
            Step::In(Some(Ok(_))) => {} // binary frames ignored (protocol is JSON text)
            Step::In(Some(Err(_))) => break,
            Step::Idle => break,
        }
    }
    hub.disconnect(conn);
    let _ = ws.close(None).await;
}
```

**(c) 426 on a plain GET** comes free from `handshake_accept` (it errors before
any upgrade). Verify the error → response mapping emits the 426 status.

- [ ] 4. Run `cargo test -p jerrycan-realtime --test ws_live` — expected PASS. Also `cargo test -p jerrycan-realtime` and `cargo clippy --workspace --all-targets -- -D warnings` (match the repo's gate).
- [ ] 5. Commit: `realtime: connection hub, upgrade handler, and WS connection loop with loopback tests`

---

## Task 9: Broadcast end-to-end (with the tenant negative control)

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/broadcast.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (Publish arm + `deliver` Broadcast arm move here)
- Test: extend `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/tests/ws_live.rs`

- [ ] 1. Failing tests (append to `ws_live.rs`). The tenant test uses a header-driven test resolver — the same seam generated wiring uses, proving the resolver contract:

```rust
fn header_resolver() -> jerrycan_realtime::PrincipalResolver {
    std::sync::Arc::new(|ctx| {
        Box::pin(async move {
            let user = ctx
                .headers()
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(jerrycan_core::Error::unauthorized)?
                .to_string();
            let tenant = ctx
                .headers()
                .get("x-tenant")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            Ok(jerrycan_realtime::Principal { user_id: user, tenant_id: tenant, role: None })
        })
    })
}

async fn connect_as(port: u16, user: &str, tenant: &str) -> WsClient {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/realtime").into_client_request().unwrap();
    req.headers_mut().insert("x-user", user.parse().unwrap());
    req.headers_mut().insert("x-tenant", tenant.parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.expect("ws connect");
    ws
}

#[tokio::test(flavor = "multi_thread")]
async fn broadcast_reaches_subscribers_but_not_publisher_or_other_tenants() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db)
        .broadcast("room", TopicScope::Tenant)
        .principal(header_resolver());
    let (port, shutdown, task) = serve(rt).await;

    let mut a = connect_as(port, "alice", "t1").await; // publisher
    let mut b = connect_as(port, "bob", "t1").await;   // same tenant — receives
    let mut c = connect_as(port, "carol", "t2").await; // OTHER tenant — must not

    for ws in [&mut a, &mut b, &mut c] {
        ws.send(Message::Text(r#"{"op":"join","channel":"broadcast:room","ref":1}"#.into()))
            .await
            .unwrap();
        assert_eq!(recv_json(ws).await["op"], "joined");
    }

    a.send(Message::Text(
        r#"{"op":"publish","channel":"broadcast:room","payload":{"msg":"hi"},"ref":2}"#.into(),
    ))
    .await
    .unwrap();

    // Bob gets the event.
    let ev = recv_json(&mut b).await;
    assert_eq!(ev["op"], "event");
    assert_eq!(ev["channel"], "broadcast:room");
    assert_eq!(ev["payload"]["msg"], "hi");

    // NEGATIVE CONTROLS: carol (cross-tenant) and alice (self) get NOTHING.
    // Prove it by round-tripping a heartbeat on each — the next frame must be
    // the ack, not a leaked event.
    for ws in [&mut c, &mut a] {
        ws.send(Message::Text(r#"{"op":"heartbeat","ref":9}"#.into())).await.unwrap();
        let next = recv_json(ws).await;
        assert_eq!(next["op"], "heartbeat_ack", "leaked broadcast: {next}");
    }

    let _ = shutdown.send(());
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_requires_membership_of_the_channel() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db).broadcast("lobby", TopicScope::None);
    let (port, shutdown, task) = serve(rt).await;
    let mut ws = connect(port).await;
    // Publish WITHOUT joining ⇒ 403-coded error envelope.
    ws.send(Message::Text(
        r#"{"op":"publish","channel":"broadcast:lobby","payload":{},"ref":1}"#.into(),
    ))
    .await
    .unwrap();
    let err = recv_json(&mut ws).await;
    assert_eq!(err["op"], "error");
    assert_eq!(err["code"], "JC0403");
    let _ = shutdown.send(());
    let _ = task.await;
}
```

(Define the `WsClient` type alias once at the top of `ws_live.rs`.)

- [ ] 2. Run `cargo test -p jerrycan-realtime --test ws_live broadcast` — expected FAIL (publish answers "not implemented").
- [ ] 3. Implement `broadcast.rs` + wire it:

```rust
//! Broadcast: ephemeral client-to-client pub/sub. Publishing requires having
//! joined the topic; tenant-scoped topics are partitioned per tenant; the
//! publisher's own connection is excluded from delivery (Supabase parity:
//! self-broadcast off).

use crate::bus::BusMessage;
use crate::channel::ChannelId;
use crate::protocol::ServerMsg;
use std::sync::Arc;

impl crate::Hub {
    /// The Publish op: gate, then put it on the bus (self included — delivery
    /// happens uniformly in `deliver_broadcast` when the pump hands it back).
    pub(crate) async fn publish(
        self: &Arc<Self>,
        conn: u64,
        channel: &str,
        payload: serde_json::Value,
        r#ref: Option<u64>,
    ) {
        let Some(id @ ChannelId::Broadcast(topic)) =
            ChannelId::parse(channel).map(|id| (id.clone(), id)).map(|(a, _)| a)
        else {
            return self.send_to(conn, ServerMsg::Error {
                code: "JC0404".into(),
                message: "publish targets a broadcast channel".into(),
                channel: Some(channel.to_string()),
                r#ref,
            });
        };
        let (joined, tenant) = {
            let conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.get(&conn) else { return };
            (
                sub.channels.contains(&id),
                sub.principal.as_ref().and_then(|p| p.tenant_id.clone()),
            )
        };
        if !joined {
            return self.send_to(conn, ServerMsg::Error {
                code: "JC0403".into(),
                message: "join the channel before publishing".into(),
                channel: Some(channel.to_string()),
                r#ref,
            });
        }
        // Tenant partition key: the publisher's tenant when the topic is
        // tenant-scoped, None otherwise.
        let scope = self
            .config
            .broadcast
            .iter()
            .find(|(n, _)| *n == topic)
            .map(|(_, s)| *s);
        let tenant_id = match scope {
            Some(crate::TopicScope::Tenant) => tenant,
            _ => None,
        };
        let node = self.node_id;
        if let Err(e) = self
            .bus
            .publish(BusMessage::Broadcast { topic, tenant_id, payload, origin: Some((node, conn)) })
            .await
        {
            eprintln!("jerrycan-realtime: bus publish failed: {e}");
        }
    }

    /// Bus → local subscribers. Runs on EVERY node (the publisher's included).
    pub(crate) fn deliver_broadcast(
        &self,
        topic: &str,
        tenant_id: Option<&str>,
        payload: &serde_json::Value,
        origin: Option<(u64, u64)>,
    ) {
        let id = ChannelId::Broadcast(topic.to_string());
        let channel = id.as_string();
        let mut drop_list = Vec::new();
        {
            let conns = self.conns.lock().expect("hub mutex");
            for (cid, sub) in conns.iter() {
                if !sub.channels.contains(&id) {
                    continue;
                }
                if origin == Some((self.node_id, *cid)) {
                    continue; // no self-delivery
                }
                // Tenant partition: both sides must agree exactly.
                if let Some(t) = tenant_id {
                    if sub.principal.as_ref().and_then(|p| p.tenant_id.as_deref()) != Some(t) {
                        continue;
                    }
                }
                if sub
                    .tx
                    .try_send(ServerMsg::Event { channel: channel.clone(), payload: payload.clone() })
                    .is_err()
                {
                    drop_list.push(*cid);
                }
            }
        }
        for cid in drop_list {
            self.conns.lock().expect("hub mutex").remove(&cid);
        }
    }
}
```

Wire the `ClientMsg::Publish` arm in `handle_client` to `self.publish(...).await`
and the `BusMessage::Broadcast` arm in `Hub::deliver` to `deliver_broadcast`.
Add `pub(crate) mod broadcast;` to lib.rs.

- [ ] 4. Run `cargo test -p jerrycan-realtime --test ws_live` — expected PASS (all, including Task 8's).
- [ ] 5. Commit: `realtime: broadcast pub/sub with tenant partitioning and no self-delivery`

---

## Task 10: Presence (merge functions + join/sync/leave over loopback)

The fiddliest feature (spec's words); the merge/diff/expiry logic is pure and
unit-tested, then wired through the same bus path.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/presence.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (replace the inline stub module; Track/Untrack arms; presence arms in `deliver`; snapshot + sweep in the supervisor)
- Test: unit tests in `presence.rs` + loopback tests in `tests/ws_live.rs`

- [ ] 1. Failing unit tests (in `presence.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn part(topic: &str) -> Partition {
        Partition { topic: topic.into(), tenant_id: None }
    }

    #[test]
    fn set_then_state_then_clear_produces_join_and_leave_diffs() {
        let mut map = PresenceMap::default();
        let joins = map.set(&part("editors"), "u1", 1, serde_json::json!({"c": 3}), 1_000);
        assert_eq!(joins, Some(serde_json::json!({"u1": {"c": 3}})));
        // Same key, newer meta: last-writer-wins, still reported as a join diff.
        let joins = map.set(&part("editors"), "u1", 1, serde_json::json!({"c": 4}), 2_000);
        assert_eq!(joins, Some(serde_json::json!({"u1": {"c": 4}})));
        assert_eq!(
            map.state(&part("editors")),
            serde_json::json!({"u1": {"c": 4}})
        );
        let leave = map.clear(&part("editors"), "u1", 1);
        assert_eq!(leave, Some(serde_json::json!({"u1": {"c": 4}})));
        assert_eq!(map.state(&part("editors")), serde_json::json!({}));
    }

    #[test]
    fn partitions_are_isolated_by_tenant() {
        let mut map = PresenceMap::default();
        let t1 = Partition { topic: "editors".into(), tenant_id: Some("t1".into()) };
        let t2 = Partition { topic: "editors".into(), tenant_id: Some("t2".into()) };
        map.set(&t1, "u1", 1, serde_json::json!({}), 0);
        assert_eq!(map.state(&t2), serde_json::json!({}), "cross-tenant presence must be empty");
    }

    #[test]
    fn clear_from_another_node_does_not_remove_a_local_claim() {
        // Last-writer-wins is per (key): a clear from node 2 for a key node 1
        // owns is ignored (node 1's set is authoritative until IT clears).
        let mut map = PresenceMap::default();
        map.set(&part("editors"), "u1", 1, serde_json::json!({}), 0);
        assert!(map.clear(&part("editors"), "u1", 2).is_none());
        assert_eq!(map.state(&part("editors")), serde_json::json!({"u1": {}}));
    }

    #[test]
    fn sweep_expires_entries_from_silent_nodes() {
        let mut map = PresenceMap::default();
        map.set(&part("editors"), "u1", 1, serde_json::json!({}), 0);
        map.set(&part("editors"), "u2", 2, serde_json::json!({}), 0);
        // Node 1 heartbeats at t=60_000; node 2 stays silent.
        map.touch_node(1, 60_000);
        let expired = map.sweep(100_000, 90_000); // ttl 90s
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0.topic, "editors");
        assert_eq!(expired[0].1, serde_json::json!({"u2": {}}));
        assert_eq!(map.state(&part("editors")), serde_json::json!({"u1": {}}));
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime presence` — expected FAIL.
- [ ] 3. Implement `presence.rs`:

```rust
//! Presence: per-topic online state, merged across nodes. One meta per key
//! (last-writer-wins on the client's own key — resolved in the spec); entries
//! are owned by the node that set them; silent nodes expire after a TTL.

use std::collections::HashMap;

/// A presence partition: the topic plus its tenant slice (None for
/// non-tenant-scoped topics).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Partition {
    pub(crate) topic: String,
    pub(crate) tenant_id: Option<String>,
}

struct Entry {
    node: u64,
    meta: serde_json::Value,
}

#[derive(Default)]
pub(crate) struct PresenceMap {
    entries: HashMap<Partition, HashMap<String, Entry>>,
    /// node id → last heartbeat (epoch ms). The local node touches itself on
    /// every snapshot tick, remote nodes on every snapshot received.
    node_seen: HashMap<u64, u64>,
}

impl PresenceMap {
    /// Set/replace `key` in the partition. Returns the join diff to broadcast
    /// (always Some — a replace re-announces the new meta).
    pub(crate) fn set(
        &mut self,
        part: &Partition,
        key: &str,
        node: u64,
        meta: serde_json::Value,
        now_ms: u64,
    ) -> Option<serde_json::Value> {
        self.node_seen.insert(node, now_ms);
        self.entries
            .entry(part.clone())
            .or_default()
            .insert(key.to_string(), Entry { node, meta: meta.clone() });
        Some(serde_json::json!({ key: meta }))
    }

    /// Clear `key` if `node` owns it. Returns the leave diff when removed.
    pub(crate) fn clear(
        &mut self,
        part: &Partition,
        key: &str,
        node: u64,
    ) -> Option<serde_json::Value> {
        let bucket = self.entries.get_mut(part)?;
        match bucket.get(key) {
            Some(e) if e.node == node => {
                let e = bucket.remove(key).expect("checked above");
                Some(serde_json::json!({ key: e.meta }))
            }
            _ => None,
        }
    }

    /// Full state of a partition: `{key: meta, ...}`.
    pub(crate) fn state(&self, part: &Partition) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = self
            .entries
            .get(part)
            .map(|b| b.iter().map(|(k, e)| (k.clone(), e.meta.clone())).collect())
            .unwrap_or_default();
        serde_json::Value::Object(map)
    }

    /// Record a node heartbeat (its own snapshot tick, or a received one).
    pub(crate) fn touch_node(&mut self, node: u64, now_ms: u64) {
        self.node_seen.insert(node, now_ms);
    }

    /// Drop every entry owned by a node not seen within `ttl_ms`. Returns the
    /// leave diffs per partition, so the hub can broadcast them.
    pub(crate) fn sweep(&mut self, now_ms: u64, ttl_ms: u64) -> Vec<(Partition, serde_json::Value)> {
        let dead: std::collections::HashSet<u64> = self
            .node_seen
            .iter()
            .filter(|(_, seen)| now_ms.saturating_sub(**seen) > ttl_ms)
            .map(|(n, _)| *n)
            .collect();
        // A node that never heartbeated but owns entries is dead too — any
        // owner absent from node_seen counts as unseen since 0.
        let mut out = Vec::new();
        for (part, bucket) in self.entries.iter_mut() {
            let mut leaves = serde_json::Map::new();
            bucket.retain(|k, e| {
                let node_dead =
                    dead.contains(&e.node) || !self.node_seen.contains_key(&e.node);
                if node_dead {
                    leaves.insert(k.clone(), e.meta.clone());
                }
                !node_dead
            });
            if !leaves.is_empty() {
                out.push((part.clone(), serde_json::Value::Object(leaves)));
            }
        }
        for n in dead {
            self.node_seen.remove(&n);
        }
        self.entries.retain(|_, b| !b.is_empty());
        out
    }
}
```

Note the sweep test setup: `set(...)` touches the owner node, so
`sweep_expires_entries_from_silent_nodes` relies on `touch_node(1, 60_000)`
refreshing node 1 only — keep `set`'s `node_seen` insert (the test's t=0 set
then t=100_000 sweep with ttl 90_000 expires node 2 at 0 but keeps node 1 at
60_000).

Then wire the hub (lib.rs):
- `ClientMsg::Track { channel, state, ref }` → parse `presence:{name}`, require
  joined + `may_join` still true; presence key = `principal.user_id` (fallback:
  `conn:{conn_id}` for anonymous on scope-none topics); publish
  `BusMessage::PresenceSet { topic, tenant_id, key, node, meta }`; ack via
  `Joined`-style is NOT sent — the diff echoed back through the bus is the ack.
- `ClientMsg::Untrack` → `BusMessage::PresenceClear`.
- `Hub::deliver` arms: `PresenceSet` → `presence.set(...)`, broadcast the join
  diff to subscribers of that partition as `ServerMsg::PresenceDiff { joins,
  leaves: {} }`; `PresenceClear` → leave diff likewise; `PresenceSnapshot` →
  `touch_node` + reconcile (set every listed key's node liveness).
- On `join` of a presence channel: send `ServerMsg::PresenceState` with the
  partition's current state immediately after `Joined`.
- On `disconnect`: for every presence channel the conn had tracked, publish
  `PresenceClear` (track which keys a conn owns in `Subscriber`, e.g.
  `tracked: HashSet<(ChannelId, String)>`).
- Supervisor tick (extend the Task 8 `on_serve` loop with a
  `tokio::time::interval(Duration::from_secs(30))` select arm): publish
  `PresenceSnapshot` for the local node's tracked keys, `touch_node(self)`,
  and run `sweep(now, 90_000)`, broadcasting any leave diffs.

- [ ] 4. Loopback test (append to `ws_live.rs`), then run:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn presence_join_sync_track_and_leave() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let rt = Realtime::new(db)
        .presence("editors", TopicScope::Auth)
        .principal(header_resolver());
    let (port, shutdown, task) = serve(rt).await;

    let mut a = connect_as(port, "alice", "t1").await;
    a.send(Message::Text(r#"{"op":"join","channel":"presence:editors","ref":1}"#.into()))
        .await
        .unwrap();
    assert_eq!(recv_json(&mut a).await["op"], "joined");
    // Initial sync: empty state.
    let state = recv_json(&mut a).await;
    assert_eq!(state["op"], "presence_state");
    assert_eq!(state["state"], serde_json::json!({}));

    a.send(Message::Text(
        r#"{"op":"track","channel":"presence:editors","state":{"cursor":1}}"#.into(),
    ))
    .await
    .unwrap();
    let diff = recv_json(&mut a).await;
    assert_eq!(diff["op"], "presence_diff");
    assert_eq!(diff["joins"]["alice"]["cursor"], 1);

    // Bob joins late: his initial state already contains alice.
    let mut b = connect_as(port, "bob", "t1").await;
    b.send(Message::Text(r#"{"op":"join","channel":"presence:editors","ref":1}"#.into()))
        .await
        .unwrap();
    assert_eq!(recv_json(&mut b).await["op"], "joined");
    let state = recv_json(&mut b).await;
    assert_eq!(state["state"]["alice"]["cursor"], 1);

    // Alice disconnects: bob sees the leave diff.
    drop(a);
    let diff = recv_json(&mut b).await;
    assert_eq!(diff["op"], "presence_diff");
    assert!(diff["leaves"]["alice"].is_object(), "leave diff for alice: {diff}");

    let _ = shutdown.send(());
    let _ = task.await;
}
```

`cargo test -p jerrycan-realtime` — expected PASS (unit + loopback).
- [ ] 5. Commit: `realtime: presence state with join/sync/leave diffs and node-expiry sweep`

---

## Task 11: Changes foundation — DDL templates, detection SQL, NOTIFY payload

Grows `src/changes/mod.rs` (created in Task 6) with everything both adapters
share. Pure string/serde work — fully unit-tested, no database.

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/mod.rs`

- [ ] 1. Failing tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeChannelSpec;

    fn lead() -> ChangeChannelSpec {
        ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
        }
    }

    #[test]
    fn publication_ddl_is_create_or_set_table() {
        let specs = [lead()];
        assert_eq!(
            publication_exists_sql(),
            "SELECT 1 FROM pg_publication WHERE pubname = 'jc_changes'"
        );
        assert_eq!(
            create_publication_sql(&specs),
            r#"CREATE PUBLICATION jc_changes FOR TABLE "lead""#
        );
        assert_eq!(
            reconcile_publication_sql(&specs),
            r#"ALTER PUBLICATION jc_changes SET TABLE "lead""#
        );
        assert_eq!(
            replica_identity_sql(&lead()),
            r#"ALTER TABLE "lead" REPLICA IDENTITY FULL"#
        );
    }

    #[test]
    fn trigger_ddl_embeds_pk_and_tenant_columns_and_old_row_keys() {
        let f = notify_function_sql(&lead());
        // OLD is only valid on UPDATE/DELETE — the function must guard it.
        assert!(f.contains("CREATE OR REPLACE FUNCTION jc_notify_change_lead()"), "{f}");
        assert!(f.contains("pg_notify('jc_changes'"), "{f}");
        assert!(f.contains("TG_OP"), "{f}");
        assert!(f.contains("workspace_id"), "{f}");
        let t = trigger_sql(&lead());
        assert!(t.starts_with(r#"DROP TRIGGER IF EXISTS jc_changes_lead ON "lead""#), "{t}");
        assert!(t.contains(r#"CREATE TRIGGER jc_changes_lead AFTER INSERT OR UPDATE OR DELETE ON "lead""#), "{t}");
        assert!(t.contains("FOR EACH ROW EXECUTE FUNCTION jc_notify_change_lead()"), "{t}");
    }

    #[test]
    fn notify_payload_round_trips_and_stays_small() {
        let p = NotifyPayload {
            table: "lead".into(),
            op: ChangeOp::Update,
            pk: "42".into(),
            tenant_id: Some("7".into()),
            old_tenant_id: Some("3".into()),
        };
        let s = serde_json::to_string(&p).unwrap();
        // Compact keys: NOTIFY payloads are capped at 8000 bytes.
        assert!(s.len() < 120, "payload must stay far under the NOTIFY cap: {s}");
        let back: NotifyPayload = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
        // The exact wire keys are a contract with the generated trigger SQL.
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v.get("t").is_some() && v.get("o").is_some() && v.get("id").is_some());
    }

    #[test]
    fn event_from_notify_maps_delete_with_old_tenant() {
        let p = NotifyPayload {
            table: "lead".into(),
            op: ChangeOp::Delete,
            pk: "42".into(),
            tenant_id: None,
            old_tenant_id: Some("3".into()),
        };
        let ev = p.into_event("Lead");
        assert_eq!(ev.op, ChangeOp::Delete);
        assert_eq!(ev.pk, "42");
        assert_eq!(ev.old_tenant_id.as_deref(), Some("3"));
        assert!(ev.row.is_none());
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime changes` — expected FAIL.
- [ ] 3. Implement in `changes/mod.rs` (below the Task 6 types):

```rust
use crate::ChangeChannelSpec;

/// The one publication realtime owns. Fixed name: reconcile, don't multiply.
pub(crate) const PUBLICATION: &str = "jc_changes";
/// The one NOTIFY channel the trigger fallback uses.
pub(crate) const NOTIFY_CHANNEL: &str = "jc_changes";
/// The replication slot name.
pub(crate) const SLOT: &str = "jc_realtime";

pub(crate) fn publication_exists_sql() -> String {
    format!("SELECT 1 FROM pg_publication WHERE pubname = '{PUBLICATION}'")
}

fn table_list(specs: &[ChangeChannelSpec]) -> String {
    specs.iter().map(|s| format!("\"{}\"", s.table)).collect::<Vec<_>>().join(", ")
}

pub(crate) fn create_publication_sql(specs: &[ChangeChannelSpec]) -> String {
    format!("CREATE PUBLICATION {PUBLICATION} FOR TABLE {}", table_list(specs))
}

pub(crate) fn reconcile_publication_sql(specs: &[ChangeChannelSpec]) -> String {
    format!("ALTER PUBLICATION {PUBLICATION} SET TABLE {}", table_list(specs))
}

pub(crate) fn replica_identity_sql(spec: &ChangeChannelSpec) -> String {
    format!("ALTER TABLE \"{}\" REPLICA IDENTITY FULL", spec.table)
}

/// The compact LISTEN/NOTIFY payload (8KB NOTIFY cap ⇒ keys only, no row —
/// the listener refetches the body). Keys are single letters by design; the
/// trigger SQL builds this exact object.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct NotifyPayload {
    #[serde(rename = "t")]
    pub(crate) table: String,
    #[serde(rename = "o")]
    pub(crate) op: ChangeOp,
    #[serde(rename = "id")]
    pub(crate) pk: String,
    #[serde(rename = "tn", skip_serializing_if = "Option::is_none", default)]
    pub(crate) tenant_id: Option<String>,
    #[serde(rename = "to", skip_serializing_if = "Option::is_none", default)]
    pub(crate) old_tenant_id: Option<String>,
}

impl NotifyPayload {
    pub(crate) fn into_event(self, entity: &str) -> ChangeEvent {
        ChangeEvent {
            entity: entity.to_string(),
            op: self.op,
            pk: self.pk,
            row: None, // the trigger adapter refetches for insert/update
            tenant_id: self.tenant_id,
            old_tenant_id: self.old_tenant_id,
        }
    }
}

/// The per-table notify trigger function. NEW/OLD validity per TG_OP is
/// handled with CASE guards; every value is ::text so scope comparison is
/// uniform. `serde_json`'s `snake_case` for ChangeOp matches lower(TG_OP).
pub(crate) fn notify_function_sql(spec: &ChangeChannelSpec) -> String {
    let table = &spec.table;
    let pk = &spec.pk_column;
    let tenant_new = match &spec.tenant_column {
        Some(c) => format!("CASE WHEN TG_OP <> 'DELETE' THEN NEW.\"{c}\"::text END"),
        None => "NULL".to_string(),
    };
    let tenant_old = match &spec.tenant_column {
        Some(c) => format!("CASE WHEN TG_OP <> 'INSERT' THEN OLD.\"{c}\"::text END"),
        None => "NULL".to_string(),
    };
    format!(
        "CREATE OR REPLACE FUNCTION jc_notify_change_{table}() RETURNS trigger AS $$\n\
         BEGIN\n\
         \x20 PERFORM pg_notify('{NOTIFY_CHANNEL}', json_build_object(\n\
         \x20   't', TG_TABLE_NAME,\n\
         \x20   'o', lower(TG_OP),\n\
         \x20   'id', CASE WHEN TG_OP = 'DELETE' THEN OLD.\"{pk}\"::text ELSE NEW.\"{pk}\"::text END,\n\
         \x20   'tn', {tenant_new},\n\
         \x20   'to', {tenant_old}\n\
         \x20 )::text);\n\
         \x20 RETURN NULL;\n\
         END;\n\
         $$ LANGUAGE plpgsql"
    )
}

/// Idempotent trigger install: drop-if-exists then create (works on PG < 14
/// where CREATE OR REPLACE TRIGGER is unavailable). Two statements joined by
/// `;` — the adapter executes them separately.
pub(crate) fn trigger_sql(spec: &ChangeChannelSpec) -> String {
    let table = &spec.table;
    format!(
        "DROP TRIGGER IF EXISTS jc_changes_{table} ON \"{table}\";\n\
         CREATE TRIGGER jc_changes_{table} AFTER INSERT OR UPDATE OR DELETE ON \"{table}\"\n\
         FOR EACH ROW EXECUTE FUNCTION jc_notify_change_{table}()"
    )
}
```

Also add the `ChangeSource` trait + detection SQL constants (consumed by
Tasks 15–17):

```rust
/// Detection queries (run over the sea-orm Db — the data layer stays sqlx).
pub(crate) const SHOW_WAL_LEVEL: &str = "SHOW wal_level";
pub(crate) const SHOW_MAX_SLOT_WAL_KEEP_SIZE: &str = "SHOW max_slot_wal_keep_size";
pub(crate) const CAN_REPLICATE: &str =
    "SELECT rolreplication OR rolsuper AS ok FROM pg_roles WHERE rolname = current_user";

/// One CDC source: runs until shutdown, emitting decoded events. Both
/// adapters implement this; the hub treats them identically (spec: the client
/// sees identical behavior — only the source differs).
pub(crate) trait ChangeSource: Send + 'static {
    fn run(
        self: Box<Self>,
        events: tokio::sync::mpsc::Sender<ChangeEvent>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;
}
```

- [ ] 4. Run `cargo test -p jerrycan-realtime changes` — expected PASS.
- [ ] 5. Commit: `realtime: shared changes model, generated DDL templates, notify payload`

---

## Task 12: pgoutput part 1 — Lsn + replication outer frames (`changes/pgoutput.rs`)

Pure byte decoding, testable against synthetic frames — deliberately no
database anywhere in Tasks 12–13.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/pgoutput.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/mod.rs` (`pub(crate) mod pgoutput;` — declared as `mod pgoutput;` inside the `changes` module directory)

- [ ] 1. Failing tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsn_parses_and_displays_postgres_form() {
        let lsn = Lsn::parse("16/B374D848").unwrap();
        assert_eq!(lsn.0, (0x16u64 << 32) | 0xB374_D848);
        assert_eq!(lsn.to_string(), "16/B374D848");
        assert_eq!(Lsn(0).to_string(), "0/0");
        assert!(Lsn::parse("junk").is_none());
    }

    #[test]
    fn xlogdata_frame_decodes() {
        // 'w' + wal_start(8) + wal_end(8) + send_time(8) + payload.
        let mut buf = vec![b'w'];
        buf.extend_from_slice(&100u64.to_be_bytes());
        buf.extend_from_slice(&200u64.to_be_bytes());
        buf.extend_from_slice(&300i64.to_be_bytes());
        buf.extend_from_slice(b"PAYLOAD");
        let ReplicationFrame::XLogData { wal_start, wal_end, data } =
            ReplicationFrame::parse(&buf).unwrap()
        else {
            panic!("wrong frame kind")
        };
        assert_eq!(wal_start, Lsn(100));
        assert_eq!(wal_end, Lsn(200));
        assert_eq!(data, b"PAYLOAD");
    }

    #[test]
    fn keepalive_frame_decodes_reply_flag() {
        // 'k' + wal_end(8) + timestamp(8) + reply(1).
        let mut buf = vec![b'k'];
        buf.extend_from_slice(&500u64.to_be_bytes());
        buf.extend_from_slice(&0i64.to_be_bytes());
        buf.push(1);
        let ReplicationFrame::Keepalive { wal_end, reply_requested } =
            ReplicationFrame::parse(&buf).unwrap()
        else {
            panic!("wrong frame kind")
        };
        assert_eq!(wal_end, Lsn(500));
        assert!(reply_requested);
    }

    #[test]
    fn truncated_frames_error_instead_of_panicking() {
        assert!(ReplicationFrame::parse(&[b'w', 0, 1]).is_err());
        assert!(ReplicationFrame::parse(&[]).is_err());
        assert!(ReplicationFrame::parse(&[b'z']).is_err());
    }

    #[test]
    fn standby_status_update_encodes_the_r_frame() {
        let bytes = standby_status_update(Lsn(0xAB), 12345);
        assert_eq!(bytes[0], b'r');
        assert_eq!(bytes.len(), 1 + 8 + 8 + 8 + 8 + 1);
        // written / flushed / applied all report the same confirmed LSN.
        for chunk in [&bytes[1..9], &bytes[9..17], &bytes[17..25]] {
            assert_eq!(u64::from_be_bytes(chunk.try_into().unwrap()), 0xAB);
        }
        assert_eq!(bytes[33], 0, "no reply requested");
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime pgoutput` — expected FAIL.
- [ ] 3. Implement:

```rust
//! The replication stream's outer frames (XLogData / keepalive / standby
//! status) and the Postgres LSN. Pure functions over bytes — unit-tested with
//! synthetic frames; no socket, no database.

/// A WAL location. Wire form u64; text form "hi/lo" hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct Lsn(pub(crate) u64);

impl Lsn {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        let (hi, lo) = s.split_once('/')?;
        let hi = u64::from_str_radix(hi, 16).ok()?;
        let lo = u64::from_str_radix(lo, 16).ok()?;
        Some(Lsn((hi << 32) | lo))
    }
}

impl std::fmt::Display for Lsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFF_FFFF)
    }
}

#[derive(Debug)]
pub(crate) enum ReplicationFrame<'a> {
    XLogData { wal_start: Lsn, wal_end: Lsn, data: &'a [u8] },
    Keepalive { wal_end: Lsn, reply_requested: bool },
}

pub(crate) fn read_u64(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes(s.try_into().expect("8 bytes")))
        .ok_or_else(|| format!("frame truncated at {at}"))
}

impl<'a> ReplicationFrame<'a> {
    pub(crate) fn parse(buf: &'a [u8]) -> Result<Self, String> {
        match buf.first() {
            Some(b'w') => {
                let wal_start = Lsn(read_u64(buf, 1)?);
                let wal_end = Lsn(read_u64(buf, 9)?);
                let _send_time = read_u64(buf, 17)?;
                Ok(Self::XLogData { wal_start, wal_end, data: &buf[25..] })
            }
            Some(b'k') => {
                let wal_end = Lsn(read_u64(buf, 1)?);
                let _ts = read_u64(buf, 9)?;
                let reply = *buf.get(17).ok_or("keepalive truncated")?;
                Ok(Self::Keepalive { wal_end, reply_requested: reply != 0 })
            }
            Some(t) => Err(format!("unknown replication frame tag {t:#x}")),
            None => Err("empty replication frame".into()),
        }
    }
}

/// Postgres epoch (2000-01-01) microseconds — the clock field of 'r' frames.
pub(crate) fn pg_epoch_micros_now() -> i64 {
    const PG_EPOCH_UNIX_SECS: i64 = 946_684_800;
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    unix - PG_EPOCH_UNIX_SECS * 1_000_000
}

/// The continuous LSN confirmation the spec mandates: written/flushed/applied
/// all advance to `confirmed` so the server can recycle WAL.
pub(crate) fn standby_status_update(confirmed: Lsn, clock_pg_micros: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(34);
    out.push(b'r');
    out.extend_from_slice(&confirmed.0.to_be_bytes());
    out.extend_from_slice(&confirmed.0.to_be_bytes());
    out.extend_from_slice(&confirmed.0.to_be_bytes());
    out.extend_from_slice(&clock_pg_micros.to_be_bytes());
    out.push(0);
    out
}
```

- [ ] 4. Run `cargo test -p jerrycan-realtime pgoutput` — expected PASS.
- [ ] 5. Commit: `realtime: LSN and replication outer-frame codec`

---

## Task 13: pgoutput part 2 — logical message decode + relation cache + event assembly

Still pure bytes. pgoutput proto_version 1 messages: Begin `B`, Commit `C`,
Relation `R`, Insert `I`, Update `U`, Delete `D` (Origin `O`, Type `Y`,
Truncate `T` are skipped). TupleData columns are text-format under proto v1.

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/pgoutput.rs`

- [ ] 1. Failing tests (synthetic frames built by a tiny in-test builder — this is the captured-frame strategy from the spec's testing section, hermetic and byte-exact):

```rust
#[cfg(test)]
mod logical_tests {
    use super::*;

    /// Build a Relation ('R') message for a 2-column table.
    fn relation_msg(rel_id: u32, name: &str, cols: &[&str]) -> Vec<u8> {
        let mut b = vec![b'R'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.extend_from_slice(b"public\0"); // namespace
        b.extend_from_slice(name.as_bytes());
        b.push(0);
        b.push(b'f'); // replica identity: full
        b.extend_from_slice(&(cols.len() as u16).to_be_bytes());
        for c in cols {
            b.push(0); // flags
            b.extend_from_slice(c.as_bytes());
            b.push(0);
            b.extend_from_slice(&25u32.to_be_bytes()); // type oid (text)
            b.extend_from_slice(&(-1i32).to_be_bytes()); // typmod
        }
        b
    }

    /// TupleData: n columns of ('t', len, text) / 'n' null.
    fn tuple(vals: &[Option<&str>]) -> Vec<u8> {
        let mut b = (vals.len() as u16).to_be_bytes().to_vec();
        for v in vals {
            match v {
                Some(s) => {
                    b.push(b't');
                    b.extend_from_slice(&(s.len() as u32).to_be_bytes());
                    b.extend_from_slice(s.as_bytes());
                }
                None => b.push(b'n'),
            }
        }
        b
    }

    fn insert_msg(rel_id: u32, vals: &[Option<&str>]) -> Vec<u8> {
        let mut b = vec![b'I'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.push(b'N');
        b.extend_from_slice(&tuple(vals));
        b
    }

    fn update_msg_with_old(rel_id: u32, old: &[Option<&str>], new: &[Option<&str>]) -> Vec<u8> {
        let mut b = vec![b'U'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.push(b'O'); // OLD tuple follows (REPLICA IDENTITY FULL)
        b.extend_from_slice(&tuple(old));
        b.push(b'N');
        b.extend_from_slice(&tuple(new));
        b
    }

    fn delete_msg(rel_id: u32, old: &[Option<&str>]) -> Vec<u8> {
        let mut b = vec![b'D'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.push(b'O');
        b.extend_from_slice(&tuple(old));
        b
    }

    #[test]
    fn relation_then_insert_yields_named_row() {
        let mut cache = RelationCache::default();
        assert!(matches!(
            decode_logical(&relation_msg(1, "lead", &["id", "workspace_id"]), &mut cache).unwrap(),
            Logical::Meta
        ));
        let Logical::Row(row) =
            decode_logical(&insert_msg(1, &[Some("42"), Some("7")]), &mut cache).unwrap()
        else {
            panic!("expected a row")
        };
        assert_eq!(row.table, "lead");
        assert_eq!(row.op, crate::changes::ChangeOp::Insert);
        assert_eq!(row.new.as_ref().unwrap()["id"], "42");
        assert_eq!(row.new.as_ref().unwrap()["workspace_id"], "7");
        assert!(row.old.is_none());
    }

    #[test]
    fn update_carries_old_and_new_tuples() {
        let mut cache = RelationCache::default();
        decode_logical(&relation_msg(1, "lead", &["id", "workspace_id"]), &mut cache).unwrap();
        let Logical::Row(row) = decode_logical(
            &update_msg_with_old(1, &[Some("42"), Some("3")], &[Some("42"), Some("7")]),
            &mut cache,
        )
        .unwrap() else {
            panic!("expected a row")
        };
        assert_eq!(row.old.as_ref().unwrap()["workspace_id"], "3");
        assert_eq!(row.new.as_ref().unwrap()["workspace_id"], "7");
    }

    #[test]
    fn delete_carries_only_old() {
        let mut cache = RelationCache::default();
        decode_logical(&relation_msg(1, "lead", &["id", "workspace_id"]), &mut cache).unwrap();
        let Logical::Row(row) =
            decode_logical(&delete_msg(1, &[Some("42"), Some("7")]), &mut cache).unwrap()
        else {
            panic!("expected a row")
        };
        assert_eq!(row.op, crate::changes::ChangeOp::Delete);
        assert!(row.new.is_none());
        assert_eq!(row.old.as_ref().unwrap()["id"], "42");
    }

    #[test]
    fn begin_commit_and_unknown_tables_are_meta_or_skipped() {
        let mut cache = RelationCache::default();
        let mut begin = vec![b'B'];
        begin.extend_from_slice(&[0u8; 20]); // final_lsn + ts + xid
        assert!(matches!(decode_logical(&begin, &mut cache).unwrap(), Logical::Meta));
        let mut commit = vec![b'C', 0];
        commit.extend_from_slice(&900u64.to_be_bytes()); // commit lsn
        commit.extend_from_slice(&950u64.to_be_bytes()); // end lsn
        commit.extend_from_slice(&0i64.to_be_bytes());
        let Logical::Commit { end_lsn } = decode_logical(&commit, &mut cache).unwrap() else {
            panic!("expected commit")
        };
        assert_eq!(end_lsn, Lsn(950));
        // An insert for an unseen relation id is skipped, not a crash.
        assert!(matches!(
            decode_logical(&insert_msg(99, &[Some("1")]), &mut cache).unwrap(),
            Logical::Meta
        ));
    }

    #[test]
    fn row_change_becomes_change_event_with_scope_keys() {
        use crate::ChangeChannelSpec;
        let spec = ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
        };
        let row = RowChange {
            table: "lead".into(),
            op: crate::changes::ChangeOp::Update,
            old: Some(serde_json::json!({"id": "42", "workspace_id": "3"})),
            new: Some(serde_json::json!({"id": "42", "workspace_id": "7"})),
        };
        let ev = row.into_event(&spec).unwrap();
        assert_eq!(ev.pk, "42");
        assert_eq!(ev.tenant_id.as_deref(), Some("7"));
        assert_eq!(ev.old_tenant_id.as_deref(), Some("3"));
        assert_eq!(ev.row.as_ref().unwrap()["workspace_id"], "7");
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime logical` — expected FAIL.
- [ ] 3. Implement in `pgoutput.rs`: a `RelationCache` (`HashMap<u32, (String, Vec<String>)>`), `Logical` enum (`Meta`, `Commit { end_lsn }`, `Row(RowChange)`), `RowChange { table, op, old, new }` with `into_event(&ChangeChannelSpec) -> Option<ChangeEvent>` (pk from new-else-old; tenant keys via the spec's tenant column; values as JSON strings since proto v1 tuples are text), plus the byte readers: cstr, u16/u32/i64 big-endian, `read_tuple(&[u8], at) -> (Vec<Option<String>>, usize)` handling `t`/`n`/`u` column kinds (`u` = unchanged TOAST ⇒ treat as absent: carry `None` and, on update, fall back to the old tuple's value when present — test this in a follow-up assert). Decode arms:
  - `R` → parse namespace/name/identity/columns → cache insert → `Meta`
  - `B`, `O`, `Y`, `T`, `M` → `Meta` (skipped)
  - `C` → read flags(1) + commit_lsn(8) + end_lsn(8) + ts(8) → `Commit { end_lsn }`
  - `I` → rel_id + expect `N` + tuple → `Row` (old: None)
  - `U` → rel_id + optional `K`/`O` tuple + `N` tuple → `Row`
  - `D` → rel_id + `K`/`O` tuple → `Row` (new: None)
  - unseen rel_id ⇒ `Meta` (never panic; log once)

  Tuples map to `serde_json::Value::Object` with the cached column names; a
  `None` column is JSON `null`.
- [ ] 4. Run `cargo test -p jerrycan-realtime pgoutput logical` — expected PASS. `cargo clippy -p jerrycan-realtime --all-targets -- -D warnings` clean.
- [ ] 5. Commit: `realtime: pgoutput logical decode with relation cache and event assembly`

---

## Task 14: the replication socket (`changes/wire.rs`)

The hand-rolled connection (Resolved #1): tokio TCP → optional rustls
(SSLRequest) → startup with `replication=database` → auth → simple query →
CopyBoth. `postgres-protocol` supplies startup/password/SASL encoding and
DataRow parsing; we own the outer framing because mainline postgres-protocol
does not parse `CopyBothResponse` ('W').

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/wire.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/mod.rs` (`mod wire;`)
- Modify: `/Users/sorcecoder/github/jerrycan/Cargo.toml` (+ `tokio-rustls` workspace entry if not added in Task 1)

- [ ] 1. Failing tests — the pure parts (URL parsing + frame splitting):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_urls_parse_host_port_db_user_password() {
        let c = PgUrl::parse("postgres://app:s3cr%40t@db.example.com:5433/prod").unwrap();
        assert_eq!(c.host, "db.example.com");
        assert_eq!(c.port, 5433);
        assert_eq!(c.user, "app");
        assert_eq!(c.password.as_deref(), Some("s3cr@t")); // percent-decoded
        assert_eq!(c.dbname, "prod");
        // Defaults: port 5432, dbname = user.
        let d = PgUrl::parse("postgres://alice@localhost").unwrap();
        assert_eq!(d.port, 5432);
        assert_eq!(d.dbname, "alice");
        assert!(PgUrl::parse("sqlite::memory:").is_none());
        assert!(PgUrl::parse("postgres://").is_none());
    }

    #[test]
    fn backend_frames_split_on_tag_and_length() {
        // Two frames: 'W' (CopyBothResponse, body ignored) then 'd' CopyData.
        let mut buf = bytes::BytesMut::new();
        buf.extend_from_slice(&[b'W']);
        buf.extend_from_slice(&7i32.to_be_bytes()); // len includes itself: 4 + 3 body
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(&[b'd']);
        buf.extend_from_slice(&9i32.to_be_bytes()); // 4 + 5 body
        buf.extend_from_slice(b"hello");
        let f1 = split_frame(&mut buf).unwrap().unwrap();
        assert_eq!(f1.tag, b'W');
        assert_eq!(f1.body.as_ref(), &[0, 0, 0]);
        let f2 = split_frame(&mut buf).unwrap().unwrap();
        assert_eq!(f2.tag, b'd');
        assert_eq!(f2.body.as_ref(), b"hello");
        assert!(split_frame(&mut buf).unwrap().is_none(), "buffer drained");
        // A partial frame waits for more bytes.
        buf.extend_from_slice(&[b'd', 0, 0]);
        assert!(split_frame(&mut buf).unwrap().is_none());
    }

    #[test]
    fn copy_data_frames_encode_with_length_prefix() {
        let payload = [b'r', 1, 2, 3];
        let framed = encode_copy_data(&payload);
        assert_eq!(framed[0], b'd');
        assert_eq!(i32::from_be_bytes(framed[1..5].try_into().unwrap()), 4 + 4);
        assert_eq!(&framed[5..], &payload);
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime wire` — expected FAIL.
- [ ] 3. Implement `wire.rs`:

```rust
//! The one place jerrycan speaks the Postgres wire protocol itself: the
//! logical-replication session. Mainline tokio-postgres has no replication
//! support (verified 0.7.18) and git forks can't be published, so this module
//! owns: TCP connect, the SSLRequest/rustls preamble, startup with
//! `replication=database`, auth (trust/cleartext/md5/scram), simple queries,
//! and CopyBoth framing. postgres-protocol does the message encoding/crypto.
//! Everything row-shaped still goes through sqlx/sea-orm — never this file.
```

Pieces (all real code in the implementation; summarized here by signature +
behavior, the bodies follow directly from the protocol):

```rust
pub(crate) struct PgUrl {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub dbname: String,
}
impl PgUrl { pub(crate) fn parse(url: &str) -> Option<Self> { /* strip scheme
    postgres:// or postgresql://, split userinfo@hostport/db?query, percent-
    decode user/password, default port 5432 and dbname=user */ } }

pub(crate) struct Frame { pub tag: u8, pub body: bytes::Bytes }

/// Zero-copy split of one backend frame off the buffer; Ok(None) = incomplete.
pub(crate) fn split_frame(buf: &mut bytes::BytesMut) -> Result<Option<Frame>, String> { ... }

/// 'd' + i32 len + payload — standby status updates ride CopyData.
pub(crate) fn encode_copy_data(payload: &[u8]) -> Vec<u8> { ... }

/// Either plain TCP or TLS — one enum so the session code is generic-free.
pub(crate) enum PgStream {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
}
// impl AsyncRead + AsyncWrite for PgStream by delegation.

pub(crate) struct ReplicationSocket {
    stream: PgStream,
    buf: bytes::BytesMut,
}

impl ReplicationSocket {
    /// Connect + SSLRequest preamble + startup + auth, up to ReadyForQuery.
    /// TLS policy (resolved #14): server declines ('N') ⇒ plaintext; accepts
    /// ('S') ⇒ rustls with webpki roots (ring provider, exactly like
    /// jerrycan-auth::oauth), failure = hard error, no downgrade.
    pub(crate) async fn connect(url: &PgUrl) -> jerrycan_core::Result<Self> { ... }

    /// Auth loop: postgres_protocol::message::backend::Message::parse over
    /// non-copy frames; AuthenticationOk / CleartextPassword
    /// (frontend::password_message) / MD5 (authentication::md5_hash) / SASL
    /// (authentication::sasl::ScramSha256 with ChannelBinding::unsupported —
    /// documented v1 limitation). ErrorResponse ⇒ Err with the server message.
    async fn authenticate(&mut self, url: &PgUrl) -> jerrycan_core::Result<()> { ... }

    /// Simple query returning text rows (IDENTIFY_SYSTEM, START_REPLICATION
    /// preflight). frontend::query + DataRowBody parsing until ReadyForQuery.
    pub(crate) async fn simple_query(&mut self, sql: &str) -> jerrycan_core::Result<Vec<Vec<Option<String>>>> { ... }

    /// Issue START_REPLICATION and enter CopyBoth: returns once the 'W'
    /// CopyBothResponse arrives. ErrorResponse before 'W' surfaces the
    /// server's message (slot invalid, wal_level, etc.).
    pub(crate) async fn start_replication(&mut self, slot: &str, publication: &str) -> jerrycan_core::Result<()> {
        let sql = format!(
            "START_REPLICATION SLOT {slot} LOGICAL 0/0 (proto_version '1', publication_names '\"{publication}\"')"
        );
        ...
    }

    /// Next CopyData payload (the replication frames of Task 12). Handles
    /// interleaved NoticeResponse/ParameterStatus; CopyDone/ErrorResponse ⇒
    /// Err (the supervisor reconnects).
    pub(crate) async fn next_copy_data(&mut self) -> jerrycan_core::Result<bytes::Bytes> { ... }

    /// Send one standby status update (CopyData-wrapped 'r' frame).
    pub(crate) async fn send_standby_status(&mut self, confirmed: super::pgoutput::Lsn) -> jerrycan_core::Result<()> { ... }
}
```

Implementation notes that MUST hold:
- The startup parameters are exactly `[("user", user), ("database", dbname), ("replication", "database"), ("application_name", "jerrycan-realtime")]` via `postgres_protocol::message::frontend::startup_message`.
- The startup message is the ONE frontend message without a tag byte; everything after auth is tag-framed. `split_frame` is only used post-startup-response; the SSLRequest answer is a single raw byte (`S`/`N`).
- rustls `ClientConfig` mirrors `crates/jerrycan-auth/src/oauth.rs`: `ring` provider explicitly, `webpki-roots` trust anchors, no native certs.
- [ ] 4. Run `cargo test -p jerrycan-realtime wire` — expected PASS (pure parts). The socket paths are exercised live in Task 15's ignored tests. `cargo clippy` clean.
- [ ] 5. Commit: `realtime: hand-rolled replication socket over postgres-protocol and rustls`

---

## Task 15: the logical-replication adapter (`changes/replication.rs`)

Slot management, the decode loop, LSN confirmation, supervised reconnect with
auto-recreate + resync, and advisory-lock leader election. Live tests need a
`wal_level=logical` Postgres and are `#[ignore]`d.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/replication.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/mod.rs` (`mod replication;`)
- Test: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/tests/replication_pg.rs` (new)

- [ ] 1. Write the failing live test file:

```rust
#![cfg(not(target_os = "windows"))]
//! Live logical-replication tests. Need a wal_level=logical Postgres:
//!
//! ```text
//! docker run --rm -d --name jc-rt-pg -p 5433:5432 -e POSTGRES_PASSWORD=postgres \
//!   postgres:16 -c wal_level=logical -c max_replication_slots=4 -c max_wal_senders=4
//! JERRYCAN_TEST_PG_LOGICAL=postgres://postgres:postgres@127.0.0.1:5433/postgres \
//!   cargo test -p jerrycan-realtime --test replication_pg -- --ignored --test-threads=1
//! ```
//!
//! Ignored by default (CI's eval job provides the container). Single-threaded:
//! the tests share one slot/publication namespace per run.
use jerrycan_realtime::{ChangeChannelSpec, Realtime};

fn url() -> String {
    std::env::var("JERRYCAN_TEST_PG_LOGICAL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5433/postgres".into())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn replication_streams_insert_update_and_tenant_move() {
    // 1. Fresh table with a tenant column (unique suffix per run).
    // 2. ensure_replication(db, &[spec]) — publication + REPLICA IDENTITY FULL
    //    + slot, all idempotent (assert calling it twice succeeds).
    // 3. Spawn ReplicationAdapter::run with an mpsc; INSERT a row via sqlx,
    //    assert a ChangeEvent { op: Insert, pk, tenant_id: Some("1"), row: Some(_) }
    //    arrives within 10s.
    // 4. UPDATE the row's tenant 1→2: assert tenant_id=Some("2"),
    //    old_tenant_id=Some("1"), op: Update.
    // 5. DELETE: assert op Delete with old tenant key and row: None body rules.
    // 6. Drop the slot in teardown (SELECT pg_drop_replication_slot).
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn advisory_lock_elects_exactly_one_leader_and_fails_over() {
    // Two LeaderGate::acquire(url) futures: the first resolves, the second
    // stays pending; dropping the first's guard (its connection) lets the
    // second acquire within 10s. This is pg_try_advisory_lock + a dedicated
    // sqlx connection — no replication needed, but colocated here because the
    // leader gates the replication adapter.
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn slot_invalidation_recreates_and_emits_resync() {
    // Drop the slot behind the adapter's back (pg_drop_replication_slot from
    // a second connection while the adapter is between reconnects), then
    // assert the adapter (a) recreates the slot, (b) emits a Resync bus event
    // (exposed for tests via the adapter's event channel), (c) resumes
    // streaming new inserts.
}
```

Write the three test bodies out fully (they are ordinary sqlx + adapter
plumbing; the comments above are their specification — expand them into real
code in this task, no stubs left behind).

- [ ] 2. Run `cargo test -p jerrycan-realtime --test replication_pg -- --ignored` **with the container up** — expected FAIL (module missing). Without a container this task's red/green loop cannot run; starting the container is part of the task.
- [ ] 3. Implement `replication.rs`:

```rust
//! The PRIMARY change source: logical decoding of the WAL via pgoutput.
//! Self-maintaining: idempotent publication/slot, REPLICA IDENTITY FULL,
//! continuous LSN confirmation, supervised reconnect with backoff, slot
//! auto-recreate + resync on invalidation, advisory-lock leader election.

use super::pgoutput::{self, Lsn, ReplicationFrame};
use super::wire::{PgUrl, ReplicationSocket};

/// The reserved advisory-lock key for the replication leader. Distinct from
/// jerrycan-jobs' cron key (see JOBS_CRON_ADVISORY_KEY) — both are documented
/// project-reserved keys.
pub const REALTIME_LEADER_ADVISORY_KEY: i64 = 0x6A63_5254_4C44_5231; // "jcRTLDR1"

/// Held by the elected leader: a dedicated (non-pooled) connection owning
/// pg_try_advisory_lock. Dropping it releases the lock server-side.
pub(crate) struct LeaderGuard {
    _conn: sqlx::PgConnection,
}

pub(crate) struct LeaderGate;

impl LeaderGate {
    /// Poll pg_try_advisory_lock every 5s on a dedicated connection until
    /// acquired or shutdown. A dropped/killed connection = automatic release,
    /// so failover needs no coordination.
    pub(crate) async fn acquire(
        url: &str,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Option<LeaderGuard> { /* sqlx::Connection::connect(url), loop:
        SELECT pg_try_advisory_lock($1) → true ⇒ Some(guard); false ⇒ select!
        5s sleep vs shutdown.changed(); reconnect on connection error. */ }
}

/// Idempotent DDL reconcile: publication (create-or-SET TABLE), REPLICA
/// IDENTITY FULL per table, slot (create when absent). All through the
/// data-layer Db except slot creation, which is plain SQL too
/// (pg_create_logical_replication_slot).
pub(crate) async fn ensure_replication(
    db: &jerrycan_db::Db,
    specs: &[crate::ChangeChannelSpec],
) -> jerrycan_core::Result<()> { ... }

/// One streaming session: START_REPLICATION → decode → events out, LSN
/// confirmations back (every keepalive-with-reply and every 10s tick).
/// Returns Ok(()) on clean shutdown, Err to trigger the supervisor's backoff.
pub(crate) async fn stream_once(
    socket: &mut ReplicationSocket,
    specs: &[crate::ChangeChannelSpec],
    events: &tokio::sync::mpsc::Sender<super::ChangeEvent>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> jerrycan_core::Result<()> {
    /* relation cache + confirmed-lsn state; loop select! over
       socket.next_copy_data() / shutdown / 10s confirm interval:
       - XLogData → decode_logical → Row → into_event(spec) → events.send
       - Commit { end_lsn } → confirmed = end_lsn
       - Keepalive { reply_requested: true } → send_standby_status(confirmed)
       - interval tick → send_standby_status(confirmed)  */
}

/// The supervised leader loop the lib.rs source-selection spawns:
/// acquire leadership → ensure DDL → connect → stream; on error, backoff
/// 1s→30s (doubling), re-ensure, reconnect. A "slot does not exist /
/// invalidated" error additionally recreates the slot and emits a Resync
/// through `events` (the hub broadcasts ServerMsg::Resync per changes
/// channel and puts BusMessage::Resync on the bus for peer nodes).
pub(crate) async fn run_supervised(
    db: jerrycan_db::Db,
    specs: Vec<crate::ChangeChannelSpec>,
    events: tokio::sync::mpsc::Sender<super::ChangeEvent>,
    resync: tokio::sync::mpsc::Sender<()>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) { ... }
```

Slot-invalidation detection: the START_REPLICATION / stream error whose
SQLSTATE is `55000` (object_not_in_prerequisite_state — invalidated slot) or
`42704` (undefined_object — dropped slot) triggers recreate+resync; other
errors just backoff-reconnect. Surface the SQLSTATE from wire.rs's
ErrorResponse parsing (postgres-protocol exposes the fields).

Also implement `impl ChangeSource for ReplicationAdapter` wrapping
`run_supervised` (the trait object the Task 17 selector spawns), where
`ReplicationAdapter { db, url, specs, resync_tx }`.

- [ ] 4. With the container up: `JERRYCAN_TEST_PG_LOGICAL=... cargo test -p jerrycan-realtime --test replication_pg -- --ignored --test-threads=1` — expected PASS ×3. Without the container: `cargo test -p jerrycan-realtime` stays green (all live tests ignored).
- [ ] 5. Commit: `realtime: logical replication adapter with slot lifecycle, LSN confirm, leader election`

---

## Task 16: the trigger + LISTEN/NOTIFY fallback (`changes/triggers.rs`)

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/triggers.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/mod.rs` (`mod triggers;`)
- Test: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/tests/changes_pg.rs` (new)

- [ ] 1. Failing live test file (default Postgres — NO logical wal_level needed; that asymmetry is the point of the fallback):

```rust
//! Live trigger-fallback tests against a STOCK Postgres (wal_level=replica):
//!
//! ```text
//! docker run --rm -d --name jc-rt-pg-stock -p 5434:5432 -e POSTGRES_PASSWORD=postgres postgres:16
//! JERRYCAN_TEST_PG=postgres://postgres:postgres@127.0.0.1:5434/postgres \
//!   cargo test -p jerrycan-realtime --test changes_pg -- --ignored --test-threads=1
//! ```
```

Tests (write them fully):
- `triggers_install_idempotently` — `ensure_triggers(db, &specs)` twice; both succeed; `SELECT tgname FROM pg_trigger` shows exactly one `jc_changes_{table}`.
- `insert_update_delete_flow_through_notify_with_refetch` — spawn `TriggerAdapter::run`; INSERT ⇒ event with `row: Some(...)` (refetched body, correct pk + tenant key); UPDATE crossing tenants ⇒ `tenant_id` new + `old_tenant_id` old; DELETE ⇒ `row: None`, old tenant key present.
- `refetch_of_an_already_deleted_row_degrades_to_keys_only` — insert+delete in one transaction; the insert event may arrive after the row is gone; assert the event still arrives with `row: None` (fail-open on body, fail-closed on scope: scope keys came from the NOTIFY payload).

- [ ] 2. Run with the stock container — expected FAIL (module missing).
- [ ] 3. Implement `triggers.rs`:

```rust
//! The FALLBACK change source: generated AFTER-row triggers → pg_notify →
//! sqlx PgListener. Payload carries table/op/pk/scope keys only (8KB NOTIFY
//! cap); the adapter refetches the row body for insert/update. Multi-node is
//! free — every node LISTENs; Postgres is the bus (delivery here goes
//! hub-local, never onto the realtime bus).

/// Idempotent DDL: per spec table, the notify function + trigger from
/// changes/mod.rs templates, executed through the data-layer Db
/// (execute_unprepared, statements split on the `;\n` the template emits).
pub(crate) async fn ensure_triggers(
    db: &jerrycan_db::Db,
    specs: &[crate::ChangeChannelSpec],
) -> jerrycan_core::Result<()> { ... }

pub(crate) struct TriggerAdapter {
    pub(crate) db: jerrycan_db::Db,
    pub(crate) url: String,
    pub(crate) specs: Vec<crate::ChangeChannelSpec>,
}

impl TriggerAdapter {
    /// PgListener::connect(url) → listen("jc_changes") → loop recv():
    /// parse NotifyPayload → find the spec by table → for insert/update,
    /// refetch: SELECT row_to_json(t)::text FROM (SELECT * FROM "{table}"
    /// WHERE "{pk}"::text = $1) t — through the sea-orm Db, $1 bound.
    /// Missing row ⇒ keys-only event. Listener errors reconnect with the same
    /// 1s→30s backoff as replication (PgListener auto-reconnects across
    /// recv() by default; keep the explicit supervision for the connect).
    pub(crate) async fn run(
        self,
        events: tokio::sync::mpsc::Sender<super::ChangeEvent>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) { ... }
}
```

Statement-splitting note: `trigger_sql` returns two statements; execute the
DROP and CREATE separately (sea-orm's `execute_unprepared` handles one
statement per call on Postgres reliably — split on the template's `;\n`).

- [ ] 4. Run with the stock container: `... --test changes_pg -- --ignored --test-threads=1` — expected PASS ×3. Plain `cargo test -p jerrycan-realtime` still green without any container.
- [ ] 5. Commit: `realtime: trigger + LISTEN/NOTIFY fallback adapter with row refetch`

---

## Task 17: source auto-selection, startup surfacing, JC0530/JC0531

Detect-replication-else-triggers at startup; the outcome is loud; sqlite is a
coded diagnostic. The two new JC codes must be registered in `codes.rs` in the
SAME commit that introduces the emitting strings — the platform's
`every_emitted_code_is_in_the_registry` test walks every crate's `src/`.

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (supervisor: selection + adapter spawn + resync fan-out; `Hub::deliver` Change/Resync arms)
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/changes/mod.rs` (the `detect` function)
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/codes.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/docs/ai/13-error-codes.md` + `/Users/sorcecoder/github/jerrycan/crates/jerrycan/embedded/ai/13-error-codes.md`

- [ ] 1. Failing tests. (a) Unit, in `changes/mod.rs`:

```rust
#[tokio::test]
async fn detect_rejects_sqlite_with_jc0530() {
    let db = jerrycan_db::Db::connect("sqlite::memory:").await.unwrap();
    let err = detect(&db).await.unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("JC0530"), "sqlite must be a coded diagnostic: {msg}");
}
```

(b) Loopback, in `tests/ws_live.rs` — joining a changes channel on a
sqlite-backed app answers the JC0530 error envelope (configure `changes` +
sqlite + the header resolver; join `changes:Lead`; expect
`{"op":"error","code":"JC0530"}`).

(c) Platform registry, in `codes.rs` tests (extends the existing suite):

```rust
#[test]
fn realtime_codes_are_registered() {
    assert_eq!(lookup("JC0530").unwrap().title, "realtime requires postgres");
    assert_eq!(lookup("JC0531").unwrap().title, "realtime replication unavailable");
}
```

- [ ] 2. Run `cargo test -p jerrycan-realtime detect` and `cargo test -p jerrycan realtime_codes` — expected FAIL.
- [ ] 3. Implement.

`changes/mod.rs`:

```rust
/// Which change source runs (surfaced in startup logs; spec: detection
/// outcome is loud).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Replication,
    Triggers,
}

/// Startup detection. sqlite ⇒ JC0530 (Changes need Postgres). On Postgres:
/// wal_level=logical AND replication privilege ⇒ Replication, else Triggers
/// with the JC0531 fix-naming diagnostic on stderr. Also warns when
/// max_slot_wal_keep_size is -1 (unbounded WAL retention behind a dead slot).
pub(crate) async fn detect(db: &jerrycan_db::Db) -> jerrycan_core::Result<SourceKind> {
    if db.backend() != jerrycan_db::Backend::Postgres {
        return Err(jerrycan_core::Error::new(
            jerrycan_core::http::StatusCode::INTERNAL_SERVER_ERROR,
            "JC0530",
            "realtime changes require Postgres (JERRYCAN_DATABASE_URL is not postgres://)",
        ));
    }
    let wal_level: String = /* query_one SHOW wal_level over db.conn() */;
    let can_replicate: bool = /* CAN_REPLICATE query */;
    if wal_level == "logical" && can_replicate {
        let keep: String = /* SHOW max_slot_wal_keep_size */;
        if keep == "-1" {
            eprintln!(
                "jerrycan-realtime: max_slot_wal_keep_size is -1 (unbounded) — set it \
                 (e.g. 1GB) so a stalled replication slot can never fill the disk"
            );
        }
        Ok(SourceKind::Replication)
    } else {
        eprintln!(
            "jerrycan-realtime: JC0531 logical replication unavailable \
             (wal_level={wal_level}, replication_role={can_replicate}) — falling back to \
             triggers + LISTEN/NOTIFY. One-time host fix for full fidelity: set \
             wal_level=logical (postgresql.conf or `ALTER SYSTEM SET wal_level = 'logical'`) \
             and grant REPLICATION to the app role, then restart Postgres."
        );
        Ok(SourceKind::Triggers)
    }
}
```

(Match `Error::new`'s real signature from core's `error.rs` — the existing
JC0408 construction in `serve.rs` shows the pattern.)

lib.rs supervisor additions (in the Task 8 `on_serve`, before the pump loop):

```rust
// Postgres Changes: pick the source, own the DDL, spawn the adapter.
if !hub.config.changes.is_empty() {
    match hub.db.as_ref() {
        None => eprintln!("jerrycan-realtime: JC0530 changes configured without a database"),
        Some(db) => match changes::detect(db).await {
            Err(e) => eprintln!("jerrycan-realtime: {e:?} — changes channels answer JC0530"),
            Ok(changes::SourceKind::Replication) => {
                eprintln!("jerrycan-realtime: changes source = logical replication (pgoutput)");
                /* spawn run_supervised(...); events → hub: leader publishes
                   BusMessage::Change onto the bus (all nodes deliver);
                   resync → BusMessage::Resync */
            }
            Ok(changes::SourceKind::Triggers) => {
                eprintln!("jerrycan-realtime: changes source = triggers + LISTEN/NOTIFY");
                /* ensure_triggers + spawn TriggerAdapter::run; events →
                   hub.deliver_change DIRECTLY (resolved #10: no bus) */
            }
        },
    }
}
```

`Hub::deliver_change(&ChangeEvent)` — the delivery routing built on Task 5's
filter: for each subscriber of `changes:{entity}`, send
`ServerMsg::Event { channel, payload: {"type": op, "pk": ..., "row": ...} }`
when `change_visible`, and a delete-shaped
`{"type":"delete","pk":...}` when `delete_view_for_old_tenant`. Fill the
`BusMessage::Change`/`Resync` arms of `Hub::deliver`. When detection failed,
mark the hub (`changes_unavailable: AtomicBool`) so `join` on a changes
channel answers the JC0530 envelope.

`codes.rs` registry entries:

```rust
CodeInfo {
    code: "JC0530",
    title: "realtime requires postgres",
    cause: "the design declares realtime changes but the app is running on sqlite",
    fix: "point JERRYCAN_DATABASE_URL at a Postgres database (broadcast/presence channels work without it; changes channels need Postgres)",
    doc: "jerrycan docs realtime",
},
CodeInfo {
    code: "JC0531",
    title: "realtime replication unavailable",
    cause: "wal_level is not 'logical' or the role lacks REPLICATION, so changes run on the trigger + LISTEN/NOTIFY fallback (identical client behavior, weaker delivery guarantee)",
    fix: "set wal_level=logical and grant REPLICATION to the app role, then restart Postgres — realtime upgrades itself on next start",
    doc: "jerrycan docs realtime",
},
```

Add the two rows to `docs/ai/13-error-codes.md` AND its embedded twin at
`crates/jerrycan/embedded/ai/13-error-codes.md` (docsidx serves the embedded
bytes; keep them byte-identical).

- [ ] 4. Run: `cargo test -p jerrycan-realtime`, `cargo test -p jerrycan codes`, and the loopback JC0530 test — expected PASS. The full `cargo test --workspace` green.
- [ ] 5. Commit: `realtime: detect-replication-else-triggers with JC0530/JC0531 diagnostics`

---

## Task 18: the Redis bus (`realtime-redis`)

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/bus_redis.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/src/lib.rs` (`#[cfg(feature = "realtime-redis")] pub(crate) mod bus_redis;`, bus selection in `Extension::register`, pump start in the supervisor)
- Test: `/Users/sorcecoder/github/jerrycan/crates/jerrycan-realtime/tests/redis_bus.rs` (new)

- [ ] 1. Failing live test file:

```rust
#![cfg(feature = "realtime-redis")]
//! Live Redis bus tests (multi-node fan-out). Need a local Redis:
//!
//! ```text
//! docker run --rm -d -p 6379:6379 redis:7
//! cargo test -p jerrycan-realtime --features realtime-redis --test redis_bus -- --ignored
//! ```
```

Tests (full bodies in this task):
- `two_buses_exchange_messages` — two `RedisBus::new(url)` instances (two
  simulated nodes) with their pumps running; `publish` on node A; both A's and
  B's `subscribe()` receivers get the message (echo semantics preserved).
- `two_hubs_fan_out_broadcast_across_nodes` — two full `Realtime` apps on two
  ephemeral ports, both with `JERRYCAN_REDIS_URL` set (pass the URL through a
  `Realtime::redis_url(...)` builder override so tests don't mutate global
  env), a WS client on each; publish on node A's client; node B's client
  receives; A's own client does not (origin exclusion across the bus).

- [ ] 2. Run `cargo test -p jerrycan-realtime --features realtime-redis --test redis_bus -- --ignored` (Redis up) — expected FAIL.
- [ ] 3. Implement `bus_redis.rs`:

```rust
//! Redis pub/sub bus (feature `realtime-redis`): one channel carries the
//! serde-encoded BusMessage stream; every node PUBLISHes and every node's
//! pump forwards received messages into its local broadcast fan-in.
//! Publishing uses a lazily-connected ConnectionManager (register() is sync);
//! subscribing needs a dedicated pub/sub connection (redis 1.x:
//! Client::get_async_pubsub), driven by `run_pump` inside the supervisor.

pub(crate) const CHANNEL: &str = "jc:realtime:bus";

pub(crate) struct RedisBus {
    url: String,
    publish_conn: tokio::sync::OnceCell<redis::aio::ConnectionManager>,
    fanin: tokio::sync::broadcast::Sender<crate::bus::BusMessage>,
}

impl RedisBus {
    pub(crate) fn new(url: String) -> Self { ... }

    pub(crate) async fn publish(&self, msg: crate::bus::BusMessage) -> jerrycan_core::Result<()> {
        let conn = self.publish_conn.get_or_try_init(|| async { /* Client::open + ConnectionManager::new, errors → Error::internal */ }).await?;
        let payload = serde_json::to_string(&msg).expect("bus messages serialize");
        /* PUBLISH CHANNEL payload via conn.clone() */
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<crate::bus::BusMessage> {
        self.fanin.subscribe()
    }

    /// The supervisor-run pump: get_async_pubsub → subscribe(CHANNEL) →
    /// on_message stream → decode → fanin.send. Reconnect with 1s→30s
    /// backoff; undecodable payloads are logged and skipped (version skew
    /// between nodes mid-deploy must not kill the pump).
    pub(crate) async fn run_pump(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) { ... }
}
```

Bus selection in `Extension::register`:

```rust
let bus = {
    #[cfg(feature = "realtime-redis")]
    if let Some(url) = self.redis_url.clone().or_else(|| std::env::var("JERRYCAN_REDIS_URL").ok()) {
        bus::AnyBus::Redis(bus_redis::RedisBus::new(url))
    } else {
        bus::AnyBus::Local(bus::LocalBus::new())
    }
    #[cfg(not(feature = "realtime-redis"))]
    bus::AnyBus::Local(bus::LocalBus::new())
};
```

plus the `Realtime::redis_url(url)` builder setter (test seam + explicit
config) and, in the supervisor, `if let AnyBus::Redis(b) = &hub.bus { spawn
b.run_pump(shutdown.clone()) }` before the pump loop.

- [ ] 4. Run (Redis up): the two ignored tests PASS; `cargo test -p jerrycan-realtime --features realtime-redis` green (non-ignored suite unaffected); `cargo test -p jerrycan-realtime` (no feature) green.
- [ ] 5. Commit: `realtime: Redis pub/sub bus for multi-node fan-out behind realtime-redis`

---

## Task 19: facade features + re-export

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/Cargo.toml`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/lib.rs`

- [ ] 1. Failing check: add a feature-gated smoke test to the facade (e.g. in `crates/jerrycan/src/lib.rs`'s test module or an existing feature-surface test):

```rust
/// The realtime facade surface: `jerrycan::realtime::{Realtime, Principal,
/// TopicScope, ChangeChannelSpec}` must resolve when the feature is on —
/// generated wiring (realtimegen) is compiled against exactly these paths.
#[cfg(all(test, feature = "realtime"))]
mod realtime_facade {
    #[test]
    fn facade_paths_resolve() {
        fn _typecheck(rt: crate::realtime::Realtime) -> crate::realtime::Realtime {
            rt.broadcast("room", crate::realtime::TopicScope::Tenant)
        }
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan --features realtime realtime_facade` — expected FAIL (unknown feature).
- [ ] 3. Implement. `crates/jerrycan/Cargo.toml`:

```toml
# Realtime channels (Postgres Changes + Broadcast + Presence over WebSockets).
# Changes stream from the database, so `realtime` implies `db` (like jobs).
realtime = ["dep:jerrycan-realtime", "db"]
# Multi-node realtime fan-out over Redis (mirrors jobs-redis).
realtime-redis = ["realtime", "jerrycan-realtime/realtime-redis"]
```

plus `jerrycan-realtime = { workspace = true, optional = true }` under
`[dependencies]`. `crates/jerrycan/src/lib.rs` (after the `jobs` re-export):

```rust
#[cfg(feature = "realtime")]
pub use jerrycan_realtime as realtime;
```

- [ ] 4. Run `cargo test -p jerrycan --features realtime` and `cargo check -p jerrycan --features realtime-redis` — expected PASS.
- [ ] 5. Commit: `facade: realtime and realtime-redis features`

---

## Task 20: platform model — `RealtimeDesign` in `design.rs` + the contract schema

**Verify the storage preconditions from the COORDINATION section first.**

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/design.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/docs/contracts/design-schema.json`

- [ ] 1. Failing tests (in `design.rs`'s test module — mirror the `wants_jobs`/`wants_oauth` tests exactly):

```rust
pub(crate) const V2_REALTIME: &str = r#"{
    "name": "rt-app", "contract_version": 2,
    "auth": { "model": "jwt", "roles": ["owner", "member"] },
    "dependencies": ["db", "auth"],
    "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
    "realtime": {
        "changes": ["Lead"],
        "broadcast": [{ "name": "deal_room", "scope": "tenant" }],
        "presence": [{ "name": "editors", "scope": "tenant" }]
    },
    "modules": [
        { "name": "workspaces",
          "entities": [{ "name": "Workspace", "fields": [
              { "name": "id", "type": "integer" }, { "name": "name", "type": "string" } ]}],
          "endpoints": [{ "operation_id": "list_workspaces", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Workspace", "list": true } }] },
        { "name": "leads",
          "entities": [{ "name": "Lead",
              "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
              "fields": [{ "name": "id", "type": "integer" },
                         { "name": "phone", "type": "string" }] }],
          "endpoints": [{ "operation_id": "list_leads", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Lead", "list": true } }] }
    ]
}"#;

#[test]
fn realtime_block_round_trips_and_gates_the_facade_feature() {
    let d: Design = serde_json::from_str(V2_REALTIME).unwrap();
    assert!(d.wants_realtime());
    let rt = d.realtime.as_ref().unwrap();
    assert_eq!(rt.changes, vec!["Lead"]);
    assert_eq!(rt.broadcast[0].name, "deal_room");
    assert_eq!(rt.broadcast[0].scope, RealtimeScope::Tenant);
    let feats = d.facade_features();
    assert!(feats.contains(&"realtime"), "{feats:?}");
    assert_eq!(feats.last(), Some(&"realtime"), "realtime is appended last (after storage): {feats:?}");
    // Round trip.
    let back = serde_json::to_string(&d).unwrap();
    let re: Design = serde_json::from_str(&back).unwrap();
    assert!(re.wants_realtime());
    // Absent block ⇒ no feature (v0/v1 designs untouched).
    let plain: Design = serde_json::from_str(MINIMAL).unwrap();
    assert!(!plain.wants_realtime());
    assert!(!plain.facade_features().contains(&"realtime"));
}

#[test]
fn published_schema_accepts_the_realtime_block() {
    let s = include_str!("../../../../docs/contracts/design-schema.json");
    assert!(s.contains("\"realtime\"") && s.contains("\"broadcast\"") && s.contains("\"presence\""));
}
```

- [ ] 2. Run `cargo test -p jerrycan realtime_block` — expected FAIL.
- [ ] 3. Implement. `design.rs` (alongside `JobDesign`, mirroring storage's block):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeDesign {
    /// Entity names whose row changes are subscribable (published +
    /// REPLICA IDENTITY FULL + scope-filtered delivery).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broadcast: Vec<RealtimeTopic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presence: Vec<RealtimeTopic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeTopic {
    pub name: String,
    pub scope: RealtimeScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RealtimeScope {
    None,
    Tenant,
    Auth,
}
```

Add to `Design`: `#[serde(default, skip_serializing_if = "Option::is_none")]
pub realtime: Option<RealtimeDesign>,` (after `storage`), plus:

```rust
/// The `realtime` block switches on the realtime crate wiring + the facade
/// `realtime` feature. Like jobs, the block itself is the declaration.
pub fn wants_realtime(&self) -> bool {
    self.realtime
        .as_ref()
        .is_some_and(|r| !r.changes.is_empty() || !r.broadcast.is_empty() || !r.presence.is_empty())
}
```

and in `facade_features()`, after the storage push: `if self.wants_realtime() {
features.push("realtime"); }` (appended LAST so existing feature order is
unchanged — same comment discipline as oauth/storage).

`design-schema.json`: add under `properties` (sibling of `jobs`/`storage`):

```json
"realtime": {
  "type": "object",
  "additionalProperties": false,
  "description": "Realtime channels (contract v2): row-change subscriptions (scope-filtered by owner/tenant), ephemeral broadcast topics, and presence topics, served over one WebSocket endpoint at /realtime.",
  "properties": {
    "changes": {
      "type": "array",
      "items": { "type": "string", "pattern": "^[A-Z][A-Za-z0-9]*$" },
      "description": "Entity names whose row changes are subscribable. Requires db + an active auth model; delivery is tenant-filtered when the entity is tenant-owned."
    },
    "broadcast": { "type": "array", "items": { "$ref": "#/$defs/realtime_topic" } },
    "presence": { "type": "array", "items": { "$ref": "#/$defs/realtime_topic" } }
  }
},
```

and in `$defs`:

```json
"realtime_topic": {
  "type": "object",
  "required": ["name", "scope"],
  "additionalProperties": false,
  "properties": {
    "name": { "type": "string", "pattern": "^[a-z][a-z0-9_]*$" },
    "scope": {
      "enum": ["none", "tenant", "auth"],
      "description": "Who may join/publish: none = public, auth = any authenticated principal, tenant = tenant members (delivery partitioned per tenant)."
    }
  }
}
```

- [ ] 4. Run `cargo test -p jerrycan platform::design` — expected PASS (including storage's existing schema spot-checks).
- [ ] 5. Commit: `platform: realtime block in the design contract (v2)`

---

## Task 21: platform validation (`questions.rs`)

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/questions.rs`

- [ ] 1. Failing tests (using `V2_REALTIME` from design.rs — export it like `MINIMAL`/`V1_FULL`):

```rust
#[test]
fn realtime_requires_contract_v2() {
    let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
    d.contract_version = 1;
    assert!(validate(&d).iter().any(|q| q.id == "/realtime" && q.question.contains("contract_version")));
}

#[test]
fn realtime_changes_entities_must_exist() {
    let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
    d.realtime.as_mut().unwrap().changes[0] = "Ghost".into();
    assert!(validate(&d).iter().any(|q| q.id == "/realtime/changes/0" && q.question.contains("Ghost")));
}

#[test]
fn realtime_requires_db_and_changes_require_active_auth() {
    let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
    d.dependencies.retain(|x| x != "db");
    assert!(validate(&d).iter().any(|q| q.id == "/realtime" && q.question.contains("db")));

    let mut d2: Design = serde_json::from_str(V2_REALTIME).unwrap();
    d2.auth = None;
    // (tenancy also complains; assert the realtime-specific question exists)
    assert!(validate(&d2).iter().any(|q| q.id == "/realtime/changes" && q.question.contains("auth")));
}

#[test]
fn tenant_scoped_topics_require_tenancy_and_snake_case_unique_names() {
    let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
    d.tenancy = None;
    assert!(validate(&d).iter().any(|q| q.id == "/realtime/broadcast/0" && q.question.contains("tenancy")));

    let mut d2: Design = serde_json::from_str(V2_REALTIME).unwrap();
    d2.realtime.as_mut().unwrap().broadcast.push(RealtimeTopic { name: "Deal-Room".into(), scope: RealtimeScope::None });
    assert!(validate(&d2).iter().any(|q| q.id == "/realtime/broadcast/1" && q.question.contains("snake_case")));

    let mut d3: Design = serde_json::from_str(V2_REALTIME).unwrap();
    let dup = d3.realtime.as_ref().unwrap().broadcast[0].clone();
    d3.realtime.as_mut().unwrap().broadcast.push(dup);
    assert!(validate(&d3).iter().any(|q| q.id == "/realtime/broadcast/1" && q.question.contains("unique")));
}

#[test]
fn valid_realtime_design_is_question_free() {
    let d: Design = serde_json::from_str(V2_REALTIME).unwrap();
    assert!(validate(&d).is_empty(), "{:?}", validate(&d));
}
```

- [ ] 2. Run `cargo test -p jerrycan questions` — expected FAIL.
- [ ] 3. Implement in `validate()` (after the jobs section, gated on `d.realtime.is_some()`):
  - `contract_version < 2` ⇒ question at `/realtime` ("realtime is a contract v2 construct — set contract_version to 2").
  - block present but no `db` dependency ⇒ `/realtime` ("realtime requires a database dependency — add `db` (Changes stream from Postgres)"). Mirror the jobs-require-db phrasing.
  - each `changes[i]` must be a declared entity (reuse the collected `entity_names` set) ⇒ `/realtime/changes/{i}`.
  - `changes` non-empty without an active auth model (same `active_auth_model` derivation the tenancy check uses) ⇒ `/realtime/changes` ("changes delivery is scope-filtered by the authenticated principal — set auth.model to session or jwt").
  - per topic (broadcast then presence, index-addressed): name `is_snake` ⇒ else question; unique within its list ⇒ else question; `scope == tenant` requires `d.tenancy.is_some()` ⇒ else question; `scope != none` requires an active auth model ⇒ else question.
- [ ] 4. Run `cargo test -p jerrycan questions` — expected PASS (all existing tests untouched).
- [ ] 5. Commit: `platform: validate the realtime block (v2-gated)`

---

## Task 22: the generator (`realtimegen.rs`)

Emits the tool-owned `crates/realtime/` wiring crate, mirroring `jobsgen.rs`
exactly: `Cargo.toml` + `src/lib.rs` + `tests/acceptance.rs`, ALL tool-owned
(realtime has no agent-authored task bodies — regeneration rewrites
everything). The lib exports `pub fn realtime(db: jerrycan::db::Db) ->
jerrycan::realtime::Realtime` carrying: the principal resolver (auth-model-
specific), one `.changes(...)` per entity (table/pk/tenant column derived from
the design exactly like genroute's DDL derivation), and the topic wiring.

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/realtimegen.rs`
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/mod.rs` (`pub mod realtimegen;`)

- [ ] 1. Failing tests (in `realtimegen.rs`, mirroring jobsgen's suite):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::Design;

    fn rt_design() -> Design {
        serde_json::from_str(crate::platform::design::tests::V2_REALTIME).unwrap()
    }

    #[test]
    fn wiring_is_deterministic_and_derives_table_pk_and_tenant_column() {
        let d = rt_design();
        let a = wiring_rs(&d);
        assert_eq!(a, wiring_rs(&d), "byte-identical across runs (JL0003 contract)");
        assert!(a.contains("pub fn realtime(db: jerrycan::db::Db) -> jerrycan::realtime::Realtime"), "{a}");
        // Lead belongs_to Workspace (the tenancy entity) ⇒ tenant filter on workspace_id.
        assert!(a.contains(r#"entity: "Lead".to_string()"#), "{a}");
        assert!(a.contains(r#"table: "lead".to_string()"#), "{a}");
        assert!(a.contains(r#"pk_column: "id".to_string()"#), "{a}");
        assert!(a.contains(r#"tenant_column: Some("workspace_id".to_string())"#), "{a}");
        assert!(a.contains(r#".broadcast("deal_room", jerrycan::realtime::TopicScope::Tenant)"#), "{a}");
        assert!(a.contains(r#".presence("editors", jerrycan::realtime::TopicScope::Tenant)"#), "{a}");
    }

    #[test]
    fn jwt_resolver_reads_bearer_then_token_query_and_resolves_tenant() {
        let a = wiring_rs(&rt_design()); // V2_REALTIME is jwt + tenancy
        assert!(a.contains("shared::Tenant"), "tenancy design resolves the Tenant guard: {a}");
        assert!(a.contains("token"), "jwt designs accept ?token= (browsers can't set WS headers): {a}");
        assert!(a.contains("jerrycan::auth::jwt::decode"), "{a}");
    }

    #[test]
    fn non_tenant_entity_gets_no_tenant_column_and_session_model_uses_current_user() {
        let mut d = rt_design();
        d.tenancy = None;
        d.auth.as_mut().unwrap().model = crate::platform::design::AuthModel::Session;
        d.modules[1].entities[0].belongs_to.clear();
        let a = wiring_rs(&d);
        assert!(a.contains("tenant_column: None"), "{a}");
        assert!(a.contains("shared::CurrentUser"), "{a}");
        assert!(!a.contains("shared::Tenant"), "{a}");
    }

    #[test]
    fn acceptance_tests_are_ignored_live_pg_and_carry_the_negative_control() {
        let a = acceptance_rs(&rt_design());
        assert!(a.contains("#[ignore]"), "realtime acceptance needs live Postgres: {a}");
        assert!(a.contains("JERRYCAN_TEST_DATABASE_URL"), "{a}");
        assert!(a.contains("cross_tenant"), "the negative control is generated, not optional: {a}");
        assert!(a.contains("changes:Lead"), "{a}");
        assert_eq!(a, acceptance_rs(&rt_design()), "deterministic");
    }

    #[test]
    fn write_realtime_is_tool_owned_and_rewrites_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let d = rt_design();
        let created = write_realtime(tmp.path(), &d).unwrap();
        assert!(created.contains(&"crates/realtime/Cargo.toml".to_string()));
        assert!(created.contains(&"crates/realtime/src/lib.rs".to_string()));
        assert!(created.contains(&"crates/realtime/tests/acceptance.rs".to_string()));
        // Tool-owned: a hand edit is rewritten (no agent-owned files here).
        let lib = tmp.path().join("crates/realtime/src/lib.rs");
        std::fs::write(&lib, "// hand edit\n").unwrap();
        write_realtime(tmp.path(), &d).unwrap();
        assert!(std::fs::read_to_string(&lib).unwrap().contains("pub fn realtime("));
    }
}
```

- [ ] 2. Run `cargo test -p jerrycan realtimegen` — expected FAIL.
- [ ] 3. Implement `realtimegen.rs` with:
  - `cargo_toml()` — package `realtime`, deps `jerrycan.workspace = true`,
    `shared = { path = "../shared" }` (the resolver uses shared types),
    `serde_json.workspace = true`; dev-dep `tokio.workspace = true`
    (mirrors jobsgen's shape, plus shared).
  - `resolver_rs(design) -> String` — the principal closure per auth model:
    - **tenancy declared** (validation guarantees an active model): resolve
      `shared::Tenant` via `ctx.resolve::<shared::Tenant>()` (the app-wide
      `provide_dep(shared::tenant)` factory — 401/403 semantics ride along),
      plus the user id from the model-specific extractor; emit
      `Principal { user_id, tenant_id: Some(tenant.id().to_string()), role: Some(tenant.role.clone()) }`.
    - **jwt, no tenancy**: try `<shared::CurrentUser as jerrycan::FromRequest>::from_request(ctx)`
      (Bearer header); on 401, fall back to the `token` query parameter
      decoded with `jerrycan::auth::jwt::decode::<shared::SessionUser>(&token,
      ctx.resolve::<jerrycan::auth::Auth>().await?.jwt_key())`.
    - **session, no tenancy**: `shared::CurrentUser` only (cookies ride the
      upgrade request).
    - **no auth model** (validation only allows all-`none` topics): no
      `.principal(...)` call at all.
  - `changes_spec(design, entity) -> (table, pk, tenant_column)` — table =
    `Design::to_snake(entity)`; pk = the entity's declared `id` (or the
    synthetic default `"id"`); tenant column = `Design::fk_column(&tenancy.entity)`
    when the entity `belongs_to` the tenancy entity (walk modules + subroutes
    exactly like `tenant_owned()`).
  - `wiring_rs(design)` — the header comment `//! GENERATED by jerrycan — the
    realtime channel wiring. TOOL-OWNED: 'jerrycan generate' rewrites this
    file.`, `#![forbid(unsafe_code)]`, and the `realtime(db)` fn chaining
    builder calls in design order (changes array order, then broadcast, then
    presence — deterministic).
  - `acceptance_rs(design)` — `#[ignore]`d `#[tokio::test]`s with a header
    documenting `JERRYCAN_TEST_DATABASE_URL=postgres://… cargo test -p realtime
    -- --ignored`: per changes entity a subscribe→insert-row→assert-event test
    AND the `cross_tenant_change_never_arrives_{entity}` negative control
    (insert as tenant B, assert tenant A's socket stays silent through a
    heartbeat round-trip); per broadcast/presence topic a round-trip test.
    These drive a real WS client (dev-dep `tokio-tungstenite` rides through
    the facade? No — generated apps may not depend on it; use the
    `jerrycan::realtime` test client seam: emit the tests against
    `tokio-tungstenite` as a dev-dependency of the generated realtime crate,
    declared in `cargo_toml()`'s `[dev-dependencies]` — it is workspace-pinned
    in generated apps? It is NOT: generated apps have their own workspace.
    Resolution: `cargo_toml()` pins `tokio-tungstenite = { version = "0.30",
    default-features = false, features = ["handshake", "connect"] }` directly
    in `[dev-dependencies]`.)
  - `write_realtime(target, design) -> Result<Vec<String>, String>` — all
    three files tool-owned, rewritten every run.
- [ ] 4. Run `cargo test -p jerrycan realtimegen` — expected PASS.
- [ ] 5. Commit: `platform: realtimegen emits the tool-owned realtime wiring crate`

---

## Task 23: mounting + regeneration wiring (`mounting.rs`)

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/mounting.rs`

- [ ] 1. Failing tests (mirror the jobs mounting tests):

```rust
fn realtime_design() -> Design {
    serde_json::from_str(crate::platform::design::tests::V2_REALTIME).unwrap()
}

#[test]
fn expected_main_wires_realtime_extension_before_db_move() {
    let main = expected_main(&realtime_design());
    let rt = main.find(".extend(realtime::realtime(db.clone()))").unwrap();
    let db = main.find(".extend(db)\n").unwrap();
    assert!(rt < db, "realtime registers with a db CLONE before .extend(db) moves it: {main}");
    // No stub comment for the reserved name.
    assert!(!main.contains("app dependency `realtime`"), "{main}");
}

#[test]
fn expected_main_without_realtime_has_no_realtime_wiring() {
    let mut d = realtime_design();
    d.realtime = None;
    assert!(!expected_main(&d).contains("realtime::realtime"));
}

#[test]
fn regenerate_adds_realtime_member_and_route_dep_and_removes_when_dropped() {
    // tempdir scaffold-shaped fixture: Cargo.toml with the members markers,
    // crates/app/Cargo.toml with the route-deps markers (copy the pattern the
    // existing jobs regenerate test uses). Assert: with realtime, members
    // contains "crates/realtime" and app deps contain
    // `realtime = { path = "../realtime" }` and crates/realtime/ exists;
    // regenerating after clearing d.realtime removes the directory.
}
```

- [ ] 2. Run `cargo test -p jerrycan mounting` — expected FAIL.
- [ ] 3. Implement in `mounting.rs`, each mirroring the jobs lines:
  - reserved-name filter: `!matches!(d.as_str(), "db" | "validate" | "auth" | "observe" | "storage" | "realtime")` (storage already added by its plan).
  - `extension_block`: after the jobs block, before `.extend(db)`:
    ```rust
    // Realtime needs the db (Changes + DDL reconcile): register with a CLONE
    // before `.extend(db)` moves it. has_realtime implies wants_db (questions.rs).
    if design.wants_realtime() {
        block.push_str("        .extend(realtime::realtime(db.clone()))\n");
    }
    ```
  - `regenerate()` step 1e (after the jobs crate step): `write_realtime` when
    `wants_realtime()`, else remove a stale `crates/realtime/` (byte-for-byte
    the jobs pattern).
  - members splice: `members.push_str("    \"crates/realtime\",\n")` after the
    jobs member; app route-deps splice: `deps.push_str("realtime = { path = \"../realtime\" }\n")`.
- [ ] 4. Run `cargo test -p jerrycan mounting` + the full `cargo test -p jerrycan` (scaffold/generate conformance fixtures that assert `expected_main` bytes may need refreshing — regenerate any snapshot the suite owns and inspect the diff is realtime-only). Expected PASS.
- [ ] 5. Commit: `platform: mount generated realtime crate and wire the extension`

---

## Task 24: docs — `docs/ai/18-realtime.md` + docsidx

**Files**
- Create: `/Users/sorcecoder/github/jerrycan/docs/ai/18-realtime.md`
- Create: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/embedded/ai/18-realtime.md` (byte-identical copy)
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/platform/docsidx.rs` (PAGES entry `("realtime", include_str!("../../embedded/ai/18-realtime.md"))`)
- Modify: `/Users/sorcecoder/github/jerrycan/crates/jerrycan/src/lib.rs` (doc_page gate)

- [ ] 1. Failing test: the docsidx search test suite (existing) plus a new spot-check:

```rust
#[test]
fn realtime_page_is_served() {
    assert!(PAGES.iter().any(|(t, body)| *t == "realtime" && body.contains("changes:")));
}
```

- [ ] 2. Run `cargo test -p jerrycan docsidx` — expected FAIL.
- [ ] 3. Write the page. Contents (jargon-free, per the repo's copy rule): what realtime gives you (live row changes, messages between clients, who's-online lists); the `design.json` block with the V2_REALTIME example; the wire protocol with a full join→event transcript; how delivery is filtered ("you only receive what you could GET"); the two change sources and the one-time `wal_level=logical` host fix (JC0531); Postgres requirement for changes (JC0530); multi-node via `realtime-redis`; browser auth (`?token=`); delivery guarantee (live-UI at-most-once, refetch on reconnect/resync). Every Rust snippet compile-checked: gate the page in `lib.rs`:

```rust
#[cfg(all(feature = "realtime", feature = "auth"))]
doc_page!(page_18_realtime, "../../../docs/ai/18-realtime.md");
```

- [ ] 4. Run `cargo test -p jerrycan docsidx` and `cargo test -p jerrycan --features realtime,auth --doc` — expected PASS.
- [ ] 5. Commit: `docs: realtime guide (18-realtime.md) served and doc-tested`

---

## Task 25: the eval gate — reference design + battery

The reference Supabase-shaped slice gains realtime channels; the eval drives a
real WS client: subscribe → mutate over HTTP → the scoped event arrives; the
cross-tenant negative control must NOT arrive. Un-skippable in CI +
pre-publish (spec: eval gate).

**Files**
- Modify: `/Users/sorcecoder/github/jerrycan/conformance/designs/reference-slice.design.json`
- Modify: `/Users/sorcecoder/github/jerrycan/conformance/eval/PROTOCOL.md` + the reference spec under `/Users/sorcecoder/github/jerrycan/conformance/eval/specs/`
- Modify: whatever conformance fixtures the platform test suite derives from the reference design (run the suite; refresh ONLY realtime-attributable diffs)

- [ ] 1. Failing test: extend the reference design —

```json
"realtime": {
  "changes": ["Lead"],
  "broadcast": [{ "name": "deal_room", "scope": "tenant" }],
  "presence": [{ "name": "editors", "scope": "tenant" }]
}
```

and bump `contract_version` to 2 **if storage has not already** (COORDINATION
precondition — with storage landed it already reads 2). Run
`cargo test -p jerrycan` — the reference-driven suites (questions'
`reference_shaped..._question_free`, jobsgen fixtures, scaffold snapshots) now
exercise the realtime block; expected state: everything passes EXCEPT
snapshots that legitimately gained realtime wiring — refresh those and verify
each diff is realtime-only.
- [ ] 2. Regenerate the reference app (`jerrycan generate` over the reference design per the eval PROTOCOL) and confirm the generated `crates/realtime/` compiles inside the generated workspace: `cargo check` in the generated app must be green with the `realtime` facade feature auto-enabled by `sync_facade_features`.
- [ ] 3. Extend the eval battery per `conformance/eval/PROTOCOL.md` conventions: the realtime step list —
  1. serve the migrated app against the eval's `wal_level=logical` Postgres;
  2. login two users in two different workspaces (tenants) over HTTP;
  3. open two WS clients (`?token=`), both `join` `changes:Lead`;
  4. POST a lead as tenant-A over HTTP → assert tenant-A's socket receives the
     insert event with the row body within 10s;
  5. **negative control**: assert tenant-B's socket receives nothing for it
     (heartbeat round-trip proves silence) — a leak turns the gate red;
  6. broadcast round-trip on `deal_room` within tenant A; cross-tenant silence
     on tenant B;
  7. presence: track on `editors`, second same-tenant client sees state+diff;
  8. repeat step 4-5 once against a stock-Postgres run (trigger fallback) —
     same client-visible behavior, proving "identical behavior; only the
     source differs".
  Also run the generated `crates/realtime/tests/acceptance.rs` with
  `JERRYCAN_TEST_DATABASE_URL` pointed at the eval database
  (`cargo test -p realtime -- --ignored`).
- [ ] 4. Run the full workspace suite + the eval battery; expected PASS. Record the realtime rows in `conformance/eval/results.md` per its format.
- [ ] 5. Commit: `eval: realtime channels in the reference slice with cross-tenant negative control`

---

## Execution order & gating summary

| Tasks | Needs | CI-runnable without services |
|---|---|---|
| 1–14 | nothing external | yes (all unit/loopback) |
| 15 | Postgres w/ `wal_level=logical` (docker cmd in test header) | ignored tests; crate suite still green |
| 16 | stock Postgres | ignored tests |
| 17 | nothing (sqlite path unit-tested) | yes |
| 18 | Redis | ignored tests behind `realtime-redis` |
| 19–24 | nothing external | yes |
| 25 | the eval battery's Postgres containers | eval job only |

Every task ends with `cargo test --workspace` (default features) green and
`cargo clippy --workspace --all-targets -- -D warnings` clean.

## Top risks (ordered)

1. **The hand-rolled replication socket (Tasks 14–15).** Mainline
   tokio-postgres genuinely lacks replication support, so wire.rs carries real
   protocol surface: SCRAM, TLS preamble, CopyBoth framing, error-code
   mapping. Mitigation: postgres-protocol does all crypto/encoding; framing
   and decoding are pure and unit-tested; auth paths get dedicated live tests
   (trust + scram at minimum — the docker image defaults to scram-sha-256).
   If SCRAM misbehaves against a managed provider, the trigger fallback keeps
   every app functional while it's fixed.
2. **Presence merge semantics across nodes** (Task 10/18): last-writer-wins +
   node-expiry is deliberately simpler than Phoenix's CRDT; multi-meta-per-key
   and clock-skew edge cases are documented away for v1. The merge functions
   are pure and heavily unit-tested to keep the invariants pinned.
3. **hyper upgrade integration** (Task 2/8): `with_upgrades()` changes the
   connection type the drain loop pins; the graceful-shutdown path and the
   write-stall `TimedIo` now sit under long-lived WS connections. The loopback
   suite plus core's raw-upgrade test cover it, but watch the 10s drain cap —
   WS tasks spawned by handlers are detached from the JoinSet by design
   (documented in ws.rs; live-UI clients reconnect).
4. **Slot lifecycle under failure** (Task 15): invalidation detection depends
   on SQLSTATE mapping from ErrorResponse; wrong mapping degrades to plain
   reconnect-backoff (safe, but no auto-recreate). The ignored
   `slot_invalidation_recreates_and_emits_resync` test is the guard — do not
   skip it in the eval job.
5. **Reference-fixture churn** (Task 25): the reference design feeds many
   platform snapshots; every refreshed fixture must be diff-reviewed as
   realtime-only.
