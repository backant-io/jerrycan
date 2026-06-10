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
    assert!(repo.contains("SELECT title, done FROM todos"), "{repo}");
    assert!(
        repo.contains("RETURNING id"),
        "postgres insert branch: {repo}"
    );
    assert!(
        repo.contains("last_insert_id()"),
        "sqlite insert branch: {repo}"
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
        sqlite.contains("CREATE TABLE todos")
            && sqlite.contains("INTEGER PRIMARY KEY AUTOINCREMENT"),
        "{sqlite}"
    );
    assert!(sqlite.contains("title TEXT NOT NULL"));
    assert!(
        sqlite.contains("done BOOLEAN NOT NULL DEFAULT 0"),
        "optional field gets a default: {sqlite}"
    );
    assert!(postgres.contains("id BIGSERIAL PRIMARY KEY"), "{postgres}");
    assert!(postgres.contains("done BOOLEAN NOT NULL DEFAULT FALSE"));
    // Subroute entities get their own module-owned migration:
    assert!(root.join("crates/routes/todos/migrations/sqlite").exists());
    let users = fs::read_to_string(
        root.join("crates/routes/users/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap();
    assert!(users.contains("CREATE TABLE users"));
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
