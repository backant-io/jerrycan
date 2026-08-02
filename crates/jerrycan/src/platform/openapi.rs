//! design.json → OpenAPI 3.1. Tool-owned output served by jerrycan-validate's
//! OpenApi extension. Deterministic: same design, same bytes.

use super::design::*;
use serde_json::{Value, json};

fn field_schema(f: &Field) -> Value {
    let mut schema = match f.field_type {
        FieldType::String => json!({ "type": "string" }),
        FieldType::Integer => json!({ "type": "integer", "format": "int64" }),
        FieldType::Float => json!({ "type": "number", "format": "double" }),
        FieldType::Boolean => json!({ "type": "boolean" }),
        FieldType::Datetime => json!({ "type": "string", "format": "date-time" }),
        FieldType::Uuid => json!({ "type": "string", "format": "uuid" }),
        FieldType::Json => json!({}),
    };
    // Range/length constraints (#80) ride into the document as JSON Schema
    // keywords (JC0552 pins min/max to integer fields, min_len/max_len to
    // string fields). Absent keys emit nothing, so every unconstrained
    // design's document stays byte-identical.
    if let Some(mn) = f.min {
        schema["minimum"] = json!(mn);
    }
    if let Some(mx) = f.max {
        schema["maximum"] = json!(mx);
    }
    if let Some(mn) = f.min_len {
        schema["minLength"] = json!(mn);
    }
    if let Some(mx) = f.max_len {
        schema["maxLength"] = json!(mx);
    }
    // Response-hidden fields (#112): the entity component is shared between the
    // no-DTO request body and the response `$ref`, and OpenAPI `writeOnly`
    // (present in requests, omitted from responses) is correct for both. Only
    // emitted for a write_only / `password_hash` field, so every other design's
    // document stays byte-identical.
    if Design::field_is_write_only(f) {
        schema["writeOnly"] = json!(true);
    }
    schema
}

fn entity_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

/// Convert a scalar type schema to its OpenAPI-3.1 nullable form (#274):
/// `{"type": "T", …}` becomes `{"type": ["T", "null"], …}`, preserving `format`,
/// constraints, and `writeOnly`. A schema with no `type` key (a JSON field's `{}`,
/// which already admits null) is returned unchanged. Applied only to the db-mode
/// columns generated as `Option<T>` (optional fields, `set_null` fks) — they
/// serialize `None` as an explicit `null` and accept `null` on input, so a bare
/// scalar `type` (which excludes null under JSON Schema 2020-12 / OpenAPI 3.1)
/// would make a contract-conformant client reject the value.
fn make_nullable(mut schema: Value) -> Value {
    if let Some(t) = schema.get("type").and_then(Value::as_str) {
        let t = t.to_string();
        schema["type"] = json!([t, "null"]);
    }
    schema
}

/// The OpenAPI schema for a `belongs_to` fk column: the target pk's scalar type,
/// made nullable (#274) when the fk is `set_null` — a nullable `Option<T>` column.
fn fk_schema(design: &Design, b: &BelongsTo) -> Value {
    let schema = match design.target_key_rust_type(&b.entity) {
        "String" => json!({ "type": "string" }),
        _ => json!({ "type": "integer", "format": "int64" }),
    };
    if b.on_delete == OnDelete::SetNull {
        make_nullable(schema)
    } else {
        schema
    }
}

fn success_schema(s: &Success) -> Option<Value> {
    // #269: a 204 (No Content) or a 3xx redirect has an EMPTY body — `return_type`
    // emits `NoContent`/`Redirect`, so advertise NO response body regardless of a
    // declared `entity`/`list` (a 204-with-body is invalid HTTP/OpenAPI, and a
    // generated client SDK would try to parse an absent body). Mirrors the
    // empty-body arms of `genroute::return_type`.
    if s.status == 204 || (300..400).contains(&s.status) {
        return None;
    }
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
    // signature-authenticated ops (and any op in a `none` design) carry none. A
    // public_read GET (#105) carries none EITHER — genroute emits its handler
    // unguarded regardless of the declared `auth_required` (the shared
    // `Design::endpoint_is_public_read_get`), so advertising a credential here
    // would be a lie: a generated client would refuse anonymous calls to a feed
    // that correctly serves them. A role-gated GET keeps its stanza (it keeps
    // its guard).
    if ep.is_guarded()
        && !design.endpoint_is_public_read_get(m, ep)
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
        let schema = if rb.is_inline() {
            // An inline DTO body (issue #122): advertise the ad-hoc
            // `{Pascal(operation_id)}Request` schema (registered in `walk_schemas`).
            entity_ref(&format!("{}Request", to_pascal(&ep.operation_id)))
        } else if design.wants_db() && design.endpoint_uses_request_dto(m, ep, design.wants_auth())
        {
            // A defaulted-entity UPDATE advertises `{Entity}UpdateRequest` (keeps
            // the `default` fields — settable on update); every other DTO
            // endpoint advertises `{Entity}Request` (issue #85 D1).
            let entity = rb.entity.as_deref().expect("entity body");
            let name = if ep.method.is_update() && design.entity_has_default(entity) {
                format!("{entity}UpdateRequest")
            } else {
                format!("{entity}Request")
            };
            entity_ref(&name)
        } else {
            entity_ref(rb.entity.as_deref().expect("entity body"))
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
    // #115: a create (POST with a body) on an entity carrying a composite `unique`
    // group may 409 on a duplicate `(col, …)` (the CREATE UNIQUE INDEX → JC0409) —
    // document it so a generated client expects the conflict. Only when the design
    // hasn't already declared a 409 for this op (keep the author's description).
    if ep.method == HttpMethod::POST
        && op["responses"].get("409").is_none()
        && let Some(rb) = &ep.request_body
        && let Some(entity) = rb.entity.as_deref()
        && let Some(group) = m
            .entities
            .iter()
            .find(|e| e.name == entity)
            .and_then(|e| e.unique.first())
    {
        let cols = group.join(", ");
        op["responses"]["409"] =
            json!({ "description": format!("a row with the same ({cols}) already exists") });
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
        // #273: a db-mode entity with no declared `id` field gets a synthetic i64
        // primary key (`model_rs_db` surfaces `pub id`), serialized in every response
        // and accepted in a plain-body create. The component omitted it (built from
        // `fields` alone) → a contract client couldn't read the row's `id` and thus
        // couldn't drive any sibling `/{id}` route (the RESPONSE-side counterpart to
        // #271). Spell out the synthetic pk (integer, required). A DECLARED `id` rides
        // the fields loop below; memory-mode Models have no synthetic id, so the
        // db-gate keeps memory documents identical.
        if design.wants_db() && !e.fields.iter().any(|f| f.name == "id") {
            properties.insert("id".into(), json!({ "type": "integer", "format": "int64" }));
            required.push(Value::String("id".into()));
        }
        for f in &e.fields {
            // #274: in db mode an optional field is `Option<T>` — it serializes `None`
            // as an explicit `null` and accepts `null` — so its schema must admit null.
            // Memory-mode optional fields are bare `T` with `#[serde(default)]` (a
            // `null` fails to deserialize), so the db-gate keeps them non-nullable and
            // every memory document identical. A declared `id` is EXCLUDED: `model_rs_db`
            // emits the pk as a non-`Option` `pub id` and ignores its `required` flag, so
            // even a (pathological) `id: required=false` stays a non-null pk — never
            // nullable in the contract.
            let schema = if design.wants_db() && !f.required && f.name != "id" {
                make_nullable(field_schema(f))
            } else {
                field_schema(f)
            };
            properties.insert(f.name.clone(), schema);
            if f.required {
                required.push(Value::String(f.name.clone()));
            }
        }
        // #271: in db mode the entity's `belongs_to` fk columns are real columns of the
        // row. The component is the RESPONSE shape (the serialized Model carries them)
        // AND — when no `{Entity}Request` DTO is minted — the plain CREATE body (the
        // client MUST send them). Building the component from `fields` alone advertised
        // a create body missing the fk column(s), so a client generated from the
        // contract 422'd (`missing field {fk}_id`) while the generated probe (built
        // from the entity MODEL, not the contract) posted the full column set and
        // greened. Spell them out via `fk_schema` (target-pk type, nullable when
        // `set_null` per #274), required unless nullable — mirroring `request_schema`.
        // Memory-mode structs carry no fk columns, so the db-gate keeps every memory
        // document identical.
        if design.wants_db() {
            for b in &e.belongs_to {
                let col = b.fk_column();
                properties.insert(col.clone(), fk_schema(design, b));
                if b.on_delete != OnDelete::SetNull {
                    required.push(Value::String(col));
                }
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
                        .is_some_and(|rb| rb.entity.as_deref() == Some(e.name.as_str()))
            });
        if needs_request_schema {
            schemas.insert(
                format!("{}Request", e.name),
                request_schema(design, e, false),
            );
        }
        // A defaulted entity with an UPDATE endpoint also advertises
        // `{Entity}UpdateRequest` — the update wire shape KEEPS the `default` fields
        // so a client can change them after create (issue #85 D1).
        let needs_update_schema = design.wants_db()
            && e.fields.iter().any(|f| f.default.is_some())
            && m.endpoints.iter().any(|ep| {
                ep.method.is_update()
                    && design.endpoint_uses_request_dto(m, ep, design.wants_auth())
                    && ep
                        .request_body
                        .as_ref()
                        .is_some_and(|rb| rb.entity.as_deref() == Some(e.name.as_str()))
            });
        if needs_update_schema {
            schemas.insert(
                format!("{}UpdateRequest", e.name),
                request_schema(design, e, true),
            );
        }
    }
    // Inline-DTO bodies (issue #122): a custom action whose body is not a table row
    // advertises `{Pascal(operation_id)}Request` — a plain object schema built from
    // its `fields` (types + required set + #80 constraints), so the custom action is
    // fully described in the contract even though no entity backs it.
    for ep in &m.endpoints {
        if let Some(rb) = ep.request_body.as_ref()
            && rb.is_inline()
        {
            schemas.insert(
                format!("{}Request", to_pascal(&ep.operation_id)),
                inline_request_schema(&rb.fields),
            );
        }
    }
    for sub in &m.subroutes {
        walk_schemas(design, sub, schemas);
    }
}

/// The inline-DTO request schema (issue #122): a plain object over the declared
/// `fields` — each field's type schema (with #80 min/max/minLength/maxLength via
/// `field_schema`) plus the required set. No fk columns, no server-owned omission
/// (unlike `request_schema` — an inline body is not an entity).
fn inline_request_schema(fields: &[Field]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for f in fields {
        properties.insert(f.name.clone(), field_schema(f));
        if f.required {
            required.push(Value::String(f.name.clone()));
        }
    }
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The `{Entity}Request` schema (issues #34 + #53): the entity's request shape
/// minus every server-owned field — the identity `user_id` fk (#34), a path-
/// redundant parent fk (#53b), and any `default` field (#53a). Unlike the entity
/// component (declared fields only), the request shape must spell out the
/// REMAINING fk columns too — they ARE required client input, and a generated
/// client needs their types.
fn request_schema(design: &Design, e: &Entity, for_update: bool) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let omit_identity = design.wants_auth();
    let path_fks = design.entity_path_fk_columns(&e.name);
    for b in e.belongs_to.iter().filter(|b| {
        !(omit_identity && design.is_identity_fk(b)) && !path_fks.contains(&b.fk_column())
    }) {
        let col = b.fk_column();
        // #274: a `set_null` fk is a nullable `Option<T>` in the request DTO too.
        properties.insert(col.clone(), fk_schema(design, b));
        if b.on_delete != OnDelete::SetNull {
            required.push(Value::String(col));
        }
    }
    // A STATIC `default` field (#53a) is server-owned on CREATE (dropped); on UPDATE
    // it is client-settable (kept), so `for_update` includes it (issue #85 D1). A
    // `now`-default timestamp (#110) is server-owned AND set-once, so it is dropped
    // from BOTH request schemas (immutable on update). The extra clause is inert for
    // every non-`now` field — designs without the sentinel stay byte-identical.
    for f in e
        .fields
        .iter()
        .filter(|f| (for_update || f.default.is_none()) && !Design::field_is_now_default(f))
    {
        // #274: the db request DTO types an optional field as `Option<T>` (it accepts
        // `null`), so its schema must admit null. `request_schema` is only built in db
        // mode (gated at the call sites in `walk_schemas`), so this needs no db-check.
        let schema = if f.required {
            field_schema(f)
        } else {
            make_nullable(field_schema(f))
        };
        properties.insert(f.name.clone(), schema);
        if f.required {
            required.push(Value::String(f.name.clone()));
        }
    }
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The generated member-management surface (issue #107): the four tool-owned
/// member routes on the tenant module. Unlike storage buckets these are
/// first-class tenant routes, so they ARE advertised — a generated client can
/// invite/remove/re-role members without reading the Rust. Gated exactly like
/// genroute's emission (tenancy + db + auth), so any design without the surface
/// keeps a byte-identical document.
fn member_surface_paths(design: &Design, paths: &mut serde_json::Map<String, Value>) {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return;
    };
    if !design.wants_db() || !design.wants_auth() {
        return;
    }
    // The mount-resolved base of the module DECLARING the tenant entity —
    // recursing so a tenant declared in a subroute is still found (mirrors
    // genroute's per-module emission site).
    fn base_of(m: &ModuleDesign, prefix: &str, entity: &str) -> Option<String> {
        let base = format!("{}{}", prefix, m.effective_mount());
        if m.entities.iter().any(|e| e.name == entity) {
            return Some(base);
        }
        m.subroutes.iter().find_map(|s| base_of(s, &base, entity))
    }
    let Some(base) = design
        .modules
        .iter()
        .find_map(|m| base_of(m, "", &tenancy.entity))
    else {
        return;
    };
    let entity = &tenancy.entity;
    let snake = Design::to_snake(entity);
    let fk = Design::fk_column(entity);
    // The admin role: member_roles[0], same fallback as genroute's emission.
    let admin = tenancy
        .member_roles
        .first()
        .map(String::as_str)
        .unwrap_or("member");
    // The tenant fk param types as the tenant KEY; `user_id` is an opaque string
    // (no FK backs it — migrated-uuid support), and `role` is pinned to the
    // declared member_roles so a generated client can't even send an off-set one.
    let fk_schema = match design.target_key_rust_type(entity) {
        "String" => json!({ "type": "string" }),
        _ => json!({ "type": "integer", "format": "int64" }),
    };
    let role_schema = if tenancy.member_roles.is_empty() {
        json!({ "type": "string" })
    } else {
        json!({ "type": "string", "enum": tenancy.member_roles })
    };
    let member_schema = json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "format": "int64" },
            "user_id": { "type": "string" },
            "role": role_schema.clone(),
        },
        "required": ["id", "user_id", "role"],
    });
    // The POST wire/echo shape (the handler echoes what it wrote; the row id is
    // in the roster).
    let add_schema = json!({
        "type": "object",
        "properties": { "user_id": { "type": "string" }, "role": role_schema.clone() },
        "required": ["user_id", "role"],
    });
    let fk_param = json!({ "name": fk, "in": "path", "required": true, "schema": fk_schema });
    let user_param = json!({ "name": "user_id", "in": "path", "required": true, "schema": { "type": "string" } });
    let security = security_scheme_name(design).map(|scheme| json!([{ scheme: [] }]));
    let not_member = format!("caller is not a member of this {snake} (membership guard)");
    let not_admin = format!("caller does not hold the admin role `{admin}`");
    let bad_role = "role is not one of the declared member_roles";

    let mut list = json!({
        "operationId": format!("list_{snake}_members"),
        "parameters": [fk_param.clone()],
        "responses": {
            "200": {
                "description": "success",
                "content": { "application/json": {
                    "schema": { "type": "array", "items": member_schema } } },
            },
            "404": { "description": not_member.clone() },
        },
    });
    let mut add = json!({
        "operationId": format!("add_{snake}_member"),
        "parameters": [fk_param.clone()],
        "requestBody": {
            "required": true,
            "content": { "application/json": { "schema": add_schema.clone() } },
        },
        "responses": {
            "201": {
                "description": "success",
                "content": { "application/json": { "schema": add_schema } },
            },
            "403": { "description": not_admin.clone() },
            "404": { "description": not_member.clone() },
            "409": { "description": "user is already a member" },
            "422": { "description": bad_role },
        },
    });
    let mut set_role = json!({
        "operationId": format!("set_{snake}_member_role"),
        "parameters": [fk_param.clone(), user_param.clone()],
        "requestBody": {
            "required": true,
            "content": { "application/json": { "schema": {
                "type": "object",
                "properties": { "role": role_schema },
                "required": ["role"],
            } } },
        },
        "responses": {
            "204": { "description": "success" },
            "403": { "description": not_admin },
            "404": { "description": format!("{not_member}, or no such member") },
            "409": { "description": format!("cannot demote the last {admin}") },
            "422": { "description": bad_role },
        },
    });
    let mut remove = json!({
        "operationId": format!("remove_{snake}_member"),
        "parameters": [fk_param, user_param],
        "responses": {
            "204": { "description": "success" },
            "403": {
                "description": format!(
                    "removing another member requires the admin role `{admin}` (self-removal is open to any member)"
                ),
            },
            "404": { "description": format!("{not_member}, or no such member") },
            "409": { "description": format!("cannot remove the last {admin}") },
        },
    });
    if let Some(sec) = security {
        for op in [&mut list, &mut add, &mut set_role, &mut remove] {
            op["security"] = sec.clone();
        }
    }
    let collection = format!("{}/{{{fk}}}/members", base.trim_end_matches('/'));
    let item = format!("{collection}/{{user_id}}");
    let entry = paths.entry(collection).or_insert_with(|| json!({}));
    entry["get"] = list;
    entry["post"] = add;
    let entry = paths.entry(item).or_insert_with(|| json!({}));
    entry["patch"] = set_role;
    entry["delete"] = remove;
}

/// The complete OpenAPI 3.1 document for a design.
pub fn document(design: &Design) -> Value {
    let mut paths = serde_json::Map::new();
    let mut schemas = serde_json::Map::new();
    for m in &design.modules {
        walk_paths(design, m, "", &mut paths);
        walk_schemas(design, m, &mut schemas);
    }
    // The generated member-management routes (issue #107) join the document
    // AFTER the design's own paths (tool-owned routes, tenancy designs only).
    member_surface_paths(design, &mut paths);
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

    /// #105 gate-lie fix: a `public_read` GET runs UNGUARDED — genroute strips
    /// its `CurrentUser` even when the design declares `auth_required` — so its
    /// operation must NOT advertise a credential. WHY (Rule 9): before the shared
    /// `Design::endpoint_is_public_read_get` predicate, this stanza keyed on the
    /// raw `is_guarded()` and stamped `cookieAuth` on the public feed — a client
    /// generated from the contract refused anonymous calls a correct handler
    /// serves (the doc lied about the running code). Writes on the same entity,
    /// and guarded GETs on a NON-public_read sibling, keep their stanza.
    #[test]
    fn public_read_get_advertises_no_security_but_writes_keep_it() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::PUBLIC_READ).unwrap(),
        );
        // The DECLARED-guarded list GET carries no security — the flag overrides.
        assert!(
            d["paths"]["/posts/"]["get"].get("security").is_none(),
            "a public_read GET must not advertise a credential: {}",
            d["paths"]["/posts/"]["get"]
        );
        assert!(
            d["paths"]["/posts/{id}"]["get"].get("security").is_none(),
            "the public detail GET carries none either: {}",
            d["paths"]["/posts/{id}"]["get"]
        );
        // Writes keep the credential the guard actually demands.
        for (path, method) in [
            ("/posts/", "post"),
            ("/posts/{id}", "put"),
            ("/posts/{id}", "delete"),
        ] {
            assert_eq!(
                d["paths"][path][method]["security"],
                json!([{ "cookieAuth": [] }]),
                "write {method} {path} keeps its security stanza"
            );
        }
        // The guarded GET on the non-public sibling entity keeps it too.
        assert_eq!(
            d["paths"]["/posts/drafts"]["get"]["security"],
            json!([{ "cookieAuth": [] }]),
            "a guarded non-public_read GET keeps its stanza"
        );
    }

    /// #271: the entity component (used as the RESPONSE row AND — when no
    /// `{Entity}Request` DTO is minted — as the plain CREATE body) must spell out the
    /// entity's `belongs_to` fk columns in db mode: they are real NOT-NULL columns of
    /// the row, and a create that resolves to the plain entity body needs them as
    /// client input. Before this, `walk_schemas` built the component from `fields`
    /// alone, so a client generated from the contract posting the advertised body got
    /// a 422 (`missing field {fk}_id`) — while the generated probe (built from the
    /// entity MODEL, not the contract) posted the full column set and greened, hiding
    /// it. The `{Entity}Request` DTO path already spelled the fks out; the two
    /// disagreed. fk-alias makes it acute (two non-identity fks). Memory mode stays
    /// fields-only (its structs carry no fk columns) — byte-identical.
    #[test]
    fn entity_component_spells_out_belongs_to_fk_columns() {
        const LEDGER: &str = r#"{
            "name": "ledger", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "ledger",
                "entities": [
                    { "name": "Account", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "name", "type": "string" } ]},
                    { "name": "Transfer",
                      "belongs_to": [
                          { "entity": "Account", "as": "from_account", "on_delete": "cascade" },
                          { "entity": "Account", "as": "to_account", "on_delete": "set_null" } ],
                      "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "amount", "type": "integer" } ]}
                ],
                "endpoints": [
                    { "operation_id": "create_transfer", "method": "POST", "path": "/transfers",
                      "request_body": { "entity": "Transfer" },
                      "success": { "status": 201, "entity": "Transfer" } } ] }]
        }"#;
        let design: Design = serde_json::from_str(LEDGER).unwrap();
        assert!(design.wants_db(), "fixture must be db mode");
        let d = document(&design);
        // No DTO is minted (no identity fk / default / path-redundant fk), so the
        // create body $refs the PLAIN entity — the component MUST carry the fks.
        assert_eq!(
            d["paths"]["/ledger/transfers"]["post"]["requestBody"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/Transfer",
            "the create body is the plain entity: {}",
            d["paths"]["/ledger/transfers"]["post"]
        );
        assert!(
            d["components"]["schemas"].get("TransferRequest").is_none(),
            "no DTO minted for a plain create: {}",
            d["components"]["schemas"]
        );
        let transfer = &d["components"]["schemas"]["Transfer"];
        // BOTH aliased fk columns are present and typed as the target pk (integer). The
        // cascade fk is a bare `integer`; the `set_null` fk is nullable per #274
        // (`Option<T>` column → serializes `null`) so it types as `["integer","null"]`.
        assert_eq!(transfer["properties"]["from_account_id"]["type"], "integer");
        assert_eq!(
            transfer["properties"]["to_account_id"]["type"],
            json!(["integer", "null"])
        );
        let required = transfer["required"].as_array().unwrap();
        // A cascade (NOT NULL) fk is required; a set_null (nullable) fk is present
        // but optional — mirrors the request-schema required rule.
        assert!(
            required.iter().any(|v| v == "from_account_id"),
            "cascade fk is required: {transfer}"
        );
        assert!(
            !required.iter().any(|v| v == "to_account_id"),
            "set_null fk is present but optional: {transfer}"
        );
        // The declared fields are still there.
        assert_eq!(transfer["properties"]["amount"]["type"], "integer");
    }

    /// #273: a db-mode entity that does NOT declare an `id` field gets a synthetic
    /// i64 primary key (`model_rs_db` surfaces `pub id`), which the handler serializes
    /// in every response. The entity component must spell it out (integer, required) —
    /// otherwise a contract client can't read the row's `id` and can't drive any
    /// sibling `/{id}` route. An entity that DECLARES `id` carries it via its fields
    /// (no duplication); a memory-mode Model has no synthetic id, so its component
    /// stays fields-only.
    #[test]
    fn db_synthetic_pk_id_is_spelled_out_in_the_component() {
        const DB: &str = r#"{
            "name": "synthid", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "widgets",
                "entities": [
                    { "name": "Widget", "fields": [{ "name": "label", "type": "string" }] },
                    { "name": "Gadget", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "label", "type": "string" } ]} ],
                "endpoints": [
                    { "operation_id": "get_widget", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Widget" } } ] }]
        }"#;
        let d = document(&serde_json::from_str::<Design>(DB).unwrap());
        // Synthetic-pk entity: the component gains `id` (integer, required).
        let widget = &d["components"]["schemas"]["Widget"];
        assert_eq!(widget["properties"]["id"]["type"], "integer");
        assert_eq!(widget["properties"]["id"]["format"], "int64");
        assert!(
            widget["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "id"),
            "synthetic id is required: {widget}"
        );
        // Declared-id entity: `id` present exactly once (from its field, not doubled).
        let gadget = &d["components"]["schemas"]["Gadget"];
        assert_eq!(gadget["properties"]["id"]["type"], "integer");
        assert_eq!(
            gadget["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| *v == "id")
                .count(),
            1,
            "declared id is not duplicated: {gadget}"
        );
        // Memory mode: no synthetic id → the component stays fields-only.
        const MEM: &str = r#"{
            "name": "memsynth", "contract_version": 1, "dependencies": [],
            "modules": [{ "name": "widgets",
                "entities": [{ "name": "Widget", "fields": [{ "name": "label", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "get_widget", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Widget" } } ] }]
        }"#;
        let dm = document(&serde_json::from_str::<Design>(MEM).unwrap());
        assert!(
            dm["components"]["schemas"]["Widget"]["properties"]
                .get("id")
                .is_none(),
            "memory-mode component carries no synthetic id: {}",
            dm["components"]["schemas"]["Widget"]
        );
    }

    /// #274: in db mode an optional (`required:false`) field and a `set_null` fk are
    /// `Option<T>` columns — they serialize `None` as an explicit `null` and accept
    /// `null` on input — so their schemas must admit null (`type: [T, "null"]`) on
    /// BOTH the response component and the request DTO. A required field / cascade fk
    /// stays a bare scalar; a JSON field's `{}` already admits null. Memory-mode
    /// optional fields are bare `T` with `#[serde(default)]` (a `null` fails to
    /// deserialize), so they stay non-nullable — every memory document byte-identical.
    #[test]
    fn db_optional_and_set_null_fields_advertise_nullable() {
        const DB: &str = r#"{
            "name": "nulls", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "items",
                "entities": [
                    { "name": "Cat", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "name", "type": "string" } ]},
                    { "name": "Item",
                      "belongs_to": [
                          { "entity": "Cat", "as": "primary_cat", "on_delete": "cascade" },
                          { "entity": "Cat", "as": "backup_cat", "on_delete": "set_null" } ],
                      "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "title", "type": "string" },
                        { "name": "note", "type": "string", "required": false },
                        { "name": "meta", "type": "json", "required": false } ]} ],
                "endpoints": [
                    { "operation_id": "make_item", "method": "POST", "path": "/",
                      "request_body": { "entity": "Item" },
                      "success": { "status": 201, "entity": "Item" } } ] }]
        }"#;
        let item = document(&serde_json::from_str::<Design>(DB).unwrap())["components"]["schemas"]
            ["Item"]
            .clone();
        let props = &item["properties"];
        // Required scalar stays bare; optional scalar admits null.
        assert_eq!(props["title"]["type"], "string");
        assert_eq!(props["note"]["type"], json!(["string", "null"]));
        // Optional JSON field's `{}` already admits null — no `type` key added.
        assert!(
            props["meta"].get("type").is_none(),
            "json field unchanged: {item}"
        );
        // Cascade fk bare; set_null fk nullable.
        assert_eq!(props["primary_cat_id"]["type"], "integer");
        assert_eq!(props["backup_cat_id"]["type"], json!(["integer", "null"]));
        // The optional field is present but NOT required; nullability ≠ optionality.
        let req = item["required"].as_array().unwrap();
        assert!(
            !req.iter().any(|v| v == "note"),
            "optional not required: {item}"
        );
        assert!(
            req.iter().any(|v| v == "title"),
            "required stays required: {item}"
        );

        // Memory mode: the same optional field stays a bare scalar (non-nullable).
        const MEM: &str = r#"{
            "name": "memnulls", "contract_version": 1, "dependencies": [],
            "modules": [{ "name": "items",
                "entities": [{ "name": "Item", "fields": [
                    { "name": "title", "type": "string" },
                    { "name": "note", "type": "string", "required": false } ]}],
                "endpoints": [
                    { "operation_id": "make_item", "method": "POST", "path": "/",
                      "request_body": { "entity": "Item" },
                      "success": { "status": 201, "entity": "Item" } } ] }]
        }"#;
        let mprops = document(&serde_json::from_str::<Design>(MEM).unwrap())["components"]["schemas"]
            ["Item"]["properties"]
            .clone();
        assert_eq!(
            mprops["note"]["type"], "string",
            "memory optional stays non-nullable"
        );
    }

    /// #274 (request side): a db-mode `{Entity}Request` DTO also types an optional
    /// field as `Option<T>`, so its schema must admit null. Guarded per-user `Post`
    /// (identity fk → `PostRequest` minted, omitting `user_id`): its optional
    /// `subtitle` advertises `["string","null"]` while required `body` stays `string`.
    #[test]
    fn db_request_dto_optional_field_is_nullable() {
        const D: &str = r#"{
            "name": "reqnull", "contract_version": 1, "dependencies": ["db", "auth"],
            "auth": { "model": "jwt", "roles": ["user"] },
            "modules": [{ "name": "posts",
                "entities": [
                    { "name": "User", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "email", "type": "string" } ]},
                    { "name": "Post",
                      "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                      "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "body", "type": "string" },
                        { "name": "subtitle", "type": "string", "required": false } ]} ],
                "endpoints": [
                    { "operation_id": "create_post", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Post" },
                      "success": { "status": 201, "entity": "Post" } } ] }]
        }"#;
        let d = document(&serde_json::from_str::<Design>(D).unwrap());
        let dto = &d["components"]["schemas"]["PostRequest"];
        assert!(
            dto["properties"].get("user_id").is_none(),
            "identity fk omitted from the request DTO: {dto}"
        );
        assert_eq!(dto["properties"]["body"]["type"], "string");
        assert_eq!(
            dto["properties"]["subtitle"]["type"],
            json!(["string", "null"])
        );
    }

    /// #274 guard: a declared `id` is NEVER nullable in the component, even if a design
    /// (pathologically) marks it `required:false`. `model_rs_db` emits the pk as a
    /// non-`Option` `pub id` and ignores that flag, so the contract must keep it a bare
    /// non-null scalar — the nullable branch excludes a field named `id`.
    #[test]
    fn declared_id_is_never_nullable_even_if_marked_optional() {
        const D: &str = r#"{
            "name": "optid", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "things",
                "entities": [{ "name": "Thing", "fields": [
                    { "name": "id", "type": "integer", "required": false },
                    { "name": "label", "type": "string" } ]}],
                "endpoints": [
                    { "operation_id": "get_thing", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Thing" } } ] }]
        }"#;
        let thing = document(&serde_json::from_str::<Design>(D).unwrap())["components"]["schemas"]
            ["Thing"]
            .clone();
        assert_eq!(
            thing["properties"]["id"]["type"], "integer",
            "a declared id stays a non-null pk regardless of its required flag: {thing}"
        );
    }

    /// #269: a 204 (No Content) or a 3xx (redirect) success emits NO response body
    /// in the contract even when the route declares an `entity`/`list`, because the
    /// generated handler is `NoContent`/`Redirect` (empty-bodied). A 204-with-body is
    /// invalid per HTTP/OpenAPI, and a client generated from the contract would try to
    /// parse an absent body. A 200/201 success with the same entity keeps its body
    /// schema. WHY (Rule 9): before this, `success_schema` keyed only on `entity`/`list`
    /// — ignoring `success.status` — and advertised a phantom `Thing` body for the
    /// 204/303 routes while the running handler sends nothing (the doc lied about the
    /// code). The validator (`questions.rs`) deliberately ALLOWS `{204|3xx, entity}`,
    /// so this shape is reachable.
    #[test]
    fn bodyless_status_success_omits_response_body() {
        const D: &str = r#"{
            "name": "statusdoc", "contract_version": 0, "dependencies": [],
            "modules": [{ "name": "things",
                "entities": [{ "name": "Thing", "fields": [
                    { "name": "id", "type": "integer" },
                    { "name": "label", "type": "string" } ]}],
                "endpoints": [
                    { "operation_id": "make_thing", "method": "POST", "path": "/",
                      "request_body": { "entity": "Thing" },
                      "success": { "status": 201, "entity": "Thing" } },
                    { "operation_id": "clear_thing", "method": "DELETE", "path": "/{id}",
                      "success": { "status": 204, "entity": "Thing" } },
                    { "operation_id": "redir_thing", "method": "GET", "path": "/go/{id}",
                      "success": { "status": 303, "entity": "Thing", "list": true } },
                    { "operation_id": "get_thing", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Thing" } } ] }]
        }"#;
        let d = document(&serde_json::from_str::<Design>(D).unwrap());
        // 204 → NoContent handler: `description` only, no body content, regardless of
        // the declared entity (a 204-with-body is invalid).
        let del = &d["paths"]["/things/{id}"]["delete"]["responses"]["204"];
        assert_eq!(del["description"], "success");
        assert!(
            del.get("content").is_none(),
            "204 success advertises no body: {del}"
        );
        // 3xx → Redirect handler: no body even with entity + list.
        let redir = &d["paths"]["/things/go/{id}"]["get"]["responses"]["303"];
        assert!(
            redir.get("content").is_none(),
            "3xx success advertises no body: {redir}"
        );
        // A 201 create keeps its entity body schema (regression guard — the fix must
        // not suppress bodies on body-bearing statuses).
        assert_eq!(
            d["paths"]["/things/"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/Thing",
            "a 201 create still advertises its entity body: {}",
            d["paths"]["/things/"]["post"]["responses"]["201"]
        );
        // And a 200 GET keeps its entity body too.
        assert_eq!(
            d["paths"]["/things/{id}"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/Thing"
        );
    }

    /// Issue #80: declared range/length constraints ride into the document as
    /// JSON Schema keywords — `minimum`/`maximum` on integer fields,
    /// `minLength`/`maxLength` on string fields — on BOTH the entity component
    /// and the request DTO schemas, so a generated client knows the bounds the
    /// deserialize-validator enforces. An unconstrained design's document stays
    /// byte-identical (no keyword ever emitted), and `values` is NOT backfilled
    /// to `enum` (out of scope — it would diff existing enum documents).
    #[test]
    fn constraints_ride_into_schemas() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::CONSTRAINT_DTO)
                .unwrap(),
        );
        let item = &d["components"]["schemas"]["Item"]["properties"];
        assert_eq!(item["quantity"]["minimum"], json!(1), "entity minimum");
        assert_eq!(item["quantity"]["maximum"], json!(600), "entity maximum");
        assert_eq!(item["code"]["maxLength"], json!(5), "entity maxLength");
        assert_eq!(item["note"]["minLength"], json!(2), "entity minLength");
        let req = &d["components"]["schemas"]["ItemRequest"]["properties"];
        assert_eq!(req["quantity"]["minimum"], json!(1), "request DTO minimum");
        assert_eq!(
            req["quantity"]["maximum"],
            json!(600),
            "request DTO maximum"
        );
        assert_eq!(req["code"]["maxLength"], json!(5), "request DTO maxLength");
        let upd = &d["components"]["schemas"]["ItemUpdateRequest"]["properties"];
        assert_eq!(upd["code"]["maxLength"], json!(5), "update DTO maxLength");
        assert!(
            item["status"].get("enum").is_none(),
            "values must NOT backfill to enum (would diff existing documents): {}",
            item["status"]
        );
        // No-drift: the unconstrained golden document gains none of the keywords.
        let plain = document_json(&serde_json::from_str::<Design>(GOLDEN).unwrap());
        for kw in ["minimum", "maximum", "minLength", "maxLength"] {
            assert!(
                !plain.contains(kw),
                "unconstrained golden must not gain `{kw}`"
            );
        }
    }

    /// #112: a `write_only` field (and any `password_hash` column) is marked
    /// `writeOnly: true` on the shared entity component — present in requests,
    /// omitted from responses, correct for both. A normal field gains no keyword,
    /// and the request DTO KEEPS the field (input path). The unconstrained golden
    /// gains no `writeOnly` (byte-identity for designs with no hidden field).
    #[test]
    fn write_only_fields_are_marked_writeonly_in_the_document() {
        let d = document(
            &serde_json::from_str::<Design>(crate::platform::genroute::tests::WRITE_ONLY).unwrap(),
        );
        let account = &d["components"]["schemas"]["Account"]["properties"];
        assert_eq!(
            account["api_token"]["writeOnly"],
            json!(true),
            "an explicit write_only field is writeOnly"
        );
        assert_eq!(
            account["password_hash"]["writeOnly"],
            json!(true),
            "a password_hash column is auto-marked writeOnly"
        );
        assert!(
            account["email"].get("writeOnly").is_none(),
            "a normal field gains no writeOnly: {}",
            account["email"]
        );
        // The request DTO keeps the field (input path unaffected).
        assert!(
            d["components"]["schemas"]["AccountRequest"]["properties"]["api_token"].is_object(),
            "the write_only field stays in the request schema (input)"
        );
        // No-drift: a design with no hidden field gains no writeOnly.
        let plain = document_json(&serde_json::from_str::<Design>(GOLDEN).unwrap());
        assert!(
            !plain.contains("writeOnly"),
            "a design with no write_only/password_hash gains no writeOnly"
        );
    }

    /// The `required_roles.is_empty()` conjunct is LOAD-BEARING: a ROLE-GATED GET
    /// on a `public_read` entity keeps its guard (genroute keeps `CurrentUser`),
    /// so it must keep its `security` stanza. Deleting the conjunct from the
    /// shared predicate must turn this red.
    #[test]
    fn role_gated_get_on_a_public_read_entity_keeps_security() {
        let mut design: Design =
            serde_json::from_str(crate::platform::genroute::tests::PUBLIC_READ).unwrap();
        design.modules[1]
            .endpoints
            .iter_mut()
            .find(|ep| ep.operation_id == "list_posts")
            .unwrap()
            .required_roles = vec!["user".to_string()];
        let d = document(&design);
        assert_eq!(
            d["paths"]["/posts/"]["get"]["security"],
            json!([{ "cookieAuth": [] }]),
            "a role-gated GET keeps its guard AND its advertised credential: {}",
            d["paths"]["/posts/"]["get"]
        );
    }

    /// The strict-resolution pin (#105 whole-branch review): an ENTITY-LESS
    /// `auth_required` GET (`GET /stats`, custom-JSON success, no `{param}`)
    /// beside a `public_read` FIRST entity KEEPS its `security` stanza — the
    /// first-entity fallback tied it to `Post` and stripped the stanza, so the
    /// shipped contract advertised an open endpoint for a route the design
    /// declared authenticated (genroute keeps its `CurrentUser`; the doc must
    /// keep the credential the handler demands).
    #[test]
    fn entityless_authed_get_beside_a_public_read_entity_keeps_security() {
        let mut design: Design =
            serde_json::from_str(crate::platform::genroute::tests::PUBLIC_READ).unwrap();
        let stats: Endpoint = serde_json::from_str(
            r#"{ "operation_id": "get_stats", "method": "GET", "path": "/stats",
                 "auth_required": true, "success": { "status": 200 } }"#,
        )
        .unwrap();
        design.modules[1].endpoints.push(stats);
        let d = document(&design);
        assert_eq!(
            d["paths"]["/posts/stats"]["get"]["security"],
            json!([{ "cookieAuth": [] }]),
            "an entity-less auth_required GET keeps its advertised credential: {}",
            d["paths"]["/posts/stats"]["get"]
        );
        // The explicit public reads still advertise none.
        assert!(
            d["paths"]["/posts/"]["get"].get("security").is_none(),
            "the explicit public_read list GET still carries no stanza: {}",
            d["paths"]["/posts/"]["get"]
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
        // #271 gate-lock: the memory-mode `Note` component omits the `belongs_to` fk
        // columns — the memory Model struct (`model_rs`) carries only `fields`, so the
        // plain `Json<Note>` body has no `user_id`/`folder_id`. This positively pins
        // the `wants_db()` gate on the fk emission: if that gate were dropped, this
        // memory component would gain server-supplied fk columns the struct lacks.
        let note = &d["components"]["schemas"]["Note"]["properties"];
        assert!(
            note.get("user_id").is_none() && note.get("folder_id").is_none(),
            "memory-mode entity component carries no fk columns: {note}"
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

    /// Issue #107 / 0.6.0 Task 2: the generated member-management routes are
    /// FIRST-CLASS tenant routes, so — unlike storage buckets — they appear in
    /// the OpenAPI document: a generated client can list/add/re-role/remove
    /// members without ever reading the generated Rust. The contract encodes the
    /// authorization design: guarded ops advertise the design's credential, the
    /// role wire-values are pinned to the declared member_roles (a client can't
    /// even send an off-set role), and the 403/409/422 responses spell out the
    /// admin gate, the last-admin lockout, and the role validation.
    #[test]
    fn tenancy_document_advertises_the_member_surface() {
        let design: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let d = document(&design);
        let list = &d["paths"]["/workspaces/{workspace_id}/members"]["get"];
        assert_eq!(
            list["operationId"], "list_workspace_members",
            "list op: {d}"
        );
        // Guarded like every tenant route: the jwt design advertises bearerAuth.
        assert_eq!(list["security"], json!([{ "bearerAuth": [] }]));
        // The roster item shape is the T1 row: {id, user_id, role}.
        let item = &list["responses"]["200"]["content"]["application/json"]["schema"]["items"];
        assert_eq!(item["required"], json!(["id", "user_id", "role"]));
        // role is pinned to the DECLARED member_roles.
        assert_eq!(
            item["properties"]["role"]["enum"],
            json!(["owner", "member"])
        );
        // The fk param types as the tenant key (integer pk here).
        assert_eq!(list["parameters"][0]["name"], "workspace_id");
        assert_eq!(list["parameters"][0]["schema"]["type"], "integer");

        let add = &d["paths"]["/workspaces/{workspace_id}/members"]["post"];
        assert_eq!(add["operationId"], "add_workspace_member");
        let body = &add["requestBody"]["content"]["application/json"]["schema"];
        assert_eq!(body["required"], json!(["user_id", "role"]));
        assert!(add["responses"]["201"].is_object(), "add is a 201: {add}");
        for status in ["403", "404", "409", "422"] {
            assert!(
                add["responses"][status].is_object(),
                "add advertises {status}: {add}"
            );
        }

        let set = &d["paths"]["/workspaces/{workspace_id}/members/{user_id}"]["patch"];
        assert_eq!(set["operationId"], "set_workspace_member_role");
        assert_eq!(set["parameters"][1]["name"], "user_id");
        assert_eq!(set["parameters"][1]["schema"]["type"], "string");
        assert!(set["responses"]["204"].is_object());
        assert_eq!(
            set["responses"]["409"]["description"], "cannot demote the last owner",
            "the last-admin lockout is contract-visible: {set}"
        );

        let remove = &d["paths"]["/workspaces/{workspace_id}/members/{user_id}"]["delete"];
        assert_eq!(remove["operationId"], "remove_workspace_member");
        assert!(remove["responses"]["204"].is_object());
        assert_eq!(
            remove["responses"]["409"]["description"],
            "cannot remove the last owner"
        );
        // The self-removal exception is part of the advertised contract.
        assert!(
            remove["responses"]["403"]["description"]
                .as_str()
                .unwrap()
                .contains("self-removal"),
            "DELETE documents the self-removal exception: {remove}"
        );
    }

    /// NO-DRIFT: a design without the member surface — no tenancy at all, or a
    /// tenancy design in a mode genroute wouldn't emit for — advertises NO
    /// member routes, keeping the document byte-identical to pre-#107.
    #[test]
    fn non_tenancy_documents_have_no_member_paths() {
        // (a) the plain todos design.
        let d = doc();
        assert!(
            d["paths"]
                .as_object()
                .unwrap()
                .keys()
                .all(|k| !k.contains("/members")),
            "no member paths without tenancy: {d}"
        );
        // (b) the SAME tenancy design with tenancy stripped loses exactly the
        // member paths and nothing else.
        let design: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let mut stripped = design.clone();
        stripped.tenancy = None;
        let with = document(&design);
        let without = document(&stripped);
        let with_paths = with["paths"].as_object().unwrap();
        let without_paths = without["paths"].as_object().unwrap();
        let extra: Vec<&str> = with_paths
            .keys()
            .filter(|k| !without_paths.contains_key(*k))
            .map(String::as_str)
            .collect();
        assert_eq!(
            extra,
            vec![
                "/workspaces/{workspace_id}/members",
                "/workspaces/{workspace_id}/members/{user_id}"
            ],
            "the ONLY path delta is the member surface"
        );
        for (k, v) in without_paths {
            assert_eq!(
                v, &with_paths[k],
                "shared path `{k}` must be identical with/without tenancy"
            );
        }
    }

    /// #115: a create (POST) on an entity with a composite `unique` group carries a
    /// documented 409 whose description names the columns; a plain create does not.
    #[test]
    fn composite_unique_create_documents_a_409() {
        const LIKES: &str = r#"{
            "name": "likes-api", "contract_version": 1,
            "dependencies": ["db"],
            "modules": [{
                "name": "engagement",
                "entities": [
                    { "name": "Post", "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "Like",
                      "belongs_to": [{ "entity": "Post" }],
                      "unique": [["post_id", "reaction"]],
                      "fields": [{ "name": "reaction", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_post", "method": "POST", "path": "/posts",
                      "request_body": { "entity": "Post" },
                      "success": { "status": 201, "entity": "Post" } },
                    { "operation_id": "create_like", "method": "POST", "path": "/likes",
                      "request_body": { "entity": "Like" },
                      "success": { "status": 201, "entity": "Like" } }
                ]
            }]
        }"#;
        let d = document(&serde_json::from_str::<Design>(LIKES).unwrap());
        let like = &d["paths"]["/engagement/likes"]["post"];
        assert_eq!(
            like["responses"]["409"]["description"],
            "a row with the same (post_id, reaction) already exists",
            "the composite-unique create must document a 409 naming the columns: {like}"
        );
        // A plain create (no composite unique on Post) gets no 409.
        let post = &d["paths"]["/engagement/posts"]["post"];
        assert!(
            post["responses"].get("409").is_none(),
            "a plain create carries no composite-unique 409: {post}"
        );
    }
}
