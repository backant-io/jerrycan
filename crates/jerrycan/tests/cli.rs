//! Fast CLI contract tests. Exit codes per docs/contracts/cli-ux.md:
//! 0 ok · 1 gate failed · 2 usage error · 3 environment error.

use std::process::Command;

fn jerrycan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jerrycan"))
}

#[test]
fn version_prints_and_exits_zero() {
    let out = jerrycan().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_flag_is_usage_error_exit_2() {
    let out = jerrycan().arg("--definitely-not-a-flag").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn help_lands_on_stdout_not_stderr() {
    let out = jerrycan().arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("jerrycan"));
    assert!(out.stderr.is_empty(), "help must not pollute stderr");
}

#[test]
fn missing_required_arg_is_usage_error_exit_2() {
    // `new` requires --design; no interactive prompts ever (cli-ux.md non-goals).
    let out = jerrycan().args(["new", "demo"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--design"),
        "must name the exact missing flag: {err}"
    );
}

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

/// Multi-module design WITH jobs (6 endpoint-bearing modules + 2 cron jobs) —
/// the whole-design surface `gen-tests` without `--module` must cover.
const REFERENCE: &str = include_str!("../../../conformance/designs/reference-slice.design.json");

/// #27 regression: a design whose `tenancy.entity` IS the auth identity entity.
/// It validates clean (no completeness question) but can't scaffold — the
/// generated membership table would declare `user_id` twice.
const TENANT_IS_IDENTITY: &str = include_str!("fixtures/tenant-is-identity.design.json");

/// Issue #69a: `jerrycan add` and `generate route` REWRITE tool-owned lib.rs, and
/// before this an agent's hand-added `mod` wiring (a cross-module sweep) vanished
/// silently — exactly how JR4 lost its wiring. The command must WARN loudly:
/// stderr AND the `--json` envelope must NAME the dropped line. It still succeeds
/// (warn, not refuse) so the rest of the regeneration proceeds.
#[test]
fn add_and_generate_route_warn_loudly_about_dropped_agent_mod_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, GOLDEN).unwrap(); // todo-api (memory mode; no build needed)
    let app = tmp.path().join("todo-api");
    let st = jerrycan()
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "scaffold must succeed");

    // The agent wires a cross-module sweep by hand-adding a `mod` line to the
    // TOOL-owned lib.rs (the file jerrycan regenerates).
    let lib = app.join("crates/routes/todos/src/lib.rs");
    let sweep = "mod cross_sweep;\n";
    let readd = || {
        let orig = std::fs::read_to_string(&lib).unwrap();
        std::fs::write(&lib, format!("{orig}{sweep}")).unwrap();
    };

    // (1) `jerrycan add validate --json` regenerates every module → would drop it.
    readd();
    let out = jerrycan()
        .current_dir(&app)
        .args(["--json", "add", "validate"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "add still succeeds (warn, not refuse)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("warning") && stderr.contains("mod cross_sweep;"),
        "stderr must warn and NAME the dropped line: {stderr}"
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON envelope");
    let warnings = payload["warnings"].to_string();
    assert!(
        warnings.contains("mod cross_sweep;") && warnings.contains("lib.rs"),
        "--json envelope must carry the dropped line + file: {payload}"
    );

    // (2) `jerrycan generate route todos --json` regenerates the module the same way.
    readd();
    let out = jerrycan()
        .current_dir(&app)
        .args(["--json", "generate", "route", "todos"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mod cross_sweep;"),
        "generate route must warn too: {stderr}"
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON envelope");
    assert!(
        payload["warnings"].to_string().contains("mod cross_sweep;"),
        "generate route --json envelope must carry the dropped line: {payload}"
    );

    // No-drift control: a clean regeneration (no agent edits) reports NO warnings.
    let out = jerrycan()
        .current_dir(&app)
        .args(["--json", "generate", "route", "todos"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        payload.get("warnings").is_none(),
        "a clean regeneration carries no `warnings` key: {payload}"
    );
}

/// #27 fail-loud: `tenancy.entity` naming the auth identity must be rejected
/// BEFORE any file is written, with the JC0540 code and a message naming both
/// fixes — not die mid-scaffold with a raw SQLite `duplicate column name`.
#[test]
fn new_rejects_tenant_as_auth_identity_before_scaffolding() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, TENANT_IS_IDENTITY).unwrap();
    let app_dir = tmp.path().join("app");

    let out = jerrycan()
        .args(["--json", "new"])
        .arg(&app_dir)
        .arg("--design")
        .arg(&design_path)
        .output()
        .unwrap();

    assert_ne!(out.status.code(), Some(0), "the conflict must fail the run");
    // #28: a --json failure emits EXACTLY one JSON envelope on stdout.
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is exactly one JSON document");
    assert_eq!(payload["ok"], serde_json::json!(false));
    assert_eq!(payload["code"], "JC0540");
    let err = payload["error"].as_str().expect("error is a string");
    // The message names both fixes (per-user → belongs_to; orgs/teams → separate entity).
    assert!(
        err.contains("belongs_to") && err.contains("tenant entity"),
        "error must name both fixes: {err}"
    );
    // No half-scaffolded tree: the app dir was never created.
    assert!(
        !app_dir.exists(),
        "no files may be written when validation rejects the design"
    );
}

/// #28: the machine envelope is universal — a failure with no JC code (a missing
/// design file) still emits `{ok:false, code:null, error, hint}`, not empty stdout.
#[test]
fn json_failure_envelope_is_emitted_for_a_non_lint_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist.json");

    let out = jerrycan()
        .args(["--json", "new"])
        .arg(tmp.path().join("app"))
        .arg("--design")
        .arg(&missing)
        .output()
        .unwrap();

    assert_ne!(out.status.code(), Some(0), "a missing design must fail");
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is exactly one JSON document");
    assert_eq!(payload["ok"], serde_json::json!(false));
    assert!(
        payload["code"].is_null(),
        "no JC code for a plain usage error"
    );
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("does-not-exist"),
        "error names the unreadable path: {}",
        payload["error"]
    );
    assert!(payload.get("hint").is_some(), "hint key is always present");
}

/// #27: `jerrycan explain JC0540` returns the tenant-identity guidance.
#[test]
fn explain_jc0540_returns_the_tenant_identity_guidance() {
    let out = jerrycan().args(["explain", "JC0540"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("JC0540"));
    assert!(
        text.contains("belongs_to") && text.contains("tenant entity"),
        "explain names both fixes: {text}"
    );
}

#[test]
fn new_scaffolds_and_emits_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");

    let out = jerrycan()
        .args(["--json", "new"])
        .arg(&app_dir)
        .arg("--design")
        .arg(&design_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // --json: stdout is exactly one JSON document matching the MCP outputSchema.
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert!(payload["created"].as_array().unwrap().len() > 10);
    assert!(payload["next_step"].as_str().unwrap().contains("check"));
    assert!(app_dir.join("crates/routes/todos/src/lib.rs").exists());
}

#[test]
fn new_with_invalid_design_returns_questions_and_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN.replace("\"todo-api\"", "\"Todo API\"")).unwrap();

    let out = jerrycan()
        .args(["--json", "new"])
        .arg(tmp.path().join("x"))
        .arg("--design")
        .arg(&design_path)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "incomplete design = gate failure"
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["status"], "questions");
    assert!(
        payload["questions"][0]["id"]
            .as_str()
            .unwrap()
            .starts_with("/name")
    );
}

#[test]
fn generate_route_adds_a_module_and_rewires_mounting() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );

    // Add a module to the app's design.json (the agent's edit), then generate.
    let dj = app_dir.join("design.json");
    let mut design: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dj).unwrap()).unwrap();
    design["modules"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "tags",
            "endpoints": [{ "operation_id": "list_tags", "method": "GET", "path": "/",
                            "success": { "status": 200 } }]
        }));
    std::fs::write(&dj, serde_json::to_string_pretty(&design).unwrap()).unwrap();

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "generate", "route", "tags"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        payload["created"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("routes/tags"))
    );
    assert!(
        payload["modified"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "crates/app/src/main.rs")
    );
    let main_rs = std::fs::read_to_string(app_dir.join("crates/app/src/main.rs")).unwrap();
    assert!(main_rs.contains(".mount(\"/tags\", route_tags::module())"));
}

#[test]
fn generate_route_for_unknown_module_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["generate", "route", "ghosts"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("design.json"));
}

#[test]
fn generate_migration_writes_numbered_pair_and_rewires() {
    // db mode is required for migrations: the aggregated migrations.rs only
    // exists when the design wants db, and that file is what proves the rewire.
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["db"]);
    std::fs::write(&design_path, serde_json::to_string_pretty(&design).unwrap()).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );

    // JSON surface: created lists both dialect files; next_step points at check.
    let out = jerrycan()
        .current_dir(&app_dir)
        .args([
            "--json",
            "generate",
            "migration",
            "add_due_index",
            "--module",
            "todos",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let created = payload["created"].as_array().unwrap();
    assert!(
        created.iter().any(|p| p
            .as_str()
            .unwrap()
            .ends_with("migrations/sqlite/0002_add_due_index.sql")),
        "{created:?}"
    );
    assert!(
        created.iter().any(|p| p
            .as_str()
            .unwrap()
            .ends_with("migrations/postgres/0002_add_due_index.sql")),
        "{created:?}"
    );
    assert_eq!(
        payload["next_step"],
        "edit both dialect files, then run jerrycan check"
    );
    // The aggregated migrations.rs must now include the new sqlite file — the
    // rewire is what makes a migration actually run, so this is the load-bearing
    // assertion, not the file-on-disk.
    let agg = std::fs::read_to_string(app_dir.join("crates/app/src/migrations.rs")).unwrap();
    assert!(agg.contains("0002_add_due_index"), "{agg}");

    // Human surface carries the numbered stem; a non-snake name is a usage error.
    let bad = jerrycan()
        .current_dir(&app_dir)
        .args(["generate", "migration", "BadName", "--module", "todos"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("snake_case"));
}

#[test]
fn list_routes_walks_the_module_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "list", "routes"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let routes = payload["routes"].as_array().unwrap();
    let find = |method: &str, path: &str| {
        routes
            .iter()
            .any(|r| r["method"] == method && r["path"] == path)
    };
    assert!(find("GET", "/todos/"), "{routes:?}");
    assert!(find("DELETE", "/todos/{id}"));
    assert!(
        find("GET", "/todos/comments/"),
        "subroute paths compose: {routes:?}"
    );
    assert!(find("POST", "/users/"));
    let todo = routes.iter().find(|r| r["path"] == "/todos/").unwrap();
    assert_eq!(todo["module"], "todos");
    assert_eq!(todo["handler"], "list_todos");
}

#[test]
fn add_db_flips_the_design_and_regenerates() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "add", "db"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        payload["modified"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "design.json")
    );

    let dj: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(app_dir.join("design.json")).unwrap())
            .unwrap();
    assert!(
        dj["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d == "db")
    );
    let main_rs = std::fs::read_to_string(app_dir.join("crates/app/src/main.rs")).unwrap();
    assert!(
        main_rs.contains("Db::from_env"),
        "mounting regenerated in db mode: {main_rs}"
    );
    let ws = std::fs::read_to_string(app_dir.join("Cargo.toml")).unwrap();
    assert!(ws.contains("features = [\"db\"]"), "{ws}");

    let deny = std::fs::read_to_string(app_dir.join("deny.toml")).unwrap();
    assert!(
        deny.contains("CDLA-Permissive-2.0"),
        "db policy applied on add: {deny}"
    );
    assert!(
        app_dir.join(".cargo/audit.toml").exists(),
        "audit ignore applied on add"
    );
    assert!(
        payload["next_step"].as_str().unwrap().contains("repo.rs"),
        "must warn about hand-migration"
    );

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["add", "nonsense"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn db_migrate_applies_module_migrations() {
    let tmp = tempfile::tempdir().unwrap();
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["db"]);
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, serde_json::to_string_pretty(&design).unwrap()).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );

    let db_file = tmp.path().join("test.db");
    let url = format!("sqlite://{}?mode=rwc", db_file.display());
    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "db", "migrate", "--url", &url])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let applied = payload["applied"].as_array().unwrap();
    assert!(
        applied
            .iter()
            .any(|a| a.as_str().unwrap().contains("todos")),
        "{applied:?}"
    );

    // Idempotent: second run applies nothing.
    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "db", "migrate", "--url", &url])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(payload["applied"].as_array().unwrap().is_empty());
}

#[test]
fn schema_json_prints_the_derived_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["db"]);
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, serde_json::to_string_pretty(&design).unwrap()).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(
        jerrycan()
            .args(["new"])
            .arg(&app_dir)
            .arg("--design")
            .arg(&design_path)
            .status()
            .unwrap()
            .success()
    );
    // scaffold writes schema.json for a db app.
    assert!(
        app_dir.join("schema.json").exists(),
        "scaffold wrote schema.json"
    );

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "schema"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let tables = payload["tables"].as_array().expect("tables array");
    assert!(
        tables.iter().any(|t| t["name"] == "todos"),
        "contract names the todos table: {payload}"
    );
    assert_eq!(payload["schema_version"], 1);
}

#[test]
fn docs_command_prints_pages_and_searches() {
    let out = jerrycan().args(["docs", "dependencies"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("# Dependencies"));

    let out = jerrycan()
        .args(["--json", "docs", "--search", "override_dep"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["results"][0]["page"], "testing");

    let out = jerrycan().args(["docs", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn docs_list_enumerates_every_page_in_one_call() {
    // Gap A: an agent must be able to enumerate the whole docs surface in a
    // single call, not ~30 searches. `--json docs --list` returns every page.
    let listed: serde_json::Value = {
        let out = jerrycan()
            .args(["--json", "docs", "--list"])
            .output()
            .unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let pages = listed["pages"].as_array().expect("pages array");
    // The well-known slugs an agent expects must all appear (covers the surface).
    let slugs: Vec<&str> = pages.iter().map(|p| p["page"].as_str().unwrap()).collect();
    for must in ["app", "testing", "database", "tenancy", "auth-advanced"] {
        assert!(
            slugs.contains(&must),
            "page index must list `{must}`: {slugs:?}"
        );
    }
    assert!(
        pages.len() >= 15,
        "index lists the whole surface: {}",
        pages.len()
    );
    // Every row carries a one-line summary so a page can be picked without a get.
    assert!(
        pages
            .iter()
            .all(|p| !p["summary"].as_str().unwrap_or("").is_empty()),
        "each page row has a summary"
    );

    // Bare `jerrycan docs` (no topic, no --search) lists too, same page count.
    let bare: serde_json::Value = {
        let out = jerrycan().args(["--json", "docs"]).output().unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    };
    assert_eq!(bare["pages"].as_array().unwrap().len(), pages.len());
}

#[test]
fn docs_search_default_limit_does_not_silently_truncate() {
    // Gap A: a broad query used to be capped at 5 hits, silently hiding pages.
    // The default limit is now the page count, so a near-universal term returns
    // more than the old cap of 5 (one hit per matching page).
    let out = jerrycan()
        .args(["--json", "docs", "--search", "jerrycan"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = payload["results"].as_array().unwrap();
    assert!(
        results.len() > 5,
        "default search no longer caps at 5: got {}",
        results.len()
    );
    // --limit still honored (and can clamp below the default).
    let out = jerrycan()
        .args(["--json", "docs", "--search", "jerrycan", "--limit", "3"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["results"].as_array().unwrap().len(), 3);
}

#[test]
fn onboard_prints_the_runbook_without_frontmatter() {
    let out = jerrycan().arg("onboard").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("# Building a backend with jerrycan"),
        "runbook must start at the H1, not YAML frontmatter: {}",
        &stdout[..stdout.len().min(80)]
    );
    assert!(
        stdout.contains("Entry path"),
        "3-way entry branching missing"
    );
    assert!(
        stdout.contains("Phase 1c — Migrating from Supabase"),
        "migration phase missing"
    );
}

#[test]
fn onboard_json_is_one_document_with_next_step() {
    let out = jerrycan().args(["--json", "onboard"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["markdown"].as_str().unwrap().contains("Entry path"));
    assert!(v["next_step"].as_str().is_some());
}

#[test]
fn onboard_emit_skill_claude_code_writes_under_home() {
    let home = tempfile::tempdir().unwrap();
    let out = jerrycan()
        .env("HOME", home.path())
        .args([
            "--json",
            "onboard",
            "--emit-skill",
            "--agent",
            "claude-code",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["written"].as_array().unwrap().len(), 1);
    assert!(
        home.path()
            .join(".claude/skills/jerrycan-backend/SKILL.md")
            .exists()
    );
}

#[test]
fn onboard_emit_skill_without_agent_is_usage_error() {
    let out = jerrycan()
        .args(["onboard", "--emit-skill"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn onboard_emit_skill_unknown_agent_is_usage_error_naming_ids() {
    let out = jerrycan()
        .args(["onboard", "--emit-skill", "--agent", "zed"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("claude-code"));
}

/// #156 follow-on: `gen-tests` with NO `--module` covers the whole design —
/// one acceptance suite per endpoint-bearing module plus the jobs suite once.
/// WHY: JC0551's jobs diagnostic suggests the bare command (a jobs-only design
/// has no module name to pass), so the bare form must exist and must clear
/// every JC0551 the check can raise. Each file must be byte-identical to what
/// the per-module writer produces, and `expected_failing` must aggregate
/// without double-counting jobs. The `--module {m}` path's payload stays
/// frozen — existing goldens and agent runbooks depend on it verbatim.
#[test]
fn gen_tests_without_module_covers_every_endpoint_module_and_jobs() {
    use jerrycan::platform::design::Design;
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(bare.join("design.json"), REFERENCE).unwrap();

    let out = jerrycan()
        .current_dir(&bare)
        .args(["--json", "gen-tests"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bare gen-tests must run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");

    // Oracle: the same writers, driven directly, into a sibling root.
    let design = Design::from_path(&bare.join("design.json")).unwrap();
    let oracle = tmp.path().join("oracle");
    std::fs::create_dir_all(&oracle).unwrap();
    let mut expected_files = Vec::new();
    let mut expected_count = 0usize;
    let mut users_count = 0usize;
    for m in &design.modules {
        // every reference-slice module bears endpoints, so all are covered
        let (rel, c) =
            jerrycan::platform::testgen::write_acceptance(&oracle, &design, &m.name).unwrap();
        expected_files.push(rel);
        expected_count += c;
        if m.name == "users" {
            users_count = c;
        }
    }
    let (jobs_rel, jobs_count) =
        jerrycan::platform::jobsgen::write_jobs_acceptance(&oracle, &design)
            .unwrap()
            .expect("reference slice declares jobs");
    expected_files.push(jobs_rel);
    expected_count += jobs_count;

    let created: Vec<String> = payload["tests_created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        created, expected_files,
        "one suite per endpoint-bearing module, then jobs once"
    );
    for rel in &created {
        let a = std::fs::read(bare.join(rel)).unwrap_or_else(|e| panic!("{rel} not written: {e}"));
        let b = std::fs::read(oracle.join(rel)).unwrap();
        assert_eq!(a, b, "{rel} must match the per-module writer byte-for-byte");
    }
    assert_eq!(
        payload["expected_failing"].as_u64().unwrap() as usize,
        expected_count,
        "aggregate = sum of module counts + jobs counted exactly once"
    );
    let next = payload["next_step"].as_str().unwrap();
    for m in &design.modules {
        assert!(
            next.contains(&format!("cargo test -p route-{}", m.name)),
            "next_step must list route-{}: {next}",
            m.name
        );
    }
    assert!(next.contains("cargo test -p jobs"), "{next}");

    // The single-module path is UNCHANGED: same frozen payload, same bytes.
    let single = tmp.path().join("single");
    std::fs::create_dir_all(&single).unwrap();
    std::fs::write(single.join("design.json"), REFERENCE).unwrap();
    let out = jerrycan()
        .current_dir(&single)
        .args(["--json", "gen-tests", "--module", "users"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        payload["tests_created"],
        serde_json::json!([
            "crates/routes/users/tests/acceptance.rs",
            "crates/jobs/tests/acceptance.rs"
        ])
    );
    let single_total = users_count + jobs_count;
    assert_eq!(
        payload["next_step"].as_str().unwrap(),
        format!(
            "cargo test -p route-users && cargo test -p jobs (expect {single_total} failures total), implement handlers + job tasks, iterate"
        ),
        "the --module path's contract is frozen"
    );
    assert_eq!(
        std::fs::read(single.join("crates/routes/users/tests/acceptance.rs")).unwrap(),
        std::fs::read(bare.join("crates/routes/users/tests/acceptance.rs")).unwrap(),
        "both paths write the identical module suite"
    );
}

/// #156 actionability proof: on a jobs-only design (cron jobs, zero endpoint
/// modules) the JC0551 diagnostic says `run \`jerrycan gen-tests\`` — there is
/// no module to name, so that exact command must run AND clear the diagnostic.
/// WHY: a gate-honesty release must never ship a diagnostic whose suggested
/// fix cannot be executed. Full `jerrycan check` needs a compiled scaffold, so
/// the clear is asserted through the exact step check runs (missing_acceptance_tests).
#[test]
fn gen_tests_without_module_clears_jc0551_for_a_jobs_only_design() {
    use jerrycan::platform::checkpipe::missing_acceptance_tests;
    use jerrycan::platform::design::Design;
    const JOBS_ONLY: &str = r#"{
      "name": "cron-only",
      "contract_version": 1,
      "dependencies": ["db"],
      "jobs": [{ "name": "nightly_cleanup", "schedule": "0 3 * * *" }],
      "modules": []
    }"#;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), JOBS_ONLY).unwrap();
    let design = Design::from_path(&tmp.path().join("design.json")).unwrap();

    // The gate refuses the never-gen-tested jobs surface, suggesting the bare command.
    let before = missing_acceptance_tests(tmp.path(), &design, None);
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(
        before[0].suggestion.as_deref(),
        Some("run `jerrycan gen-tests`"),
        "the suggestion is the exact command run below"
    );

    // Run the suggested command VERBATIM.
    let out = jerrycan()
        .current_dir(tmp.path())
        .args(["--json", "gen-tests"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the suggested fix must be runnable: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        payload["tests_created"],
        serde_json::json!(["crates/jobs/tests/acceptance.rs"])
    );
    let next = payload["next_step"].as_str().unwrap();
    assert!(
        next.contains("cargo test -p jobs") && next.contains("implement job tasks"),
        "{next}"
    );

    // It cleared: the exact step `jerrycan check` runs no longer raises JC0551.
    assert!(tmp.path().join("crates/jobs/tests/acceptance.rs").exists());
    let after = missing_acceptance_tests(tmp.path(), &design, None);
    assert!(
        after.is_empty(),
        "the suggested command must clear the diagnostic that suggests it: {after:?}"
    );
}
