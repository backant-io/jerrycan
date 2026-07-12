//! Realtime extension for jerrycan: Postgres Changes + Broadcast + Presence
//! over one WebSocket endpoint, with mandatory scope-filtered delivery.
//! <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod changes;
pub mod protocol;
pub(crate) mod broadcast;
pub(crate) mod bus;
#[cfg(feature = "realtime-redis")]
pub(crate) mod bus_redis;
pub(crate) mod channel;
pub(crate) mod presence;
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
    /// Set when Changes detection failed (sqlite / no db): a join on a
    /// `changes:` channel then answers JC0530. Wired in Task 17.
    pub(crate) changes_unavailable: std::sync::atomic::AtomicBool,
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
            ClientMsg::Heartbeat { r#ref } => {
                self.send_to(conn, ServerMsg::HeartbeatAck { r#ref })
            }
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
        if let Some(id) = crate::channel::ChannelId::parse(channel) {
            if let Some(sub) = self.conns.lock().expect("hub mutex").get_mut(&conn) {
                sub.channels.remove(&id);
            }
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
        }
    }

    /// Replication-gap resync: tell every changes subscriber to refetch.
    pub(crate) fn deliver_resync(&self, entity: Option<String>) {
        use crate::protocol::ServerMsg;
        let conns = self.conns.lock().expect("hub mutex");
        for sub in conns.values() {
            for id in &sub.channels {
                if let crate::channel::ChannelId::Changes(e) = id {
                    if entity.as_deref().is_none_or(|want| want == e) {
                        let _ = sub.tx.try_send(ServerMsg::Resync {
                            channel: id.as_string(),
                        });
                    }
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
            changes_unavailable: std::sync::atomic::AtomicBool::new(false),
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
        use crate::channel::{ChangeEventView, ChannelId, change_visible, delete_view_for_old_tenant};
        use crate::changes::ChangeOp;
        use crate::protocol::ServerMsg;
        let Some(spec) = self.config.changes.iter().find(|s| s.entity == ev.entity) else {
            return;
        };
        let id = ChannelId::Changes(ev.entity.clone());
        let channel = id.as_string();
        let view = ChangeEventView {
            tenant_id: ev.tenant_id.clone(),
            old_tenant_id: ev.old_tenant_id.clone(),
        };
        let op_str = match ev.op {
            ChangeOp::Insert => "insert",
            ChangeOp::Update => "update",
            ChangeOp::Delete => "delete",
        };
        let mut drop_list = Vec::new();
        {
            let conns = self.conns.lock().expect("hub mutex");
            for (cid, sub) in conns.iter() {
                if !sub.channels.contains(&id) {
                    continue;
                }
                let p = sub.principal.as_ref();
                let payload = if change_visible(spec, &view, p) {
                    Some(serde_json::json!({ "type": op_str, "pk": ev.pk, "row": ev.row }))
                } else if delete_view_for_old_tenant(spec, &view, p) {
                    Some(serde_json::json!({ "type": "delete", "pk": ev.pk }))
                } else {
                    None
                };
                if let Some(payload) = payload {
                    if sub
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
        }
        for cid in drop_list {
            self.conns.lock().expect("hub mutex").remove(&cid);
        }
    }
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
        return;
    };
    match changes::detect(&db).await {
        Err(e) => {
            eprintln!("jerrycan-realtime: {e:?} — changes channels answer JC0530");
            hub.changes_unavailable.store(true, Ordering::Relaxed);
        }
        Ok(changes::SourceKind::Replication) => {
            eprintln!("jerrycan-realtime: changes source = logical replication (pgoutput)");
            let specs = hub.config.changes.clone();
            let (etx, mut erx) = tokio::sync::mpsc::channel::<changes::ChangeEvent>(1024);
            let (rtx, mut rrx) = tokio::sync::mpsc::channel::<()>(16);
            tokio::spawn(changes::replication::run_supervised(
                db,
                specs,
                etx,
                rtx,
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
                    }
                }
            });
        }
        Ok(changes::SourceKind::Triggers) => {
            eprintln!("jerrycan-realtime: changes source = triggers + LISTEN/NOTIFY");
            let specs = hub.config.changes.clone();
            if let Err(e) = changes::triggers::ensure_triggers(&db, &specs).await {
                eprintln!("jerrycan-realtime: trigger DDL reconcile failed: {e}");
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
