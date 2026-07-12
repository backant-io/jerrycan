//! The Postgres IR both front-ends produce: everything downstream stages need,
//! nothing sqlparser-shaped leaks past this module except policy `Expr`s.

use super::parse::RawStatement;
use sqlparser::ast::{
    self, AlterTableOperation, ArrayElemTypeDef, ColumnOption, CreatePolicyCommand, DataType, Expr,
    ObjectName, Owner, ReferentialAction, Statement, TableConstraint, TimezoneInfo,
    UserDefinedTypeRepresentation, Value,
};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct PgDatabase {
    /// Keyed "schema.table".
    pub tables: BTreeMap<String, PgTable>,
    /// Enum type name ("schema.name") → labels.
    pub enums: BTreeMap<String, Vec<String>>,
    pub policies: Vec<PgPolicy>,
    /// Publication name → sorted table names.
    pub publications: BTreeMap<String, Vec<String>>,
    pub functions: Vec<PgRawObject>,
    pub triggers: Vec<PgRawObject>,
    /// Statements neither parsed nor recognized (candidate gap items).
    pub unparsed: Vec<(String, usize)>,
}

#[derive(Debug, Default)]
pub struct PgTable {
    pub schema: String,
    pub name: String,
    pub columns: Vec<PgColumn>,
    pub pk: Vec<String>,
    pub fks: Vec<PgFk>,
    pub rls_enabled: bool,
    pub line: usize,
}

#[derive(Debug)]
pub struct PgColumn {
    pub name: String,
    /// Normalized lowercase type name, e.g. "text", "timestamptz", "public.customer_status".
    pub pg_type: String,
    pub not_null: bool,
    pub unique: bool,
    pub indexed: bool,
    /// `CHECK (col IN ('a','b'))` values, column-level (enum-by-check).
    pub check_in_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FkAction {
    Cascade,
    SetNull,
    Restrict,
}

#[derive(Debug)]
pub struct PgFk {
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: FkAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyCommand {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Debug)]
pub struct PgPolicy {
    pub table: String,
    pub name: String,
    pub command: PolicyCommand,
    /// The `TO role, …` clause (lowercased), e.g. ["authenticated"].
    pub to_roles: Vec<String>,
    pub using: Option<ast::Expr>,
    pub with_check: Option<ast::Expr>,
    pub original: String,
    pub line: usize,
}

#[derive(Debug)]
pub struct PgRawObject {
    pub name: String,
    pub sql: String,
    pub line: usize,
}

/// "schema.table" (default schema "public" when unqualified), lowercased.
fn object_name(name: &ObjectName) -> String {
    let parts: Vec<String> = name
        .0
        .iter()
        .filter_map(|p| p.as_ident().map(|i| i.value.to_lowercase()))
        .collect();
    match parts.len() {
        0 => String::new(),
        1 => format!("public.{}", parts[0]),
        _ => parts.join("."),
    }
}

/// A user-defined type name as written (no schema defaulting) so extension
/// scalars like `citext` stay bare while enums stay `schema.name`.
fn custom_type_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .filter_map(|p| p.as_ident().map(|i| i.value.to_lowercase()))
        .collect::<Vec<_>>()
        .join(".")
}

fn ident_of_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(i) => Some(i.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| i.value.to_lowercase()),
        Expr::Nested(inner) => ident_of_expr(inner),
        _ => None,
    }
}

fn ref_action(a: Option<ReferentialAction>) -> FkAction {
    match a {
        Some(ReferentialAction::Cascade) => FkAction::Cascade,
        Some(ReferentialAction::SetNull) => FkAction::SetNull,
        _ => FkAction::Restrict,
    }
}

/// Normalized Postgres type name for the type map.
pub fn data_type_name(dt: &DataType) -> String {
    use DataType as D;
    match dt {
        D::Text | D::TinyText | D::MediumText | D::LongText => "text".into(),
        D::Char(_) | D::Character(_) => "char".into(),
        D::Varchar(_) | D::CharVarying(_) | D::CharacterVarying(_) | D::Nvarchar(_) => {
            "varchar".into()
        }
        D::Uuid => "uuid".into(),
        D::SmallInt(_) | D::Int2(_) | D::TinyInt(_) => "smallint".into(),
        D::Int(_) | D::Integer(_) | D::Int4(_) | D::MediumInt(_) => "integer".into(),
        D::BigInt(_) | D::Int8(_) => "bigint".into(),
        D::Numeric(_) | D::Decimal(_) | D::Dec(_) => "numeric".into(),
        D::Real | D::Float4 | D::Float(_) => "real".into(),
        D::Double(_) | D::DoublePrecision | D::Float8 => "double precision".into(),
        D::Bool | D::Boolean => "boolean".into(),
        D::Date | D::Date32 => "date".into(),
        D::Timestamp(_, tz) => match tz {
            TimezoneInfo::Tz | TimezoneInfo::WithTimeZone => "timestamptz".into(),
            _ => "timestamp".into(),
        },
        D::TimestampNtz(_) => "timestamp".into(),
        D::Time(..) => "time".into(),
        D::JSON => "json".into(),
        D::JSONB => "jsonb".into(),
        D::Bytea => "bytea".into(),
        D::Array(inner) => {
            let elem = match inner {
                ArrayElemTypeDef::AngleBracket(b)
                | ArrayElemTypeDef::SquareBracket(b, _)
                | ArrayElemTypeDef::Parenthesis(b) => data_type_name(b),
                ArrayElemTypeDef::None => "element".into(),
            };
            format!("{elem}[]")
        }
        D::Custom(name, _) => custom_type_name(name),
        other => other.to_string().to_lowercase(),
    }
}

/// `CHECK (col IN ('a','b'))` → Some(["a","b"]) when the checked column is `col`.
fn extract_in_values(expr: &Expr, col: &str) -> Option<Vec<String>> {
    match expr {
        Expr::Nested(inner) => extract_in_values(inner, col),
        Expr::InList {
            expr: lhs,
            list,
            negated: false,
        } => {
            if ident_of_expr(lhs).as_deref() != Some(col) {
                return None;
            }
            let mut values = Vec::new();
            for item in list {
                match item {
                    Expr::Value(v) => match &v.value {
                        Value::SingleQuotedString(s) => values.push(s.clone()),
                        _ => return None,
                    },
                    _ => return None,
                }
            }
            (!values.is_empty()).then_some(values)
        }
        _ => None,
    }
}

impl PgDatabase {
    pub fn fold(stmts: &[RawStatement]) -> Self {
        let mut db = Self::default();
        for raw in stmts {
            match raw {
                RawStatement::Parsed { stmt, sql, line } => db.fold_stmt(stmt, sql, *line),
                RawStatement::Unparsed { sql, line } => {
                    if !db.try_publication(sql) {
                        db.unparsed.push((sql.clone(), *line));
                    }
                }
            }
        }
        db
    }

    fn fold_stmt(&mut self, stmt: &Statement, sql: &str, line: usize) {
        match stmt {
            Statement::CreateTable(ct) => self.fold_create_table(ct, line),
            Statement::CreateType {
                name,
                representation: Some(UserDefinedTypeRepresentation::Enum { labels }),
            } => {
                self.enums.insert(
                    object_name(name),
                    labels.iter().map(|i| i.value.clone()).collect(),
                );
            }
            Statement::CreateIndex(ci) => {
                let table = object_name(&ci.table_name);
                if ci.columns.len() == 1
                    && let Some(col) = ident_of_expr(&ci.columns[0].column.expr)
                    && let Some(t) = self.tables.get_mut(&table)
                    && let Some(column) = t.columns.iter_mut().find(|c| c.name == col)
                {
                    column.indexed = true;
                    if ci.unique {
                        column.unique = true;
                    }
                }
            }
            Statement::AlterTable(at) => {
                let table = object_name(&at.name);
                for op in &at.operations {
                    match op {
                        AlterTableOperation::AddConstraint { constraint, .. } => {
                            if let Some(t) = self.tables.get_mut(&table) {
                                apply_table_constraint(t, constraint);
                            }
                        }
                        AlterTableOperation::EnableRowLevelSecurity => {
                            if let Some(t) = self.tables.get_mut(&table) {
                                t.rls_enabled = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Statement::CreatePolicy(p) => {
                let command = match p.command {
                    Some(CreatePolicyCommand::Select) => PolicyCommand::Select,
                    Some(CreatePolicyCommand::Insert) => PolicyCommand::Insert,
                    Some(CreatePolicyCommand::Update) => PolicyCommand::Update,
                    Some(CreatePolicyCommand::Delete) => PolicyCommand::Delete,
                    _ => PolicyCommand::All,
                };
                let to_roles = p
                    .to
                    .as_ref()
                    .map(|owners| {
                        owners
                            .iter()
                            .map(|o| match o {
                                Owner::Ident(i) => i.value.to_lowercase(),
                                Owner::CurrentRole => "current_role".into(),
                                Owner::CurrentUser => "current_user".into(),
                                Owner::SessionUser => "session_user".into(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.policies.push(PgPolicy {
                    table: object_name(&p.table_name),
                    name: p.name.value.clone(),
                    command,
                    to_roles,
                    using: p.using.clone(),
                    with_check: p.with_check.clone(),
                    original: sql.to_string(),
                    line,
                });
            }
            Statement::CreateFunction(cf) => self.functions.push(PgRawObject {
                name: object_name(&cf.name),
                sql: sql.to_string(),
                line,
            }),
            Statement::CreateTrigger(ct) => self.triggers.push(PgRawObject {
                name: object_name(&ct.name),
                sql: sql.to_string(),
                line,
            }),
            _ => {}
        }
    }

    fn fold_create_table(&mut self, ct: &ast::CreateTable, line: usize) {
        let key = object_name(&ct.name);
        let (schema, name) = key.split_once('.').unwrap_or(("public", key.as_str()));
        let mut table = PgTable {
            schema: schema.to_string(),
            name: name.to_string(),
            line,
            ..Default::default()
        };
        for col in &ct.columns {
            let col_name = col.name.value.to_lowercase();
            let mut pg_col = PgColumn {
                name: col_name.clone(),
                pg_type: data_type_name(&col.data_type),
                not_null: false,
                unique: false,
                indexed: false,
                check_in_values: None,
            };
            for opt in &col.options {
                match &opt.option {
                    ColumnOption::NotNull => pg_col.not_null = true,
                    ColumnOption::Unique(_) => pg_col.unique = true,
                    ColumnOption::PrimaryKey(_) => {
                        pg_col.not_null = true;
                        pg_col.unique = true;
                        table.pk.push(col_name.clone());
                    }
                    ColumnOption::ForeignKey(fk) => {
                        table.fks.push(PgFk {
                            columns: vec![col_name.clone()],
                            ref_table: object_name(&fk.foreign_table),
                            ref_columns: fk
                                .referred_columns
                                .iter()
                                .map(|i| i.value.to_lowercase())
                                .collect(),
                            on_delete: ref_action(fk.on_delete),
                        });
                    }
                    ColumnOption::Check(c) => {
                        if let Some(values) = extract_in_values(&c.expr, &col_name) {
                            pg_col.check_in_values = Some(values);
                        }
                    }
                    _ => {}
                }
            }
            table.columns.push(pg_col);
        }
        for constraint in &ct.constraints {
            apply_table_constraint(&mut table, constraint);
        }
        self.tables.insert(key, table);
    }

    /// The two-form publication recognizer (case-insensitive, whitespace-tolerant):
    /// `CREATE PUBLICATION <name> FOR TABLE <t>[, <t>…]` and
    /// `ALTER PUBLICATION <name> ADD TABLE <t>[, <t>…]`.
    fn try_publication(&mut self, sql: &str) -> bool {
        let words: Vec<String> = sql.split_whitespace().map(|w| w.to_lowercase()).collect();
        let lower: Vec<&str> = words.iter().map(String::as_str).collect();
        let is_create = lower.first() == Some(&"create") && lower.get(1) == Some(&"publication");
        let is_alter = lower.first() == Some(&"alter") && lower.get(1) == Some(&"publication");
        if !is_create && !is_alter {
            return false;
        }
        let Some(name_raw) = words.get(2) else {
            return false;
        };
        let name = name_raw.trim_matches('"').to_string();
        // Find "TABLE" after FOR (create) or ADD (alter); collect the rest.
        let table_kw = lower.iter().position(|w| *w == "table");
        let Some(idx) = table_kw else {
            return false;
        };
        // Everything after "table" up to end, split on commas.
        let rest = sql
            .split_whitespace()
            .skip(idx + 1)
            .collect::<Vec<_>>()
            .join(" ");
        let mut tables: Vec<String> = self.publications.get(&name).cloned().unwrap_or_default();
        for raw in rest.split(',') {
            let tok = raw
                .trim()
                .trim_end_matches(';')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('"');
            if tok.is_empty() {
                continue;
            }
            let normalized = if tok.contains('.') {
                tok.to_lowercase()
            } else {
                format!("public.{}", tok.to_lowercase())
            };
            tables.push(normalized);
        }
        tables.sort();
        tables.dedup();
        self.publications.insert(name, tables);
        true
    }
}

fn apply_table_constraint(table: &mut PgTable, constraint: &TableConstraint) {
    match constraint {
        TableConstraint::PrimaryKey(pk) => {
            for c in &pk.columns {
                if let Some(name) = ident_of_expr(&c.column.expr) {
                    if !table.pk.contains(&name) {
                        table.pk.push(name.clone());
                    }
                    if let Some(col) = table.columns.iter_mut().find(|col| col.name == name) {
                        col.not_null = true;
                        col.unique = true;
                    }
                }
            }
        }
        TableConstraint::Unique(u) => {
            if u.columns.len() == 1
                && let Some(name) = ident_of_expr(&u.columns[0].column.expr)
                && let Some(col) = table.columns.iter_mut().find(|col| col.name == name)
            {
                col.unique = true;
            }
        }
        TableConstraint::ForeignKey(fk) => {
            table.fks.push(PgFk {
                columns: fk.columns.iter().map(|i| i.value.to_lowercase()).collect(),
                ref_table: object_name(&fk.foreign_table),
                ref_columns: fk
                    .referred_columns
                    .iter()
                    .map(|i| i.value.to_lowercase())
                    .collect(),
                on_delete: ref_action(fk.on_delete),
            });
        }
        TableConstraint::Check(c) => {
            // Table-level CHECK (col IN (...)) → attach to the referenced column.
            for col in table.columns.iter_mut() {
                if let Some(values) = extract_in_values(&c.expr, &col.name) {
                    col.check_in_values = Some(values);
                    break;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
create type public.customer_status as enum ('lead', 'active', 'churned');

create table public.workspaces (
    id uuid primary key,
    name text not null
);

create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    email text not null unique,
    status public.customer_status not null,
    score numeric,
    created_at timestamptz not null default now()
);

create index customers_score_idx on public.customers (score);

alter table public.customers enable row level security;

create policy "workspace members" on public.customers
    using (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));

create function public.audit() returns trigger as $$ begin return new; end; $$ language plpgsql;

create publication supabase_realtime for table public.customers;
alter publication supabase_realtime add table public.workspaces;
"#;

    fn db() -> PgDatabase {
        PgDatabase::fold(&crate::platform::migrate::parse::split_and_parse(SCHEMA))
    }

    #[test]
    fn tables_columns_fks_uniques_and_rls_fold_from_the_dump() {
        let db = db();
        let c = &db.tables["public.customers"];
        assert!(c.rls_enabled);
        assert_eq!(c.pk, vec!["id"]);
        let ws_fk = c
            .fks
            .iter()
            .find(|f| f.ref_table == "public.workspaces")
            .unwrap();
        assert_eq!(ws_fk.columns, vec!["workspace_id"]);
        assert_eq!(ws_fk.on_delete, FkAction::Cascade);
        let email = c.columns.iter().find(|col| col.name == "email").unwrap();
        assert!(email.not_null && email.unique);
        let score = c.columns.iter().find(|col| col.name == "score").unwrap();
        assert!(score.indexed, "CREATE INDEX marks the column");
        assert_eq!(
            db.enums["public.customer_status"],
            vec!["lead", "active", "churned"]
        );
    }

    #[test]
    fn policies_functions_and_publications_are_collected() {
        let db = db();
        assert_eq!(db.policies.len(), 1);
        assert_eq!(db.policies[0].table, "public.customers");
        assert!(db.policies[0].using.is_some());
        assert_eq!(db.functions.len(), 1, "raw function captured for the gap report");
        assert_eq!(
            db.publications["supabase_realtime"],
            vec!["public.customers", "public.workspaces"],
            "CREATE + ALTER PUBLICATION both recognized (from Unparsed statements)"
        );
    }
}
