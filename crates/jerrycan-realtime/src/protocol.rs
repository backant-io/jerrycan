//! The jerrycan-native realtime envelope (spec decision #9). One WS endpoint
//! multiplexes all channels; every frame is a JSON object tagged by `op`.
//! Channels are namespaced strings: `changes:{Entity}` / `broadcast:{name}` /
//! `presence:{name}`.

use serde::{Deserialize, Serialize};

/// Client → server frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMsg {
    Join {
        channel: String,
        #[serde(default)]
        r#ref: Option<u64>,
    },
    Leave {
        channel: String,
        #[serde(default)]
        r#ref: Option<u64>,
    },
    /// Broadcast publish (ephemeral, client-to-client).
    Publish {
        channel: String,
        payload: serde_json::Value,
        #[serde(default)]
        r#ref: Option<u64>,
    },
    /// Presence: set/replace this connection's state on the topic.
    Track {
        channel: String,
        state: serde_json::Value,
        #[serde(default)]
        r#ref: Option<u64>,
    },
    /// Presence: clear this connection's state on the topic.
    Untrack {
        channel: String,
        #[serde(default)]
        r#ref: Option<u64>,
    },
    Heartbeat {
        #[serde(default)]
        r#ref: Option<u64>,
    },
}

/// Server → client frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ServerMsg {
    Joined {
        channel: String,
        r#ref: Option<u64>,
    },
    Left {
        channel: String,
        r#ref: Option<u64>,
    },
    /// A delivery: changes payloads are `{"type","pk","row"?,"old_pk"?}`;
    /// broadcast payloads are the publisher's JSON verbatim.
    Event {
        channel: String,
        payload: serde_json::Value,
    },
    /// Full presence state, sent on join to a presence channel.
    PresenceState {
        channel: String,
        state: serde_json::Value,
    },
    /// Incremental joins/leaves after the initial state.
    PresenceDiff {
        channel: String,
        joins: serde_json::Value,
        leaves: serde_json::Value,
    },
    HeartbeatAck {
        r#ref: Option<u64>,
    },
    /// The replication slot was recreated after a gap — refetch and resubscribe.
    Resync {
        channel: String,
    },
    Error {
        code: String,
        message: String,
        channel: Option<String>,
        r#ref: Option<u64>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_join_parses() {
        let m: ClientMsg =
            serde_json::from_str(r#"{"op":"join","channel":"broadcast:room","ref":1}"#).unwrap();
        assert_eq!(
            m,
            ClientMsg::Join {
                channel: "broadcast:room".into(),
                r#ref: Some(1)
            }
        );
    }

    #[test]
    fn client_publish_carries_arbitrary_payload() {
        let m: ClientMsg = serde_json::from_str(
            r#"{"op":"publish","channel":"broadcast:room","payload":{"x":1}}"#,
        )
        .unwrap();
        let ClientMsg::Publish { payload, .. } = m else {
            panic!("wrong variant")
        };
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
