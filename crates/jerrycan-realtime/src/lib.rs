//! Realtime extension for jerrycan: Postgres Changes + Broadcast + Presence
//! over one WebSocket endpoint, with mandatory scope-filtered delivery.
//! <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod changes;
pub mod protocol;
pub(crate) mod bus;
pub(crate) mod channel;
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
        }
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
