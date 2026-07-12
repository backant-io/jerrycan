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
