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
    // `done` is `required: false`, so its column is NULLABLE (it backs an
    // `Option<bool>` Model field) — no NOT NULL, no zero-DEFAULT.
    assert!(
        sqlite.to_lowercase().contains("\"done\" boolean")
            && !sqlite.to_lowercase().contains("\"done\" boolean not null"),
        "optional bool field is a nullable native boolean: {sqlite}"
    );
    assert!(postgres.to_lowercase().contains("bigserial"), "{postgres}");
    // Postgres renders the native boolean type as `bool` (SQLite as `boolean`);
    // both are native booleans, not the old BIGINT-as-i64 storage. Still nullable.
    assert!(
        postgres.to_lowercase().contains("\"done\" bool")
            && !postgres.to_lowercase().contains("\"done\" bool not null"),
        "optional bool field is a nullable native boolean: {postgres}"
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

/// Pipe a file through the pinned toolchain's rustfmt (the same one `cargo fmt`
/// runs) with the generated apps' edition. cwd is pinned to the app root so no
/// stray rustfmt.toml changes the defaults.
fn rustfmt(root: &std::path::Path, src: &str) -> String {
    use std::io::Write as _;
    let mut child = std::process::Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("rustfmt must be runnable (pinned toolchain component)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(src.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "rustfmt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A db+auth+storage+cors design whose long module (`organization-invitations`)
/// and bucket (`organization-documents`) names push their `.mount(..)` lines past
/// rustfmt's `fn_call_width` (60), and whose CORS methods list pushes an array
/// setter past rustfmt's wrap point (issue #128). The one-line emission of any of
/// these would be rewrapped by `cargo fmt`, tripping JL0003 on the tool-owned
/// main.rs the agent never touched — so `expected_main` must pre-wrap all three.
const LONG_MOUNTS_AND_CORS: &str = r#"{
    "name": "invites-app", "contract_version": 2,
    "auth": { "model": "session", "roles": ["owner", "member"] },
    "dependencies": ["db", "auth"],
    "cors": {
        "origins": ["https://app.example", "https://admin.example"],
        "methods": ["GET", "POST", "PUT", "DELETE"],
        "headers": ["content-type", "authorization"],
        "allow_credentials": true
    },
    "storage": { "buckets": [
        { "name": "organization-documents", "visibility": "public", "max_size": "5MB" }
    ]},
    "modules": [
        { "name": "organization-invitations",
          "entities": [{ "name": "Invitation", "fields": [
              { "name": "id", "type": "integer" },
              { "name": "email", "type": "string" } ]}],
          "endpoints": [{ "operation_id": "list_invitations", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Invitation", "list": true } }] }
    ]
}"#;

/// #128: the tool-owned app/src/main.rs and app/src/migrations.rs must be
/// rustfmt FIXPOINTS for every app shape. Agents run `cargo fmt` before
/// `jerrycan check`; if fmt rewraps a GENERATED file the agent never touched
/// (the OpenApi extend line, a >100-char `workspaces` postgres include_str!,
/// the single-migration array rustfmt collapses to `&[Migration { .. }]`, or a
/// module/bucket `.mount(..)` / CORS setter past rustfmt's wrap point), JL0003
/// fires on it and blames the agent for drift it didn't cause. WHY a real
/// rustfmt round-trip: string goldens can't prove fixpoint-ness — only feeding
/// the emitted bytes back through rustfmt can.
#[test]
fn tool_owned_main_and_migrations_are_rustfmt_fixpoints() {
    for design_src in [
        // openapi extend line + multi-entry migrations with a >100-char postgres path
        include_str!("../../../conformance/designs/reference-slice.design.json"),
        // single-module db app: the collapsed `&[Migration { .. }]` shape
        include_str!("../../../conformance/designs/limits-api.design.json"),
        // memory mode: no migrations.rs, main.rs without the openapi line
        GOLDEN,
        // long module + bucket mounts (> fn_call_width) + a wrapping CORS setter
        LONG_MOUNTS_AND_CORS,
    ] {
        let design: Design = serde_json::from_str(design_src).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        scaffold::scaffold(&root, &design).unwrap();
        for rel in ["crates/app/src/main.rs", "crates/app/src/migrations.rs"] {
            let path = root.join(rel);
            if !path.exists() {
                continue;
            }
            let emitted = fs::read_to_string(&path).unwrap();
            let formatted = rustfmt(&root, &emitted);
            assert_eq!(
                emitted, formatted,
                "{} must be a rustfmt fixpoint for design `{}` — otherwise an \
                 agent's `cargo fmt` rewrites it and JL0003 fires on a file the \
                 agent never touched",
                rel, design.name
            );
        }
    }
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
fn tenancy_generates_the_tenant_guard_in_shared() {
    let s = include_str!("../../../conformance/designs/reference-slice.design.json");
    let d: jerrycan::platform::design::Design = serde_json::from_str(s).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    jerrycan::platform::scaffold::scaffold(&root, &d).unwrap();
    let shared = std::fs::read_to_string(root.join("crates/shared/src/lib.rs")).unwrap();
    assert!(shared.contains("pub struct Tenant"), "{shared}");
    assert!(
        shared.contains("pub async fn tenant("),
        "guard factory: {shared}"
    );
    assert!(
        shared.contains("workspace_members"),
        "membership check: {shared}"
    );
    let main_rs = std::fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert!(
        main_rs.contains(".provide_dep(shared::tenant)"),
        "{main_rs}"
    );
}

#[test]
fn reference_slice_design_is_valid_contract_v2() {
    let s = include_str!("../../../conformance/designs/reference-slice.design.json");
    let d: jerrycan::platform::design::Design = serde_json::from_str(s).unwrap();
    assert_eq!(d.contract_version, 2);
    let qs = jerrycan::platform::questions::validate(&d);
    assert!(qs.is_empty(), "{qs:?}");
    assert_eq!(d.tenant_owned().len(), 2); // Lead, ApiKey
}

#[test]
fn generate_migration_emits_numbered_pair_and_rewires() {
    let (_t, root) = scaffold_db();
    let created =
        jerrycan::platform::genroute::generate_migration(&root, "todos", "add_due_index").unwrap();
    assert!(
        created
            .iter()
            .any(|p| p.ends_with("migrations/sqlite/0002_add_due_index.sql")),
        "{created:?}"
    );
    assert!(
        created
            .iter()
            .any(|p| p.ends_with("migrations/postgres/0002_add_due_index.sql")),
        "{created:?}"
    );
    let agg = std::fs::read_to_string(root.join("crates/app/src/migrations.rs")).unwrap();
    assert!(agg.contains("0002_add_due_index"), "{agg}");
    // numbering continues
    let again = jerrycan::platform::genroute::generate_migration(&root, "todos", "more").unwrap();
    assert!(
        again.iter().any(|p| p.ends_with("0003_more.sql")),
        "{again:?}"
    );
}

#[tokio::test]
async fn schema_verify_flags_staleness_with_jc0520() {
    let s = include_str!("../../../conformance/designs/reference-slice.design.json");
    let d: jerrycan::platform::design::Design = serde_json::from_str(s).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    jerrycan::platform::scaffold::scaffold(&root, &d).unwrap();
    // fresh derivation written → verify passes
    let c = jerrycan::platform::schema::derive_schema(&root, &d)
        .await
        .unwrap();
    std::fs::write(
        root.join("schema.json"),
        jerrycan::platform::schema::render(&c),
    )
    .unwrap();
    assert!(
        jerrycan::platform::schema::verify_fresh(&root, &d)
            .await
            .unwrap()
            .is_empty()
    );
    // stale file → JC0520 diagnostic
    std::fs::write(root.join("schema.json"), "{}").unwrap();
    let diags = jerrycan::platform::schema::verify_fresh(&root, &d)
        .await
        .unwrap();
    assert!(diags.iter().any(|x| x.code == "JC0520"), "{diags:?}");
    // missing file → also JC0520
    std::fs::remove_file(root.join("schema.json")).unwrap();
    assert!(
        !jerrycan::platform::schema::verify_fresh(&root, &d)
            .await
            .unwrap()
            .is_empty()
    );
}

/// F2 regression probe (the eval finding): per-module acceptance tests migrate
/// ONLY their own module, but Lead belongs_to Workspace lives in a DIFFERENT
/// module. Under SQLite FK enforcement, a real `FOREIGN KEY ... REFERENCES
/// "workspaces"` on the leads table makes every insert 500 with "no such table:
/// workspaces" (the leads migration never creates it). The fix: the cross-module
/// relation is an UNENFORCED column, so applying the leads migration ALONE — with
/// foreign_keys=ON — and inserting a lead with workspace_id=1 must succeed.
#[tokio::test]
async fn cross_module_fk_lets_per_module_migration_insert_under_fk_enforcement() {
    use jerrycan::db::sea_orm::ConnectionTrait;

    let s = include_str!("../../../conformance/designs/reference-slice.design.json");
    let d: Design = serde_json::from_str(s).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    scaffold::scaffold(&root, &d).unwrap();

    // Only the leads module's migrations — exactly what `gen-tests --module leads`
    // applies. The workspaces table is deliberately absent.
    let all = jerrycan::platform::mounting::collect_migrations(&root).unwrap();
    let leads_only: Vec<_> = all
        .into_iter()
        .filter(|m| m.name.starts_with("leads_"))
        .collect();
    assert!(
        !leads_only.is_empty(),
        "leads module must own a migration file"
    );

    let db = jerrycan::db::Db::connect("sqlite::memory:").await.unwrap();
    // FK enforcement is ON — `Db::connect` pins `foreign_keys=ON` on every
    // SQLite connection (no manual PRAGMA needed). Without enforcement the bug
    // would be masked; this is the exact enforcement gen-tests runs under.
    db.migrate_owned(&leads_only).await.unwrap();

    // Insert a lead whose workspace_id points at a workspace row that does NOT
    // (and cannot) exist in this single-module database. A real FK would reject
    // this (or the migration itself would have failed on "no such table"); the
    // unenforced relation lets it through, which is the whole point of F2.
    db.conn()
        .execute_unprepared(
            "INSERT INTO leads (workspace_id, phone, name, status) VALUES (1, '555', 'A', 'new')",
        )
        .await
        .expect("cross-module fk is unenforced: insert with a dangling workspace_id must succeed");

    let count = db
        .conn()
        .query_one(jerrycan::db::sea_orm::Statement::from_string(
            jerrycan::db::sea_orm::DatabaseBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM leads".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    let n: i64 = count
        .try_get::<i64>("", "n")
        .or_else(|_| count.try_get::<i32>("", "n").map(i64::from))
        .unwrap();
    assert_eq!(n, 1, "the lead row persisted");
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

/// #115 composite / multi-column UNIQUE: an entity's `unique: [["user_id",
/// "post_id"]]` group emits a standalone `CREATE UNIQUE INDEX … (user_id,
/// post_id)` on the entity's table, in BOTH dialects — the DB constraint that
/// makes a duplicate `(user_id, post_id)` a 409 (JC0409) instead of a race.
#[test]
fn db_mode_emits_composite_unique_index_both_dialects() {
    const LIKES: &str = r#"{
        "name": "likes-api", "contract_version": 1,
        "dependencies": ["db"],
        "modules": [{
            "name": "engagement",
            "entities": [
                { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                { "name": "Post", "fields": [{ "name": "title", "type": "string" }] },
                { "name": "Like",
                  "belongs_to": [{ "entity": "User" }, { "entity": "Post" }],
                  "unique": [["user_id", "post_id"]],
                  "fields": [{ "name": "reaction", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "create_like", "method": "POST", "path": "/likes",
                  "request_body": { "entity": "Like" },
                  "success": { "status": 201, "entity": "Like" } }
            ]
        }]
    }"#;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("likes-api");
    let design: Design = serde_json::from_str(LIKES).unwrap();
    scaffold::scaffold(&root, &design).unwrap();
    for dialect in ["sqlite", "postgres"] {
        let sql = fs::read_to_string(root.join(format!(
            "crates/routes/engagement/migrations/{dialect}/0001_create_tables.sql"
        )))
        .unwrap();
        // Exactly one composite unique index, over both fk columns in author
        // order, on the Like table, with the ordinal-disambiguated name (#115
        // review: collision-proof, never a lossy column join).
        assert!(
            sql.contains(
                "CREATE UNIQUE INDEX \"idx_likes_uc0\" ON \"likes\" (\"user_id\", \"post_id\")"
            ),
            "{dialect}: the composite unique index must be emitted verbatim:\n{sql}"
        );
        // Byte-identity floor: the plain User/Post tables emit no such index —
        // the Like group is the ONLY composite unique index in the file.
        assert_eq!(
            sql.matches("CREATE UNIQUE INDEX").count(),
            1,
            "{dialect}: only the composite-unique entity gets an index:\n{sql}"
        );
    }
}

/// Issue #119 — the acceptance criterion: a two-reference entity and a
/// self-reference both scaffold to DISTINCT aliased fk columns with DISTINCT
/// DDL constraint names, in ONE module (so the FKs are real DB constraints).
/// `Transfer belongs_to Account as from_account/to_account` → from_account_id +
/// to_account_id (never a single colliding account_id); `Comment belongs_to
/// Comment as parent` → parent_id → comments.
#[test]
fn db_mode_emits_aliased_fk_columns_with_distinct_constraint_names() {
    const LEDGER: &str = r#"{
        "name": "ledger-api", "contract_version": 1,
        "dependencies": ["db"],
        "modules": [{
            "name": "ledger",
            "entities": [
                { "name": "Account", "fields": [{ "name": "name", "type": "string" }] },
                { "name": "Transfer",
                  "belongs_to": [
                      { "entity": "Account", "as": "from_account" },
                      { "entity": "Account", "as": "to_account" }
                  ],
                  "fields": [{ "name": "amount", "type": "integer" }] },
                { "name": "Comment",
                  "belongs_to": [{ "entity": "Comment", "as": "parent", "on_delete": "cascade" }],
                  "fields": [{ "name": "body", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "create_transfer", "method": "POST", "path": "/transfers",
                  "request_body": { "entity": "Transfer" },
                  "success": { "status": 201, "entity": "Transfer" } }
            ]
        }]
    }"#;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ledger-api");
    let design: Design = serde_json::from_str(LEDGER).unwrap();
    scaffold::scaffold(&root, &design).unwrap();
    for dialect in ["sqlite", "postgres"] {
        let sql = fs::read_to_string(root.join(format!(
            "crates/routes/ledger/migrations/{dialect}/0001_create_tables.sql"
        )))
        .unwrap();
        // Two DISTINCT fk columns on transfers — the alias replaced the default
        // account_id, so the two refs to Account no longer collide on one column.
        assert!(
            sql.contains("\"from_account_id\"") && sql.contains("\"to_account_id\""),
            "{dialect}: both aliased fk columns must be emitted:\n{sql}"
        );
        assert!(
            !sql.contains("\"account_id\""),
            "{dialect}: the default account_id must NOT be emitted (the alias replaced it):\n{sql}"
        );
        // Two independent FOREIGN KEYs to accounts (one per aliased column).
        assert_eq!(
            sql.matches("REFERENCES \"accounts\"").count(),
            2,
            "{dialect}: two distinct FKs must reference accounts:\n{sql}"
        );
        // The self-reference: parent_id → comments.
        assert!(
            sql.contains("\"parent_id\"") && sql.contains("REFERENCES \"comments\""),
            "{dialect}: the self-reference must emit parent_id → comments:\n{sql}"
        );
    }
    // Postgres NAMES its table constraints `fk_{table}_{col}` — the alias makes the
    // two FKs to accounts get DISTINCT names; without it, two constraints named
    // `fk_transfers_account_id` on one table would be rejected at apply. This is the
    // #119 acceptance criterion: two distinct constraint names for the two refs.
    let pg = fs::read_to_string(
        root.join("crates/routes/ledger/migrations/postgres/0001_create_tables.sql"),
    )
    .unwrap();
    assert!(
        pg.contains("\"fk_transfers_from_account_id\"")
            && pg.contains("\"fk_transfers_to_account_id\"")
            && pg.contains("\"fk_comments_parent_id\""),
        "postgres must name the three FK constraints distinctly:\n{pg}"
    );
}
