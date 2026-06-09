//! Syntax + invariant gate for the platform contracts (full JSON-Schema
//! validation arrives with the MCP implementation in Phase 1).

use serde_json::Value;

fn load(rel: &str) -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/contracts/");
    let raw = std::fs::read_to_string(format!("{path}{rel}"))
        .unwrap_or_else(|e| panic!("missing contract file {rel}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{rel} is not valid JSON: {e}"))
}

#[test]
fn mcp_tools_contract_holds_its_invariants() {
    let doc = load("mcp-tools.json");
    let tools = doc["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 9, "spec §7.2 defines exactly 9 tools");

    let mut names = std::collections::HashSet::new();
    let workflow = [
        "jerrycan_design",
        "jerrycan_scaffold",
        "jerrycan_generate",
        "jerrycan_gen_tests",
        "jerrycan_check",
        "jerrycan_package",
    ];
    for t in tools {
        let name = t["name"].as_str().expect("tool name");
        assert!(
            name.starts_with("jerrycan_"),
            "{name}: tools are jerrycan_-prefixed"
        );
        assert!(names.insert(name.to_string()), "{name}: duplicate tool");
        assert!(
            t["description"].as_str().is_some_and(|d| d.len() > 20),
            "{name}: real description"
        );
        assert_eq!(t["inputSchema"]["type"], "object", "{name}: object input");
        if workflow.contains(&name) {
            let required: Vec<_> = t["outputSchema"]["required"]
                .as_array()
                .expect("required")
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            assert!(
                required.contains(&"next_step"),
                "{name}: workflow tools must return next_step"
            );
        }
    }
}

#[test]
fn design_schema_is_module_grouped_and_recursive() {
    let doc = load("design-schema.json");
    assert_eq!(doc["$id"], "https://jerrycan.cc/schemas/design-v0.json");
    assert_eq!(
        doc["properties"]["modules"]["items"]["$ref"],
        "#/$defs/module"
    );
    // Subroutes recurse into the same module definition (spec §5.1 "fractal").
    assert_eq!(
        doc["$defs"]["module"]["properties"]["subroutes"]["items"]["$ref"],
        "#/$defs/module"
    );
    // operation_id is the handler-name contract used by the §5.3 naming lint.
    assert!(doc["$defs"]["endpoint"]["properties"]["operation_id"]["pattern"].is_string());
}
