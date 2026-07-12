//! The `--live` catalog front-end (resolved ambiguity #9): read Postgres
//! catalogs into the SAME `PgDatabase` IR the offline parser produces — one
//! translator, two front-ends. Policy `qual`/`with_check` arrive as TEXT and
//! parse with the same sqlparser expression parser; unparseable text degrades
//! to a gap (never guessed). Object bytes and table rows are NOT fetched live.

use super::pgmodel::{FkAction, PgColumn, PgDatabase, PgFk, PgPolicy, PgTable, PolicyCommand};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// Accumulates catalog rows into the shared IR. The pure, tested core of live
/// mode — `read_live` feeds it rows; the translator never knows the difference.
#[derive(Default)]
pub struct LiveBuilder {
    db: PgDatabase,
}

fn key(schema: &str, table: &str) -> String {
    format!("{}.{}", schema.to_lowercase(), table.to_lowercase())
}

impl LiveBuilder {
    fn table_mut(&mut self, schema: &str, table: &str) -> &mut PgTable {
        let k = key(schema, table);
        self.db.tables.entry(k).or_insert_with(|| PgTable {
            schema: schema.to_lowercase(),
            name: table.to_lowercase(),
            ..Default::default()
        })
    }

    pub fn column(&mut self, schema: &str, table: &str, name: &str, pg_type: &str, not_null: bool) {
        let col = PgColumn {
            name: name.to_lowercase(),
            pg_type: pg_type.to_lowercase(),
            not_null,
            unique: false,
            indexed: false,
            check_in_values: None,
        };
        self.table_mut(schema, table).columns.push(col);
    }

    pub fn pk(&mut self, schema: &str, table: &str, col: &str) {
        let name = col.to_lowercase();
        let t = self.table_mut(schema, table);
        if !t.pk.contains(&name) {
            t.pk.push(name.clone());
        }
        if let Some(c) = t.columns.iter_mut().find(|c| c.name == name) {
            c.not_null = true;
            c.unique = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fk(
        &mut self,
        schema: &str,
        table: &str,
        col: &str,
        ref_schema: &str,
        ref_table: &str,
        ref_col: &str,
        on_delete: &str,
    ) {
        let action = match on_delete.to_uppercase().as_str() {
            "CASCADE" => FkAction::Cascade,
            "SET NULL" => FkAction::SetNull,
            _ => FkAction::Restrict,
        };
        let fk = PgFk {
            columns: vec![col.to_lowercase()],
            ref_table: key(ref_schema, ref_table),
            ref_columns: vec![ref_col.to_lowercase()],
            on_delete: action,
        };
        self.table_mut(schema, table).fks.push(fk);
    }

    pub fn rls(&mut self, schema: &str, table: &str, enabled: bool) {
        self.table_mut(schema, table).rls_enabled = enabled;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn policy(
        &mut self,
        schema: &str,
        table: &str,
        name: &str,
        command: &str,
        roles: &[&str],
        qual: Option<&str>,
        with_check: Option<&str>,
    ) {
        let command = match command.to_uppercase().as_str() {
            "SELECT" => PolicyCommand::Select,
            "INSERT" => PolicyCommand::Insert,
            "UPDATE" => PolicyCommand::Update,
            "DELETE" => PolicyCommand::Delete,
            _ => PolicyCommand::All,
        };
        let original = qual
            .or(with_check)
            .map(|s| s.to_string())
            .unwrap_or_default();
        self.db.policies.push(PgPolicy {
            table: key(schema, table),
            name: name.to_string(),
            command,
            to_roles: roles.iter().map(|r| r.to_lowercase()).collect(),
            using: qual.and_then(parse_expr_text),
            with_check: with_check.and_then(parse_expr_text),
            original,
            line: 0,
        });
    }

    pub fn enum_label(&mut self, schema: &str, name: &str, label: &str) {
        self.db
            .enums
            .entry(key(schema, name))
            .or_default()
            .push(label.to_string());
    }

    pub fn publication(&mut self, name: &str, tables: &[(&str, &str)]) {
        let mut list: Vec<String> = tables.iter().map(|(s, t)| key(s, t)).collect();
        list.sort();
        list.dedup();
        self.db.publications.insert(name.to_string(), list);
    }

    pub fn finish(self) -> PgDatabase {
        self.db
    }
}

/// Parse a catalog `qual`/`with_check` TEXT into an `Expr` with the same parser
/// the offline path uses. Failure → `None` (the recognizer then gaps it).
fn parse_expr_text(text: &str) -> Option<sqlparser::ast::Expr> {
    Parser::new(&PostgreSqlDialect {})
        .try_with_sql(text)
        .ok()?
        .parse_expr()
        .ok()
}

/// What `read_live` returns for the shared translator tail.
pub struct LiveRead {
    pub db: PgDatabase,
    pub providers: Vec<String>,
    pub buckets_json: Option<String>,
}

/// Read Postgres catalogs into the shared IR. Never fetches table rows or object
/// bytes (an offline step). Best-effort per catalog: a missing extension
/// (pg_cron, storage, auth) is skipped, not fatal.
pub async fn read_live(conn: &str) -> Result<LiveRead, String> {
    use jerrycan_db::sea_orm::{ConnectionTrait, DbBackend, Statement};

    let db = jerrycan_db::Db::connect(conn)
        .await
        .map_err(|e| e.message().to_string())?;
    let c = db.conn();
    let q = |sql: &str| Statement::from_string(DbBackend::Postgres, sql.to_string());
    let mut b = LiveBuilder::default();

    // Columns (public/auth/storage).
    let rows = c
        .query_all(q(
            "select table_schema, table_name, column_name, data_type, is_nullable \
                      from information_schema.columns \
                      where table_schema in ('public','auth','storage') order by ordinal_position",
        ))
        .await
        .map_err(|e| e.to_string())?;
    for r in &rows {
        let g = |k: &str| r.try_get::<String>("", k).unwrap_or_default();
        b.column(
            &g("table_schema"),
            &g("table_name"),
            &g("column_name"),
            &g("data_type"),
            g("is_nullable") == "NO",
        );
    }

    // Primary keys.
    let rows = c
        .query_all(q("select tc.table_schema, tc.table_name, kcu.column_name \
                      from information_schema.table_constraints tc \
                      join information_schema.key_column_usage kcu \
                        on kcu.constraint_name=tc.constraint_name and kcu.table_schema=tc.table_schema \
                      where tc.constraint_type='PRIMARY KEY' and tc.table_schema='public'"))
        .await
        .map_err(|e| e.to_string())?;
    for r in &rows {
        let g = |k: &str| r.try_get::<String>("", k).unwrap_or_default();
        b.pk(&g("table_schema"), &g("table_name"), &g("column_name"));
    }

    // Foreign keys.
    let rows = c
        .query_all(q("select tc.table_schema, tc.table_name, kcu.column_name, \
                        ccu.table_schema as ref_schema, ccu.table_name as ref_table, ccu.column_name as ref_col, \
                        rc.delete_rule \
                      from information_schema.table_constraints tc \
                      join information_schema.key_column_usage kcu on kcu.constraint_name=tc.constraint_name and kcu.table_schema=tc.table_schema \
                      join information_schema.referential_constraints rc on rc.constraint_name=tc.constraint_name \
                      join information_schema.constraint_column_usage ccu on ccu.constraint_name=rc.unique_constraint_name \
                      where tc.constraint_type='FOREIGN KEY' and tc.table_schema='public'"))
        .await
        .map_err(|e| e.to_string())?;
    for r in &rows {
        let g = |k: &str| r.try_get::<String>("", k).unwrap_or_default();
        b.fk(
            &g("table_schema"),
            &g("table_name"),
            &g("column_name"),
            &g("ref_schema"),
            &g("ref_table"),
            &g("ref_col"),
            &g("delete_rule"),
        );
    }

    // Row-level security flag.
    let rows = c
        .query_all(q(
            "select n.nspname as s, c.relname as t, c.relrowsecurity as e \
                      from pg_class c join pg_namespace n on n.oid=c.relnamespace \
                      where n.nspname='public' and c.relkind='r'",
        ))
        .await
        .map_err(|e| e.to_string())?;
    for r in &rows {
        let s = r.try_get::<String>("", "s").unwrap_or_default();
        let t = r.try_get::<String>("", "t").unwrap_or_default();
        let e = r.try_get::<bool>("", "e").unwrap_or(false);
        b.rls(&s, &t, e);
    }

    // Policies (qual/with_check as TEXT).
    let rows = c
        .query_all(q("select schemaname, tablename, policyname, cmd, \
                        array_to_string(roles, ',') as roles, qual, with_check from pg_policies"))
        .await
        .map_err(|e| e.to_string())?;
    for r in &rows {
        let g = |k: &str| r.try_get::<String>("", k).unwrap_or_default();
        let roles_csv = g("roles");
        let roles: Vec<&str> = roles_csv.split(',').filter(|s| !s.is_empty()).collect();
        let qual = r.try_get::<String>("", "qual").ok();
        let wc = r.try_get::<String>("", "with_check").ok();
        b.policy(
            &g("schemaname"),
            &g("tablename"),
            &g("policyname"),
            &g("cmd"),
            &roles,
            qual.as_deref(),
            wc.as_deref(),
        );
    }

    // Publications.
    if let Ok(rows) = c
        .query_all(q(
            "select pubname, schemaname, tablename from pg_publication_tables",
        ))
        .await
    {
        let mut by_pub: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        for r in &rows {
            let g = |k: &str| r.try_get::<String>("", k).unwrap_or_default();
            by_pub
                .entry(g("pubname"))
                .or_default()
                .push((g("schemaname"), g("tablename")));
        }
        for (name, tables) in &by_pub {
            let refs: Vec<(&str, &str)> = tables
                .iter()
                .map(|(s, t)| (s.as_str(), t.as_str()))
                .collect();
            b.publication(name, &refs);
        }
    }

    // Enums.
    if let Ok(rows) = c
        .query_all(q(
            "select n.nspname as s, t.typname as name, e.enumlabel as label \
                      from pg_enum e join pg_type t on t.oid=e.enumtypid \
                      join pg_namespace n on n.oid=t.typnamespace order by e.enumsortorder",
        ))
        .await
    {
        for r in &rows {
            let g = |k: &str| r.try_get::<String>("", k).unwrap_or_default();
            b.enum_label(&g("s"), &g("name"), &g("label"));
        }
    }

    // OAuth providers (auth.identities may be absent).
    let mut providers = Vec::new();
    if let Ok(rows) = c
        .query_all(q("select distinct provider from auth.identities"))
        .await
    {
        for r in &rows {
            if let Ok(p) = r.try_get::<String>("", "provider") {
                providers.push(p);
            }
        }
        providers.sort();
        providers.dedup();
    }

    // Buckets (storage may be absent).
    let buckets_json = c
        .query_one(q(
            "select coalesce(json_agg(x), '[]'::json)::text as j from storage.buckets x",
        ))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<String>("", "j").ok());

    Ok(LiveRead {
        db: b.finish(),
        providers,
        buckets_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_rows_fold_into_the_same_ir_as_the_offline_parser() {
        let mut b = LiveBuilder::default();
        b.column(
            "public",
            "customers",
            "email",
            "text",
            /*not_null*/ true,
        );
        b.column("public", "customers", "id", "uuid", true);
        b.pk("public", "customers", "id");
        b.fk(
            "public",
            "customers",
            "workspace_id",
            "public",
            "workspaces",
            "id",
            "CASCADE",
        );
        b.rls("public", "customers", true);
        b.policy(
            "public",
            "customers",
            "m",
            "ALL",
            &["public"],
            Some("(workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = auth.uid()))"),
            None,
        );
        let db = b.finish();
        let t = &db.tables["public.customers"];
        assert!(t.rls_enabled && t.pk == vec!["id"]);
        assert_eq!(
            t.fks[0].on_delete,
            crate::platform::migrate::pgmodel::FkAction::Cascade
        );
        assert!(
            db.policies[0].using.is_some(),
            "qual text parsed into an Expr"
        );
    }

    #[test]
    #[ignore = "needs a live postgres; set JERRYCAN_TEST_PG_URL"]
    fn read_live_folds_a_real_schema_into_the_ir() {
        // Round-trip a tiny schema through the catalog reader against a real
        // Postgres. Never runs in CI (external service); documents the contract.
        let url = std::env::var("JERRYCAN_TEST_PG_URL").expect("JERRYCAN_TEST_PG_URL");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let read = rt.block_on(super::read_live(&url)).expect("read_live");
        assert!(
            read.db.tables.keys().any(|k| k.starts_with("public.")),
            "at least one public table folds from the catalogs"
        );
    }

    #[test]
    fn unparseable_policy_text_degrades_to_a_gap_not_a_crash() {
        let mut b = LiveBuilder::default();
        b.column("public", "t", "id", "uuid", true);
        b.policy(
            "public",
            "t",
            "weird",
            "ALL",
            &[],
            Some("some_extension_fn(id ==> 3)"),
            None,
        );
        let db = b.finish();
        assert!(
            db.policies[0].using.is_none(),
            "kept with original text; recognizer will gap it"
        );
        assert!(db.policies[0].original.contains("some_extension_fn"));
    }
}
