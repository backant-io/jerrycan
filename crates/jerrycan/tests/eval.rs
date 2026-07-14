//! Deterministic agent-loop eval: for each reference spec, scaffold → apply the
//! reference fixtures → check → serve → smoke a request. Scores pass/fail per
//! spec. This is the CI signal that the loop works across designs; the real-LLM
//! eval (conformance/eval/results.md) is a dispatched agent.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}
fn jc() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_jerrycan"))
}
fn framework_dep() -> String {
    format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    )
}

const SPECS: &[&str] = &["blog", "tasks", "shortener", "inventory", "notes"];

#[test]
#[ignore = "heavy: scaffolds, builds, checks, and serves 5 reference apps"]
fn scripted_agent_loop_builds_all_reference_apps() {
    let mut passed = 0;
    let mut report = String::new();
    for spec in SPECS {
        match run_one(spec) {
            Ok(()) => {
                passed += 1;
                report.push_str(&format!("PASS {spec}\n"));
            }
            Err(e) => report.push_str(&format!("FAIL {spec}: {e}\n")),
        }
    }
    eprintln!("scripted eval:\n{report}");
    assert_eq!(
        passed,
        SPECS.len(),
        "all reference apps must build+check+serve:\n{report}"
    );
}

fn run_one(spec: &str) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let app = tmp.path().join(spec);
    let design = repo_root().join(format!("conformance/eval/specs/{spec}.design.json"));

    // scaffold
    let st = Command::new(jc())
        .env("JERRYCAN_FRAMEWORK_DEP", framework_dep())
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("scaffold failed".into());
    }

    // apply reference fixtures: copy each <module>_handlers.rs to its route crate
    let fixtures = repo_root().join(format!("conformance/eval/fixtures/{spec}"));
    for entry in std::fs::read_dir(&fixtures).map_err(|e| format!("fixtures dir: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        // e.g. posts_handlers.rs OR posts__comments_handlers.rs
        let fname = entry.file_name().to_string_lossy().to_string();
        // map "<module>_handlers.rs" → crates/routes/<module>/src/handlers.rs
        // map "<module>__<sub>_handlers.rs" → crates/routes/<module>/src/subroutes/<sub>/handlers.rs
        let target = handler_target(&app, &fname)?;
        std::fs::create_dir_all(target.parent().unwrap()).ok();
        std::fs::copy(entry.path(), &target).map_err(|e| format!("copy {fname}: {e}"))?;
    }

    // check (full gate)
    let out = Command::new(jc())
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .map_err(|e| e.to_string())?;
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("check json: {e}"))?;
    if payload["ok"] != true {
        return Err(format!("check failed: {}", payload["diagnostics"]));
    }

    // serve + smoke one request to the first listed route
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .env("JERRYCAN_ADDR", &addr)
        .args(["run", "-p", "app"])
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut up = false;
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let result = if up {
        let routes = Command::new(jc())
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["--json", "list", "routes"])
            .output()
            .unwrap();
        let rv: serde_json::Value = serde_json::from_slice(&routes.stdout).unwrap();
        let first = rv["routes"][0]["path"].as_str().unwrap_or("/").to_string();
        let body = http_get(&addr, &first);
        if !(body.starts_with("HTTP/1.1 2") || body.starts_with("HTTP/1.1 404")) {
            Err(format!("serve smoke bad status: {body}"))
        } else if spec == "tasks" {
            // Persist-smoke: prove the PUT handler actually writes through the repo.
            // create → update → get must return the *updated* value, not the original.
            persist_smoke(&addr)
        } else {
            Ok(())
        }
    } else {
        Err("app did not start".into())
    };
    let _ = server.kill();
    let _ = server.wait();
    result
}

fn handler_target(app: &Path, fixture_name: &str) -> Result<PathBuf, String> {
    let stem = fixture_name
        .strip_suffix("_handlers.rs")
        .ok_or("bad fixture name")?;
    let base = app.join("crates/routes");
    if let Some((module, sub)) = stem.split_once("__") {
        Ok(base
            .join(module)
            .join("src/subroutes")
            .join(sub)
            .join("handlers.rs"))
    } else {
        Ok(base.join(stem).join("src/handlers.rs"))
    }
}

fn http_get(addr: &str, path: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn http_request(addr: &str, method: &str, path: &str, json_body: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json_body}",
        json_body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// create → update → get against the `tasks` app, asserting the PUT persists.
/// The first inserted task gets id 1 (in-memory repo seeds next_id at 1).
fn persist_smoke(addr: &str) -> Result<(), String> {
    let created = http_request(addr, "POST", "/tasks/", r#"{"title":"first","done":false}"#);
    if !created.starts_with("HTTP/1.1 201") {
        return Err(format!("persist-smoke create not 201: {created}"));
    }
    let updated = http_request(
        addr,
        "PUT",
        "/tasks/1",
        r#"{"title":"renamed","done":true}"#,
    );
    if !updated.starts_with("HTTP/1.1 200") {
        return Err(format!("persist-smoke update not 200: {updated}"));
    }
    let fetched = http_get(addr, "/tasks/1");
    if !fetched.starts_with("HTTP/1.1 200") {
        return Err(format!("persist-smoke get not 200: {fetched}"));
    }
    // The mutation must be visible in the GET body, not silently dropped.
    if !fetched.contains("renamed") {
        return Err(format!(
            "persist-smoke: PUT did not persist; GET still shows the old value: {fetched}"
        ));
    }
    Ok(())
}
