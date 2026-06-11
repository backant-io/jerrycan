//! Per-module code generation: route crates, fractal subroutes, handler stubs.
//! Ownership rule: Cargo.toml/lib.rs/subroutes mod.rs files are TOOL-owned
//! (always rewritten); handlers/model/repo/deps are AGENT-owned (create-once).

use super::design::*;
use super::templates::{ROUTE_CARGO, render};
use std::fs;
use std::path::Path;

/// Generation mode derived from the design's reserved dependencies.
#[derive(Debug, Clone, Copy, Default)]
pub struct GenMode {
    pub db: bool,
    pub auth: bool,
}

/// snake/ident helpers: crate `route-todo-list` → ident `route_todo_list`.
pub fn crate_ident(module_name: &str) -> String {
    format!("route_{}", module_name.replace('-', "_"))
}

/// The entity's declared `id` field, if the design provides one — it becomes
/// the table's primary key (no synthetic pk is added alongside it).
fn declared_id(e: &Entity) -> Option<FieldType> {
    e.fields
        .iter()
        .find(|f| f.name == "id")
        .map(|f| f.field_type)
}

/// The Rust type repos and `/{id}` handlers key on: the declared id field's
/// rust_type (String for text pks), i64 for integer or synthetic ids.
fn key_rust_type(e: &Entity) -> &'static str {
    match declared_id(e) {
        Some(t) => t.rust_type(),
        None => "i64",
    }
}

fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .map(|rb| rb.entity.as_str())
        .or(ep.success.entity.as_deref())
        .or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

fn return_type(ep: &Endpoint) -> String {
    let entity = ep.success.entity.as_deref();
    match (ep.success.status, entity, ep.success.list) {
        (204, _, _) => "Result<NoContent>".to_string(),
        (201, Some(e), _) => format!("Result<Created<{e}>>"),
        (201, None, _) => "Result<Created<serde_json::Value>>".to_string(),
        (_, Some(e), true) => format!("Result<Json<Vec<{e}>>>"),
        (_, Some(e), false) => format!("Result<Json<{e}>>"),
        (_, None, _) => "Result<Json<serde_json::Value>>".to_string(),
    }
}

fn path_params(ep: &Endpoint) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = ep.path.as_str();
    while let Some(start) = rest.find('{') {
        let Some(end_rel) = rest[start..].find('}') else {
            break;
        };
        out.push(rest[start + 1..start + end_rel].to_string());
        rest = &rest[start + end_rel + 1..];
    }
    out
}

fn handler_params(m: &ModuleDesign, ep: &Endpoint, mode: GenMode) -> String {
    let mut params = Vec::new();
    if let Some(e) = endpoint_repo_entity(m, ep) {
        params.push(format!("_repo: Dep<{e}Repo>"));
    }
    // Guard param (order: repo, user, path, body): an authenticated session.
    if mode.auth && ep.is_guarded() {
        params.push("_user: CurrentUser".to_string());
    }
    let params_in_path = path_params(ep);
    // A param named `id` keys the endpoint's entity, so it takes that entity's
    // key type (String for text pks); other params stay i64.
    let key = endpoint_repo_entity(m, ep)
        .and_then(|name| m.entities.iter().find(|e| e.name == name))
        .map(key_rust_type)
        .unwrap_or("i64");
    let param_type = |p: &str| if p == "id" { key } else { "i64" };
    match params_in_path.len() {
        0 => {}
        1 => params.push(format!(
            "Path(_{p}): Path<{ty}>",
            p = params_in_path[0],
            ty = param_type(&params_in_path[0])
        )),
        _ => {
            let names: Vec<String> = params_in_path.iter().map(|p| format!("_{p}")).collect();
            let types = params_in_path
                .iter()
                .map(|p| param_type(p))
                .collect::<Vec<_>>()
                .join(", ");
            params.push(format!("Path(({})): Path<({})>", names.join(", "), types));
        }
    }
    if let Some(ref rb) = ep.request_body {
        params.push(format!("Json(_body): Json<{}>", rb.entity));
    }
    params.join(", ")
}

/// A leading comment for role-guarded endpoints, reminding the agent to add the
/// `require_role` import and call it before proceeding (empty for unguarded /
/// no-role endpoints). The stub itself imports only `CurrentUser` (the param
/// type it uses); the agent adds `require_role` when implementing the guard.
fn guard_comment(ep: &Endpoint) -> String {
    if ep.required_roles.is_empty() {
        String::new()
    } else {
        let roles = ep.required_roles.join("\", \"");
        format!(
            "    // guard: requires role \"{roles}\" — add `use jerrycan::auth::require_role;` and call require_role(&_user.0.role, \"{roles}\")? before proceeding\n"
        )
    }
}

pub(crate) fn handlers_rs(m: &ModuleDesign, mode: GenMode) -> String {
    let mut uses = String::from("use jerrycan::prelude::*;\n");
    let mentions_entities = m
        .endpoints
        .iter()
        .any(|ep| ep.request_body.is_some() || ep.success.entity.is_some());
    if mentions_entities {
        uses.push_str("use super::model::*;\n");
    }
    if !m.entities.is_empty() {
        uses.push_str("use super::repo::*;\n");
    }
    // Auth mode: a guarded endpoint takes a `_user: CurrentUser` param, so the
    // stub imports ONLY that alias (which it uses). It does NOT import
    // `require_role` — a raw stub doesn't call it, so the import would be unused
    // and fail `-D warnings`; the agent adds the import (see guard_comment) when
    // implementing the role check.
    if mode.auth && m.endpoints.iter().any(|ep| ep.is_guarded()) {
        uses.push_str("use shared::CurrentUser;\n");
    }
    let mut out = format!(
        "//! Handlers for `{}` — thin: extract → call → respond.\n//! Generated stubs return 500 until implemented.\n{uses}\n",
        m.name
    );
    for ep in &m.endpoints {
        let guard = if mode.auth {
            guard_comment(ep)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "pub(crate) async fn {op}({params}) -> {ret} {{\n{guard}    Err(Error::internal(\"{op} not implemented — replace this stub\"))\n}}\n\n",
            op = ep.operation_id,
            params = handler_params(m, ep, mode),
            ret = return_type(ep),
        ));
    }
    out
}

pub(crate) fn model_rs(m: &ModuleDesign) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::from(
        "//! Entities and DTOs for this module.\nuse serde::{Deserialize, Serialize};\n\n",
    );
    for e in &m.entities {
        out.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct ");
        out.push_str(&e.name);
        out.push_str(" {\n");
        for f in &e.fields {
            if !f.required {
                out.push_str("    #[serde(default)]\n");
            }
            out.push_str(&format!(
                "    pub {}: {},\n",
                f.name,
                f.field_type.rust_type()
            ));
        }
        out.push_str("}\n\n");
    }
    Some(out)
}

fn memory_repo_rs(m: &ModuleDesign) -> String {
    let mut out = String::from(
        "//! In-memory data access (Phase 1; jerrycan-db replaces this in Phase 2).\nuse super::model::*;\nuse std::collections::BTreeMap;\nuse std::sync::Mutex;\nuse std::sync::atomic::{AtomicI64, Ordering};\n\n",
    );
    for e in &m.entities {
        let n = &e.name;
        out.push_str(&format!(
            r#"// Stub handlers don't call the repo yet; remove this allow as you implement them.
#[allow(dead_code)]
pub struct {n}Repo {{
    items: Mutex<BTreeMap<i64, {n}>>,
    next_id: AtomicI64,
}}

#[allow(dead_code)]
impl {n}Repo {{
    pub fn new() -> Self {{
        Self {{ items: Mutex::new(BTreeMap::new()), next_id: AtomicI64::new(1) }}
    }}
    pub fn all(&self) -> Vec<{n}> {{
        self.items.lock().unwrap().values().cloned().collect()
    }}
    pub fn get(&self, id: i64) -> Option<{n}> {{
        self.items.lock().unwrap().get(&id).cloned()
    }}
    pub fn insert(&self, item: {n}) -> i64 {{
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.items.lock().unwrap().insert(id, item);
        id
    }}
    pub fn remove(&self, id: i64) -> bool {{
        self.items.lock().unwrap().remove(&id).is_some()
    }}
    pub fn update(&self, id: i64, item: {n}) -> bool {{
        match self.items.lock().unwrap().get_mut(&id) {{
            Some(slot) => {{
                *slot = item;
                true
            }}
            None => false,
        }}
    }}
}}

impl Default for {n}Repo {{
    fn default() -> Self {{
        Self::new()
    }}
}}

"#
        ));
    }
    out
}

/// SQL table name for an entity: lowercased + pluralized (`Todo` → `todos`).
fn table_name(entity: &str) -> String {
    format!("{}s", entity.to_lowercase())
}

/// `Alias::new("col"), …` for the generated repo's column list — sea-query
/// owns identifier quoting per dialect, so no escaped-quote SQL templates.
fn alias_cols(e: &Entity) -> String {
    e.fields
        .iter()
        .map(|f| format!("Alias::new(\"{}\")", f.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_constructor(e: &Entity) -> String {
    let fields = e
        .fields
        .iter()
        .map(|f| {
            // Booleans are stored as integers (see `column_type`): sqlx `Any`
            // can't decode a SQLite `bool`, so read the i64 column and compare.
            if f.field_type == FieldType::Boolean {
                format!("{name}: row.get::<i64, _>(\"{name}\") != 0", name = f.name)
            } else {
                format!("{name}: row.get(\"{name}\")", name = f.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} {{ {fields} }}", e.name)
}

/// One `SimpleExpr` per field for INSERT values, in column order. Booleans
/// ride as i64 (sqlx `Any` binds a Rust `bool` with SQLite's `Bool` type
/// info, which it then can't read back).
fn value_exprs(e: &Entity) -> String {
    e.fields
        .iter()
        .map(|f| {
            if f.field_type == FieldType::Boolean {
                format!("(item.{} as i64).into()", f.name)
            } else {
                format!("item.{}.into()", f.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `(Alias, SimpleExpr)` pairs for UPDATE SET — every field except the pk.
fn update_pairs(e: &Entity) -> String {
    e.fields
        .iter()
        .filter(|f| f.name != "id")
        .map(|f| {
            if f.field_type == FieldType::Boolean {
                format!(
                    "(Alias::new(\"{n}\"), (item.{n} as i64).into())",
                    n = f.name
                )
            } else {
                format!("(Alias::new(\"{n}\"), item.{n}.into())", n = f.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sql_repo(e: &Entity) -> String {
    let entity = &e.name;
    let snake = entity.to_lowercase();
    let table = table_name(entity);
    let n_cols = e.fields.len();
    let cols = alias_cols(e);
    let values = value_exprs(e);
    let set_pairs = update_pairs(e);
    let ctor = row_constructor(e);
    let key = key_rust_type(e);
    format!(
        r#"pub struct {entity}Repo {{
    db: Db,
}}

/// DI factory — registered by the tool-owned lib.rs via `.provide_dep`.
pub(crate) async fn {snake}_repo(db: Dep<Db>) -> Result<{entity}Repo> {{
    Ok({entity}Repo {{ db: (*db).clone() }})
}}

// Stub handlers don't call the repo yet; remove this allow as you implement them.
#[allow(dead_code)]
impl {entity}Repo {{
    fn table() -> Alias {{
        Alias::new("{table}")
    }}

    fn cols() -> [Alias; {n_cols}] {{
        [{cols}]
    }}

    pub async fn all(&self) -> Result<Vec<{entity}>> {{
        let (sql, values) = Query::select()
            .columns(Self::cols())
            .from(Self::table())
            .order_by(Alias::new("id"), Order::Asc)
            .build_any_sqlx(self.db.query_builder());
        let rows = jerrycan::db::sqlx::query_with(&sql, values)
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(|row| {ctor}).collect())
    }}

    pub async fn get(&self, id: {key}) -> Result<Option<{entity}>> {{
        let (sql, values) = Query::select()
            .columns(Self::cols())
            .from(Self::table())
            .and_where(Expr::col(Alias::new("id")).eq(id))
            .build_any_sqlx(self.db.query_builder());
        let row = jerrycan::db::sqlx::query_with(&sql, values)
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(row.map(|row| {ctor}))
    }}

    pub async fn insert(&self, item: {entity}) -> Result<{key}> {{
        // sea-query renders RETURNING for both backends; never last_insert_id
        // (the sqlx `Any` driver returns None for it on sqlite).
        let (sql, values) = Query::insert()
            .into_table(Self::table())
            .columns(Self::cols())
            .values_panic([{values}])
            .returning(Query::returning().columns([Alias::new("id")]))
            .build_any_sqlx(self.db.query_builder());
        let row = jerrycan::db::sqlx::query_with(&sql, values)
            .fetch_one(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(row.get("id"))
    }}

    pub async fn remove(&self, id: {key}) -> Result<bool> {{
        let (sql, values) = Query::delete()
            .from_table(Self::table())
            .and_where(Expr::col(Alias::new("id")).eq(id))
            .build_any_sqlx(self.db.query_builder());
        let result = jerrycan::db::sqlx::query_with(&sql, values)
            .execute(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(result.rows_affected() > 0)
    }}

    pub async fn update(&self, id: {key}, item: {entity}) -> Result<bool> {{
        let (sql, values) = Query::update()
            .table(Self::table())
            .values([{set_pairs}])
            .and_where(Expr::col(Alias::new("id")).eq(id))
            .build_any_sqlx(self.db.query_builder());
        let result = jerrycan::db::sqlx::query_with(&sql, values)
            .execute(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(result.rows_affected() > 0)
    }}
}}

"#,
    )
}

pub(crate) fn repo_rs(m: &ModuleDesign, mode: GenMode) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    if !mode.db {
        return Some(memory_repo_rs(m));
    }
    let mut out = String::from(
        "//! Data access — sea-query builders over jerrycan::db (agent-owned; edit freely).\nuse jerrycan::db::sea_query::{Alias, Expr, Order, Query};\nuse jerrycan::db::sea_query_binder::SqlxBinder;\nuse jerrycan::db::sqlx::Row;\nuse jerrycan::db::{db_error, Db};\nuse jerrycan::prelude::*;\n\nuse super::model::*;\n\n",
    );
    for e in &m.entities {
        out.push_str(&sql_repo(e));
    }
    Some(out)
}

/// Dual-dialect `CREATE TABLE` DDL for one module's entities (None if it has
/// none), rendered by sea-query so dialect differences (autoincrement vs
/// bigserial, quoting) are library-owned, never hand-rolled strings.
fn migration_ddl(m: &ModuleDesign, backend_is_pg: bool) -> Option<String> {
    use sea_query::{Alias, ColumnDef, PostgresQueryBuilder, SqliteQueryBuilder, Table};
    if m.entities.is_empty() {
        return None;
    }
    // Booleans are stored as integers (0/1): the sqlx `Any` driver cannot
    // round-trip a Rust `bool` against SQLite (it rejects the `Bool` type
    // info on read), so the repo binds `bool as i64` and reads `i64 != 0`.
    // big_integer round-trips identically on both backends under `Any`.
    fn typed(c: &mut ColumnDef, t: FieldType) -> &mut ColumnDef {
        match t {
            FieldType::String | FieldType::Datetime | FieldType::Uuid => c.text(),
            FieldType::Integer | FieldType::Boolean => c.big_integer(),
            FieldType::Float => c.double(),
            FieldType::Json => c.text(), // unreachable: validated out in db mode
        }
    }
    let mut out = String::new();
    for e in &m.entities {
        let mut table = Table::create();
        table.table(Alias::new(table_name(&e.name)));
        // A declared `id` field IS the pk (typed as declared); only entities
        // without one get the synthetic autoincrement pk. Emitting both would
        // be a duplicate-column error.
        let mut pk = ColumnDef::new(Alias::new("id"));
        match declared_id(e) {
            Some(t) if t != FieldType::Integer => {
                typed(&mut pk, t).not_null().primary_key();
            }
            _ => {
                pk.big_integer().not_null().auto_increment().primary_key();
            }
        }
        table.col(&mut pk);
        for f in e.fields.iter().filter(|f| f.name != "id") {
            let mut col = ColumnDef::new(Alias::new(f.name.as_str()));
            typed(&mut col, f.field_type).not_null();
            if !f.required {
                match f.field_type {
                    FieldType::Integer | FieldType::Boolean => col.default(0i64),
                    FieldType::Float => col.default(0.0f64),
                    _ => col.default(""),
                };
            }
            table.col(&mut col);
        }
        let sql = if backend_is_pg {
            table.build(PostgresQueryBuilder)
        } else {
            table.build(SqliteQueryBuilder)
        };
        out.push_str(&sql);
        out.push_str(";\n\n");
    }
    Some(out)
}

/// Emit the module's own migration file plus one file per entity-bearing
/// subroute (`0001_create_tables_{sub}.sql`), both dialects, recursing.
/// Migrations are agent-owned create-once — never clobbered once applied.
fn write_module_migrations(
    crate_dir: &Path,
    m: &ModuleDesign,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    if let Some(ddl) = migration_ddl(m, false) {
        write_agent_owned(
            &crate_dir.join("migrations/sqlite/0001_create_tables.sql"),
            &ddl,
            created,
            root,
        )?;
    }
    if let Some(ddl) = migration_ddl(m, true) {
        write_agent_owned(
            &crate_dir.join("migrations/postgres/0001_create_tables.sql"),
            &ddl,
            created,
            root,
        )?;
    }
    write_subtree_migrations(crate_dir, m, created, root)
}

/// Subroute migrations land in the OWNING (top) crate's migrations dir, named
/// `0001_create_tables_{sub_snake}.sql`, recursing to arbitrary depth.
fn write_subtree_migrations(
    crate_dir: &Path,
    m: &ModuleDesign,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    for sub in &m.subroutes {
        let sub_snake = sub.name.replace('-', "_");
        if let Some(ddl) = migration_ddl(sub, false) {
            write_agent_owned(
                &crate_dir.join(format!(
                    "migrations/sqlite/0001_create_tables_{sub_snake}.sql"
                )),
                &ddl,
                created,
                root,
            )?;
        }
        if let Some(ddl) = migration_ddl(sub, true) {
            write_agent_owned(
                &crate_dir.join(format!(
                    "migrations/postgres/0001_create_tables_{sub_snake}.sql"
                )),
                &ddl,
                created,
                root,
            )?;
        }
        write_subtree_migrations(crate_dir, sub, created, root)?;
    }
    Ok(())
}

pub(crate) fn deps_rs(m: &ModuleDesign) -> String {
    let mut out = String::from(
        "//! Agent-owned: module-scoped dependencies and middleware.\nuse jerrycan::prelude::*;\n\n/// Called by the tool-owned lib.rs — register module deps/middleware here;\n/// regeneration never touches this file.\npub(crate) fn configure(module: Module) -> Module {\n",
    );
    for dep in &m.dependencies {
        out.push_str(&format!(
            "    // declared dependency `{dep}`: define a type and .provide/.provide_dep it here\n"
        ));
    }
    out.push_str("    module\n}\n");
    out
}

/// Route-table lines: endpoints grouped by path (first-seen order), first
/// method via the free fn, the rest chained.
fn route_lines(m: &ModuleDesign, indent: &str) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut by_path: std::collections::HashMap<&str, Vec<&Endpoint>> =
        std::collections::HashMap::new();
    for ep in &m.endpoints {
        if !by_path.contains_key(ep.path.as_str()) {
            order.push(&ep.path);
        }
        by_path.entry(&ep.path).or_default().push(ep);
    }
    let mut out = String::new();
    for path in order {
        let eps = &by_path[path];
        let mut chain = format!(
            "{}(handlers::{})",
            eps[0].method.builder_fn(),
            eps[0].operation_id
        );
        for ep in &eps[1..] {
            chain.push_str(&format!(
                ".{}(handlers::{})",
                ep.method.builder_fn(),
                ep.operation_id
            ));
        }
        out.push_str(&format!("{indent}.route(\"{path}\", {chain})\n"));
    }
    out
}

fn module_body(m: &ModuleDesign, indent: &str, mode: GenMode) -> String {
    let mut body = format!("{indent}Module::new(\"{}\")\n", m.name);
    for e in &m.entities {
        if mode.db {
            body.push_str(&format!(
                "{indent}    .provide_dep(repo::{}_repo)\n",
                e.name.to_lowercase()
            ));
        } else {
            body.push_str(&format!(
                "{indent}    .provide(repo::{}Repo::new())\n",
                e.name
            ));
        }
    }
    body.push_str(&route_lines(m, &format!("{indent}    ")));
    for sub in &m.subroutes {
        body.push_str(&format!(
            "{indent}    .mount(\"{}\", subroutes::{}::module())\n",
            sub.effective_mount(),
            sub.name.replace('-', "_"),
        ));
    }
    body
}

fn mod_decls(m: &ModuleDesign) -> String {
    let mut out = String::from("mod deps;\nmod handlers;\n");
    if !m.entities.is_empty() {
        out.push_str("mod model;\nmod repo;\n");
    }
    if !m.subroutes.is_empty() {
        out.push_str("mod subroutes;\n");
    }
    out
}

pub(crate) fn lib_rs(m: &ModuleDesign, mode: GenMode) -> String {
    format!(
        "//! Route module `{name}` — TOOL-OWNED, regenerated by `jerrycan generate`.\n//! The sole public item is `module()`; agent code lives in handlers/model/repo/deps.\n#![forbid(unsafe_code)]\n\n{mods}\nuse jerrycan::prelude::*;\n\n/// Build this module's routes, subroutes, and scoped dependencies.\npub fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        ", mode),
    )
}

fn subroute_mod_rs(m: &ModuleDesign, mode: GenMode) -> String {
    format!(
        "//! Subroute `{name}` — TOOL-OWNED mod.rs; same fractal shape as a module.\n\n{mods}\nuse jerrycan::prelude::*;\n\npub(crate) fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        ", mode),
    )
}

fn write_tool_owned(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    fs::create_dir_all(path.parent().expect("file path has parent")).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    created.push(rel(path, root));
    Ok(())
}

fn write_agent_owned(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    if path.exists() {
        return Ok(()); // never clobber agent work
    }
    write_tool_owned(path, content, created, root)
}

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Write (or refresh) one top-level route crate under `routes_dir`
/// (= `<app>/crates/routes`). Returns paths written, relative to routes_dir's parent's parent.
/// Precondition: the design has passed `questions::validate` — generation assumes
/// validated names and entity references.
pub fn write_module(
    routes_dir: &Path,
    m: &ModuleDesign,
    mode: GenMode,
) -> Result<Vec<String>, String> {
    let root = routes_dir
        .ancestors()
        .nth(2)
        .unwrap_or(routes_dir)
        .to_path_buf();
    let crate_dir = routes_dir.join(&m.name);
    let src = crate_dir.join("src");
    let mut created = Vec::new();

    let cargo = render(ROUTE_CARGO, &[("name", &m.name)])?;
    write_tool_owned(&crate_dir.join("Cargo.toml"), &cargo, &mut created, &root)?;
    write_tool_owned(&src.join("lib.rs"), &lib_rs(m, mode), &mut created, &root)?;
    write_unit_files(&src, m, mode, &mut created, &root)?;
    write_subroutes(&src, m, mode, &mut created, &root)?;
    // db mode: agent-owned create-once migrations for this crate (module + subroutes).
    if mode.db {
        write_module_migrations(&crate_dir, m, &mut created, &root)?;
    }
    Ok(created)
}

/// The agent-owned file set shared by modules and subroutes.
fn write_unit_files(
    dir: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    write_agent_owned(
        &dir.join("handlers.rs"),
        &handlers_rs(m, mode),
        created,
        root,
    )?;
    write_agent_owned(&dir.join("deps.rs"), &deps_rs(m), created, root)?;
    if let Some(model) = model_rs(m) {
        write_agent_owned(&dir.join("model.rs"), &model, created, root)?;
    }
    if let Some(repo) = repo_rs(m, mode) {
        write_agent_owned(&dir.join("repo.rs"), &repo, created, root)?;
    }
    Ok(())
}

fn write_subroutes(
    src: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    if m.subroutes.is_empty() {
        return Ok(());
    }
    let sub_root = src.join("subroutes");
    let mut decls = String::from("//! TOOL-OWNED: subroute declarations.\n");
    for sub in &m.subroutes {
        decls.push_str(&format!("pub(crate) mod {};\n", sub.name.replace('-', "_")));
    }
    write_tool_owned(&sub_root.join("mod.rs"), &decls, created, root)?;
    for sub in &m.subroutes {
        let dir = sub_root.join(sub.name.replace('-', "_"));
        write_tool_owned(
            &dir.join("mod.rs"),
            &subroute_mod_rs(sub, mode),
            created,
            root,
        )?;
        write_unit_files(&dir, sub, mode, created, root)?;
        write_subroutes(&dir, sub, mode, created, root)?; // arbitrary depth
    }
    Ok(())
}

/// `jerrycan generate dep <name> --module <m>`: record in design + remind in deps.rs.
pub fn add_dependency(design: &mut Design, module_path: &str, dep: &str) -> Result<(), String> {
    let m = module_by_path_mut(design, module_path)
        .ok_or_else(|| format!("module `{module_path}` not found in design.json"))?;
    if !m.dependencies.iter().any(|d| d == dep) {
        m.dependencies.push(dep.to_string());
    }
    Ok(())
}

/// One row of the live route tree — the same shape the CLI and MCP both emit.
#[derive(Debug, serde::Serialize)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub module: String,
    pub handler: String,
}

/// Walk the design's module tree into a flat, mount-resolved route table.
pub fn route_map(design: &Design) -> Vec<RouteEntry> {
    fn walk(m: &ModuleDesign, prefix: &str, top: &str, out: &mut Vec<RouteEntry>) {
        let base = format!("{}{}", prefix, m.effective_mount());
        for ep in &m.endpoints {
            out.push(RouteEntry {
                method: format!("{:?}", ep.method),
                path: format!("{}{}", base.trim_end_matches('/'), ep.path),
                module: top.to_string(),
                handler: ep.operation_id.clone(),
            });
        }
        for sub in &m.subroutes {
            walk(sub, &base, top, out);
        }
    }
    let mut out = Vec::new();
    for m in &design.modules {
        walk(m, "", &m.name, &mut out);
    }
    out
}

pub fn module_by_path<'a>(design: &'a Design, path: &str) -> Option<&'a ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter().find(|s| s.name == part)?;
    }
    Some(cur)
}

pub fn module_by_path_mut<'a>(design: &'a mut Design, path: &str) -> Option<&'a mut ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter_mut().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter_mut().find(|s| s.name == part)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::MINIMAL;

    fn todos() -> ModuleDesign {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        d.modules.into_iter().next().unwrap()
    }

    #[test]
    fn handler_signatures_follow_the_mapping_rules() {
        let m = todos();
        let h = handlers_rs(&m, GenMode::default());
        assert!(
            h.contains(
                "pub(crate) async fn list_todos(_repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>>"
            ),
            "{h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn create_todo(_repo: Dep<TodoRepo>, Json(_body): Json<Todo>) -> Result<Created<Todo>>"
            ),
            "{h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn delete_todo(_repo: Dep<TodoRepo>, Path(_id): Path<i64>) -> Result<NoContent>"
            ),
            "{h}"
        );
        assert!(h.contains("not implemented — replace this stub"));
    }

    #[test]
    fn multi_param_endpoints_map_to_path_tuples() {
        let mut m = todos();
        m.endpoints.push(Endpoint {
            operation_id: "move_todo".into(),
            method: HttpMethod::POST,
            path: "/{id}/position/{slot}".into(),
            auth_required: false,
            required_roles: vec![],
            request_body: None,
            success: Success {
                status: 204,
                entity: None,
                list: false,
            },
            errors: vec![],
        });
        let h = handlers_rs(&m, GenMode::default());
        assert!(
            h.contains("pub(crate) async fn move_todo(_repo: Dep<TodoRepo>, Path((_id, _slot)): Path<(i64, i64)>) -> Result<NoContent>"),
            "{h}"
        );
    }

    #[test]
    fn lib_rs_groups_routes_by_path_and_mounts_subroutes() {
        let m = todos();
        let lib = lib_rs(&m, GenMode::default());
        assert!(lib.contains("pub fn module() -> Module"), "{lib}");
        assert!(
            lib.contains(".route(\"/\", get(handlers::list_todos).post(handlers::create_todo))"),
            "{lib}"
        );
        assert!(
            lib.contains(".route(\"/{id}\", delete(handlers::delete_todo))"),
            "{lib}"
        );
        assert!(
            lib.contains(".mount(\"/comments\", subroutes::comments::module())"),
            "{lib}"
        );
        assert!(lib.contains(".provide(repo::TodoRepo::new())"), "{lib}");
        assert!(
            lib.contains("deps::configure("),
            "agent hook must wrap the module: {lib}"
        );
        assert!(lib.contains("#![forbid(unsafe_code)]"));
    }

    #[test]
    fn model_and_repo_are_generated_from_entities() {
        let m = todos();
        let model = model_rs(&m).unwrap();
        assert!(model.contains("pub struct Todo"));
        assert!(model.contains("pub title: String"));
        assert!(
            model.contains("#[serde(default)]\n    pub done: bool"),
            "{model}"
        );
        let repo = repo_rs(&m, GenMode::default()).unwrap();
        assert!(repo.contains("pub struct TodoRepo"));
        assert!(
            repo.contains("#[allow(dead_code)]"),
            "stub-phase repo must pass -D warnings: {repo}"
        );
        for method in [
            "pub fn all(",
            "pub fn get(",
            "pub fn insert(",
            "pub fn remove(",
            "pub fn update(",
        ] {
            assert!(repo.contains(method), "{repo}");
        }
    }

    #[test]
    fn write_module_respects_the_ownership_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let routes = tmp.path().join("crates/routes");
        let m = todos();

        let created = write_module(&routes, &m, GenMode::default()).unwrap();
        assert!(created.iter().any(|p| p.ends_with("todos/src/lib.rs")));
        assert!(
            created
                .iter()
                .any(|p| p.ends_with("todos/src/subroutes/comments/mod.rs"))
        );

        // Agent edits handlers.rs; tool hand-edits lib.rs (illegally).
        let handlers = routes.join("todos/src/handlers.rs");
        fs::write(&handlers, "// AGENT CODE\n").unwrap();
        let lib = routes.join("todos/src/lib.rs");
        fs::write(&lib, "// hand edit\n").unwrap();

        write_module(&routes, &m, GenMode::default()).unwrap();
        assert_eq!(
            fs::read_to_string(&handlers).unwrap(),
            "// AGENT CODE\n",
            "agent-owned: preserved"
        );
        assert!(
            fs::read_to_string(&lib)
                .unwrap()
                .contains("pub fn module()"),
            "tool-owned: restored"
        );
    }

    /// Real agent designs declare `id` on their entities (the docs' Todo does).
    /// Emitting the synthetic pk alongside it is a duplicate-column error that
    /// breaks every migration at apply time — the declared field must BE the pk.
    #[test]
    fn declared_id_field_becomes_the_pk_not_a_duplicate_column() {
        let mut m = todos();
        m.entities[0].fields.insert(
            0,
            Field {
                name: "id".into(),
                field_type: FieldType::Integer,
                required: true,
                unique: false,
                index: false,
                values: None,
            },
        );
        let ddl = migration_ddl(&m, false).unwrap();
        assert_eq!(
            ddl.matches("\"id\"").count(),
            1,
            "one id column only:\n{ddl}"
        );
        assert!(
            ddl.contains("PRIMARY KEY AUTOINCREMENT"),
            "sqlite autoincrement pk: {ddl}"
        );
        let pg = migration_ddl(&m, true).unwrap();
        assert!(
            pg.to_lowercase().contains("bigserial"),
            "postgres serial pk: {pg}"
        );
        assert_eq!(pg.matches("\"id\"").count(), 1, "{pg}");
    }

    /// Text ids (uuid/string) are the pk with their declared type, and the
    /// whole generated surface keys on String — repo signatures, the insert
    /// return (sqlite has no last_insert_id for text pks), and Path extractors.
    #[test]
    fn text_id_keys_the_table_repo_and_handlers_consistently() {
        let mut m = todos();
        m.entities[0].fields.insert(
            0,
            Field {
                name: "id".into(),
                field_type: FieldType::Uuid,
                required: true,
                unique: false,
                index: false,
                values: None,
            },
        );
        let ddl = migration_ddl(&m, false).unwrap();
        assert!(
            ddl.to_lowercase()
                .contains("\"id\" text not null primary key"),
            "text pk, no autoincrement: {ddl}"
        );
        assert!(!ddl.contains("AUTOINCREMENT"), "{ddl}");
        assert_eq!(ddl.matches("\"id\"").count(), 1, "{ddl}");

        let repo = repo_rs(
            &m,
            GenMode {
                db: true,
                ..GenMode::default()
            },
        )
        .unwrap();
        assert!(
            repo.contains("pub async fn get(&self, id: String)"),
            "{repo}"
        );
        assert!(
            repo.contains("pub async fn insert(&self, item: Todo) -> Result<String>"),
            "{repo}"
        );
        assert!(!repo.contains(".last_insert_id()"), "{repo}");

        let h = handlers_rs(&m, GenMode::default());
        assert!(h.contains("Path(_id): Path<String>"), "{h}");
    }

    #[test]
    fn subroutes_without_entities_have_no_model_or_repo() {
        let m = todos();
        let sub = &m.subroutes[0];
        assert!(model_rs(sub).is_none());
        assert!(repo_rs(sub, GenMode::default()).is_none());
        let h = handlers_rs(sub, GenMode::default());
        assert!(
            h.contains("pub(crate) async fn list_comments() -> Result<Json<serde_json::Value>>"),
            "{h}"
        );
    }
}
