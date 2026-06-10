//! Heavy conformance tests (#[ignore]): real cargo builds of generated apps.
//! Run with: cargo test -p jerrycan --test conformance -- --include-ignored

use std::io::{Read, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// Scaffold the golden app wired to the LOCAL framework (path dep).
fn scaffold_golden(tmp: &Path) -> PathBuf {
    let design = tmp.join("design.json");
    std::fs::write(&design, GOLDEN).unwrap();
    let app = tmp.join("todo-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success());
    app
}

/// Scaffold the golden app in DB+validate mode against the LOCAL framework.
fn scaffold_golden_db(tmp: &Path) -> PathBuf {
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["db", "validate"]);
    let design_path = tmp.join("design.json");
    std::fs::write(&design_path, serde_json::to_string_pretty(&design).unwrap()).unwrap();
    let app = tmp.join("todo-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .unwrap();
    assert!(st.success());
    app
}

#[test]
#[ignore = "heavy: db-mode golden app must build and pass the full gate"]
fn db_mode_scaffold_passes_jerrycan_check() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_db(tmp.path());
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
    assert!(out.status.success());
}

#[test]
#[ignore = "heavy: full cargo build of a generated workspace"]
fn scaffolded_app_builds_with_zero_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden(tmp.path());
    let out = Command::new("cargo")
        .current_dir(&app)
        .env("RUSTFLAGS", "-D warnings")
        .args(["build", "--workspace"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "generated app must build warning-free:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "heavy: full verification pipeline incl. cargo-audit/cargo-deny"]
fn fresh_scaffold_passes_jerrycan_check() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden(tmp.path());
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("check --json emits one JSON document");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
    assert!(out.status.success());
}

/// THE Phase 1 exit criterion: an agent builds a working multi-module CRUD
/// service via MCP only (design → scaffold → implement → check → serve).
#[test]
#[ignore = "heavy: MCP loop + cargo build + live HTTP round-trips"]
fn agent_generates_working_crud_service_via_mcp_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let mut c =
        common::McpClient::start_in_with_env(tmp.path(), &[("JERRYCAN_FRAMEWORK_DEP", &dep)]);

    // 1. design: draft in, validated design.json out.
    let draft: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let (err, payload) = c.call_tool(
        "jerrycan_design",
        serde_json::json!({"requirements": "multi-module todo backend", "draft": draft}),
    );
    assert!(!err, "{payload}");
    assert_eq!(payload["status"], "complete");
    let design_path = payload["design_path"].as_str().unwrap().to_string();

    // 2. scaffold.
    let app = tmp.path().join("todo-api");
    let (err, payload) = c.call_tool(
        "jerrycan_scaffold",
        serde_json::json!({"design_path": design_path, "directory": app.to_str().unwrap()}),
    );
    assert!(!err, "{payload}");

    // 3. the "agent" implements the handlers (canned fixtures).
    for (fixture, target) in [
        ("todos_handlers.rs", "crates/routes/todos/src/handlers.rs"),
        (
            "comments_handlers.rs",
            "crates/routes/todos/src/subroutes/comments/handlers.rs",
        ),
        ("users_handlers.rs", "crates/routes/users/src/handlers.rs"),
    ] {
        std::fs::copy(
            repo_root().join("conformance/fixtures").join(fixture),
            app.join(target),
        )
        .unwrap();
    }

    // 4. verify: the full gate must be green.
    let (err, payload) = c.call_tool(
        "jerrycan_check",
        serde_json::json!({"directory": app.to_str().unwrap()}),
    );
    assert!(!err, "{payload}");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
    c.shutdown();

    // 5. serve and exercise the CRUD loop over real HTTP.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("JERRYCAN_ADDR", &addr)
        .args(["run", "-p", "app"])
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut connected = None;
    while std::time::Instant::now() < deadline {
        if let Ok(s) = std::net::TcpStream::connect(&addr) {
            connected = Some(s);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let http = |req: String| -> String {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };
    drop(connected.expect("generated app started serving within 120s"));

    let res = http("GET /todos/ HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.ends_with("[]"), "empty store first: {res}");

    let body = r#"{"title":"ship phase 1"}"#;
    let res = http(format!(
        "POST /todos/ HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    assert!(res.starts_with("HTTP/1.1 201"), "{res}");

    let res = http("GET /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(
        res.starts_with("HTTP/1.1 200") && res.contains("ship phase 1"),
        "{res}"
    );

    let res = http("GET /users/ HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 200"), "multi-module proof: {res}");

    let res = http("DELETE /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 204"), "{res}");

    let res = http("GET /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 404"), "{res}");

    let _ = server.kill();
    let _ = server.wait();
}

/// The Phase 2 TDD loop: gen-tests makes the design executable and FAILING,
/// the agent implements, the same tests go green, the gate stays green.
#[test]
#[ignore = "heavy: scaffold + gen-tests + red run + implement + green run"]
fn tdd_loop_goes_red_then_green_on_sqlite() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_db(tmp.path());

    // 1. Generate acceptance tests for both top-level modules.
    let mut expected_failing = 0usize;
    for module in ["todos", "users"] {
        let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .args(["--json", "gen-tests", "--module", module])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        expected_failing += payload["expected_failing"].as_u64().unwrap() as usize;
    }
    assert_eq!(expected_failing, 10, "todos 8 + users 2");

    // 2. RED: stubs must fail every acceptance test. `--no-fail-fast` so cargo
    // runs EVERY test binary (it otherwise halts at the first failing one, and
    // only the todos binary's `test result: FAILED.` line would be emitted).
    let out = Command::new("cargo")
        .current_dir(&app)
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "stub handlers must fail the acceptance suite"
    );
    let test_output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let failed: usize = test_output
        .lines()
        .filter_map(|l| {
            l.strip_prefix("test result: FAILED. ")?
                .split("; ")
                .nth(1)?
                .strip_suffix(" failed")
                .map(|n| n.parse::<usize>().unwrap_or(0))
        })
        .sum();
    assert_eq!(
        failed, expected_failing,
        "every generated test red:\n{test_output}"
    );

    // 3. The agent implements (db fixtures).
    for (fixture, target) in [
        (
            "db/todos_handlers.rs",
            "crates/routes/todos/src/handlers.rs",
        ),
        (
            "db/comments_handlers.rs",
            "crates/routes/todos/src/subroutes/comments/handlers.rs",
        ),
        (
            "db/users_handlers.rs",
            "crates/routes/users/src/handlers.rs",
        ),
    ] {
        let dest = app.join(target);
        std::fs::copy(
            repo_root().join("conformance/fixtures").join(fixture),
            &dest,
        )
        .unwrap();
        // RED's `cargo test` and this copy land in the same wall-clock second, so
        // bump the mtime forward — cargo's fingerprint is mtime-based and would
        // otherwise skip recompiling the changed handler and re-run the stubs.
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&dest)
            .unwrap()
            .set_modified(future)
            .unwrap();
    }

    // 4. GREEN: the same acceptance tests pass.
    let st = Command::new("cargo")
        .current_dir(&app)
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        st.success(),
        "implemented handlers must satisfy the design contract"
    );

    // 5. And the full gate holds.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
}
