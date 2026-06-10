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
use std::process::Command;

/// Mirror of design::tests::MINIMAL (that const is `#[cfg(test)]`-private to the
/// crate, so integration tests inline it). The todos module has an entity (Todo)
/// → it emits model.rs + repo.rs, the exact dead-code-vs-stub shape under test.
const MINIMAL: &str = r#"{
    "name": "demo-api",
    "contract_version": 0,
    "auth": { "model": "session", "roles": ["admin"] },
    "dependencies": ["db"],
    "modules": [{
        "name": "todos",
        "entities": [{ "name": "Todo", "fields": [
            { "name": "title", "type": "string" },
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
    let module = design.modules.into_iter().next().expect("todos module");
    let created = write_module(&routes, &module, mode).expect("write_module");
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
        // Keep the generated workspace's target dir inside the tempdir so we don't
        // collide with / pollute the parent workspace's target.
        .env("CARGO_TARGET_DIR", app.join("target"))
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

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("path has parent")).expect("create_dir_all");
    fs::write(path, content).expect("write file");
}
