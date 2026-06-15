//! Heavy conformance tests (#[ignore]): real cargo builds of generated apps.
//! Run with: cargo test -p jerrycan --test conformance -- --include-ignored

use std::io::{Read, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

/// The v2 north-star eval slice: a tenant-scoped, JWT-guarded, db-backed
/// sales-engagement backend (workspaces/leads/api-keys/billing). This is the
/// heavy gate proving the full SeaORM stack scaffolds, builds, and behaves.
const KOLLI: &str = include_str!("../../../conformance/designs/kolli-slice.design.json");

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

/// Scaffold the golden app in auth+observe mode (in-memory repos) against the
/// LOCAL framework with auth+observe features.
#[cfg(feature = "auth")]
fn scaffold_golden_auth(tmp: &Path) -> PathBuf {
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["auth", "observe"]);
    design["auth"] = serde_json::json!({ "model": "session", "roles": ["admin"] });
    for ep in design["modules"][0]["endpoints"].as_array_mut().unwrap() {
        if ep["operation_id"] == "create_todo" {
            ep["auth_required"] = serde_json::json!(true);
        }
        if ep["operation_id"] == "delete_todo" {
            ep["required_roles"] = serde_json::json!(["admin"]);
        }
    }
    // The comments subroute's create is mutating too: JL0004 demands every
    // mutating route in an auth design be guarded, so mark it auth_required.
    for ep in design["modules"][0]["subroutes"][0]["endpoints"]
        .as_array_mut()
        .unwrap()
    {
        if ep["operation_id"] == "create_comment" {
            ep["auth_required"] = serde_json::json!(true);
        }
    }
    for ep in design["modules"][1]["endpoints"].as_array_mut().unwrap() {
        if ep["operation_id"] == "create_user" {
            ep["auth_required"] = serde_json::json!(true);
        }
    }
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

/// Spec §4 Phase 3: an agent builds an AUTH-guarded, OBSERVED API. The generated
/// app must build, pass the full gate (JL0004 satisfied — every mutation guarded),
/// reject credential-less mutations with 401, accept admin-cookied ones, and
/// expose observe's /healthz and /metrics.
#[cfg(feature = "auth")]
#[test]
#[ignore = "heavy: auth+observe golden app builds, checks, and serves guarded routes"]
fn auth_observe_app_builds_checks_and_guards() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_auth(tmp.path());
    for (fixture, target) in [
        (
            "auth/todos_handlers.rs",
            "crates/routes/todos/src/handlers.rs",
        ),
        (
            "auth/comments_handlers.rs",
            "crates/routes/todos/src/subroutes/comments/handlers.rs",
        ),
        (
            "auth/users_handlers.rs",
            "crates/routes/users/src/handlers.rs",
        ),
    ] {
        std::fs::copy(
            repo_root().join("conformance/fixtures").join(fixture),
            app.join(target),
        )
        .unwrap();
    }

    // Full gate green (JL0004 must be satisfied — guarded mutations).
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

    // Serve and exercise guard behavior over real HTTP.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("JERRYCAN_ADDR", &addr)
        .env("JERRYCAN_SECRET", "a-very-long-development-secret-string!!")
        .args(["run", "-p", "app"])
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let http = |req: String| -> String {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };

    // Public list works without auth.
    assert!(
        http("GET /todos/ HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into())
            .starts_with("HTTP/1.1 200")
    );
    // Guarded create without a cookie → 401.
    let body = r#"{"title":"x","done":false}"#;
    let create = |cookie: &str| {
        format!(
            "POST /todos/ HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{body}",
            body.len(),
            cookie
        )
    };
    assert!(
        http(create("")).starts_with("HTTP/1.1 401"),
        "no cookie → 401"
    );
    // Mint an admin cookie with the same secret and create successfully. The app
    // has no /login route, so build the session cookie via jerrycan-auth in-test.
    let cookie = {
        let auth = jerrycan::auth::Auth::with_secret("a-very-long-development-secret-string!!");
        let token = auth
            .sessions()
            .encode(&serde_json::json!({ "id": 1, "role": "admin" }))
            .unwrap();
        format!("Cookie: jerrycan_session={token}\r\n")
    };
    assert!(
        http(create(&cookie)).starts_with("HTTP/1.1 201"),
        "admin cookie → 201"
    );
    // Observe endpoints live.
    assert_eq!(
        http("GET /healthz HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into())
            .lines()
            .next()
            .unwrap(),
        "HTTP/1.1 200 OK"
    );
    assert!(
        http("GET /metrics HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into())
            .contains("jerrycan_requests_total")
    );

    let _ = server.kill();
    let _ = server.wait();
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

/// Spec §11 Phase 2 exit: an agent builds a POSTGRES-backed API test-first,
/// all green. Runs wherever JERRYCAN_TEST_PG_URL points at a live Postgres
/// (CI service container); skips loudly elsewhere.
#[test]
#[ignore = "heavy: full TDD loop against live Postgres (JERRYCAN_TEST_PG_URL)"]
fn agent_builds_postgres_backed_api_test_first() {
    let Ok(pg_url) = std::env::var("JERRYCAN_TEST_PG_URL") else {
        eprintln!("SKIP: JERRYCAN_TEST_PG_URL not set (CI provides a postgres service)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_db(tmp.path());

    for module in ["todos", "users"] {
        let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .args(["gen-tests", "--module", module])
            .status()
            .unwrap();
        assert!(st.success());
    }
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
        std::fs::copy(
            repo_root().join("conformance/fixtures").join(fixture),
            app.join(target),
        )
        .unwrap();
    }

    // Apply migrations to the real Postgres, then serve against it and drive CRUD.
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["db", "migrate", "--url", &pg_url])
        .status()
        .unwrap();
    assert!(st.success(), "migrations must apply to live Postgres");

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("JERRYCAN_ADDR", &addr)
        .env("JERRYCAN_DATABASE_URL", &pg_url)
        .args(["run", "-p", "app"])
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let http = |req: String| -> String {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };

    let body = r#"{"title":"pg ship","done":false}"#;
    let res = http(format!(
        "POST /todos/ HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    assert!(res.starts_with("HTTP/1.1 201"), "{res}");
    let res = http("GET /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(
        res.starts_with("HTTP/1.1 200") && res.contains("pg ship"),
        "{res}"
    );
    let res = http("GET /openapi.json HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(
        res.starts_with("HTTP/1.1 200") && res.contains("3.1.0"),
        "validate extension live: {res}"
    );
    let res = http("DELETE /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 204"), "{res}");

    // Test-first, all green, against Postgres:
    let st = Command::new("cargo")
        .current_dir(&app)
        .env("JERRYCAN_DATABASE_URL", &pg_url)
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    let _ = server.kill();
    let _ = server.wait();
    assert!(
        st.success(),
        "acceptance suite must be green against Postgres"
    );
}

/// Spec §11 Phase 3 exit: the golden app deploys to Docker + k8s + bare server
/// from one command. Each leg is gated on its tool; missing tools SKIP that leg
/// loudly. The binary (bare-server) leg is unconditional.
///
/// DOCKER leg — pre-publish reality: the EMITTED `deploy/Dockerfile` does an
/// in-container `cargo build` of the generated app, which depends on `jerrycan`.
/// Today jerrycan is UNPUBLISHED (crates.io has only 0.0.0 reservations) and the
/// conformance scaffold wires a host PATH dep via `JERRYCAN_FRAMEWORK_DEP` that
/// lives OUTSIDE the `COPY . .` build context — so `docker build -f
/// deploy/Dockerfile` cannot fetch the framework and FAILS. The emitted
/// Dockerfile is correct for the post-0.1.0 world and stays the default artifact.
/// To PROVE the deploy-anywhere intent TODAY (a containerized jerrycan app serves
/// over HTTP) we build a THIN runtime image from the host-built binary instead:
/// the `--binary` artifact is copied into a minimal image and run. This needs a
/// Linux-runnable binary; on a non-Linux host the host binary is the host OS's
/// format and cannot run in a Linux container, so the docker leg SKIPs loudly.
#[test]
#[ignore = "heavy: package the golden app and prove binary/docker/k8s deploy paths"]
fn golden_app_deploys_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    // Reuse the memory-mode golden app (deploy paths are storage-agnostic).
    let app = scaffold_golden(tmp.path());
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

    // ONE command emits every artifact (after a green check gate).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args([
            "--json",
            "package",
            "--binary",
            "--docker",
            "--k8s",
            "--systemd",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "package failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let artifacts = payload["artifacts"].as_array().unwrap();
    for expected in [
        "deploy/Dockerfile",
        "deploy/k8s.yaml",
        "deploy/todo-api.service",
        "deploy/todo-api",
        "deploy/sbom.json",
    ] {
        assert!(
            artifacts.iter().any(|a| a == expected) || app.join(expected).exists(),
            "missing {expected}"
        );
    }
    // SBOM is valid CycloneDX.
    let sbom: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(app.join("deploy/sbom.json")).unwrap())
            .unwrap();
    assert_eq!(sbom["bomFormat"], "CycloneDX");
    assert!(
        sbom["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "tokio")
    );

    // BARE SERVER leg: run the built binary directly, curl it.
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");
    let mut bin = Command::new(app.join("deploy/todo-api"))
        .env("JERRYCAN_ADDR", &addr)
        .spawn()
        .expect("packaged binary runs");
    await_listen(&addr, 60);
    assert!(
        http_get(&addr, "/todos/").starts_with("HTTP/1.1 200"),
        "bare binary serves"
    );
    let _ = bin.kill();
    let _ = bin.wait();

    // DOCKER leg (gated): build a THIN runtime image from the host-built binary
    // and run it (see the function doc for why we don't use the emitted, publish-
    // gated, in-container-build Dockerfile here). Needs docker AND a Linux host
    // (the host binary must be executable inside a Linux container).
    if !tool_present("docker") {
        eprintln!("SKIP docker leg: docker not present");
    } else if std::env::consts::OS != "linux" {
        eprintln!(
            "SKIP docker leg: host is {} — the host-built binary is not a Linux \
             executable and cannot run in a Linux container (CI proves this leg on Linux)",
            std::env::consts::OS
        );
    } else {
        // distroless/static needs a fully static (musl) binary; a dynamically
        // linked gnu binary needs a glibc base. Pick the base to match.
        let base = if musl_built(&app) {
            "gcr.io/distroless/static:nonroot"
        } else {
            "debian:stable-slim"
        };
        let test_dockerfile = format!(
            "FROM {base}\nCOPY deploy/todo-api /usr/local/bin/todo-api\n\
             EXPOSE 8000\nENV JERRYCAN_ADDR=0.0.0.0:8000\n\
             ENTRYPOINT [\"/usr/local/bin/todo-api\"]\n"
        );
        std::fs::write(app.join("Dockerfile.thin"), &test_dockerfile).unwrap();
        let tag = "jerrycan-conformance:test";
        let build = Command::new("docker")
            .current_dir(&app)
            .args(["build", "-f", "Dockerfile.thin", "-t", tag, "."])
            .status()
            .unwrap();
        assert!(build.success(), "thin-image docker build");
        let port = pick_port();
        let run = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "-p",
                &format!("{port}:8000"),
                "--name",
                "jerrycan-conformance",
                tag,
            ])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "docker run: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let addr = format!("127.0.0.1:{port}");
        await_listen(&addr, 60);
        let body = http_get(&addr, "/todos/");
        let _ = Command::new("docker")
            .args(["stop", "jerrycan-conformance"])
            .status();
        let _ = Command::new("docker").args(["rmi", "-f", tag]).status();
        assert!(
            body.starts_with("HTTP/1.1 200"),
            "containerized app serves: {body}"
        );
    }

    // K8S leg (gated): validate the manifests parse + are structurally appl-able.
    // `kubectl apply --dry-run=client` still performs API-resource discovery
    // against the cluster (it queries `/api` to map kinds), so it needs a
    // reachable cluster — `--dry-run=client` is NOT cluster-free. We therefore
    // gate on cluster reachability, not merely on kubectl being installed
    // (`kubectl --version` is also not a valid flag — probe `version --client`).
    if kubectl_present() && cluster_reachable() {
        let out = Command::new("kubectl")
            .current_dir(&app)
            .args(["apply", "--dry-run=client", "-f", "deploy/k8s.yaml"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "kubectl dry-run: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        // Structural fallback: every YAML doc parses and has kind+apiVersion.
        let y = std::fs::read_to_string(app.join("deploy/k8s.yaml")).unwrap();
        let docs: Vec<&str> = y.split("\n---\n").collect();
        assert_eq!(docs.len(), 3, "Deployment + Service + NetworkPolicy");
        for d in docs {
            assert!(
                d.contains("apiVersion:") && d.contains("kind:"),
                "valid manifest doc"
            );
        }
        eprintln!(
            "SKIP kubectl dry-run: no reachable cluster — used structural manifest validation"
        );
    }
}

/// THE v2 north-star gate: the kolli-slice design — a tenant-scoped, JWT-guarded,
/// db-backed multi-module backend (workspaces/leads/api-keys/billing) — scaffolds
/// onto the full SeaORM stack, the generated workspace BUILDS, its generated
/// acceptance + isolation tests run and fail ONLY on unimplemented stubs (JC0500),
/// and the lighter check gates (jerrycan lints + schema-contract freshness) are
/// clean. We deliberately skip the heaviest gates here (cargo-audit/cargo-deny are
/// exercised by the db-mode golden test); this gate's job is to pin the SeaORM
/// compile-tax baseline and the red-test shape on the real eval design.
///
/// WHY the stub-class assertion matters (Rule 9): a pre-implementation scaffold
/// MUST go red because the handlers are unimplemented — every red is a JC0500
/// "not implemented" stub. If a red were instead a 401/403/422, the *generator*
/// would be wiring the wrong status (a guard misfire or a validation false-reject)
/// onto a request the test intends to succeed — a real bug. So this gate fails
/// loudly if any acceptance failure carries a non-500 status, while the
/// `*_without_auth_is_401` guard tests are expected to PASS (the guard runs before
/// the stub, so a credential-less request is correctly rejected pre-implementation).
#[test]
#[ignore = "heavy: kolli-slice (SeaORM) scaffolds, builds, reds-on-stubs; records cold-build baseline"]
fn kolli_slice_scaffold_passes_check() {
    let tmp = tempfile::tempdir().unwrap();

    // Scaffold the kolli-slice design wired to the LOCAL framework path dep, the
    // same way every other heavy test wires it (env passed to the child only).
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, KOLLI).unwrap();
    let app = tmp.path().join("kolli-slice");
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
    assert!(st.success(), "kolli-slice must scaffold");

    // schema.json is written by the db-mode scaffold (derived from migrations).
    assert!(
        app.join("schema.json").exists(),
        "db-mode scaffold must emit schema.json"
    );

    // gen-tests for every top-level module — mirrors the binary invocation the
    // other heavy tests use. Each emits a failing acceptance suite (stubs).
    let mut expected_failing = 0usize;
    for module in [
        "users",
        "workspaces",
        "leads",
        "api-keys",
        "billing",
        "integrations",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .args(["--json", "gen-tests", "--module", module])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "gen-tests {module} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        expected_failing += payload["expected_failing"].as_u64().unwrap() as usize;
    }
    assert!(
        expected_failing > 0,
        "the design must generate failing acceptance tests"
    );

    // COLD BUILD baseline: time the generated workspace's first build. The tempdir
    // is its own target root, so this is a genuine from-scratch SeaORM compile —
    // print it so CI logs carry the v2 compile-tax baseline.
    let t0 = std::time::Instant::now();
    let build = Command::new("cargo")
        .current_dir(&app)
        .args(["build", "--workspace"])
        .output()
        .unwrap();
    let cold_build = t0.elapsed();
    assert!(
        build.status.success(),
        "kolli-slice (SeaORM) generated workspace must build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    eprintln!("kolli-slice cold build: {cold_build:?}");

    // INCREMENTAL test-build baseline: build (don't run) the route-leads test
    // binary now that deps are warm — the tightest agent inner-loop signal. `cargo
    // test --no-run` compiles the test target without executing it.
    let t1 = std::time::Instant::now();
    let leads_build = Command::new("cargo")
        .current_dir(&app)
        .args(["test", "-p", "route-leads", "--no-run"])
        .output()
        .unwrap();
    let leads_test_build = t1.elapsed();
    assert!(
        leads_build.status.success(),
        "route-leads test binary must compile:\n{}",
        String::from_utf8_lossy(&leads_build.stderr)
    );
    eprintln!("kolli-slice route-leads incremental test-build: {leads_test_build:?}");

    // RED on stubs: run every generated test. `--no-fail-fast` so cargo runs all
    // test binaries (it otherwise halts at the first failing crate).
    let out = Command::new("cargo")
        .current_dir(&app)
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "pre-implementation stubs must fail the acceptance suite"
    );
    let test_output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Every red MUST be a JC0500 stub. A test failure prints the asserted-against
    // status as `  left: <code>` (the observed status) above a `right:` (the
    // designed status). Walk every observed status from a failed assertion and
    // require it to be 500 — a 401/403/422 here would mean the generator mis-wired
    // a guard or a validator onto a request the test means to succeed (a REAL bug).
    let observed: Vec<u16> = test_output
        .lines()
        .filter_map(|l| l.trim().strip_prefix("left: "))
        .filter_map(|n| n.trim().parse::<u16>().ok())
        .collect();
    assert!(
        !observed.is_empty(),
        "expected failed-assertion `left:` lines in:\n{test_output}"
    );
    let non_stub: Vec<u16> = observed.iter().copied().filter(|s| *s != 500).collect();
    assert!(
        non_stub.is_empty(),
        "acceptance failures must be ONLY JC0500 stubs (500); found non-stub \
         observed statuses {non_stub:?} — a guard/validation false-failure is a \
         generator bug:\n{test_output}"
    );
    // And the only JC#### code surfacing in any failure body is the stub code: a
    // JC0422 (validation) in a failure body would be a false-reject of a body the
    // generator itself built from the design fixtures.
    assert!(
        !test_output.contains("JC0422"),
        "no acceptance failure may carry JC0422 (validation false-reject):\n{test_output}"
    );
    // The guard tests must be PRESENT and PASSING — proof the JWT guard runs ahead
    // of the stub (a credential-less mutation is correctly 401 pre-implementation).
    assert!(
        test_output.contains("_without_auth_is_401 ... ok"),
        "guard tests must pass (guard precedes the stub):\n{test_output}"
    );
    assert!(
        !test_output.contains("_without_auth_is_401 ... FAILED"),
        "a guard test must never fail — the guard precedes the stub:\n{test_output}"
    );

    // The lighter check gates, run directly (audit/deny are too heavy here and are
    // covered by the db-mode golden test): jerrycan lints and schema-contract
    // freshness must both be clean on the fresh scaffold.
    let design: jerrycan::platform::design::Design = serde_json::from_str(KOLLI).unwrap();
    let lints = jerrycan::platform::lints::run(&app, &design);
    assert!(
        lints.is_empty(),
        "jerrycan lints must be clean on a fresh scaffold: {lints:?}"
    );
    let schema_drift = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(jerrycan::platform::schema::verify_fresh(&app, &design))
        .expect("schema derivation must succeed");
    assert!(
        schema_drift.is_empty(),
        "scaffolded schema.json must match a fresh derivation: {schema_drift:?}"
    );
}

// Small helpers for the deploy-anywhere test (no earlier-phase equivalents exist
// in this file; the auth_observe test inlines its own closures).
fn pick_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn tool_present(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
/// kubectl rejects `--version`; its client-only probe is `version --client`.
fn kubectl_present() -> bool {
    Command::new("kubectl")
        .args(["version", "--client"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
/// `kubectl apply --dry-run=client` still discovers API resources from the
/// cluster, so the dry-run leg only runs when a cluster is reachable.
fn cluster_reachable() -> bool {
    Command::new("kubectl")
        .args(["cluster-info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
fn await_listen(addr: &str, secs: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    panic!("nothing listening on {addr} after {secs}s");
}
fn http_get(addr: &str, path: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
/// True when `jerrycan package --binary` produced a static musl binary (so a
/// distroless/static runtime base is appropriate); false ⇒ a gnu host binary.
fn musl_built(app: &Path) -> bool {
    app.join("target/x86_64-unknown-linux-musl/release/app")
        .exists()
}
