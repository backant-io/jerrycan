//! Realtime extension for jerrycan: Postgres Changes + Broadcast + Presence
//! over one WebSocket endpoint, with mandatory scope-filtered delivery.
//! <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub(crate) mod broadcast;
pub(crate) mod bus;
#[cfg(feature = "realtime-redis")]
pub(crate) mod bus_redis;
pub mod changes;
pub(crate) mod channel;
pub(crate) mod presence;
pub mod protocol;
pub(crate) mod ws;

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
///
/// `#[non_exhaustive]` + the [`ChangeChannelSpec::new`] builder (0.7.3): the
/// generated realtime wiring (realtimegen) constructs this DOWNSTREAM, so a
/// struct literal would make every field-add a breaking change. The builder
/// keeps field-adds a non-breaking minor forever — `owner_column` (#216) was
/// the first such add. The defining crate still literal-constructs it (in-crate
/// literals are exempt from `#[non_exhaustive]`), so the unit/DDL tests below
/// keep their literals; only cross-crate callers must use the builder.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ChangeChannelSpec {
    pub entity: String,
    pub table: String,
    pub pk_column: String,
    /// The tenant fk column when the entity is tenant-owned; None ⇒ the entity
    /// is not tenant-scoped (see `owner_column` for the per-user case).
    pub tenant_column: Option<String>,
    /// The identity fk column (e.g. `user_id`) when the entity is per-user
    /// owned (#79/#216) and NOT tenant-scoped; None ⇒ genuinely auth-only.
    /// Set only when `tenant_column` is None: a per-user `changes` event is
    /// delivered ONLY to the row's owner (`ev.owner_id == principal.user_id`),
    /// mirroring the REST `get_for`/`all_for` owner-scoping. Without it, a
    /// per-user entity's changes broadcast every user's rows to every
    /// authenticated subscriber — the cross-user leak #216 closes.
    pub owner_column: Option<String>,
    /// Column names stripped from the broadcast row before it reaches any
    /// subscriber — the entity's `write_only`/`password_hash` columns (#167).
    /// Empty ⇒ the full row is broadcast (byte-identical to pre-0.6.18). The
    /// value still transits the engine's own process memory (decoded from the
    /// WAL tuple or SELECTed by the trigger) but is NEVER sent to a subscriber,
    /// which is the security guarantee that lets a `write_only` column coexist
    /// with a `changes` channel.
    pub hidden_columns: Vec<String>,
}

impl ChangeChannelSpec {
    /// Start a spec from the three required identifiers (entity/table/pk); scope
    /// keys and hidden columns default to none. The cross-crate constructor
    /// (`#[non_exhaustive]` forbids struct literals downstream) — chain
    /// `.tenant_column(..)`, `.owner_column(..)`, `.hidden_columns(..)` as needed.
    pub fn new(
        entity: impl Into<String>,
        table: impl Into<String>,
        pk_column: impl Into<String>,
    ) -> Self {
        Self {
            entity: entity.into(),
            table: table.into(),
            pk_column: pk_column.into(),
            tenant_column: None,
            owner_column: None,
            hidden_columns: Vec::new(),
        }
    }

    /// Set the tenant fk column (the entity is tenant-owned). Mutually exclusive
    /// with `owner_column` in practice — realtimegen sets at most one.
    pub fn tenant_column(mut self, column: Option<String>) -> Self {
        self.tenant_column = column;
        self
    }

    /// Set the identity fk column (the entity is per-user owned, #216).
    pub fn owner_column(mut self, column: Option<String>) -> Self {
        self.owner_column = column;
        self
    }

    /// Set the columns stripped from the broadcast row before delivery (#167).
    pub fn hidden_columns(mut self, columns: Vec<String>) -> Self {
        self.hidden_columns = columns;
        self
    }
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
    pub(crate) resolver: Option<PrincipalResolver>,
    /// Explicit Redis URL for the multi-node bus (else `JERRYCAN_REDIS_URL`).
    /// Only consulted under the `realtime-redis` feature.
    pub(crate) redis_url: Option<String>,
}

impl Realtime {
    pub fn new(db: jerrycan_db::Db) -> Self {
        Self {
            db: Some(db),
            ..Self::builder()
        }
    }

    pub fn builder() -> Self {
        Self {
            db: None,
            mount: "/realtime".into(),
            config: RealtimeConfig::default(),
            resolver: None,
            redis_url: None,
        }
    }

    /// Set the Redis URL for the multi-node fan-out bus (mirrors `jobs-redis`).
    /// Takes precedence over `JERRYCAN_REDIS_URL`; only consulted with the
    /// `realtime-redis` feature enabled.
    pub fn redis_url(mut self, url: &str) -> Self {
        self.redis_url = Some(url.to_string());
        self
    }

    /// Install the principal resolver: it authenticates the connection at
    /// upgrade time (session cookie / bearer / `?token=`). Generated wiring
    /// supplies it; an absent resolver ⇒ anonymous connections (only scope-none
    /// channels joinable).
    pub fn principal(mut self, resolver: PrincipalResolver) -> Self {
        self.resolver = Some(resolver);
        self
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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Resolves the connection's principal at upgrade time from the request
/// (session cookie / bearer / `?token=`). Generated wiring supplies it; an
/// absent resolver ⇒ anonymous connections (only scope-none channels joinable).
pub type PrincipalResolver = Arc<
    dyn for<'a> Fn(
            &'a mut jerrycan_core::RequestCtx,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = jerrycan_core::Result<Principal>> + Send + 'a>,
        > + Send
        + Sync,
>;

/// Per-connection outbound queue capacity; a full queue disconnects the client
/// (at-most-once live-UI semantics — resolved decision #12).
const CONN_QUEUE: usize = 128;

pub(crate) struct Subscriber {
    pub(crate) principal: Option<Principal>,
    pub(crate) tx: tokio::sync::mpsc::Sender<crate::protocol::ServerMsg>,
    pub(crate) channels: HashSet<crate::channel::ChannelId>,
    /// Presence keys this connection owns (per presence channel), so a
    /// disconnect can clear them. Filled in Task 10.
    pub(crate) tracked: HashSet<(crate::channel::ChannelId, String)>,
}

/// The connection hub: registry + delivery. One per app.
pub struct Hub {
    pub(crate) config: RealtimeConfig,
    pub(crate) node_id: u64,
    pub(crate) bus: bus::AnyBus,
    pub(crate) db: Option<jerrycan_db::Db>,
    pub(crate) conns: Mutex<HashMap<u64, Subscriber>>,
    pub(crate) presence: Mutex<presence::PresenceMap>,
    /// Set when the Changes source is unavailable — detection failed (sqlite /
    /// no db), the trigger DDL was denied (#212), OR the replication source
    /// never attached on first connect (#228). A join on a `changes:` channel
    /// then answers JC0530. `Arc` so the spawned replication supervisor shares
    /// the exact flag `Hub::join` reads (it flips it on a first-connect failure).
    pub(crate) changes_unavailable: Arc<std::sync::atomic::AtomicBool>,
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
            Subscriber {
                principal,
                tx,
                channels: Default::default(),
                tracked: Default::default(),
            },
        );
        (id, rx)
    }

    pub(crate) async fn disconnect(self: &Arc<Self>, conn: u64) {
        // Publish presence leaves for everything this conn tracked, then drop it.
        self.presence_disconnect(conn).await;
    }

    /// Send to one connection; a full/closed queue drops the connection.
    pub(crate) fn send_to(&self, conn: u64, msg: crate::protocol::ServerMsg) {
        let mut conns = self.conns.lock().expect("hub mutex");
        if let Some(sub) = conns.get(&conn)
            && sub.tx.try_send(msg).is_err()
        {
            conns.remove(&conn); // slow consumer: rx closes, loop ends
        }
    }

    /// One client frame: parse, dispatch, reply via the conn's own queue.
    pub(crate) async fn handle_client(self: &Arc<Self>, conn: u64, text: &str) {
        use crate::protocol::{ClientMsg, ServerMsg};
        let msg: ClientMsg = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                return self.send_to(
                    conn,
                    ServerMsg::Error {
                        code: "JC0422".into(),
                        message: format!("unparseable frame: {e}"),
                        channel: None,
                        r#ref: None,
                    },
                );
            }
        };
        match msg {
            ClientMsg::Heartbeat { r#ref } => self.send_to(conn, ServerMsg::HeartbeatAck { r#ref }),
            ClientMsg::Join { channel, r#ref } => self.join(conn, &channel, r#ref),
            ClientMsg::Leave { channel, r#ref } => self.leave(conn, &channel, r#ref),
            ClientMsg::Publish {
                channel,
                payload,
                r#ref,
            } => self.publish(conn, &channel, payload, r#ref).await,
            ClientMsg::Track {
                channel,
                state,
                r#ref,
            } => self.track(conn, &channel, state, r#ref).await,
            ClientMsg::Untrack { channel, r#ref } => self.untrack(conn, &channel, r#ref).await,
        }
    }

    fn join(self: &Arc<Self>, conn: u64, channel: &str, r#ref: Option<u64>) {
        use crate::protocol::ServerMsg;
        let Some(id) = crate::channel::ChannelId::parse(channel) else {
            return self.send_to(
                conn,
                ServerMsg::Error {
                    code: "JC0404".into(),
                    message: "unknown channel namespace".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            );
        };
        // Changes on a deployment without a working source answer JC0530.
        if matches!(id, crate::channel::ChannelId::Changes(_))
            && self
                .changes_unavailable
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return self.send_to(
                conn,
                ServerMsg::Error {
                    code: "JC0530".into(),
                    message: "realtime changes require Postgres".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            );
        }
        let allowed = {
            let conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.get(&conn) else { return };
            crate::channel::may_join(&id, &self.config, sub.principal.as_ref())
        };
        match allowed {
            Err("unknown channel") => self.send_to(
                conn,
                ServerMsg::Error {
                    code: "JC0404".into(),
                    message: "unknown channel".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            ),
            Err(reason) => self.send_to(
                conn,
                ServerMsg::Error {
                    code: if reason.contains("authentication") {
                        "JC0401"
                    } else {
                        "JC0403"
                    }
                    .into(),
                    message: reason.into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            ),
            Ok(()) => {
                if let Some(sub) = self.conns.lock().expect("hub mutex").get_mut(&conn) {
                    sub.channels.insert(id.clone());
                }
                self.send_to(
                    conn,
                    ServerMsg::Joined {
                        channel: channel.to_string(),
                        r#ref,
                    },
                );
                // Presence initial state (Task 10).
                self.on_join_presence(conn, &id);
            }
        }
    }

    fn leave(self: &Arc<Self>, conn: u64, channel: &str, r#ref: Option<u64>) {
        use crate::protocol::ServerMsg;
        if let Some(id) = crate::channel::ChannelId::parse(channel)
            && let Some(sub) = self.conns.lock().expect("hub mutex").get_mut(&conn)
        {
            sub.channels.remove(&id);
        }
        self.send_to(
            conn,
            ServerMsg::Left {
                channel: channel.to_string(),
                r#ref,
            },
        );
    }

    /// Bus fan-in: everything published on the bus is delivered here on every
    /// node. Each arm's real delivery lands in its own task.
    pub(crate) fn deliver(&self, msg: bus::BusMessage) {
        match msg {
            bus::BusMessage::Broadcast {
                topic,
                tenant_id,
                payload,
                origin,
            } => self.deliver_broadcast(&topic, tenant_id.as_deref(), &payload, origin),
            bus::BusMessage::Change(ev) => self.deliver_change(&ev), // Task 17
            bus::BusMessage::PresenceSet {
                topic,
                tenant_id,
                key,
                node,
                meta,
            } => self.deliver_presence_set(&topic, tenant_id, &key, node, meta),
            bus::BusMessage::PresenceClear {
                topic,
                tenant_id,
                key,
                node,
            } => self.deliver_presence_clear(&topic, tenant_id, &key, node),
            bus::BusMessage::PresenceSnapshot { node, entries } => {
                self.deliver_presence_snapshot(node, entries)
            }
            bus::BusMessage::Resync { entity } => self.deliver_resync(entity),
            // #232: a peer node (the replication leader) marked/lifted the change
            // source. Store its health locally so THIS node's `changes:` join
            // (`Hub::join`) answers JC0530 when the source is down and re-admits
            // on recovery — closing the multi-node follower dead-feed. On the
            // in-process LocalBus this is the marking node's own echo re-storing
            // the value it already set (idempotent).
            bus::BusMessage::ChangesHealth { unavailable } => self
                .changes_unavailable
                .store(unavailable, Ordering::Relaxed),
        }
    }

    /// Replication-gap resync: tell every changes subscriber to refetch.
    pub(crate) fn deliver_resync(&self, entity: Option<String>) {
        use crate::protocol::ServerMsg;
        let conns = self.conns.lock().expect("hub mutex");
        for sub in conns.values() {
            for id in &sub.channels {
                if let crate::channel::ChannelId::Changes(e) = id
                    && entity.as_deref().is_none_or(|want| want == e)
                {
                    let _ = sub.tx.try_send(ServerMsg::Resync {
                        channel: id.as_string(),
                    });
                }
            }
        }
    }
}

/// The app-provided dependency: the ws extractor resolves this.
#[derive(Clone)]
pub struct RealtimeHandle {
    pub(crate) hub: Arc<Hub>,
    pub(crate) resolver: Option<PrincipalResolver>,
}

impl jerrycan_core::Extension for Realtime {
    fn register(self, app: jerrycan_core::App) -> jerrycan_core::App {
        let bus = build_bus(&self);
        let hub = Arc::new(Hub {
            config: self.config.clone(),
            node_id: rand_node_id(),
            bus,
            db: self.db.clone(),
            conns: Mutex::new(HashMap::new()),
            presence: Mutex::new(presence::PresenceMap::default()),
            changes_unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            next_conn: AtomicU64::new(1),
        });
        let handle = RealtimeHandle {
            hub: hub.clone(),
            resolver: self.resolver,
        };
        let mount = self.mount.clone();
        app.provide(handle)
            .route(&mount, jerrycan_core::get(ws::ws_handler))
            .on_serve("realtime", move |ctx, shutdown| {
                supervisor(hub, ctx, shutdown)
            })
    }
}

/// Construct the fan-out bus: Redis when `realtime-redis` is on and a URL is
/// configured (explicit or `JERRYCAN_REDIS_URL`), else the in-process LocalBus.
fn build_bus(rt: &Realtime) -> bus::AnyBus {
    #[cfg(feature = "realtime-redis")]
    {
        if let Some(url) = rt
            .redis_url
            .clone()
            .or_else(|| std::env::var("JERRYCAN_REDIS_URL").ok())
        {
            return bus::AnyBus::Redis(bus_redis::RedisBus::new(url));
        }
    }
    let _ = rt;
    bus::AnyBus::Local(bus::LocalBus::new())
}

/// The serve-time supervisor: source selection (Task 17) + the bus pump.
async fn supervisor(
    hub: Arc<Hub>,
    ctx: jerrycan_core::TaskContext,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let _ = ctx;
    // Multi-node bus pump: forward Redis pub/sub into the local fan-in.
    #[cfg(feature = "realtime-redis")]
    if let bus::AnyBus::Redis(_) = &hub.bus {
        let hub_pump = hub.clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            if let bus::AnyBus::Redis(b) = &hub_pump.bus {
                b.run_pump(sd).await;
            }
        });
    }
    start_change_source(&hub, shutdown.clone()).await;
    presence_supervise(&hub, &mut shutdown).await;
}

/// Bus pump + presence sweep. Extended in Task 10 with the presence tick.
async fn presence_supervise(hub: &Arc<Hub>, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    let mut rx = hub.bus.subscribe();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tick.tick() => hub.presence_tick().await,
            msg = rx.recv() => match msg {
                Ok(m) => hub.deliver(m),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("jerrycan-realtime: bus lagged, dropped {n} messages");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

/// A random node id (presence/broadcast origin tagging across nodes). Uses the
/// std hasher over the current time — no rand dep.
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

// ---------------------------------------------------------------------------
// Stubs replaced by later tasks: Broadcast (Task 9), Presence (Task 10),
// Changes source + delivery (Task 17). They keep the crate compiling and the
// bus/handle_client wiring honest while those tasks land.
// ---------------------------------------------------------------------------

impl Hub {
    /// Scope-filtered change delivery (the security pillar, at the socket). For
    /// each `changes:{entity}` subscriber: the new-row view when `change_visible`
    /// (Supabase RLS parity — you only receive what you could GET), or a
    /// delete-shaped view when the row moved out of the subscriber's tenant.
    pub(crate) fn deliver_change(&self, ev: &crate::changes::ChangeEvent) {
        use crate::changes::ChangeOp;
        use crate::channel::{
            ChangeEventView, ChannelId, change_visible, delete_view_for_old_tenant,
        };
        use crate::protocol::ServerMsg;
        let Some(spec) = self.config.changes.iter().find(|s| s.entity == ev.entity) else {
            return;
        };
        let id = ChannelId::Changes(ev.entity.clone());
        let channel = id.as_string();
        let view = ChangeEventView {
            tenant_id: ev.tenant_id.clone(),
            old_tenant_id: ev.old_tenant_id.clone(),
            owner_id: ev.owner_id.clone(),
        };
        let op_str = match ev.op {
            ChangeOp::Insert => "insert",
            ChangeOp::Update => "update",
            ChangeOp::Delete => "delete",
        };
        // Strip the entity's hidden (write_only/password_hash) columns from the
        // row ONCE, before the per-subscriber fan-out (#167): the changes
        // broadcast ships the RAW DB row, so a hidden column would otherwise
        // reach every subscriber. Empty `hidden_columns` ⇒ the row is cloned
        // unchanged ⇒ byte-identical broadcast.
        let projected_row = ev
            .row
            .as_ref()
            .map(|r| project_row(r, &spec.hidden_columns));
        let mut drop_list = Vec::new();
        {
            let conns = self.conns.lock().expect("hub mutex");
            for (cid, sub) in conns.iter() {
                if !sub.channels.contains(&id) {
                    continue;
                }
                let p = sub.principal.as_ref();
                let payload = if change_visible(spec, &view, p) {
                    Some(serde_json::json!({ "type": op_str, "pk": ev.pk, "row": projected_row }))
                } else if delete_view_for_old_tenant(spec, &view, p) {
                    Some(serde_json::json!({ "type": "delete", "pk": ev.pk }))
                } else {
                    None
                };
                if let Some(payload) = payload
                    && sub
                        .tx
                        .try_send(ServerMsg::Event {
                            channel: channel.clone(),
                            payload,
                        })
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

/// Remove the `hidden` keys from `row` when it is a JSON object; any other JSON
/// shape is returned unchanged. An empty `hidden` set clones `row` verbatim, so
/// an entity with no write_only column broadcasts a byte-identical row (#167).
fn project_row(row: &serde_json::Value, hidden: &[String]) -> serde_json::Value {
    match row {
        serde_json::Value::Object(map) if !hidden.is_empty() => {
            let mut map = map.clone();
            for key in hidden {
                map.remove(key);
            }
            serde_json::Value::Object(map)
        }
        other => other.clone(),
    }
}

/// Broadcast the change-source health to every node (#232). The node that marks
/// or lifts `changes_unavailable` publishes this so followers — which only
/// deliver from the bus and never stream from Postgres — set their own flag and
/// answer `changes:` joins with JC0530 (or re-admit). On the in-process
/// LocalBus it loops back and idempotently re-stores the value the local node
/// already set, so single-node behavior is unchanged.
async fn publish_changes_health(bus: &bus::AnyBus, unavailable: bool) {
    let _ = bus
        .publish(bus::BusMessage::ChangesHealth { unavailable })
        .await;
}

/// Detect-replication-else-triggers at startup, then spawn the chosen adapter.
/// Replication path: leader → bus (every node delivers uniformly). Trigger
/// path: hub-local delivery (Postgres is the bus — no realtime bus, so
/// realtime-redis never double-delivers changes).
async fn start_change_source(hub: &Arc<Hub>, shutdown: tokio::sync::watch::Receiver<bool>) {
    if hub.config.changes.is_empty() {
        return;
    }
    let Some(db) = hub.db.clone() else {
        eprintln!("jerrycan-realtime: JC0530 changes configured without a database");
        hub.changes_unavailable.store(true, Ordering::Relaxed);
        publish_changes_health(&hub.bus, true).await;
        return;
    };
    match changes::detect(&db).await {
        Err(e) => {
            eprintln!("jerrycan-realtime: {e:?} — changes channels answer JC0530");
            hub.changes_unavailable.store(true, Ordering::Relaxed);
            publish_changes_health(&hub.bus, true).await;
        }
        Ok(changes::SourceKind::Replication) => {
            eprintln!("jerrycan-realtime: changes source = logical replication (pgoutput)");
            let specs = hub.config.changes.clone();
            let (etx, mut erx) = tokio::sync::mpsc::channel::<changes::ChangeEvent>(1024);
            let (rtx, mut rrx) = tokio::sync::mpsc::channel::<()>(16);
            // #232: the supervisor sends the change-source health here on every
            // mark/lift transition (and on each backoff iteration while it has
            // never connected, for late-joiner convergence); the forwarding task
            // republishes it across the bus so followers learn.
            let (htx, mut hrx) = tokio::sync::mpsc::channel::<bool>(16);
            tokio::spawn(changes::replication::run_supervised(
                db,
                specs,
                etx,
                rtx,
                htx,
                // #228: the supervisor flips this on a first-connect failure so a
                // permanently mis-provisioned replication PG fails loud (JC0530)
                // instead of admitting `changes:` joins to a silent dead feed.
                hub.changes_unavailable.clone(),
                shutdown.clone(),
            ));
            let hub2 = hub.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        ev = erx.recv() => match ev {
                            Some(ev) => { let _ = hub2.bus.publish(bus::BusMessage::Change(ev)).await; }
                            None => break,
                        },
                        Some(()) = rrx.recv() => {
                            let _ = hub2.bus.publish(bus::BusMessage::Resync { entity: None }).await;
                        }
                        Some(unavailable) = hrx.recv() => {
                            publish_changes_health(&hub2.bus, unavailable).await;
                        }
                    }
                }
            });
        }
        Ok(changes::SourceKind::Triggers) => {
            eprintln!("jerrycan-realtime: changes source = triggers + LISTEN/NOTIFY");
            let specs = hub.config.changes.clone();
            if let Err(e) = changes::triggers::ensure_triggers(&db, &specs).await {
                // #212 (fail loud): a privilege-restricted Postgres can deny the
                // trigger/function DDL. Spawning the LISTEN adapter anyway would
                // admit `changes:` subscribers to a feed that can never NOTIFY —
                // a silent dead feed. Mirror the sibling failure branches: mark
                // changes unavailable so a join answers JC0530, and do NOT spawn.
                eprintln!(
                    "jerrycan-realtime: trigger DDL reconcile failed: {e} — changes channels answer JC0530"
                );
                hub.changes_unavailable.store(true, Ordering::Relaxed);
                publish_changes_health(&hub.bus, true).await;
                return;
            }
            let (etx, mut erx) = tokio::sync::mpsc::channel::<changes::ChangeEvent>(1024);
            let url = db.url().to_string();
            let adapter = changes::triggers::TriggerAdapter {
                db: db.clone(),
                url,
                specs,
            };
            tokio::spawn(adapter.run(etx, shutdown.clone()));
            let hub2 = hub.clone();
            tokio::spawn(async move {
                while let Some(ev) = erx.recv().await {
                    hub2.deliver_change(&ev);
                }
            });
        }
    }
}

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
                owner_column: None,
                hidden_columns: Vec::new(),
            })
            .broadcast("room", TopicScope::Tenant)
            .presence("editors", TopicScope::Tenant)
            .mount("/rt");
        assert_eq!(rt.config.changes.len(), 1);
        assert_eq!(rt.config.changes[0].entity, "Lead");
        assert_eq!(
            rt.config.broadcast,
            vec![("room".to_string(), TopicScope::Tenant)]
        );
        assert_eq!(
            rt.config.presence,
            vec![("editors".to_string(), TopicScope::Tenant)]
        );
        assert_eq!(rt.mount, "/rt");
    }

    /// The default mount is /realtime (one endpoint multiplexes all channels).
    #[test]
    fn default_mount_is_realtime() {
        assert_eq!(Realtime::builder().mount, "/realtime");
    }
}

/// Socket-seam scope-filter tests: the sqlite loopback (`tests/ws_live.rs`)
/// cannot exercise Changes (they require Postgres), so the security pillar at
/// the actual delivery seam — `deliver_change` routing to real per-connection
/// queues — is proven here in-process. A regression in the wiring that CALLS
/// the pure filter (wrong principal, dropped filter, mis-routed delete) turns
/// these red even though `channel.rs`'s pure-function controls stay green.
#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::changes::{ChangeEvent, ChangeOp};
    use crate::channel::ChannelId;
    use crate::protocol::ServerMsg;

    fn hub_with_lead() -> Arc<Hub> {
        hub_with_lead_hiding(Vec::new())
    }

    /// A single-`Lead`-channel hub whose broadcast row strips `hidden` columns —
    /// `hub_with_lead()` passes an empty set (full-row broadcast), the #167
    /// projection test passes `["secret"]`.
    fn hub_with_lead_hiding(hidden: Vec<String>) -> Arc<Hub> {
        let config = RealtimeConfig {
            changes: vec![ChangeChannelSpec {
                entity: "Lead".into(),
                table: "lead".into(),
                pk_column: "id".into(),
                tenant_column: Some("workspace_id".into()),
                owner_column: None,
                hidden_columns: hidden,
            }],
            ..Default::default()
        };
        Arc::new(Hub {
            config,
            node_id: 1,
            bus: bus::AnyBus::Local(bus::LocalBus::new()),
            db: None,
            conns: Mutex::new(HashMap::new()),
            presence: Mutex::new(presence::PresenceMap::default()),
            changes_unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            next_conn: AtomicU64::new(1),
        })
    }

    fn tenant(id: &str) -> Option<Principal> {
        Some(Principal {
            user_id: "u".into(),
            tenant_id: Some(id.into()),
            role: None,
        })
    }

    fn join_changes(hub: &Arc<Hub>, conn: u64) {
        hub.conns
            .lock()
            .unwrap()
            .get_mut(&conn)
            .unwrap()
            .channels
            .insert(ChannelId::Changes("Lead".into()));
    }

    /// #228 (Rule 9) fail-loud CONTRACT: when the Changes source is unavailable
    /// (`changes_unavailable` set — by sqlite/no-db detection, the #212 trigger-
    /// DDL denial, or the #228 replication first-connect failure), a `changes:`
    /// join is REFUSED with JC0530 rather than admitting the socket to a feed
    /// that never delivers. This test turns RED if the refusal in `Hub::join` is
    /// removed — the payoff of every branch that sets the flag. No live PG needed.
    #[test]
    fn changes_join_is_refused_jc0530_when_source_unavailable() {
        let hub = hub_with_lead();
        hub.changes_unavailable
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (c1, mut rx1) = hub.connect(tenant("t1"));
        hub.join(c1, "changes:Lead", Some(7));
        match rx1.try_recv() {
            Ok(ServerMsg::Error { code, r#ref, .. }) => {
                assert_eq!(
                    code, "JC0530",
                    "an unavailable changes source must answer JC0530"
                );
                assert_eq!(r#ref, Some(7), "the error echoes the join ref");
            }
            other => panic!("a changes join on an unavailable source must be refused: {other:?}"),
        }
        // The refusal must NOT have joined the channel (no silent membership).
        assert!(
            !hub.conns
                .lock()
                .unwrap()
                .get(&c1)
                .unwrap()
                .channels
                .contains(&ChannelId::Changes("Lead".into())),
            "a refused join must not add channel membership"
        );
    }

    /// #232 (the multi-node fix at the delivery seam): a `ChangesHealth` bus
    /// message applied via `Hub::deliver` toggles THIS node's `changes:` join
    /// admission — exactly how a follower learns the replication leader marked
    /// (or lifted) the source it never streams itself. `{unavailable:true}` must
    /// make a subsequent join answer JC0530; `{false}` must re-admit it. This is
    /// the follower-side payoff of every publish site: it turns RED if the
    /// `deliver` arm ignores the variant (leaving the flag untouched), which is
    /// the dead-feed regression #232 closes.
    #[test]
    fn deliver_changes_health_toggles_changes_join_admission() {
        let hub = hub_with_lead(); // source starts available

        // Leader reports the source DOWN → this node refuses the join (JC0530).
        hub.deliver(bus::BusMessage::ChangesHealth { unavailable: true });
        let (c1, mut rx1) = hub.connect(tenant("t1"));
        hub.join(c1, "changes:Lead", Some(1));
        match rx1.try_recv() {
            Ok(ServerMsg::Error { code, .. }) => assert_eq!(
                code, "JC0530",
                "a follower must refuse joins once the leader reports the source down"
            ),
            other => panic!("expected JC0530 after ChangesHealth{{true}}: {other:?}"),
        }
        assert!(
            !hub.conns
                .lock()
                .unwrap()
                .get(&c1)
                .unwrap()
                .channels
                .contains(&ChannelId::Changes("Lead".into())),
            "a refused join must not add channel membership"
        );

        // Leader reports RECOVERY → this node re-admits the join.
        hub.deliver(bus::BusMessage::ChangesHealth { unavailable: false });
        let (c2, mut rx2) = hub.connect(tenant("t2"));
        hub.join(c2, "changes:Lead", Some(2));
        match rx2.try_recv() {
            Ok(ServerMsg::Joined { channel, .. }) => assert_eq!(channel, "changes:Lead"),
            other => panic!("expected Joined after ChangesHealth{{false}}: {other:?}"),
        }
    }

    /// #232: the forwarding glue — `publish_changes_health` must place a
    /// `ChangesHealth` on the bus verbatim, so under Redis the health reaches
    /// every peer node's pump. A regression that drops or reshapes the publish
    /// turns this red.
    #[tokio::test]
    async fn publish_changes_health_puts_the_variant_on_the_bus() {
        let bus = bus::AnyBus::Local(bus::LocalBus::new());
        let mut rx = bus.subscribe();
        publish_changes_health(&bus, true).await;
        assert!(
            matches!(
                rx.recv().await.unwrap(),
                bus::BusMessage::ChangesHealth { unavailable: true }
            ),
            "publish_changes_health must emit ChangesHealth{{true}} on the bus"
        );
    }

    /// THE socket-level negative control: an insert in tenant t2 reaches a t2
    /// subscriber and NEVER a t1 subscriber joined to the same channel.
    #[test]
    fn deliver_change_delivers_only_to_the_owning_tenant() {
        let hub = hub_with_lead();
        let (c1, mut rx1) = hub.connect(tenant("t1"));
        let (c2, mut rx2) = hub.connect(tenant("t2"));
        join_changes(&hub, c1);
        join_changes(&hub, c2);

        hub.deliver_change(&ChangeEvent {
            entity: "Lead".into(),
            op: ChangeOp::Insert,
            pk: "42".into(),
            row: Some(serde_json::json!({"id":"42","workspace_id":"t2","secret":"x"})),
            tenant_id: Some("t2".into()),
            old_tenant_id: None,
            owner_id: None,
        });

        match rx2.try_recv() {
            Ok(ServerMsg::Event { channel, payload }) => {
                assert_eq!(channel, "changes:Lead");
                assert_eq!(payload["type"], "insert");
                assert_eq!(payload["row"]["secret"], "x");
            }
            other => panic!("t2 must receive its own tenant's insert: {other:?}"),
        }
        assert!(
            rx1.try_recv().is_err(),
            "t1 (other tenant) must receive NOTHING — cross-tenant leak at the seam"
        );
    }

    /// A tenant move t1→t2 at the seam: the old tenant gets a delete-shaped view
    /// with NO row body; the new tenant gets the update WITH the body.
    #[test]
    fn deliver_change_tenant_move_splits_delete_to_old_update_to_new() {
        let hub = hub_with_lead();
        let (c1, mut rx1) = hub.connect(tenant("t1"));
        let (c2, mut rx2) = hub.connect(tenant("t2"));
        join_changes(&hub, c1);
        join_changes(&hub, c2);

        hub.deliver_change(&ChangeEvent {
            entity: "Lead".into(),
            op: ChangeOp::Update,
            pk: "42".into(),
            row: Some(serde_json::json!({"id":"42","workspace_id":"t2","secret":"s"})),
            tenant_id: Some("t2".into()),
            old_tenant_id: Some("t1".into()),
            owner_id: None,
        });

        match rx1.try_recv() {
            Ok(ServerMsg::Event { payload, .. }) => {
                assert_eq!(payload["type"], "delete");
                assert_eq!(payload["pk"], "42");
                assert!(
                    payload.get("row").is_none(),
                    "old tenant must NOT receive the row body: {payload}"
                );
            }
            other => panic!("old tenant gets a delete-shaped view: {other:?}"),
        }
        match rx2.try_recv() {
            Ok(ServerMsg::Event { payload, .. }) => {
                assert_eq!(payload["type"], "update");
                assert_eq!(payload["row"]["secret"], "s");
            }
            other => panic!("new tenant gets the update with the body: {other:?}"),
        }
    }

    /// #167 (SECURITY): a hidden (write_only/password_hash) column is projected
    /// OUT of the broadcast row before delivery — the subscriber receives every
    /// OTHER column but NEVER the hidden one, on BOTH insert and update. This is
    /// the realtime twin of the REST `skip_serializing` hide: the changes
    /// channel ships the raw DB row, so without this projection the column
    /// (present in `ev.row` from the WAL decode / trigger SELECT) would leak to
    /// every subscriber. A regression that drops the projection turns this red.
    #[test]
    fn deliver_change_projects_hidden_columns_out_of_the_broadcast() {
        let hub = hub_with_lead_hiding(vec!["secret".into()]);
        let (c2, mut rx2) = hub.connect(tenant("t2"));
        join_changes(&hub, c2);

        let full_row = serde_json::json!({
            "id": "42", "workspace_id": "t2", "email": "a@b.c", "secret": "shhh"
        });

        for op in [ChangeOp::Insert, ChangeOp::Update] {
            hub.deliver_change(&ChangeEvent {
                entity: "Lead".into(),
                op,
                pk: "42".into(),
                row: Some(full_row.clone()),
                tenant_id: Some("t2".into()),
                old_tenant_id: None,
                owner_id: None,
            });
            match rx2.try_recv() {
                Ok(ServerMsg::Event { channel, payload }) => {
                    assert_eq!(channel, "changes:Lead");
                    assert!(
                        payload["row"].get("secret").is_none(),
                        "the write_only `secret` must NEVER be broadcast: {payload}"
                    );
                    // Every other column still rides along unchanged.
                    assert_eq!(payload["row"]["id"], "42", "{payload}");
                    assert_eq!(payload["row"]["workspace_id"], "t2", "{payload}");
                    assert_eq!(payload["row"]["email"], "a@b.c", "{payload}");
                }
                other => panic!("subscriber must receive the projected row: {other:?}"),
            }
        }
    }

    /// A per-user (identity-owned, non-tenant) changes hub: `tenant_column: None,
    /// owner_column: Some("user_id")`. The #216 shape — its events must reach
    /// ONLY the row's owner.
    fn hub_with_note() -> Arc<Hub> {
        let config = RealtimeConfig {
            changes: vec![ChangeChannelSpec {
                entity: "Note".into(),
                table: "notes".into(),
                pk_column: "id".into(),
                tenant_column: None,
                owner_column: Some("user_id".into()),
                hidden_columns: Vec::new(),
            }],
            ..Default::default()
        };
        Arc::new(Hub {
            config,
            node_id: 1,
            bus: bus::AnyBus::Local(bus::LocalBus::new()),
            db: None,
            conns: Mutex::new(HashMap::new()),
            presence: Mutex::new(presence::PresenceMap::default()),
            changes_unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            next_conn: AtomicU64::new(1),
        })
    }

    fn user(id: &str) -> Option<Principal> {
        Some(Principal {
            user_id: id.into(),
            tenant_id: None,
            role: None,
        })
    }

    fn join_note(hub: &Arc<Hub>, conn: u64) {
        hub.conns
            .lock()
            .unwrap()
            .get_mut(&conn)
            .unwrap()
            .channels
            .insert(ChannelId::Changes("Note".into()));
    }

    /// #216 (SECURITY) — THE per-user negative control at the delivery seam: a
    /// Note owned by u1 reaches u1's socket and NEVER u2's, on insert AND on
    /// delete (delete carries the owner from the OLD row). A regression that
    /// drops the owner filter — the pre-0.7.3 `(None, _) => true` world-visible
    /// arm — turns this red: it is the exact cross-user leak #216 closes.
    #[test]
    fn deliver_change_per_user_delivers_only_to_the_owner() {
        let hub = hub_with_note();
        let (c1, mut rx1) = hub.connect(user("u1"));
        let (c2, mut rx2) = hub.connect(user("u2"));
        join_note(&hub, c1);
        join_note(&hub, c2);

        // Insert of u1's Note: the owner receives it, the other user nothing.
        hub.deliver_change(&ChangeEvent {
            entity: "Note".into(),
            op: ChangeOp::Insert,
            pk: "7".into(),
            row: Some(serde_json::json!({"id":"7","user_id":"u1","body":"hi"})),
            tenant_id: None,
            old_tenant_id: None,
            owner_id: Some("u1".into()),
        });
        match rx1.try_recv() {
            Ok(ServerMsg::Event { channel, payload }) => {
                assert_eq!(channel, "changes:Note");
                assert_eq!(payload["type"], "insert");
                assert_eq!(payload["row"]["user_id"], "u1");
            }
            other => panic!("u1 must receive its own Note insert: {other:?}"),
        }
        assert!(
            rx2.try_recv().is_err(),
            "u2 must receive NOTHING — cross-user leak at the seam (#216)"
        );

        // Delete of u1's Note: owner_id comes from the OLD row (no new row),
        // and the owner still receives the delete-shaped view; u2 nothing.
        hub.deliver_change(&ChangeEvent {
            entity: "Note".into(),
            op: ChangeOp::Delete,
            pk: "7".into(),
            row: None,
            tenant_id: None,
            old_tenant_id: None,
            owner_id: Some("u1".into()),
        });
        match rx1.try_recv() {
            Ok(ServerMsg::Event { payload, .. }) => {
                assert_eq!(payload["type"], "delete");
                assert_eq!(payload["pk"], "7");
            }
            other => panic!("u1 must receive its own Note delete: {other:?}"),
        }
        assert!(
            rx2.try_recv().is_err(),
            "u2 must never see u1's delete either (#216)"
        );
    }
}
