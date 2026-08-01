//! Broadcast: ephemeral client-to-client pub/sub. Publishing requires having
//! joined the topic; tenant-scoped topics are partitioned per tenant; the
//! publisher's own connection is excluded from delivery (Supabase parity:
//! self-broadcast off).

use crate::bus::BusMessage;
use crate::channel::ChannelId;
use crate::protocol::ServerMsg;
use jerrycan_core::http::StatusCode;
use jerrycan_core::{Error, Result};
use std::sync::Arc;

impl crate::RealtimeHandle {
    /// Publish an event to a declared broadcast topic from server code — the
    /// canonical "a REST handler created a row, now push it to every subscriber"
    /// path (issue #50). Resolve this handle in a handler (`Dep<RealtimeHandle>`)
    /// and call `publish` after the write; no client connection or WS round-trip
    /// is involved.
    ///
    /// It enforces the SAME gate as the WS client publish path: `topic` MUST name
    /// a declared `broadcast:` topic, so an unknown name or a `changes`/`presence`
    /// channel is a clear `Err` (JC0404), never a silent drop. A server publish
    /// carries no connection principal, so it is un-partitioned and reaches EVERY
    /// current subscriber of the topic — publishing to a `tenant`-scoped topic is
    /// therefore a JC0403 `Err` (delivering to all tenants would break the
    /// per-tenant isolation the scope promises). Declare the topic scope `none`
    /// or `auth` to publish it from a handler, or use
    /// [`publish_to`](Self::publish_to) to reach a single tenant's sockets on a
    /// `tenant`-scoped topic.
    pub async fn publish(&self, topic: &str, payload: serde_json::Value) -> Result<()> {
        self.hub.publish_from_server(topic, payload).await
    }

    /// Tenant-partitioned server publish (issue #104): the partitioned twin of
    /// [`publish`](Self::publish). It goes through the same server-publish path but
    /// stamps the event with `tenant_id`, so on a `tenant`-scoped topic delivery
    /// reaches ONLY that tenant's sockets (a socket receives it exactly when
    /// `principal.tenant_id == tenant_id`). This is the tenant-partitioned
    /// broadcast plain `publish` cannot express — it is un-partitioned and so
    /// refused (JC0403) on a `tenant`-scoped topic. Resolve `Dep<RealtimeHandle>`
    /// in a handler and call it after a tenant-scoped write.
    ///
    /// `topic` MUST name a declared **`tenant`-scoped** broadcast topic: an unknown
    /// name (or a `changes`/`presence` channel) is JC0404, and a `none`/`auth`
    /// topic is JC0403 — those are un-partitioned, so the `tenant_id` argument
    /// would be silently ignored; use `publish` for them. The two methods are thus
    /// a clean duality: `publish` for `none`/`auth`, `publish_to` for `tenant`.
    /// Because delivery admits a socket only when its verified `principal.tenant_id`
    /// equals `tenant_id`, `publish_to(A, …)` can never reach another tenant.
    pub async fn publish_to(
        &self,
        tenant_id: &str,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        self.hub.publish_to_tenant(tenant_id, topic, payload).await
    }
}

impl crate::Hub {
    /// The declared scope of a broadcast `topic`, or JC0404 when no such broadcast
    /// topic exists (an unknown name, or a `changes`/`presence` channel). Shared by
    /// both server-publish paths so they agree on what a valid target is.
    fn server_publish_scope(&self, topic: &str) -> Result<crate::TopicScope> {
        self.config
            .broadcast
            .iter()
            .find(|(n, _)| n == topic)
            .map(|(_, s)| *s)
            .ok_or_else(|| {
                Error::new(
                    StatusCode::NOT_FOUND,
                    "JC0404",
                    format!(
                        "no broadcast topic `{topic}` is declared — server publish targets a declared broadcast topic"
                    ),
                )
            })
    }

    /// Server-side broadcast publish (no connection, no principal). Validates the
    /// topic against the same invariants the WS client `publish` gate enforces,
    /// then puts it on the bus with `origin: None` (nothing to exclude) and
    /// `tenant_id: None` (un-partitioned) so `deliver_broadcast` fans it out to
    /// every subscriber on every node.
    pub(crate) async fn publish_from_server(
        &self,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let scope = self.server_publish_scope(topic)?;
        if scope == crate::TopicScope::Tenant {
            return Err(Error::new(
                StatusCode::FORBIDDEN,
                "JC0403",
                format!(
                    "broadcast topic `{topic}` is tenant-scoped; a plain `publish` is un-partitioned and would leak across tenants, so it is refused. Use `publish_to(tenant_id, \"{topic}\", payload)` to reach a single tenant's sockets — and do NOT downgrade the topic's scope to work around this: `none`/`auth` topics are visible to every tenant"
                ),
            ));
        }
        self.bus
            .publish(BusMessage::Broadcast {
                topic: topic.to_string(),
                tenant_id: None,
                payload,
                origin: None,
            })
            .await
    }

    /// Tenant-partitioned server publish (issue #104). Validates the topic, then
    /// puts it on the bus with `tenant_id: Some(tenant)` so `deliver_broadcast`
    /// admits ONLY sockets whose verified `principal.tenant_id` equals `tenant` —
    /// the same partition key the WS client `publish` derives from its own
    /// principal. A `none`/`auth` topic is refused (JC0403): those are
    /// un-partitioned, so the tenant argument would be silently ignored — plain
    /// `publish` is the supported path there. Unknown topic ⇒ JC0404.
    pub(crate) async fn publish_to_tenant(
        &self,
        tenant_id: &str,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let scope = self.server_publish_scope(topic)?;
        if scope != crate::TopicScope::Tenant {
            return Err(Error::new(
                StatusCode::FORBIDDEN,
                "JC0403",
                format!(
                    "broadcast topic `{topic}` is not tenant-scoped; `publish_to` partitions per tenant, so on an un-partitioned `none`/`auth` topic the tenant argument would be silently ignored. Use `publish(\"{topic}\", payload)` for it"
                ),
            ));
        }
        self.bus
            .publish(BusMessage::Broadcast {
                topic: topic.to_string(),
                tenant_id: Some(tenant_id.to_string()),
                payload,
                origin: None,
            })
            .await
    }
}

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
        let Some(id @ ChannelId::Broadcast(_)) = ChannelId::parse(channel) else {
            return self.send_to(
                conn,
                ServerMsg::Error {
                    code: "JC0404".into(),
                    message: "publish targets a broadcast channel".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            );
        };
        let ChannelId::Broadcast(topic) = id.clone() else {
            unreachable!("matched Broadcast above")
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
            return self.send_to(
                conn,
                ServerMsg::Error {
                    code: "JC0403".into(),
                    message: "join the channel before publishing".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            );
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
            .publish(BusMessage::Broadcast {
                topic,
                tenant_id,
                payload,
                origin: Some((node, conn)),
            })
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
                if let Some(t) = tenant_id
                    && sub.principal.as_ref().and_then(|p| p.tenant_id.as_deref()) != Some(t)
                {
                    continue;
                }
                if sub
                    .tx
                    .try_send(ServerMsg::Event {
                        channel: channel.clone(),
                        payload: payload.clone(),
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

#[cfg(test)]
mod server_publish_tests {
    //! The server-side broadcast publish API (issue #50): a REST handler resolves
    //! `RealtimeHandle` and calls `publish(topic, payload)` to push an event to
    //! every subscriber of a broadcast topic — the "created a row, notify the UI"
    //! path, expressible without a handler dialing its own WS endpoint. The
    //! server API enforces the SAME gate as the WS client publish path: the topic
    //! MUST be a declared broadcast topic (a `changes`/`presence`/unknown name is
    //! an Err), and a `tenant`-scoped topic is an Err (an un-partitioned server
    //! publish must never cross the per-tenant boundary the scope promises).
    use crate::bus::{AnyBus, LocalBus};
    use crate::channel::ChannelId;
    use crate::presence::PresenceMap;
    use crate::protocol::ServerMsg;
    use crate::{ChangeChannelSpec, Hub, Principal, RealtimeConfig, RealtimeHandle, TopicScope};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

    /// A hub declaring `events` (auth-scoped, server-publishable), `room`
    /// (tenant-scoped) and a `Lead` changes entity — the three topic kinds a
    /// server publish must tell apart.
    fn hub() -> Arc<Hub> {
        let config = RealtimeConfig {
            changes: vec![ChangeChannelSpec {
                entity: "Lead".into(),
                table: "leads".into(),
                pk_column: "id".into(),
                tenant_column: Some("workspace_id".into()),
                owner_column: None,
                hidden_columns: Vec::new(),
            }],
            broadcast: vec![
                ("events".into(), TopicScope::Auth),
                ("room".into(), TopicScope::Tenant),
            ],
            presence: vec![],
        };
        Arc::new(Hub {
            config,
            node_id: 1,
            bus: AnyBus::Local(LocalBus::new()),
            db: None,
            conns: Mutex::new(HashMap::new()),
            presence: Mutex::new(PresenceMap::default()),
            changes_unavailable: Arc::new(AtomicBool::new(false)),
            next_conn: AtomicU64::new(1),
        })
    }

    fn handle(hub: &Arc<Hub>) -> RealtimeHandle {
        RealtimeHandle {
            hub: hub.clone(),
            resolver: None,
        }
    }

    fn auth_user() -> Option<Principal> {
        Some(Principal {
            user_id: "u1".into(),
            tenant_id: None,
            role: None,
        })
    }

    fn tenant_user(tenant: &str) -> Option<Principal> {
        Some(Principal {
            user_id: format!("u-{tenant}"),
            tenant_id: Some(tenant.into()),
            role: None,
        })
    }

    fn join(hub: &Arc<Hub>, conn: u64, topic: &str) {
        hub.conns
            .lock()
            .unwrap()
            .get_mut(&conn)
            .unwrap()
            .channels
            .insert(ChannelId::Broadcast(topic.into()));
    }

    /// THE positive control: a handler-side `publish` on a declared broadcast
    /// topic reaches a subscribed client. Delivery flows publish → bus →
    /// `deliver`, exactly as the serve-time supervisor pumps it, so this proves
    /// the whole seam, not just that the message was enqueued.
    #[tokio::test]
    async fn server_publish_reaches_a_subscriber() {
        let hub = hub();
        // Subscribe to the bus BEFORE publishing (the supervisor does the same);
        // a tokio broadcast only reaches receivers present at send time.
        let mut bus_rx = hub.bus.subscribe();
        let (conn, mut rx) = hub.connect(auth_user());
        join(&hub, conn, "events");

        handle(&hub)
            .publish("events", serde_json::json!({ "type": "created", "id": 7 }))
            .await
            .expect("publish to a declared auth topic succeeds");

        // Pump the one bus message into the local fan-in, as `presence_supervise`
        // does on every node.
        hub.deliver(bus_rx.recv().await.expect("bus carries the publish"));

        match rx.try_recv() {
            Ok(ServerMsg::Event { channel, payload }) => {
                assert_eq!(channel, "broadcast:events");
                assert_eq!(payload["type"], "created");
                assert_eq!(payload["id"], 7);
            }
            other => panic!("subscriber must receive the server-published event: {other:?}"),
        }
    }

    /// An unknown topic name is a clear Err (JC0404), never a silent drop.
    #[tokio::test]
    async fn server_publish_to_an_unknown_topic_errs() {
        let hub = hub();
        let err = handle(&hub)
            .publish("ghost", serde_json::json!({}))
            .await
            .expect_err("an undeclared topic must error");
        assert_eq!(err.code(), "JC0404");
    }

    /// A `changes` entity is not a broadcast topic — publishing to it errs
    /// (broadcast-only, mirroring the WS publish gate's channel-kind check).
    #[tokio::test]
    async fn server_publish_to_a_changes_entity_errs() {
        let hub = hub();
        let err = handle(&hub)
            .publish("Lead", serde_json::json!({}))
            .await
            .expect_err("a changes entity is not a publishable broadcast topic");
        assert_eq!(err.code(), "JC0404");
    }

    /// A `tenant`-scoped topic is an Err for plain `publish`: it carries no
    /// principal, so it cannot pick a tenant partition, and delivering to all
    /// tenants would break the isolation the scope promises. Fail loud, never leak.
    /// (The supported path is `publish_to`, exercised below.)
    #[tokio::test]
    async fn server_publish_to_a_tenant_scoped_topic_errs() {
        let hub = hub();
        let err = handle(&hub)
            .publish("room", serde_json::json!({}))
            .await
            .expect_err("a tenant-scoped topic is not un-partitioned-publishable");
        assert_eq!(err.code(), "JC0403");
        // And it must NOT have reached the bus (no partial/leaky publish).
        let mut bus_rx = hub.bus.subscribe();
        assert!(
            bus_rx.try_recv().is_err(),
            "a rejected publish must never touch the bus"
        );
    }

    /// #104 THE tenant-partitioned publish: `publish_to(A, "room", …)` on a
    /// `tenant`-scoped topic reaches tenant-A's socket and NEVER tenant-B's — the
    /// partition key is the event's `tenant_id`, matched against each socket's
    /// verified `principal.tenant_id` in `deliver_broadcast`. This is the whole
    /// point of the feature: a server can now broadcast to exactly one tenant's
    /// workspace. A regression that dropped the partition would deliver B's socket
    /// too and turn this red — the cross-tenant leak control.
    #[tokio::test]
    async fn publish_to_reaches_only_the_target_tenant() {
        let hub = hub();
        let mut bus_rx = hub.bus.subscribe();
        let (a, mut rx_a) = hub.connect(tenant_user("A"));
        let (b, mut rx_b) = hub.connect(tenant_user("B"));
        join(&hub, a, "room");
        join(&hub, b, "room");

        handle(&hub)
            .publish_to("A", "room", serde_json::json!({ "msg": "for-A" }))
            .await
            .expect("publish_to a declared tenant topic succeeds");

        // Pump the one bus message into the local fan-in (as the supervisor does).
        hub.deliver(bus_rx.recv().await.expect("bus carries the publish"));

        match rx_a.try_recv() {
            Ok(ServerMsg::Event { channel, payload }) => {
                assert_eq!(channel, "broadcast:room");
                assert_eq!(payload["msg"], "for-A");
            }
            other => panic!("tenant-A socket must receive its own tenant's publish: {other:?}"),
        }
        assert!(
            rx_b.try_recv().is_err(),
            "tenant-B socket must receive NOTHING — cross-tenant leak on publish_to"
        );
    }

    /// The `publish` ↔ `publish_to` duality: `publish_to` on a `none`/`auth`
    /// (un-partitioned) topic is a clear JC0403 pointing at `publish`, never a
    /// silent no-op where the tenant argument is ignored. `events` is auth-scoped.
    #[tokio::test]
    async fn publish_to_a_non_tenant_topic_errs() {
        let hub = hub();
        let err = handle(&hub)
            .publish_to("A", "events", serde_json::json!({}))
            .await
            .expect_err("publish_to targets a tenant-scoped topic only");
        assert_eq!(err.code(), "JC0403");
        // A rejected publish never reaches the bus.
        let mut bus_rx = hub.bus.subscribe();
        assert!(
            bus_rx.try_recv().is_err(),
            "a rejected publish_to must never touch the bus"
        );
    }

    /// `publish_to` shares the topic-existence gate: an unknown name is JC0404.
    #[tokio::test]
    async fn publish_to_an_unknown_topic_errs() {
        let hub = hub();
        let err = handle(&hub)
            .publish_to("A", "ghost", serde_json::json!({}))
            .await
            .expect_err("an undeclared topic must error");
        assert_eq!(err.code(), "JC0404");
    }
}
