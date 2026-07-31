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
    // #118: give this app a unique runnable-bin name so its `debug/app_<uid>` never
    // collides with a concurrently-built sibling in the shared target dir.
    common::isolate_app_bin(&app);

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

    // gen-tests per top-level module (#123a: the honest check gate refuses a
    // never-gen-tested module with JC0551, and the generated acceptance suite
    // it then runs must be green on the reference fixtures).
    let design: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(app.join("design.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("design json: {e}"))?;
    for m in design["modules"]
        .as_array()
        .ok_or("design has no modules")?
    {
        let name = m["name"].as_str().ok_or("module without a name")?;
        let st = Command::new(jc())
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["gen-tests", "--module", name])
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("gen-tests {name} failed"));
        }
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

// ===================== #118 concurrent-build contamination guard =====================

struct ServeableApp {
    // Held so the scaffold source dir survives through build + serve; dropped
    // (and cleaned) at the end of the test.
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    bin: String,
}

/// Scaffold a reference spec, give it a UNIQUE runnable-bin name (the #118 fix),
/// and drop in its reference handler fixtures so its list routes answer 200.
fn prepare_serveable_app(spec: &str) -> ServeableApp {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join(spec);
    let design = repo_root().join(format!("conformance/eval/specs/{spec}.design.json"));
    let st = Command::new(jc())
        .env("JERRYCAN_FRAMEWORK_DEP", framework_dep())
        .arg("new")
        .arg(&dir)
        .arg("--design")
        .arg(&design)
        .status()
        .expect("scaffold");
    assert!(st.success(), "{spec} must scaffold");
    let bin = common::isolate_app_bin(&dir);
    let fixtures = repo_root().join(format!("conformance/eval/fixtures/{spec}"));
    for entry in std::fs::read_dir(&fixtures).expect("fixtures dir") {
        let entry = entry.expect("fixture entry");
        let fname = entry.file_name().to_string_lossy().to_string();
        let target = handler_target(&dir, &fname).expect("handler target");
        std::fs::create_dir_all(target.parent().unwrap()).ok();
        std::fs::copy(entry.path(), &target).expect("copy fixture");
    }
    ServeableApp {
        _tmp: tmp,
        dir,
        bin,
    }
}

/// Build the app's runnable bin into the SHARED target (the contended dir).
fn build_app(dir: &Path) {
    let st = Command::new("cargo")
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["build", "-p", "app"])
        .status()
        .expect("cargo build -p app");
    assert!(st.success(), "app in {} must build", dir.display());
}

/// Serve a previously-built app by EXECUTING its resolved artifact directly (not
/// `cargo run`, which would re-uplift `debug/app` and mask a stale-binary collision).
fn serve_bin(bin: &Path, dir: &Path) -> (std::process::Child, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(bin)
        .current_dir(dir)
        .env("JERRYCAN_ADDR", &addr)
        .spawn()
        .expect("serve app binary");
    (child, addr)
}

fn await_up(addr: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    panic!("server at {addr} did not come up");
}

fn assert_route(addr: &str, path: &str, want_status: &str, why: &str) {
    let body = http_get(addr, path);
    let head = body.lines().next().unwrap_or("");
    assert!(
        head.starts_with(&format!("HTTP/1.1 {want_status}")),
        "{why}: expected {want_status} at {addr}{path}, got: {head}"
    );
}

/// #118 regression: two DIFFERENT designs, built and served CONCURRENTLY through
/// the harness into the SHARED target, must each serve ONLY their own routes.
/// `blog` exposes `/authors/` (which `notes` lacks); `notes` exposes `/notes/`
/// (which `blog` lacks). Before the fix both apps were package `app` with a bin
/// also named `app`, so both uplifted to the SAME `.../debug/app`; whichever built
/// last clobbered the other, and serving the "first" app then exec'd the second's
/// binary — the foreign route answered (a cross-app bleed that cost real debug
/// turns on round-5 apps). `common::isolate_app_bin` gives each app its own
/// `debug/app_<uid>`, so there is no shared mutable artifact path to clobber.
///
/// Determinism: build both into the shared target, then serve each by its RESOLVED
/// artifact (`debug/<bin>`) rather than via `cargo run` (which re-uplifts and would
/// mask the hazard). Pre-fix both resolve to `debug/app` — one binary answering on
/// both ports — so a cross-route check fails on EVERY run; post-fix each resolves
/// to its own `debug/app_<uid>` and all four checks pass.
#[test]
#[ignore = "heavy: scaffold+build+serve two designs concurrently (#118 contamination guard)"]
fn concurrent_distinct_apps_do_not_contaminate() {
    let a = prepare_serveable_app("blog");
    let b = prepare_serveable_app("notes");

    // The fix's core invariant: each app got a UNIQUE bin name, so the shared target
    // can never funnel both into one `debug/app`. (Pre-fix both are bin `app` →
    // equal → this fails before we even serve.)
    assert_ne!(
        a.bin, b.bin,
        "each scaffolded app must get a unique bin name (#118)"
    );

    // Build BOTH into the shared target concurrently — the contention the bug needs.
    std::thread::scope(|s| {
        s.spawn(|| build_app(&a.dir));
        s.spawn(|| build_app(&b.dir));
    });

    // Post-fix the two artifacts are DISTINCT files; pre-fix only `debug/app` exists.
    let debug = common::shared_app_target().join("debug");
    let bin_a = debug.join(&a.bin);
    let bin_b = debug.join(&b.bin);
    assert!(
        bin_a.exists(),
        "blog artifact {} must exist",
        bin_a.display()
    );
    assert!(
        bin_b.exists(),
        "notes artifact {} must exist",
        bin_b.display()
    );

    // Serve both concurrently by their resolved artifacts and cross-check routes.
    let (mut sa, addr_a) = serve_bin(&bin_a, &a.dir);
    let (mut sb, addr_b) = serve_bin(&bin_b, &b.dir);
    let checks = std::panic::catch_unwind(|| {
        await_up(&addr_a);
        await_up(&addr_b);
        // blog answers its OWN /authors/ (200) and 404s notes' /notes/.
        assert_route(&addr_a, "/authors/", "200", "blog serves its own /authors/");
        assert_route(
            &addr_a,
            "/notes/",
            "404",
            "blog must NOT serve notes' /notes/ (cross-app bleed)",
        );
        // notes answers its OWN /notes/ (200) and 404s blog's /authors/.
        assert_route(&addr_b, "/notes/", "200", "notes serves its own /notes/");
        assert_route(
            &addr_b,
            "/authors/",
            "404",
            "notes must NOT serve blog's /authors/ (cross-app bleed)",
        );
    });
    let _ = sa.kill();
    let _ = sa.wait();
    let _ = sb.kill();
    let _ = sb.wait();
    if let Err(p) = checks {
        std::panic::resume_unwind(p);
    }
}
