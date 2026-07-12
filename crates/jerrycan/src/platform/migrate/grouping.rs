//! Spec §Deterministic translator (2): cluster tables into modules by FK graph
//! + shared name prefix. Hub tables (tenant, users) anchor their own modules —
//! edges INTO a hub are ignored so tenancy doesn't collapse the app into one
//! module. The agent refines grouping afterwards (spec §Agent-judgment layer).

use std::collections::{BTreeMap, BTreeSet};

/// `edges` are (child, parent) fk pairs among "schema.table" keys.
/// Returns (module_name, member tables sorted), sorted by module name.
pub fn group_modules(
    tables: &[String],
    edges: &[(String, String)],
    hubs: &BTreeSet<String>,
) -> Vec<(String, Vec<String>)> {
    // Union-find over non-hub edges.
    let mut parent: BTreeMap<&str, &str> =
        tables.iter().map(|t| (t.as_str(), t.as_str())).collect();
    fn find<'a>(parent: &BTreeMap<&'a str, &'a str>, mut x: &'a str) -> &'a str {
        while parent[x] != x {
            x = parent[x];
        }
        x
    }
    for (child, par) in edges {
        if hubs.contains(child) || hubs.contains(par) {
            continue;
        }
        let (rc, rp) = (find(&parent, child.as_str()), find(&parent, par.as_str()));
        if rc != rp {
            let (lo, hi) = if rc < rp { (rc, rp) } else { (rp, rc) };
            parent.insert(hi, lo); // deterministic: smaller name wins as root
        }
    }
    // Merge components whose ROOT tables share a `_`-prefix of ≥ 3 chars.
    let mut components: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in tables {
        components
            .entry(find(&parent, t).to_string())
            .or_default()
            .push(t.clone());
    }
    let mut by_prefix: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (root, mut members) in components {
        members.sort();
        let bare = root.rsplit('.').next().unwrap_or(&root);
        let prefix = bare.split('_').next().unwrap_or(bare);
        let key = if prefix.len() >= 3 {
            prefix.to_string()
        } else {
            bare.to_string()
        };
        by_prefix.entry(key).or_default().extend(members);
    }
    let mut out: Vec<(String, Vec<String>)> = by_prefix
        .into_iter()
        .map(|(name, mut members)| {
            members.sort();
            members.dedup();
            // Module names are kebab-case (questions.rs); tables are snake.
            (name.replace('_', "-"), members)
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fk_components_group_and_hub_edges_do_not_merge_everything() {
        // customers→workspaces(hub), notes→customers, billing_invoices→workspaces(hub),
        // billing_receipts→billing_invoices, plans (isolated).
        let edges = vec![
            (
                "public.customers".to_string(),
                "public.workspaces".to_string(),
            ),
            ("public.notes".to_string(), "public.customers".to_string()),
            (
                "public.billing_invoices".to_string(),
                "public.workspaces".to_string(),
            ),
            (
                "public.billing_receipts".to_string(),
                "public.billing_invoices".to_string(),
            ),
        ];
        let tables = [
            "public.billing_invoices",
            "public.billing_receipts",
            "public.customers",
            "public.notes",
            "public.plans",
            "public.workspaces",
        ]
        .map(String::from)
        .to_vec();
        let hubs = ["public.workspaces".to_string()].into_iter().collect();
        let modules = group_modules(&tables, &edges, &hubs);
        // Deterministic: sorted by module name; hub gets its own module.
        assert_eq!(
            modules,
            vec![
                (
                    "billing".to_string(),
                    vec![
                        "public.billing_invoices".into(),
                        "public.billing_receipts".into()
                    ]
                ),
                (
                    "customers".to_string(),
                    vec!["public.customers".into(), "public.notes".into()]
                ),
                ("plans".to_string(), vec!["public.plans".into()]),
                ("workspaces".to_string(), vec!["public.workspaces".into()]),
            ]
        );
    }
}
