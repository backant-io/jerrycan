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
