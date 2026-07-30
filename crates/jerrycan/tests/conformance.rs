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
const REFERENCE: &str = include_str!("../../../conformance/designs/reference-slice.design.json");

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

/// Reset the target Postgres to a clean slate before a DB-backed heavy test
/// migrates into it. The heavy suite runs `--test-threads=1`, so there is never a
/// concurrent user of this database; dropping and recreating `public` BEFORE the
/// run (no teardown to race, unlike DROP DATABASE) fully isolates each
/// Postgres-backed test from whatever ran before. Requires `psql` — `heavy.yml`
/// installs `postgresql-client`.
fn reset_pg_public_schema(pg_url: &str) {
    let st = Command::new("psql")
        .arg(pg_url)
        .args(["-v", "ON_ERROR_STOP=1"])
        .args([
            "-c",
            "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;",
        ])
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "psql is required to reset the Postgres schema for the DB-backed \
                 heavy test (install postgresql-client): {e}"
            )
        });
    assert!(
        st.success(),
        "failed to reset public schema on the test database"
    );
}

#[test]
#[ignore = "heavy: db-mode golden app must build and pass the full gate"]
fn db_mode_scaffold_passes_jerrycan_check() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_db(tmp.path());
    // The documented workflow, in order: gen-tests → implement → check. Since
    // #123a a never-gen-tested scaffold is refused with JC0551, and the gate's
    // tests step then runs the generated acceptance suite — so the db fixtures
    // must be in place for the full gate to be green.
    for module in ["todos", "users"] {
        let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["gen-tests", "--module", module])
            .status()
            .unwrap();
        assert!(st.success(), "gen-tests {module} must succeed");
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
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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

/// Scaffold the golden app in DB+validate mode PLUS a `rate_limit` block, against
/// the LOCAL framework (path dep). The block adds only app-level middleware wiring,
/// so the existing db handler fixtures still drive a green check.
fn scaffold_golden_db_rate_limited(tmp: &Path) -> PathBuf {
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["db", "validate"]);
    design["rate_limit"] = serde_json::json!({ "limit": 100, "window": "1m" });
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

/// The #83 acceptance: a design carrying `rate_limit: { limit: 100, window: "1m" }`
/// scaffolds a `main.rs` that WIRES the limiter (`.extend(RateLimit::per_window(
/// 100, Duration::from_secs(60)))`) with the `rate-limit` facade feature on, passes
/// the full `jerrycan check` gate, and — the whole point of #83 — does so WITHOUT
/// tripping JL0003. Rate limiting used to be reachable only by hand-editing the
/// tool-owned main.rs, which permanently drifted it from the generator (JL0003).
/// Now the wiring is GENERATED from the design, so main.rs equals the generator's
/// output byte-for-byte and stays a `cargo fmt` fixpoint (no drift, ever).
#[test]
#[ignore = "heavy: rate-limited db golden app must build, fmt-clean, and pass the gate w/o JL0003"]
fn rate_limited_db_scaffold_checks_green_without_tripping_jl0003() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_db_rate_limited(tmp.path());

    // The generated main.rs wires the limiter (no-builder regime: rustfmt breaks
    // per_window's two args). Asserting the exact bytes proves what `check`'s
    // JL0003 lint compares main.rs against.
    let main = std::fs::read_to_string(app.join("crates/app/src/main.rs")).unwrap();
    assert!(
        main.contains(
            "        .extend(jerrycan::ratelimit::RateLimit::per_window(\n            100,\n            std::time::Duration::from_secs(60),\n        ))\n"
        ),
        "main.rs must wire the generated rate limiter:\n{main}"
    );
    // The `rate-limit` facade feature is enabled on the workspace jerrycan dep, so
    // `jerrycan::ratelimit::RateLimit` resolves.
    let ws_cargo = std::fs::read_to_string(app.join("Cargo.toml")).unwrap();
    assert!(
        ws_cargo.contains("\"rate-limit\""),
        "the rate-limit facade feature must be enabled:\n{ws_cargo}"
    );

    // The TOOL-OWNED main.rs must be a `rustfmt` fixpoint: running fmt must not
    // rewrite the generated `.extend(RateLimit..)` line — a rewrite is exactly what
    // would drift main.rs from the generator and trip JL0003 on the NEXT check.
    // (Only the tool-owned files are held to this; agent-owned stubs are theirs to
    // format, and JL0003 never inspects them.)
    let fmt = Command::new("rustfmt")
        .args(["--edition", "2024", "--check"])
        .arg(app.join("crates/app/src/main.rs"))
        .output()
        .unwrap();
    assert!(
        fmt.status.success(),
        "the generated main.rs must be a rustfmt fixpoint (no drift → no JL0003):\n{}\n{}",
        String::from_utf8_lossy(&fmt.stdout),
        String::from_utf8_lossy(&fmt.stderr)
    );

    // The documented workflow: gen-tests → implement → check (#123a refuses a
    // never-gen-tested module with JC0551). Reuse the golden db handler fixtures.
    for module in ["todos", "users"] {
        let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["gen-tests", "--module", module])
            .status()
            .unwrap();
        assert!(st.success(), "gen-tests {module} must succeed");
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

    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "the rate-limited app must pass the full gate; diagnostics: {}",
        payload["diagnostics"]
    );
    // The crux of #83: the generated wiring must NOT trip JL0003 (generated-file
    // drift). Assert it explicitly — a green `ok` alone could mask a lint that was
    // never evaluated.
    assert!(
        !payload["diagnostics"].to_string().contains("JL0003"),
        "generated rate-limit wiring must not trip JL0003: {}",
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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

/// Issue #116 — the compile proof. A transitively-owned grandchild (`Card`
/// belongs_to `Board` belongs_to the tenant `Org`) whose flat write lives in an
/// entity-hosting SUBROUTE (`/cards`, no tenant fk in the path → MembershipSet)
/// has its generated stub STEERED to `CardRepo::create_for_memberships(...)`.
/// Before the emission-gate fix, `entity_is_flat_tenant_owned` only scanned the
/// declaring top-level module's own endpoints — never a subroute — so it returned
/// false, the repo OMITTED the `*_for_memberships` methods, and a handler that
/// FOLLOWED its own steer failed to compile (`method not found`) behind a green
/// `check`. This scaffolds the shape, implements the create by following the steer
/// verbatim, and requires the workspace to build warning-free: the acceptance
/// criterion for #116 IS that the framework's own guidance compiles.
#[test]
#[ignore = "heavy: scaffolds the #116 grandchild-in-subroute shape and builds it"]
fn flat_grandchild_steer_following_handler_compiles() {
    const REPRO_116: &str = r#"{
        "name": "boards-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "orgs",
              "entities": [{ "name": "Org", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                  "auth_required": true,
                  "success": { "status": 200, "entity": "Org", "list": true } }] },
            { "name": "boards",
              "entities": [{ "name": "Board",
                  "belongs_to": [{ "entity": "Org" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "name", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_boards", "method": "GET", "path": "/",
                  "auth_required": true,
                  "success": { "status": 200, "entity": "Board", "list": true } }],
              "subroutes": [{
                  "name": "cards", "mount": "/cards",
                  "entities": [{ "name": "Card",
                      "belongs_to": [{ "entity": "Board" }],
                      "fields": [{ "name": "id", "type": "integer" },
                                 { "name": "title", "type": "string" }] }],
                  "endpoints": [{ "operation_id": "create_card", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Card" },
                      "success": { "status": 201, "entity": "Card" } }]
              }] }
        ]
    }"#;

    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, REPRO_116).unwrap();
    let app = tmp.path().join("boards-api");
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
    assert!(st.success(), "jerrycan new must scaffold the #116 shape");

    // The grandchild's flat write lives in the `cards` subroute of `boards`.
    let handlers_path = app.join("crates/routes/boards/src/subroutes/cards/handlers.rs");
    let handlers = std::fs::read_to_string(&handlers_path).unwrap();
    // The steer names the membership-checked create (fires regardless of the gate).
    assert!(
        handlers.contains("CardRepo::create_for_memberships(_user.0.id, card)"),
        "the subroute stub must steer to create_for_memberships:\n{handlers}"
    );
    // The gate must now EMIT that method, or following the steer is method-not-found.
    let repo =
        std::fs::read_to_string(app.join("crates/routes/boards/src/subroutes/cards/repo.rs"))
            .unwrap();
    assert!(
        repo.contains("pub async fn create_for_memberships("),
        "the emission gate must emit create_for_memberships for the grandchild-in-subroute (#116):\n{repo}"
    );

    // Follow the steer verbatim: call the membership-checked create. Keep the stub's
    // Err return (the create returns the new id; the point is that it type-checks).
    let stub = "    Err(Error::internal(\"create_card not implemented — replace this stub\"))";
    assert!(handlers.contains(stub), "unexpected stub body:\n{handlers}");
    let implemented = handlers.replace(
        stub,
        "    let _id = _repo.create_for_memberships(_user.0.id, _body).await?;\n    Err(Error::internal(\"create_card membership-checked create wired\"))",
    );
    std::fs::write(&handlers_path, &implemented).unwrap();

    // The proof: the workspace builds warning-free with a steer-following handler.
    let out = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .env("RUSTFLAGS", "-D warnings")
        .args(["build", "--workspace"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a handler following its own #116 steer must compile (was method-not-found):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// #123a: the full pipeline on a freshly-scaffolded, NEVER-gen-tested app must
/// not read green — the pre-fix `check` folded a zero-test `cargo test` (exit
/// 0) into ok:true, so a scaffold nobody ever tested shipped a green verdict.
/// The pipeline is fail-fast per class, so JC0551 being the failing class is
/// itself the proof that build/clippy/audit/deny/tests are all green on a
/// fresh scaffold; the verdict then flips on the acceptance step, naming each
/// endpoint-bearing module and the exact gen-tests command that fixes it.
#[test]
#[ignore = "heavy: full verification pipeline incl. cargo-audit/cargo-deny"]
fn fresh_scaffold_check_refuses_hollow_green_with_jc0551() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden(tmp.path());
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("check --json emits one JSON document");
    assert_eq!(
        payload["ok"], false,
        "a never-gen-tested scaffold must NOT read green: {payload}"
    );
    let diags = payload["diagnostics"].as_array().unwrap();
    assert!(
        !diags.is_empty() && diags.iter().all(|d| d["code"] == "JC0551"),
        "every diagnostic is JC0551 — the acceptance step is the failing class, \
         proving each earlier class (build/clippy/audit/deny/tests) was green: {diags:?}"
    );
    for m in ["todos", "users"] {
        assert!(
            diags.iter().any(|d| {
                let msg = d["message"].as_str().unwrap();
                msg.contains(&format!("module `{m}`"))
                    && msg.contains(&format!("jerrycan gen-tests --module {m}"))
            }),
            "JC0551 names `{m}` and its gen-tests command: {diags:?}"
        );
    }
    assert!(
        !out.status.success(),
        "a red check verdict must exit non-zero"
    );
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

    // #123a: gen-tests before check — the honest gate refuses a never-gen-tested
    // module with JC0551, and the acceptance suite it now runs (incl. the 401
    // guard probes) must be green on the implemented auth handlers.
    for module in ["todos", "users"] {
        let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["gen-tests", "--module", module])
            .status()
            .unwrap();
        assert!(st.success(), "gen-tests {module} must succeed");
    }

    // Full gate green (JL0004 must be satisfied — guarded mutations).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
            // SessionUser.id is a String (the stringified user pk), so the
            // minted cookie must use a string id or the app's session decode
            // rejects it as a 401.
            .encode(&serde_json::json!({ "id": "1", "role": "admin" }))
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
    let shared_target = common::shared_app_target();
    let mut c = common::McpClient::start_in_with_env(
        tmp.path(),
        &[
            ("JERRYCAN_FRAMEWORK_DEP", &dep),
            ("CARGO_TARGET_DIR", shared_target.to_str().unwrap()),
        ],
    );

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

    // 3. gen-tests via MCP (#123a: the check tool refuses a never-gen-tested
    // module with JC0551, so the agent loop runs gen-tests before check —
    // exactly the workflow the scaffold's next_step orders).
    for module in ["todos", "users"] {
        let (err, payload) = c.call_tool(
            "jerrycan_gen_tests",
            serde_json::json!({"directory": app.to_str().unwrap(), "module": module}),
        );
        assert!(!err, "{payload}");
    }

    // 4. the "agent" implements the handlers (canned fixtures).
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

    // 5. verify: the full gate must be green (incl. the generated acceptance
    // suite, which the canned implementations satisfy).
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

    // 6. serve and exercise the CRUD loop over real HTTP.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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

/// The J3 face-off failure (issue #51): ONE module with a root entity (Project at
/// `/`) AND a second entity (Task) that has its own creator (`POST /tasks`) and
/// its own `/{id}` routes. The generated `update_task`/`delete_task` probes must
/// seed a TASK (via `POST /tasks`) — not reuse the module-root Project creator —
/// so they are GREEN on a CORRECT handler. Before the fix they seeded a Project
/// and hit `/tasks/1`, a guaranteed 404 on correct code.
const J3_TWO_ENTITY: &str = r#"{
  "name": "j3",
  "contract_version": 1,
  "auth": { "model": "none" },
  "dependencies": ["db"],
  "modules": [{
    "name": "projects",
    "entities": [
      { "name": "Task", "fields": [{ "name": "title", "type": "string" }] },
      { "name": "Project", "fields": [{ "name": "name", "type": "string" }] }
    ],
    "endpoints": [
      { "operation_id": "list_projects", "method": "GET", "path": "/", "success": { "status": 200, "entity": "Project", "list": true } },
      { "operation_id": "create_project", "method": "POST", "path": "/", "request_body": { "entity": "Project" }, "success": { "status": 201, "entity": "Project" } },
      { "operation_id": "list_tasks", "method": "GET", "path": "/tasks", "success": { "status": 200, "entity": "Task", "list": true } },
      { "operation_id": "create_task", "method": "POST", "path": "/tasks", "request_body": { "entity": "Task" }, "success": { "status": 201, "entity": "Task" } },
      { "operation_id": "update_task", "method": "PUT", "path": "/tasks/{id}", "request_body": { "entity": "Task" }, "success": { "status": 200, "entity": "Task" }, "errors": [{ "status": 404, "when": "unknown id" }] },
      { "operation_id": "delete_task", "method": "DELETE", "path": "/tasks/{id}", "success": { "status": 204 }, "errors": [{ "status": 404, "when": "unknown id" }] }
    ]
  }]
}"#;

/// The correct `projects` handlers: `update_task`/`delete_task` operate on the
/// TASK repo. They only pass if the generated probe seeded a real Task row.
const J3_HANDLERS: &str = r#"//! Correct J3 handlers.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn list_projects(repo: Dep<ProjectRepo>) -> Result<Json<Vec<Project>>> {
    Ok(Json(repo.all().await?))
}
pub(crate) async fn create_project(repo: Dep<ProjectRepo>, Json(body): Json<Project>) -> Result<Created<Project>> {
    repo.insert(body.clone()).await?;
    Ok(Created(body))
}
pub(crate) async fn list_tasks(repo: Dep<TaskRepo>) -> Result<Json<Vec<Task>>> {
    Ok(Json(repo.all().await?))
}
pub(crate) async fn create_task(repo: Dep<TaskRepo>, Json(body): Json<Task>) -> Result<Created<Task>> {
    repo.insert(body.clone()).await?;
    Ok(Created(body))
}
pub(crate) async fn update_task(repo: Dep<TaskRepo>, Path(id): Path<i64>, Json(body): Json<Task>) -> Result<Json<Task>> {
    if repo.update(id, body.clone()).await? { Ok(Json(body)) } else { Err(Error::not_found()) }
}
pub(crate) async fn delete_task(repo: Dep<TaskRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id).await? { Ok(NoContent) } else { Err(Error::not_found()) }
}
"#;

/// Issue #51 end-to-end: a two-entity module's `/{id}` probes seed the RIGHT
/// entity and go GREEN on a correct scaffold. Scaffold → gen-tests (RED on stubs)
/// → implement the correct handlers → the SAME probes pass. Mirrors
/// `tdd_loop_goes_red_then_green_on_sqlite` for the J3 shape.
#[test]
#[ignore = "heavy: scaffold + gen-tests + red run + implement + green run (J3 seeding)"]
fn second_entity_id_probes_go_green_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, J3_TWO_ENTITY).unwrap();
    let app = tmp.path().join("j3");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "J3 must scaffold");

    // gen-tests: the two /{id} probes seed a Task via its OWN creator.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "gen-tests", "--module", "projects"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let acceptance = app.join("crates/routes/projects/tests/acceptance.rs");
    let generated = std::fs::read_to_string(&acceptance).unwrap();
    // The Task probes seed the /tasks collection, not the root Project creator.
    for probe in ["update_task_returns_200", "delete_task_returns_204"] {
        let body = &generated[generated.find(probe).expect(probe)..];
        assert!(
            body[..body.find("assert_eq!").unwrap()].contains("post_json(\"/projects/tasks\""),
            "{probe} must seed a Task via POST /projects/tasks:\n{generated}"
        );
    }

    // RED: stubs (500) fail the suite.
    let red = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(!red.status.success(), "stub handlers must fail the suite");

    // Implement the correct handlers.
    let dest = app.join("crates/routes/projects/src/handlers.rs");
    std::fs::write(&dest, J3_HANDLERS).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    std::fs::File::options()
        .write(true)
        .open(&dest)
        .unwrap()
        .set_modified(future)
        .unwrap();

    // GREEN: the same probes pass — proving the update/delete probes addressed a
    // seeded Task row, not a phantom id.
    let green = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        green.success(),
        "the second-entity /{{id}} probes must go green on a correct handler"
    );
}

/// The public-read/owner-write conformance fixture (#105): Post is per-user
/// identity-owned with `public_read: true`; `list_posts` is DECLARED
/// `auth_required` (the entity flag must override it — correct-by-construction).
/// User lives in its OWN module so the identity fk is cross-module (no DB FK —
/// the isolation test's session users need no seeded rows).
const FEED_PUBLIC_READ: &str = r#"{
  "name": "feed-api",
  "contract_version": 1,
  "auth": { "model": "session", "roles": ["user"] },
  "dependencies": ["db", "auth"],
  "modules": [
    {
      "name": "users",
      "entities": [{ "name": "User", "fields": [{ "name": "email", "type": "string" }] }],
      "endpoints": [
        { "operation_id": "register", "method": "POST", "path": "/register",
          "public": true,
          "request_body": { "entity": "User" },
          "success": { "status": 201, "entity": "User" } }
      ]
    },
    {
      "name": "posts",
      "entities": [
        { "name": "Post", "public_read": true,
          "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
          "fields": [{ "name": "title", "type": "string" }] }
      ],
      "endpoints": [
        { "operation_id": "list_posts", "method": "GET", "path": "/",
          "auth_required": true,
          "success": { "status": 200, "entity": "Post", "list": true } },
        { "operation_id": "get_post", "method": "GET", "path": "/{id}",
          "success": { "status": 200, "entity": "Post" },
          "errors": [{ "status": 404, "when": "unknown id" }] },
        { "operation_id": "create_post", "method": "POST", "path": "/",
          "auth_required": true,
          "request_body": { "entity": "Post" },
          "success": { "status": 201, "entity": "Post" } },
        { "operation_id": "update_post", "method": "PUT", "path": "/{id}",
          "auth_required": true,
          "request_body": { "entity": "Post" },
          "success": { "status": 200, "entity": "Post" },
          "errors": [{ "status": 404, "when": "unknown id or not the owner" }] },
        { "operation_id": "delete_post", "method": "DELETE", "path": "/{id}",
          "auth_required": true,
          "success": { "status": 204 },
          "errors": [{ "status": 404, "when": "unknown id or not the owner" }] }
      ]
    }
  ]
}"#;

/// The correct public-read/owner-write posts handlers (#105): PUBLIC reads via
/// the unscoped `all()`/`get()` (no session), owner-scoped writes via the
/// server-injected session user id + `update_for`/`remove_for`.
const FEED_POSTS_HANDLERS: &str = r#"//! Correct #105 posts handlers: public reads, owner-scoped writes.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use shared::CurrentUser;

pub(crate) async fn list_posts(repo: Dep<PostRepo>) -> Result<Json<Vec<Post>>> {
    Ok(Json(repo.all().await?))
}

pub(crate) async fn get_post(repo: Dep<PostRepo>, Path(id): Path<i64>) -> Result<Json<Post>> {
    repo.get(id).await?.map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn create_post(repo: Dep<PostRepo>, user: CurrentUser, Json(body): Json<PostRequest>) -> Result<Created<Post>> {
    let user_id: i64 = user.0.id.parse().map_err(|_| Error::unauthorized())?;
    let mut post = Post { id: 0, user_id, title: body.title };
    post.id = repo.insert(post.clone()).await?;
    Ok(Created(post))
}

pub(crate) async fn update_post(repo: Dep<PostRepo>, user: CurrentUser, Path(id): Path<i64>, Json(body): Json<PostRequest>) -> Result<Json<Post>> {
    let user_id: i64 = user.0.id.parse().map_err(|_| Error::unauthorized())?;
    let post = Post { id, user_id, title: body.title };
    if repo.update_for(user_id, id, post.clone()).await? {
        Ok(Json(post))
    } else {
        Err(Error::not_found())
    }
}

pub(crate) async fn delete_post(repo: Dep<PostRepo>, user: CurrentUser, Path(id): Path<i64>) -> Result<NoContent> {
    let user_id: i64 = user.0.id.parse().map_err(|_| Error::unauthorized())?;
    if repo.remove_for(user_id, id).await? {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
"#;

/// The trivial users handler for the feed fixture.
const FEED_USERS_HANDLERS: &str = r#"//! Correct users handlers for the #105 feed fixture.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn register(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    let mut user = body.clone();
    user.id = repo.insert(body).await?;
    Ok(Created(user))
}
"#;

/// Issue #105 end-to-end: the public-read/owner-write shape proves out on a real
/// scaffold. Scaffold → gen-tests (the acceptance suite carries the #105
/// isolation test and NO 401 probe for the public reads) → RED on stubs →
/// implement the correct handlers → the SAME suite goes GREEN — proving anon
/// list serves another user's row (200), anon detail 200s, anon create 401s, a
/// non-owner PUT/DELETE 404s with the row surviving, and the owner's PUT 200s.
/// Then the full `jerrycan check` gate passes on the implemented app — JL0006
/// stays silent on the module's unscoped public reads while the write needles
/// stay armed.
#[test]
#[ignore = "heavy: scaffold + gen-tests + red run + implement + green run + check (#105 public_read)"]
fn public_read_feed_goes_green_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, FEED_PUBLIC_READ).unwrap();
    let app = tmp.path().join("feed-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "the public_read design must scaffold");

    // The generated posts handlers: public GETs take no CurrentUser (despite the
    // declared auth_required on list_posts), writes keep the guard.
    let stubs = std::fs::read_to_string(app.join("crates/routes/posts/src/handlers.rs")).unwrap();
    assert!(
        stubs.contains("async fn list_posts(_repo: Dep<PostRepo>)"),
        "the public list stub must take no CurrentUser:\n{stubs}"
    );
    assert!(
        stubs.contains("async fn create_post(_repo: Dep<PostRepo>, _user: CurrentUser,"),
        "writes keep the guard:\n{stubs}"
    );

    // gen-tests: the suite carries the #105 isolation test; the public reads get
    // no 401 probe (the pre-fix gate-lie generated a permanently-red one).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "gen-tests", "--module", "posts"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gen-tests posts: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let acceptance =
        std::fs::read_to_string(app.join("crates/routes/posts/tests/acceptance.rs")).unwrap();
    assert!(
        acceptance.contains("async fn anon_reads_but_only_the_owner_writes_posts()"),
        "the #105 isolation test must be generated:\n{acceptance}"
    );
    assert!(
        !acceptance.contains("list_posts_without_auth_is_401")
            && !acceptance.contains("get_post_without_auth_is_401"),
        "no 401 probe for the public reads (red-when-correct otherwise):\n{acceptance}"
    );
    assert!(
        acceptance.contains("create_post_without_auth_is_401"),
        "writes keep their 401 probes:\n{acceptance}"
    );

    // #123a: the users module needs its acceptance file too, or the final
    // `jerrycan check` refuses the app with JC0551 (never-gen-tested module).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "gen-tests", "--module", "users"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gen-tests users: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // RED: stubs (500) fail the generated suite.
    let red = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(!red.status.success(), "stub handlers must fail the suite");

    // Implement the correct handlers.
    install_handler(
        &app,
        "crates/routes/posts/src/handlers.rs",
        FEED_POSTS_HANDLERS,
    );
    install_handler(
        &app,
        "crates/routes/users/src/handlers.rs",
        FEED_USERS_HANDLERS,
    );

    // GREEN: the same probes pass — the four-way #105 contract holds on a real
    // app: anon read 200 (another user's row), anon write 401, non-owner write
    // 404 (row survives), owner write 200.
    let green = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        green.success(),
        "the correct public-read/owner-write handlers must satisfy the generated suite"
    );

    // And the full gate holds on the implemented app: JL0006 must stay silent on
    // the unscoped public reads (`repo.all()`/`repo.get(`) in this public_read
    // module while the owner-scoped writes pass untouched.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
}

/// The #124 conformance fixture: a tenant module (`clubs`) that HOSTS a
/// tenant-owned child (Book belongs_to Club in the SAME module), so the
/// JL0006 scan reads the tenant's OWN handlers alongside the child's.
const CLUB_HOSTED_CHILD: &str = r#"{
  "name": "club-api",
  "contract_version": 1,
  "auth": { "model": "session", "roles": ["owner", "member"] },
  "dependencies": ["db", "auth"],
  "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
  "modules": [
    {
      "name": "users",
      "entities": [{ "name": "User", "fields": [{ "name": "email", "type": "string" }] }],
      "endpoints": [
        { "operation_id": "register", "method": "POST", "path": "/register",
          "public": true,
          "request_body": { "entity": "User" },
          "success": { "status": 201, "entity": "User" } }
      ]
    },
    {
      "name": "clubs",
      "entities": [
        { "name": "Club", "fields": [{ "name": "name", "type": "string" }] },
        { "name": "Book", "belongs_to": [{ "entity": "Club", "on_delete": "cascade" }],
          "fields": [{ "name": "title", "type": "string" }] }
      ],
      "endpoints": [
        { "operation_id": "list_clubs", "method": "GET", "path": "/",
          "auth_required": true,
          "success": { "status": 200, "entity": "Club", "list": true } },
        { "operation_id": "create_club", "method": "POST", "path": "/",
          "auth_required": true,
          "request_body": { "entity": "Club" },
          "success": { "status": 201, "entity": "Club" } },
        { "operation_id": "get_club", "method": "GET", "path": "/{id}",
          "auth_required": true,
          "success": { "status": 200, "entity": "Club" },
          "errors": [{ "status": 404, "when": "unknown id or not a member" }] },
        { "operation_id": "list_books", "method": "GET", "path": "/{club_id}/books",
          "auth_required": true,
          "success": { "status": 200, "entity": "Book", "list": true } },
        { "operation_id": "create_book", "method": "POST", "path": "/{club_id}/books",
          "auth_required": true,
          "request_body": { "entity": "Book" },
          "success": { "status": 201, "entity": "Book" } }
      ]
    }
  ]
}"#;

/// The CORRECT #124 clubs handlers: the tenant's own PathScoped detail route
/// (`get_club`) calls the unscoped `repo.get` on the TENANT repo — legitimate,
/// because the `Dep<Tenant>` guard already verified membership in the path
/// club — while the hosted child stays on the scoped accessors.
const CLUB_HANDLERS_CORRECT: &str = r#"//! Correct #124 clubs handlers: unscoped tenant detail read, scoped child.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;
use shared::Tenant;
use shared::CurrentUser;

pub(crate) async fn list_clubs(repo: Dep<ClubRepo>, user: CurrentUser) -> Result<Json<Vec<Club>>> {
    Ok(Json(repo.all_for_member(user.0.id.clone()).await?))
}

pub(crate) async fn create_club(repo: Dep<ClubRepo>, user: CurrentUser, Json(body): Json<Club>) -> Result<Created<Club>> {
    let mut club = body;
    club.id = repo.create_with_membership(user.0.id.clone(), club.clone()).await?;
    Ok(Created(club))
}

pub(crate) async fn get_club(repo: Dep<ClubRepo>, _tenant: Dep<Tenant>, Path(club_id): Path<i64>) -> Result<Json<Club>> {
    // Membership in the path club was already verified by the Dep<Tenant> guard;
    // the unscoped get on the TENANT repo is the correct call here (#124).
    repo.get(club_id).await?.map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn list_books(repo: Dep<BookRepo>, _tenant: Dep<Tenant>, Path(_club_id): Path<i64>) -> Result<Json<Vec<Book>>> {
    Ok(Json(repo.all_for(_tenant.id()).await?))
}

pub(crate) async fn create_book(repo: Dep<BookRepo>, _tenant: Dep<Tenant>, Path(club_id): Path<i64>, Json(body): Json<BookRequest>) -> Result<Created<Book>> {
    let mut book = Book { id: 0, club_id, title: body.title };
    book.id = repo.insert(book.clone()).await?;
    Ok(Created(book))
}
"#;

/// The trivial users handler for the #124 fixture.
const CLUB_USERS_HANDLERS: &str = r#"//! Correct users handlers for the #124 fixture.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn register(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    let mut user = body.clone();
    user.id = repo.insert(body).await?;
    Ok(Created(user))
}
"#;

/// Issue #124 end-to-end: a child-hosting tenant app with CORRECT handlers
/// passes the full `jerrycan check` gate — JL0006 stays silent on the tenant's
/// own path-verified detail read (`repo.get` in `get_club`, the pre-fix false
/// positive) — while a REAL leak (the child's `list_books` swapped to the
/// unscoped `repo.all()`) still fails the same gate with JL0006 pointing at
/// that line. WHY (Rule 9): the exemption must be surgical — green on correct
/// code, red on the exact leak class the lint exists for.
#[test]
#[ignore = "heavy: scaffold + implement + check green, then leak + check red (#124 tenant-detail exemption)"]
fn child_hosting_tenant_app_goes_green_on_correct_handlers() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, CLUB_HOSTED_CHILD).unwrap();
    let app = tmp.path().join("club-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(
        st.success(),
        "the child-hosting tenant design must scaffold"
    );

    // Implement the correct handlers: get_club reads the guard-verified tenant
    // via the unscoped `repo.get`, the child stays on the scoped accessors.
    install_handler(
        &app,
        "crates/routes/clubs/src/handlers.rs",
        CLUB_HANDLERS_CORRECT,
    );
    install_handler(
        &app,
        "crates/routes/users/src/handlers.rs",
        CLUB_USERS_HANDLERS,
    );

    // #123a: gen-tests before check — the honest gate refuses a never-gen-tested
    // module with JC0551, and the generated suite (incl. the member-surface and
    // isolation tests) must be green on the correct handlers.
    for module in ["users", "clubs"] {
        let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["gen-tests", "--module", module])
            .status()
            .unwrap();
        assert!(st.success(), "gen-tests {module} must succeed");
    }

    // GREEN: the full gate passes — pre-#124 this was a false JL0006 on
    // `get_club`'s legitimate `repo.get(club_id)`.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "correct child-hosting tenant handlers must pass the gate (JL0006 silent \
         on the tenant's own detail read): {}",
        payload["diagnostics"]
    );

    // RED: a real unscoped call in the CHILD's handler still fails the gate —
    // the exemption never reaches the child (the actual JL0006 target).
    install_handler(
        &app,
        "crates/routes/clubs/src/handlers.rs",
        &CLUB_HANDLERS_CORRECT.replace("repo.all_for(_tenant.id())", "repo.all()"),
    );
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], false,
        "the child's unscoped repo.all() must still fail the gate"
    );
    let diags = payload["diagnostics"].as_array().unwrap();
    assert!(
        diags.iter().any(|d| d["code"] == "JL0006"
            && d["file"] == "crates/routes/clubs/src/handlers.rs"
            && d["message"].as_str().unwrap().contains("all()")),
        "JL0006 must name the child's unscoped all(): {diags:?}"
    );
}

/// Issue #53 end-to-end: body-omittable server-owned fields. J4's shape (a public
/// `POST /subscribers` whose `confirmed`/`status` default server-side) AND J2's
/// shape (a nested `POST /habits/{habit_id}/checkins` whose parent fk comes from
/// the path). Both are UN-buildable before #53 (the minimal body 422s / the fk is
/// body-required); here the generated probe omits those fields and a correct
/// handler goes GREEN.
const OMITTABLE_DESIGN: &str = r#"{
  "name": "body-omittables",
  "contract_version": 0,
  "auth": { "model": "none" },
  "dependencies": ["db"],
  "modules": [
    {
      "name": "subscribers",
      "entities": [
        { "name": "Subscriber", "fields": [
          { "name": "email", "type": "string" },
          { "name": "confirmed", "type": "boolean", "default": false },
          { "name": "status", "type": "string", "values": ["active", "expired"], "default": "active" }
        ]}
      ],
      "endpoints": [
        { "operation_id": "list_subscribers", "method": "GET", "path": "/",
          "success": { "status": 200, "entity": "Subscriber", "list": true } },
        { "operation_id": "create_subscriber", "method": "POST", "path": "/",
          "request_body": { "entity": "Subscriber" },
          "success": { "status": 201, "entity": "Subscriber" } }
      ]
    },
    {
      "name": "habits",
      "entities": [
        { "name": "Habit", "fields": [{ "name": "name", "type": "string" }] },
        { "name": "Checkin", "belongs_to": [{ "entity": "Habit" }],
          "fields": [{ "name": "note", "type": "string" }] }
      ],
      "endpoints": [
        { "operation_id": "list_habits", "method": "GET", "path": "/",
          "success": { "status": 200, "entity": "Habit", "list": true } },
        { "operation_id": "create_habit", "method": "POST", "path": "/",
          "request_body": { "entity": "Habit" },
          "success": { "status": 201, "entity": "Habit" } },
        { "operation_id": "create_checkin", "method": "POST", "path": "/{habit_id}/checkins",
          "request_body": { "entity": "Checkin" },
          "success": { "status": 201, "entity": "Checkin" } }
      ]
    }
  ]
}"#;

/// Correct subscribers handlers: `SubscriberRequest` has NO `confirmed`/`status`,
/// so the handler MUST supply the server defaults (it can't even name them from
/// the body — a compile-time forcing function).
const OMITTABLE_SUBSCRIBERS_HANDLERS: &str = r#"//! Correct subscribers handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_subscribers(repo: Dep<SubscriberRepo>) -> Result<Json<Vec<Subscriber>>> {
    Ok(Json(repo.all().await?))
}

pub(crate) async fn create_subscriber(repo: Dep<SubscriberRepo>, Json(body): Json<SubscriberRequest>) -> Result<Created<Subscriber>> {
    let mut sub = Subscriber { id: 0, email: body.email, confirmed: false, status: "active".into() };
    sub.id = repo.insert(sub.clone()).await?;
    Ok(Created(sub))
}
"#;

/// Correct habits handlers: `create_checkin` injects the `habit_id` PATH param
/// (the DTO omits it), so the checkin attaches to the path's habit.
const OMITTABLE_HABITS_HANDLERS: &str = r#"//! Correct habits handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_habits(repo: Dep<HabitRepo>) -> Result<Json<Vec<Habit>>> {
    Ok(Json(repo.all().await?))
}

pub(crate) async fn create_habit(repo: Dep<HabitRepo>, Json(body): Json<Habit>) -> Result<Created<Habit>> {
    let mut habit = body.clone();
    habit.id = repo.insert(body).await?;
    Ok(Created(habit))
}

pub(crate) async fn create_checkin(repo: Dep<CheckinRepo>, Path(habit_id): Path<i64>, Json(body): Json<CheckinRequest>) -> Result<Created<Checkin>> {
    let mut checkin = Checkin { id: 0, habit_id, note: body.note };
    checkin.id = repo.insert(checkin.clone()).await?;
    Ok(Created(checkin))
}
"#;

/// Overwrite a handler file and bump its mtime so cargo's mtime fingerprint
/// recompiles it (RED's `cargo test` and this write share a wall-clock second).
fn install_handler(app: &Path, rel: &str, contents: &str) {
    let dest = app.join(rel);
    std::fs::write(&dest, contents).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    std::fs::File::options()
        .write(true)
        .open(&dest)
        .unwrap()
        .set_modified(future)
        .unwrap();
}

#[test]
#[ignore = "heavy: scaffold + gen-tests + red run + implement + green run (#53 body-omittables)"]
fn body_omittable_fields_go_green_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, OMITTABLE_DESIGN).unwrap();
    let app = tmp.path().join("body-omittables");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "design must scaffold");

    // gen-tests both modules; capture the generated probe bodies.
    for module in ["subscribers", "habits"] {
        let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["--json", "gen-tests", "--module", module])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "gen-tests {module}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The generated probe bodies OMIT the server-owned fields (contract on the wire).
    let subs =
        std::fs::read_to_string(app.join("crates/routes/subscribers/tests/acceptance.rs")).unwrap();
    assert!(
        subs.contains("create_subscriber_returns_201")
            && !subs.contains("\"confirmed\"")
            && !subs.contains("\"status\""),
        "subscriber probe must omit defaulted fields:\n{subs}"
    );
    let habits =
        std::fs::read_to_string(app.join("crates/routes/habits/tests/acceptance.rs")).unwrap();
    assert!(
        habits.contains("post_json(\"/habits/1/checkins\"") && !habits.contains("\"habit_id\""),
        "checkin probe must post under the path habit and omit habit_id:\n{habits}"
    );

    // RED: stubs (500) fail the generated suite.
    let red = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(!red.status.success(), "stub handlers must fail the suite");

    // Implement the correct handlers.
    install_handler(
        &app,
        "crates/routes/subscribers/src/handlers.rs",
        OMITTABLE_SUBSCRIBERS_HANDLERS,
    );
    install_handler(
        &app,
        "crates/routes/habits/src/handlers.rs",
        OMITTABLE_HABITS_HANDLERS,
    );

    // Append value assertions that reuse each module's tool-owned `app()` helper:
    // the minimal body 201s AND the server-owned values are readable server-side.
    let subs_test = "\n#[tokio::test]\nasync fn create_subscriber_applies_server_defaults() {\n    let t = app().await;\n    let res = t.post_json(\"/subscribers/\", &serde_json::json!({\"email\": \"a@b.c\"})).await;\n    assert_eq!(res.status().as_u16(), 201, \"minimal body must 201; body: {}\", res.text());\n    let body: serde_json::Value = serde_json::from_str(&res.text()).expect(\"json\");\n    assert_eq!(body[\"confirmed\"], serde_json::json!(false), \"server default confirmed=false; body: {}\", res.text());\n    assert_eq!(body[\"status\"], serde_json::json!(\"active\"), \"server default status=active; body: {}\", res.text());\n}\n";
    let habits_test = "\n#[tokio::test]\nasync fn create_checkin_attaches_to_path_habit() {\n    let t = app().await;\n    let h = t.post_json(\"/habits/\", &serde_json::json!({\"name\": \"run\"})).await;\n    assert_eq!(h.status().as_u16(), 201, \"seed habit; body: {}\", h.text());\n    let res = t.post_json(\"/habits/1/checkins\", &serde_json::json!({\"note\": \"did it\"})).await;\n    assert_eq!(res.status().as_u16(), 201, \"checkin without habit_id must 201; body: {}\", res.text());\n    let body: serde_json::Value = serde_json::from_str(&res.text()).expect(\"json\");\n    assert_eq!(body[\"habit_id\"], serde_json::json!(1), \"checkin attaches to the path's habit; body: {}\", res.text());\n}\n";
    for (rel, extra) in [
        ("crates/routes/subscribers/tests/acceptance.rs", subs_test),
        ("crates/routes/habits/tests/acceptance.rs", habits_test),
    ] {
        let path = app.join(rel);
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str(extra);
        std::fs::write(&path, &content).unwrap();
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(future)
            .unwrap();
    }

    // GREEN: the generated probes AND the value assertions pass.
    let green = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        green.success(),
        "correct handlers must satisfy the body-omittable contract (defaults applied, path fk injected)"
    );
}

/// Issue #110 end-to-end: a `datetime` field defaulting to `"now"` is a dynamic
/// server-set, set-once timestamp. `created_at` is dropped from BOTH request DTOs
/// (create + update) and their OpenAPI schemas, stays in the response entity
/// schema, and the create stub steers the handler to `now_rfc3339()`. A correct
/// app — create sets `created_at = now_rfc3339()`, update PRESERVES it — passes the
/// full `jerrycan check` gate.
const NOW_DESIGN: &str = r#"{
  "name": "notes-now",
  "contract_version": 0,
  "auth": { "model": "none" },
  "dependencies": ["db"],
  "modules": [
    {
      "name": "notes",
      "entities": [
        { "name": "Note", "fields": [
          { "name": "body", "type": "string" },
          { "name": "created_at", "type": "datetime", "default": "now" }
        ]}
      ],
      "endpoints": [
        { "operation_id": "list_notes", "method": "GET", "path": "/",
          "success": { "status": 200, "entity": "Note", "list": true } },
        { "operation_id": "create_note", "method": "POST", "path": "/",
          "request_body": { "entity": "Note" },
          "success": { "status": 201, "entity": "Note" } },
        { "operation_id": "update_note", "method": "PUT", "path": "/{id}",
          "request_body": { "entity": "Note" },
          "success": { "status": 200, "entity": "Note" } }
      ]
    }
  ]
}"#;

/// Correct notes handlers: `NoteRequest`/`NoteUpdateRequest` have NO `created_at`,
/// so the create handler MUST set it via `now_rfc3339()` (a compile-time forcing
/// function) and the update handler PRESERVES the stored value — a client can never
/// rewrite the timestamp.
const NOW_HANDLERS: &str = r#"//! Correct notes handlers (#110 now-default).
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_notes(repo: Dep<NoteRepo>) -> Result<Json<Vec<Note>>> {
    Ok(Json(repo.all().await?))
}

pub(crate) async fn create_note(repo: Dep<NoteRepo>, Json(body): Json<NoteRequest>) -> Result<Created<Note>> {
    let mut note = Note { id: 0, body: body.body, created_at: now_rfc3339() };
    note.id = repo.insert(note.clone()).await?;
    Ok(Created(note))
}

pub(crate) async fn update_note(repo: Dep<NoteRepo>, Path(id): Path<i64>, Json(body): Json<NoteUpdateRequest>) -> Result<Json<Note>> {
    let existing = repo.get(id).await?.ok_or_else(Error::not_found)?;
    let updated = Note { id, body: body.body, created_at: existing.created_at };
    if repo.update(id, updated.clone()).await? {
        Ok(Json(updated))
    } else {
        Err(Error::not_found())
    }
}
"#;

#[test]
#[ignore = "heavy: scaffold + gen-tests + implement + full check gate (#110 now-default)"]
fn now_default_timestamp_goes_green_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, NOW_DESIGN).unwrap();
    let app = tmp.path().join("notes-now");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "the now-default design must scaffold");

    // BOTH request DTOs drop `created_at` (server-owned on create, immutable on
    // update); the entity Model KEEPS it (present in every response).
    let model = std::fs::read_to_string(app.join("crates/routes/notes/src/model.rs")).unwrap();
    for dto in ["pub struct NoteRequest {", "pub struct NoteUpdateRequest {"] {
        let body = model
            .split(dto)
            .nth(1)
            .unwrap_or_else(|| panic!("{dto} must be emitted:\n{model}"))
            .split('}')
            .next()
            .unwrap();
        assert!(
            !body.contains("created_at"),
            "{dto} must omit created_at (the divergence: dropped on both):\n{body}"
        );
        assert!(
            body.contains("body"),
            "{dto} keeps the client field:\n{body}"
        );
    }
    assert!(
        model.contains("pub created_at: String,"),
        "the entity Model KEEPS created_at (present in responses):\n{model}"
    );

    // The create stub steers the handler at now_rfc3339().
    let handlers =
        std::fs::read_to_string(app.join("crates/routes/notes/src/handlers.rs")).unwrap();
    assert!(
        handlers.contains("now_rfc3339()") && handlers.contains("server-set timestamp"),
        "the create stub must steer created_at at now_rfc3339():\n{handlers}"
    );

    // OpenAPI: the response schema (Note) includes created_at; both request schemas
    // omit it.
    let openapi: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(app.join("openapi.json")).unwrap()).unwrap();
    let schemas = &openapi["components"]["schemas"];
    assert!(
        !schemas["Note"]["properties"]["created_at"].is_null(),
        "the response entity schema must include created_at: {}",
        schemas["Note"]
    );
    for req in ["NoteRequest", "NoteUpdateRequest"] {
        assert!(
            schemas[req]["properties"]["created_at"].is_null(),
            "{req} schema must omit created_at: {}",
            schemas[req]
        );
    }

    // gen-tests the module, implement the correct handlers, then the FULL gate is
    // green (JC0551 cleared, the generated acceptance suite passes on real handlers).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "gen-tests", "--module", "notes"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gen-tests notes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    install_handler(&app, "crates/routes/notes/src/handlers.rs", NOW_HANDLERS);

    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "correct now-default handlers must pass the full gate; diagnostics: {}",
        payload["diagnostics"]
    );
    assert!(out.status.success());
}

/// The #80 conformance fixture: a db-mode design whose fields declare
/// range/length constraints. `body` (min_len 2 / max_len 30) is the FIRST
/// rejectable field of the Note body, so the generated string reject probe
/// carries the `"a".repeat(31)` over-max EXPRESSION; `priority`/`points`
/// (integer min/max, both minimums above the default fixture `1`) force the
/// in-range clamp — a broken derivation would send `1`, trip the migration
/// CHECK, and turn the 201 probes red. `seq` (min 3000000000 / max
/// 4102444800, both `> i32::MAX`) forces the `i64`-suffixed fixture literal
/// (0.6.5 final review, Critical): a bare `3000000000` inside
/// `serde_json::json!` is typed i32 and the whole suite is a HARD compile
/// error — this test COMPILES AND RUNS the generated probes, so a regression
/// can't scaffold-and-pass again.
const LIMITS: &str = include_str!("../../../conformance/designs/limits-api.design.json");

/// The correct limits-api handlers: plain store + echo. Deliberately ZERO
/// hand-written validation — the generated `de_*` deserialize-validators own
/// the declared bounds, which is the whole #80 payoff.
const LIMITS_HANDLERS: &str = r#"//! Correct #80 handlers: store + echo; the generated de_* validators own the bounds.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn create_note(repo: Dep<NoteRepo>, Json(body): Json<Note>) -> Result<Created<Note>> {
    let mut note = body;
    note.id = repo.insert(note.clone()).await?;
    Ok(Created(note))
}

pub(crate) async fn update_note(repo: Dep<NoteRepo>, Path(id): Path<i64>, Json(body): Json<Note>) -> Result<Json<Note>> {
    if repo.update(id, body.clone()).await? { Ok(Json(body)) } else { Err(Error::not_found()) }
}

pub(crate) async fn create_score(repo: Dep<ScoreRepo>, Json(body): Json<Score>) -> Result<Created<Score>> {
    let mut score = body;
    score.id = repo.insert(score.clone()).await?;
    Ok(Created(score))
}
"#;

/// Issue #80 end-to-end: a range/length-constrained design is greenable with
/// ZERO hand-written `Valid` impls. Scaffold (migration CHECKs emitted) →
/// gen-tests (the three 422 reject probes are reject-counted OUT of
/// expected_failing) → RED on stubs (exactly the four success/404 probes) →
/// trivial store+echo handlers → the SAME suite goes GREEN — the in-range
/// happy path 201/200s through the de_* validator AND the DB CHECK, and the
/// `_rejects_out_of_range_{field}` probes (incl. the compiled-and-run
/// `"a".repeat(31)` over-max string) assert 422 — → full `jerrycan check` ok.
#[test]
#[ignore = "heavy: scaffold + gen-tests + red run + implement + green run + check (#80 constraints)"]
fn constrained_design_goes_green_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, LIMITS).unwrap();
    let app = tmp.path().join("limits-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "the constrained design must scaffold");

    // Defense-in-depth: every constrained column carries its migration CHECK,
    // so the in-range fixtures below are proven against the DB too.
    let ddl = std::fs::read_to_string(
        app.join("crates/routes/notes/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap();
    for check in [
        "CHECK (length(\"body\") BETWEEN 2 AND 30)",
        "CHECK (\"priority\" BETWEEN 2 AND 5)",
        "CHECK (\"points\" BETWEEN 10 AND 100)",
        "CHECK (\"seq\" BETWEEN 3000000000 AND 4102444800)",
    ] {
        assert!(
            ddl.contains(check),
            "missing `{check}` in migration:\n{ddl}"
        );
    }

    // gen-tests: 7 tests, of which the three 422 reject probes pass on stubs
    // (the boundary rejects before the handler) — expected_failing counts only
    // the four success/404 probes (the T3 reject math, proven end-to-end).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "gen-tests", "--module", "notes"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gen-tests notes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        payload["expected_failing"], 4,
        "the three reject probes must be excluded from expected_failing: {payload}"
    );

    let acceptance =
        std::fs::read_to_string(app.join("crates/routes/notes/tests/acceptance.rs")).unwrap();
    // Each constrained body gets its out-of-range 422 probe, corrupting the
    // FIRST rejectable field: the string one carries the `"a".repeat(31)`
    // over-max EXPRESSION (compiled and executed below, not just pinned as
    // text), the integer one sends max + 1.
    for (probe, needle) in [
        (
            "async fn create_note_rejects_out_of_range_body()",
            "\"body\": \"a\".repeat(31)",
        ),
        (
            "async fn update_note_rejects_out_of_range_body()",
            "\"body\": \"a\".repeat(31)",
        ),
        (
            "async fn create_score_rejects_out_of_range_points()",
            "\"points\": 101",
        ),
    ] {
        let at = acceptance
            .find(probe)
            .unwrap_or_else(|| panic!("{probe} missing:\n{acceptance}"));
        let fn_body = &acceptance[at..acceptance[at..].find("\n}").unwrap() + at];
        assert!(
            fn_body.contains(needle) && fn_body.contains(", 422,"),
            "{probe} must send {needle} and assert 422:\n{fn_body}"
        );
    }
    // The happy-path fixtures are derived IN-RANGE: the default integer
    // fixture `1` is clamped up to each declared minimum (2 and 10) — a raw
    // `1` would violate the CHECKs above and redden the 201 probes on a
    // CORRECT handler.
    assert!(
        acceptance.contains("\"priority\": 2") && acceptance.contains("\"points\": 10"),
        "integer fixtures must clamp into the declared range:\n{acceptance}"
    );
    // A `> i32::MAX` bound emits an `i64`-suffixed fixture literal — a bare
    // `3000000000` inside `serde_json::json!` is typed i32 and the generated
    // suite would not COMPILE (deny-by-default `overflowing_literals`); the
    // red/green runs below execute it, so this can't regress silently.
    assert!(
        acceptance.contains("\"seq\": 3000000000i64"),
        "out-of-i32-range fixtures must be i64-suffixed:\n{acceptance}"
    );

    // RED: stubs (500) fail exactly the four success/404 probes — the three
    // reject probes PASS on stubs (the 422 precedes the handler), proving the
    // out-of-range bodies are refused by the GENERATED validators alone.
    let red = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(!red.status.success(), "stub handlers must fail the suite");
    let red_out = format!(
        "{}{}",
        String::from_utf8_lossy(&red.stdout),
        String::from_utf8_lossy(&red.stderr)
    );
    let failed: usize = red_out
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
        failed, 4,
        "the 422 reject probes must already pass on stubs:\n{red_out}"
    );

    // Implement the correct handlers: store + echo, ZERO hand-written
    // validation — the #80 contract is enforced entirely by generated code.
    install_handler(&app, "crates/routes/notes/src/handlers.rs", LIMITS_HANDLERS);

    // GREEN: the same suite passes — in-range bodies clear the de_* validator
    // AND the migration CHECK (201/200), out-of-range bodies 422 at the
    // boundary, on the create AND update paths.
    let green = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        green.success(),
        "a constrained design must be greenable with zero hand-written Valid impls"
    );

    // And the full gate holds on the implemented app.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
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
            .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
            .env("CARGO_TARGET_DIR", common::shared_app_target())
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

    // Clean slate before migrating: isolate this test from any prior run's tables
    // (see reset_pg_public_schema). Safe because the heavy suite runs
    // single-threaded, so there is never a concurrent user of this database.
    reset_pg_public_schema(&pg_url);

    // Apply migrations to the real Postgres, then serve against it and drive CRUD.
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
    // The heart of this test — a static `x86_64-unknown-linux-musl` binary built
    // and served, plus a Linux container image — cannot be produced on a non-Linux
    // host: there is no musl cross-linker, so the link fails (Apple's ld rejects
    // the GNU `-Bstatic`/`--as-needed`/… flags). CI runs this on Linux; skip loudly
    // elsewhere so `cargo test --include-ignored` stays green for macOS/other
    // contributors. Package file-generation (Dockerfile/k8s/systemd/SBOM) is
    // covered on every host by tests/package.rs.
    if !cfg!(target_os = "linux") {
        eprintln!(
            "SKIP golden_app_deploys_everywhere: needs a Linux host for the musl \
             binary + container legs (host is {}); CI covers it.",
            std::env::consts::OS
        );
        return;
    }
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

    // #123a: `package` shares the check gate, which now refuses a
    // never-gen-tested module with JC0551 — gen-tests first, as the workflow
    // orders (the canned implementations satisfy the generated suite).
    for module in ["todos", "users"] {
        let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["gen-tests", "--module", module])
            .status()
            .unwrap();
        assert!(st.success(), "gen-tests {module} must succeed");
    }

    // ONE command emits every artifact (after a green check gate).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
            .env("CARGO_TARGET_DIR", common::shared_app_target())
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
            .env("CARGO_TARGET_DIR", common::shared_app_target())
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

/// THE v2 north-star gate: the reference-slice design — a tenant-scoped, JWT-guarded,
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
#[ignore = "heavy: reference-slice (SeaORM) scaffolds, builds, reds-on-stubs; records cold-build baseline"]
fn reference_slice_scaffold_passes_check() {
    let tmp = tempfile::tempdir().unwrap();

    // Scaffold the reference-slice design wired to the LOCAL framework path dep, the
    // same way every other heavy test wires it (env passed to the child only).
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, REFERENCE).unwrap();
    let app = tmp.path().join("reference-slice");
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
    assert!(st.success(), "reference-slice must scaffold");

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
            .env("CARGO_TARGET_DIR", common::shared_app_target())
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
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["build", "--workspace"])
        .output()
        .unwrap();
    let cold_build = t0.elapsed();
    assert!(
        build.status.success(),
        "reference-slice (SeaORM) generated workspace must build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    eprintln!("reference-slice cold build: {cold_build:?}");

    // INCREMENTAL test-build baseline: build (don't run) the route-leads test
    // binary now that deps are warm — the tightest agent inner-loop signal. `cargo
    // test --no-run` compiles the test target without executing it.
    let t1 = std::time::Instant::now();
    let leads_build = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "-p", "route-leads", "--no-run"])
        .output()
        .unwrap();
    let leads_test_build = t1.elapsed();
    assert!(
        leads_build.status.success(),
        "route-leads test binary must compile:\n{}",
        String::from_utf8_lossy(&leads_build.stderr)
    );
    eprintln!("reference-slice route-leads incremental test-build: {leads_test_build:?}");

    // RED on stubs: run every generated test. `--no-fail-fast` so cargo runs all
    // test binaries (it otherwise halts at the first failing crate).
    let out = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
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
    let design: jerrycan::platform::design::Design = serde_json::from_str(REFERENCE).unwrap();
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

/// The #115 composite-unique conformance fixture: a `Like` per `(user, post)`.
/// User and Post live in their OWN modules, so the fk columns are cross-module
/// (unenforced — no seeded parent row needed for a flat create probe) while the
/// `unique: [["user_id","post_id"]]` composite index is emitted on the likes
/// table regardless. The generated suite carries the composite-unique 409 test.
const COMPOSITE_UNIQUE_LIKES: &str = r#"{
  "name": "likes-api",
  "contract_version": 1,
  "dependencies": ["db"],
  "modules": [
    { "name": "users",
      "entities": [{ "name": "User", "fields": [{ "name": "email", "type": "string" }] }],
      "endpoints": [{ "operation_id": "create_user", "method": "POST", "path": "/",
        "request_body": { "entity": "User" }, "success": { "status": 201, "entity": "User" } }] },
    { "name": "posts",
      "entities": [{ "name": "Post", "fields": [{ "name": "title", "type": "string" }] }],
      "endpoints": [{ "operation_id": "create_post", "method": "POST", "path": "/",
        "request_body": { "entity": "Post" }, "success": { "status": 201, "entity": "Post" } }] },
    { "name": "engagement",
      "entities": [{ "name": "Like",
        "belongs_to": [{ "entity": "User" }, { "entity": "Post" }],
        "unique": [["user_id", "post_id"]],
        "fields": [{ "name": "reaction", "type": "string" }] }],
      "endpoints": [{ "operation_id": "create_like", "method": "POST", "path": "/",
        "request_body": { "entity": "Like" }, "success": { "status": 201, "entity": "Like" } }] }
  ]
}"#;

const LIKES_USERS_HANDLERS: &str = r#"//! Correct users handler for the #115 fixture.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn create_user(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    repo.insert(body.clone()).await?;
    Ok(Created(body))
}
"#;

const LIKES_POSTS_HANDLERS: &str = r#"//! Correct posts handler for the #115 fixture.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn create_post(repo: Dep<PostRepo>, Json(body): Json<Post>) -> Result<Created<Post>> {
    repo.insert(body.clone()).await?;
    Ok(Created(body))
}
"#;

/// The correct engagement handler: a plain `insert`. The SECOND create with the
/// same `(user_id, post_id)` hits the `CREATE UNIQUE INDEX` — `db_error` maps the
/// unique violation to `Error::conflict` → 409, no application-level check.
const LIKES_ENGAGEMENT_HANDLERS: &str = r#"//! Correct #115 engagement handler: insert; the composite UNIQUE index 409s a dup.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn create_like(repo: Dep<LikeRepo>, Json(body): Json<Like>) -> Result<Created<Like>> {
    repo.insert(body.clone()).await?;
    Ok(Created(body))
}
"#;

/// Issue #115 end-to-end: the composite / multi-column UNIQUE proves out on a
/// real scaffold. Scaffold → gen-tests (the suite carries the composite-unique
/// 409 test) → RED on stubs → implement the plain-insert handlers → the SAME
/// suite goes GREEN, so a duplicate `(user_id, post_id)` insert is a 409 through
/// the DB index (no application-level SELECT-then-INSERT). Then the full
/// `jerrycan check` gate passes on the implemented app.
#[test]
#[ignore = "heavy: scaffold + gen-tests + red run + implement + green run + check (#115 composite unique)"]
fn composite_unique_conflict_goes_409_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, COMPOSITE_UNIQUE_LIKES).unwrap();
    let app = tmp.path().join("likes-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "the composite-unique design must scaffold");

    // The migration carries the composite unique index on the likes table.
    for dialect in ["sqlite", "postgres"] {
        let sql = std::fs::read_to_string(app.join(format!(
            "crates/routes/engagement/migrations/{dialect}/0001_create_tables.sql"
        )))
        .unwrap();
        assert!(
            sql.contains(
                "CREATE UNIQUE INDEX \"idx_likes_uc0\" ON \"likes\" (\"user_id\", \"post_id\")"
            ),
            "{dialect}: the composite unique index must be scaffolded:\n{sql}"
        );
    }

    // gen-tests every module; the engagement suite carries the 409 conflict test.
    for module in ["users", "posts", "engagement"] {
        let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", common::shared_app_target())
            .args(["--json", "gen-tests", "--module", module])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "gen-tests {module}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let acceptance =
        std::fs::read_to_string(app.join("crates/routes/engagement/tests/acceptance.rs")).unwrap();
    assert!(
        acceptance.contains("async fn like_composite_unique_0_is_409()"),
        "the composite-unique 409 test must be generated:\n{acceptance}"
    );

    // RED: stubs fail the generated suite (the first create never inserts).
    let red = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(!red.status.success(), "stub handlers must fail the suite");

    // Implement the correct plain-insert handlers.
    install_handler(
        &app,
        "crates/routes/users/src/handlers.rs",
        LIKES_USERS_HANDLERS,
    );
    install_handler(
        &app,
        "crates/routes/posts/src/handlers.rs",
        LIKES_POSTS_HANDLERS,
    );
    install_handler(
        &app,
        "crates/routes/engagement/src/handlers.rs",
        LIKES_ENGAGEMENT_HANDLERS,
    );

    // GREEN: the composite-unique 409 test passes — a duplicate (user_id,
    // post_id) is a 409 through the DB index, not a race.
    let green = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        green.success(),
        "the plain-insert handlers must satisfy the composite-unique 409 test"
    );

    // The full gate holds on the implemented app.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
}

/// The #119 fk-alias conformance fixture: a ledger `Transfer` with TWO aliased
/// references to `Account` (from_account/to_account) and a self-referential
/// `Comment` (parent), all in ONE module so the fk columns carry REAL DDL FOREIGN
/// KEY constraints — the case a single un-aliased `belongs_to` cannot express.
/// GET-only so the generated probes read (no create probe → no FK-seed needed).
const FK_ALIAS_LEDGER: &str = r#"{
  "name": "ledger-api",
  "contract_version": 1,
  "dependencies": ["db"],
  "modules": [
    { "name": "ledger",
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
        { "operation_id": "list_transfers", "method": "GET", "path": "/transfers",
          "success": { "status": 200, "entity": "Transfer", "list": true } },
        { "operation_id": "show_transfer", "method": "GET", "path": "/transfers/{id}",
          "success": { "status": 200, "entity": "Transfer" } },
        { "operation_id": "list_comments", "method": "GET", "path": "/comments",
          "success": { "status": 200, "entity": "Comment", "list": true } }
      ] }
  ]
}"#;

/// The correct GET-only handlers over the aliased-fk entities.
const FK_ALIAS_HANDLERS: &str = r#"//! Correct #119 handlers: GET-only reads over the aliased-fk entities.
use jerrycan::prelude::*;
use super::model::*;
use super::repo::*;

pub(crate) async fn list_transfers(repo: Dep<TransferRepo>) -> Result<Json<Vec<Transfer>>> {
    Ok(Json(repo.all().await?))
}

pub(crate) async fn show_transfer(repo: Dep<TransferRepo>, Path(id): Path<i64>) -> Result<Json<Transfer>> {
    repo.get(id).await?.map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn list_comments(repo: Dep<CommentRepo>) -> Result<Json<Vec<Comment>>> {
    Ok(Json(repo.all().await?))
}
"#;

/// Issue #119 end-to-end: the belongs_to fk alias proves out on a real scaffold.
/// The two-reference `Transfer` (from_account_id + to_account_id, two distinct
/// FKs to `accounts` with distinct constraint names) and the self-referential
/// `Comment` (parent_id → comments) scaffold, the generated SeaORM model
/// (distinct Relation variants + a single `Related` impl per target) COMPILES,
/// the generated suite goes GREEN on the correct handlers, and the full
/// `jerrycan check` gate passes. Un-aliased `belongs_to` stays byte-identical
/// (covered by determinism.rs); THIS proves the alias path is buildable end to
/// end — a single un-aliased belongs_to could never express two refs to one table.
#[test]
#[ignore = "heavy: scaffold + build + gen-tests + red run + implement + green run + check (#119 fk alias)"]
fn fk_alias_two_refs_and_self_ref_go_green_on_a_correct_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, FK_ALIAS_LEDGER).unwrap();
    let app = tmp.path().join("ledger-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "the fk-alias design must scaffold");

    // The migration carries the two aliased fk columns + the self-ref column, with
    // two distinct FKs to accounts (distinct constraint names on Postgres, where
    // two same-named table constraints would be rejected at apply).
    for dialect in ["sqlite", "postgres"] {
        let sql = std::fs::read_to_string(app.join(format!(
            "crates/routes/ledger/migrations/{dialect}/0001_create_tables.sql"
        )))
        .unwrap();
        assert!(
            sql.contains("\"from_account_id\"")
                && sql.contains("\"to_account_id\"")
                && !sql.contains("\"account_id\""),
            "{dialect}: both aliased fk columns replace the default account_id:\n{sql}"
        );
        assert_eq!(
            sql.matches("REFERENCES \"accounts\"").count(),
            2,
            "{dialect}: two distinct FKs must reference accounts:\n{sql}"
        );
        assert!(
            sql.contains("\"parent_id\"") && sql.contains("REFERENCES \"comments\""),
            "{dialect}: the self-reference must emit parent_id → comments:\n{sql}"
        );
    }
    let pg = std::fs::read_to_string(
        app.join("crates/routes/ledger/migrations/postgres/0001_create_tables.sql"),
    )
    .unwrap();
    assert!(
        pg.contains("\"fk_transfers_from_account_id\"")
            && pg.contains("\"fk_transfers_to_account_id\"")
            && pg.contains("\"fk_comments_parent_id\""),
        "postgres must name the three FK constraints distinctly:\n{pg}"
    );

    // gen-tests the module (the ledger suite carries the list/show read probes).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "gen-tests", "--module", "ledger"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gen-tests ledger: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // RED: the stub handlers (500) fail the generated read suite.
    let red = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .unwrap();
    assert!(!red.status.success(), "stub handlers must fail the suite");

    // Implement the correct GET-only handlers.
    install_handler(
        &app,
        "crates/routes/ledger/src/handlers.rs",
        FK_ALIAS_HANDLERS,
    );

    // GREEN: the aliased-fk model compiles and the read suite passes.
    let green = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["test", "--workspace"])
        .status()
        .unwrap();
    assert!(
        green.success(),
        "the correct handlers must satisfy the generated read suite"
    );

    // The full gate holds on the implemented app.
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON doc");
    assert_eq!(
        payload["ok"], true,
        "diagnostics: {}",
        payload["diagnostics"]
    );
}

/// A create-surfaced sibling of `FK_ALIAS_LEDGER` (issue #179): the same
/// two-aliased-fk `Transfer belongs_to Account as from_account / as to_account`
/// pair, with `create_account` + `create_transfer` POST endpoints so the
/// two-aliased-fk INSERT can be driven live. Kept separate so the GET-only
/// `fk_alias_two_refs_and_self_ref_go_green_on_a_correct_scaffold` fixture stays
/// byte-identical; the self-referential `Comment` is dropped as irrelevant to
/// the INSERT proof.
const FK_ALIAS_LEDGER_CREATE: &str = r#"{
  "name": "ledger-api",
  "contract_version": 1,
  "dependencies": ["db"],
  "modules": [
    { "name": "ledger",
      "entities": [
        { "name": "Account", "fields": [{ "name": "name", "type": "string" }] },
        { "name": "Transfer",
          "belongs_to": [
            { "entity": "Account", "as": "from_account" },
            { "entity": "Account", "as": "to_account" }
          ],
          "fields": [{ "name": "amount", "type": "integer" }] }
      ],
      "endpoints": [
        { "operation_id": "create_account", "method": "POST", "path": "/accounts",
          "success": { "status": 201, "entity": "Account" } },
        { "operation_id": "show_account", "method": "GET", "path": "/accounts/{id}",
          "success": { "status": 200, "entity": "Account" } },
        { "operation_id": "create_transfer", "method": "POST", "path": "/transfers",
          "success": { "status": 201, "entity": "Transfer" } },
        { "operation_id": "show_transfer", "method": "GET", "path": "/transfers/{id}",
          "success": { "status": 200, "entity": "Transfer" } }
      ] }
  ]
}"#;

/// Correct create/read handlers: the two-aliased-fk INSERT reads BOTH aliased
/// fk columns straight off the request body and persists them distinctly.
const FK_ALIAS_CREATE_HANDLERS: &str = r#"//! Correct #119 create/read handlers: the two-aliased-fk INSERT run live.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn create_account(
    repo: Dep<AccountRepo>,
    Json(body): Json<Account>,
) -> Result<Created<Account>> {
    let id = repo.insert(Account { id: body.id, name: body.name.clone() }).await?;
    Ok(Created(Account { id, name: body.name }))
}

pub(crate) async fn show_account(repo: Dep<AccountRepo>, Path(id): Path<i64>) -> Result<Json<Account>> {
    repo.get(id).await?.map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn create_transfer(
    repo: Dep<TransferRepo>,
    Json(body): Json<Transfer>,
) -> Result<Created<Transfer>> {
    let id = repo
        .insert(Transfer {
            id: body.id,
            from_account_id: body.from_account_id,
            to_account_id: body.to_account_id,
            amount: body.amount,
        })
        .await?;
    Ok(Created(Transfer {
        id,
        from_account_id: body.from_account_id,
        to_account_id: body.to_account_id,
        amount: body.amount,
    }))
}

pub(crate) async fn show_transfer(repo: Dep<TransferRepo>, Path(id): Path<i64>) -> Result<Json<Transfer>> {
    repo.get(id).await?.map(Json).ok_or_else(Error::not_found)
}
"#;

/// Issue #179: the two-aliased-fk INSERT (#119) proven END TO END, live over HTTP.
/// The GET-only sibling above only asserts the migration SQL; the INSERT path
/// (seeding a row under BOTH `from_account_id` and `to_account_id`) was never run.
/// Here the app scaffolds, the correct insert handlers COMPILE, and — served on a
/// real port over a fresh sqlite file — two `Account` rows are POSTed, then a
/// `Transfer` referencing BOTH via the two DISTINCT aliased fks: it must 201 and
/// the persisted row (GET round-trip) must carry the two distinct fk values.
///
/// The hand-driven POST (not the generated create probe) is deliberate: the
/// generated happy-path probe posts `{}`, which cannot express the two required
/// aliased fk values nor seed the referenced accounts — so it is an AGENT-TODO
/// stub for this shape, not a valid INSERT proof. Framework code is untouched;
/// this is coverage of already-shipped #119 behaviour.
#[test]
#[ignore = "heavy: scaffold + build + serve + live two-aliased-fk INSERT over HTTP (#119/#179)"]
fn fk_alias_two_refs_insert_persists_both_aliased_fks_live() {
    let tmp = tempfile::tempdir().unwrap();
    let design = tmp.path().join("design.json");
    std::fs::write(&design, FK_ALIAS_LEDGER_CREATE).unwrap();
    let app = tmp.path().join("ledger-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .args(["new"])
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success(), "the fk-alias create design must scaffold");

    install_handler(
        &app,
        "crates/routes/ledger/src/handlers.rs",
        FK_ALIAS_CREATE_HANDLERS,
    );

    // Compile the app binary (proves the two-aliased-fk insert handlers build
    // against the generated aliased Model/repo) before serving it.
    let build = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["build", "-p", "app"])
        .status()
        .unwrap();
    assert!(build.success(), "the app with insert handlers must compile");

    // Serve live over a fresh sqlite file (so migrations run clean and account
    // ids start at 1); `mode=rwc` lets sqlx CREATE the file.
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");
    let db_file = app.join("live.db");
    let _ = std::fs::remove_file(&db_file);
    let db_url = format!("sqlite://{}?mode=rwc", db_file.display());
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .env("JERRYCAN_ADDR", &addr)
        .env("JERRYCAN_SECRET", "a-very-long-development-secret-string!!")
        .env("JERRYCAN_DATABASE_URL", &db_url)
        .args(["run", "-p", "app"])
        .spawn()
        .unwrap();

    // Drive the battery; ALWAYS kill the server afterwards, even on a panic.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        await_listen(&addr, 180);

        // Seed two DISTINCT accounts (ids assigned by the DB, read from the
        // responses so the proof never assumes the autoincrement values).
        let a1 = http_post_json(&addr, "/ledger/accounts", r#"{"name":"alice"}"#);
        assert_eq!(http_status(&a1), 201, "create account A must 201:\n{a1}");
        let from_id = http_json_body(&a1)["id"].as_i64().expect("account A id");
        let a2 = http_post_json(&addr, "/ledger/accounts", r#"{"name":"bob"}"#);
        assert_eq!(http_status(&a2), 201, "create account B must 201:\n{a2}");
        let to_id = http_json_body(&a2)["id"].as_i64().expect("account B id");
        assert_ne!(from_id, to_id, "the two accounts must be distinct rows");

        // The two-aliased-fk INSERT: a Transfer referencing BOTH accounts.
        let body =
            format!(r#"{{"from_account_id":{from_id},"to_account_id":{to_id},"amount":100}}"#);
        let created = http_post_json(&addr, "/ledger/transfers", &body);
        assert_eq!(
            http_status(&created),
            201,
            "the two-aliased-fk INSERT must 201:\n{created}"
        );
        let created = http_json_body(&created);
        assert_eq!(
            created["from_account_id"].as_i64(),
            Some(from_id),
            "created transfer echoes from_account_id"
        );
        assert_eq!(
            created["to_account_id"].as_i64(),
            Some(to_id),
            "created transfer echoes to_account_id"
        );

        // Round-trip: the PERSISTED row carries the two distinct aliased fks.
        let tid = created["id"].as_i64().expect("transfer id");
        let got = http_json_body(&http_get(&addr, &format!("/ledger/transfers/{tid}")));
        assert_eq!(
            got["from_account_id"].as_i64(),
            Some(from_id),
            "persisted from_account_id"
        );
        assert_eq!(
            got["to_account_id"].as_i64(),
            Some(to_id),
            "persisted to_account_id"
        );
        assert_ne!(
            got["from_account_id"], got["to_account_id"],
            "the two aliased fks persist as DISTINCT values: {got}"
        );
    }));
    let _ = server.kill();
    let _ = server.wait();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
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
/// POST a JSON body over raw HTTP and return the whole response (status line +
/// headers + body). No auth header — the fk-alias ledger design is unguarded.
fn http_post_json(addr: &str, path: &str, body: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
/// The numeric status of a raw HTTP response (`HTTP/1.1 201 Created` → 201).
fn http_status(raw: &str) -> u16 {
    raw.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status in response:\n{raw}"))
}
/// Parse the JSON body (everything past the header terminator) of a raw response.
fn http_json_body(raw: &str) -> serde_json::Value {
    let body = raw.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
    serde_json::from_str(body.trim()).unwrap_or_else(|e| panic!("body not JSON ({e}):\n{raw}"))
}
/// True when `jerrycan package --binary` produced a static musl binary (so a
/// distroless/static runtime base is appropriate); false ⇒ a gnu host binary.
fn musl_built(app: &Path) -> bool {
    app.join("target/x86_64-unknown-linux-musl/release/app")
        .exists()
}
