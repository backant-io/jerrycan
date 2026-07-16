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

/// The OpenAPI security scheme NAME advertised for a design's auth model
/// (issue #29): `bearerAuth` under `jwt`, `cookieAuth` under `session`, and none
/// under `none` (so a no-auth design's document is byte-identical to before).
fn security_scheme_name(design: &Design) -> Option<&'static str> {
    match design.auth_model() {
        AuthModel::Jwt => Some("bearerAuth"),
        AuthModel::Session => Some("cookieAuth"),
        AuthModel::None => None,
    }
}

fn operation(design: &Design, m: &ModuleDesign, ep: &Endpoint) -> Value {
    let mut op = json!({ "operationId": ep.operation_id, "responses": {} });

    // A guarded op requires the design's credential (issue #29): stamp `security`
    // referencing the model's scheme so a generated client sends it. Public /
    // signature-authenticated ops (and any op in a `none` design) carry none.
    if ep.is_guarded()
        && let Some(scheme) = security_scheme_name(design)
    {
        op["security"] = json!([{ scheme: [] }]);
    }

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
        // Server-owned fields (issues #34 + #53): a body that drops the identity
        // fk, a `default` field, or a path-redundant parent fk advertises the
        // `{Entity}Request` schema so generated clients never send a field the
        // server supplies (from the session, a declared default, or the path).
        // db-gated (issue #43) to stay in lockstep with genroute: the request DTO
        // only exists in db mode (memory-mode structs carry no fk columns), so a
        // memory-mode contract advertises the plain entity — never over-specifying
        // fk columns the memory struct doesn't have.
        let schema =
            if design.wants_db() && design.endpoint_uses_request_dto(m, ep, design.wants_auth()) {
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
        // Server-owned fields (issues #34 + #53): an entity used as a request body
        // whose wire shape drops a field (identity fk, a `default` field, or a
        // path-redundant parent fk) gets a `{Entity}Request` schema — the wire
        // input shape with those fields removed.
        let needs_request_schema = design.wants_db()
            && m.endpoints.iter().any(|ep| {
                design.endpoint_uses_request_dto(m, ep, design.wants_auth())
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

/// The `{Entity}Request` schema (issues #34 + #53): the entity's request shape
/// minus every server-owned field — the identity `user_id` fk (#34), a path-
/// redundant parent fk (#53b), and any `default` field (#53a). Unlike the entity
/// component (declared fields only), the request shape must spell out the
/// REMAINING fk columns too — they ARE required client input, and a generated
/// client needs their types.
fn request_schema(design: &Design, e: &Entity) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let omit_identity = design.wants_auth();
    let path_fks = design.entity_path_fk_columns(&e.name);
    for b in e.belongs_to.iter().filter(|b| {
        !(omit_identity && Design::is_identity_fk(b))
            && !path_fks.contains(&Design::fk_column(&b.entity))
    }) {
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
    // A `default` field (#53a) is server-owned — not part of the wire input shape.
    for f in e.fields.iter().filter(|f| f.default.is_none()) {
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
    let mut components = json!({ "schemas": schemas });
    // securitySchemes only when the design has a real guard credential (issue #29);
    // omitted entirely for `none` so that document stays byte-identical to before.
    if let Some(name) = security_scheme_name(design) {
        let scheme = match design.auth_model() {
            AuthModel::Jwt => json!({ "type": "http", "scheme": "bearer", "bearerFormat": "JWT" }),
            // The generated `Session` guard reads the `jerrycan_session` cookie.
            _ => json!({ "type": "apiKey", "in": "cookie", "name": "jerrycan_session" }),
        };
        components["securitySchemes"] = json!({ name: scheme });
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": design.name,
            "version": "0.1.0",
            "description": design.description.clone().unwrap_or_default(),
        },
        "paths": paths,
        "components": components,
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
    const REFERENCE_SLICE: &str =
        include_str!("../../../../conformance/designs/reference-slice.design.json");

    fn doc() -> Value {
        document(&serde_json::from_str::<Design>(GOLDEN).unwrap())
    }

    /// A `jwt` design advertises `bearerAuth` (http/bearer/JWT) and stamps
    /// `security` on every guarded operation, so a client generated from the
    /// contract sends `Authorization: Bearer <jwt>` — the credential the
    /// generated Bearer guard actually verifies (issue #29). Unguarded (public)
    /// ops carry no security.
    #[test]
    fn jwt_design_advertises_bearer_security_on_guarded_ops() {
        let d = document(&serde_json::from_str::<Design>(REFERENCE_SLICE).unwrap());
        let scheme = &d["components"]["securitySchemes"]["bearerAuth"];
        assert_eq!(scheme["type"], "http");
        assert_eq!(scheme["scheme"], "bearer");
        assert_eq!(scheme["bearerFormat"], "JWT");
        assert!(
            d["components"]["securitySchemes"]["cookieAuth"].is_null(),
            "jwt advertises bearer, never cookie: {}",
            d["components"]["securitySchemes"]
        );
        // create_lead is auth_required → per-op bearer security.
        assert_eq!(
            d["paths"]["/leads/"]["post"]["security"],
            json!([{ "bearerAuth": [] }])
        );
        // register is public → no security stanza.
        assert!(
            d["paths"]["/users/register"]["post"]
                .get("security")
                .is_none(),
            "public op carries no security: {}",
            d["paths"]["/users/register"]["post"]
        );
    }

    /// A `session` design advertises `cookieAuth` (apiKey in the
    /// `jerrycan_session` cookie) and stamps `security` on guarded ops.
    #[test]
    fn session_design_advertises_cookie_security_on_guarded_ops() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::SERVER_FK).unwrap(),
        );
        let scheme = &d["components"]["securitySchemes"]["cookieAuth"];
        assert_eq!(scheme["type"], "apiKey");
        assert_eq!(scheme["in"], "cookie");
        assert_eq!(scheme["name"], "jerrycan_session");
        assert!(
            d["components"]["securitySchemes"]["bearerAuth"].is_null(),
            "session advertises cookie, never bearer"
        );
        // list_users is auth_required → per-op cookie security.
        assert_eq!(
            d["paths"]["/users/"]["get"]["security"],
            json!([{ "cookieAuth": [] }])
        );
    }

    /// A `none`-model design emits NO security schemes and NO per-op security —
    /// its OpenAPI stays byte-identical to before this feature (issue #29).
    #[test]
    fn no_auth_design_emits_no_security() {
        let d = doc();
        assert!(
            d["components"].get("securitySchemes").is_none(),
            "none model adds no securitySchemes: {}",
            d["components"]
        );
        assert!(
            d["paths"]["/todos/"]["post"].get("security").is_none(),
            "none model adds no per-op security"
        );
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

    /// Issue #53a: a `default` field is dropped from the `{Entity}Request` schema
    /// (and its `required` list) so a generated client never sends a server-owned
    /// value — even for a PUBLIC (no-auth) create. The entity RESPONSE schema keeps
    /// every field (the server returns them).
    #[test]
    fn defaulted_fields_omitted_from_request_schema() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::DEFAULTS).unwrap(),
        );
        let create = &d["paths"]["/subscribers/"]["post"];
        assert_eq!(
            create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SubscriberRequest"
        );
        let req = &d["components"]["schemas"]["SubscriberRequest"];
        assert!(req["properties"]["email"].is_object(), "{req}");
        assert!(
            req["properties"].get("confirmed").is_none()
                && req["properties"].get("status").is_none(),
            "request schema must omit server-owned defaults: {req}"
        );
        // The entity response schema is untouched — it carries the defaulted fields.
        let entity = &d["components"]["schemas"]["Subscriber"]["properties"];
        assert!(entity["confirmed"].is_object() && entity["status"].is_object());
    }

    /// Issue #53b: a path-redundant parent fk is dropped from the `{Entity}Request`
    /// schema — the client sends it in the URL, not the body.
    #[test]
    fn nested_parent_fk_omitted_from_request_schema() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::NESTED_FK).unwrap(),
        );
        let create = &d["paths"]["/habits/{habit_id}/checkins"]["post"];
        assert_eq!(
            create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CheckinRequest"
        );
        let req = &d["components"]["schemas"]["CheckinRequest"];
        assert!(req["properties"]["note"].is_object(), "{req}");
        assert!(
            req["properties"].get("habit_id").is_none(),
            "path-redundant fk must not be in the request schema: {req}"
        );
        // Habit (top-level create) advertises the plain entity — no Request schema.
        assert!(
            d["components"]["schemas"].get("HabitRequest").is_none(),
            "a top-level create needs no Request schema: {}",
            d["components"]["schemas"]
        );
    }

    /// Issue #43: the request-DTO omission is db-gated, so a MEMORY-mode design (no
    /// `db` dependency) with an identity-FK entity advertises the PLAIN entity as its
    /// request body — never a `{Entity}Request` component. WHY: in memory mode
    /// genroute keeps `Json<Entity>` (the memory struct carries no fk columns at all),
    /// so a `{Entity}Request` schema spelling out `folder_id` would over-specify a
    /// column the server never deserializes — the OpenAPI contract and the handler
    /// signature would disagree. All three surfaces (genroute/openapi/testgen) now
    /// gate the DTO on db mode identically.
    #[test]
    fn memory_mode_request_schema_is_the_plain_entity_not_a_dto() {
        const MEMORY_IDENTITY_FK: &str = r#"{
            "name": "memnotes", "contract_version": 1,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["auth"],
            "modules": [{
                "name": "notes",
                "entities": [
                    { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                    { "name": "Folder", "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "Note",
                      "belongs_to": [
                          { "entity": "User", "on_delete": "cascade" },
                          { "entity": "Folder", "on_delete": "cascade" }
                      ],
                      "fields": [{ "name": "body", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_note", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Note" },
                      "success": { "status": 201, "entity": "Note" } }
                ]
            }]
        }"#;
        let design: Design = serde_json::from_str(MEMORY_IDENTITY_FK).unwrap();
        assert!(!design.wants_db(), "fixture must be memory mode");
        let d = document(&design);
        // The request body $refs the PLAIN entity, not NoteRequest.
        assert_eq!(
            d["paths"]["/notes/"]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Note",
            "memory-mode request body is the plain entity: {}",
            d["paths"]["/notes/"]["post"]
        );
        // No {Entity}Request component is minted in memory mode.
        assert!(
            d["components"]["schemas"].get("NoteRequest").is_none(),
            "memory mode mints no request DTO component: {}",
            d["components"]["schemas"]
        );
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
