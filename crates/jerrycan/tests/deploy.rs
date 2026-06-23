//! `jerrycan deploy <target>` generates a self-contained deploy kit.
use jerrycan::platform::{deploy, design::Design};
use std::io::Write;
use std::process::Command;

fn demo_design() -> Design {
    serde_json::from_str(
        r#"{ "name": "Acme API", "contract_version": 1, "dependencies": ["db"],
             "modules": [{ "name": "items", "entities": [
               { "name": "Item", "fields": [{ "name": "title", "type": "string" }] }],
               "endpoints": [{ "operation_id": "list_items", "method": "GET", "path": "/",
                 "success": { "status": 200, "entity": "Item", "list": true } }] }] }"#,
    )
    .unwrap()
}

#[test]
fn render_target_emits_the_four_artifacts_with_the_app_slug() {
    let design = demo_design();
    let artifacts = deploy::emit("render", &design).expect("render target");
    let paths: Vec<&str> = artifacts.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "deploy/render/deploy.sh",
            "deploy/render/teardown.sh",
            "deploy/render/render.yaml",
            "deploy/render/README.md",
        ],
        "stable artifact set + order"
    );
    let deploy_sh = &artifacts[0].1;
    // The app name is slugified into the resource base name.
    assert!(
        deploy_sh.contains("acme-api"),
        "app slug substituted: {deploy_sh}"
    );
    assert!(!deploy_sh.contains("Acme API"), "raw name not leaked");
}

#[test]
fn unknown_target_is_an_error() {
    let err = deploy::emit("heroku", &demo_design()).unwrap_err();
    assert!(err.contains("unknown deploy target"), "{err}");
    assert!(err.contains("render"), "lists the supported targets: {err}");
}

#[test]
fn render_yaml_declares_a_web_service_a_db_and_secret_envs() {
    let artifacts = deploy::emit("render", &demo_design()).unwrap();
    let yaml = &artifacts[2].1; // render.yaml
    assert!(yaml.contains("type: web_service"), "{yaml}");
    assert!(yaml.contains("name: acme-api"), "{yaml}");
    assert!(yaml.contains("healthCheckPath: /healthz"), "{yaml}");
    assert!(
        yaml.contains("databases:") && yaml.contains("name: acme-api-db"),
        "{yaml}"
    );
    // JERRYCAN_SECRET is generateValue (Render generates + stores it), never inline.
    assert!(
        yaml.contains("key: JERRYCAN_SECRET") && yaml.contains("generateValue: true"),
        "{yaml}"
    );
    assert!(
        yaml.contains("key: JERRYCAN_ENV") && yaml.contains("value: prod"),
        "{yaml}"
    );
}

#[test]
fn deploy_sh_has_the_secure_idempotent_flow() {
    let artifacts = deploy::emit("render", &demo_design()).unwrap();
    let sh = &artifacts[0].1;
    // Strict bash.
    assert!(sh.starts_with("#!/usr/bin/env bash\n"), "{sh}");
    assert!(sh.contains("set -euo pipefail"), "{sh}");
    // Requires the key; supports the test/advanced overrides.
    assert!(sh.contains("RENDER_API_KEY"), "{sh}");
    assert!(sh.contains("RENDER_API_BASE"), "override for tests: {sh}");
    assert!(sh.contains("JERRYCAN_DEPLOY_SKIP_BUILD"), "test hook: {sh}");
    // Preflight, postgres, secret-gen, service, deploy-poll, idempotency.
    assert!(sh.contains("/v1/owners"), "preflight: {sh}");
    assert!(sh.contains("/v1/postgres"), "{sh}");
    assert!(sh.contains("connection-info"), "{sh}");
    assert!(
        sh.contains("openssl rand"),
        "generates JERRYCAN_SECRET: {sh}"
    );
    assert!(sh.contains("JERRYCAN_ENV") && sh.contains("prod"), "{sh}");
    assert!(sh.contains("/v1/services"), "{sh}");
    assert!(
        sh.contains("healthCheckPath") || sh.contains("/healthz"),
        "{sh}"
    );
    assert!(
        sh.contains("find_or_create") || sh.contains("?name="),
        "idempotent: {sh}"
    );
    // Secrets are redacted from output (never echo the secret value).
    assert!(
        sh.contains("redact") || sh.contains("***"),
        "redaction: {sh}"
    );
    // No literal secret value is ever printed.
    assert!(
        !sh.contains("echo \"$JERRYCAN_SECRET\""),
        "must not echo the secret: {sh}"
    );
    // Writes resource ids (not secrets) for idempotent re-run + teardown.
    assert!(sh.contains(".deploy-state.json"), "{sh}");
}

fn shellcheck(script: &str) {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP shellcheck (not installed)");
        return;
    }
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(script.as_bytes()).unwrap();
    let out = Command::new("shellcheck")
        .args(["-S", "warning"]) // warnings + errors fail
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "shellcheck:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn generated_scripts_pass_shellcheck() {
    let a = deploy::emit("render", &demo_design()).unwrap();
    shellcheck(&a[0].1); // deploy.sh
    shellcheck(&a[1].1); // teardown.sh
}

#[test]
fn teardown_deletes_the_service_and_db_with_a_guard() {
    let a = deploy::emit("render", &demo_design()).unwrap();
    let td = &a[1].1;
    assert!(
        td.starts_with("#!/usr/bin/env bash\n") && td.contains("set -euo pipefail"),
        "{td}"
    );
    assert!(
        td.contains("DELETE") && td.contains("/v1/services/"),
        "{td}"
    );
    assert!(td.contains("/v1/postgres/"), "{td}");
    assert!(td.contains(".deploy-state.json"), "reads stored ids: {td}");
    assert!(
        td.to_lowercase().contains("destroy") || td.contains("read -r"),
        "confirmation guard: {td}"
    );
}

#[test]
fn readme_documents_key_prereq_secrets_and_teardown() {
    let a = deploy::emit("render", &demo_design()).unwrap();
    let r = &a[3].1;
    assert!(r.contains("RENDER_API_KEY"), "{r}");
    assert!(
        r.to_lowercase().contains("registry") && r.contains("JERRYCAN_DEPLOY_SKIP_BUILD"),
        "registry prereq: {r}"
    );
    assert!(
        r.contains("JERRYCAN_SECRET") && r.contains("JERRYCAN_SECRET_OLD"),
        "rotation runbook: {r}"
    );
    assert!(r.contains("teardown.sh"), "{r}");
    assert!(
        r.to_lowercase().contains("least") || r.to_lowercase().contains("scope"),
        "token scope: {r}"
    );
}

#[test]
fn cmd_deploy_writes_artifacts_and_gitignores_the_state_file() {
    // The CLI writes the kit into deploy/render/ and appends the state-file path
    // to .gitignore so the (id-only) deploy state never lands in version control.
    let tmp = tempfile::tempdir().unwrap();
    let design = r#"{ "name": "Acme API", "contract_version": 1, "dependencies": [],
        "modules": [{ "name": "items", "endpoints": [
          { "operation_id": "list_items", "method": "GET", "path": "/",
            "success": { "status": 200 } }] }] }"#;
    std::fs::write(tmp.path().join("design.json"), design).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(tmp.path())
        .args(["deploy", "render"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "deploy failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for rel in [
        "deploy/render/deploy.sh",
        "deploy/render/teardown.sh",
        "deploy/render/render.yaml",
        "deploy/render/README.md",
    ] {
        assert!(tmp.path().join(rel).exists(), "missing artifact {rel}");
    }

    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(
        gitignore
            .lines()
            .any(|l| l.trim() == "deploy/render/.deploy-state.json"),
        "state file not gitignored: {gitignore}"
    );

    // Re-running must not duplicate the gitignore line (idempotent append).
    let out2 = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(tmp.path())
        .args(["deploy", "render"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let gitignore2 = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    let count = gitignore2
        .lines()
        .filter(|l| l.trim() == "deploy/render/.deploy-state.json")
        .count();
    assert_eq!(count, 1, "gitignore line duplicated: {gitignore2}");
}

// --- security regression: the API error path must never leak a secret --------

/// A known DB password the stub plants in the connection-info response. The
/// script puts it into `JERRYCAN_DATABASE_URL` (a `postgres://…` URL) and sends
/// it inside the `POST /v1/services` request body. The stub then returns 400
/// echoing that body — exactly the Render-validation-error shape that leaked the
/// secret before the fix. The test asserts this token never reaches the output.
const STUB_DB_PASSWORD: &str = "S3cr3tDbPassw0rdDoNotLeak";

/// A stub Render API that drives the generated `deploy.sh` to the
/// `POST /v1/services` step, then returns **HTTP 400 echoing the request body**
/// (which carries `JERRYCAN_SECRET` + the `postgres://` DB URL). Returns the
/// host-root base URL (no `/v1` — the script's paths already include it).
fn spawn_leaky_render_stub() -> String {
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            let (method, path) = (parts[0].to_string(), parts[1].to_string());
            // Drain headers; capture Content-Length to read the body.
            let mut clen = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                if h == "\r\n" || h == "\n" {
                    break;
                }
                if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                    clen = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; clen];
            if clen > 0 {
                r.read_exact(&mut body).ok();
            }
            let req_body = String::from_utf8_lossy(&body).to_string();

            let p = path.split('?').next().unwrap();
            // Reach the service-create step with canned 200s; the DB URL the
            // script forwards carries STUB_DB_PASSWORD (the leak under test).
            let (status, resp): (&str, String) = match (method.as_str(), p) {
                ("GET", "/v1/owners") => ("200 OK", r#"[{"owner":{"id":"own_1"}}]"#.into()),
                ("GET", "/v1/postgres") => ("200 OK", "[]".into()),
                ("POST", "/v1/postgres") => {
                    ("200 OK", r#"{"id":"pg_1","status":"creating"}"#.into())
                }
                ("GET", "/v1/postgres/pg_1") => ("200 OK", r#"{"status":"available"}"#.into()),
                ("GET", "/v1/postgres/pg_1/connection-info") => (
                    "200 OK",
                    format!(
                        r#"{{"internalConnectionString":"postgres://u:{STUB_DB_PASSWORD}@h:5432/d"}}"#
                    ),
                ),
                ("GET", "/v1/services") => ("200 OK", "[]".into()),
                // The leak: a Render validation 400 that mirrors the request body
                // — which embeds JERRYCAN_SECRET + the postgres:// DB URL.
                ("POST", "/v1/services") => (
                    "400 Bad Request",
                    format!(r#"{{"message":"validation failed","request":{req_body}}}"#),
                ),
                _ => ("200 OK", "{}".into()),
            };
            let http = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
            use std::io::Write as _;
            s.write_all(http.as_bytes()).ok();
        }
    });
    addr
}

#[test]
fn deploy_sh_error_path_never_leaks_the_secret() {
    // Run the REAL generated deploy.sh against a stub whose POST /v1/services
    // returns HTTP 400 echoing the request body (the secret + DB URL live in that
    // body). The script must fail (non-zero) AND no secret may reach stdout/stderr.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("deploy").join("render");
    std::fs::create_dir_all(&dir).unwrap();
    let a = deploy::emit("render", &demo_design()).unwrap();
    let script = dir.join("deploy.sh");
    std::fs::write(&script, &a[0].1).unwrap();

    let base = spawn_leaky_render_stub();
    // RENDER_API_BASE is the HOST ROOT (the script paths already carry /v1).
    let out = Command::new("bash")
        .arg(&script)
        .env("RENDER_API_KEY", "rnd_testkey")
        .env("RENDER_API_BASE", &base)
        .env("JERRYCAN_DEPLOY_SKIP_BUILD", "1")
        .env("JERRYCAN_DEPLOY_IMAGE", "registry/test/acme-api")
        .env("JERRYCAN_DEPLOY_TAG", "test")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // 1. The 400 on the secret-carrying call must FAIL the deploy.
    assert!(
        !out.status.success(),
        "deploy.sh should fail when POST /v1/services returns 400.\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );

    // 2. The DB password (a postgres:// URL component the script forwarded in the
    //    request body) must NOT appear anywhere — neither raw nor inside a URL.
    assert!(
        !combined.contains(STUB_DB_PASSWORD),
        "DB password leaked into output:\n{combined}"
    );
    assert!(
        !combined.contains("postgres://"),
        "a postgres:// URL (DB credentials) reached the output:\n{combined}"
    );

    // 3. The generated JERRYCAN_SECRET is `openssl rand -base64 48` → a 64-char
    //    base64 blob. No base64-ish run of 40+ chars may survive into the output.
    let leaked_base64 = combined
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
        .any(|tok| {
            let core = tok.trim_end_matches('=');
            core.len() >= 40
                && core
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
        });
    assert!(
        !leaked_base64,
        "a base64-ish secret blob (40+ chars) reached the output:\n{combined}"
    );

    // 4. The error line for the service call must be body-free: it names the
    //    method+path+code but withholds the (secret-bearing) response body.
    assert!(
        stderr.contains("POST /v1/services -> 400"),
        "expected a body-free 'POST /v1/services -> 400' error line:\n{stderr}"
    );
    assert!(
        !combined.contains("validation failed") || !combined.contains("\"request\""),
        "the echoed request body (carrying secrets) must not be printed:\n{combined}"
    );
}

// --- Task 6: determinism -----------------------------------------------------

#[test]
fn deploy_kit_is_byte_deterministic() {
    // The generator is pure templating (no timestamps, no HashMap iteration), so
    // the same design must always produce the byte-identical deploy kit — the
    // precondition for committing generated artifacts and reviewing diffs.
    let d = demo_design();
    let a = deploy::emit("render", &d).unwrap();
    let b = deploy::emit("render", &d).unwrap();
    assert_eq!(a, b, "same design -> byte-identical deploy kit");
}

// --- Task 7: mock-Render-API happy-path flow ---------------------------------

/// A tiny stub of the Render REST API: records each `METHOD PATH` it serves and
/// replies with canned 200 JSON so the generated `deploy.sh` can run end-to-end
/// with no real Render (and no docker). Returns the **host-root** base URL (no
/// `/v1` — the script's paths already carry it) plus the shared call log.
fn spawn_render_stub() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let rec = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 || line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            let (method, path) = (parts[0].to_string(), parts[1].to_string());
            // Drain headers; capture Content-Length so we consume any body.
            let mut clen = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                if h == "\r\n" || h == "\n" {
                    break;
                }
                if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                    clen = v.trim().parse().unwrap_or(0);
                }
            }
            if clen > 0 {
                let mut b = vec![0u8; clen];
                r.read_exact(&mut b).ok();
            }
            rec.lock().unwrap().push(format!("{method} {path}"));
            let body = render_stub_body(&method, &path);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            use std::io::Write as _;
            s.write_all(resp.as_bytes()).ok();
        }
    });
    (addr, calls)
}

/// Canned 200 bodies that walk the deploy through to a `live` deploy + a URL.
fn render_stub_body(method: &str, path: &str) -> String {
    // Strip the query string for matching.
    let p = path.split('?').next().unwrap();
    match (method, p) {
        ("GET", "/v1/owners") => r#"[{"owner":{"id":"own_1"}}]"#.into(),
        ("GET", "/v1/postgres") => "[]".into(),
        ("POST", "/v1/postgres") => r#"{"id":"pg_1","status":"creating"}"#.into(),
        ("GET", "/v1/postgres/pg_1") => r#"{"status":"available"}"#.into(),
        ("GET", "/v1/postgres/pg_1/connection-info") => {
            r#"{"internalConnectionString":"postgres://u:p@h:5432/d"}"#.into()
        }
        ("GET", "/v1/services") => "[]".into(),
        ("POST", "/v1/services") => {
            r#"{"service":{"id":"srv_1","serviceDetails":{"url":"https://acme-api.onrender.com"}}}"#
                .into()
        }
        ("GET", "/v1/services/srv_1") => {
            r#"{"service":{"serviceDetails":{"url":"https://acme-api.onrender.com"}}}"#.into()
        }
        ("GET", _) if p.ends_with("/deploys") => r#"[{"deploy":{"status":"live"}}]"#.into(),
        _ => "{}".into(),
    }
}

#[test]
fn deploy_sh_runs_the_full_flow_against_a_stub() {
    // Run the REAL generated deploy.sh against a stub Render API with the build
    // skipped (no docker). It must exit 0, print the live URL, never print the
    // secret value, and hit the expected create sequence (owners → postgres →
    // services). Proves the orchestration + happy path actually work end-to-end.
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("deploy").join("render");
    std::fs::create_dir_all(&dir).unwrap();
    let a = deploy::emit("render", &demo_design()).unwrap();
    let script = dir.join("deploy.sh");
    std::fs::write(&script, &a[0].1).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (base, calls) = spawn_render_stub();
    // RENDER_API_BASE is the HOST ROOT (the script paths already carry /v1).
    let out = Command::new("bash")
        .arg(&script)
        .env("RENDER_API_KEY", "rnd_test")
        .env("RENDER_API_BASE", &base)
        .env("JERRYCAN_DEPLOY_SKIP_BUILD", "1")
        .env("JERRYCAN_DEPLOY_IMAGE", "registry/test/acme-api")
        .env("JERRYCAN_DEPLOY_TAG", "test")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "deploy.sh failed.\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );
    assert!(
        stdout.contains("Deployed acme-api") && stdout.contains("onrender.com"),
        "expected the live URL in the summary:\n{stdout}"
    );
    // The secret value must never appear in any output.
    assert!(
        !stdout.contains("JERRYCAN_SECRET="),
        "secret value leaked: {stdout}"
    );
    // The expected create sequence happened.
    let seq = calls.lock().unwrap().join("\n");
    for expect in ["GET /v1/owners", "POST /v1/postgres", "POST /v1/services"] {
        assert!(seq.contains(expect), "missing {expect} in:\n{seq}");
    }
}

// --- Task 8: ignored live deploy roundtrip -----------------------------------

/// Real Render deploy. Run with:
///   RENDER_API_KEY=… JERRYCAN_DEPLOY_IMAGE=ghcr.io/you/jc-deploy-test \
///   cargo test -p jerrycan --test deploy live_render_deploy_roundtrip -- --ignored --nocapture
#[test]
#[ignore = "needs a real RENDER_API_KEY + a pushed image (costs money/time)"]
fn live_render_deploy_roundtrip() {
    let Ok(_key) = std::env::var("RENDER_API_KEY") else {
        eprintln!("SKIP: RENDER_API_KEY not set");
        return;
    };
    // The operator scaffolds a tiny app, runs deploy.sh, curls the URL for 200,
    // then runs teardown.sh. Document the manual steps here; the assertion is the
    // operator confirming a 2xx from the live URL and a clean teardown.
    eprintln!(
        "MANUAL/LIVE: scaffold a minimal app, `./deploy/render/deploy.sh`, curl the \
         printed URL for 200, then `./deploy/render/teardown.sh`. This test documents \
         the procedure; wire full automation here once a CI Render project exists."
    );
}
