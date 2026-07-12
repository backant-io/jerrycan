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
    Resync {
        entity: Option<String>,
    },
}

const BUS_CAPACITY: usize = 1024;

/// In-process bus: a tokio broadcast channel.
pub(crate) struct LocalBus {
    tx: tokio::sync::broadcast::Sender<BusMessage>,
}

impl LocalBus {
    pub(crate) fn new() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(BUS_CAPACITY).0,
        }
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
            let BusMessage::Broadcast {
                topic,
                tenant_id,
                origin,
                ..
            } = rx.recv().await.unwrap()
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
