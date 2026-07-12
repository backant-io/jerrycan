//! Postgres Changes: the shared event model (adapters land in later tasks).
use crate::ChangeChannelSpec;
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
    specs
        .iter()
        .map(|s| format!("\"{}\"", s.table))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn create_publication_sql(specs: &[ChangeChannelSpec]) -> String {
    format!(
        "CREATE PUBLICATION {PUBLICATION} FOR TABLE {}",
        table_list(specs)
    )
}

pub(crate) fn reconcile_publication_sql(specs: &[ChangeChannelSpec]) -> String {
    format!(
        "ALTER PUBLICATION {PUBLICATION} SET TABLE {}",
        table_list(specs)
    )
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

/// Detection queries (run over the sea-orm Db — the data layer stays sqlx).
pub(crate) const SHOW_WAL_LEVEL: &str = "SHOW wal_level";
pub(crate) const SHOW_MAX_SLOT_WAL_KEEP_SIZE: &str = "SHOW max_slot_wal_keep_size";
pub(crate) const CAN_REPLICATE: &str =
    "SELECT rolreplication OR rolsuper AS ok FROM pg_roles WHERE rolname = current_user";

/// One CDC source: runs until shutdown, emitting decoded events. Both adapters
/// implement this; the hub treats them identically (spec: the client sees
/// identical behavior — only the source differs).
pub(crate) trait ChangeSource: Send + 'static {
    fn run(
        self: Box<Self>,
        events: tokio::sync::mpsc::Sender<ChangeEvent>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
}

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
        assert!(
            f.contains("CREATE OR REPLACE FUNCTION jc_notify_change_lead()"),
            "{f}"
        );
        assert!(f.contains("pg_notify('jc_changes'"), "{f}");
        assert!(f.contains("TG_OP"), "{f}");
        assert!(f.contains("workspace_id"), "{f}");
        let t = trigger_sql(&lead());
        assert!(
            t.starts_with(r#"DROP TRIGGER IF EXISTS jc_changes_lead ON "lead""#),
            "{t}"
        );
        assert!(
            t.contains(r#"CREATE TRIGGER jc_changes_lead AFTER INSERT OR UPDATE OR DELETE ON "lead""#),
            "{t}"
        );
        assert!(
            t.contains("FOR EACH ROW EXECUTE FUNCTION jc_notify_change_lead()"),
            "{t}"
        );
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
        assert!(
            s.len() < 120,
            "payload must stay far under the NOTIFY cap: {s}"
        );
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
