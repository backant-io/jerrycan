//! Empirical proof that an emitted route crate (entities → repo + stub handlers)
//! compiles under `-D warnings`. This reproduces the reviewer's finding that the
//! in-memory repo is dead code while handlers are stubs, and pins the fix.
//!
//! Heavy (invokes cargo on a generated crate), so it's `#[ignore]`d for the fast
//! suite. Run explicitly:
//!   cargo test -p jerrycan --test genroute_compile -- --include-ignored
//!
//! Predates Task 9's fuller scaffold-driven compile gate; cheap insurance until then.

use jerrycan::platform::design::Design;
use jerrycan::platform::genroute::{GenMode, write_module};
use std::fs;
use std::path::Path;

mod common;
use std::process::Command;

/// Mirror of design::tests::MINIMAL (that const is `#[cfg(test)]`-private to the
/// crate, so integration tests inline it). The todos module has an entity (Todo)
/// → it emits model.rs + repo.rs, the exact dead-code-vs-stub shape under test.
/// The Todo also carries a field named `type` — a Rust keyword — so this compile
/// gate empirically proves issue #10: the emitted `r#type` raw identifier (Model
/// field + ActiveModel binds) compiles under `-D warnings`.
const MINIMAL: &str = r#"{
    "name": "demo-api",
    "contract_version": 0,
    "auth": { "model": "session", "roles": ["admin"] },
    "dependencies": ["db"],
    "modules": [{
        "name": "todos",
        "entities": [{ "name": "Todo", "fields": [
            { "name": "title", "type": "string" },
            { "name": "type", "type": "string", "required": false },
            { "name": "done", "type": "boolean", "required": false }
        ]}],
        "endpoints": [
            { "operation_id": "list_todos", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Todo", "list": true } },
            { "operation_id": "create_todo", "method": "POST", "path": "/",
              "request_body": { "entity": "Todo" },
              "success": { "status": 201, "entity": "Todo" } },
            { "operation_id": "delete_todo", "method": "DELETE", "path": "/{id}",
              "required_roles": ["admin"],
              "success": { "status": 204 },
              "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }] }
        ],
        "subroutes": [{
            "name": "comments",
            "endpoints": [{ "operation_id": "list_comments", "method": "GET", "path": "/",
                            "success": { "status": 200 } }]
        }]
    }]
}"#;

/// Absolute path to this crate's source dir (the local `jerrycan` facade crate),
/// resolved at compile time so the generated workspace can depend on it by path.
fn jerrycan_crate_dir() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

#[test]
#[ignore = "invokes cargo on a generated crate; run with --include-ignored"]
fn generated_module_crate_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path();
    let routes = app.join("crates/routes");

    // 1. Emit the todos route crate (model.rs + repo.rs + stub handlers.rs).
    let design: Design = serde_json::from_str(MINIMAL).expect("MINIMAL parses");
    // This test isolates the db dead-code-vs-stub shape; auth guards are exercised
    // by the conformance auth_observe test, so keep auth off here (the minimal
    // shared crate has no CurrentUser alias).
    let mode = GenMode {
        db: design.wants_db(),
        auth: false,
    };
    let module = design.modules.first().expect("todos module");
    let created = write_module(&routes, module, mode, &design).expect("write_module");
    assert!(
        created.iter().any(|p| p.ends_with("todos/src/repo.rs")),
        "the entity-bearing module must emit repo.rs (the dead-code case): {created:?}"
    );

    // 2. Wrap it in a minimal workspace pointing jerrycan at the local crate by
    //    path with `default-features = false` (lib facade only — no `cli`), which
    //    is exactly how a scaffolded app depends on the framework.
    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let workspace_cargo = format!(
        r#"[workspace]
resolver = "3"
members = [
    "crates/shared",
    "crates/routes/todos",
]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
jerrycan = {{ path = "{jerrycan_dir}", default-features = false, features = ["db"] }}
tokio = {{ version = "1", features = ["macros", "rt-multi-thread", "net", "time", "sync"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
"#
    );
    write(&app.join("Cargo.toml"), &workspace_cargo);

    // shared crate referenced by the route crate's `shared = { path = "../../shared" }`.
    let shared = app.join("crates/shared");
    write(
        &shared.join("Cargo.toml"),
        r#"[package]
name = "shared"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
"#,
    );
    write(&shared.join("src/lib.rs"), "#![forbid(unsafe_code)]\n");

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // 3. The gate the reviewer ran: strict clippy over all targets.
    let output = Command::new(env!("CARGO"))
        .current_dir(app)
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        // Shared cargo target dir across scaffolded apps (see
        // common::shared_app_target): deps compile once; the heavy suite is
        // single-threaded so the shared output path is never contended.
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");

    if !output.status.success() {
        panic!(
            "emitted crate failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A minimal AUTH-mode design: one module with a guarded POST endpoint. The
/// endpoint's `auth_required: true` makes it take a `_user: CurrentUser` param,
/// so handlers.rs imports `shared::CurrentUser`. There are no `required_roles`,
/// so the stub never calls `require_role` — which is precisely the case that
/// regressed: emitting `use jerrycan::auth::{require_role, Session};` here left
/// both imports unused, tripping `clippy -D warnings` on untouched generated code.
const AUTH_MINIMAL: &str = r#"{
    "name": "auth-api",
    "contract_version": 0,
    "auth": { "model": "session", "roles": ["admin"] },
    "dependencies": ["auth"],
    "modules": [{
        "name": "secrets",
        "entities": [{ "name": "Secret", "fields": [
            { "name": "value", "type": "string" }
        ]}],
        "endpoints": [
            { "operation_id": "create_secret", "method": "POST", "path": "/",
              "auth_required": true,
              "request_body": { "entity": "Secret" },
              "success": { "status": 201, "entity": "Secret" } }
        ]
    }]
}"#;

/// Companion to `generated_module_crate_passes_strict_clippy`, for AUTH mode.
/// Uses the REAL scaffold (so the `shared` crate gets the genuine
/// `CurrentUser = Session<SessionUser>` alias and the route crate gets
/// `features = ["auth"]`) and asserts the raw generated stubs — never touched by
/// any agent or fixture — pass strict clippy. This is the gate that was missing:
/// the existing compile test runs with `auth: false`, so it could not catch an
/// unused auth import.
#[test]
#[ignore = "invokes cargo on a scaffolded auth crate; run with --include-ignored"]
fn generated_auth_module_crate_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("auth-app");

    // Sanity-check the fixture design parses and is in auth mode before scaffolding.
    let design: Design = serde_json::from_str(AUTH_MINIMAL).expect("AUTH_MINIMAL parses");
    assert!(design.wants_auth(), "design must be in auth mode");

    // Run the real scaffold via the jerrycan BINARY (the same pattern conformance.rs
    // uses): the framework dep is passed through the CHILD's environment, so no
    // `unsafe set_var` on this process is needed. The dep points at this local crate
    // (carrying the `auth` feature, injected by scaffold from facade_features) so the
    // shared crate's CurrentUser alias and the route crate's wiring are exactly what a
    // real `jerrycan new` produces.
    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, AUTH_MINIMAL);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold the auth app");

    // Sanity: the generated handler stub imports CurrentUser but NOT require_role.
    let handlers = fs::read_to_string(app.join("crates/routes/secrets/src/handlers.rs"))
        .expect("read generated handlers.rs");
    assert!(
        handlers.contains("use shared::CurrentUser;"),
        "guarded stub must import the param type it uses:\n{handlers}"
    );
    assert!(
        !handlers.contains("use jerrycan::auth::"),
        "raw stub must NOT import require_role/Session — it uses neither:\n{handlers}"
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // The gate the reviewer ran, scoped to the generated route crate.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "-p",
            "route-secrets",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");

    if !output.status.success() {
        panic!(
            "scaffolded auth route crate failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A minimal JWT design with realtime (issue #29): a guarded route + a `changes`
/// channel. The `jwt` model flips the shared alias to `Bearer<SessionUser>` AND
/// the realtime `?token=` fallback to `jerrycan::auth::Bearer(claims)` — the two
/// MUST move in lockstep or the realtime `match` arms disagree on the guard type
/// and the realtime crate won't compile. This is the compile gate for that
/// coupling (the unit tests pin the emitted strings; this proves they build).
const JWT_REALTIME: &str = r#"{
    "name": "jwt-rt",
    "contract_version": 2,
    "auth": { "model": "jwt", "roles": ["admin"] },
    "dependencies": ["db", "auth", "realtime"],
    "modules": [{
        "name": "notes",
        "entities": [{ "name": "Note", "fields": [
            { "name": "text", "type": "string", "required": true }
        ]}],
        "endpoints": [
            { "operation_id": "list_notes", "method": "GET", "path": "/",
              "auth_required": true,
              "success": { "status": 200, "entity": "Note", "list": true } },
            { "operation_id": "create_note", "method": "POST", "path": "/",
              "auth_required": true,
              "request_body": { "entity": "Note" },
              "success": { "status": 201, "entity": "Note" } }
        ]
    }],
    "realtime": { "changes": ["Note"], "broadcast": [{ "name": "note_created", "scope": "auth" }], "presence": [] }
}"#;

/// The same realtime-publish design under the SESSION auth model — the server
/// publish API (issue #50) must compile under both auth models (the P5 realtime
/// coupling). Session flips the shared alias to `Session<SessionUser>` and the
/// resolver to plain `CurrentUser::from_request` (no `?token=`), but the write
/// handler's `Dep<RealtimeHandle>` publish path is identical.
const SESSION_REALTIME: &str = r#"{
    "name": "sess-rt",
    "contract_version": 2,
    "auth": { "model": "session", "roles": ["admin"] },
    "dependencies": ["db", "auth", "realtime"],
    "modules": [{
        "name": "notes",
        "entities": [{ "name": "Note", "fields": [
            { "name": "text", "type": "string", "required": true }
        ]}],
        "endpoints": [
            { "operation_id": "list_notes", "method": "GET", "path": "/",
              "auth_required": true,
              "success": { "status": 200, "entity": "Note", "list": true } },
            { "operation_id": "create_note", "method": "POST", "path": "/",
              "auth_required": true,
              "request_body": { "entity": "Note" },
              "success": { "status": 201, "entity": "Note" } }
        ]
    }],
    "realtime": { "changes": ["Note"], "broadcast": [{ "name": "note_created", "scope": "auth" }], "presence": [] }
}"#;

/// Given a scaffolded realtime-publish app (module `notes`, an `auth`-scoped
/// broadcast topic `note_created`, a write endpoint `create_note`), assert the
/// generator wired the `Dep<RealtimeHandle>` param + the publish stub comment
/// (issue #50), IMPLEMENT the advertised one-liner (the agent's edit), and
/// require the route + realtime crates to pass strict clippy — so the server
/// publish call is compiled against the live facade, not just asserted as a
/// string. Shared by the jwt and session gates.
fn implement_publish_and_clippy(app: &std::path::Path) {
    let handlers_path = app.join("crates/routes/notes/src/handlers.rs");
    let handlers = fs::read_to_string(&handlers_path).expect("read notes handlers");
    assert!(
        handlers.contains("_rt: Dep<jerrycan::realtime::RealtimeHandle>"),
        "the write handler must take the RealtimeHandle dep:\n{handlers}"
    );
    assert!(
        handlers.contains("_rt.publish(\"note_created\", serde_json::json!("),
        "the stub comment must show the publish one-liner on the declared topic:\n{handlers}"
    );
    // The read handler must NOT gain the dep (canonical pattern is write→push).
    let before_create = handlers
        .split("pub(crate) async fn create_note")
        .next()
        .expect("list_notes precedes create_note");
    assert!(
        !before_create.contains("_rt"),
        "read handlers must not gain the realtime dep:\n{before_create}"
    );
    // Implement the one-liner the comment advertises: the publish call must
    // type-check against the real facade (the return keeps the stub's Err).
    // The stub body wraps (op_len 11 ⇒ the inner `Error::internal("…")` call exceeds
    // rustfmt's fn_call_width, issue #165), so match the wrapped form.
    let implemented = handlers.replace(
        "    Err(Error::internal(\n        \"create_note not implemented — replace this stub\",\n    ))",
        "    _rt.publish(\"note_created\", serde_json::json!({ \"type\": \"created\" })).await?;\n    Err(Error::internal(\"realtime publish wired\"))",
    );
    assert_ne!(
        implemented, handlers,
        "the create_note stub must be replaced with a real publish call"
    );
    write(&handlers_path, &implemented);

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    let output = Command::new(env!("CARGO"))
        .current_dir(app)
        .args([
            "clippy",
            "-p",
            "route-notes",
            "-p",
            "realtime",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "realtime-publish app failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
#[ignore = "invokes cargo on a scaffolded jwt+realtime app; run with --include-ignored"]
fn generated_jwt_realtime_app_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("jwt-rt-app");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, JWT_REALTIME);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(
        status.success(),
        "jerrycan new must scaffold the jwt+realtime app"
    );

    // The jwt model flips the shared guard alias to Bearer...
    let shared = fs::read_to_string(app.join("crates/shared/src/lib.rs")).expect("read shared");
    assert!(
        shared.contains("pub type CurrentUser = jerrycan::auth::Bearer<SessionUser>;"),
        "jwt design must alias CurrentUser to Bearer:\n{shared}"
    );
    // ...and the realtime `?token=` fallback wraps claims in Bearer to match it.
    let rt = fs::read_to_string(app.join("crates/realtime/src/lib.rs")).expect("read realtime");
    assert!(
        rt.contains("jerrycan::auth::Bearer(claims)")
            && !rt.contains("jerrycan::auth::Session(claims)"),
        "realtime jwt fallback must wrap claims in Bearer (lockstep with the alias):\n{rt}"
    );

    // Both the guarded route crate and the realtime crate must build — the realtime
    // crate is where the guard-type coupling would surface as a compile error — AND
    // the write handler's server-side publish one-liner (issue #50) must compile.
    implement_publish_and_clippy(&app);
}

/// The server publish API (issue #50) must also compile under the SESSION auth
/// model (the P5 realtime coupling: jwt AND session move in lockstep). Scaffolds
/// a session+realtime app, implements the emitted publish one-liner, and gates
/// on strict clippy over the route + realtime crates.
#[test]
#[ignore = "invokes cargo on a scaffolded session+realtime app; run with --include-ignored"]
fn generated_session_realtime_app_handler_publishes_broadcast() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("sess-rt-app");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, SESSION_REALTIME);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(
        status.success(),
        "jerrycan new must scaffold the session+realtime app"
    );

    // Session model: the realtime resolver reads the session cookie via
    // CurrentUser (no `?token=` fallback, no Bearer wrapping).
    let rt = fs::read_to_string(app.join("crates/realtime/src/lib.rs")).expect("read realtime");
    assert!(
        rt.contains("shared::CurrentUser") && !rt.contains("jerrycan::auth::Bearer(claims)"),
        "session realtime resolver uses CurrentUser, not a Bearer/token fallback:\n{rt}"
    );

    implement_publish_and_clippy(&app);
}

/// A minimal JOBS-mode design: db + two jobs — one CRON (`expire_trials`, with a
/// schedule + a named queue) and one QUEUE-only (`send_email`, no schedule → a
/// `{Name}Payload` struct + the 2-arg stub). This exercises BOTH generated task
/// shapes plus the registry's cron/queue closure wiring in one crate. Jobs
/// require a db (validation enforces it), so the design declares `db`.
const JOBS_MINIMAL: &str = r#"{
    "name": "jobs-api",
    "contract_version": 1,
    "dependencies": ["db"],
    "jobs": [
        { "name": "expire_trials", "schedule": "0 * * * *", "queue": "billing" },
        { "name": "send_email" }
    ],
    "modules": [{
        "name": "things",
        "endpoints": [
            { "operation_id": "list_things", "method": "GET", "path": "/",
              "success": { "status": 200 } }
        ]
    }]
}"#;

/// THE Task 7 compile gate: the generated top-level `crates/jobs/` crate (the
/// dispatch registry + the wired `Jobs` extension + the typed task stubs) must
/// compile under strict clippy. Uses the REAL scaffold via the jerrycan binary
/// (so the jobs crate gets the genuine facade dep with the `jobs` feature, the
/// workspace member, and the app dep) and asserts the raw generated stubs — never
/// touched by any agent — pass `-D warnings`. Exercises both job shapes: a cron
/// 1-arg stub and a queue 2-arg stub + payload struct.
#[test]
#[ignore = "invokes cargo on a scaffolded jobs crate; run with --include-ignored"]
fn generated_jobs_crate_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("jobs-app");

    let design: Design = serde_json::from_str(JOBS_MINIMAL).expect("JOBS_MINIMAL parses");
    assert!(design.wants_jobs(), "design must declare jobs");
    assert!(design.wants_db(), "jobs require db");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, JOBS_MINIMAL);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold the jobs app");

    // The tool-owned registry wires the cron 1-arg closure and the queue 2-arg
    // closure (with payload deserialization); the agent task modules carry the
    // matching stub shapes.
    let lib = fs::read_to_string(app.join("crates/jobs/src/lib.rs")).expect("read jobs lib.rs");
    assert!(
        lib.contains("Box::pin(expire_trials::expire_trials(ctx))"),
        "cron closure (1-arg) must be wired:\n{lib}"
    );
    assert!(
        lib.contains("send_email::send_email(ctx, p).await"),
        "queue closure (2-arg, deserialized payload) must be wired:\n{lib}"
    );
    let cron = fs::read_to_string(app.join("crates/jobs/src/expire_trials.rs")).expect("cron stub");
    assert!(
        cron.contains("pub async fn expire_trials(mut _ctx: TaskContext)"),
        "cron stub is 1-arg owned ctx:\n{cron}"
    );
    let queue = fs::read_to_string(app.join("crates/jobs/src/send_email.rs")).expect("queue stub");
    assert!(
        queue.contains("pub struct SendEmailPayload {}")
            && queue.contains(
                "pub async fn send_email(mut _ctx: TaskContext, _payload: SendEmailPayload)"
            ),
        "queue stub has payload struct + 2-arg fn:\n{queue}"
    );
    // The app wires the extension + runs JOBS_MIGRATIONS.
    let main = fs::read_to_string(app.join("crates/app/src/main.rs")).expect("main.rs");
    assert!(
        main.contains(".extend(jobs::jobs(db.clone()))")
            && main.contains("db.migrate(jerrycan::jobs::JOBS_MIGRATIONS)"),
        "main.rs must wire the jobs extension + migrations:\n{main}"
    );

    write(&app.join("rust-toolchain.toml"), "");

    // The gate: strict clippy over the generated jobs crate.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "-p",
            "jobs",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");

    if !output.status.success() {
        panic!(
            "generated jobs crate failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A contract-v2 STORAGE design covering all four bucket scope variants:
/// `avatars` = public + owner User (plain user scope), `invoices` = private +
/// owner Org (the tenancy entity) + owner_prefix (tenant scope), `exports` =
/// private + no owner (unowned), `reports` = private + owner Member where
/// Member belongs_to Org (user-in-tenant scope).
const STORAGE_MINIMAL: &str = r#"{
    "name": "files-app", "contract_version": 2,
    "auth": { "model": "session", "roles": ["owner", "member"] },
    "dependencies": ["db", "auth"],
    "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
    "storage": { "buckets": [
        { "name": "avatars", "visibility": "public", "owner": "User",
          "owner_prefix": true, "max_size": "1MB", "allowed_mime": ["image/*"] },
        { "name": "invoices", "visibility": "private", "owner": "Org",
          "owner_prefix": true, "max_size": "1MB" },
        { "name": "exports", "visibility": "private" },
        { "name": "reports", "visibility": "private", "owner": "Member", "owner_prefix": true }
    ]},
    "modules": [
        { "name": "orgs",
          "entities": [
              { "name": "Org", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "plan", "type": "string" } ] },
              { "name": "User", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "email", "type": "string" } ] },
              { "name": "Member", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "nick", "type": "string" } ],
                "belongs_to": [{ "entity": "Org" }] }
          ],
          "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/", "auth_required": true,
              "success": { "status": 200, "entity": "Org", "list": true } }] }
    ]
}"#;

/// THE storage compile gate (review 2026-07-11 finding: nothing in-repo proved
/// the generated storage code COMPILES — only substring unit tests existed).
/// Scaffolds a real storage app via the jerrycan binary (all four bucket scope
/// variants), then requires the generated `storage` crate to pass strict
/// clippy AND its own generated acceptance + isolation test battery.
#[test]
#[ignore = "scaffolds a storage app and invokes cargo on it; run with --include-ignored"]
fn generated_storage_crate_passes_strict_clippy_and_its_acceptance_tests() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("files-app");

    let design: Design = serde_json::from_str(STORAGE_MINIMAL).expect("STORAGE_MINIMAL parses");
    assert!(design.wants_storage(), "design must declare buckets");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, STORAGE_MINIMAL);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(
        status.success(),
        "jerrycan new must scaffold the storage app"
    );

    // Sanity: each scope variant emitted its distinguishing guard signature.
    let read =
        |rel: &str| fs::read_to_string(app.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let avatars = read("crates/storage/src/avatars.rs");
    assert!(
        avatars.contains("user: CurrentUser,") && !avatars.contains("Tenant"),
        "avatars is the plain user scope:\n{avatars}"
    );
    let invoices = read("crates/storage/src/invoices.rs");
    assert!(
        invoices.contains("tenant: Dep<Tenant>,") && invoices.contains("owner_prefix: true"),
        "invoices is the tenant + prefix scope:\n{invoices}"
    );
    let exports = read("crates/storage/src/exports.rs");
    assert!(
        exports.contains("_user: CurrentUser,") && exports.contains("Scope::default()"),
        "exports is the unowned scope:\n{exports}"
    );
    let reports = read("crates/storage/src/reports.rs");
    assert!(
        reports.contains("user: CurrentUser, tenant: Dep<Tenant>,"),
        "reports is the user-in-tenant scope:\n{reports}"
    );
    assert!(
        app.join("crates/storage/tests/acceptance.rs").is_file(),
        "the generated acceptance battery must exist"
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // Gate 1: strict clippy over the generated storage crate — all targets,
    // so the acceptance tests must COMPILE under -D warnings too.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "-p",
            "storage",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "generated storage crate failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // Gate 2: the generated acceptance + isolation tests must PASS — they are
    // the per-bucket security contract (cross-owner/tenant/prefix negative
    // controls), generated as real tests, not stubs.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args(["test", "-p", "storage"])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo test");
    if !output.status.success() {
        panic!(
            "generated storage acceptance tests failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// The same four-scope storage design, but the tenant (`Org`) and its owner
/// entities key on a `uuid` pk — the shape a migrated Supabase project produces
/// (auth.users + tenant rows are uuid). Proves the stringified-pk identity end to
/// end: the generated acceptance battery seeds uuid-keyed memberships, mints
/// session cookies, and runs the cross-owner/tenant/prefix isolation controls.
const STORAGE_UUID: &str = r#"{
    "name": "files-app", "contract_version": 2,
    "auth": { "model": "session", "roles": ["owner", "member"] },
    "dependencies": ["db", "auth"],
    "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
    "storage": { "buckets": [
        { "name": "avatars", "visibility": "public", "owner": "User",
          "owner_prefix": true, "max_size": "1MB", "allowed_mime": ["image/*"] },
        { "name": "invoices", "visibility": "private", "owner": "Org",
          "owner_prefix": true, "max_size": "1MB" },
        { "name": "exports", "visibility": "private" },
        { "name": "reports", "visibility": "private", "owner": "Member", "owner_prefix": true }
    ]},
    "modules": [
        { "name": "orgs",
          "entities": [
              { "name": "Org", "fields": [
                  { "name": "id", "type": "uuid" },
                  { "name": "plan", "type": "string" } ] },
              { "name": "User", "fields": [
                  { "name": "id", "type": "uuid" },
                  { "name": "email", "type": "string" } ] },
              { "name": "Member", "fields": [
                  { "name": "id", "type": "uuid" },
                  { "name": "nick", "type": "string" } ],
                "belongs_to": [{ "entity": "Org" }] }
          ],
          "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/", "auth_required": true,
              "success": { "status": 200, "entity": "Org", "list": true } }] }
    ]
}"#;

/// Runtime proof of the uuid identity fix: a storage app whose tenant/owner pks
/// are uuid must scaffold, compile under strict clippy, AND pass its generated
/// acceptance + cross-tenant isolation battery — real (non-stub) handlers, so a
/// broken identity (i64 session id, bigint membership user_id) would turn it red.
#[test]
#[ignore = "scaffolds a uuid-tenant storage app and invokes cargo on it; run with --include-ignored"]
fn generated_uuid_tenant_storage_crate_passes_its_acceptance_tests() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("files-app");

    let design: Design = serde_json::from_str(STORAGE_UUID).expect("STORAGE_UUID parses");
    assert!(design.wants_storage(), "design must declare buckets");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, STORAGE_UUID);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold the uuid app");

    // The membership DDL types user_id + the uuid tenant fk as TEXT.
    let members_ddl =
        fs::read_to_string(app.join("crates/routes/orgs/migrations/sqlite/0001_create_tables.sql"))
            .expect("read members DDL")
            .to_lowercase();
    assert!(
        members_ddl.contains("\"user_id\" text") && members_ddl.contains("\"org_id\" text"),
        "membership user_id + uuid tenant fk are TEXT:\n{members_ddl}"
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // The generated storage acceptance + isolation tests must PASS (real handlers,
    // uuid-keyed memberships seeded, cross-owner/tenant/prefix negative controls).
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args(["test", "-p", "storage"])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo test");
    if !output.status.success() {
        panic!(
            "uuid-tenant storage acceptance tests failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A contract-v2 REALTIME design: jwt auth + tenancy + a `realtime` block with a
/// tenant-scoped changes entity (`Lead` belongs_to the tenancy entity), a
/// broadcast topic, and a presence topic. This is the exact shape a migrated
/// Supabase project produces, and the shape whose generated wiring crate did NOT
/// compile (the resolver called `user.id()`/`tenant.role()` and `CurrentUser::from`
/// on a `SessionUser`, none of which exist on the real API).
const REALTIME_MINIMAL: &str = r#"{
    "name": "rt-app", "contract_version": 2,
    "auth": { "model": "jwt", "roles": ["owner", "member"] },
    "dependencies": ["db", "auth"],
    "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
    "realtime": {
        "changes": ["Lead"],
        "broadcast": [{ "name": "deal_room", "scope": "tenant" }],
        "presence": [{ "name": "editors", "scope": "tenant" }]
    },
    "modules": [
        { "name": "workspaces",
          "entities": [{ "name": "Workspace", "fields": [
              { "name": "id", "type": "integer" }, { "name": "name", "type": "string" } ]}],
          "endpoints": [{ "operation_id": "list_workspaces", "method": "GET", "path": "/", "auth_required": true,
              "success": { "status": 200, "entity": "Workspace", "list": true } }] },
        { "name": "leads",
          "entities": [{ "name": "Lead",
              "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
              "fields": [{ "name": "id", "type": "integer" },
                         { "name": "phone", "type": "string" }] }],
          "endpoints": [{ "operation_id": "list_leads", "method": "GET", "path": "/", "auth_required": true,
              "success": { "status": 200, "entity": "Lead", "list": true } }] }
    ]
}"#;

/// THE realtime compile gate (review 2026-07-12 finding: a real Supabase
/// migration produced a `crates/realtime/` wiring crate that DID NOT COMPILE —
/// the resolver emitted `user.id()`, `tenant.role()`, and `CurrentUser::from(claims)`,
/// none of which match the real auth API, yet every existing test only asserted on
/// the emitted STRING). Scaffolds a real realtime app via the jerrycan binary
/// (jwt + tenancy resolver, all three channel kinds), then requires the generated
/// `realtime` crate to pass strict clippy over ALL targets — which compiles the
/// tool-owned `src/lib.rs` resolver AND the (live-Postgres, `#[ignore]`d)
/// acceptance battery. Had this existed, the broken wiring would have been red.
#[test]
#[ignore = "scaffolds a realtime app and invokes cargo on it; run with --include-ignored"]
fn generated_realtime_crate_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("rt-app");

    let design: Design = serde_json::from_str(REALTIME_MINIMAL).expect("REALTIME_MINIMAL parses");
    assert!(
        design.wants_realtime(),
        "design must declare a realtime block"
    );

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, REALTIME_MINIMAL);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(
        status.success(),
        "jerrycan new must scaffold the realtime app"
    );

    // Sanity: the tool-owned resolver wires all three channel kinds and resolves a
    // jwt principal from a tenant-scoped connection (the exact code path that broke).
    let lib =
        fs::read_to_string(app.join("crates/realtime/src/lib.rs")).expect("read realtime lib.rs");
    assert!(
        lib.contains(".changes(jerrycan::realtime::ChangeChannelSpec")
            && lib.contains(".broadcast(\"deal_room\"")
            && lib.contains(".presence(\"editors\""),
        "realtime lib must wire changes + broadcast + presence:\n{lib}"
    );
    // #104: the tenant leg is now membership-aware — it reads an optional `?tenant=`,
    // verifies it against the `{tenant}_members` table (refusing a non-member), and
    // falls back to the sole membership — instead of binding an arbitrary first
    // membership via `ctx.resolve::<shared::Tenant>()`. This is the exact resolver
    // whose compile is the whole point of this gate.
    assert!(
        lib.contains(".principal(")
            && lib.contains(r#".and_then(|m| m.get("tenant").cloned())"#)
            && lib.contains("_members WHERE workspace_id = ? AND user_id = ?"),
        "jwt + tenancy design must emit a membership-verified WS tenant-select:\n{lib}"
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // The gate: strict clippy over the generated realtime crate — all targets, so
    // the resolver in src/lib.rs AND the acceptance battery must COMPILE under
    // -D warnings. This is what no test did before: compile the emitted crate.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "-p",
            "realtime",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "generated realtime crate failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A memory-mode design carrying a `cors` block (issue #21): an explicit origin
/// allowlist plus methods/headers/credentials. Memory mode keeps the build cheap
/// (no db) while still emitting the full app/src/main.rs that carries the CORS
/// wiring under test.
const CORS_MINIMAL: &str = r#"{
    "name": "cors-app",
    "contract_version": 0,
    "dependencies": [],
    "cors": {
        "origins": ["https://app.example", "https://admin.example"],
        "methods": ["GET", "POST", "PUT", "PATCH", "DELETE"],
        "headers": ["content-type", "authorization"],
        "allow_credentials": true
    },
    "modules": [{
        "name": "things",
        "endpoints": [
            { "operation_id": "list_things", "method": "GET", "path": "/",
              "success": { "status": 200 } }
        ]
    }]
}"#;

/// THE cors compile gate (issue #21): a `cors` block emits `.cors(CorsConfig::new(..))`
/// plus a `JERRYCAN_CORS_ORIGINS` env-override preamble into the tool-owned
/// app/src/main.rs. Nothing else in the suite compiles that main.rs under
/// -D warnings, so this scaffolds a real app with a cors block and requires the
/// `app` crate to pass strict clippy — empirically proving the emitted CORS wiring
/// (and the env reader) actually compiles, not just that the string looks right.
#[test]
#[ignore = "scaffolds a cors app and invokes cargo on it; run with --include-ignored"]
fn generated_cors_app_main_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("cors-app");

    let design: Design = serde_json::from_str(CORS_MINIMAL).expect("CORS_MINIMAL parses");
    assert!(design.cors.is_some(), "design must declare a cors block");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, CORS_MINIMAL);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold the cors app");

    // Sanity: main.rs carries the emitted CORS layer + the env-override preamble.
    let main = fs::read_to_string(app.join("crates/app/src/main.rs")).expect("read main.rs");
    assert!(
        main.contains("let cors = CorsConfig::new(cors_origins);")
            && main.contains(".cors(cors)")
            && main.contains("std::env::var(\"JERRYCAN_CORS_ORIGINS\")"),
        "main.rs must wire the split CORS builder (`let cors = CorsConfig::new(..)` + \
         `.cors(cors)`, the #128 fmt-fixpoint form) with the env override:\n{main}"
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // The gate: strict clippy over the generated `app` crate compiles main.rs — the
    // emitted `.cors(..)` layer and the JERRYCAN_CORS_ORIGINS reader must hold up
    // under -D warnings (tool-owned code an agent never touches).
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "-p",
            "app",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "generated cors app failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// The agent-eval LinkVault shape (issue #34): `Collection` belongs_to the auth
/// identity entity (`User`, cross-module), every collections endpoint guarded.
/// Before the server-owned-FK rule, the generated `Json<Collection>` body made
/// `user_id` REQUIRED on the wire — a clean client omitting it got a 422 before
/// the handler could inject the session user's id.
const LINKVAULT: &str = r#"{
    "name": "linkvault",
    "contract_version": 1,
    "auth": { "model": "session", "roles": ["admin"] },
    "dependencies": ["db", "auth"],
    "modules": [
        { "name": "users",
          "entities": [{ "name": "User", "fields": [
              { "name": "email", "type": "string" } ]}],
          "endpoints": [
              { "operation_id": "list_users", "method": "GET", "path": "/",
                "auth_required": true,
                "success": { "status": 200, "entity": "User", "list": true } }
          ] },
        { "name": "collections",
          "entities": [{ "name": "Collection",
              "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
              "fields": [{ "name": "title", "type": "string" }] }],
          "endpoints": [
              { "operation_id": "create_collection", "method": "POST", "path": "/",
                "auth_required": true,
                "request_body": { "entity": "Collection" },
                "success": { "status": 201, "entity": "Collection" } },
              { "operation_id": "list_collections", "method": "GET", "path": "/",
                "auth_required": true,
                "success": { "status": 200, "entity": "Collection", "list": true } }
          ] }
    ]
}"#;

/// The agent's side of the e2e: a real `create_collection` that INJECTS the
/// session user's id (the body has no `user_id`) plus a plain list handler.
/// This is exactly what the generated stub comment tells the agent to write.
const LINKVAULT_HANDLERS: &str = r#"//! E2E fixture: implemented handlers for the collections module.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use shared::CurrentUser;

pub(crate) async fn create_collection(
    repo: Dep<CollectionRepo>,
    user: CurrentUser,
    Json(body): Json<CollectionRequest>,
) -> Result<Created<Collection>> {
    // server-owned fk: the session, not the client, decides user_id.
    let user_id: i64 = user
        .0
        .id
        .parse()
        .map_err(|_| Error::internal("session id is not an integer"))?;
    let id = repo
        .insert(Collection { id: body.id, user_id, title: body.title })
        .await?;
    // Owner-scoped read: the per-user repo emits only *_for(user_id) accessors
    // (the unscoped get/all are not generated — #79 make-impossible).
    let row = repo.get_for(user_id, id).await?.ok_or_else(Error::not_found)?;
    Ok(Created(row))
}

pub(crate) async fn list_collections(
    repo: Dep<CollectionRepo>,
    user: CurrentUser,
) -> Result<Json<Vec<Collection>>> {
    let user_id: i64 = user
        .0
        .id
        .parse()
        .map_err(|_| Error::internal("session id is not an integer"))?;
    Ok(Json(repo.all_for(user_id).await?))
}
"#;

/// THE issue #34 e2e gate: scaffold the LinkVault shape, implement the handlers
/// the way the stub comment instructs, and require (1) the whole scaffolded
/// workspace to pass strict clippy — the DTO emission and the untouched users
/// stubs compile under `-D warnings` — and (2) the GENERATED acceptance battery
/// for the collections module to PASS. Its create probe posts a body WITHOUT
/// `user_id`; green means the eval's 422 scenario is fixed end to end (contract,
/// probe, and wire behavior agree: the server injects the session user's id).
#[test]
#[ignore = "scaffolds an app and invokes cargo on it; run with --include-ignored"]
fn guarded_identity_fk_scaffold_accepts_bodies_without_user_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("linkvault");

    let design: Design = serde_json::from_str(LINKVAULT).expect("LINKVAULT parses");
    assert!(design.wants_auth() && design.wants_db(), "auth + db mode");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, LINKVAULT);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold linkvault");

    // Generate the acceptance battery for the module under test (gen-tests is
    // a separate step from `new`, mirroring the real agent workflow).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["gen-tests", "--module", "collections"])
        .output()
        .expect("run jerrycan gen-tests");
    assert!(
        out.status.success(),
        "gen-tests failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The emitted artifacts carry the server-owned-FK shape end to end.
    let read =
        |rel: &str| fs::read_to_string(app.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let model = read("crates/routes/collections/src/model.rs");
    assert!(
        model.contains("pub struct CollectionRequest"),
        "request DTO emitted:\n{model}"
    );
    let handlers = read("crates/routes/collections/src/handlers.rs");
    assert!(
        handlers.contains("Json(_body): Json<CollectionRequest>")
            && handlers.contains("server-owned fk"),
        "stub takes the DTO and says the server injects user_id:\n{handlers}"
    );
    let acceptance = read("crates/routes/collections/tests/acceptance.rs");
    // The JSON key form (`"user_id"`) must be absent from every probe body;
    // the bare identifier still appears in the `test_cookie_for(user_id: i64)`
    // preamble helper, which is fine.
    assert!(
        acceptance.contains("serde_json::json!({\"title\": \"test-value\"})")
            && !acceptance.contains("\"user_id\""),
        "generated probe bodies must omit user_id:\n{acceptance}"
    );

    // Implement the handlers the way the stub comment instructs.
    write(
        &app.join("crates/routes/collections/src/handlers.rs"),
        LINKVAULT_HANDLERS,
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // Gate 1: the scaffolded WORKSPACE (DTO, implemented handlers, untouched
    // users stubs, generated tests) compiles under strict clippy.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "linkvault workspace failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // Gate 2: the generated acceptance battery passes — the create probe posts
    // WITHOUT user_id and must reach 201 (the eval's 422 scenario, now green).
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args(["test", "-p", "route-collections"])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo test");
    if !output.status.success() {
        panic!(
            "linkvault generated acceptance tests failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// The #150 make-impossible half: the LinkVault shape under an OPT-IN
/// `auth.identity: "Account"` (identity entity `Account`, fk `account_id`). Before
/// #150 every consumer hardcoded `user_id`, so a non-`User` identity silently lost
/// owner-scoping AND kept its fk client-writable (spoofable ownership behind a
/// green check). This design's `Note belongs_to Account` must instead get the full
/// per-user treatment keyed on `account_id`. Proves the non-`User` path end to end.
const ACCOUNTVAULT: &str = r#"{
    "name": "accountvault",
    "contract_version": 1,
    "auth": { "model": "session", "roles": ["admin"], "identity": "Account" },
    "dependencies": ["db", "auth"],
    "modules": [
        { "name": "accounts",
          "entities": [{ "name": "Account", "fields": [
              { "name": "email", "type": "string" } ]}],
          "endpoints": [
              { "operation_id": "list_accounts", "method": "GET", "path": "/",
                "auth_required": true,
                "success": { "status": 200, "entity": "Account", "list": true } }
          ] },
        { "name": "notes",
          "entities": [{ "name": "Note",
              "belongs_to": [{ "entity": "Account", "on_delete": "cascade" }],
              "fields": [{ "name": "title", "type": "string" }] }],
          "endpoints": [
              { "operation_id": "create_note", "method": "POST", "path": "/",
                "auth_required": true,
                "request_body": { "entity": "Note" },
                "success": { "status": 201, "entity": "Note" } },
              { "operation_id": "list_notes", "method": "GET", "path": "/",
                "auth_required": true,
                "success": { "status": 200, "entity": "Note", "list": true } }
          ] }
    ]
}"#;

/// The agent's side of the non-`User` identity e2e: `create_note` INJECTS the
/// session principal into the DERIVED `account_id` (the body has no `account_id`),
/// and reads go through the owner-scoped `*_for(account_id)` accessors — exactly
/// what the generated stub comment (`has NO account_id`) instructs.
const ACCOUNTVAULT_HANDLERS: &str = r#"//! E2E fixture: implemented handlers for the notes module (auth.identity=Account).
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use shared::CurrentUser;

pub(crate) async fn create_note(
    repo: Dep<NoteRepo>,
    user: CurrentUser,
    Json(body): Json<NoteRequest>,
) -> Result<Created<Note>> {
    // server-owned fk: the session, not the client, decides account_id.
    let account_id: i64 = user
        .0
        .id
        .parse()
        .map_err(|_| Error::internal("session id is not an integer"))?;
    let id = repo
        .insert(Note { id: body.id, account_id, title: body.title })
        .await?;
    // Owner-scoped read: the per-user repo emits only *_for(account_id) accessors
    // (the unscoped get/all are not generated — #79/#150 make-impossible).
    let row = repo.get_for(account_id, id).await?.ok_or_else(Error::not_found)?;
    Ok(Created(row))
}

pub(crate) async fn list_notes(
    repo: Dep<NoteRepo>,
    user: CurrentUser,
) -> Result<Json<Vec<Note>>> {
    let account_id: i64 = user
        .0
        .id
        .parse()
        .map_err(|_| Error::internal("session id is not an integer"))?;
    Ok(Json(repo.all_for(account_id).await?))
}
"#;

/// THE #150 non-`User` identity gate: scaffold the ACCOUNTVAULT shape, generate its
/// acceptance battery, and require the whole workspace to pass strict clippy. This
/// PROVES the opt-in `auth.identity: "Account"` path end to end: (a) it COMPILES
/// under `-D warnings`, (b) the guarded `NoteRequest` DTO OMITS `account_id` (the
/// #34 server-injected fk) — asserted on the emitted model + probe bodies, then
/// compiled, and (c) reads are OWNER-SCOPED via the `account_id` accessor (the repo
/// emits `all_for`/`get_for` keyed on `account_id`; the implemented handlers call
/// them and compile). The acceptance battery is compiled (all-targets) but not run
/// — a non-`User` identity table is not auto-seeded by the session harness (out of
/// scope for #150, whose contract is owner-scoping DETECTION, not test seeding).
#[test]
#[ignore = "scaffolds an app and invokes cargo on it; run with --include-ignored"]
fn opt_in_account_identity_scaffold_owner_scopes_and_omits_account_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("accountvault");

    let design: Design = serde_json::from_str(ACCOUNTVAULT).expect("ACCOUNTVAULT parses");
    assert!(
        design.wants_auth() && design.wants_db(),
        "auth + db mode (opt-in Account identity)"
    );

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, ACCOUNTVAULT);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold accountvault");

    // Generate the acceptance battery for the owned module (probe bodies omit the fk).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["gen-tests", "--module", "notes"])
        .output()
        .expect("run jerrycan gen-tests");
    assert!(
        out.status.success(),
        "gen-tests failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let read =
        |rel: &str| fs::read_to_string(app.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));

    // (b) The guarded DTO omits account_id; the Model keeps it.
    let model = read("crates/routes/notes/src/model.rs");
    let dto = model
        .split("pub struct NoteRequest {")
        .nth(1)
        .expect("NoteRequest emitted")
        .split('}')
        .next()
        .unwrap();
    assert!(
        !dto.contains("account_id"),
        "guarded DTO must omit the server-injected account_id:\n{dto}"
    );
    assert!(
        model.contains("pub account_id: i64,"),
        "the Model keeps account_id (server writes it):\n{model}"
    );

    // (c) The repo owner-scopes on account_id (the unscoped all()/get() are gone).
    let repo = read("crates/routes/notes/src/repo.rs");
    assert!(
        repo.contains("pub async fn all_for(&self, account_id: i64) -> Result<Vec<Note>>")
            && repo.contains("Column::AccountId.eq(account_id)"),
        "repo must owner-scope on account_id:\n{repo}"
    );

    // The stub steer names the ACTUAL omitted column, and the probe bodies omit it.
    let handlers = read("crates/routes/notes/src/handlers.rs");
    assert!(
        handlers.contains("has NO `account_id`") && !handlers.contains("has NO `user_id`"),
        "stub steer must name account_id:\n{handlers}"
    );
    let acceptance = read("crates/routes/notes/tests/acceptance.rs");
    assert!(
        !acceptance.contains("\"account_id\""),
        "generated probe bodies must omit account_id:\n{acceptance}"
    );

    // Implement the handlers the way the stub comment instructs.
    write(
        &app.join("crates/routes/notes/src/handlers.rs"),
        ACCOUNTVAULT_HANDLERS,
    );

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    // (a) The gate: the whole workspace (DTO, owner-scoped repo, implemented
    // handlers, generated acceptance bodies) compiles under strict clippy.
    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "accountvault workspace failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// The 0.6.5 T2 review battery (#80): every constrained-field shape that broke
/// a freshly scaffolded app, in one module — a required range int (`quantity`),
/// an OPTIONAL SINGLE-BOUND string (`note`, max_len only → the
/// clippy::collapsible_if case), an OPTIONAL TWO-BOUND string (`label`,
/// min_len + max_len → the consecutive-let-chain shape, T2-fix Minor-2), an
/// optional min-only int (`rating`), an "unbounded" `max: i64::MAX` int
/// (`views` → the unused_comparisons case), an optional enum (`priority` →
/// the #47 E0308 twin), and two `> i32::MAX` bounds (`starts_at`/`seq` → the
/// 0.6.5 final-review overflowing_literals case: large literals must stay
/// compilable in the validators here and `i64`-suffixed in the gen-tests
/// probes, covered end-to-end by conformance's limits-api). Shared by the
/// memory and db gates below via `constrained_design`.
const CONSTRAINED_MODULES: &str = r#""modules": [{
        "name": "items",
        "entities": [{ "name": "Item", "fields": [
            { "name": "quantity", "type": "integer", "min": 1, "max": 600 },
            { "name": "note", "type": "string", "required": false, "max_len": 20 },
            { "name": "label", "type": "string", "required": false, "min_len": 2, "max_len": 20 },
            { "name": "rating", "type": "integer", "required": false, "min": 1 },
            { "name": "views", "type": "integer", "max": 9223372036854775807 },
            { "name": "priority", "type": "string", "required": false, "values": ["low", "high"] },
            { "name": "starts_at", "type": "integer", "required": false, "min": 0, "max": 4102444800 },
            { "name": "seq", "type": "integer", "required": false, "min": 3000000000 }
        ]}],
        "endpoints": [
            { "operation_id": "list_items", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Item", "list": true } },
            { "operation_id": "create_item", "method": "POST", "path": "/",
              "request_body": { "entity": "Item" },
              "success": { "status": 201, "entity": "Item" } }
        ]
    }]"#;

fn constrained_design(name: &str, deps: &str) -> String {
    format!(
        r#"{{ "name": "{name}", "contract_version": 0, "dependencies": [{deps}], {CONSTRAINED_MODULES} }}"#
    )
}

/// Scaffold `design` via the real binary and require the whole workspace to
/// pass `cargo clippy --all-targets -- -D warnings` — the exact gate a
/// scaffolded app runs on itself (`jerrycan check`).
fn scaffold_and_strict_clippy(tmp: &Path, name: &str, design: &str) -> std::path::PathBuf {
    let app = tmp.join(name);
    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.join(format!("{name}.design.json"));
    write(&design_path, design);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(status.success(), "jerrycan new must scaffold {name}");

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "{name} failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    app
}

/// THE 0.6.5 T2 compile gate (review CRITICAL 1 + 2, IMPORTANT 3): a scaffolded
/// app whose design declares range/length constraints must compile AND pass its
/// own strict-clippy gate in BOTH modes — the string-pinning unit tests alone
/// let three generated-app breakages ship: the memory-mode optional validator
/// paired an `Option<T>` fn with a bare field (E0308), the optional single-bound
/// body tripped clippy::collapsible_if, and `max: i64::MAX` tripped
/// unused_comparisons. The memory app additionally proves at runtime that an
/// optional constraint still ENFORCES: present-but-violating → serde error,
/// absent → ok.
#[test]
#[ignore = "scaffolds two constrained apps and invokes cargo on them; run with --include-ignored"]
fn constrained_field_apps_pass_strict_clippy_in_both_modes() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Memory mode: bare-typed optional fields (`#[serde(default)]`) — the E0308 case.
    let mem = scaffold_and_strict_clippy(
        tmp.path(),
        "constrained-mem",
        &constrained_design("constrained-mem", ""),
    );
    // db mode: Option-typed optional fields — the collapsible_if case.
    scaffold_and_strict_clippy(
        tmp.path(),
        "constrained-db",
        &constrained_design("constrained-db", "\"db\""),
    );

    // Runtime enforcement round-trip at the attach site (the memory struct):
    // a present-but-violating optional value errors; an absent one passes.
    let model_path = mem.join("crates/routes/items/src/model.rs");
    let mut model = fs::read_to_string(&model_path).expect("read memory model.rs");
    model.push_str(
        r##"#[cfg(test)]
mod constraint_roundtrip {
    use super::Item;

    #[test]
    fn optional_constraints_enforce_when_present_and_allow_absence() {
        let ok = serde_json::from_str::<Item>(r#"{"quantity": 5, "views": 1}"#);
        assert!(ok.is_ok(), "absent optionals must deserialize: {ok:?}");
        let long = "x".repeat(21);
        let bad = serde_json::from_str::<Item>(&format!(
            r#"{{"quantity": 5, "views": 1, "note": "{long}"}}"#
        ));
        assert!(bad.is_err(), "21-char note must violate max_len 20");
        let bad = serde_json::from_str::<Item>(r#"{"quantity": 5, "views": 1, "rating": 0}"#);
        assert!(bad.is_err(), "rating 0 must violate min 1");
        let bad =
            serde_json::from_str::<Item>(r#"{"quantity": 5, "views": 1, "priority": "urgent"}"#);
        assert!(bad.is_err(), "priority outside values must be rejected");
        let bad = serde_json::from_str::<Item>(r#"{"quantity": 601, "views": 1}"#);
        assert!(bad.is_err(), "quantity 601 must violate max 600");
        let bad = serde_json::from_str::<Item>(r#"{"quantity": 5, "views": 1, "seq": 2999999999}"#);
        assert!(bad.is_err(), "seq below min 3000000000 must be rejected");
        let ok = serde_json::from_str::<Item>(
            r#"{"quantity": 5, "views": 1, "note": "ok", "rating": 3, "priority": "low", "starts_at": 4102444800, "seq": 3000000000}"#,
        );
        assert!(ok.is_ok(), "in-range values (incl. > i32::MAX) must pass: {ok:?}");
    }
}
"##,
    );
    write(&model_path, &model);
    let output = Command::new(env!("CARGO"))
        .current_dir(&mem)
        .args(["test", "-p", "route-items"])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo test");
    if !output.status.success() {
        panic!(
            "memory constraint round-trip failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Issue #122 (Finding 3): a db-mode design whose custom action carries an INLINE
/// request body — a #80-constrained REQUIRED field (`amount` min 1 → a
/// `de_checkout_request_amount` validator wired via `#[serde(deserialize_with]`) AND
/// an OPTIONAL field (`note` → `#[serde(default)] Option<String>`) — must scaffold
/// and pass its own strict-clippy gate. The inline `CheckoutRequest` DTO lands in the
/// SAME module `model.rs` as the entity `Order` and its `OrderRequest` DTO, so this
/// also proves the two DTO kinds coexist (distinct names) under `-D warnings`, and
/// the `checkout` handler stub takes `Json<CheckoutRequest>` and returns the
/// entity-less `Result<Json<serde_json::Value>>`. The prior inline coverage was only
/// string-matching unit tests + a memory-mode/unconstrained fixture — no db-mode
/// inline body, no constrained inline field, and no optional inline field was ever
/// actually compiled.
#[test]
#[ignore = "scaffolds a db-mode inline-body app and invokes cargo on it; run with --include-ignored"]
fn db_mode_inline_request_body_app_passes_strict_clippy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold_and_strict_clippy(
        tmp.path(),
        "inline-db",
        r#"{
            "name": "inline-db", "contract_version": 0, "dependencies": ["db"],
            "modules": [{
                "name": "checkout",
                "entities": [{ "name": "Order", "fields": [
                    { "name": "total", "type": "integer" },
                    { "name": "status", "type": "string", "values": ["open", "paid"], "default": "open" }
                ]}],
                "endpoints": [
                    { "operation_id": "list_orders", "method": "GET", "path": "/",
                      "success": { "status": 200, "entity": "Order", "list": true } },
                    { "operation_id": "create_order", "method": "POST", "path": "/",
                      "request_body": { "entity": "Order" },
                      "success": { "status": 201, "entity": "Order" } },
                    { "operation_id": "checkout", "method": "POST", "path": "/checkout",
                      "request_body": { "fields": [
                        { "name": "amount", "type": "integer", "min": 1 },
                        { "name": "note", "type": "string", "required": false } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
    );
}

/// Issue #127 (the LATENT uncompilable case): a NON-tenant param-mount child —
/// `items` mounted at `/orgs/{org_id}`, NO `tenancy` block — has its parent fk
/// `org_id` dropped from the request DTO by #82. With no `Dep<Tenant>` to resolve
/// it, the fk was previously UN-injectable: `handler_params` scanned `ep.path`
/// (`/`) only, so no `Path` param existed, and a handler following the
/// `server_owned_fk_comment` steer (`inject the _org_id path value`) referenced a
/// param the framework never generated → it could not compile. The fix binds the
/// mount-inherited fk as `Path(_org_id)`. This scaffolds the shape, IMPLEMENTS the
/// create by injecting the path fk (the exact injection the steer names), and
/// requires the route crate to build under strict clippy — the acceptance proof
/// that the once-uncompilable case now compiles.
const NON_TENANT_PARAM_MOUNT: &str = r#"{
    "name": "shop-api", "contract_version": 1, "dependencies": ["db"],
    "modules": [
        { "name": "orgs",
          "entities": [{ "name": "Org", "fields": [
              { "name": "id", "type": "integer" },
              { "name": "name", "type": "string" } ]}],
          "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Org", "list": true } }] },
        { "name": "items", "mount": "/orgs/{org_id}",
          "entities": [{ "name": "Item",
              "belongs_to": [{ "entity": "Org" }],
              "fields": [{ "name": "id", "type": "integer" },
                         { "name": "label", "type": "string" }] }],
          "endpoints": [{ "operation_id": "create_item", "method": "POST", "path": "/",
              "request_body": { "entity": "Item" },
              "success": { "status": 201, "entity": "Item" } }] }
    ]
}"#;

#[test]
#[ignore = "scaffolds a non-tenant param-mount app and invokes cargo on it; run with --include-ignored"]
fn non_tenant_param_mount_child_injects_path_fk_and_compiles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = tmp.path().join("shop-api");

    let jerrycan_dir = jerrycan_crate_dir().replace('\\', "/");
    let dep = format!("jerrycan = {{ path = \"{jerrycan_dir}\", default-features = false }}");
    let design_path = tmp.path().join("design.json");
    write(&design_path, NON_TENANT_PARAM_MOUNT);
    let status = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design_path)
        .status()
        .expect("run jerrycan new");
    assert!(
        status.success(),
        "jerrycan new must scaffold the non-tenant param-mount app"
    );

    // The generated stub now binds the mount-inherited fk as a Path param — the
    // thing that did NOT exist before the fix.
    let handlers_path = app.join("crates/routes/items/src/handlers.rs");
    let handlers = fs::read_to_string(&handlers_path).expect("read items handlers");
    assert!(
        handlers.contains("Path(_org_id): Path<i64>"),
        "the non-tenant param-mount create must bind the mount fk `_org_id` as a Path:\n{handlers}"
    );
    assert!(
        handlers.contains("inject the `_org_id` path"),
        "the steer must name the now-generated Path param:\n{handlers}"
    );

    // Follow the steer: inject `_org_id` (the path fk the DTO dropped) when building
    // the Item. This is exactly what was previously impossible — no such param
    // existed. If it compiles, the latent case is fixed.
    let implemented = handlers.replace(
        "    Err(Error::internal(\n        \"create_item not implemented — replace this stub\",\n    ))",
        "    let id = _repo\n        .insert(Item { id: _body.id, org_id: _org_id, label: _body.label.clone() })\n        .await?;\n    Ok(Created(Item { id, org_id: _org_id, label: _body.label }))",
    );
    assert_ne!(
        implemented, handlers,
        "the create_item stub must be replaced with a real path-fk injection"
    );
    write(&handlers_path, &implemented);

    // Avoid inheriting the parent jerrycan workspace; this temp dir is its own root.
    write(&app.join("rust-toolchain.toml"), "");

    let output = Command::new(env!("CARGO"))
        .current_dir(&app)
        .args([
            "clippy",
            "-p",
            "route-items",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .output()
        .expect("run cargo clippy");
    if !output.status.success() {
        panic!(
            "non-tenant param-mount app failed `cargo clippy -- -D warnings`\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("path has parent")).expect("create_dir_all");
    fs::write(path, content).expect("write file");
}
