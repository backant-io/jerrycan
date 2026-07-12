//! Spec §3 tenancy detection + §4's owner translation, rule R3 (resolved
//! ambiguity #4): membership-join → tenancy; owner-only apps → tenant = User;
//! owner tables under org tenancy translate only when the tenant fk is present.

use super::pgmodel::{PgDatabase, PgPolicy, PolicyCommand};
use super::rls::{Recognized, Scope, recognize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
pub struct TenancyDetection {
    pub tenant_table: Option<String>,
    pub membership_table: Option<String>,
    /// From the membership role column's CHECK/enum values, declaration order.
    pub member_roles: Vec<String>,
}

/// Per-table access summary the CRUD/storage mappers consume.
#[derive(Debug)]
pub enum TableAccess {
    /// Membership-join scoping (tenant fk column carried on the table).
    Tenant {
        required_roles_by_command: BTreeMap<PolicyCommand, Vec<String>>,
    },
    /// R3(b): owner scoping expressed as tenancy over User.
    OwnerAsUserTenant { owner_column: String },
    /// Only `Authenticated` / role gates — guarded but not row-scoped.
    AuthOnly,
    /// SELECT is public; writes carry one of the scoped variants above.
    PublicRead { write: Box<TableAccess> },
    /// RLS enabled but at least one policy didn't recognize → agent work.
    Gap { reasons: Vec<String> },
    /// RLS disabled (resolved ambiguity #6): guarded by default + advisory.
    NoRls,
}

fn policies_for<'a>(db: &'a PgDatabase, table: &str) -> Vec<&'a PgPolicy> {
    db.policies.iter().filter(|p| p.table == table).collect()
}

/// Roles declared on the membership table's `role` column (CHECK IN or enum).
fn membership_roles(db: &PgDatabase, membership_table: &str) -> Vec<String> {
    let Some(table) = db.tables.get(membership_table) else {
        return Vec::new();
    };
    let Some(role_col) = table.columns.iter().find(|c| c.name == "role") else {
        return Vec::new();
    };
    if let Some(values) = &role_col.check_in_values {
        return values.clone();
    }
    db.enums.get(&role_col.pg_type).cloned().unwrap_or_default()
}

pub fn detect(db: &PgDatabase) -> TenancyDetection {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut outer_by_table: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in &db.policies {
        if let Recognized::Scopes(scopes) = recognize(p) {
            for s in scopes {
                if let Scope::TenantMembership {
                    membership_table,
                    outer_column,
                    ..
                } = s
                {
                    *counts.entry(membership_table.clone()).or_default() += 1;
                    outer_by_table
                        .entry(membership_table)
                        .or_default()
                        .insert(outer_column);
                }
            }
        }
    }
    // Most-referenced membership table; tie-break lexicographically smallest.
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let membership_table = ranked.first().map(|(name, _)| name.clone());

    // The tenant is what the membership table's fk (on the outer column) references.
    let tenant_table = membership_table.as_ref().and_then(|mt| {
        let outers = outer_by_table.get(mt)?;
        let table = db.tables.get(mt)?;
        table
            .fks
            .iter()
            .find(|fk| fk.columns.len() == 1 && outers.contains(&fk.columns[0]))
            .map(|fk| fk.ref_table.clone())
    });

    let member_roles = membership_table
        .as_ref()
        .map(|mt| membership_roles(db, mt))
        .unwrap_or_default();

    TenancyDetection {
        tenant_table,
        membership_table,
        member_roles,
    }
}

/// True when `table` carries a single-column fk to the tenant table.
fn has_tenant_fk(db: &PgDatabase, table: &str, det: &TenancyDetection) -> bool {
    let Some(tenant) = det.tenant_table.as_deref() else {
        return false;
    };
    db.tables
        .get(table)
        .is_some_and(|t| t.fks.iter().any(|fk| fk.ref_table == tenant))
}

pub fn table_access(db: &PgDatabase, det: &TenancyDetection) -> BTreeMap<String, TableAccess> {
    let mut out = BTreeMap::new();
    for (key, table) in &db.tables {
        if !key.starts_with("public.") {
            continue;
        }
        if !table.rls_enabled {
            out.insert(key.clone(), TableAccess::NoRls);
            continue;
        }
        out.insert(key.clone(), classify_table(db, det, key));
    }
    out
}

fn classify_table(db: &PgDatabase, det: &TenancyDetection, key: &str) -> TableAccess {
    let mut gap_reasons = Vec::new();
    let mut roles_by_command: BTreeMap<PolicyCommand, Vec<String>> = BTreeMap::new();
    let mut has_membership = false;
    let mut owner_column: Option<String> = None;
    let mut has_public_read = false;
    let mut has_authenticated = false;

    for p in policies_for(db, key) {
        match recognize(p) {
            Recognized::Gap { reason } => gap_reasons.push(reason),
            Recognized::Scopes(scopes) => {
                for s in scopes {
                    match s {
                        Scope::TenantMembership {
                            membership_table,
                            outer_column,
                            required_roles,
                        } => {
                            // The shape certifies syntax; only the DETECTED membership
                            // table certifies semantics (a share-list that matched the
                            // shape names a different table → gap, never guessed).
                            if det.membership_table.as_deref() != Some(membership_table.as_str())
                                || !db.tables.get(key).is_some_and(|t| {
                                    t.fks.iter().any(|fk| fk.columns == [outer_column.clone()])
                                })
                            {
                                gap_reasons.push(format!(
                                    "membership predicate references `{membership_table}`, not the detected tenant membership table"
                                ));
                            } else {
                                has_membership = true;
                                roles_by_command
                                    .entry(p.command)
                                    .or_default()
                                    .extend(required_roles);
                            }
                        }
                        Scope::Owner { column } => owner_column = Some(column),
                        Scope::PublicRead => has_public_read = true,
                        Scope::Authenticated => has_authenticated = true,
                        // Storage-only scopes never appear on public tables.
                        Scope::OwnerPrefix | Scope::BucketEq { .. } => {
                            gap_reasons.push("storage-only scope on a public table".into())
                        }
                    }
                }
            }
        }
    }

    if !gap_reasons.is_empty() {
        return TableAccess::Gap {
            reasons: gap_reasons,
        };
    }

    // Dedup role lists deterministically.
    for roles in roles_by_command.values_mut() {
        roles.sort();
        roles.dedup();
    }

    let scoped: Option<TableAccess> = if has_membership {
        Some(TableAccess::Tenant {
            required_roles_by_command: roles_by_command,
        })
    } else if let Some(column) = owner_column {
        if det.tenant_table.is_some() {
            if has_tenant_fk(db, key, det) {
                // R3(a): owner-scoped but carries the tenant fk → tenant scoping.
                Some(TableAccess::Tenant {
                    required_roles_by_command: BTreeMap::new(),
                })
            } else {
                // R3(a): owner-scoped, no tenant fk under org tenancy → never guessed.
                return TableAccess::Gap {
                    reasons: vec![format!(
                        "owner-scoped table `{key}` has no tenant fk under org tenancy — cannot preserve isolation deterministically"
                    )],
                };
            }
        } else {
            // R3(b): no org tenant anywhere → each user is their own tenant.
            Some(TableAccess::OwnerAsUserTenant {
                owner_column: column,
            })
        }
    } else if has_authenticated {
        Some(TableAccess::AuthOnly)
    } else {
        None
    };

    match (has_public_read, scoped) {
        (true, Some(write)) => TableAccess::PublicRead {
            write: Box::new(write),
        },
        (true, None) => TableAccess::PublicRead {
            write: Box::new(TableAccess::AuthOnly),
        },
        (false, Some(access)) => access,
        (false, None) => TableAccess::Gap {
            reasons: vec!["RLS enabled but no recognizable scope".into()],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    const ORG_SCHEMA: &str = r#"
create table public.workspaces (id uuid primary key, name text not null);
create table public.workspace_members (
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    user_id uuid not null,
    role text not null check (role in ('owner', 'member')),
    primary key (workspace_id, user_id)
);
create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id),
    email text not null
);
alter table public.customers enable row level security;
create policy m on public.customers using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));
create table public.todos (id uuid primary key, user_id uuid not null, title text);
alter table public.todos enable row level security;
create policy own on public.todos using (user_id = auth.uid());
"#;

    #[test]
    fn membership_join_detects_the_tenant_and_member_roles() {
        let db = PgDatabase::fold(&parse::split_and_parse(ORG_SCHEMA));
        let det = detect(&db);
        assert_eq!(det.tenant_table.as_deref(), Some("public.workspaces"));
        assert_eq!(
            det.membership_table.as_deref(),
            Some("public.workspace_members")
        );
        assert_eq!(
            det.member_roles,
            vec!["owner", "member"],
            "from the role CHECK, declaration order"
        );
    }

    #[test]
    fn owner_scoped_table_without_the_tenant_fk_is_a_blocking_gap_under_org_tenancy() {
        let db = PgDatabase::fold(&parse::split_and_parse(ORG_SCHEMA));
        let det = detect(&db);
        // todos has user_id = auth.uid() but no workspace_id → R3(a): gap, never guessed.
        let access = table_access(&db, &det);
        assert!(matches!(access["public.todos"], TableAccess::Gap { .. }));
        assert!(matches!(
            access["public.customers"],
            TableAccess::Tenant { .. }
        ));
    }

    #[test]
    fn pure_owner_apps_get_user_as_the_tenant() {
        let owner_only = r#"
create table public.todos (id uuid primary key, user_id uuid not null, title text);
alter table public.todos enable row level security;
create policy own on public.todos using (user_id = auth.uid());
"#;
        let db = PgDatabase::fold(&parse::split_and_parse(owner_only));
        let det = detect(&db);
        assert!(det.tenant_table.is_none());
        let access = table_access(&db, &det);
        // R3(b): no org tenant anywhere → owner tables scope by tenant User.
        assert!(matches!(
            access["public.todos"],
            TableAccess::OwnerAsUserTenant { .. }
        ));
    }
}
