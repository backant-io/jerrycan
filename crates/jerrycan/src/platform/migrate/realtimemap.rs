//! Spec §7: the `supabase_realtime` publication → `realtime.changes`. A
//! published table we didn't model gaps (never invented). Broadcast + Presence
//! live in client code, so each is a standing advisory.

use super::gaps::{GapItem, GapKind, Severity};
use std::collections::BTreeMap;

pub struct RealtimeOutput {
    /// Sorted entity names whose row changes are subscribable.
    pub changes: Vec<String>,
    pub gaps: Vec<GapItem>,
}

pub fn build_realtime(
    publications: &BTreeMap<String, Vec<String>>,
    table_to_entity: &BTreeMap<String, String>,
) -> RealtimeOutput {
    let mut changes = Vec::new();
    let mut gaps = Vec::new();
    if let Some(tables) = publications.get("supabase_realtime") {
        for table in tables {
            match table_to_entity.get(table) {
                Some(entity) => changes.push(entity.clone()),
                None => gaps.push(GapItem {
                    kind: GapKind::RealtimeChannel,
                    source: table.clone(),
                    location: "schema.sql".into(),
                    reason: "table is published to supabase_realtime but was not modeled as an entity".into(),
                    original: format!("alter publication supabase_realtime add table {table}"),
                    suggested: "add the table's entity to realtime.changes after modeling it, or drop the subscription".into(),
                    severity: Severity::Blocking,
                }),
            }
        }
    }
    changes.sort();
    changes.dedup();

    // Broadcast + Presence topics live in client code, not the database.
    gaps.push(GapItem {
        kind: GapKind::Broadcast,
        source: "realtime broadcast topics".into(),
        location: "(client code)".into(),
        reason: "Broadcast topics live in client code, not the database".into(),
        original: String::new(),
        suggested: "recreate used topics as realtime.broadcast[] entries from frontend usage".into(),
        severity: Severity::Advisory,
    });
    gaps.push(GapItem {
        kind: GapKind::Presence,
        source: "realtime presence topics".into(),
        location: "(client code)".into(),
        reason: "Presence topics live in client code, not the database".into(),
        original: String::new(),
        suggested: "recreate used topics as realtime.presence[] entries from frontend usage".into(),
        severity: Severity::Advisory,
    });

    RealtimeOutput { changes, gaps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn publication_tables_become_realtime_changes_plus_standing_advisories() {
        let mut pubs = BTreeMap::new();
        pubs.insert(
            "supabase_realtime".to_string(),
            vec!["public.customers".to_string(), "public.ghosts".to_string()],
        );
        let mapped: BTreeMap<String, String> =
            [("public.customers".to_string(), "Customer".to_string())]
                .into_iter()
                .collect();
        let out = build_realtime(&pubs, &mapped);
        assert_eq!(out.changes, vec!["Customer"]);
        // A published table we didn't map → realtime_channel gap (blocking).
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::RealtimeChannel
            && g.source.contains("public.ghosts")));
        // Broadcast/Presence live client-side (spec §7) → one advisory each, always.
        assert!(
            out.gaps
                .iter()
                .any(|g| g.kind == crate::platform::migrate::gaps::GapKind::Broadcast)
        );
        assert!(
            out.gaps
                .iter()
                .any(|g| g.kind == crate::platform::migrate::gaps::GapKind::Presence)
        );
    }
}
