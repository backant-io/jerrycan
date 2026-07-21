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

    let mut member_roles = membership_table
        .as_ref()
        .map(|mt| membership_roles(db, mt))
        .unwrap_or_default();
    // #139: a source membership table with NO role constraint (no CHECK IN, no
    // enum) yields an empty role set — which the emitted design's tenancy block
    // would fail JC0548 on (0.6.0 requires non-empty `member_roles`, since
    // `member_roles[0]` is the admin role the generated member surface gates
    // on). Synthesize the default pair, admin first, so role-less migrations
    // keep translating; `authmap::build_auth` derives auth.roles from the same
    // list, so the design stays self-consistent.
    if membership_table.is_some() && member_roles.is_empty() {
        member_roles = vec!["admin".to_string(), "member".to_string()];
    }

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

/// Which SQL commands the table's RLS policies actually cover. Postgres
/// default-denies any command with no policy — a SELECT-only policy set means
/// writes are denied for everyone, so the translation must not emit write
/// endpoints (that would grant what the source denies). NoRls/Gap tables get
/// the full set: their endpoints are fully guarded and the agent reviews them.
pub fn covered_commands(
    db: &PgDatabase,
    table: &str,
    access: &TableAccess,
) -> std::collections::BTreeSet<PolicyCommand> {
    if matches!(access, TableAccess::NoRls | TableAccess::Gap { .. }) {
        return super::crud::all_commands();
    }
    let mut covered = std::collections::BTreeSet::new();
    for p in policies_for(db, table) {
        if matches!(recognize(p), Recognized::Scopes(_)) {
            if p.command == PolicyCommand::All {
                return super::crud::all_commands();
            }
            covered.insert(p.command);
        }
    }
    covered
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
                            } else if let Some(unknown) = required_roles
                                .iter()
                                .find(|r| !det.member_roles.contains(r))
                            {
                                // A role literal outside the membership role set
                                // can't become required_roles (auth.roles wouldn't
                                // declare it — questions.rs would reject the design).
                                gap_reasons.push(format!(
                                    "policy requires role `{unknown}` which is not among the membership table's declared roles"
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
    fn a_membership_shaped_policy_on_a_foreign_table_gaps_never_guessed() {
        // `group_id in (select group_id from public.team_links where user_id =
        // auth.uid())` matches the membership SHAPE but names a table that is
        // NOT the detected tenant membership table. Translating it to tenant
        // scoping would swap the isolation boundary — the semantic validation
        // in classify_table must gap it.
        let schema = format!(
            "{ORG_SCHEMA}\n\
             create policy m2 on public.customers for delete using\n\
                 (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));\n\
             create table public.team_links (group_id uuid not null, user_id uuid not null);\n\
             create table public.docs (id uuid primary key, group_id uuid not null);\n\
             alter table public.docs enable row level security;\n\
             create policy d on public.docs using\n\
                 (group_id in (select group_id from public.team_links where user_id = auth.uid()));"
        );
        let db = PgDatabase::fold(&parse::split_and_parse(&schema));
        let det = detect(&db);
        assert_eq!(
            det.membership_table.as_deref(),
            Some("public.workspace_members"),
            "workspace_members outranks team_links"
        );
        let access = table_access(&db, &det);
        match &access["public.docs"] {
            TableAccess::Gap { reasons } => assert!(
                reasons.iter().any(|r| r.contains("team_links")),
                "{reasons:?}"
            ),
            other => panic!("docs must gap, got {other:?}"),
        }
    }

    #[test]
    fn a_role_literal_outside_the_membership_roles_gaps_instead_of_an_invalid_design() {
        // The design's auth.roles come from the membership role CHECK; a policy
        // requiring a role outside that set would emit required_roles that
        // questions.rs rejects — the whole migration would abort. Gap instead.
        let schema = format!(
            "{ORG_SCHEMA}\n\
             create policy su on public.customers for delete using\n\
                 (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid() and role = 'superadmin'));"
        );
        let db = PgDatabase::fold(&parse::split_and_parse(&schema));
        let det = detect(&db);
        let access = table_access(&db, &det);
        match &access["public.customers"] {
            TableAccess::Gap { reasons } => assert!(
                reasons.iter().any(|r| r.contains("superadmin")),
                "{reasons:?}"
            ),
            other => panic!("customers must gap, got {other:?}"),
        }
    }

    /// #139 (0.6.0 release blocker): a source membership table with NO role
    /// constraint (no CHECK IN, no enum — common in hand-rolled Supabase
    /// schemas) must NOT emit an empty `member_roles`: the migrated design
    /// would fail JC0548 (`member_roles` non-empty is what makes
    /// `member_roles[0]` a reliable admin role for the generated member
    /// surface), turning a previously-translatable migration into a hard
    /// check failure. The default pair is synthesized, admin FIRST.
    #[test]
    fn a_role_less_membership_table_synthesizes_the_default_member_roles() {
        let schema = r#"
create table public.teams (id uuid primary key, name text not null);
create table public.team_members (
    team_id uuid not null references public.teams(id) on delete cascade,
    user_id uuid not null,
    role text not null,
    primary key (team_id, user_id)
);
create table public.notes (
    id uuid primary key,
    team_id uuid not null references public.teams(id),
    body text not null
);
alter table public.notes enable row level security;
create policy m on public.notes using
    (team_id in (select team_id from public.team_members where user_id = auth.uid()));
"#;
        let db = PgDatabase::fold(&parse::split_and_parse(schema));
        let det = detect(&db);
        assert_eq!(det.membership_table.as_deref(), Some("public.team_members"));
        assert_eq!(
            det.member_roles,
            vec!["admin", "member"],
            "no role CHECK/enum -> the default pair, admin first (JC0548 needs non-empty)"
        );
        // The scoped translation itself is untouched by the synthesized roles.
        let access = table_access(&db, &det);
        assert!(matches!(access["public.notes"], TableAccess::Tenant { .. }));
    }

    /// The synthesized default NEVER overrides declared roles: a membership
    /// table WITH a role CHECK keeps its declared set verbatim (the #139 fix
    /// touches only the empty-roles path).
    #[test]
    fn declared_member_roles_are_never_overridden_by_the_default() {
        let db = PgDatabase::fold(&parse::split_and_parse(ORG_SCHEMA));
        let det = detect(&db);
        assert_eq!(det.member_roles, vec!["owner", "member"]);
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
