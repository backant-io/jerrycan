//! Per-module code generation: route crates, fractal subroutes, handler stubs.
//! Ownership rule: Cargo.toml/lib.rs/subroutes mod.rs files are TOOL-owned
//! (always rewritten); handlers/model/repo/deps are AGENT-owned (create-once).

use super::design::*;
use super::templates::{ROUTE_CARGO, render};
use std::fs;
use std::path::Path;

/// Generation mode derived from the design's reserved dependencies.
#[derive(Debug, Clone, Copy, Default)]
pub struct GenMode {
    pub db: bool,
    pub auth: bool,
}

/// snake/ident helpers: crate `route-todo-list` → ident `route_todo_list`.
pub fn crate_ident(module_name: &str) -> String {
    format!("route_{}", module_name.replace('-', "_"))
}

/// The entity's declared `id` field, if the design provides one — it becomes
/// the table's primary key (no synthetic pk is added alongside it).
fn declared_id(e: &Entity) -> Option<FieldType> {
    e.fields
        .iter()
        .find(|f| f.name == "id")
        .map(|f| f.field_type)
}

/// The Rust type repos and `/{id}` handlers key on: the declared id field's
/// rust_type (String for text pks), i64 for integer or synthetic ids.
fn key_rust_type(e: &Entity) -> &'static str {
    match declared_id(e) {
        Some(t) => t.rust_type(),
        None => "i64",
    }
}

/// The bare collection path a parameterized endpoint acts under: its path with the
/// trailing `/{param}` removed (`/tasks/{id}` → `/tasks`, `/{id}` → `/`). None when
/// the path carries no `{param}` (nothing to strip). Mirrors testgen's
/// `collection_path`, kept local so the two generators stay decoupled.
fn collection_path(ep: &Endpoint) -> Option<String> {
    let p = ep.path.as_str();
    let brace = p.rfind('{')?;
    let cut = p[..brace].rfind('/').unwrap_or(0);
    Some(if cut == 0 {
        "/".to_string()
    } else {
        p[..cut].to_string()
    })
}

/// The POST creator (with a body) mounted at a bare collection `path` in this
/// module — the route whose entity owns the rows addressable under `path/{id}`.
fn creator_at<'a>(m: &'a ModuleDesign, path: &str) -> Option<&'a Endpoint> {
    m.endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::POST && ep.path == path && ep.request_body.is_some())
}

/// The entity whose repo/model a route's handler binds. Resolution order: the
/// request body's entity, then the success entity, then — for a no-body endpoint
/// like `DELETE /{id}` that names neither (issue #56) — the entity of the
/// COLLECTION it acts under (its parent path's POST creator), so a multi-entity
/// module's `/tasks/{id}` stub binds `TaskRepo`, not the module's FIRST entity.
/// Falls back to the first entity only when path-based resolution finds nothing
/// (a bare `/import`, or a module with no matching creator) — byte-identical to
/// the pre-#56 behavior for every single-entity module (the collection creator IS
/// the sole entity there).
fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .map(|rb| rb.entity.as_str())
        .or(ep.success.entity.as_deref())
        .or_else(|| {
            collection_path(ep)
                .and_then(|coll| creator_at(m, &coll))
                .and_then(|c| c.request_body.as_ref())
                .map(|rb| rb.entity.as_str())
        })
        .or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

fn return_type(ep: &Endpoint) -> String {
    let entity = ep.success.entity.as_deref();
    match (ep.success.status, entity, ep.success.list) {
        (204, _, _) => "Result<NoContent>".to_string(),
        // A 3xx-success endpoint (issue #46) redirects — it returns a `Redirect`,
        // never a JSON body. `success_body` emits a matching `Ok(Redirect::…)` stub.
        (s, _, _) if (300..400).contains(&s) => "Result<Redirect>".to_string(),
        (201, Some(e), _) => format!("Result<Created<{e}>>"),
        (201, None, _) => "Result<Created<serde_json::Value>>".to_string(),
        (_, Some(e), true) => format!("Result<Json<Vec<{e}>>>"),
        (_, Some(e), false) => format!("Result<Json<{e}>>"),
        (_, None, _) => "Result<Json<serde_json::Value>>".to_string(),
    }
}

fn path_params(ep: &Endpoint) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = ep.path.as_str();
    while let Some(start) = rest.find('{') {
        let Some(end_rel) = rest[start..].find('}') else {
            break;
        };
        out.push(rest[start + 1..start + end_rel].to_string());
        rest = &rest[start + end_rel + 1..];
    }
    out
}

/// True when this endpoint operates on a tenant-owned entity (its repo entity
/// belongs_to the design's tenancy entity). Such guarded endpoints take the
/// membership-checked `Dep<shared::Tenant>` instead of a bare `CurrentUser`.
fn endpoint_is_tenant_owned(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> bool {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return false;
    };
    let Some(entity) = endpoint_repo_entity(m, ep) else {
        return false;
    };
    m.entities
        .iter()
        .find(|e| e.name == entity)
        .is_some_and(|e| e.belongs_to.iter().any(|b| b.entity == tenancy.entity))
}

/// The request-DTO rule (issues #34 + #53) at the Rust layer: an endpoint whose
/// body entity has a field the wire contract drops takes `Json<{Entity}Request>`
/// (the trimmed DTO — see `request_dto_rs`) instead of `Json<{Entity}>`. Three
/// drop reasons: the server-owned identity fk (#34, guarded+auth — server injects
/// the session user's id), a `default` field (#53a — server applies the declared
/// value), or a path-redundant parent fk (#53b — the handler injects the path
/// value). db-gated because only `model_rs_db` structs surface these columns at
/// all (the memory-mode entity has no fk columns, and defaults there stay
/// required — the DTO lives in db mode alongside the SeaORM model).
fn endpoint_takes_request_dto(
    m: &ModuleDesign,
    ep: &Endpoint,
    mode: GenMode,
    design: &Design,
) -> bool {
    mode.db && design.endpoint_uses_request_dto(m, ep, mode.auth)
}

/// True when this endpoint gets the server-side realtime publish wiring (issue
/// #50): a MUTATING endpoint (POST/PUT/PATCH/DELETE — the "created a row, now
/// push it" shape) in a design that declares a server-publishable broadcast
/// topic (scope `none`/`auth`). GET endpoints and designs with no such topic
/// (realtime-free, or only tenant-scoped broadcasts) emit nothing, keeping their
/// handlers byte-identical.
fn endpoint_emits_realtime_publish(ep: &Endpoint, design: &Design) -> bool {
    !matches!(ep.method, HttpMethod::GET) && design.server_publishable_broadcast().is_some()
}

fn handler_params(m: &ModuleDesign, ep: &Endpoint, mode: GenMode, design: &Design) -> String {
    let mut params = Vec::new();
    if let Some(e) = endpoint_repo_entity(m, ep) {
        params.push(format!("_repo: Dep<{e}Repo>"));
    }
    // Guard param (order: repo, guard, path, body). A guarded endpoint on a
    // tenant-owned entity takes the membership-checked `Dep<shared::Tenant>` (the
    // Tenant factory consumes CurrentUser, so auth is still enforced: 401 from a
    // missing session, 403 from no membership); other guarded endpoints take the
    // bare authenticated session.
    if mode.auth && ep.is_guarded() {
        if endpoint_is_tenant_owned(m, ep, design) {
            params.push("_tenant: Dep<Tenant>".to_string());
        } else {
            params.push("_user: CurrentUser".to_string());
        }
    }
    // Server-side realtime publish (issue #50): a mutating handler in a
    // server-publishable-broadcast design resolves the RealtimeHandle so it can
    // push the write to subscribers (fully-qualified path — no `use` line to
    // keep byte-identical output for designs the rule doesn't touch).
    if endpoint_emits_realtime_publish(ep, design) {
        params.push("_rt: Dep<jerrycan::realtime::RealtimeHandle>".to_string());
    }
    let params_in_path = path_params(ep);
    // A param named `id` keys the endpoint's entity, so it takes that entity's
    // key type (String for text pks); other params stay i64.
    let key = endpoint_repo_entity(m, ep)
        .and_then(|name| m.entities.iter().find(|e| e.name == name))
        .map(key_rust_type)
        .unwrap_or("i64");
    let param_type = |p: &str| if p == "id" { key } else { "i64" };
    match params_in_path.len() {
        0 => {}
        1 => params.push(format!(
            "Path(_{p}): Path<{ty}>",
            p = params_in_path[0],
            ty = param_type(&params_in_path[0])
        )),
        _ => {
            let names: Vec<String> = params_in_path.iter().map(|p| format!("_{p}")).collect();
            let types = params_in_path
                .iter()
                .map(|p| param_type(p))
                .collect::<Vec<_>>()
                .join(", ");
            params.push(format!("Path(({})): Path<({})>", names.join(", "), types));
        }
    }
    if let Some(ref rb) = ep.request_body {
        // Server-owned FK (issue #34): the guarded body drops `user_id`, so the
        // param type is the `{Entity}Request` DTO, not the entity itself.
        if endpoint_takes_request_dto(m, ep, mode, design) {
            params.push(format!("Json(_body): Json<{}Request>", rb.entity));
        } else {
            params.push(format!("Json(_body): Json<{}>", rb.entity));
        }
    }
    params.join(", ")
}

/// A leading comment for role-guarded endpoints, reminding the agent how to
/// enforce the role before proceeding (empty for unguarded / no-role endpoints).
/// A tenant-owned endpoint carries `_tenant: Dep<Tenant>` and checks the role on
/// the membership (`_tenant.require_role(...)?`); other endpoints take a bare
/// `CurrentUser` and call `require_role(&_user.0.role, ...)` directly.
fn guard_comment(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> String {
    if ep.required_roles.is_empty() {
        return String::new();
    }
    let roles = ep.required_roles.join("\", \"");
    if endpoint_is_tenant_owned(m, ep, design) {
        format!(
            "    // guard: requires role \"{roles}\" — call _tenant.require_role(\"{roles}\")? before proceeding\n"
        )
    } else {
        format!(
            "    // guard: requires role \"{roles}\" — add `use jerrycan::auth::require_role;` and call require_role(&_user.0.role, \"{roles}\")? before proceeding\n"
        )
    }
}

/// The stub body a generated handler returns until the agent implements it.
/// Every non-redirect handler returns a 500 (`Err(Error::internal(...))`) so the
/// acceptance suite is RED until real logic lands. A 3xx-success endpoint (issue
/// #46) instead gets a COMPILING `Redirect`-shaped stub whose status already
/// matches the contract — the agent only needs to point it at the real target, not
/// hand-switch the whole return type from `Json`. The `"/"` placeholder is flagged
/// with a TODO; the constructor is chosen so the declared 3xx status is emitted
/// verbatim (302→`to`, 303→`see_other`, 307→`temporary`, 308→`permanent`; any other
/// 3xx falls back to `see_other`, which the agent adjusts alongside the target).
fn success_body(ep: &Endpoint) -> String {
    let status = ep.success.status;
    if (300..400).contains(&status) {
        let ctor = match status {
            302 => "to",
            307 => "temporary",
            308 => "permanent",
            _ => "see_other",
        };
        return format!(
            "    // TODO (issue #46): redirect to the real target — replace \"/\" with the\n    // destination this {status} endpoint should send clients to.\n    Ok(Redirect::{ctor}(\"/\"))"
        );
    }
    format!(
        "    Err(Error::internal(\"{op} not implemented — replace this stub\"))",
        op = ep.operation_id
    )
}

pub(crate) fn handlers_rs(m: &ModuleDesign, mode: GenMode, design: &Design) -> String {
    let mut uses = String::from("use jerrycan::prelude::*;\n");
    let mentions_entities = m
        .endpoints
        .iter()
        .any(|ep| ep.request_body.is_some() || ep.success.entity.is_some());
    if mentions_entities {
        uses.push_str("use super::model::*;\n");
    }
    if !m.entities.is_empty() {
        uses.push_str("use super::repo::*;\n");
    }
    // Auth mode: a guarded endpoint takes either `_tenant: Dep<Tenant>` (tenant-
    // owned) or `_user: CurrentUser` (everything else). Import ONLY the alias(es)
    // a param actually uses, or `-D warnings` trips on an unused import. The agent
    // adds `require_role` itself (see guard_comment); a raw stub never calls it.
    if mode.auth {
        let needs_tenant = m
            .endpoints
            .iter()
            .any(|ep| ep.is_guarded() && endpoint_is_tenant_owned(m, ep, design));
        let needs_user = m
            .endpoints
            .iter()
            .any(|ep| ep.is_guarded() && !endpoint_is_tenant_owned(m, ep, design));
        if needs_tenant {
            uses.push_str("use shared::Tenant;\n");
        }
        if needs_user {
            uses.push_str("use shared::CurrentUser;\n");
        }
    }
    let mut out = format!(
        "//! Handlers for `{}` — thin: extract → call → respond.\n//! Generated stubs return 500 until implemented.\n{uses}\n",
        m.name
    );
    for ep in &m.endpoints {
        let guard = if mode.auth {
            guard_comment(m, ep, design)
        } else {
            String::new()
        };
        let server_owned = server_owned_fk_comment(m, ep, mode, design);
        let realtime = realtime_publish_comment(ep, design);
        out.push_str(&format!(
            "pub(crate) async fn {op}({params}) -> {ret} {{\n{guard}{server_owned}{realtime}{body}\n}}\n\n",
            op = ep.operation_id,
            params = handler_params(m, ep, mode, design),
            ret = return_type(ep),
            body = success_body(ep),
        ));
    }
    out
}

/// A leading comment for endpoints whose body DTO omits the identity FK (issue
/// #34): tells the agent the SERVER injects the session user's id — the body
/// (`{Entity}Request`) has no `user_id`, so the handler must set it when
/// building the entity. Tenant-owned handlers take `Dep<Tenant>` (no session
/// param in the stub), so their variant says to add a `CurrentUser` param.
fn server_owned_fk_comment(
    m: &ModuleDesign,
    ep: &Endpoint,
    mode: GenMode,
    design: &Design,
) -> String {
    if !endpoint_takes_request_dto(m, ep, mode, design) {
        return String::new();
    }
    let entity = &ep.request_body.as_ref().expect("dto implies body").entity;
    let Some(e) = m.entities.iter().find(|e| &e.name == entity) else {
        return String::new();
    };
    let mut out = String::new();
    // Identity fk (#34): guarded + auth → the wire body omits `user_id`. The rule is
    // method-agnostic, but the STUB GUIDANCE is not (issue #42): a CREATE (POST)
    // injects the session user's id; an UPDATE (PUT/PATCH) must PRESERVE the existing
    // row's owner — reassigning it to the caller would let an admin editing another
    // user's row silently take ownership. So split the note by method.
    if mode.auth && design.endpoint_omits_identity_fk(m, ep) {
        let is_create = matches!(ep.method, HttpMethod::POST);
        let tenant_owned = endpoint_is_tenant_owned(m, ep, design);
        out.push_str(&match (is_create, tenant_owned) {
            (true, true) => format!(
                "    // server-owned fk: `{entity}Request` has NO `user_id` — the server injects the\n    // session user's id. Add a `user: CurrentUser` param and use `user.0.id` (the\n    // stringified user pk; parse it for an integer fk) when building the {entity}.\n"
            ),
            (true, false) => format!(
                "    // server-owned fk: `{entity}Request` has NO `user_id` — the server injects the\n    // session user's id. Use `_user.0.id` (the stringified user pk; parse it for an\n    // integer fk) when building the {entity}.\n"
            ),
            (false, true) => format!(
                "    // server-owned fk: `{entity}Request` has NO `user_id` — on UPDATE, PRESERVE the\n    // existing row's owner. Do NOT reassign `user_id`; scope the update through the\n    // membership (`_tenant`) so a non-owner can't take the row.\n"
            ),
            (false, false) => format!(
                "    // server-owned fk: `{entity}Request` has NO `user_id` — on UPDATE, PRESERVE the\n    // existing row's owner. Do NOT reassign `user_id` to `_user.0.id`; scope the\n    // UPDATE to the owner (e.g. WHERE user_id = _user.0.id) so a non-owner can't take it.\n"
            ),
        });
    }
    // Path-redundant parent fk (#53b): comes from the endpoint's own path param
    // (`_{col}`), so the handler injects it instead of reading it from the body.
    for col in design.entity_path_fk_columns(entity) {
        out.push_str(&format!(
            "    // path-owned fk: `{entity}Request` has NO `{col}` — inject the `_{col}` path\n    // value (the handler's Path param) when building the {entity}.\n"
        ));
    }
    // Server-owned defaults (#53a): the DTO omits each default field; the handler
    // writes the declared value into the NOT-NULL column.
    let defaults: Vec<String> = e
        .fields
        .iter()
        .filter_map(|f| {
            f.default.as_ref().map(|v| {
                format!(
                    "`{}` = {}",
                    f.name,
                    serde_json::to_string(v).unwrap_or_else(|_| "…".into())
                )
            })
        })
        .collect();
    if !defaults.is_empty() {
        out.push_str(&format!(
            "    // server-owned defaults: `{entity}Request` omits {} — set each to its\n    // declared default when building the {entity}.\n",
            defaults.join(", ")
        ));
    }
    out
}

/// The stub comment for server-side realtime publish (issue #50): shows the
/// one-liner that pushes this write to every subscriber of a declared broadcast
/// topic, using the `_rt: Dep<RealtimeHandle>` param `handler_params` added.
/// Empty unless `endpoint_emits_realtime_publish`, so untouched designs stay
/// byte-identical.
fn realtime_publish_comment(ep: &Endpoint, design: &Design) -> String {
    if !endpoint_emits_realtime_publish(ep, design) {
        return String::new();
    }
    let topic = design
        .server_publishable_broadcast()
        .expect("gated by endpoint_emits_realtime_publish");
    format!(
        "    // realtime (issue #50): after the write succeeds, push it to every\n    // subscriber of a broadcast topic —\n    //   _rt.publish(\"{topic}\", serde_json::json!({{ /* event payload */ }})).await?;\n"
    )
}

pub(crate) fn model_rs(m: &ModuleDesign) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::from(
        "//! Entities and DTOs for this module.\nuse serde::{Deserialize, Serialize};\n\n",
    );
    for e in &m.entities {
        out.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct ");
        out.push_str(&e.name);
        out.push_str(" {\n");
        for f in &e.fields {
            out.push_str(&keyword_field_attrs(&f.name, "    ", false));
            if !f.required {
                out.push_str("    #[serde(default)]\n");
            }
            out.push_str(&enum_validate_attr(e, f, "    ", ""));
            out.push_str(&format!(
                "    pub {}: {},\n",
                rust_ident(&f.name),
                f.field_type.rust_type()
            ));
        }
        out.push_str("}\n\n");
    }
    out.push_str(&enum_deserialize_fns(&m.entities));
    Some(out)
}

/// serde `rename` (+ sea_orm `column_name` in db mode) attribute line(s) for a
/// field whose name is a Rust keyword: its struct field is emitted as a raw
/// identifier (`type` → `r#type`), so these keep the wire (JSON) name — and the
/// SQL column name — as the original `type`. Empty for a non-keyword field, so
/// output is byte-identical to before for every existing design.
fn keyword_field_attrs(name: &str, indent: &str, db: bool) -> String {
    if !is_rust_keyword(name) {
        return String::new();
    }
    let mut s = format!("{indent}#[serde(rename = \"{name}\")]\n");
    if db {
        s.push_str(&format!("{indent}#[sea_orm(column_name = \"{name}\")]\n"));
    }
    s
}

/// The `#[serde(deserialize_with = ...)]` line wiring an enum `values` field to
/// its generated allow-list validator (issue #47), or empty for a non-enum field
/// so every design WITHOUT `values` stays byte-identical. `path_prefix` is
/// `"super::"` for the db-mode SeaORM Model (nested in `pub mod {snake}`, so the
/// root-level validator is reached via `super::`) and `""` for the root-level
/// memory struct and the `{Entity}Request` DTO.
fn enum_validate_attr(entity: &Entity, f: &Field, indent: &str, path_prefix: &str) -> String {
    if f.values.is_none() {
        return String::new();
    }
    format!(
        "{indent}#[serde(deserialize_with = \"{path_prefix}de_{snake}_{field}\")]\n",
        snake = Design::to_snake(&entity.name),
        field = f.name,
    )
}

/// The root-level `de_{entity}_{field}` validators for every enum `values` field
/// across a module's entities (issue #47). Each rejects an out-of-range value with
/// a serde error, which the `Json` extractor surfaces as `422 JC0422` — so invalid
/// enum input is refused at the request boundary, on EVERY write path (create AND
/// update), BEFORE the database (whose CHECK stays as defense-in-depth). Empty when
/// no entity declares `values`. serde paths are fully qualified so the module's
/// `use` list is untouched (the db root imports nothing; the memory root imports
/// only serde's derives).
fn enum_deserialize_fns(entities: &[Entity]) -> String {
    let mut out = String::new();
    for e in entities {
        let snake = Design::to_snake(&e.name);
        for f in &e.fields {
            let Some(values) = &f.values else {
                continue;
            };
            let allowed = values
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let msg = format!("{} must be one of: {}", f.name, values.join(", "));
            let name = format!("de_{snake}_{}", f.name);
            if f.required {
                out.push_str(&format!(
                    "// Enum validator (issue #47): out-of-range `{field}` → serde error → 422.\nfn {name}<'de, D>(de: D) -> std::result::Result<String, D::Error>\nwhere\n    D: serde::Deserializer<'de>,\n{{\n    let value = <String as serde::Deserialize>::deserialize(de)?;\n    const ALLOWED: &[&str] = &[{allowed}];\n    if !ALLOWED.contains(&value.as_str()) {{\n        return Err(<D::Error as serde::de::Error>::custom(\"{msg}\"));\n    }}\n    Ok(value)\n}}\n\n",
                    field = f.name,
                ));
            } else {
                out.push_str(&format!(
                    "// Enum validator (issue #47): checks `{field}` when present (optional).\nfn {name}<'de, D>(de: D) -> std::result::Result<Option<String>, D::Error>\nwhere\n    D: serde::Deserializer<'de>,\n{{\n    let value = <Option<String> as serde::Deserialize>::deserialize(de)?;\n    if let Some(ref inner) = value {{\n        const ALLOWED: &[&str] = &[{allowed}];\n        if !ALLOWED.contains(&inner.as_str()) {{\n            return Err(<D::Error as serde::de::Error>::custom(\"{msg}\"));\n        }}\n    }}\n    Ok(value)\n}}\n\n",
                    field = f.name,
                ));
            }
        }
    }
    out
}

/// PascalCase a snake_case column name for SeaORM's `Column` variants:
/// `workspace_id` -> `WorkspaceId` (each underscore-separated word capitalized).
fn col_pascal(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    for word in snake.split('_') {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// db-mode model.rs: one SeaORM entity module per entity. Plain serde structs
/// (`model_rs`) are memory-mode only; db mode emits `DeriveEntityModel` over the
/// jerrycan facade — generated apps carry NO direct sea-orm dep, so each module
/// aliases `use jerrycan::db::sea_orm;` (the derive macros emit bare `sea_orm::`
/// paths; see docs/ai/08-database.md). `design` resolves fk target key types and
/// whether a belongs_to target lives in the same module (intra-module relation).
/// `auth` (the GenMode flag) gates the server-owned-FK request DTOs (issue #34):
/// an identity-FK entity used as a GUARDED request body also gets a
/// `{Entity}Request` struct without `user_id`.
pub(crate) fn model_rs_db(m: &ModuleDesign, design: &Design, auth: bool) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let local: std::collections::HashSet<&str> =
        m.entities.iter().map(|e| e.name.as_str()).collect();
    let mut out = String::from(
        "//! Entities and DTOs for this module (db mode: SeaORM entities).\n//! Agent-owned: edit freely.\n\n",
    );
    for e in &m.entities {
        let snake = Design::to_snake(&e.name);
        let table = design.table_name(&e.name);
        let key = key_rust_type(e);
        // The synthetic pk surfaces as a visible `id` field so POST bodies may
        // omit it (`#[serde(default)]`); a declared id has no default.
        let id_default = if declared_id(e).is_some() {
            ""
        } else {
            "        #[serde(default)]\n"
        };

        let mut fields = String::new();
        // fk columns, in belongs_to order, before declared fields.
        for b in &e.belongs_to {
            let col = Design::fk_column(&b.entity);
            let ty = design.target_key_rust_type(&b.entity);
            if b.on_delete == OnDelete::SetNull {
                fields.push_str("        #[serde(default)]\n");
                fields.push_str(&format!("        pub {col}: Option<{ty}>,\n"));
            } else {
                fields.push_str(&format!("        pub {col}: {ty},\n"));
            }
        }
        // declared fields (the declared id is the pk, emitted above; skip it).
        for f in e.fields.iter().filter(|f| f.name != "id") {
            let base = match f.field_type {
                FieldType::Json => "Json",
                FieldType::Boolean => "bool",
                _ => f.field_type.rust_type(),
            };
            // A keyword field is a raw identifier (`type` → `r#type`); the serde
            // rename + sea_orm column_name keep the wire and SQL names as `type`.
            fields.push_str(&keyword_field_attrs(&f.name, "        ", true));
            let ident = rust_ident(&f.name);
            if f.required {
                fields.push_str(&enum_validate_attr(e, f, "        ", "super::"));
                fields.push_str(&format!("        pub {ident}: {base},\n"));
            } else {
                fields.push_str("        #[serde(default)]\n");
                fields.push_str(&enum_validate_attr(e, f, "        ", "super::"));
                fields.push_str(&format!("        pub {ident}: Option<{base}>,\n"));
            }
        }

        // Intra-module belongs_to → Relation arm + Related impl; cross-module
        // targets stay decoupled (fk field only, no relation).
        let mut relation_arms = String::new();
        let mut related_impls = String::new();
        for b in &e.belongs_to {
            if !local.contains(b.entity.as_str()) {
                continue;
            }
            let target_snake = Design::to_snake(&b.entity);
            let fk_pascal = col_pascal(&Design::fk_column(&b.entity));
            let target_pascal = &b.entity;
            relation_arms.push_str(&format!(
                "        #[sea_orm(belongs_to = \"super::{target_snake}::Entity\", from = \"Column::{fk_pascal}\", to = \"super::{target_snake}::Column::Id\")]\n        {target_pascal},\n"
            ));
            related_impls.push_str(&format!(
                "\n    impl Related<super::{target_snake}::Entity> for Entity {{\n        fn to() -> RelationDef {{\n            Relation::{target_pascal}.def()\n        }}\n    }}\n"
            ));
        }
        // Empty enum on one line (matches docs/ai/08-database.md); arms get a body.
        let relation = if relation_arms.is_empty() {
            "    pub enum Relation {}\n".to_string()
        } else {
            format!("    pub enum Relation {{\n{relation_arms}    }}\n")
        };

        out.push_str(&format!(
            r#"pub mod {snake} {{
    use jerrycan::db::sea_orm;
    use jerrycan::db::sea_orm::entity::prelude::*;
    use serde::{{Deserialize, Serialize}};

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "{table}")]
    pub struct Model {{
        #[sea_orm(primary_key)]
{id_default}        pub id: {key},
{fields}    }}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
{relation}{related_impls}
    impl ActiveModelBehavior for ActiveModel {{}}
}}
pub use {snake}::Model as {entity};

"#,
            entity = e.name,
        ));
        // Request DTO (issues #34 + #53): an entity taken as a request body whose
        // wire shape drops a server-owned field (identity fk, a `default` field,
        // or a path-redundant parent fk) gets the trimmed `{Entity}Request`,
        // emitted right after its entity block.
        let needs_dto = m.endpoints.iter().any(|ep| {
            design.endpoint_uses_request_dto(m, ep, auth)
                && ep
                    .request_body
                    .as_ref()
                    .is_some_and(|rb| rb.entity == e.name)
        });
        if needs_dto {
            out.push_str(&request_dto_rs(e, design));
        }
    }
    out.push_str(&enum_deserialize_fns(&m.entities));
    Some(out)
}

/// The `{Entity}Request` DTO (issues #34 + #53): the entity's deserialization
/// shape MINUS every field the wire contract drops — the server-owned identity
/// `user_id` fk (#34), any `default` field (#53a), and a path-redundant parent
/// fk (#53b). The client never sends these; the server supplies each value.
/// Everything else mirrors the Model: the pk `id` (synthetic → `#[serde(default)]`),
/// the remaining fk columns (SetNull → `Option` + default), then the declared
/// fields with the same optionality and keyword renames. Plain serde struct —
/// only the Model touches SeaORM.
fn request_dto_rs(e: &Entity, design: &Design) -> String {
    let entity = &e.name;
    let key = key_rust_type(e);
    let id_default = if declared_id(e).is_some() {
        ""
    } else {
        "    #[serde(default)]\n"
    };
    // Identity fk is dropped only under auth (#34 injects the session user's id);
    // a path-redundant parent fk (#53b) is dropped regardless of auth.
    let omit_identity = design.wants_auth();
    let path_fks = design.entity_path_fk_columns(entity);
    let mut fields = String::new();
    for b in e.belongs_to.iter().filter(|b| {
        !(omit_identity && Design::is_identity_fk(b))
            && !path_fks.contains(&Design::fk_column(&b.entity))
    }) {
        let col = Design::fk_column(&b.entity);
        let ty = design.target_key_rust_type(&b.entity);
        if b.on_delete == OnDelete::SetNull {
            fields.push_str("    #[serde(default)]\n");
            fields.push_str(&format!("    pub {col}: Option<{ty}>,\n"));
        } else {
            fields.push_str(&format!("    pub {col}: {ty},\n"));
        }
    }
    // A `default` field (#53a) is server-owned: it does not appear on the wire.
    for f in e
        .fields
        .iter()
        .filter(|f| f.name != "id" && f.default.is_none())
    {
        let base = f.field_type.rust_type();
        fields.push_str(&keyword_field_attrs(&f.name, "    ", false));
        let ident = rust_ident(&f.name);
        if f.required {
            fields.push_str(&enum_validate_attr(e, f, "    ", ""));
            fields.push_str(&format!("    pub {ident}: {base},\n"));
        } else {
            fields.push_str("    #[serde(default)]\n");
            fields.push_str(&enum_validate_attr(e, f, "    ", ""));
            fields.push_str(&format!("    pub {ident}: Option<{base}>,\n"));
        }
    }
    let doc = request_dto_doc(e, &path_fks, omit_identity);
    format!(
        "{doc}#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\npub struct {entity}Request {{\n{id_default}    pub id: {key},\n{fields}}}\n\n"
    )
}

/// The doc comment for a `{Entity}Request` DTO — one `///` line per dropped
/// server-owned field with its reason, so an agent reading the struct sees
/// exactly which keys the server supplies. Ordered identity fk → path fk →
/// defaults (the struct-field order the DTO omits them in).
fn request_dto_doc(e: &Entity, path_fks: &[String], omit_identity: bool) -> String {
    let mut reasons = Vec::new();
    for b in &e.belongs_to {
        let col = Design::fk_column(&b.entity);
        if omit_identity && Design::is_identity_fk(b) {
            reasons.push(format!("`{col}` (the authenticated session user's id)"));
        } else if path_fks.contains(&col) {
            reasons.push(format!("`{col}` (from the request path)"));
        }
    }
    for f in e.fields.iter().filter(|f| f.default.is_some()) {
        let v = f.default.as_ref().expect("filtered to Some");
        reasons.push(format!(
            "`{}` (server default {})",
            f.name,
            serde_json::to_string(v).unwrap_or_else(|_| "…".into())
        ));
    }
    format!(
        "/// Request body for `{}` — the wire input shape. These SERVER-OWNED fields\n/// are omitted (the client never sends them; the server supplies each):\n///   {}.\n",
        e.name,
        reasons.join(", ")
    )
}

fn memory_repo_rs(m: &ModuleDesign) -> String {
    let mut out = String::from(
        "//! In-memory data access (Phase 1; jerrycan-db replaces this in Phase 2).\nuse super::model::*;\nuse std::collections::BTreeMap;\nuse std::sync::Mutex;\nuse std::sync::atomic::{AtomicI64, Ordering};\n\n",
    );
    for e in &m.entities {
        let n = &e.name;
        out.push_str(&format!(
            r#"// Stub handlers don't call the repo yet; remove this allow as you implement them.
#[allow(dead_code)]
pub struct {n}Repo {{
    items: Mutex<BTreeMap<i64, {n}>>,
    next_id: AtomicI64,
}}

#[allow(dead_code)]
impl {n}Repo {{
    pub fn new() -> Self {{
        Self {{ items: Mutex::new(BTreeMap::new()), next_id: AtomicI64::new(1) }}
    }}
    pub fn all(&self) -> Vec<{n}> {{
        self.items.lock().unwrap().values().cloned().collect()
    }}
    pub fn get(&self, id: i64) -> Option<{n}> {{
        self.items.lock().unwrap().get(&id).cloned()
    }}
    pub fn insert(&self, item: {n}) -> i64 {{
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.items.lock().unwrap().insert(id, item);
        id
    }}
    pub fn remove(&self, id: i64) -> bool {{
        self.items.lock().unwrap().remove(&id).is_some()
    }}
    pub fn update(&self, id: i64, item: {n}) -> bool {{
        match self.items.lock().unwrap().get_mut(&id) {{
            Some(slot) => {{
                *slot = item;
                true
            }}
            None => false,
        }}
    }}
}}

impl Default for {n}Repo {{
    fn default() -> Self {{
        Self::new()
    }}
}}

"#
        ));
    }
    out
}

/// The Model field names in struct order — the order `model_rs_db` emits and
/// the order an ActiveModel literal must set them: pk `id`, then fk columns (in
/// belongs_to order), then the declared non-id fields. Used to build the
/// `field: Set(item.field)` lists so they stay in lockstep with the model.
fn model_field_names(e: &Entity) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    for b in &e.belongs_to {
        names.push(Design::fk_column(&b.entity));
    }
    for f in e.fields.iter().filter(|f| f.name != "id") {
        names.push(f.name.clone());
    }
    names
}

/// ActiveModel field assignments, one per line. `item` is consumed, so values
/// move in (no clones). A synthetic pk (no declared `id`) is `NotSet` so the
/// DB assigns the autoincrement id; a declared pk is `Set` from the item.
/// `with_id == false` omits the id line (the update path sets it explicitly).
fn active_sets(e: &Entity, with_id: bool) -> String {
    let indent = "            ";
    let mut out = String::new();
    for name in model_field_names(e) {
        if name == "id" {
            if !with_id {
                continue;
            }
            if declared_id(e).is_some() {
                out.push_str(&format!("{indent}id: Set(item.id),\n"));
            } else {
                out.push_str(&format!("{indent}id: sea_orm::ActiveValue::NotSet,\n"));
            }
        } else {
            // A keyword field is a raw identifier on both the ActiveModel field
            // and the moved-in `item` value (`r#type: Set(item.r#type)`).
            let ident = rust_ident(&name);
            out.push_str(&format!("{indent}{ident}: Set(item.{ident}),\n"));
        }
    }
    out
}

/// Tenant-scoped accessors for an entity that belongs_to the design's tenancy
/// entity (empty otherwise). Keyed on the fk column so a tenant can only reach
/// its own rows: `all_for` filters the fk, `get_for` adds the id, `remove_for`
/// deletes on both. Param name = fk column; param type = the tenant pk type.
fn scoped_methods(e: &Entity, design: &Design) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    if !e.belongs_to.iter().any(|b| b.entity == tenancy.entity) {
        return String::new();
    }
    let entity = &e.name;
    let snake = Design::to_snake(entity);
    let fk_col = Design::fk_column(&tenancy.entity);
    let fk_pascal = col_pascal(&fk_col);
    let fk_ty = design.target_key_rust_type(&tenancy.entity);
    let key = key_rust_type(e);
    // The pk is `Set` from `item.id` (the synthetic pk also surfaces as a visible
    // `id` field), the rest from `item` (active_sets with the id line omitted) —
    // correct for both declared and synthetic primary keys, and `id` (the path
    // param) is consumed once by the ownership check, so no clone for text pks.
    let update_sets = active_sets(e, false);
    format!(
        r#"
    // Tenant-scoped accessors — handlers must use these for tenant-owned data (JL0006).
    pub async fn all_for(&self, {fk_col}: {fk_ty}) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .order_by_asc({snake}::Column::Id)
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}

    pub async fn get_for(&self, {fk_col}: {fk_ty}, id: {key}) -> Result<Option<{entity}>> {{
        {snake}::Entity::find_by_id(id)
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .one(self.db.conn())
            .await
            .map_err(db_error)
    }}

    pub async fn remove_for(&self, {fk_col}: {fk_ty}, id: {key}) -> Result<bool> {{
        let r = {snake}::Entity::delete_many()
            .filter({snake}::Column::Id.eq(id))
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .exec(self.db.conn())
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected > 0)
    }}

    pub async fn update_for(&self, {fk_col}: {fk_ty}, id: {key}, item: {entity}) -> Result<bool> {{
        // Scope the write to the tenant: only proceed if the row is already
        // theirs (a foreign or unknown id is a no-op, returning false → 404).
        if {snake}::Entity::find_by_id(id)
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .one(self.db.conn())
            .await
            .map_err(db_error)?
            .is_none()
        {{
            return Ok(false);
        }}
        let m = {snake}::ActiveModel {{
            id: Set(item.id),
{update_sets}        }};
        match m.update(self.db.conn()).await {{
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(e) => Err(db_error(e)),
        }}
    }}
"#
    )
}

fn sql_repo(e: &Entity, design: &Design) -> String {
    let entity = &e.name;
    let snake = Design::to_snake(entity);
    let key = key_rust_type(e);
    let insert_sets = active_sets(e, true);
    let update_sets = active_sets(e, false);
    let scoped = scoped_methods(e, design);
    // The insert differs by pk type. An auto-increment integer pk is assigned by
    // the DB, so `ActiveModel::insert` returns the persisted row (with its id).
    // A client-supplied text pk (string/uuid) is already known, and
    // `ActiveModel::insert`'s post-insert refetch fails for a text pk on sqlite
    // ("Failed to find inserted item" — it refetches by rowid, not the text id),
    // so run the INSERT via `Entity::insert(..).exec(..)` and return the known id.
    let insert_body = if key == "String" {
        format!(
            "    pub async fn insert(&self, item: {entity}) -> Result<{key}> {{\n\
             \x20       let id = item.id.clone();\n\
             \x20       {snake}::Entity::insert({snake}::ActiveModel {{\n\
             {insert_sets}        }})\n\
             \x20       .exec(self.db.conn())\n\
             \x20       .await\n\
             \x20       .map_err(db_error)?;\n\
             \x20       Ok(id)\n\
             \x20   }}"
        )
    } else {
        format!(
            "    pub async fn insert(&self, item: {entity}) -> Result<{key}> {{\n\
             \x20       let row = {snake}::ActiveModel {{\n\
             {insert_sets}        }}\n\
             \x20       .insert(self.db.conn())\n\
             \x20       .await\n\
             \x20       .map_err(db_error)?;\n\
             \x20       Ok(row.id)\n\
             \x20   }}"
        )
    };
    format!(
        r#"pub struct {entity}Repo {{
    db: Db,
}}

/// DI factory — registered by the tool-owned lib.rs via `.provide_dep`.
pub(crate) async fn {snake}_repo(db: Dep<Db>) -> Result<{entity}Repo> {{
    Ok({entity}Repo {{ db: (*db).clone() }})
}}

// Stub handlers don't call the repo yet; remove this allow as you implement them.
#[allow(dead_code)]
impl {entity}Repo {{
    pub async fn all(&self) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .order_by_asc({snake}::Column::Id)
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}

    pub async fn get(&self, id: {key}) -> Result<Option<{entity}>> {{
        {snake}::Entity::find_by_id(id)
            .one(self.db.conn())
            .await
            .map_err(db_error)
    }}

{insert_body}

    pub async fn remove(&self, id: {key}) -> Result<bool> {{
        let r = {snake}::Entity::delete_by_id(id)
            .exec(self.db.conn())
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected > 0)
    }}

    pub async fn update(&self, id: {key}, item: {entity}) -> Result<bool> {{
        let m = {snake}::ActiveModel {{
            id: Set(id),
{update_sets}        }};
        match m.update(self.db.conn()).await {{
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(e) => Err(db_error(e)),
        }}
    }}
{scoped}}}

"#,
    )
}

pub(crate) fn repo_rs(m: &ModuleDesign, mode: GenMode, design: &Design) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    if !mode.db {
        return Some(memory_repo_rs(m));
    }
    // `ColumnTrait`/`QueryFilter` back the `.filter(Column::Fk.eq(..))` calls in
    // the tenant-scoped accessors only; a module with no tenant-owned entity must
    // not import them or it trips `-D warnings` on otherwise-untouched generated
    // code. Everything else (find/insert/update + order_by) is always used.
    let has_scoped = m
        .entities
        .iter()
        .any(|e| !scoped_methods(e, design).is_empty());
    let filter_imports = if has_scoped {
        "ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder"
    } else {
        "ActiveModelTrait, ActiveValue::Set, EntityTrait, QueryOrder"
    };
    // The `use jerrycan::db::sea_orm;` alias resolves the bare `sea_orm::` paths
    // the repo writes (DbErr, ActiveValue::NotSet); the trait imports come through
    // the same facade so generated crates carry NO direct sea-orm dependency.
    let mut out = format!(
        "//! Data access — SeaORM over jerrycan::db (agent-owned; edit freely).\nuse jerrycan::db::sea_orm;\nuse jerrycan::db::sea_orm::{{{filter_imports}}};\nuse jerrycan::db::{{db_error, Db}};\nuse jerrycan::prelude::*;\n\nuse super::model::*;\n\n",
    );
    for e in &m.entities {
        out.push_str(&sql_repo(e, design));
    }
    Some(out)
}

/// The SchemaBuilder matching a dialect — renders both `CREATE TABLE` and the
/// standalone `CREATE INDEX` statements consistently.
fn schema_sql<S: sea_query::SchemaStatementBuilder>(stmt: &S, backend_is_pg: bool) -> String {
    use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
    if backend_is_pg {
        stmt.build(PostgresQueryBuilder)
    } else {
        stmt.build(SqliteQueryBuilder)
    }
}

/// Map a FieldType onto a native column type. Booleans are native BOOLEAN now
/// (the Model field is `bool` under SeaORM, which round-trips it on both
/// backends); JSON is `jsonb` on Postgres (sea-orm's Json maps to sqlx JSON,
/// which Postgres expects in a json/jsonb column) and TEXT on SQLite.
fn ddl_typed(
    c: &mut sea_query::ColumnDef,
    t: FieldType,
    backend_is_pg: bool,
) -> &mut sea_query::ColumnDef {
    match t {
        FieldType::String | FieldType::Datetime | FieldType::Uuid => c.text(),
        FieldType::Integer => c.big_integer(),
        FieldType::Boolean => c.boolean(),
        FieldType::Float => c.double(),
        FieldType::Json => {
            if backend_is_pg {
                c.json_binary()
            } else {
                c.text()
            }
        }
    }
}

/// Dual-dialect `CREATE TABLE` DDL for one module's entities (None if it has
/// none), rendered by sea-query so dialect differences (autoincrement vs
/// bigserial, quoting) are library-owned, never hand-rolled strings. `design`
/// resolves fk target key types/tables and the tenancy membership table.
fn migration_ddl(m: &ModuleDesign, backend_is_pg: bool, design: &Design) -> Option<String> {
    use sea_query::{Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, Table};
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::new();
    // Standalone `CREATE INDEX` statements appended after their table (sea-query
    // emits indexes separately from the table, on both dialects).
    let mut indexes = String::new();
    // SQL comments documenting cross-module relations the migration no longer
    // enforces (appended right after the table, ahead of its indexes).
    let mut comments = String::new();
    // A belongs_to target in THIS module is a real, enforceable FK (the parent
    // table is created by the same migration). A cross-module target becomes an
    // unenforced relation column — the SAME intra-module predicate `model_rs_db`
    // uses to decide which relations to wire (F2: per-module gen-tests migrate only
    // one module, so a real FK to another module's table 500s with "no such table").
    let local: std::collections::HashSet<&str> =
        m.entities.iter().map(|e| e.name.as_str()).collect();
    for e in &m.entities {
        let tbl = design.table_name(&e.name);
        let mut table = Table::create();
        table.table(Alias::new(tbl.clone()));
        // A declared `id` field IS the pk (typed as declared); only entities
        // without one get the synthetic autoincrement pk. Emitting both would
        // be a duplicate-column error.
        let mut pk = ColumnDef::new(Alias::new("id"));
        match declared_id(e) {
            Some(t) if t != FieldType::Integer => {
                ddl_typed(&mut pk, t, backend_is_pg)
                    .not_null()
                    .primary_key();
            }
            _ => {
                pk.big_integer().not_null().auto_increment().primary_key();
            }
        }
        table.col(&mut pk);
        // fk columns, in belongs_to order, BEFORE declared fields — lockstep with
        // the Model (`model_rs_db`/`model_field_names`). NOT NULL unless SetNull
        // (the column must drop to NULL when the parent dies). An intra-module
        // target also gets a real foreign-key constraint carrying the declared
        // on_delete policy; a cross-module target stays an unenforced relation:
        // bare column + an index + a documenting comment (no FK constraint).
        for b in &e.belongs_to {
            let col = Design::fk_column(&b.entity);
            let target_table = design.table_name(&b.entity);
            let mut fk_col = ColumnDef::new(Alias::new(col.clone()));
            match design.target_key_rust_type(&b.entity) {
                "String" => ddl_typed(&mut fk_col, FieldType::String, backend_is_pg),
                _ => fk_col.big_integer(),
            };
            if b.on_delete != OnDelete::SetNull {
                fk_col.not_null();
            }
            table.col(&mut fk_col);
            if local.contains(b.entity.as_str()) {
                let action = match b.on_delete {
                    OnDelete::Cascade => ForeignKeyAction::Cascade,
                    OnDelete::SetNull => ForeignKeyAction::SetNull,
                    OnDelete::Restrict => ForeignKeyAction::Restrict,
                };
                table.foreign_key(
                    ForeignKey::create()
                        .name(format!("fk_{tbl}_{col}"))
                        .from(Alias::new(tbl.clone()), Alias::new(col.clone()))
                        .to(Alias::new(target_table), Alias::new("id"))
                        .on_delete(action),
                );
            } else {
                // Cross-module: no DB constraint, but an unconstrained fk should
                // still be indexed for lookups/joins. Dedupe with a declared
                // `index: true` field of the same name (none today, but cheap to
                // guard) so we never emit two identically-named indexes.
                let idx_name = format!("idx_{tbl}_{col}");
                if e.fields.iter().any(|f| f.name == col && f.index) {
                    // already produced by the declared-field index pass
                } else {
                    let mut idx = Index::create();
                    idx.name(idx_name)
                        .table(Alias::new(tbl.clone()))
                        .col(Alias::new(col.clone()));
                    indexes.push_str(&schema_sql(&idx, backend_is_pg));
                    indexes.push_str(";\n\n");
                }
                comments.push_str(&format!(
                    "-- {col}: references {target_table}.id (cross-module; enforced by handlers, see schema.json)\n"
                ));
            }
        }
        for f in e.fields.iter().filter(|f| f.name != "id") {
            let mut col = ColumnDef::new(Alias::new(f.name.as_str()));
            ddl_typed(&mut col, f.field_type, backend_is_pg);
            // A `required: false` field backs an `Option<T>` Model field, so its
            // column is NULLABLE (sea-query's default) — inserting `None` binds
            // NULL, which a NOT NULL column would reject. Required fields are
            // NOT NULL with no default; the old zero-DEFAULTs only existed to let
            // NOT NULL and optional coexist, a contradiction now removed.
            if f.required {
                col.not_null();
            }
            if f.unique {
                col.unique_key();
            }
            // Enum `values` constrain the column to that set via a CHECK.
            if let Some(values) = &f.values {
                col.check(Expr::col(Alias::new(f.name.as_str())).is_in(values.clone()));
            }
            table.col(&mut col);
            // An indexed field gets a standalone CREATE INDEX after the table.
            if f.index {
                let idx_name = format!("idx_{tbl}_{name}", name = f.name);
                let mut idx = Index::create();
                idx.name(idx_name)
                    .table(Alias::new(tbl.clone()))
                    .col(Alias::new(f.name.as_str()));
                indexes.push_str(&schema_sql(&idx, backend_is_pg));
                indexes.push_str(";\n\n");
            }
        }
        out.push_str(&schema_sql(&table, backend_is_pg));
        out.push_str(";\n\n");
        out.push_str(&comments);
        comments.clear();
        out.push_str(&indexes);
        indexes.clear();
    }
    // The tenant module also owns the membership table: who belongs to the tenant
    // and in what role. Emitted right after the tenant's own table so the fk
    // resolves. UNIQUE(user_id, fk) keeps a user from joining the same tenant
    // twice — rendered as a standalone CREATE UNIQUE INDEX (cleanest on both).
    if let Some(tenancy) = &design.tenancy
        && m.entities.iter().any(|e| e.name == tenancy.entity)
    {
        let tenant_table = design.table_name(&tenancy.entity);
        let members = format!("{}_members", Design::to_snake(&tenancy.entity));
        let fk = Design::fk_column(&tenancy.entity);
        let mut table = Table::create();
        table.table(Alias::new(members.clone()));
        let mut pk = ColumnDef::new(Alias::new("id"));
        pk.big_integer().not_null().auto_increment().primary_key();
        table.col(&mut pk);
        // user_id is TEXT (the stringified user pk), matching SessionUser.id and
        // storage_objects.owner_id: one shape holds an integer OR a uuid user id,
        // so a migrated Supabase app whose auth.users are uuid can seed membership.
        table.col(ColumnDef::new(Alias::new("user_id")).text().not_null());
        let mut fk_col = ColumnDef::new(Alias::new(fk.clone()));
        match design.target_key_rust_type(&tenancy.entity) {
            "String" => ddl_typed(&mut fk_col, FieldType::String, backend_is_pg),
            _ => fk_col.big_integer(),
        };
        fk_col.not_null();
        table.col(&mut fk_col);
        table.col(ColumnDef::new(Alias::new("role")).text().not_null());
        table.foreign_key(
            ForeignKey::create()
                .name(format!("fk_{members}_{fk}"))
                .from(Alias::new(members.clone()), Alias::new(fk.clone()))
                .to(Alias::new(tenant_table), Alias::new("id"))
                .on_delete(ForeignKeyAction::Cascade),
        );
        out.push_str(&schema_sql(&table, backend_is_pg));
        out.push_str(";\n\n");
        let mut uniq = Index::create();
        uniq.unique()
            .name(format!("idx_{members}_user_tenant"))
            .table(Alias::new(members.clone()))
            .col(Alias::new("user_id"))
            .col(Alias::new(fk.clone()));
        out.push_str(&schema_sql(&uniq, backend_is_pg));
        out.push_str(";\n\n");
    }
    Some(out)
}

/// Emit the module's own migration file plus one file per entity-bearing
/// subroute (`0001_create_tables_{sub}.sql`), both dialects, recursing.
/// Migrations are agent-owned create-once — never clobbered once applied.
fn write_module_migrations(
    crate_dir: &Path,
    m: &ModuleDesign,
    created: &mut Vec<String>,
    root: &Path,
    design: &Design,
) -> Result<(), String> {
    if let Some(ddl) = migration_ddl(m, false, design) {
        write_agent_owned(
            &crate_dir.join("migrations/sqlite/0001_create_tables.sql"),
            &ddl,
            created,
            root,
        )?;
    }
    if let Some(ddl) = migration_ddl(m, true, design) {
        write_agent_owned(
            &crate_dir.join("migrations/postgres/0001_create_tables.sql"),
            &ddl,
            created,
            root,
        )?;
    }
    write_subtree_migrations(crate_dir, m, created, root, design)
}

/// Subroute migrations land in the OWNING (top) crate's migrations dir, named
/// `0001_create_tables_{sub_snake}.sql`, recursing to arbitrary depth.
fn write_subtree_migrations(
    crate_dir: &Path,
    m: &ModuleDesign,
    created: &mut Vec<String>,
    root: &Path,
    design: &Design,
) -> Result<(), String> {
    for sub in &m.subroutes {
        let sub_snake = sub.name.replace('-', "_");
        if let Some(ddl) = migration_ddl(sub, false, design) {
            write_agent_owned(
                &crate_dir.join(format!(
                    "migrations/sqlite/0001_create_tables_{sub_snake}.sql"
                )),
                &ddl,
                created,
                root,
            )?;
        }
        if let Some(ddl) = migration_ddl(sub, true, design) {
            write_agent_owned(
                &crate_dir.join(format!(
                    "migrations/postgres/0001_create_tables_{sub_snake}.sql"
                )),
                &ddl,
                created,
                root,
            )?;
        }
        write_subtree_migrations(crate_dir, sub, created, root, design)?;
    }
    Ok(())
}

pub(crate) fn deps_rs(m: &ModuleDesign) -> String {
    let mut out = String::from(
        "//! Agent-owned: module-scoped dependencies and middleware.\nuse jerrycan::prelude::*;\n\n/// Called by the tool-owned lib.rs — register module deps/middleware here;\n/// regeneration never touches this file.\npub(crate) fn configure(module: Module) -> Module {\n",
    );
    for dep in &m.dependencies {
        out.push_str(&format!(
            "    // declared dependency `{dep}`: define a type and .provide/.provide_dep it here\n"
        ));
    }
    out.push_str("    module\n}\n");
    out
}

/// Route-table lines: endpoints grouped by path (first-seen order), first
/// method via the free fn, the rest chained.
fn route_lines(m: &ModuleDesign, indent: &str) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut by_path: std::collections::HashMap<&str, Vec<&Endpoint>> =
        std::collections::HashMap::new();
    for ep in &m.endpoints {
        if !by_path.contains_key(ep.path.as_str()) {
            order.push(&ep.path);
        }
        by_path.entry(&ep.path).or_default().push(ep);
    }
    let mut out = String::new();
    for path in order {
        let eps = &by_path[path];
        let mut chain = format!(
            "{}(handlers::{})",
            eps[0].method.builder_fn(),
            eps[0].operation_id
        );
        for ep in &eps[1..] {
            chain.push_str(&format!(
                ".{}(handlers::{})",
                ep.method.builder_fn(),
                ep.operation_id
            ));
        }
        out.push_str(&format!("{indent}.route(\"{path}\", {chain})\n"));
    }
    out
}

fn module_body(m: &ModuleDesign, indent: &str, mode: GenMode) -> String {
    let mut body = format!("{indent}Module::new(\"{}\")\n", m.name);
    for e in &m.entities {
        if mode.db {
            body.push_str(&format!(
                "{indent}    .provide_dep(repo::{}_repo)\n",
                Design::to_snake(&e.name)
            ));
        } else {
            body.push_str(&format!(
                "{indent}    .provide(repo::{}Repo::new())\n",
                e.name
            ));
        }
    }
    body.push_str(&route_lines(m, &format!("{indent}    ")));
    for sub in &m.subroutes {
        body.push_str(&format!(
            "{indent}    .mount(\"{}\", subroutes::{}::module())\n",
            sub.effective_mount(),
            sub.name.replace('-', "_"),
        ));
    }
    body
}

fn mod_decls(m: &ModuleDesign) -> String {
    let mut out = String::from("mod deps;\nmod handlers;\n");
    if !m.entities.is_empty() {
        out.push_str("mod model;\nmod repo;\n");
    }
    if !m.subroutes.is_empty() {
        out.push_str("mod subroutes;\n");
    }
    out
}

pub(crate) fn lib_rs(m: &ModuleDesign, mode: GenMode) -> String {
    format!(
        "//! Route module `{name}` — TOOL-OWNED, regenerated by `jerrycan generate`.\n//! The sole public item is `module()`; agent code lives in handlers/model/repo/deps.\n#![forbid(unsafe_code)]\n\n{mods}\nuse jerrycan::prelude::*;\n\n/// Build this module's routes, subroutes, and scoped dependencies.\npub fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        ", mode),
    )
}

fn subroute_mod_rs(m: &ModuleDesign, mode: GenMode) -> String {
    format!(
        "//! Subroute `{name}` — TOOL-OWNED mod.rs; same fractal shape as a module.\n\n{mods}\nuse jerrycan::prelude::*;\n\npub(crate) fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        ", mode),
    )
}

/// Per-file report of the salient AGENT declarations a tool-owned rewrite dropped
/// (issue #69a): `(relative_path, dropped_lines)` entries. Threaded through the
/// write helpers and returned by `write_module_reporting`.
type DroppedDecls = Vec<(String, Vec<String>)>;

/// A "salient" top-level item declaration for #69a drift detection: a `mod` or
/// `use` line — the cross-module wiring an agent hand-adds to a tool-owned
/// lib.rs/mod.rs (e.g. `mod cross_sweep;`). Trimmed before matching; a regenerated
/// file that no longer contains this exact line is DROPPING that line.
fn is_salient_decl(trimmed: &str) -> bool {
    trimmed.starts_with("mod ")
        || trimmed.starts_with("pub mod ")
        || trimmed.starts_with("pub(crate) mod ")
        || trimmed.starts_with("pub(super) mod ")
        || trimmed.starts_with("use ")
}

/// Does the generator ITSELF emit this decl for some design? The fixed tool-owned
/// `mod`/`use` lines (`mod deps;` … `use jerrycan::prelude::*;`) plus the
/// `pub(crate) mod <sub>;` shape of the subroutes decls file. A dropped salient
/// line that is NOT one of these is agent-authored wiring — so a design-driven
/// change (an entity or subroute removed from design.json) never masquerades as
/// lost agent work.
fn is_tool_decl(trimmed: &str) -> bool {
    matches!(
        trimmed,
        "mod deps;" | "mod handlers;" | "mod model;" | "mod repo;" | "mod subroutes;"
    ) || trimmed == "use jerrycan::prelude::*;"
        || trimmed
            .strip_prefix("pub(crate) mod ")
            .and_then(|r| r.strip_suffix(';'))
            .is_some_and(|id| {
                !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
}

/// Salient AGENT declarations (`mod`/`use`) present in `old` but absent from the
/// fresh `new` emission — the lines a tool-owned rewrite would DROP (issue #69a).
/// Tool-emitted decls are excluded so only genuine agent wiring is reported;
/// deduped, source order preserved.
fn dropped_agent_decls(old: &str, new: &str) -> Vec<String> {
    let kept: std::collections::HashSet<&str> = new
        .lines()
        .map(str::trim)
        .filter(|l| is_salient_decl(l))
        .collect();
    let mut seen = std::collections::HashSet::new();
    old.lines()
        .map(str::trim)
        .filter(|l| is_salient_decl(l) && !is_tool_decl(l) && !kept.contains(l) && seen.insert(*l))
        .map(str::to_string)
        .collect()
}

fn write_tool_owned(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
    root: &Path,
    dropped: &mut DroppedDecls,
) -> Result<(), String> {
    // #69a: before overwriting a tool-owned file, detect agent-authored `mod`/`use`
    // wiring the fresh emission would drop — regeneration must WARN loudly, never
    // silently lose an agent's cross-module wiring.
    if let Ok(old) = fs::read_to_string(path) {
        let lost = dropped_agent_decls(&old, content);
        if !lost.is_empty() {
            dropped.push((rel(path, root), lost));
        }
    }
    fs::create_dir_all(path.parent().expect("file path has parent")).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    created.push(rel(path, root));
    Ok(())
}

fn write_agent_owned(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    if path.exists() {
        return Ok(()); // never clobber agent work
    }
    // The file is absent, so `write_tool_owned` reads no prior content and can
    // drop nothing — a local throwaway drop-sink keeps the signature clean.
    write_tool_owned(path, content, created, root, &mut Vec::new())
}

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Write (or refresh) one top-level route crate under `routes_dir`
/// (= `<app>/crates/routes`). Returns paths written, relative to routes_dir's parent's parent.
/// Precondition: the design has passed `questions::validate` — generation assumes
/// validated names and entity references.
pub fn write_module(
    routes_dir: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    design: &Design,
) -> Result<Vec<String>, String> {
    write_module_reporting(routes_dir, m, mode, design).map(|(created, _dropped)| created)
}

/// Like `write_module`, but also returns, per tool-owned file, the salient
/// AGENT declarations (`mod`/`use` lines) the regeneration DROPPED — present on
/// disk, absent from the fresh emission (issue #69a): `(relative_path, lines)`.
/// The generator NEVER silently drops them; the CLI surfaces this list loudly
/// (stderr + the `--json` envelope) so an agent's hand-added cross-module wiring
/// can't vanish unnoticed. Empty in the common case (fresh scaffold, or an
/// unedited regeneration), so a no-drift run reports nothing.
pub fn write_module_reporting(
    routes_dir: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    design: &Design,
) -> Result<(Vec<String>, DroppedDecls), String> {
    let root = routes_dir
        .ancestors()
        .nth(2)
        .unwrap_or(routes_dir)
        .to_path_buf();
    let crate_dir = routes_dir.join(&m.name);
    let src = crate_dir.join("src");
    let mut created = Vec::new();
    let mut dropped = Vec::new();

    let cargo = render(ROUTE_CARGO, &[("name", &m.name)])?;
    write_tool_owned(
        &crate_dir.join("Cargo.toml"),
        &cargo,
        &mut created,
        &root,
        &mut dropped,
    )?;
    write_tool_owned(
        &src.join("lib.rs"),
        &lib_rs(m, mode),
        &mut created,
        &root,
        &mut dropped,
    )?;
    write_unit_files(&src, m, mode, design, &mut created, &root)?;
    write_subroutes(&src, m, mode, design, &mut created, &root, &mut dropped)?;
    // db mode: agent-owned create-once migrations for this crate (module + subroutes).
    if mode.db {
        write_module_migrations(&crate_dir, m, &mut created, &root, design)?;
    }
    Ok((created, dropped))
}

/// The agent-owned file set shared by modules and subroutes.
fn write_unit_files(
    dir: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    design: &Design,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    write_agent_owned(
        &dir.join("handlers.rs"),
        &handlers_rs(m, mode, design),
        created,
        root,
    )?;
    write_agent_owned(&dir.join("deps.rs"), &deps_rs(m), created, root)?;
    // db mode emits SeaORM entities; memory mode keeps plain serde structs.
    let model = if mode.db {
        model_rs_db(m, design, mode.auth)
    } else {
        model_rs(m)
    };
    if let Some(model) = model {
        write_agent_owned(&dir.join("model.rs"), &model, created, root)?;
    }
    if let Some(repo) = repo_rs(m, mode, design) {
        write_agent_owned(&dir.join("repo.rs"), &repo, created, root)?;
    }
    Ok(())
}

fn write_subroutes(
    src: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    design: &Design,
    created: &mut Vec<String>,
    root: &Path,
    dropped: &mut DroppedDecls,
) -> Result<(), String> {
    if m.subroutes.is_empty() {
        return Ok(());
    }
    let sub_root = src.join("subroutes");
    let mut decls = String::from("//! TOOL-OWNED: subroute declarations.\n");
    for sub in &m.subroutes {
        decls.push_str(&format!("pub(crate) mod {};\n", sub.name.replace('-', "_")));
    }
    write_tool_owned(&sub_root.join("mod.rs"), &decls, created, root, dropped)?;
    for sub in &m.subroutes {
        let dir = sub_root.join(sub.name.replace('-', "_"));
        write_tool_owned(
            &dir.join("mod.rs"),
            &subroute_mod_rs(sub, mode),
            created,
            root,
            dropped,
        )?;
        write_unit_files(&dir, sub, mode, design, created, root)?;
        write_subroutes(&dir, sub, mode, design, created, root, dropped)?; // arbitrary depth
    }
    Ok(())
}

/// `jerrycan generate dep <name> --module <m>`: record in design + remind in deps.rs.
pub fn add_dependency(design: &mut Design, module_path: &str, dep: &str) -> Result<(), String> {
    let m = module_by_path_mut(design, module_path)
        .ok_or_else(|| format!("module `{module_path}` not found in design.json"))?;
    if !m.dependencies.iter().any(|d| d == dep) {
        m.dependencies.push(dep.to_string());
    }
    Ok(())
}

/// Snake_case check for migration names (mirrors `questions::is_snake`, which is
/// private to that module): lowercase ASCII start, then lowercase/digit/`_`.
fn is_snake_name(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `jerrycan generate migration <name> --module <m>`: write the next numbered
/// dual-dialect migration pair (`NNNN_<name>.sql` in both sqlite/ and postgres/)
/// as agent-owned stubs, then re-run mounting to pick them up in the aggregated
/// `migrations.rs`. Returns the created files plus the rewired mounting files,
/// all relative to `root`. Numbering continues from the module's existing
/// sqlite migrations (max 4-digit prefix + 1).
pub fn generate_migration(root: &Path, module: &str, name: &str) -> Result<Vec<String>, String> {
    if !is_snake_name(name) {
        return Err(format!(
            "migration name `{name}` is not snake_case — use lowercase words joined by `_` (e.g. add_due_index)"
        ));
    }
    let crate_dir = root.join("crates/routes").join(module);
    if !crate_dir.is_dir() {
        let available = available_modules(root);
        return Err(format!(
            "module `{module}` not found under crates/routes — available: {available}"
        ));
    }
    let sqlite_dir = crate_dir.join("migrations/sqlite");
    let next = next_migration_number(&sqlite_dir);
    let stem = format!("{next:04}_{name}");
    let body = format!(
        "-- {module} {name}\n-- Write your ALTER/CREATE statements here. Both dialect files must contain\n-- the equivalent change; jerrycan check applies them to a throwaway sqlite\n-- database, so a broken migration fails fast.\n"
    );
    let mut created = Vec::new();
    write_agent_owned(
        &crate_dir.join(format!("migrations/sqlite/{stem}.sql")),
        &body,
        &mut created,
        root,
    )?;
    write_agent_owned(
        &crate_dir.join(format!("migrations/postgres/{stem}.sql")),
        &body,
        &mut created,
        root,
    )?;
    // Re-run mounting so the aggregated migrations.rs (and any other mounting
    // surface) picks up the new pair — it globs each module's sqlite dir.
    let design = Design::from_path(&root.join("design.json"))?;
    let modified = super::mounting::regenerate(root, &design)?;
    created.extend(modified);
    Ok(created)
}

/// The next 4-digit migration number for a module: scan the sqlite dir for
/// `NNNN_*.sql` stems, take the max leading 4-digit prefix, add 1 (1 if none).
fn next_migration_number(sqlite_dir: &Path) -> u32 {
    let mut max = 0u32;
    if let Ok(entries) = fs::read_dir(sqlite_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "sql") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let prefix: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = prefix.parse::<u32>() {
                    max = max.max(n);
                }
            }
        }
    }
    max + 1
}

/// Comma-joined list of route-crate directory names (for the not-found error).
fn available_modules(root: &Path) -> String {
    let routes = root.join("crates/routes");
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&routes) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

/// One row of the live route tree — the same shape the CLI and MCP both emit.
#[derive(Debug, serde::Serialize)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub module: String,
    pub handler: String,
}

/// Walk the design's module tree into a flat, mount-resolved route table.
pub fn route_map(design: &Design) -> Vec<RouteEntry> {
    fn walk(m: &ModuleDesign, prefix: &str, top: &str, out: &mut Vec<RouteEntry>) {
        let base = format!("{}{}", prefix, m.effective_mount());
        for ep in &m.endpoints {
            out.push(RouteEntry {
                method: format!("{:?}", ep.method),
                path: format!("{}{}", base.trim_end_matches('/'), ep.path),
                module: top.to_string(),
                handler: ep.operation_id.clone(),
            });
        }
        for sub in &m.subroutes {
            walk(sub, &base, top, out);
        }
    }
    let mut out = Vec::new();
    for m in &design.modules {
        walk(m, "", &m.name, &mut out);
    }
    out
}

pub fn module_by_path<'a>(design: &'a Design, path: &str) -> Option<&'a ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter().find(|s| s.name == part)?;
    }
    Some(cur)
}

pub fn module_by_path_mut<'a>(design: &'a mut Design, path: &str) -> Option<&'a mut ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter_mut().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter_mut().find(|s| s.name == part)?;
    }
    Some(cur)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::platform::design::tests::MINIMAL;

    fn demo() -> Design {
        serde_json::from_str(MINIMAL).unwrap()
    }

    fn todos() -> ModuleDesign {
        demo().modules.into_iter().next().unwrap()
    }

    /// The DDL must declare the fk columns the Model derives from belongs_to (the
    /// repo filters on them); without them every scoped insert/query hits "no such
    /// column". A CROSS-module belongs_to (Lead→Workspace, different module) is an
    /// UNENFORCED relation: just the column + an index, NO `FOREIGN KEY`/`REFERENCES`
    /// clause (per-module gen-tests migrate only one module, so a real FK to another
    /// module's table would 500 with "no such table" — fix F2). A documenting SQL
    /// comment records the relation. Enum `values` become a CHECK; unique/index
    /// fields gain their constraint/standalone index.
    #[test]
    fn cross_module_fk_is_an_unenforced_indexed_column() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let ddl = migration_ddl(&d.modules[1], false, &d).unwrap();
        let lower = ddl.to_lowercase();
        assert!(
            lower.contains("\"workspace_id\""),
            "fk column from belongs_to: {ddl}"
        );
        // No database FK constraint to a table this module's migration never creates.
        assert!(
            !lower.contains("foreign key") && !lower.contains("references \"workspaces\""),
            "cross-module belongs_to must not emit a FK constraint: {ddl}"
        );
        // The unconstrained fk is still indexed so lookups/joins stay fast.
        assert!(
            lower.contains("create index") && lower.contains("idx_leads_workspace_id"),
            "cross-module fk column must be indexed: {ddl}"
        );
        // The migration documents the relation it no longer enforces.
        assert!(
            ddl.contains(
                "-- workspace_id: references workspaces.id (cross-module; enforced by handlers, see schema.json)"
            ),
            "documenting comment: {ddl}"
        );
        assert!(lower.contains("unique"), "phone unique: {ddl}");
        let ws = migration_ddl(&d.modules[0], false, &d)
            .unwrap()
            .to_lowercase();
        assert!(
            ws.contains("check") && ws.contains("'trial'"),
            "enum check: {ws}"
        );
    }

    /// An INTRA-module belongs_to (parent and child in the same ModuleDesign) keeps
    /// its real database FOREIGN KEY constraint + on_delete policy — the parent table
    /// is created by the same migration, so SQLite FK enforcement resolves fine. This
    /// is the case the cross-module carve-out must NOT weaken.
    #[test]
    fn intra_module_fk_keeps_its_constraint_and_policy() {
        let mut d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        // Add a second entity to the workspaces module that belongs_to Workspace
        // (SAME module) with on_delete: cascade.
        let member: Entity = serde_json::from_str(
            r#"{
                "name": "Member",
                "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
                "fields": [{ "name": "email", "type": "string" }]
            }"#,
        )
        .unwrap();
        d.modules[0].entities.push(member);
        let ddl = migration_ddl(&d.modules[0], false, &d)
            .unwrap()
            .to_lowercase();
        assert!(ddl.contains("\"workspace_id\""), "fk column: {ddl}");
        assert!(
            ddl.contains("foreign key") && ddl.contains("references \"workspaces\""),
            "intra-module belongs_to keeps a real FK constraint: {ddl}"
        );
        assert!(
            ddl.contains("on delete cascade"),
            "intra-module fk keeps its policy: {ddl}"
        );
    }

    /// Optional design fields (`required: false`) render as NULLABLE columns,
    /// matching the `Option<T>` Model field they back. The old `NOT NULL DEFAULT
    /// <zero>` rendering (a v0 relic, when models were plain types) made an
    /// inserted `None` violate NOT NULL at runtime; nullable is the fix.
    #[test]
    fn optional_fields_are_nullable_columns() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let ddl = migration_ddl(&d.modules[1], false, &d)
            .unwrap()
            .to_lowercase();
        assert!(!ddl.contains("\"custom\" text not null"), "{ddl}");
        assert!(!ddl.contains("default ''"), "no zero-defaults: {ddl}");
    }

    /// A `bool` Model field is stored in a native BOOLEAN column now (SeaORM
    /// round-trips it directly — the old sqlx-Any "bool as i64" lore is dead). A
    /// SetNull belongs_to makes the fk column nullable (it must drop to NULL when
    /// the parent dies, so it can't be NOT NULL) — this holds even cross-module,
    /// where only the FK constraint is dropped, not the column's shape.
    #[test]
    fn ddl_booleans_are_native_and_set_null_fks_nullable() {
        let mut d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        d.modules[1].entities[0].fields.push(Field {
            name: "active".into(),
            field_type: FieldType::Boolean,
            required: true,
            unique: false,
            index: false,
            values: None,
            default: None,
        });
        d.modules[1].entities[0].belongs_to[0].on_delete = OnDelete::SetNull;
        let ddl = migration_ddl(&d.modules[1], false, &d)
            .unwrap()
            .to_lowercase();
        assert!(ddl.contains("\"active\" boolean not null"), "{ddl}");
        assert!(
            !ddl.contains("\"workspace_id\" bigint not null"),
            "set_null fk must be nullable: {ddl}"
        );
    }

    /// A SetNull belongs_to within ONE module keeps its real `on delete set null`
    /// FK constraint (the parent table exists in the same migration). The cross-
    /// module carve-out (which drops the constraint) must not steal this from an
    /// intra-module relation.
    #[test]
    fn intra_module_set_null_fk_keeps_its_policy() {
        let mut d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let member: Entity = serde_json::from_str(
            r#"{
                "name": "Member",
                "belongs_to": [{ "entity": "Workspace", "on_delete": "set_null" }],
                "fields": [{ "name": "email", "type": "string" }]
            }"#,
        )
        .unwrap();
        d.modules[0].entities.push(member);
        let ddl = migration_ddl(&d.modules[0], false, &d)
            .unwrap()
            .to_lowercase();
        assert!(ddl.contains("foreign key"), "intra-module FK kept: {ddl}");
        assert!(ddl.contains("on delete set null"), "policy kept: {ddl}");
    }

    /// The tenant module also owns the membership table that records who belongs
    /// to each tenant: `{tenant}_members` with user_id + the tenant fk + role,
    /// cascading so a tenant's memberships die with it.
    #[test]
    fn tenancy_generates_the_membership_table_in_the_tenant_module() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let ws = migration_ddl(&d.modules[0], false, &d)
            .unwrap()
            .to_lowercase();
        assert!(ws.contains("create table \"workspace_members\""), "{ws}");
        assert!(
            ws.contains("\"user_id\"") && ws.contains("\"role\""),
            "{ws}"
        );
        // user_id is TEXT (the stringified user pk, mirroring storage_objects.owner_id)
        // so a uuid auth.users id fits — NOT bigint, which a uuid could not hold.
        assert!(
            ws.contains("\"user_id\" text"),
            "membership user_id must be TEXT for uuid/string user pks: {ws}"
        );
        assert!(
            ws.contains("on delete cascade"),
            "member rows die with the tenant: {ws}"
        );
    }

    #[test]
    fn db_mode_models_are_sea_orm_entities() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let m = &d.modules[1];
        let src = model_rs_db(m, &d, false).unwrap();
        assert!(src.contains("pub mod lead {"), "{src}");
        // Cross-module belongs_to stays decoupled: fk field, NO relation arm.
        assert!(src.contains("pub enum Relation {}"), "{src}");
        assert!(
            src.contains("use jerrycan::db::sea_orm;"),
            "facade alias, no direct dep: {src}"
        );
        assert!(src.contains("#[sea_orm(table_name = \"leads\")]"), "{src}");
        assert!(src.contains("#[sea_orm(primary_key)]"), "{src}");
        assert!(
            src.contains("pub workspace_id: i64"),
            "fk column from belongs_to: {src}"
        );
        assert!(
            src.contains("pub custom: Option<Json>"),
            "json + optional: {src}"
        );
        assert!(src.contains("pub use lead::Model as Lead;"), "{src}");
        assert!(
            src.contains("impl ActiveModelBehavior for ActiveModel {}"),
            "{src}"
        );
    }

    /// A field named after a Rust keyword (`type`) survives into generated code
    /// as a RAW identifier (`r#type`) at every Rust position — the Model struct
    /// field and the ActiveModel `field: Set(item.field)` binds — while the serde
    /// rename + sea_orm column_name keep the wire (JSON) and SQL names as `type`.
    /// WHY: frozen external wire contracts carry `type`/`match`/`ref` fields;
    /// forcing a rename would push a permanent wire↔storage mapping into every
    /// handler. (Issue #10.)
    #[test]
    fn keyword_field_names_become_raw_identifiers_with_preserved_wire_and_sql_names() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "webhooks", "contract_version": 1, "dependencies": ["db"],
                "modules": [{ "name": "events",
                    "entities": [{ "name": "Event", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "type", "type": "string" } ] }],
                    "endpoints": [{ "operation_id": "create_event", "method": "POST", "path": "/",
                        "request_body": { "entity": "Event" },
                        "success": { "status": 201, "entity": "Event" } }] }] }"#,
        )
        .unwrap();
        // The design is accepted (validator no longer rejects the keyword field).
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "a `type` field must not raise a question: {:?}",
            crate::platform::questions::validate(&d)
        );
        let m = &d.modules[0];
        // db-mode Model: raw ident + serde rename + sea_orm column_name.
        let model = model_rs_db(m, &d, false).unwrap();
        assert!(
            model.contains("#[serde(rename = \"type\")]\n        #[sea_orm(column_name = \"type\")]\n        pub r#type: String,"),
            "keyword field is a raw ident carrying rename + column_name: {model}"
        );
        // ActiveModel assignment uses the raw ident on both sides.
        let e = &m.entities[0];
        let sets = active_sets(e, true);
        assert!(
            sets.contains("r#type: Set(item.r#type),"),
            "ActiveModel binds the raw ident: {sets}"
        );
        // memory-mode Model: raw ident + serde rename (no sea_orm attr).
        let mem = model_rs(m).unwrap();
        assert!(
            mem.contains("#[serde(rename = \"type\")]\n    pub r#type: String,"),
            "memory Model raw ident + rename: {mem}"
        );
        assert!(
            !mem.contains("column_name"),
            "no sea_orm attr in memory mode: {mem}"
        );
    }

    /// Intra-module belongs_to wires a Relation arm + Related impl (cross-module
    /// stays a bare fk for decoupling); a synthetic pk gets `#[serde(default)]`
    /// so POST bodies may omit `id`; SetNull makes the fk `Option<_>` + default.
    #[test]
    fn intra_module_relation_synthetic_pk_and_set_null_fk() {
        let mut d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        // Add a second entity to the workspaces module that belongs_to Workspace
        // (same module) with on_delete: set_null and NO declared id (synthetic pk).
        let member: Entity = serde_json::from_str(
            r#"{
                "name": "Member",
                "belongs_to": [{ "entity": "Workspace", "on_delete": "set_null" }],
                "fields": [{ "name": "email", "type": "string" }]
            }"#,
        )
        .unwrap();
        d.modules[0].entities.push(member);
        let src = model_rs_db(&d.modules[0], &d, false).unwrap();
        // Synthetic pk → visible id field with serde(default) so POST may omit it.
        assert!(
            src.contains(
                "#[sea_orm(primary_key)]\n        #[serde(default)]\n        pub id: i64,"
            ),
            "{src}"
        );
        // SetNull fk → nullable + default.
        assert!(
            src.contains("#[serde(default)]\n        pub workspace_id: Option<i64>,"),
            "{src}"
        );
        // Intra-module target → Relation arm keyed on the PascalCase fk column.
        assert!(
            src.contains("#[sea_orm(belongs_to = \"super::workspace::Entity\", from = \"Column::WorkspaceId\", to = \"super::workspace::Column::Id\")]"),
            "{src}"
        );
        assert!(src.contains("Relation::Workspace.def()"), "{src}");
        assert!(
            src.contains("impl Related<super::workspace::Entity> for Entity"),
            "{src}"
        );
    }

    /// db-mode repos run on SeaORM (entity finders + ActiveModel) over the
    /// jerrycan::db facade — never sea-query/sqlx and never `self.db.pool()`
    /// (that handle no longer exists). The insert returns the generated key.
    #[test]
    fn db_repos_query_via_sea_orm() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let src = repo_rs(
            &d.modules[1],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(src.contains("lead::Entity::find()"), "{src}");
        assert!(src.contains(".all(self.db.conn())"), "{src}");
        assert!(
            src.contains("pub async fn insert(&self, item: Lead) -> Result<i64>"),
            "{src}"
        );
        assert!(!src.contains("self.db.pool()"), "{src}");
        assert!(
            !src.contains("build_any_sqlx"),
            "repos are SeaORM now: {src}"
        );
    }

    /// An entity that belongs_to the design's tenancy entity gains tenant-scoped
    /// accessors keyed on the fk column — handlers must use these so a tenant
    /// can never read or delete another tenant's rows (JL0006).
    #[test]
    fn tenant_owned_entities_get_scoped_methods() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let src = repo_rs(
            &d.modules[1],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains("pub async fn all_for(&self, workspace_id: i64)"),
            "{src}"
        );
        assert!(
            src.contains("pub async fn get_for(&self, workspace_id: i64, id: i64)"),
            "{src}"
        );
        assert!(
            src.contains("pub async fn remove_for(&self, workspace_id: i64, id: i64)"),
            "{src}"
        );
        // A scoped UPDATE accessor exists so a tenant-owned handler can write
        // without the unscoped `repo.update(` that JL0006 forbids.
        assert!(
            src.contains("pub async fn update_for(&self, workspace_id: i64, id: i64, item:"),
            "{src}"
        );
        assert!(
            src.contains("Column::WorkspaceId.eq(workspace_id)"),
            "{src}"
        );
    }

    #[test]
    fn handler_signatures_follow_the_mapping_rules() {
        let m = todos();
        let h = handlers_rs(&m, GenMode::default(), &demo());
        assert!(
            h.contains(
                "pub(crate) async fn list_todos(_repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>>"
            ),
            "{h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn create_todo(_repo: Dep<TodoRepo>, Json(_body): Json<Todo>) -> Result<Created<Todo>>"
            ),
            "{h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn delete_todo(_repo: Dep<TodoRepo>, Path(_id): Path<i64>) -> Result<NoContent>"
            ),
            "{h}"
        );
        assert!(h.contains("not implemented — replace this stub"));
    }

    /// Issue #46: a 3xx-success endpoint gets a COMPILING `Redirect`-shaped stub —
    /// `Result<Redirect>` with `Ok(Redirect::<ctor>("/"))` whose status already
    /// matches the contract — plus a TODO naming the real target. WHY (Rule 9):
    /// before this, a 3xx endpoint emitted `Result<Json<serde_json::Value>>` and the
    /// agent had to hand-switch the whole return type to go green (the P3 papercut).
    /// The constructor tracks the declared status so the acceptance probe (which
    /// asserts the raw status — TestClient does not follow redirects) passes on the
    /// stub. A non-redirect endpoint keeps the 500 stub.
    #[test]
    fn redirect_endpoints_get_a_compiling_redirect_stub() {
        const SHORTENER: &str = r#"{
            "name": "shortener", "contract_version": 0, "dependencies": [],
            "modules": [{ "name": "links",
                "endpoints": [
                    { "operation_id": "follow_link", "method": "GET", "path": "/{code}",
                      "success": { "status": 303 } },
                    { "operation_id": "moved_link", "method": "GET", "path": "/old/{code}",
                      "success": { "status": 308 } } ] }]
        }"#;
        let d: Design = serde_json::from_str(SHORTENER).unwrap();
        let h = handlers_rs(&d.modules[0], GenMode::default(), &d);
        // 303 → see_other, and the return type is Redirect, not Json.
        assert!(
            h.contains(
                "pub(crate) async fn follow_link(Path(_code): Path<i64>) -> Result<Redirect>"
            ),
            "3xx handler returns Redirect: {h}"
        );
        assert!(
            h.contains("Ok(Redirect::see_other(\"/\"))"),
            "303 stub uses see_other and compiles: {h}"
        );
        // 308 → permanent (constructor tracks the declared status).
        assert!(
            h.contains("Ok(Redirect::permanent(\"/\"))"),
            "308 stub uses permanent: {h}"
        );
        // The TODO tells the agent to point it at the real destination.
        assert!(
            h.contains("TODO (issue #46): redirect to the real target"),
            "redirect stub carries a target TODO: {h}"
        );
        // A redirect stub must NOT be the 500 Err stub.
        let follow_stub = h
            .split("async fn follow_link")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            !follow_stub.contains("not implemented — replace this stub"),
            "redirect stub is not the 500 stub: {follow_stub}"
        );
    }

    /// A design body with realtime + a server-publishable broadcast topic
    /// (`scope: auth`) and both a read and a write endpoint. Used to pin the
    /// realtime publish wiring (issue #50) and its gating.
    const RT_BROADCAST: &str = r#"{
        "name": "rt-pub", "contract_version": 2,
        "auth": { "model": "jwt", "roles": ["admin"] },
        "dependencies": ["db", "auth", "realtime"],
        "modules": [{
            "name": "notes",
            "entities": [{ "name": "Note", "fields": [
                { "name": "text", "type": "string", "required": true } ]}],
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
        "realtime": { "changes": [], "broadcast": [{ "name": "events", "scope": "auth" }], "presence": [] }
    }"#;

    /// A mutating handler in a design that declares a server-publishable broadcast
    /// topic gets a `Dep<RealtimeHandle>` param plus a stub comment showing the
    /// one-liner (issue #50). A READ endpoint gets neither — the canonical pattern
    /// is "a write created a row, now push it".
    #[test]
    fn write_handler_gets_realtime_publish_dep_and_comment() {
        let d: Design = serde_json::from_str(RT_BROADCAST).unwrap();
        let m = &d.modules[0];
        let h = handlers_rs(
            m,
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        // The POST handler carries the resolvable dep and the copy-pasteable call.
        assert!(
            h.contains("_rt: Dep<jerrycan::realtime::RealtimeHandle>"),
            "write handler must take the RealtimeHandle dep:\n{h}"
        );
        assert!(
            h.contains(r#"_rt.publish("events", serde_json::json!("#),
            "stub comment must show the publish one-liner on the declared topic:\n{h}"
        );
        // The GET handler is untouched — no dep, no comment.
        let list = h
            .split("pub(crate) async fn create_note")
            .next()
            .expect("list_notes precedes create_note");
        assert!(
            !list.contains("_rt"),
            "read handlers must not gain the realtime dep:\n{list}"
        );
    }

    /// No-drift guard: a design whose ONLY broadcast topic is tenant-scoped is NOT
    /// server-publishable (a global server publish can't pick a tenant), so its
    /// handlers stay byte-identical — no dep, no comment. This is what keeps the
    /// reference-slice conformance design (its only topic is `deal_room`/tenant)
    /// unchanged.
    #[test]
    fn tenant_only_broadcast_emits_no_realtime_publish_wiring() {
        let mut d: Design = serde_json::from_str(RT_BROADCAST).unwrap();
        d.realtime.as_mut().unwrap().broadcast[0].scope =
            crate::platform::design::RealtimeScope::Tenant;
        let m = &d.modules[0];
        let h = handlers_rs(
            m,
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        assert!(
            !h.contains("_rt"),
            "tenant-only broadcast designs get no server-publish wiring:\n{h}"
        );
    }

    #[test]
    fn multi_param_endpoints_map_to_path_tuples() {
        let mut m = todos();
        m.endpoints.push(Endpoint {
            operation_id: "move_todo".into(),
            method: HttpMethod::POST,
            path: "/{id}/position/{slot}".into(),
            auth_required: false,
            required_roles: vec![],
            public: false,
            probe: ProbePolicy::default(),
            request_body: None,
            success: Success {
                status: 204,
                entity: None,
                list: false,
            },
            errors: vec![],
        });
        let h = handlers_rs(&m, GenMode::default(), &demo());
        assert!(
            h.contains("pub(crate) async fn move_todo(_repo: Dep<TodoRepo>, Path((_id, _slot)): Path<(i64, i64)>) -> Result<NoContent>"),
            "{h}"
        );
    }

    /// A subroute mounted under a param-carrying prefix generates a handler whose
    /// single Path param is emitted positionally for its OWN leaf `{id}` — NOT a
    /// tuple covering the parent's prefix param. The generator counts only the
    /// endpoint's own `{params}`; the leaf-binding semantics live in core
    /// (single `Path<T>` reads the last captured param). This pins that contract
    /// at the generation level: a leaf endpoint under `/{ws}` stays single-Path.
    #[test]
    fn subroute_under_param_mount_keeps_single_path_for_its_leaf() {
        // Parent module mounts at `/ws/{ws}`; its child subroute owns `/{id}`.
        let m: ModuleDesign = serde_json::from_str(
            r#"{
                "name": "ws",
                "mount": "/ws/{ws}",
                "endpoints": [
                    { "operation_id": "list_ws", "method": "GET", "path": "/",
                      "success": { "status": 200 } }
                ],
                "subroutes": [
                    {
                        "name": "leads",
                        "mount": "/leads",
                        "endpoints": [
                            { "operation_id": "show_lead", "method": "GET", "path": "/{id}",
                              "success": { "status": 200 } }
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();
        let sub = &m.subroutes[0];
        let h = handlers_rs(sub, GenMode::default(), &demo());
        // The leaf param is a single Path<i64>, not a tuple over the mount param.
        assert!(
            h.contains("pub(crate) async fn show_lead(Path(_id): Path<i64>)"),
            "leaf endpoint under a param mount must stay single-Path: {h}"
        );
        assert!(!h.contains("Path((_"), "no tuple over the mount param: {h}");
    }

    #[test]
    fn lib_rs_groups_routes_by_path_and_mounts_subroutes() {
        let m = todos();
        let lib = lib_rs(&m, GenMode::default());
        assert!(lib.contains("pub fn module() -> Module"), "{lib}");
        assert!(
            lib.contains(".route(\"/\", get(handlers::list_todos).post(handlers::create_todo))"),
            "{lib}"
        );
        assert!(
            lib.contains(".route(\"/{id}\", delete(handlers::delete_todo))"),
            "{lib}"
        );
        assert!(
            lib.contains(".mount(\"/comments\", subroutes::comments::module())"),
            "{lib}"
        );
        assert!(lib.contains(".provide(repo::TodoRepo::new())"), "{lib}");
        assert!(
            lib.contains("deps::configure("),
            "agent hook must wrap the module: {lib}"
        );
        assert!(lib.contains("#![forbid(unsafe_code)]"));
    }

    /// A multi-word entity name (`ApiKey`) must wire the same snake_case repo
    /// factory in lib.rs that repo.rs actually defines. The repo factory is named
    /// `{to_snake(name)}_repo` (`api_key_repo`); lib.rs's `.provide_dep` must point
    /// at that exact path. A `to_lowercase` here would emit `apikey_repo`, which
    /// resolves to nothing → E0425 at compile. (WHY: db DI is by-path, so the
    /// reference and the definition must agree letter-for-letter.)
    #[test]
    fn db_repo_factory_name_matches_for_multi_word_entities() {
        let mut d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let api_key: Entity = serde_json::from_str(
            r#"{ "name": "ApiKey", "fields": [{ "name": "token", "type": "string" }] }"#,
        )
        .unwrap();
        d.modules[1].entities.push(api_key);
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let lib = lib_rs(&d.modules[1], mode);
        assert!(
            lib.contains(".provide_dep(repo::api_key_repo)"),
            "lib.rs must reference the snake_case repo factory: {lib}"
        );
        let repo = repo_rs(&d.modules[1], mode, &d).unwrap();
        assert!(
            repo.contains("pub(crate) async fn api_key_repo("),
            "repo.rs must define the snake_case repo factory: {repo}"
        );
    }

    #[test]
    fn model_and_repo_are_generated_from_entities() {
        let m = todos();
        let model = model_rs(&m).unwrap();
        assert!(model.contains("pub struct Todo"));
        assert!(model.contains("pub title: String"));
        assert!(
            model.contains("#[serde(default)]\n    pub done: bool"),
            "{model}"
        );
        let repo = repo_rs(&m, GenMode::default(), &demo()).unwrap();
        assert!(repo.contains("pub struct TodoRepo"));
        assert!(
            repo.contains("#[allow(dead_code)]"),
            "stub-phase repo must pass -D warnings: {repo}"
        );
        for method in [
            "pub fn all(",
            "pub fn get(",
            "pub fn insert(",
            "pub fn remove(",
            "pub fn update(",
        ] {
            assert!(repo.contains(method), "{repo}");
        }
    }

    #[test]
    fn write_module_respects_the_ownership_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let routes = tmp.path().join("crates/routes");
        let d = demo();
        let m = &d.modules[0];

        let created = write_module(&routes, m, GenMode::default(), &d).unwrap();
        assert!(created.iter().any(|p| p.ends_with("todos/src/lib.rs")));
        assert!(
            created
                .iter()
                .any(|p| p.ends_with("todos/src/subroutes/comments/mod.rs"))
        );

        // Agent edits handlers.rs; tool hand-edits lib.rs (illegally).
        let handlers = routes.join("todos/src/handlers.rs");
        fs::write(&handlers, "// AGENT CODE\n").unwrap();
        let lib = routes.join("todos/src/lib.rs");
        fs::write(&lib, "// hand edit\n").unwrap();

        write_module(&routes, m, GenMode::default(), &d).unwrap();
        assert_eq!(
            fs::read_to_string(&handlers).unwrap(),
            "// AGENT CODE\n",
            "agent-owned: preserved"
        );
        assert!(
            fs::read_to_string(&lib)
                .unwrap()
                .contains("pub fn module()"),
            "tool-owned: restored"
        );
    }

    /// Issue #69a: regeneration REWRITES tool-owned lib.rs, and before this an
    /// agent's hand-added `mod` wiring (a cross-module sweep) vanished silently.
    /// `write_module_reporting` now reports every salient AGENT line the fresh
    /// emission drops so the CLI can warn loudly — while the tool's OWN decls
    /// (`mod handlers;` …) and design-driven churn are never mistaken for agent
    /// work. WHY (Rule 9): the loss is invisible without this report, which is
    /// exactly how JR4 lost its cross-module wiring.
    #[test]
    fn regeneration_reports_dropped_agent_mod_lines_never_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let routes = tmp.path().join("crates/routes");
        let d = demo();
        let m = &d.modules[0];

        // Fresh generation: the file did not exist, so nothing is dropped.
        let (_c, dropped) = write_module_reporting(&routes, m, GenMode::default(), &d).unwrap();
        assert!(
            dropped.is_empty(),
            "fresh scaffold drops nothing: {dropped:?}"
        );

        // Agent wires a cross-module sweep by hand-adding a `mod` line to lib.rs.
        let lib = routes.join("todos/src/lib.rs");
        let orig = fs::read_to_string(&lib).unwrap();
        fs::write(&lib, format!("{orig}mod cross_sweep;\n")).unwrap();

        // Regeneration reports the dropped line instead of silently losing it.
        let (_c, dropped) = write_module_reporting(&routes, m, GenMode::default(), &d).unwrap();
        let (file, lines) = dropped
            .iter()
            .find(|(f, _)| f.ends_with("todos/src/lib.rs"))
            .expect("the dropped lib.rs line must be reported");
        assert!(
            lines.iter().any(|l| l == "mod cross_sweep;"),
            "the exact dropped agent line must be named: {file} {lines:?}"
        );
        // The tool's own decls are NOT reported as agent drops.
        assert!(
            !lines
                .iter()
                .any(|l| l == "mod handlers;" || l == "mod deps;"),
            "tool decls must not be flagged as lost agent work: {lines:?}"
        );
    }

    /// Issue #56: in a multi-entity module where the `/{id}`-bearing entity is NOT
    /// first, a no-request-body endpoint (`DELETE /tasks/{id}`, 204) must bind the
    /// repo of the entity its COLLECTION creates (Task, via `POST /tasks`), NOT the
    /// module's first entity (Project). The body-bearing `PUT /tasks/{id}` already
    /// resolves via its request body — this pins the no-body path the scaffold used
    /// to mis-wire, handing the agent a misleading `ProjectRepo` starting point.
    #[test]
    fn no_body_id_route_binds_its_collection_entity_not_the_first() {
        let m: ModuleDesign = serde_json::from_str(
            r#"{
                "name": "projects",
                "entities": [
                    { "name": "Project", "fields": [{ "name": "name", "type": "string" }] },
                    { "name": "Task", "fields": [{ "name": "title", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_project", "method": "POST", "path": "/",
                      "request_body": { "entity": "Project" }, "success": { "status": 201, "entity": "Project" } },
                    { "operation_id": "create_task", "method": "POST", "path": "/tasks",
                      "request_body": { "entity": "Task" }, "success": { "status": 201, "entity": "Task" } },
                    { "operation_id": "delete_task", "method": "DELETE", "path": "/tasks/{id}",
                      "success": { "status": 204 } },
                    { "operation_id": "update_task", "method": "PUT", "path": "/tasks/{id}",
                      "request_body": { "entity": "Task" }, "success": { "status": 200, "entity": "Task" } }
                ]
            }"#,
        )
        .unwrap();
        let h = handlers_rs(&m, GenMode::default(), &demo());
        // The no-body DELETE binds the Task repo (its collection's entity), not Project.
        assert!(
            h.contains("pub(crate) async fn delete_task(_repo: Dep<TaskRepo>, Path(_id): Path<i64>) -> Result<NoContent>"),
            "no-body /{{id}} must bind its collection entity's repo (#56): {h}"
        );
        // Control: the body-bearing PUT already resolves via its request body.
        assert!(
            h.contains("pub(crate) async fn update_task(_repo: Dep<TaskRepo>"),
            "{h}"
        );
        // The old bug bound the module's FIRST entity (Project) — must not recur.
        assert!(
            !h.contains("delete_task(_repo: Dep<ProjectRepo>"),
            "regression: no-body route must not bind the first entity's repo: {h}"
        );
    }

    /// #56 no-drift: a SINGLE-entity module's no-body `/{id}` route is unchanged —
    /// path-based resolution and the first-entity fallback name the SAME (sole)
    /// entity, so every conformance module (all single-entity) stays byte-identical.
    #[test]
    fn single_entity_module_no_body_route_is_unchanged_by_56() {
        let m = todos(); // one entity (Todo); delete_todo is DELETE /{id}, no body
        let h = handlers_rs(&m, GenMode::default(), &demo());
        assert!(
            h.contains(
                "pub(crate) async fn delete_todo(_repo: Dep<TodoRepo>, Path(_id): Path<i64>) -> Result<NoContent>"
            ),
            "single-entity no-body /{{id}} unchanged: {h}"
        );
    }

    /// Real agent designs declare `id` on their entities (the docs' Todo does).
    /// Emitting the synthetic pk alongside it is a duplicate-column error that
    /// breaks every migration at apply time — the declared field must BE the pk.
    #[test]
    fn declared_id_field_becomes_the_pk_not_a_duplicate_column() {
        let mut m = todos();
        m.entities[0].fields.insert(
            0,
            Field {
                name: "id".into(),
                field_type: FieldType::Integer,
                required: true,
                unique: false,
                index: false,
                values: None,
                default: None,
            },
        );
        let ddl = migration_ddl(&m, false, &demo()).unwrap();
        assert_eq!(
            ddl.matches("\"id\"").count(),
            1,
            "one id column only:\n{ddl}"
        );
        assert!(
            ddl.contains("PRIMARY KEY AUTOINCREMENT"),
            "sqlite autoincrement pk: {ddl}"
        );
        let pg = migration_ddl(&m, true, &demo()).unwrap();
        assert!(
            pg.to_lowercase().contains("bigserial"),
            "postgres serial pk: {pg}"
        );
        assert_eq!(pg.matches("\"id\"").count(), 1, "{pg}");
    }

    /// Text ids (uuid/string) are the pk with their declared type, and the
    /// whole generated surface keys on String — repo signatures, the insert
    /// return (sqlite has no last_insert_id for text pks), and Path extractors.
    #[test]
    fn text_id_keys_the_table_repo_and_handlers_consistently() {
        let mut m = todos();
        m.entities[0].fields.insert(
            0,
            Field {
                name: "id".into(),
                field_type: FieldType::Uuid,
                required: true,
                unique: false,
                index: false,
                values: None,
                default: None,
            },
        );
        let ddl = migration_ddl(&m, false, &demo()).unwrap();
        assert!(
            ddl.to_lowercase()
                .contains("\"id\" text not null primary key"),
            "text pk, no autoincrement: {ddl}"
        );
        assert!(!ddl.contains("AUTOINCREMENT"), "{ddl}");
        assert_eq!(ddl.matches("\"id\"").count(), 1, "{ddl}");

        let repo = repo_rs(
            &m,
            GenMode {
                db: true,
                ..GenMode::default()
            },
            &demo(),
        )
        .unwrap();
        assert!(
            repo.contains("pub async fn get(&self, id: String)"),
            "{repo}"
        );
        assert!(
            repo.contains("pub async fn insert(&self, item: Todo) -> Result<String>"),
            "{repo}"
        );
        assert!(!repo.contains(".last_insert_id()"), "{repo}");

        let h = handlers_rs(&m, GenMode::default(), &demo());
        assert!(h.contains("Path(_id): Path<String>"), "{h}");
    }

    #[test]
    fn subroutes_without_entities_have_no_model_or_repo() {
        let m = todos();
        let sub = &m.subroutes[0];
        assert!(model_rs(sub).is_none());
        assert!(repo_rs(sub, GenMode::default(), &demo()).is_none());
        let h = handlers_rs(sub, GenMode::default(), &demo());
        assert!(
            h.contains("pub(crate) async fn list_comments() -> Result<Json<serde_json::Value>>"),
            "{h}"
        );
    }

    /// The server-owned-FK matrix design (issue #34): Collection belongs_to the
    /// auth identity entity (User → fk column `user_id`); Bookmark belongs_to
    /// BOTH User and Collection. `create_collection`/`create_bookmark` are
    /// guarded; `import_collection` is the same entity UNGUARDED.
    pub(crate) const SERVER_FK: &str = r#"{
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
              "entities": [
                  { "name": "Collection",
                    "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                    "fields": [{ "name": "title", "type": "string" }] },
                  { "name": "Bookmark",
                    "belongs_to": [
                        { "entity": "User", "on_delete": "cascade" },
                        { "entity": "Collection", "on_delete": "cascade" }
                    ],
                    "fields": [{ "name": "url", "type": "string" }] }
              ],
              "endpoints": [
                  { "operation_id": "create_collection", "method": "POST", "path": "/",
                    "auth_required": true,
                    "request_body": { "entity": "Collection" },
                    "success": { "status": 201, "entity": "Collection" } },
                  { "operation_id": "update_collection", "method": "PUT", "path": "/{id}",
                    "auth_required": true,
                    "request_body": { "entity": "Collection" },
                    "success": { "status": 200, "entity": "Collection" } },
                  { "operation_id": "create_bookmark", "method": "POST", "path": "/bookmarks",
                    "auth_required": true,
                    "request_body": { "entity": "Bookmark" },
                    "success": { "status": 201, "entity": "Bookmark" } },
                  { "operation_id": "import_collection", "method": "POST", "path": "/import",
                    "request_body": { "entity": "Collection" },
                    "success": { "status": 201, "entity": "Collection" } }
              ] }
        ]
    }"#;

    /// Issue #34: a guarded endpoint whose body entity has an identity FK gets a
    /// `{Entity}Request` DTO WITHOUT `user_id` (the server injects the session
    /// user's id); non-identity FKs stay required; the Model keeps `user_id`
    /// (responses + DB still carry it); unguarded endpoints keep the plain entity.
    #[test]
    fn guarded_identity_fk_gets_a_request_dto_without_user_id() {
        let d: Design = serde_json::from_str(SERVER_FK).unwrap();
        let m = &d.modules[1]; // collections
        let model = model_rs_db(m, &d, true).unwrap();

        // (a) guarded + identity FK → DTO exists and omits user_id.
        let dto = model
            .split("pub struct CollectionRequest {")
            .nth(1)
            .expect("CollectionRequest emitted")
            .split('}')
            .next()
            .unwrap();
        assert!(!dto.contains("user_id"), "DTO must omit user_id: {dto}");
        assert!(dto.contains("pub title: String,"), "{dto}");
        // (c) non-identity FK stays required client input in the DTO.
        let bdto = model
            .split("pub struct BookmarkRequest {")
            .nth(1)
            .expect("BookmarkRequest emitted")
            .split('}')
            .next()
            .unwrap();
        assert!(bdto.contains("pub collection_id: i64,"), "{bdto}");
        assert!(!bdto.contains("user_id"), "{bdto}");
        // The Model itself keeps the fk column — the server writes it.
        assert!(model.contains("pub user_id: i64,"), "{model}");

        // Handler params: guarded → DTO; unguarded → plain entity.
        let h = handlers_rs(
            m,
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        assert!(
            h.contains(
                "pub(crate) async fn create_collection(_repo: Dep<CollectionRepo>, _user: CurrentUser, Json(_body): Json<CollectionRequest>)"
            ),
            "{h}"
        );
        assert!(h.contains("Json(_body): Json<BookmarkRequest>"), "{h}");
        // (b) unguarded endpoint keeps the full entity (no session to inject).
        assert!(
            h.contains(
                "pub(crate) async fn import_collection(_repo: Dep<CollectionRepo>, Json(_body): Json<Collection>)"
            ),
            "{h}"
        );
        // The stub tells the agent the server owns the fk.
        let create_stub = h
            .split("async fn create_collection")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            create_stub.contains("server-owned") && create_stub.contains("_user.0.id"),
            "stub must say the session injects user_id: {create_stub}"
        );
        let import_stub = h
            .split("async fn import_collection")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            !import_stub.contains("server-owned"),
            "unguarded stub carries no injection note: {import_stub}"
        );
    }

    /// Issue #42: the identity-FK omission is method-agnostic, but the stub GUIDANCE
    /// is not. A guarded UPDATE (PUT/PATCH) on an identity-FK entity also drops
    /// `user_id`, but the handler must NOT re-inject `_user.0.id` — that would let an
    /// admin editing another user's row reassign ownership to themselves. The update
    /// stub says PRESERVE the existing owner; the create stub still says inject. WHY
    /// (Rule 9): "updates can't move ownership" is the security-relevant semantics —
    /// a comment that told the agent to set `user_id` on update would encode a
    /// row-theft footgun.
    #[test]
    fn update_on_identity_fk_preserves_owner_not_reassigns() {
        let d: Design = serde_json::from_str(SERVER_FK).unwrap();
        let m = &d.modules[1]; // collections
        let h = handlers_rs(
            m,
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        // The update also takes the DTO (user_id off the wire) — method-agnostic rule.
        assert!(
            h.contains(
                "pub(crate) async fn update_collection(_repo: Dep<CollectionRepo>, _user: CurrentUser, Path(_id): Path<i64>, Json(_body): Json<CollectionRequest>)"
            ),
            "{h}"
        );
        let update_stub = h
            .split("async fn update_collection")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        // The update stub tells the agent to PRESERVE the owner, not reassign it.
        assert!(
            update_stub.contains("PRESERVE the")
                && update_stub.contains("Do NOT reassign")
                && !update_stub.contains("injects the\n    // session user's id. Use `_user.0.id`"),
            "update stub must preserve ownership, not inject the session user: {update_stub}"
        );
        // The create stub on the SAME entity still says inject (create-oriented).
        let create_stub = h
            .split("async fn create_collection")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            create_stub.contains("the server injects the")
                && create_stub.contains("Use `_user.0.id`")
                && !create_stub.contains("PRESERVE"),
            "create stub still injects the session user's id: {create_stub}"
        );
    }

    /// The DTO is auth-mode-gated: generating the same design with auth off
    /// (genroute_compile's harness does this) must emit NO Request structs, and
    /// memory mode keeps the plain entity body (its struct has no fk fields).
    #[test]
    fn request_dto_is_absent_without_auth_or_db() {
        let d: Design = serde_json::from_str(SERVER_FK).unwrap();
        let m = &d.modules[1];
        let model = model_rs_db(m, &d, false).unwrap();
        assert!(!model.contains("CollectionRequest"), "{model}");
        assert!(!model.contains("BookmarkRequest"), "{model}");
        let h = handlers_rs(
            m,
            GenMode {
                db: false,
                auth: true,
            },
            &d,
        );
        assert!(
            h.contains("Json(_body): Json<Collection>"),
            "memory mode keeps the entity body: {h}"
        );
        assert!(!h.contains("CollectionRequest"), "{h}");
    }

    /// Issue #53a (defaulted fields): a db design whose body entity declares
    /// server-owned `default` fields drops them from `{Entity}Request` and the
    /// handler body type — a PUBLIC (unguarded, no-auth) create still uses the DTO,
    /// so the minimal client body Just Works. The stub tells the agent the server
    /// applies each default; the Model keeps the columns (NOT-NULL in the DB).
    pub(crate) const DEFAULTS: &str = r#"{
        "name": "news", "contract_version": 0, "dependencies": ["db"],
        "modules": [{ "name": "subscribers",
            "entities": [{ "name": "Subscriber", "fields": [
                { "name": "email", "type": "string" },
                { "name": "confirmed", "type": "boolean", "default": false },
                { "name": "status", "type": "string", "values": ["active", "expired"], "default": "active" } ] }],
            "endpoints": [{ "operation_id": "create_subscriber", "method": "POST", "path": "/",
                "request_body": { "entity": "Subscriber" },
                "success": { "status": 201, "entity": "Subscriber" } }] }]
    }"#;

    #[test]
    fn defaulted_fields_are_dropped_from_the_request_dto() {
        let d: Design = serde_json::from_str(DEFAULTS).unwrap();
        let m = &d.modules[0];
        // auth = false (a public, no-auth app): the DTO still exists for defaults.
        let model = model_rs_db(m, &d, false).unwrap();
        let dto = model
            .split("pub struct SubscriberRequest {")
            .nth(1)
            .expect("SubscriberRequest emitted for a defaulted-field entity")
            .split('}')
            .next()
            .unwrap();
        assert!(dto.contains("pub email: String,"), "{dto}");
        assert!(
            !dto.contains("confirmed") && !dto.contains("status"),
            "server-owned defaults must be omitted from the DTO: {dto}"
        );
        // The Model keeps the columns (NOT-NULL persisted server-side).
        assert!(model.contains("pub confirmed: bool,"), "{model}");
        assert!(model.contains("pub status: String,"), "{model}");

        let mode = GenMode {
            db: true,
            auth: false,
        };
        let h = handlers_rs(m, mode, &d);
        assert!(
            h.contains("Json(_body): Json<SubscriberRequest>"),
            "public create uses the trimmed DTO: {h}"
        );
        let stub = h
            .split("async fn create_subscriber")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            stub.contains("server-owned defaults")
                && stub.contains("`confirmed` = false")
                && stub.contains("`status` = \"active\""),
            "stub must name each default the server applies: {stub}"
        );
    }

    /// Issue #53b (nested parent fk): a child created under `POST /{habit_id}/checkins`
    /// gets `habit_id` from the PATH — the request DTO omits it and the handler
    /// (which already binds `Path(_habit_id)`) injects it. The Model keeps the fk.
    pub(crate) const NESTED_FK: &str = r#"{
        "name": "habits", "contract_version": 0, "dependencies": ["db"],
        "modules": [{ "name": "habits",
            "entities": [
                { "name": "Habit", "fields": [{ "name": "name", "type": "string" }] },
                { "name": "Checkin", "belongs_to": [{ "entity": "Habit" }],
                  "fields": [{ "name": "note", "type": "string" }] } ],
            "endpoints": [
                { "operation_id": "create_habit", "method": "POST", "path": "/",
                  "request_body": { "entity": "Habit" },
                  "success": { "status": 201, "entity": "Habit" } },
                { "operation_id": "create_checkin", "method": "POST", "path": "/{habit_id}/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } }] }]
    }"#;

    #[test]
    fn path_redundant_parent_fk_is_dropped_from_the_request_dto() {
        let d: Design = serde_json::from_str(NESTED_FK).unwrap();
        let m = &d.modules[0];
        let model = model_rs_db(m, &d, false).unwrap();
        let dto = model
            .split("pub struct CheckinRequest {")
            .nth(1)
            .expect("CheckinRequest emitted for a nested-fk entity")
            .split('}')
            .next()
            .unwrap();
        assert!(dto.contains("pub note: String,"), "{dto}");
        assert!(
            !dto.contains("habit_id"),
            "path-redundant parent fk must be omitted from the DTO: {dto}"
        );
        // The Model keeps the fk column (the row still stores habit_id).
        assert!(model.contains("pub habit_id: i64,"), "{model}");
        // Habit (top-level create) is unaffected — no DTO.
        assert!(!model.contains("HabitRequest"), "{model}");

        let mode = GenMode {
            db: true,
            auth: false,
        };
        let h = handlers_rs(m, mode, &d);
        assert!(
            h.contains(
                "pub(crate) async fn create_checkin(_repo: Dep<CheckinRepo>, Path(_habit_id): Path<i64>, Json(_body): Json<CheckinRequest>)"
            ),
            "handler binds the path param and the trimmed DTO: {h}"
        );
        let stub = h
            .split("async fn create_checkin")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            stub.contains("path-owned fk") && stub.contains("_habit_id"),
            "stub must say to inject the path fk: {stub}"
        );
    }

    /// A guarded identity-FK design whose body carries an enum `values` field —
    /// exercises the Model + `{Entity}Request` DTO validator wiring (issue #47).
    const ENUM_DTO: &str = r#"{
        "name": "tasks-app", "contract_version": 1,
        "auth": { "model": "session", "roles": ["admin"] },
        "dependencies": ["db", "auth"],
        "modules": [
            { "name": "users",
              "entities": [{ "name": "User", "fields": [{ "name": "email", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_users", "method": "GET", "path": "/",
                  "auth_required": true,
                  "success": { "status": 200, "entity": "User", "list": true } }] },
            { "name": "tasks",
              "entities": [{ "name": "Task",
                  "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                  "fields": [
                      { "name": "title", "type": "string" },
                      { "name": "status", "type": "string", "values": ["todo", "doing", "done"] },
                      { "name": "priority", "type": "string", "required": false, "values": ["low", "high"] }
                  ]}],
              "endpoints": [{ "operation_id": "create_task", "method": "POST", "path": "/",
                  "auth_required": true,
                  "request_body": { "entity": "Task" },
                  "success": { "status": 201, "entity": "Task" } }] }
        ]
    }"#;

    /// Issue #47: an enum `values` field must reject out-of-range input at the
    /// request boundary (422), not die in the DB CHECK (500). The generator wires
    /// each enum field to a generated allow-list `deserialize_with` validator, so
    /// `Json<T>` extraction 422s an out-of-range value BEFORE the handler/DB — in
    /// memory mode (the plain serde struct).
    #[test]
    fn enum_field_validates_at_deserialize_memory() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let src = model_rs(&d.modules[0]).unwrap(); // workspaces: Workspace.plan enum
        assert!(
            src.contains("#[serde(deserialize_with = \"de_workspace_plan\")]"),
            "enum field wired to its validator: {src}"
        );
        assert!(
            src.contains("fn de_workspace_plan"),
            "validator fn emitted: {src}"
        );
        assert!(
            src.contains("&[\"trial\", \"pro\"]"),
            "allow-list derived from `values`: {src}"
        );
        assert!(
            src.contains("custom("),
            "out-of-range → serde custom error (surfaces as 422): {src}"
        );
    }

    /// Same, in db mode: the SeaORM Model is nested in `pub mod {snake}`, so it
    /// references the root-level validator via `super::`.
    #[test]
    fn enum_field_validates_at_deserialize_db() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let src = model_rs_db(&d.modules[0], &d, false).unwrap();
        assert!(
            src.contains("#[serde(deserialize_with = \"super::de_workspace_plan\")]"),
            "db Model wires the validator via super::: {src}"
        );
        assert!(src.contains("fn de_workspace_plan"), "{src}");
        assert!(src.contains("&[\"trial\", \"pro\"]"), "{src}");
    }

    /// No-drift: an entity with NO enum field emits ZERO validation — output is
    /// byte-identical to before the fix.
    #[test]
    fn enum_free_entity_emits_no_validator() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let leads = model_rs_db(&d.modules[1], &d, false).unwrap(); // Lead: no enum
        assert!(
            !leads.contains("deserialize_with"),
            "enum-free entity keeps byte-identical output: {leads}"
        );
        assert!(!leads.contains("fn de_"), "no validator fns: {leads}");
    }

    /// The `{Entity}Request` DTO (issue #34) must validate its enum fields too —
    /// both required (`status`) and optional (`priority`). The Model references the
    /// validator via `super::`; the root-level DTO references it directly.
    #[test]
    fn dto_and_model_enum_fields_both_validate() {
        let d: Design = serde_json::from_str(ENUM_DTO).unwrap();
        let src = model_rs_db(&d.modules[1], &d, true).unwrap(); // tasks
        assert!(
            src.contains("#[serde(deserialize_with = \"super::de_task_status\")]"),
            "Model wires validator via super::: {src}"
        );
        assert!(
            src.contains("#[serde(deserialize_with = \"de_task_status\")]"),
            "DTO wires the same validator at root: {src}"
        );
        assert!(
            src.contains("fn de_task_priority"),
            "optional enum validated too: {src}"
        );
        assert!(
            src.contains("Result<Option<String>, D::Error>"),
            "optional validator returns Option<String>: {src}"
        );
    }
}
