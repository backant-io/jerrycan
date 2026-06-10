//! Drives the real binary over stdio with raw JSON-RPC lines.

use std::io::{BufRead, Write};

mod common;
use common::McpClient;

#[test]
fn initialize_list_and_unknown_method() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());

    let tools = c.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "all 9 contract tools served");
    assert!(names.contains(&"jerrycan_design") && names.contains(&"jerrycan_check"));
    // Every tool forwards its outputSchema (the contract defines one for each).
    for t in tools["tools"].as_array().unwrap() {
        assert!(
            t["outputSchema"].is_object(),
            "tool {} must forward outputSchema: {t}",
            t["name"]
        );
    }

    // Unknown method → -32601, server keeps running.
    let msg =
        serde_json::json!({"jsonrpc": "2.0", "id": 99, "method": "bogus/method", "params": {}});
    writeln!(c.stdin, "{msg}").unwrap();
    let mut line = String::new();
    c.stdout.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32601);

    let pong = c.request("ping", serde_json::json!({}));
    assert!(pong.as_object().unwrap().is_empty());
    c.shutdown();
}

#[test]
fn docs_tools_work_through_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());

    // A successful tools/call mirrors its payload into structuredContent.
    let result = c.request(
        "tools/call",
        serde_json::json!({"name": "jerrycan_docs_search", "arguments": {"query": "override_dep"}}),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["results"][0]["page"], "testing");

    let (err, payload) = c.call_tool(
        "jerrycan_docs_search",
        serde_json::json!({"query": "override_dep"}),
    );
    assert!(!err);
    assert_eq!(payload["results"][0]["page"], "testing");
    let (err, payload) = c.call_tool("jerrycan_docs_get", serde_json::json!({"page": "errors"}));
    assert!(!err);
    assert!(payload["markdown"].as_str().unwrap().contains("JC0404"));
    c.shutdown();
}

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

#[test]
fn design_tool_questions_then_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());

    // No draft → the template + a pointed ask, never code.
    let (err, payload) = c.call_tool(
        "jerrycan_design",
        serde_json::json!({"requirements": "todo backend"}),
    );
    assert!(!err);
    assert_eq!(payload["status"], "questions");
    assert!(
        payload["questions"][0]["question"]
            .as_str()
            .unwrap()
            .contains("draft")
    );

    // Broken draft → pointed questions with JSON-pointer ids.
    let mut bad: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    bad["name"] = serde_json::json!("Todo API");
    let (err, payload) = c.call_tool(
        "jerrycan_design",
        serde_json::json!({"requirements": "todo backend", "draft": bad}),
    );
    assert!(!err);
    assert_eq!(payload["status"], "questions");
    assert_eq!(payload["questions"][0]["id"], "/name");

    // Complete draft → written to disk, design_path returned.
    let good: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let (err, payload) = c.call_tool(
        "jerrycan_design",
        serde_json::json!({"requirements": "todo backend", "draft": good}),
    );
    assert!(!err);
    assert_eq!(payload["status"], "complete");
    let design_path = payload["design_path"].as_str().unwrap();
    assert!(std::path::Path::new(design_path).exists());
    assert!(payload["next_step"].as_str().unwrap().contains("scaffold"));
    c.shutdown();
}

#[test]
fn scaffold_generate_and_list_through_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());

    let app_dir = tmp.path().join("todo-api");
    let (err, payload) = c.call_tool(
        "jerrycan_scaffold",
        serde_json::json!({
            "design_path": tmp.path().join("design.json").to_str().unwrap(),
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(!err, "{payload}");
    assert!(payload["created"].as_array().unwrap().len() > 10);

    // Incremental generate with a design_slice (the MCP-only path).
    let (err, payload) = c.call_tool(
        "jerrycan_generate",
        serde_json::json!({
            "kind": "route",
            "path": "tags",
            "directory": app_dir.to_str().unwrap(),
            "design_slice": { "name": "tags", "endpoints": [
                { "operation_id": "list_tags", "method": "GET", "path": "/", "success": { "status": 200 } }
            ]},
        }),
    );
    assert!(!err, "{payload}");
    assert!(
        payload["modified"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "crates/app/src/main.rs")
    );
    assert!(app_dir.join("crates/routes/tags/src/lib.rs").exists());

    let (err, payload) = c.call_tool(
        "jerrycan_list_routes",
        serde_json::json!({"directory": app_dir.to_str().unwrap()}),
    );
    assert!(!err);
    assert!(
        payload["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/tags/")
    );

    c.shutdown();
}

#[test]
fn package_refuses_when_check_is_red() {
    // The package tool gates on a green full-workspace check. A freshly scaffolded
    // app still has unimplemented handler stubs, so check fails — the tool must
    // refuse with a check-failure error rather than emitting artifacts. This
    // exercises the CLI/MCP-shared run_package wiring without a multi-minute build.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let app_dir = tmp.path().join("todo-api");
    let (err, _) = c.call_tool(
        "jerrycan_scaffold",
        serde_json::json!({
            "design_path": tmp.path().join("design.json").to_str().unwrap(),
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(!err);

    let (err, payload) = c.call_tool(
        "jerrycan_package",
        serde_json::json!({"target": "k8s", "directory": app_dir.to_str().unwrap()}),
    );
    assert!(err, "packaging a red-check app must error: {payload}");
    assert!(
        payload["error"].as_str().unwrap().contains("check"),
        "error names the failed check gate: {payload}"
    );
    c.shutdown();
}

#[test]
fn partial_slice_replacement_warns_about_dropped_routes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let app_dir = tmp.path().join("todo-api");
    let (err, _) = c.call_tool(
        "jerrycan_scaffold",
        serde_json::json!({
            "design_path": tmp.path().join("design.json").to_str().unwrap(),
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(!err);

    // Replace todos with a ONE-endpoint slice: routes drop 8 -> 3 (comments subroute included).
    let (err, payload) = c.call_tool(
        "jerrycan_generate",
        serde_json::json!({
            "kind": "route",
            "path": "todos",
            "directory": app_dir.to_str().unwrap(),
            "design_slice": { "name": "todos", "endpoints": [
                { "operation_id": "list_todos", "method": "GET", "path": "/", "success": { "status": 200 } }
            ]},
        }),
    );
    assert!(!err, "{payload}");
    let next = payload["next_step"].as_str().unwrap();
    assert!(
        next.contains("warning") && next.contains("route count"),
        "{next}"
    );
    c.shutdown();
}

#[test]
fn slice_name_path_mismatch_gets_a_pointed_hint() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let app_dir = tmp.path().join("todo-api");
    let (err, _) = c.call_tool(
        "jerrycan_scaffold",
        serde_json::json!({
            "design_path": tmp.path().join("design.json").to_str().unwrap(),
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(!err);

    let (err, payload) = c.call_tool(
        "jerrycan_generate",
        serde_json::json!({
            "kind": "route",
            "path": "widgets",
            "directory": app_dir.to_str().unwrap(),
            "design_slice": { "name": "gadgets", "endpoints": [
                { "operation_id": "list_gadgets", "method": "GET", "path": "/", "success": { "status": 200 } }
            ]},
        }),
    );
    assert!(err);
    let msg = payload["error"].as_str().unwrap();
    assert!(
        msg.contains("gadgets") && msg.contains("widgets"),
        "must name both sides: {msg}"
    );
    c.shutdown();
}

#[test]
fn gen_tests_writes_tool_owned_acceptance_tests() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let app_dir = tmp.path().join("todo-api");
    let (err, _) = c.call_tool(
        "jerrycan_scaffold",
        serde_json::json!({
            "design_path": tmp.path().join("design.json").to_str().unwrap(),
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(!err);

    let (err, payload) = c.call_tool(
        "jerrycan_gen_tests",
        serde_json::json!({
            "module": "todos",
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(!err, "{payload}");
    assert_eq!(
        payload["tests_created"][0],
        "crates/routes/todos/tests/acceptance.rs"
    );
    assert_eq!(payload["expected_failing"], 8, "6 success + 2 listed 404s");
    assert!(payload["next_step"].as_str().unwrap().contains("implement"));
    let file =
        std::fs::read_to_string(app_dir.join("crates/routes/todos/tests/acceptance.rs")).unwrap();
    assert!(file.contains("GENERATED by jerrycan gen-tests"));

    // Unknown module → structured error.
    let (err, payload) = c.call_tool(
        "jerrycan_gen_tests",
        serde_json::json!({
            "module": "ghosts",
            "directory": app_dir.to_str().unwrap(),
        }),
    );
    assert!(err);
    assert!(payload["error"].as_str().unwrap().contains("ghosts"));
    c.shutdown();
}
