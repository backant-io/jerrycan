//! db-mode generation: SQL repos, migrations, mode-aware mounting, openapi.json.
//! Fast (tempdir + string assertions; real builds are the heavy conformance suite).

use jerrycan::platform::design::Design;
use jerrycan::platform::scaffold;
use std::fs;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn db_design() -> Design {
    let mut v: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    v["dependencies"] = serde_json::json!(["db", "validate"]);
    serde_json::from_value(v).unwrap()
}

fn scaffold_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    scaffold::scaffold(&root, &db_design()).unwrap();
    (tmp, root)
}

#[test]
fn db_mode_emits_sql_repos_with_di_factories() {
    let (_t, root) = scaffold_db();
    let repo = fs::read_to_string(root.join("crates/routes/todos/src/repo.rs")).unwrap();
    assert!(
        repo.contains("pub(crate) async fn todo_repo(db: Dep<Db>)"),
        "{repo}"
    );
    // Repos run on SeaORM through the jerrycan facade (NO direct sea-orm dep,
    // NO sea-query/sqlx). The alias lets the bodies write bare `sea_orm::` paths.
    assert!(
        repo.contains("use jerrycan::db::sea_orm;"),
        "facade alias resolves bare sea_orm:: paths: {repo}"
    );
    assert!(
        repo.contains("todo::Entity::find()") && repo.contains(".all(self.db.conn())"),
        "reads go through SeaORM entity finders: {repo}"
    );
    // No raw SQL strings, no sea-query builders, no sqlx pool: the dialect work
    // is library-owned inside SeaORM.
    assert!(
        !repo.contains("SELECT ") && !repo.contains("self.db.sql("),
        "no raw SQL strings in generated repos: {repo}"
    );
    assert!(
        !repo.contains("build_any_sqlx")
            && !repo.contains("self.db.pool()")
            && !repo.contains("sea_query"),
        "repos are SeaORM now, not sea-query/sqlx: {repo}"
    );
    // Synthetic pk → the DB assigns the autoincrement id (NotSet on insert), and
    // the inserted Model carries it back; never the sqlx Any last_insert_id (None
    // on sqlite, which made creates echo id 0).
    assert!(
        repo.contains("id: sea_orm::ActiveValue::NotSet,"),
        "synthetic pk is DB-assigned on insert: {repo}"
    );
    assert!(
        !repo.contains(".last_insert_id()"),
        "sqlite must not rely on last_insert_id: {repo}"
    );
    assert!(
        repo.contains("pub async fn update(&self, id: i64, item: Todo)"),
        "PUT/PATCH handlers need a persisting update: {repo}"
    );
    assert!(
        repo.contains("title: Set(item.title),") && repo.contains("done: Set(item.done),"),
        "update sets every non-pk field via the ActiveModel: {repo}"
    );
    assert!(repo.contains("map_err(db_error)"), "{repo}");
    let lib = fs::read_to_string(root.join("crates/routes/todos/src/lib.rs")).unwrap();
    assert!(lib.contains(".provide_dep(repo::todo_repo)"), "{lib}");
    assert!(
        !lib.contains("TodoRepo::new()"),
        "no in-memory provide in db mode: {lib}"
    );
}

#[test]
fn db_mode_emits_dual_dialect_migrations_from_entities() {
    let (_t, root) = scaffold_db();
    let sqlite = fs::read_to_string(
        root.join("crates/routes/todos/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap();
    let postgres = fs::read_to_string(
        root.join("crates/routes/todos/migrations/postgres/0001_create_tables.sql"),
    )
    .unwrap();
    assert!(
        sqlite.contains("CREATE TABLE \"todos\"") && sqlite.contains("PRIMARY KEY AUTOINCREMENT"),
        "{sqlite}"
    );
    assert!(sqlite.to_lowercase().contains("\"title\" text not null"));
    // Booleans are native BOOLEAN columns on both backends: the Model field is a
    // Rust `bool` under SeaORM, which round-trips it directly (no sqlx-Any i64).
    assert!(
        sqlite
            .to_lowercase()
            .contains("\"done\" boolean not null default false"),
        "optional bool field is native boolean with a default: {sqlite}"
    );
    assert!(postgres.to_lowercase().contains("bigserial"), "{postgres}");
    // Postgres renders the native boolean type as `bool` (SQLite as `boolean`);
    // both are native booleans, not the old BIGINT-as-i64 storage.
    assert!(
        postgres
            .to_lowercase()
            .contains("\"done\" bool not null default false")
    );
    // Subroute entities get their own module-owned migration:
    assert!(root.join("crates/routes/todos/migrations/sqlite").exists());
    let users = fs::read_to_string(
        root.join("crates/routes/users/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap();
    assert!(users.contains("CREATE TABLE \"users\""));
}

#[test]
fn db_mode_wires_main_and_aggregated_migrations() {
    let (_t, root) = scaffold_db();
    let main_rs = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert!(
        main_rs.contains("jerrycan::db::Db::from_env().await?"),
        "{main_rs}"
    );
    assert!(
        main_rs.contains("db.migrate(migrations::MIGRATIONS).await?"),
        "{main_rs}"
    );
    assert!(main_rs.contains(".extend(db)"), "{main_rs}");
    assert!(
        main_rs.contains("OpenApi::new(include_str!"),
        "validate mode mounts the doc: {main_rs}"
    );
    let agg = fs::read_to_string(root.join("crates/app/src/migrations.rs")).unwrap();
    assert!(agg.contains("pub const MIGRATIONS"), "{agg}");
    assert!(
        agg.contains("routes/todos/migrations/sqlite/0001_create_tables.sql"),
        "{agg}"
    );
    let ws = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(ws.contains("features = [\"db\", \"validate\"]"), "{ws}");
    assert!(root.join("openapi.json").exists());
}

#[test]
fn sql_identifiers_are_quoted_so_reserved_words_survive() {
    // A field named `order` is a SQL reserved word; quoting is the only thing that
    // keeps the generated DDL valid. (`order` is not a Rust keyword, so it passes
    // model-code validation and reaches the SQL layer.)
    let mut v: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    v["dependencies"] = serde_json::json!(["db"]);
    v["modules"][0]["entities"][0]["fields"][0]["name"] = serde_json::json!("order");
    let design: Design = serde_json::from_value(v).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    scaffold::scaffold(&root, &design).unwrap();
    let sqlite = fs::read_to_string(
        root.join("crates/routes/todos/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap();
    assert!(
        sqlite.to_lowercase().contains("\"order\" text"),
        "reserved-word column must be quoted: {sqlite}"
    );
}

#[test]
fn kolli_slice_design_is_valid_contract_v1() {
    let s = include_str!("../../../conformance/designs/kolli-slice.design.json");
    let d: jerrycan::platform::design::Design = serde_json::from_str(s).unwrap();
    assert_eq!(d.contract_version, 1);
    let qs = jerrycan::platform::questions::validate(&d);
    assert!(qs.is_empty(), "{qs:?}");
    assert_eq!(d.tenant_owned().len(), 2); // Lead, ApiKey
}

#[test]
fn memory_mode_is_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    let design: Design = serde_json::from_str(GOLDEN).unwrap();
    scaffold::scaffold(&root, &design).unwrap();
    let repo = fs::read_to_string(root.join("crates/routes/todos/src/repo.rs")).unwrap();
    assert!(repo.contains("BTreeMap"), "in-memory repo stays: {repo}");
    let main_rs = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert!(!main_rs.contains("jerrycan::db"));
    assert!(!root.join("crates/app/src/migrations.rs").exists());
    assert!(
        root.join("openapi.json").exists(),
        "openapi.json is emitted in every mode"
    );
}
