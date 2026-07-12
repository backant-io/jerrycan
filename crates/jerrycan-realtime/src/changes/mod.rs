//! Postgres Changes: the shared event model (adapters land in later tasks).
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
