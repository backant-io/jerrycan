//! The schema.json DB contract: apply a design's module migrations to a
//! throwaway in-memory SQLite, introspect the resulting tables via PRAGMAs, and
//! overlay the design's declared field types (so a `json` column reads back as
//! `json`, not the `TEXT` SQLite stores it in). The contract is the durable,
//! reviewable shape of the data layer — derived, never hand-written.

use super::design::{Design, Entity, FieldType, ModuleDesign};
use super::mounting;
use crate::db::Db;
use crate::db::sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Schema-contract format version. Bumped when the shape below changes.
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct SchemaContract {
    pub schema_version: u32,
    pub tables: Vec<Table>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub module: String,
    pub columns: Vec<Column>,
    pub foreign_keys: Vec<ForeignKeyRef>,
    pub unique: Vec<Vec<String>>,
    pub indexes: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub enums: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub r#type: String,
    pub nullable: bool,
    pub pk: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ForeignKeyRef {
    pub column: String,
    pub references: TableColumn,
    pub on_delete: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TableColumn {
    pub table: String,
    pub column: String,
}

/// SQL table name for an entity — mirrors genroute's `table_name`
/// (lowercased + pluralized: `Lead` → `leads`, `ApiKey` → `apikeys`).
fn table_name(entity: &str) -> String {
    format!("{}s", entity.to_lowercase())
}

/// Walk a module + its subroutes, calling `f` for every (top_module, entity).
/// Subroute entities attribute to the TOP-LEVEL module name because that crate
/// holds the migration file on disk (`crates/routes/{top}/migrations/...`).
fn for_each_entity<'a>(m: &'a ModuleDesign, top: &'a str, f: &mut impl FnMut(&'a str, &'a Entity)) {
    for e in &m.entities {
        f(top, e);
    }
    for sub in &m.subroutes {
        for_each_entity(sub, top, f);
    }
}

/// Index of every entity table → (owning top-level module, the Entity), plus the
/// membership table → tenant module. Drives module attribution and type overlay.
struct DesignIndex<'a> {
    /// table name → (module name, entity)
    entities: BTreeMap<String, (&'a str, &'a Entity)>,
    /// membership table name → tenant module name (if tenancy declared)
    membership: Option<(String, String)>,
    /// the fk key Rust type for the tenant ("String" → string column, else integer)
    tenant_key_string: bool,
}

impl<'a> DesignIndex<'a> {
    fn build(design: &'a Design) -> Self {
        let mut entities = BTreeMap::new();
        for m in &design.modules {
            for_each_entity(m, &m.name, &mut |top, e| {
                entities.insert(table_name(&e.name), (top, e));
            });
        }
        let mut membership = None;
        let mut tenant_key_string = false;
        if let Some(tenancy) = &design.tenancy {
            let members = format!("{}_members", Design::to_snake(&tenancy.entity));
            // The membership table lives in whichever module declares the tenant entity.
            let module = design
                .modules
                .iter()
                .find(|m| m.entities.iter().any(|e| e.name == tenancy.entity))
                .map(|m| m.name.clone())
                .unwrap_or_default();
            membership = Some((members, module));
            tenant_key_string = design.target_key_rust_type(&tenancy.entity) == "String";
        }
        Self {
            entities,
            membership,
            tenant_key_string,
        }
    }
}

/// The design-declared type string for a column, or `None` to fall back to the
/// SQLite-declared type. `entity` is the table's owning entity (None for the
/// membership table, handled by the caller).
fn overlay_type(index: &DesignIndex, table: &str, column: &str) -> Option<String> {
    // Membership table: id integer, user_id integer, role string, fk per tenant key.
    if let Some((members, _)) = &index.membership {
        if members == table {
            return Some(match column {
                "id" | "user_id" => "integer".to_string(),
                "role" => "string".to_string(),
                _ => {
                    // the tenant fk column
                    if index.tenant_key_string {
                        "string".to_string()
                    } else {
                        "integer".to_string()
                    }
                }
            });
        }
    }
    let (_, entity) = index.entities.get(table)?;
    // A declared field carries its own FieldType.
    if let Some(field) = entity.fields.iter().find(|f| f.name == column) {
        return Some(field_type_name(field.field_type));
    }
    // fk columns: not declared as fields — type follows the target key.
    for b in &entity.belongs_to {
        if Design::fk_column(&b.entity) == column {
            return Some(
                if entity_owner_key_is_string(index, &b.entity) {
                    "string"
                } else {
                    "integer"
                }
                .to_string(),
            );
        }
    }
    None
}

/// Whether a belongs_to target keys on a String pk (so its fk column is a string).
fn entity_owner_key_is_string(index: &DesignIndex, target: &str) -> bool {
    index
        .entities
        .get(&table_name(target))
        .and_then(|(_, e)| e.fields.iter().find(|f| f.name == "id"))
        .map(|f| f.field_type == FieldType::String)
        .unwrap_or(false)
}

/// The serde name of a FieldType (the same token the design.json uses).
fn field_type_name(t: FieldType) -> String {
    match t {
        FieldType::String => "string",
        FieldType::Integer => "integer",
        FieldType::Float => "float",
        FieldType::Boolean => "boolean",
        FieldType::Datetime => "datetime",
        FieldType::Uuid => "uuid",
        FieldType::Json => "json",
    }
    .to_string()
}

/// Fallback type mapping from a SQLite-declared column type when the design
/// doesn't own the column (e.g. a table not described in the design).
fn fallback_type(sqlite_decl: &str) -> String {
    let up = sqlite_decl.to_ascii_uppercase();
    if up.contains("INT") {
        "integer"
    } else if up.contains("REAL") || up.contains("DOUBLE") || up.contains("FLOAT") {
        "float"
    } else if up.contains("BOOL") {
        "boolean"
    } else {
        "string"
    }
    .to_string()
}

/// Normalize SQLite's on_delete spelling to snake_case contract tokens.
fn normalize_on_delete(raw: &str) -> String {
    match raw.to_ascii_uppercase().as_str() {
        "CASCADE" => "cascade",
        "SET NULL" => "set_null",
        "RESTRICT" => "restrict",
        _ => "no_action",
    }
    .to_string()
}

/// Pull a PRAGMA integer column that SQLite/sqlx may type as i32 or i64.
fn pragma_int(row: &crate::db::sea_orm::QueryResult, col: &str) -> Result<i64, String> {
    row.try_get::<i64>("", col)
        .or_else(|_| row.try_get::<i32>("", col).map(i64::from))
        .map_err(|e| format!("pragma column `{col}`: {e}"))
}

/// Run a SQLite query and return the rows.
async fn query(db: &Db, sql: &str) -> Result<Vec<crate::db::sea_orm::QueryResult>, String> {
    db.conn()
        .query_all(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .map_err(|e| format!("query `{sql}`: {e}"))
}

/// Apply the design's migrations to a throwaway in-memory SQLite and introspect
/// the resulting schema into a deterministic [`SchemaContract`].
pub async fn derive_schema(root: &Path, design: &Design) -> Result<SchemaContract, String> {
    // 1. Collect module migrations in the same module-then-stem order the
    //    generated migrations.rs / `jerrycan db migrate` use.
    let migrations = mounting::collect_migrations(root)?;

    // 2. Apply them to a throwaway in-memory SQLite.
    let db = Db::connect("sqlite::memory:")
        .await
        .map_err(|e| format!("connect sqlite::memory:: {}", e.message()))?;
    db.migrate_owned(&migrations)
        .await
        .map_err(|e| format!("apply migrations: {}", e.message()))?;

    let index = DesignIndex::build(design);

    // 3. Introspect every user table.
    let table_rows = query(
        &db,
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    )
    .await?;
    let mut table_names: Vec<String> = Vec::new();
    for row in &table_rows {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| format!("sqlite_master name: {e}"))?;
        if name == "_jerrycan_migrations" || name.starts_with("sqlite_") {
            continue;
        }
        table_names.push(name);
    }
    table_names.sort();

    let mut tables = Vec::new();
    for table in &table_names {
        tables.push(introspect_table(&db, &index, table).await?);
    }

    // Deterministic order: tables already sorted by name above.
    Ok(SchemaContract {
        schema_version: SCHEMA_VERSION,
        tables,
    })
}

/// Introspect one table into a [`Table`], overlaying design types and module.
async fn introspect_table(db: &Db, index: &DesignIndex<'_>, table: &str) -> Result<Table, String> {
    // Columns + pk via table_info (kept in cid order).
    let info = query(db, &format!("PRAGMA table_info(\"{table}\")")).await?;
    let mut columns = Vec::new();
    for row in &info {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| format!("table_info name: {e}"))?;
        let decl: String = row.try_get("", "type").unwrap_or_default();
        let notnull = pragma_int(row, "notnull")? != 0;
        let pk = pragma_int(row, "pk")? > 0;
        let r#type = overlay_type(index, table, &name).unwrap_or_else(|| fallback_type(&decl));
        columns.push(Column {
            name,
            r#type,
            nullable: !notnull,
            pk,
        });
    }

    // Foreign keys via foreign_key_list.
    let fk_rows = query(db, &format!("PRAGMA foreign_key_list(\"{table}\")")).await?;
    let mut foreign_keys = Vec::new();
    for row in &fk_rows {
        let column: String = row
            .try_get("", "from")
            .map_err(|e| format!("foreign_key_list from: {e}"))?;
        let ref_table: String = row
            .try_get("", "table")
            .map_err(|e| format!("foreign_key_list table: {e}"))?;
        let ref_column: String = row.try_get("", "to").unwrap_or_else(|_| "id".to_string());
        let on_delete: String = row.try_get("", "on_delete").unwrap_or_default();
        foreign_keys.push(ForeignKeyRef {
            column,
            references: TableColumn {
                table: ref_table,
                column: ref_column,
            },
            on_delete: normalize_on_delete(&on_delete),
        });
    }
    foreign_keys.sort_by(|a, b| a.column.cmp(&b.column));

    // Indexes + unique constraints via index_list / index_info.
    let mut unique: Vec<Vec<String>> = Vec::new();
    let mut indexes: Vec<String> = Vec::new();
    let idx_rows = query(db, &format!("PRAGMA index_list(\"{table}\")")).await?;
    for row in &idx_rows {
        let idx_name: String = row
            .try_get("", "name")
            .map_err(|e| format!("index_list name: {e}"))?;
        let is_unique = pragma_int(row, "unique")? != 0;
        let origin: String = row.try_get("", "origin").unwrap_or_default();
        // Skip implicit pk index (origin 'pk').
        if origin == "pk" {
            continue;
        }
        if is_unique {
            // Unique constraint or unique index: record the column set.
            let cols = index_columns(db, &idx_name).await?;
            unique.push(cols);
        } else {
            // A plain created index (origin 'c', non-unique): record by name.
            indexes.push(idx_name);
        }
    }
    unique.sort();
    indexes.sort();

    // Module attribution: entity table → owning top module; membership → tenant module.
    let module = if let Some((members, tenant_module)) = &index.membership {
        if members == table {
            tenant_module.clone()
        } else {
            index
                .entities
                .get(table)
                .map(|(m, _)| (*m).to_string())
                .unwrap_or_default()
        }
    } else {
        index
            .entities
            .get(table)
            .map(|(m, _)| (*m).to_string())
            .unwrap_or_default()
    };

    // Enum CHECK values from the design (declared `values`), keyed by column.
    let mut enums = BTreeMap::new();
    if let Some((_, entity)) = index.entities.get(table) {
        for f in &entity.fields {
            if let Some(values) = &f.values {
                enums.insert(f.name.clone(), values.clone());
            }
        }
    }

    Ok(Table {
        name: table.to_string(),
        module,
        columns,
        foreign_keys,
        unique,
        indexes,
        enums,
    })
}

/// The column names backing an index, in index_info seqno order.
async fn index_columns(db: &Db, idx_name: &str) -> Result<Vec<String>, String> {
    let rows = query(db, &format!("PRAGMA index_info(\"{idx_name}\")")).await?;
    let mut cols = Vec::new();
    for row in &rows {
        let name: String = row.try_get("", "name").unwrap_or_default();
        if !name.is_empty() {
            cols.push(name);
        }
    }
    Ok(cols)
}

/// Render a contract as pretty JSON with a trailing newline (on-disk form).
pub fn render(contract: &SchemaContract) -> String {
    let mut s = serde_json::to_string_pretty(contract).expect("contract serializes");
    s.push('\n');
    s
}

/// Derive and write `schema.json` into an app root (db mode only — the contract
/// is derived from migrations, which only exist when the design wants a db).
/// Returns the written relative path (`schema.json`), or `None` for memory mode.
/// Runs the async derivation on a throwaway runtime, as `jerrycan db migrate` does.
pub fn write_schema(root: &Path, design: &Design) -> Result<Option<String>, String> {
    if !design.wants_db() {
        return Ok(None);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let contract = runtime.block_on(derive_schema(root, design))?;
    std::fs::write(root.join("schema.json"), render(&contract))
        .map_err(|e| format!("write schema.json: {e}"))?;
    Ok(Some("schema.json".to_string()))
}

/// Compare the committed `schema.json` against a fresh derivation. An empty
/// Vec means the contract is in sync; a single JC0520 diagnostic means the file
/// is missing or stale (drifted from the module migrations). Err means the
/// derivation itself failed (an environment/migration problem, not drift).
pub async fn verify_fresh(
    root: &Path,
    design: &Design,
) -> Result<Vec<super::checkpipe::Diagnostic>, String> {
    let derived = render(&derive_schema(root, design).await?);
    let committed = std::fs::read_to_string(root.join("schema.json")).unwrap_or_default();
    if committed == derived {
        return Ok(Vec::new());
    }
    Ok(vec![super::checkpipe::Diagnostic {
        code: "JC0520".into(),
        file: Some("schema.json".into()),
        line: Some(1),
        message: "schema.json does not match the schema derived from the module migrations".into(),
        suggestion: Some("run jerrycan schema --write".into()),
        doc_url: Some("jerrycan docs database".into()),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn schema_contract_reflects_migrations_and_design_types() {
        let s = include_str!("../../../../conformance/designs/kolli-slice.design.json");
        let d: Design = serde_json::from_str(s).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        super::super::scaffold::scaffold(&root, &d).unwrap();
        let contract = derive_schema(&root, &d).await.unwrap();
        let leads = contract.tables.iter().find(|t| t.name == "leads").unwrap();
        assert_eq!(leads.module, "leads");
        let phone = leads.columns.iter().find(|c| c.name == "phone").unwrap();
        assert_eq!(phone.r#type, "string"); // design overlay, not sqlite decl
        assert!(!phone.nullable);
        let custom = leads.columns.iter().find(|c| c.name == "custom").unwrap();
        assert_eq!(custom.r#type, "json");
        assert!(custom.nullable);
        assert!(leads.foreign_keys.iter().any(|f| f.column == "workspace_id"
            && f.references.table == "workspaces"
            && f.on_delete == "cascade"));
        assert!(leads.unique.iter().any(|u| u == &vec!["phone".to_string()]));
        assert!(leads.indexes.iter().any(|i| i.contains("phone")));
        let members = contract
            .tables
            .iter()
            .find(|t| t.name == "workspace_members")
            .unwrap();
        assert_eq!(members.module, "workspaces");
        assert!(
            members
                .foreign_keys
                .iter()
                .any(|f| f.on_delete == "cascade")
        );
        // determinism: tables sorted by name, stable across runs
        let names: Vec<_> = contract.tables.iter().map(|t| t.name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn published_schema_pins_the_contract_shape() {
        // The published JSON Schema is the durable contract; spot-check that it
        // parses and pins the version this code emits (no full validator here).
        let s = include_str!("../../../../docs/contracts/db-schema.json");
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(v["$id"], "https://jerrycan.cc/schemas/db-schema-v1.json");
        assert_eq!(
            v["properties"]["schema_version"]["const"]
                .as_u64()
                .expect("schema_version const"),
            u64::from(SCHEMA_VERSION),
        );
        // The contract's required keys mirror the SchemaContract fields.
        let required: Vec<&str> = v["required"]
            .as_array()
            .expect("required")
            .iter()
            .filter_map(|x| x.as_str())
            .collect();
        assert_eq!(required, ["schema_version", "tables"]);
    }
}
