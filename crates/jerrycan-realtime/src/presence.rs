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
        self.entries.entry(part.clone()).or_default().insert(
            key.to_string(),
            Entry {
                node,
                meta: meta.clone(),
            },
        );
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
    pub(crate) fn sweep(
        &mut self,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Vec<(Partition, serde_json::Value)> {
        let node_seen = &self.node_seen;
        let dead: std::collections::HashSet<u64> = node_seen
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
                let node_dead = dead.contains(&e.node) || !node_seen.contains_key(&e.node);
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

/// Epoch milliseconds — the presence clock (node liveness + LWW).
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

use crate::channel::ChannelId;
use crate::protocol::ServerMsg;
use std::sync::Arc;

impl crate::Hub {
    /// The presence key for a connection: the user id, or `conn:{id}` for an
    /// anonymous connection on a scope-none topic.
    fn presence_key(principal: Option<&crate::Principal>, conn: u64) -> String {
        principal
            .map(|p| p.user_id.clone())
            .unwrap_or_else(|| format!("conn:{conn}"))
    }

    /// The partition tenant for a presence topic: the principal's tenant when
    /// the topic is tenant-scoped, else None (one shared partition).
    fn presence_tenant(&self, topic: &str, principal: Option<&crate::Principal>) -> Option<String> {
        match self
            .config
            .presence
            .iter()
            .find(|(n, _)| n == topic)
            .map(|(_, s)| *s)
        {
            Some(crate::TopicScope::Tenant) => principal.and_then(|p| p.tenant_id.clone()),
            _ => None,
        }
    }

    /// Track op: record this connection's presence state on a joined topic and
    /// announce it through the bus (the echoed diff is the ack).
    pub(crate) async fn track(
        self: &Arc<Self>,
        conn: u64,
        channel: &str,
        state: serde_json::Value,
        r#ref: Option<u64>,
    ) {
        let Some(id @ ChannelId::Presence(_)) = ChannelId::parse(channel) else {
            return self.send_to(
                conn,
                ServerMsg::Error {
                    code: "JC0404".into(),
                    message: "track targets a presence channel".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                },
            );
        };
        let ChannelId::Presence(topic) = id.clone() else {
            unreachable!()
        };
        let (key, tenant_id) = {
            let mut conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.get_mut(&conn) else {
                return;
            };
            if !sub.channels.contains(&id) {
                let _ = sub.tx.try_send(ServerMsg::Error {
                    code: "JC0403".into(),
                    message: "join the presence channel before tracking".into(),
                    channel: Some(channel.to_string()),
                    r#ref,
                });
                return;
            }
            let key = Self::presence_key(sub.principal.as_ref(), conn);
            let tenant_id = self.presence_tenant(&topic, sub.principal.as_ref());
            sub.tracked.insert((id.clone(), key.clone()));
            (key, tenant_id)
        };
        let _ = self
            .bus
            .publish(crate::bus::BusMessage::PresenceSet {
                topic,
                tenant_id,
                key,
                node: self.node_id,
                meta: state,
            })
            .await;
    }

    /// Untrack op: clear this connection's presence state on a topic.
    pub(crate) async fn untrack(self: &Arc<Self>, conn: u64, channel: &str, _ref: Option<u64>) {
        let Some(id @ ChannelId::Presence(_)) = ChannelId::parse(channel) else {
            return;
        };
        let ChannelId::Presence(topic) = id.clone() else {
            unreachable!()
        };
        let cleared = {
            let mut conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.get_mut(&conn) else {
                return;
            };
            let key = Self::presence_key(sub.principal.as_ref(), conn);
            let tenant_id = self.presence_tenant(&topic, sub.principal.as_ref());
            if sub.tracked.remove(&(id.clone(), key.clone())) {
                Some((key, tenant_id))
            } else {
                None
            }
        };
        if let Some((key, tenant_id)) = cleared {
            let _ = self
                .bus
                .publish(crate::bus::BusMessage::PresenceClear {
                    topic,
                    tenant_id,
                    key,
                    node: self.node_id,
                })
                .await;
        }
    }

    /// On join of a presence channel: send the current partition state to the
    /// joiner (initial sync).
    pub(crate) fn on_join_presence(self: &Arc<Self>, conn: u64, id: &ChannelId) {
        let ChannelId::Presence(topic) = id else {
            return;
        };
        let state = {
            let conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.get(&conn) else { return };
            let tenant_id = self.presence_tenant(topic, sub.principal.as_ref());
            let part = Partition {
                topic: topic.clone(),
                tenant_id,
            };
            self.presence.lock().expect("presence mutex").state(&part)
        };
        self.send_to(
            conn,
            ServerMsg::PresenceState {
                channel: id.as_string(),
                state,
            },
        );
    }

    /// On disconnect: publish a presence leave for every key this conn owned.
    pub(crate) async fn presence_disconnect(self: &Arc<Self>, conn: u64) {
        let owned: Vec<(String, Option<String>, String)> = {
            let mut conns = self.conns.lock().expect("hub mutex");
            let Some(sub) = conns.remove(&conn) else {
                return;
            };
            sub.tracked
                .iter()
                .filter_map(|(id, key)| {
                    if let ChannelId::Presence(topic) = id {
                        let tenant_id = self.presence_tenant(topic, sub.principal.as_ref());
                        Some((topic.clone(), tenant_id, key.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (topic, tenant_id, key) in owned {
            let _ = self
                .bus
                .publish(crate::bus::BusMessage::PresenceClear {
                    topic,
                    tenant_id,
                    key,
                    node: self.node_id,
                })
                .await;
        }
    }

    /// Deliver a presence set from the bus: merge, then broadcast the join diff.
    pub(crate) fn deliver_presence_set(
        &self,
        topic: &str,
        tenant_id: Option<String>,
        key: &str,
        node: u64,
        meta: serde_json::Value,
    ) {
        let part = Partition {
            topic: topic.to_string(),
            tenant_id: tenant_id.clone(),
        };
        let joins =
            self.presence
                .lock()
                .expect("presence mutex")
                .set(&part, key, node, meta, now_ms());
        if let Some(joins) = joins {
            self.broadcast_presence_diff(&part, joins, serde_json::json!({}));
        }
    }

    /// Deliver a presence clear from the bus: remove, then broadcast the leave.
    pub(crate) fn deliver_presence_clear(
        &self,
        topic: &str,
        tenant_id: Option<String>,
        key: &str,
        node: u64,
    ) {
        let part = Partition {
            topic: topic.to_string(),
            tenant_id: tenant_id.clone(),
        };
        let leaves = self
            .presence
            .lock()
            .expect("presence mutex")
            .clear(&part, key, node);
        if let Some(leaves) = leaves {
            self.broadcast_presence_diff(&part, serde_json::json!({}), leaves);
        }
    }

    /// Deliver a peer node's periodic snapshot: refresh its liveness.
    pub(crate) fn deliver_presence_snapshot(
        &self,
        node: u64,
        _entries: Vec<(String, Option<String>, String)>,
    ) {
        self.presence
            .lock()
            .expect("presence mutex")
            .touch_node(node, now_ms());
    }

    /// Send a presence diff to every local subscriber of the partition.
    fn broadcast_presence_diff(
        &self,
        part: &Partition,
        joins: serde_json::Value,
        leaves: serde_json::Value,
    ) {
        let id = ChannelId::Presence(part.topic.clone());
        let channel = id.as_string();
        let mut drop_list = Vec::new();
        {
            let conns = self.conns.lock().expect("hub mutex");
            for (cid, sub) in conns.iter() {
                if !sub.channels.contains(&id) {
                    continue;
                }
                // Partition match: the subscriber's tenant slice must equal the
                // diff's partition (None == None for scope-none/auth topics).
                if self.presence_tenant(&part.topic, sub.principal.as_ref()) != part.tenant_id {
                    continue;
                }
                if sub
                    .tx
                    .try_send(ServerMsg::PresenceDiff {
                        channel: channel.clone(),
                        joins: joins.clone(),
                        leaves: leaves.clone(),
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

    /// One presence supervisor tick: publish this node's snapshot, refresh its
    /// own liveness, sweep dead nodes, and broadcast any resulting leave diffs.
    pub(crate) async fn presence_tick(&self) {
        const NODE_TTL_MS: u64 = 90_000;
        let entries: Vec<(String, Option<String>, String)> = {
            let map = self.presence.lock().expect("presence mutex");
            map.local_entries(self.node_id)
        };
        let _ = self
            .bus
            .publish(crate::bus::BusMessage::PresenceSnapshot {
                node: self.node_id,
                entries,
            })
            .await;
        let leaves = {
            let mut map = self.presence.lock().expect("presence mutex");
            map.touch_node(self.node_id, now_ms());
            map.sweep(now_ms(), NODE_TTL_MS)
        };
        for (part, diff) in leaves {
            self.broadcast_presence_diff(&part, serde_json::json!({}), diff);
        }
    }
}

impl PresenceMap {
    /// The (topic, tenant, key) triples owned by `node` — the snapshot payload.
    pub(crate) fn local_entries(&self, node: u64) -> Vec<(String, Option<String>, String)> {
        let mut out = Vec::new();
        for (part, bucket) in &self.entries {
            for (key, e) in bucket {
                if e.node == node {
                    out.push((part.topic.clone(), part.tenant_id.clone(), key.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(topic: &str) -> Partition {
        Partition {
            topic: topic.into(),
            tenant_id: None,
        }
    }

    #[test]
    fn set_then_state_then_clear_produces_join_and_leave_diffs() {
        let mut map = PresenceMap::default();
        let joins = map.set(
            &part("editors"),
            "u1",
            1,
            serde_json::json!({"c": 3}),
            1_000,
        );
        assert_eq!(joins, Some(serde_json::json!({"u1": {"c": 3}})));
        // Same key, newer meta: last-writer-wins, still reported as a join diff.
        let joins = map.set(
            &part("editors"),
            "u1",
            1,
            serde_json::json!({"c": 4}),
            2_000,
        );
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
        let t1 = Partition {
            topic: "editors".into(),
            tenant_id: Some("t1".into()),
        };
        let t2 = Partition {
            topic: "editors".into(),
            tenant_id: Some("t2".into()),
        };
        map.set(&t1, "u1", 1, serde_json::json!({}), 0);
        assert_eq!(
            map.state(&t2),
            serde_json::json!({}),
            "cross-tenant presence must be empty"
        );
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
