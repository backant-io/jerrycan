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

    let by_name = |n: &str| {
        tools
            .iter()
            .find(|t| t["name"] == n)
            .unwrap_or_else(|| panic!("missing tool {n}"))
    };

    // generate's kind enum is frozen: middleware deferred to contract v1.
    let kind_enum: Vec<_> =
        by_name("jerrycan_generate")["inputSchema"]["properties"]["kind"]["enum"]
            .as_array()
            .expect("kind enum")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
    assert_eq!(
        kind_enum,
        ["route", "subroute", "dependency"],
        "jerrycan_generate kind enum is frozen (no middleware in v0)"
    );

    // check's diagnostics carry at minimum a machine code and a human message.
    let diag_required: Vec<_> =
        by_name("jerrycan_check")["outputSchema"]["properties"]["diagnostics"]["items"]["required"]
            .as_array()
            .expect("diagnostics required")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
    assert_eq!(
        diag_required,
        ["code", "message"],
        "jerrycan_check diagnostics require exactly code+message"
    );

    // design hands scaffold the written design.json path on completion.
    assert!(
        by_name("jerrycan_design")["outputSchema"]["properties"]["design_path"].is_object(),
        "jerrycan_design must expose a design_path output for the scaffold hand-off"
    );
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
    // Endpoints can pin a role subset; app-level deps are provided on App for all modules.
    assert!(
        doc["$defs"]["endpoint"]["properties"]["required_roles"].is_object(),
        "endpoint must allow required_roles"
    );
    assert!(
        doc["properties"]["dependencies"].is_object(),
        "design must allow app-scoped dependencies"
    );
}

/// A hand-authored design exercises the schema's own `required` lists so neither
/// the instance nor the schema can drift without this test noticing.
#[test]
fn canonical_design_instance_satisfies_schema_invariants() {
    let schema = load("design-schema.json");
    let design: Value = serde_json::from_str(
        r#"{
            "name": "demo-api",
            "contract_version": 0,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["db"],
            "modules": [
                {
                    "name": "todos",
                    "entities": [
                        { "name": "Todo", "fields": [ { "name": "title", "type": "string" } ] }
                    ],
                    "endpoints": [
                        { "operation_id": "list_todos", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Todo", "list": true } },
                        { "operation_id": "create_todo", "method": "POST", "path": "/",
                          "request_body": { "entity": "Todo" },
                          "success": { "status": 201, "entity": "Todo" } },
                        { "operation_id": "delete_todo", "method": "DELETE", "path": "/{id}",
                          "required_roles": ["admin"],
                          "success": { "status": 204 } }
                    ],
                    "subroutes": [
                        {
                            "name": "comments",
                            "endpoints": [
                                { "operation_id": "list_comments", "method": "GET", "path": "/",
                                  "success": { "status": 200 } }
                            ]
                        }
                    ]
                }
            ]
        }"#,
    )
    .expect("canonical design instance is valid JSON");

    // Read the requireds FROM the schema so both sides must move together.
    let required_keys = |node: &Value| -> Vec<String> {
        node["required"]
            .as_array()
            .expect("a required array")
            .iter()
            .map(|v| v.as_str().expect("required key is a string").to_string())
            .collect()
    };
    let assert_has_keys = |obj: &Value, keys: &[String], ctx: &str| {
        for k in keys {
            assert!(
                obj.get(k).is_some(),
                "{ctx}: missing schema-required key `{k}`"
            );
        }
    };

    // Top-level required keys.
    assert_has_keys(&design, &required_keys(&schema), "design root");

    // Walk every module (and recursively every subroute) against $defs.module.required.
    let module_required = required_keys(&schema["$defs"]["module"]);
    let endpoint_required = required_keys(&schema["$defs"]["endpoint"]);

    fn walk_modules(
        modules: &Value,
        module_required: &[String],
        endpoint_required: &[String],
        assert_has_keys: &impl Fn(&Value, &[String], &str),
    ) {
        for m in modules.as_array().expect("modules array") {
            let name = m["name"].as_str().unwrap_or("<unnamed>");
            assert_has_keys(m, module_required, &format!("module `{name}`"));
            for ep in m["endpoints"].as_array().expect("endpoints array") {
                let op = ep["operation_id"].as_str().unwrap_or("<unnamed>");
                assert_has_keys(ep, endpoint_required, &format!("endpoint `{op}`"));
            }
            if let Some(subs) = m.get("subroutes") {
                walk_modules(subs, module_required, endpoint_required, assert_has_keys);
            }
        }
    }
    walk_modules(
        &design["modules"],
        &module_required,
        &endpoint_required,
        &assert_has_keys,
    );

    // required_roles on the delete endpoint must be a subset of auth.roles.
    let roles: std::collections::HashSet<&str> = design["auth"]["roles"]
        .as_array()
        .expect("auth.roles")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let delete = design["modules"][0]["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["operation_id"] == "delete_todo")
        .expect("delete_todo endpoint");
    for role in delete["required_roles"].as_array().expect("required_roles") {
        let role = role.as_str().expect("role is a string");
        assert!(
            roles.contains(role),
            "delete_todo requires role `{role}` not declared in auth.roles"
        );
    }
}
