//! design.json → OpenAPI 3.1. Tool-owned output served by jerrycan-validate's
//! OpenApi extension. Deterministic: same design, same bytes.

use super::design::*;
use serde_json::{Value, json};

fn field_schema(t: FieldType) -> Value {
    match t {
        FieldType::String => json!({ "type": "string" }),
        FieldType::Integer => json!({ "type": "integer", "format": "int64" }),
        FieldType::Float => json!({ "type": "number", "format": "double" }),
        FieldType::Boolean => json!({ "type": "boolean" }),
        FieldType::Datetime => json!({ "type": "string", "format": "date-time" }),
        FieldType::Uuid => json!({ "type": "string", "format": "uuid" }),
        FieldType::Json => json!({}),
    }
}

fn entity_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn success_schema(s: &Success) -> Option<Value> {
    let inner = s.entity.as_deref().map(entity_ref)?;
    Some(if s.list {
        json!({ "type": "array", "items": inner })
    } else {
        inner
    })
}

fn operation(ep: &Endpoint) -> Value {
    let mut op = json!({ "operationId": ep.operation_id, "responses": {} });

    let params: Vec<Value> = {
        // every {x} in route order, integer in v0 (the generator emits i64)
        let mut out = Vec::new();
        let mut rest = ep.path.as_str();
        while let Some(start) = rest.find('{') {
            let Some(end_rel) = rest[start..].find('}') else {
                break;
            };
            out.push(json!({
                "name": rest[start + 1..start + end_rel],
                "in": "path",
                "required": true,
                "schema": { "type": "integer", "format": "int64" },
            }));
            rest = &rest[start + end_rel + 1..];
        }
        out
    };
    if !params.is_empty() {
        op["parameters"] = Value::Array(params);
    }
    if let Some(ref rb) = ep.request_body {
        op["requestBody"] = json!({
            "required": true,
            "content": { "application/json": { "schema": entity_ref(&rb.entity) } },
        });
    }
    let mut response = json!({ "description": "success" });
    if let Some(schema) = success_schema(&ep.success) {
        response["content"] = json!({ "application/json": { "schema": schema } });
    }
    op["responses"][ep.success.status.to_string()] = response;
    for ec in &ep.errors {
        op["responses"][ec.status.to_string()] = json!({ "description": ec.when });
    }
    op
}

fn walk_paths(m: &ModuleDesign, prefix: &str, paths: &mut serde_json::Map<String, Value>) {
    let base = format!("{}{}", prefix, m.effective_mount());
    for ep in &m.endpoints {
        let full = format!("{}{}", base.trim_end_matches('/'), ep.path);
        let entry = paths.entry(full).or_insert_with(|| json!({}));
        entry[ep.method.builder_fn()] = operation(ep);
    }
    for sub in &m.subroutes {
        walk_paths(sub, &base, paths);
    }
}

fn walk_schemas(m: &ModuleDesign, schemas: &mut serde_json::Map<String, Value>) {
    for e in &m.entities {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for f in &e.fields {
            properties.insert(f.name.clone(), field_schema(f.field_type));
            if f.required {
                required.push(Value::String(f.name.clone()));
            }
        }
        schemas.insert(
            e.name.clone(),
            json!({ "type": "object", "properties": properties, "required": required }),
        );
    }
    for sub in &m.subroutes {
        walk_schemas(sub, schemas);
    }
}

/// The complete OpenAPI 3.1 document for a design.
pub fn document(design: &Design) -> Value {
    let mut paths = serde_json::Map::new();
    let mut schemas = serde_json::Map::new();
    for m in &design.modules {
        walk_paths(m, "", &mut paths);
        walk_schemas(m, &mut schemas);
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": design.name,
            "version": "0.1.0",
            "description": design.description.clone().unwrap_or_default(),
        },
        "paths": paths,
        "components": { "schemas": schemas },
    })
}

/// Canonical on-disk form (pretty + trailing newline), like design.json.
pub fn document_json(design: &Design) -> String {
    let mut s = serde_json::to_string_pretty(&document(design)).expect("openapi serializes");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../../../../conformance/designs/todo-api.design.json");

    fn doc() -> Value {
        document(&serde_json::from_str::<Design>(GOLDEN).unwrap())
    }

    #[test]
    fn document_shape_is_openapi_31() {
        let d = doc();
        assert_eq!(d["openapi"], "3.1.0");
        assert_eq!(d["info"]["title"], "todo-api");
        assert!(d["paths"].is_object());
        assert!(d["components"]["schemas"]["Todo"].is_object());
    }

    #[test]
    fn paths_carry_operations_params_and_responses() {
        let d = doc();
        let show = &d["paths"]["/todos/{id}"]["get"];
        assert_eq!(show["operationId"], "show_todo");
        assert_eq!(show["parameters"][0]["name"], "id");
        assert_eq!(show["parameters"][0]["in"], "path");
        assert_eq!(show["parameters"][0]["schema"]["type"], "integer");
        assert!(show["responses"]["200"]["content"]["application/json"]["schema"]["$ref"]
            .as_str().unwrap().ends_with("Todo"));
        assert_eq!(show["responses"]["404"]["description"], "unknown id");

        let list = &d["paths"]["/todos/"]["get"];
        assert_eq!(list["responses"]["200"]["content"]["application/json"]["schema"]["type"], "array");

        let create = &d["paths"]["/todos/"]["post"];
        assert!(create["requestBody"]["content"]["application/json"]["schema"]["$ref"]
            .as_str().unwrap().ends_with("Todo"));
        assert!(create["responses"]["201"].is_object());

        // Subroute paths compose:
        assert!(d["paths"]["/todos/comments/"]["get"].is_object());
    }

    #[test]
    fn entity_schemas_map_field_types() {
        let d = doc();
        let todo = &d["components"]["schemas"]["Todo"]["properties"];
        assert_eq!(todo["title"]["type"], "string");
        assert_eq!(todo["done"]["type"], "boolean");
        let required = d["components"]["schemas"]["Todo"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "title"));
        assert!(!required.iter().any(|v| v == "done"), "optional fields are not required");
    }
}
