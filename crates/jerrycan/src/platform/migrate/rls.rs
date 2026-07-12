//! The conservative RLS recognizer (spec §4). Canonical, unambiguous shapes
//! ONLY. `recognize` returns Gap for anything else — the gap report + generated
//! isolation tests are the safety net; this module must never guess (Resolved
//! decision 3: "unrecognized → gap report (never guessed)").

use super::pgmodel::{PgPolicy, PolicyCommand};
use sqlparser::ast::{
    AccessExpr, BinaryOperator, Expr, Function, GroupByExpr, Query, Select, SelectItem, SetExpr,
    Subscript, TableFactor, Value,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `<column> = auth.uid()`
    Owner { column: String },
    /// `<outer_column> IN (SELECT <outer_column> FROM <m> WHERE <user_col> = auth.uid() [AND role …])`
    /// or the equivalent EXISTS join. Validated against the detected membership
    /// table in tenancy.rs — the recognizer only certifies the SHAPE.
    TenantMembership {
        outer_column: String,
        membership_table: String,
        required_roles: Vec<String>,
    },
    /// `(storage.foldername(name))[1] = auth.uid()[::text]`
    OwnerPrefix,
    /// `USING (true)` on SELECT.
    PublicRead,
    /// `auth.uid() IS NOT NULL`, or `TO authenticated`, or `auth.role() = 'authenticated'`.
    Authenticated,
    /// `bucket_id = '<name>'` — meaningful only on storage.objects policies.
    BucketEq { bucket: String },
}

#[derive(Debug)]
pub enum Recognized {
    Scopes(Vec<Scope>),
    Gap { reason: String },
}

pub fn recognize(policy: &PgPolicy) -> Recognized {
    // AS RESTRICTIVE composes by ANDing with every other policy on the table —
    // the per-policy scope model cannot express that, so translating any part
    // of such a table mechanically could over-grant. Never guessed.
    if policy.restrictive {
        return Recognized::Gap {
            reason:
                "AS RESTRICTIVE policies AND with the table's other policies — not auto-translated"
                    .into(),
        };
    }
    // The predicate exists but did not parse: its meaning is unknown.
    if policy.unreadable {
        return Recognized::Gap {
            reason: "policy predicate could not be parsed — not auto-translated".into(),
        };
    }
    // `TO <role>` restricted to a database role we don't model (service_role,
    // custom roles): the grant applies to a principal jerrycan doesn't have.
    if let Some(role) = policy
        .to_roles
        .iter()
        .find(|r| !matches!(r.as_str(), "public" | "anon" | "authenticated"))
    {
        return Recognized::Gap {
            reason: format!(
                "policy is granted TO database role `{role}` which has no jerrycan principal — not auto-translated"
            ),
        };
    }
    let to_authenticated = policy.to_roles.iter().any(|r| r == "authenticated");
    let exprs: Vec<&Expr> = [policy.using.as_ref(), policy.with_check.as_ref()]
        .into_iter()
        .flatten()
        .collect();
    if exprs.is_empty() {
        // `TO authenticated` with no USING/WITH CHECK is still the logged-in gate.
        if to_authenticated {
            return Recognized::Scopes(vec![Scope::Authenticated]);
        }
        return Recognized::Gap {
            reason: "policy has neither USING nor WITH CHECK we can read".into(),
        };
    }
    let mut scopes = Vec::new();
    for expr in exprs {
        for conjunct in split_conjuncts(expr) {
            match classify(conjunct, policy, to_authenticated) {
                Some(scope) => scopes.push(scope),
                None => {
                    return Recognized::Gap {
                        reason: format!(
                            "predicate `{conjunct}` is not a canonical shape — not auto-translated"
                        ),
                    };
                }
            }
        }
    }
    scopes.sort_by_key(|s| format!("{s:?}"));
    scopes.dedup();
    Recognized::Scopes(scopes)
}

/// The `bucket_id = '<name>'` a storage.objects policy targets, even when the
/// rest of the policy doesn't recognize (so a gapped policy still names its
/// bucket in the gap report). Independent of full recognition.
pub fn find_bucket(policy: &PgPolicy) -> Option<String> {
    [policy.using.as_ref(), policy.with_check.as_ref()]
        .into_iter()
        .flatten()
        .flat_map(split_conjuncts)
        .find_map(|c| match classify(c, policy, false) {
            Some(Scope::BucketEq { bucket }) => Some(bucket),
            _ => None,
        })
}

/// Unwrap `Nested`/`Cast` (never `Subquery` — that's handled by `is_auth_uid`).
fn strip(expr: &Expr) -> &Expr {
    match expr {
        Expr::Nested(inner) => strip(inner),
        Expr::Cast { expr, .. } => strip(expr),
        _ => expr,
    }
}

/// Split on top-level `AND` (recursively). `OR` is NOT split — it falls through
/// to `classify`, which won't match, so an OR-composition gaps.
fn split_conjuncts(expr: &Expr) -> Vec<&Expr> {
    match strip(expr) {
        Expr::BinaryOp {
            op: BinaryOperator::And,
            left,
            right,
        } => {
            let mut v = split_conjuncts(left);
            v.extend(split_conjuncts(right));
            v
        }
        other => vec![other],
    }
}

fn fn_name_is(f: &Function, parts: &[&str]) -> bool {
    let idents: Vec<String> = f
        .name
        .0
        .iter()
        .filter_map(|p| p.as_ident().map(|i| i.value.to_lowercase()))
        .collect();
    idents == parts
}

fn query_is_select_auth_uid(q: &Query) -> bool {
    let SetExpr::Select(select) = q.body.as_ref() else {
        return false;
    };
    select.from.is_empty()
        && select.selection.is_none()
        && select.projection.len() == 1
        && matches!(&select.projection[0], SelectItem::UnnamedExpr(e) if is_auth_uid(e))
}

/// `auth.uid()`, `(SELECT auth.uid())`, or either cast — the authenticated
/// principal marker.
fn is_auth_uid(expr: &Expr) -> bool {
    match strip(expr) {
        Expr::Function(f) => fn_name_is(f, &["auth", "uid"]),
        Expr::Subquery(q) => query_is_select_auth_uid(q),
        _ => false,
    }
}

fn is_auth_role(expr: &Expr) -> bool {
    matches!(strip(expr), Expr::Function(f) if fn_name_is(f, &["auth", "role"]))
}

fn as_column(expr: &Expr) -> Option<String> {
    match strip(expr) {
        Expr::Identifier(i) => Some(i.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| i.value.to_lowercase()),
        _ => None,
    }
}

fn as_string_lit(expr: &Expr) -> Option<String> {
    match strip(expr) {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn is_int_literal(expr: &Expr, n: u64) -> bool {
    matches!(strip(expr), Expr::Value(v)
        if matches!(&v.value, Value::Number(s, _) if s == &n.to_string()))
}

/// `(storage.foldername(<col>))[1]` — the folder-per-user prefix accessor.
fn is_foldername_first(expr: &Expr) -> bool {
    if let Expr::CompoundFieldAccess { root, access_chain } = strip(expr) {
        let root_ok =
            matches!(strip(root), Expr::Function(f) if fn_name_is(f, &["storage", "foldername"]));
        let idx_ok = access_chain.len() == 1
            && matches!(&access_chain[0],
                AccessExpr::Subscript(Subscript::Index { index }) if is_int_literal(index, 1));
        return root_ok && idx_ok;
    }
    false
}

fn classify(expr: &Expr, policy: &PgPolicy, to_authenticated: bool) -> Option<Scope> {
    // `TO authenticated` (with no anon/public grant) gates every predicate
    // behind login — `USING (true)` under it is authenticated-read, NOT public.
    let anon_visible =
        policy.to_roles.is_empty() || policy.to_roles.iter().any(|r| r == "anon" || r == "public");
    match strip(expr) {
        Expr::Value(v) if matches!(&v.value, Value::Boolean(true)) => match policy.command {
            PolicyCommand::Select if anon_visible => Some(Scope::PublicRead),
            PolicyCommand::Select if to_authenticated => Some(Scope::Authenticated),
            PolicyCommand::All if to_authenticated => Some(Scope::Authenticated),
            _ => None,
        },
        Expr::IsNotNull(inner) if is_auth_uid(inner) => Some(Scope::Authenticated),
        Expr::BinaryOp {
            op: BinaryOperator::Eq,
            left,
            right,
        } => classify_eq(left, right),
        Expr::InSubquery {
            expr: lhs,
            subquery,
            negated: false,
        } => {
            let col = as_column(lhs)?;
            match_in_membership(&col, subquery)
        }
        Expr::Exists {
            subquery,
            negated: false,
        } => match_exists_membership(subquery),
        _ => None,
    }
}

fn classify_eq(left: &Expr, right: &Expr) -> Option<Scope> {
    // Owner: <col> = auth.uid() (either order).
    if is_auth_uid(left)
        && let Some(column) = as_column(right)
    {
        return Some(Scope::Owner { column });
    }
    if is_auth_uid(right)
        && let Some(column) = as_column(left)
    {
        return Some(Scope::Owner { column });
    }
    // auth.role() = 'authenticated'
    if is_auth_role(left) || is_auth_role(right) {
        let lit = as_string_lit(left).or_else(|| as_string_lit(right));
        return if lit.as_deref() == Some("authenticated") {
            Some(Scope::Authenticated)
        } else {
            None
        };
    }
    // bucket_id = '<name>'
    if as_column(left).as_deref() == Some("bucket_id")
        && let Some(bucket) = as_string_lit(right)
    {
        return Some(Scope::BucketEq { bucket });
    }
    if as_column(right).as_deref() == Some("bucket_id")
        && let Some(bucket) = as_string_lit(left)
    {
        return Some(Scope::BucketEq { bucket });
    }
    // OwnerPrefix: (storage.foldername(name))[1] = auth.uid()[::text]
    if (is_foldername_first(left) && is_auth_uid(right))
        || (is_foldername_first(right) && is_auth_uid(left))
    {
        return Some(Scope::OwnerPrefix);
    }
    None
}

fn single_select(q: &Query) -> Option<&Select> {
    match q.body.as_ref() {
        SetExpr::Select(select) => Some(select),
        _ => None,
    }
}

/// The subquery must be a single, plain SELECT (no distinct/group/having/sort).
fn select_is_plain(select: &Select) -> bool {
    select.distinct.is_none()
        && select.having.is_none()
        && select.sort_by.is_empty()
        && matches!(&select.group_by, GroupByExpr::Expressions(e, m) if e.is_empty() && m.is_empty())
}

/// The single FROM table (no joins), as "schema.table".
fn single_from_table(select: &Select) -> Option<String> {
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return None;
    }
    match &select.from[0].relation {
        TableFactor::Table { name, .. } => {
            let parts: Vec<String> = name
                .0
                .iter()
                .filter_map(|p| p.as_ident().map(|i| i.value.to_lowercase()))
                .collect();
            Some(match parts.len() {
                0 => return None,
                1 => format!("public.{}", parts[0]),
                _ => parts.join("."),
            })
        }
        _ => None,
    }
}

/// The membership auth conjunct: `user_id = auth.uid()` (either order). The
/// column must be literally `user_id` — jerrycan's membership convention and
/// the column the generated Tenant guard queries. A different column
/// (`invited_by`, `created_by`, …) means DIFFERENT semantics: translating it
/// to plain membership would grant every member what the source scoped to a
/// specific relationship. Never guessed.
fn is_membership_auth_conjunct(left: &Expr, right: &Expr) -> bool {
    (is_auth_uid(left) && as_column(right).as_deref() == Some("user_id"))
        || (is_auth_uid(right) && as_column(left).as_deref() == Some("user_id"))
}

/// Fold a membership `WHERE` into required roles, requiring exactly one
/// `user_id = auth.uid()` conjunct. Any other shape → None (gap).
fn membership_roles(where_expr: &Expr) -> Option<Vec<String>> {
    let mut roles = Vec::new();
    let mut auth_seen = false;
    for c in split_conjuncts(where_expr) {
        match strip(c) {
            Expr::BinaryOp {
                op: BinaryOperator::Eq,
                left,
                right,
            } => {
                if is_membership_auth_conjunct(left, right) {
                    if auth_seen {
                        return None;
                    }
                    auth_seen = true;
                } else if as_column(left).as_deref() == Some("role") {
                    roles.push(as_string_lit(right)?);
                } else if as_column(right).as_deref() == Some("role") {
                    roles.push(as_string_lit(left)?);
                } else {
                    return None;
                }
            }
            Expr::InList {
                expr,
                list,
                negated: false,
            } if as_column(expr).as_deref() == Some("role") => {
                for item in list {
                    roles.push(as_string_lit(item)?);
                }
            }
            _ => return None,
        }
    }
    auth_seen.then_some(roles)
}

fn match_in_membership(outer_col: &str, subquery: &Query) -> Option<Scope> {
    let select = single_select(subquery)?;
    if !select_is_plain(select) {
        return None;
    }
    // Projection must select exactly the outer column (the canonical template).
    if select.projection.len() != 1
        || !matches!(&select.projection[0], SelectItem::UnnamedExpr(e) if as_column(e).as_deref() == Some(outer_col))
    {
        return None;
    }
    let table = single_from_table(select)?;
    let roles = membership_roles(select.selection.as_ref()?)?;
    Some(Scope::TenantMembership {
        outer_column: outer_col.to_string(),
        membership_table: table,
        required_roles: roles,
    })
}

fn match_exists_membership(subquery: &Query) -> Option<Scope> {
    let select = single_select(subquery)?;
    if !select_is_plain(select) {
        return None;
    }
    let table = single_from_table(select)?;
    let where_expr = select.selection.as_ref()?;
    let mut outer_col: Option<String> = None;
    let mut auth_seen = false;
    let mut roles = Vec::new();
    for c in split_conjuncts(where_expr) {
        match strip(c) {
            Expr::BinaryOp {
                op: BinaryOperator::Eq,
                left,
                right,
            } => {
                // auth: user_id = auth.uid()
                if is_membership_auth_conjunct(left, right) {
                    if auth_seen {
                        return None;
                    }
                    auth_seen = true;
                    continue;
                }
                // correlation: X.col = Y.col with the SAME column name on both sides.
                if let (Some(lc), Some(rc)) = (as_column(left), as_column(right)) {
                    if lc == "role" || rc == "role" {
                        // role = 'lit' is handled below; two columns named role is odd → gap.
                        return None;
                    }
                    if lc != rc || outer_col.is_some() {
                        return None;
                    }
                    outer_col = Some(lc);
                    continue;
                }
                // role = 'lit'
                if as_column(left).as_deref() == Some("role") {
                    roles.push(as_string_lit(right)?);
                } else if as_column(right).as_deref() == Some("role") {
                    roles.push(as_string_lit(left)?);
                } else {
                    return None;
                }
            }
            Expr::InList {
                expr,
                list,
                negated: false,
            } if as_column(expr).as_deref() == Some("role") => {
                for item in list {
                    roles.push(as_string_lit(item)?);
                }
            }
            _ => return None,
        }
    }
    let outer = outer_col?;
    if !auth_seen {
        return None;
    }
    Some(Scope::TenantMembership {
        outer_column: outer,
        membership_table: table,
        required_roles: roles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    fn policy_scopes(sql: &str) -> Recognized {
        let full = format!("create table public.t (id uuid primary key);\n{sql}");
        let db = PgDatabase::fold(&parse::split_and_parse(&full));
        recognize(&db.policies[0])
    }

    #[test]
    fn owner_eq_auth_uid_recognizes_both_orders_and_select_wrapping() {
        for sql in [
            r#"create policy p on public.t using (user_id = auth.uid());"#,
            r#"create policy p on public.t using (auth.uid() = user_id);"#,
            r#"create policy p on public.t using ((select auth.uid()) = user_id);"#,
        ] {
            match policy_scopes(sql) {
                Recognized::Scopes(s) => assert_eq!(
                    s,
                    vec![Scope::Owner {
                        column: "user_id".into()
                    }],
                    "{sql}"
                ),
                Recognized::Gap { reason } => panic!("{sql} must recognize: {reason}"),
            }
        }
    }

    #[test]
    fn membership_join_recognizes_in_and_exists_shapes() {
        let in_shape = r#"create policy p on public.t using
            (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));"#;
        let exists_shape = r#"create policy p on public.t using
            (exists (select 1 from public.workspace_members m
                     where m.workspace_id = t.workspace_id and m.user_id = auth.uid()));"#;
        for sql in [in_shape, exists_shape] {
            match policy_scopes(sql) {
                Recognized::Scopes(s) => assert_eq!(
                    s,
                    vec![Scope::TenantMembership {
                        outer_column: "workspace_id".into(),
                        membership_table: "public.workspace_members".into(),
                        required_roles: vec![],
                    }],
                    "{sql}"
                ),
                Recognized::Gap { reason } => panic!("{sql} must recognize: {reason}"),
            }
        }
    }

    #[test]
    fn membership_join_with_role_filter_carries_required_roles() {
        let sql = r#"create policy p on public.t for delete using
            (workspace_id in (select workspace_id from public.workspace_members
                              where user_id = auth.uid() and role = 'owner'));"#;
        match policy_scopes(sql) {
            Recognized::Scopes(s) => assert_eq!(
                s,
                vec![Scope::TenantMembership {
                    outer_column: "workspace_id".into(),
                    membership_table: "public.workspace_members".into(),
                    required_roles: vec!["owner".into()],
                }]
            ),
            Recognized::Gap { reason } => panic!("must recognize: {reason}"),
        }
    }

    #[test]
    fn storage_foldername_prefix_and_bucket_eq_recognize_together() {
        let sql = r#"create policy p on storage.objects for all using
            (bucket_id = 'avatars' and (storage.foldername(name))[1] = auth.uid()::text);"#;
        match policy_scopes(sql) {
            Recognized::Scopes(s) => assert_eq!(
                s,
                vec![
                    Scope::BucketEq {
                        bucket: "avatars".into()
                    },
                    Scope::OwnerPrefix
                ]
            ),
            Recognized::Gap { reason } => panic!("must recognize: {reason}"),
        }
    }

    #[test]
    fn public_read_and_role_gates_recognize() {
        match policy_scopes(r#"create policy p on public.t for select using (true);"#) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::PublicRead]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
        match policy_scopes(r#"create policy p on public.t using (auth.uid() is not null);"#) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::Authenticated]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
        match policy_scopes(r#"create policy p on public.t to authenticated using (true);"#) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::Authenticated]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
    }

    #[test]
    fn near_miss_shapes_gap_and_never_guess() {
        // Share-list join (not a membership shape: subquery filters on the row pk).
        let share = r#"create policy p on public.t for select using
            (exists (select 1 from public.note_shares s where s.note_id = t.id and s.shared_with = auth.uid()));"#;
        // OR-composition is never mechanical. A `true` WRITE policy is never public-read.
        let ored = r#"create policy p on public.t using (user_id = auth.uid() or is_public);"#;
        // OR of two INDIVIDUALLY-canonical shapes must also gap — splitting OR
        // like AND would certify both scopes and silently change semantics.
        let ored_canonical = r#"create policy p on public.t using
            (user_id = auth.uid()
             or workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));"#;
        let open_write = r#"create policy p on public.t for insert with check (true);"#;
        // Arbitrary jwt-claim condition.
        let claim = r#"create policy p on public.t using ((auth.jwt() ->> 'plan') = 'pro');"#;
        for sql in [share, ored, ored_canonical, open_write, claim] {
            assert!(
                matches!(policy_scopes(sql), Recognized::Gap { .. }),
                "{sql} MUST gap"
            );
        }
    }

    #[test]
    fn restrictive_policies_always_gap_even_when_the_predicate_is_canonical() {
        // AS RESTRICTIVE ANDs with the table's other policies. Recognizing its
        // owner shape as a permissive scope would let a `USING (true)` sibling
        // read publicly what the source restricts to the row owner.
        let sql = r#"create policy p on public.t as restrictive for select using (user_id = auth.uid());"#;
        assert!(
            matches!(policy_scopes(sql), Recognized::Gap { .. }),
            "restrictive MUST gap"
        );
        // …and the permissive form of the same predicate still recognizes.
        let permissive =
            r#"create policy p on public.t as permissive for select using (user_id = auth.uid());"#;
        assert!(matches!(policy_scopes(permissive), Recognized::Scopes(_)));
    }

    #[test]
    fn select_true_to_authenticated_is_authenticated_never_public_read() {
        // The classic Supabase pattern: `for select to authenticated using (true)`.
        // Anonymous users CANNOT read this in the source — PublicRead would leak.
        match policy_scopes(
            r#"create policy p on public.t for select to authenticated using (true);"#,
        ) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::Authenticated]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
        // With an anon grant the read really is public.
        match policy_scopes(
            r#"create policy p on public.t for select to anon, authenticated using (true);"#,
        ) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::PublicRead]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
    }

    #[test]
    fn policies_granted_to_unmodeled_database_roles_gap() {
        // `TO service_role` (or any custom role) has no jerrycan principal;
        // translating `USING (true)` to public-read would expose backend-only data.
        for sql in [
            r#"create policy p on public.t for select to service_role using (true);"#,
            r#"create policy p on public.t to reporting_bot using (user_id = auth.uid());"#,
        ] {
            assert!(
                matches!(policy_scopes(sql), Recognized::Gap { .. }),
                "{sql} MUST gap"
            );
        }
    }

    #[test]
    fn membership_auth_column_must_be_user_id() {
        // `invited_by = auth.uid()` scopes to a different relationship than
        // membership; certifying it as TenantMembership would grant every
        // member what the source granted only to inviters.
        let sql = r#"create policy p on public.t using
            (workspace_id in (select workspace_id from public.workspace_members where invited_by = auth.uid()));"#;
        assert!(
            matches!(policy_scopes(sql), Recognized::Gap { .. }),
            "non-user_id auth column MUST gap"
        );
        let exists = r#"create policy p on public.t using
            (exists (select 1 from public.workspace_members m
                     where m.workspace_id = t.workspace_id and m.invited_by = auth.uid()));"#;
        assert!(
            matches!(policy_scopes(exists), Recognized::Gap { .. }),
            "non-user_id auth column MUST gap (exists shape)"
        );
    }

    #[test]
    fn an_unreadable_offline_policy_statement_still_reaches_the_recognizer_and_gaps() {
        // sqlparser rejects the statement; the policy must NOT silently vanish
        // (its table's other policies would then decide access alone).
        let full = "create table public.t (id uuid primary key);\n\
                    create policy weird on public.t using (%%%);";
        let db = PgDatabase::fold(&parse::split_and_parse(full));
        assert_eq!(db.policies.len(), 1, "unreadable policy is still a policy");
        assert_eq!(db.policies[0].table, "public.t");
        assert!(db.policies[0].unreadable);
        assert!(matches!(recognize(&db.policies[0]), Recognized::Gap { .. }));
    }
}
