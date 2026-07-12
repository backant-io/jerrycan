//! Structured machine-readable gap work-items (spec §Gap report). The agent's
//! judgment queue: everything the translator will not guess lands here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    RlsPolicy,
    PgFunction,
    PgTrigger,
    EdgeFunction,
    UnmappedType,
    ForeignKey,
    RealtimeChannel,
    Broadcast,
    Presence,
    CronJob,
    SuspectedSecret,
    /// Export data that could not be carried into the generated seed
    /// mechanically (membership rows, storage object rows, dropped columns).
    SeedData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Blocking,
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapItem {
    pub kind: GapKind,
    pub source: String,
    pub location: String,
    pub reason: String,
    pub original: String,
    pub suggested: String,
    pub severity: Severity,
}

/// Sort (blocking first, then location, then source) and render pretty JSON —
/// stable bytes for identical inputs (eval-gated determinism).
pub fn render_gap_report(items: &mut [GapItem]) -> String {
    items.sort_by(|a, b| {
        (a.severity, &a.location, &a.source).cmp(&(b.severity, &b.location, &b.source))
    });
    let mut out = serde_json::to_string_pretty(&items).expect("gap items serialize");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_items_serialize_to_the_spec_shape_and_sort_deterministically() {
        let mut items = vec![
            GapItem {
                kind: GapKind::PgFunction,
                source: "public.audit()".into(),
                location: "schema.sql:90".into(),
                reason: "plpgsql bodies are ported by the agent".into(),
                original: "CREATE FUNCTION audit() …".into(),
                suggested: "port to a Rust handler or job task".into(),
                severity: Severity::Advisory,
            },
            GapItem {
                kind: GapKind::RlsPolicy,
                source: "public.orders policy \"tenant_isolation\"".into(),
                location: "schema.sql:14".into(),
                reason: "predicate references a join we don't auto-translate".into(),
                original: "USING (EXISTS (SELECT 1 FROM order_shares …))".into(),
                suggested: "implement as a guard on the orders module".into(),
                severity: Severity::Blocking,
            },
        ];
        let json = render_gap_report(&mut items);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Blocking sorts before advisory; within severity, by location.
        assert_eq!(v[0]["kind"], "rls_policy");
        assert_eq!(v[0]["severity"], "blocking");
        assert_eq!(v[1]["kind"], "pg_function");
        // Every spec field present, snake_case kinds.
        for key in [
            "kind",
            "source",
            "location",
            "reason",
            "original",
            "suggested",
            "severity",
        ] {
            assert!(v[0].get(key).is_some(), "missing {key}");
        }
        // Determinism: rendering twice is byte-identical.
        assert_eq!(json, render_gap_report(&mut items));
    }
}
