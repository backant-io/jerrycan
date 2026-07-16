//! auth/observe-mode generation: guard params, extension wiring, JL0004.

use jerrycan::platform::design::Design;
use jerrycan::platform::scaffold;
use std::fs;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

/// Golden design with session auth + an admin-guarded delete + observe.
fn auth_design() -> Design {
    let mut v: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    v["dependencies"] = serde_json::json!(["auth", "observe"]);
    v["auth"] = serde_json::json!({ "model": "session", "roles": ["admin"] });
    // mark delete_todo admin-guarded
    let eps = v["modules"][0]["endpoints"].as_array_mut().unwrap();
    for ep in eps {
        if ep["operation_id"] == "delete_todo" {
            ep["required_roles"] = serde_json::json!(["admin"]);
        }
        if ep["operation_id"] == "create_todo" {
            ep["auth_required"] = serde_json::json!(true);
        }
    }
    serde_json::from_value(v).unwrap()
}

fn scaffold_auth() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    scaffold::scaffold(&root, &auth_design()).unwrap();
    (tmp, root)
}

#[test]
fn auth_required_endpoints_get_a_session_guard_param() {
    let (_t, root) = scaffold_auth();
    let handlers = fs::read_to_string(root.join("crates/routes/todos/src/handlers.rs")).unwrap();
    // create_todo is auth_required → carries a CurrentUser guard
    assert!(
        handlers.contains("_user: CurrentUser"),
        "auth_required handler guarded: {handlers}"
    );
    // delete_todo requires role admin → guard + role check stub note
    assert!(
        handlers.contains("// guard: requires role \"admin\""),
        "{handlers}"
    );
    // list_todos is public → no guard
    let list_fn = handlers.split("async fn list_todos").nth(1).unwrap();
    assert!(
        !list_fn.split("->").next().unwrap().contains("CurrentUser"),
        "public handler unguarded"
    );
}

#[test]
fn main_wires_auth_and_observe_extensions() {
    let (_t, root) = scaffold_auth();
    let main_rs = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert!(
        main_rs.contains("jerrycan::observe::init_logging();"),
        "{main_rs}"
    );
    assert!(
        main_rs.contains(".extend(jerrycan::auth::Auth::from_env()?)"),
        "{main_rs}"
    );
    assert!(
        main_rs.contains(".extend(jerrycan::observe::Observe::new())"),
        "{main_rs}"
    );
    let ws = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(ws.contains("features = [\"auth\", \"observe\"]"), "{ws}");
}

#[test]
fn current_user_type_alias_is_generated_in_shared() {
    let (_t, root) = scaffold_auth();
    let shared = fs::read_to_string(root.join("crates/shared/src/lib.rs")).unwrap();
    // The app's notion of the session user lives in shared so guards across modules agree.
    assert!(shared.contains("pub type CurrentUser"), "{shared}");
    // The SESSION model resolves to the cookie `Session` guard (unchanged).
    assert!(
        shared.contains("pub type CurrentUser = jerrycan::auth::Session<SessionUser>;"),
        "session model uses the Session (cookie) guard: {shared}"
    );
}

/// A `jwt` auth model must emit a `Bearer<SessionUser>` alias so guarded REST
/// routes get REAL `Authorization: Bearer` guards, not silent session cookies
/// (issue #29). The `SessionUser` payload (String `id`) is identical to the
/// session model so tenant `user_id`/storage `owner_id` still line up.
#[test]
fn jwt_model_emits_bearer_guard_alias() {
    let mut v: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    v["dependencies"] = serde_json::json!(["auth"]);
    v["auth"] = serde_json::json!({ "model": "jwt", "roles": ["admin"] });
    for ep in v["modules"][0]["endpoints"].as_array_mut().unwrap() {
        if ep["operation_id"] == "create_todo" {
            ep["auth_required"] = serde_json::json!(true);
        }
    }
    let design: Design = serde_json::from_value(v).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("jwt-api");
    scaffold::scaffold(&root, &design).unwrap();
    let shared = fs::read_to_string(root.join("crates/shared/src/lib.rs")).unwrap();
    assert!(
        shared.contains("pub type CurrentUser = jerrycan::auth::Bearer<SessionUser>;"),
        "jwt model must alias CurrentUser to the Bearer guard: {shared}"
    );
    assert!(
        !shared.contains("jerrycan::auth::Session<SessionUser>"),
        "jwt model must NOT emit the Session (cookie) guard: {shared}"
    );
    // The payload struct is shared verbatim across models (String id).
    assert!(
        shared.contains("pub struct SessionUser") && shared.contains("pub id: String,"),
        "SessionUser payload is unchanged: {shared}"
    );
}
