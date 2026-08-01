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
    /// The row's identity-fk (owner) value — populated only for a per-user
    /// entity (`spec.owner_column` Some). Extracted new-or-old, so a delete
    /// carries the owner from the OLD row (#216).
    pub(crate) owner_id: Option<String>,
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
        (Some(_), Some(t)) => p.tenant_id.as_deref() == Some(t.as_str()),
        // A tenant-scoped entity with no extractable tenant key: fail CLOSED.
        (Some(_), None) => false,
        // Not tenant-scoped: a per-user (identity-owned) entity delivers ONLY to
        // the row's owner (#216); a genuinely auth-only entity to any principal.
        (None, _) => match (&spec.owner_column, &ev.owner_id) {
            (None, _) => true, // auth-only entity (no tenant, no owner)
            (Some(_), Some(o)) => p.user_id.as_str() == o.as_str(), // per-user: owner only
            (Some(_), None) => false, // owner-scoped, no extractable owner key: fail CLOSED
        },
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
        (Some(old), new) if Some(old) != new.as_ref() => {
            p.tenant_id.as_deref() == Some(old.as_str())
        }
        _ => false,
    }
}

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
                owner_column: None,
                hidden_columns: Vec::new(),
            }],
            broadcast: vec![
                ("room".into(), TopicScope::Tenant),
                ("lobby".into(), TopicScope::None),
            ],
            presence: vec![("editors".into(), TopicScope::Auth)],
        }
    }

    fn principal(tenant: &str) -> Principal {
        Principal {
            user_id: "u1".into(),
            tenant_id: Some(tenant.into()),
            role: None,
        }
    }

    #[test]
    fn channel_ids_parse_and_reject_unknown() {
        assert!(
            matches!(ChannelId::parse("changes:Lead"), Some(ChannelId::Changes(e)) if e == "Lead")
        );
        assert!(matches!(
            ChannelId::parse("broadcast:room"),
            Some(ChannelId::Broadcast(_))
        ));
        assert!(matches!(
            ChannelId::parse("presence:editors"),
            Some(ChannelId::Presence(_))
        ));
        assert!(ChannelId::parse("nope").is_none());
        assert!(ChannelId::parse("changes:").is_none());
    }

    #[test]
    fn join_requires_auth_for_changes_and_scoped_topics() {
        let c = cfg();
        // changes: never joinable anonymously (mandatory scope filter).
        assert!(may_join(&ChannelId::parse("changes:Lead").unwrap(), &c, None).is_err());
        assert!(
            may_join(
                &ChannelId::parse("changes:Lead").unwrap(),
                &c,
                Some(&principal("t1"))
            )
            .is_ok()
        );
        // tenant-scoped broadcast: needs a principal WITH a tenant.
        assert!(may_join(&ChannelId::parse("broadcast:room").unwrap(), &c, None).is_err());
        let no_tenant = Principal {
            user_id: "u".into(),
            tenant_id: None,
            role: None,
        };
        assert!(
            may_join(
                &ChannelId::parse("broadcast:room").unwrap(),
                &c,
                Some(&no_tenant)
            )
            .is_err()
        );
        // scope none: anonymous ok.
        assert!(may_join(&ChannelId::parse("broadcast:lobby").unwrap(), &c, None).is_ok());
        // auth-scoped presence: any principal.
        assert!(
            may_join(
                &ChannelId::parse("presence:editors").unwrap(),
                &c,
                Some(&no_tenant)
            )
            .is_ok()
        );
        // unknown channel names are rejected (not silently created).
        assert!(
            may_join(
                &ChannelId::Broadcast("ghost".into()),
                &c,
                Some(&principal("t1"))
            )
            .is_err()
        );
        assert!(
            may_join(
                &ChannelId::Changes("Ghost".into()),
                &c,
                Some(&principal("t1"))
            )
            .is_err()
        );
    }

    /// THE negative control (spec: security pillar). A change in tenant t2
    /// must never be visible to a t1 subscriber; breaking this filter must
    /// turn this test red.
    #[test]
    fn cross_tenant_change_is_never_visible() {
        let c = cfg();
        let spec = &c.changes[0];
        let ev = ChangeEventView {
            tenant_id: Some("t2".into()),
            old_tenant_id: None,
            owner_id: None,
        };
        assert!(!change_visible(spec, &ev, Some(&principal("t1"))));
        assert!(change_visible(spec, &ev, Some(&principal("t2"))));
        // Anonymous NEVER sees a change, scoped or not.
        assert!(!change_visible(spec, &ev, None));
        let unscoped = ChangeChannelSpec {
            tenant_column: None,
            ..spec.clone()
        };
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
        let ev = ChangeEventView {
            tenant_id: Some("t2".into()),
            old_tenant_id: Some("t1".into()),
            owner_id: None,
        };
        assert!(
            change_visible(spec, &ev, Some(&principal("t2"))),
            "new tenant sees it"
        );
        assert!(
            delete_view_for_old_tenant(spec, &ev, Some(&principal("t1"))),
            "old tenant gets the delete view"
        );
        assert!(
            !change_visible(spec, &ev, Some(&principal("t1"))),
            "old tenant must NOT get the row body"
        );
        assert!(
            !delete_view_for_old_tenant(spec, &ev, Some(&principal("t3"))),
            "third tenant sees nothing"
        );
    }

    fn per_user_spec() -> ChangeChannelSpec {
        ChangeChannelSpec {
            entity: "Note".into(),
            table: "notes".into(),
            pk_column: "id".into(),
            tenant_column: None,
            owner_column: Some("user_id".into()),
            hidden_columns: Vec::new(),
        }
    }

    fn owner(user_id: &str) -> Principal {
        Principal {
            user_id: user_id.into(),
            tenant_id: None,
            role: None,
        }
    }

    /// #216 (SECURITY): a per-user (identity-owned, non-tenant) changes event is
    /// visible ONLY to the row's owner. This is the pure-filter twin of the REST
    /// `get_for`/`all_for` owner-scoping: `ev.owner_id == principal.user_id`. The
    /// pre-0.7.3 filter mapped `(None, _) => true`, so EVERY authenticated
    /// principal saw EVERY user's rows — the cross-user leak. A regression to
    /// that world-visible arm turns this red.
    #[test]
    fn per_user_change_is_visible_only_to_its_owner() {
        let spec = per_user_spec();
        let ev = ChangeEventView {
            tenant_id: None,
            old_tenant_id: None,
            owner_id: Some("u1".into()),
        };
        assert!(
            change_visible(&spec, &ev, Some(&owner("u1"))),
            "the owner sees their own row"
        );
        assert!(
            !change_visible(&spec, &ev, Some(&owner("u2"))),
            "another user must NEVER see u1's row — the #216 cross-user leak"
        );
        // Anonymous never sees a per-user change.
        assert!(!change_visible(&spec, &ev, None));
        // Owner-scoped but no extractable owner key: fail CLOSED (not world-visible).
        let no_key = ChangeEventView {
            owner_id: None,
            ..ev.clone()
        };
        assert!(
            !change_visible(&spec, &no_key, Some(&owner("u1"))),
            "owner_column set but owner_id missing must fail closed, never deliver"
        );
    }

    /// A genuinely auth-only entity (no tenant, no owner column) is unchanged by
    /// #216: any authenticated principal receives it (the `(None, None)` arm).
    #[test]
    fn auth_only_change_is_visible_to_any_principal() {
        let spec = ChangeChannelSpec {
            owner_column: None,
            ..per_user_spec()
        };
        let ev = ChangeEventView {
            tenant_id: None,
            old_tenant_id: None,
            owner_id: None,
        };
        assert!(change_visible(&spec, &ev, Some(&owner("u1"))));
        assert!(change_visible(&spec, &ev, Some(&owner("u2"))));
        // Still never anonymous.
        assert!(!change_visible(&spec, &ev, None));
    }
}
