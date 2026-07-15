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

fn operation(design: &Design, m: &ModuleDesign, ep: &Endpoint) -> Value {
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
        // Server-owned FK (issue #34): a guarded identity-FK body advertises the
        // `{Entity}Request` schema (no `user_id`) so generated clients never
        // send a field the server injects from the session.
        let schema = if design.endpoint_omits_identity_fk(m, ep) {
            entity_ref(&format!("{}Request", rb.entity))
        } else {
            entity_ref(&rb.entity)
        };
        op["requestBody"] = json!({
            "required": true,
            "content": { "application/json": { "schema": schema } },
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

fn walk_paths(
    design: &Design,
    m: &ModuleDesign,
    prefix: &str,
    paths: &mut serde_json::Map<String, Value>,
) {
    let base = format!("{}{}", prefix, m.effective_mount());
    for ep in &m.endpoints {
        let full = format!("{}{}", base.trim_end_matches('/'), ep.path);
        let entry = paths.entry(full).or_insert_with(|| json!({}));
        entry[ep.method.builder_fn()] = operation(design, m, ep);
    }
    for sub in &m.subroutes {
        walk_paths(design, sub, &base, paths);
    }
}

fn walk_schemas(design: &Design, m: &ModuleDesign, schemas: &mut serde_json::Map<String, Value>) {
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
        // Server-owned FK (issue #34): an identity-FK entity used as a GUARDED
        // request body gets a `{Entity}Request` schema — the wire input shape:
        // non-identity fk columns (required unless SetNull) + the declared
        // fields, and NO `user_id` (the server injects the session user's id).
        let needs_request_schema = m.endpoints.iter().any(|ep| {
            design.endpoint_omits_identity_fk(m, ep)
                && ep
                    .request_body
                    .as_ref()
                    .is_some_and(|rb| rb.entity == e.name)
        });
        if needs_request_schema {
            schemas.insert(format!("{}Request", e.name), request_schema(design, e));
        }
    }
    for sub in &m.subroutes {
        walk_schemas(design, sub, schemas);
    }
}

/// The `{Entity}Request` schema (issue #34): the entity's request shape minus
/// the server-owned `user_id` fk. Unlike the entity component (declared fields
/// only), the request shape must spell out the OTHER fk columns too — they ARE
/// required client input, and a generated client needs their types.
fn request_schema(design: &Design, e: &Entity) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for b in e.belongs_to.iter().filter(|b| !Design::is_identity_fk(b)) {
        let col = Design::fk_column(&b.entity);
        let schema = match design.target_key_rust_type(&b.entity) {
            "String" => json!({ "type": "string" }),
            _ => json!({ "type": "integer", "format": "int64" }),
        };
        properties.insert(col.clone(), schema);
        if b.on_delete != OnDelete::SetNull {
            required.push(Value::String(col));
        }
    }
    for f in &e.fields {
        properties.insert(f.name.clone(), field_schema(f.field_type));
        if f.required {
            required.push(Value::String(f.name.clone()));
        }
    }
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The complete OpenAPI 3.1 document for a design.
pub fn document(design: &Design) -> Value {
    let mut paths = serde_json::Map::new();
    let mut schemas = serde_json::Map::new();
    for m in &design.modules {
        walk_paths(design, m, "", &mut paths);
        walk_schemas(design, m, &mut schemas);
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
        assert!(
            show["responses"]["200"]["content"]["application/json"]["schema"]["$ref"]
                .as_str()
                .unwrap()
                .ends_with("Todo")
        );
        assert_eq!(show["responses"]["404"]["description"], "unknown id");

        let list = &d["paths"]["/todos/"]["get"];
        assert_eq!(
            list["responses"]["200"]["content"]["application/json"]["schema"]["type"],
            "array"
        );

        let create = &d["paths"]["/todos/"]["post"];
        assert!(
            create["requestBody"]["content"]["application/json"]["schema"]["$ref"]
                .as_str()
                .unwrap()
                .ends_with("Todo")
        );
        assert!(create["responses"]["201"].is_object());

        // Subroute paths compose:
        assert!(d["paths"]["/todos/comments/"]["get"].is_object());
    }

    /// The server-owned-FK rule (issue #34) in the CONTRACT: a guarded endpoint
    /// whose body entity has an identity FK advertises a `{Entity}Request`
    /// schema WITHOUT `user_id` (the server injects the session user's id);
    /// non-identity FKs appear (required) in that schema; an unguarded endpoint
    /// on the same entity keeps the plain entity ref. WHY: OpenAPI is what an
    /// external client generates against — if it demanded `user_id`, every
    /// clean client would 422 exactly like the agent eval did.
    #[test]
    fn guarded_identity_fk_request_schema_omits_user_id() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::SERVER_FK).unwrap(),
        );
        // (a) guarded create → dedicated Request schema.
        let create = &d["paths"]["/collections/"]["post"];
        assert_eq!(
            create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CollectionRequest"
        );
        let req = &d["components"]["schemas"]["CollectionRequest"];
        assert!(req["properties"]["title"].is_object(), "{req}");
        assert!(
            req["properties"].get("user_id").is_none(),
            "request schema must omit the server-owned fk: {req}"
        );
        // (c) the non-identity fk is present AND required.
        let breq = &d["components"]["schemas"]["BookmarkRequest"];
        assert_eq!(breq["properties"]["collection_id"]["type"], "integer");
        assert!(
            breq["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "collection_id"),
            "{breq}"
        );
        assert!(breq["properties"].get("user_id").is_none(), "{breq}");
        // (b) the unguarded endpoint keeps the plain entity ref.
        let import = &d["paths"]["/collections/import"]["post"];
        assert_eq!(
            import["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Collection"
        );
        // The entity response schema itself is untouched.
        let entity = &d["components"]["schemas"]["Collection"];
        assert!(entity["properties"]["title"].is_object(), "{entity}");
    }

    #[test]
    fn entity_schemas_map_field_types() {
        let d = doc();
        let todo = &d["components"]["schemas"]["Todo"]["properties"];
        assert_eq!(todo["title"]["type"], "string");
        assert_eq!(todo["done"]["type"], "boolean");
        let required = d["components"]["schemas"]["Todo"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|v| v == "title"));
        assert!(
            !required.iter().any(|v| v == "done"),
            "optional fields are not required"
        );
    }
}
