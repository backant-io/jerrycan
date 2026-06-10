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
