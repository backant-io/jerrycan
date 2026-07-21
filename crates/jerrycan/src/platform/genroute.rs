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

/// True when this endpoint operates on a tenant-owned entity — its repo entity
/// resolves to a tenant path, directly OR transitively (issue #102: a grandchild
/// reached through a parent chain). Gates the scope-hint comment; the guard SHAPE
/// (`Dep<Tenant>` vs bare `CurrentUser`) is decided separately by
/// `endpoint_uses_tenant_guard` from the resolved path, so a flat grandchild stays
/// on the bare session.
fn endpoint_is_tenant_owned(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> bool {
    let Some(entity) = endpoint_repo_entity(m, ep) else {
        return false;
    };
    // Transitive tenant ownership (#102): the entity is tenant-owned when it resolves
    // to a tenant path — a direct child OR a grandchild through a parent chain — not
    // merely a direct `belongs_to`. `tenant_path` is `None` when there is no tenancy,
    // so it subsumes the old tenancy guard.
    m.entities
        .iter()
        .find(|e| e.name == entity)
        .is_some_and(|e| design.tenant_path(&e.name).is_some())
}

/// True when a guarded endpoint takes the membership-checked `Dep<Tenant>` — i.e.
/// its route is PATH-SCOPED, so the guard verifies membership in the tenant NAMED
/// IN THE PATH (a nested `/clubs/{club_id}/…`, or the tenant's own detail route)
/// and 404s a non-member (issue #78). A MembershipSet (flat) or Collection
/// endpoint takes a bare `CurrentUser` instead: a flat handler must not trust an
/// arbitrary membership via `Dep<Tenant>` — it scopes to the membership SET.
fn endpoint_uses_tenant_guard(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> bool {
    matches!(
        design.endpoint_tenant_shape(m, ep),
        TenantShape::PathScoped { .. }
    )
}

/// The path-param name that keys THIS endpoint's own entity (so it takes the
/// entity key type instead of `i64`). Conventionally `id`; but the tenant
/// entity's OWN detail route is normalized to `/{tenant_fk}` (issue #78), so on
/// the tenant module the tenant fk param IS the entity key. Returns `id` for
/// every other endpoint — byte-identical to the pre-#78 `p == "id"` rule for all
/// non-tenancy designs and all tenant-owned children (whose leaf param stays
/// `id`).
fn entity_key_param(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> String {
    if let Some(tenancy) = design.tenancy.as_ref() {
        let fk = Design::fk_column(&tenancy.entity);
        if m.entities.iter().any(|e| e.name == tenancy.entity)
            && endpoint_repo_entity(m, ep) == Some(tenancy.entity.as_str())
            && path_params(ep) == [fk.clone()]
        {
            return fk;
        }
    }
    "id".to_string()
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

/// The request-DTO struct name this endpoint's body deserializes into, or `None`
/// when it takes the plain `Json<{Entity}>`. An UPDATE (PUT/PATCH) of a defaulted
/// entity takes `{Entity}UpdateRequest` (keeps the `default` fields so they stay
/// settable — issue #85 D1); every other DTO endpoint takes `{Entity}Request`.
/// The single source of truth so `handler_params` and `server_owned_fk_comment`
/// agree on the name.
fn request_dto_name(
    m: &ModuleDesign,
    ep: &Endpoint,
    mode: GenMode,
    design: &Design,
) -> Option<String> {
    if !endpoint_takes_request_dto(m, ep, mode, design) {
        return None;
    }
    let entity = &ep.request_body.as_ref()?.entity;
    Some(
        if ep.method.is_update() && design.entity_has_default(entity) {
            format!("{entity}UpdateRequest")
        } else {
            format!("{entity}Request")
        },
    )
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

/// True when this endpoint is a PUBLIC read on a `public_read` entity (issue
/// #105): a GET whose repo entity opted into public_read takes NO `CurrentUser`
/// — regardless of its declared `auth_required` — so the read is public by
/// construction (the entity flag drives it; the design doesn't hand-set auth
/// per GET). Writes always keep their guard, and a role-gated GET
/// (`required_roles`) keeps its guard too: an explicit role demand outranks the
/// entity-level read-open default (stripping it would silently drop the role
/// check the design asked for). False for every non-`public_read` design,
/// keeping output byte-identical.
fn endpoint_is_public_read_get(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> bool {
    matches!(ep.method, HttpMethod::GET)
        && ep.required_roles.is_empty()
        && endpoint_repo_entity(m, ep).is_some_and(|entity| design.entity_is_public_read(entity))
}

fn handler_params(m: &ModuleDesign, ep: &Endpoint, mode: GenMode, design: &Design) -> String {
    let mut params = Vec::new();
    if let Some(e) = endpoint_repo_entity(m, ep) {
        params.push(format!("_repo: Dep<{e}Repo>"));
    }
    // Guard param (order: repo, guard, path, body). A guarded PATH-SCOPED endpoint
    // takes the membership-checked `Dep<shared::Tenant>` — the factory verifies the
    // caller belongs to the tenant NAMED IN THE PATH (401 from a missing session,
    // 404 from a non-member of that tenant), closing the #78 cross-tenant leak. A
    // flat (membership-set) or collection endpoint takes the bare authenticated
    // session and scopes to the membership SET / body tenant fk instead. A
    // public_read GET (#105) takes NO guard at all — its read is public.
    if mode.auth && ep.is_guarded() {
        if endpoint_uses_tenant_guard(m, ep, design) {
            params.push("_tenant: Dep<Tenant>".to_string());
        } else if !endpoint_is_public_read_get(m, ep, design) {
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
    // The param that keys the endpoint's entity takes that entity's key type
    // (String for text pks); other params stay i64. Conventionally `id`, but the
    // tenant entity's OWN detail route is normalized to `/{tenant_fk}` (#78), so
    // there the tenant fk param — not `id` — is the entity key.
    let key = endpoint_repo_entity(m, ep)
        .and_then(|name| m.entities.iter().find(|e| e.name == name))
        .map(key_rust_type)
        .unwrap_or("i64");
    let key_param = entity_key_param(m, ep, design);
    // The endpoint's own key param takes the entity key type; every OTHER path
    // param types from the entity it REFERENCES (issue #85) — a `{site_id}` for a
    // string-pk `Site` is `String`, not a hardcoded `i64` (falls back to `i64` for
    // an opaque param that names no entity's fk column).
    let param_type = |p: &str| {
        if p == key_param.as_str() {
            key
        } else {
            design.path_param_key_type(p)
        }
    };
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
        // A body that drops a server-owned field (issues #34/#53) takes the trimmed
        // DTO — `{Entity}Request`, or `{Entity}UpdateRequest` for a defaulted-entity
        // update (issue #85 D1) — not the entity itself.
        match request_dto_name(m, ep, mode, design) {
            Some(name) => params.push(format!("Json(_body): Json<{name}>")),
            None => params.push(format!("Json(_body): Json<{}>", rb.entity)),
        }
    }
    params.join(", ")
}

/// A leading comment for role-guarded endpoints, reminding the agent how to
/// enforce the role before proceeding (empty for unguarded / no-role endpoints).
/// A path-scoped endpoint carries `_tenant: Dep<Tenant>` and checks the role on
/// the membership (`_tenant.require_role(...)?`); other endpoints take a bare
/// `CurrentUser` and call `require_role(&_user.0.role, ...)` directly.
fn guard_comment(m: &ModuleDesign, ep: &Endpoint, design: &Design) -> String {
    if ep.required_roles.is_empty() {
        return String::new();
    }
    let roles = ep.required_roles.join("\", \"");
    if endpoint_uses_tenant_guard(m, ep, design) {
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

/// A stub comment naming the EXACT scoped repo method a tenant-owned READ handler
/// must call for its route shape (issues #78/#79), so the agent scopes correctly
/// by construction. Path-scoped reads (the guard already verified `_tenant`) use
/// `all_for`/`get_for`; membership-set (flat) reads scope to the caller's
/// memberships via `all_for_memberships`/`get_for_memberships`. Emitted only for
/// GET handlers on a tenant-OWNED entity (the tenant's own routes and every
/// non-tenant handler get nothing), so designs the rule doesn't touch — and every
/// non-tenancy design — stay byte-identical.
fn tenant_scope_comment(m: &ModuleDesign, ep: &Endpoint, mode: GenMode, design: &Design) -> String {
    if !(mode.db && mode.auth) {
        return String::new();
    }
    // Only a tenant-OWNED entity (belongs_to the tenant) has the scoped methods;
    // the tenant's OWN routes use the plain repo scoped to the verified id.
    if !endpoint_is_tenant_owned(m, ep, design) {
        return String::new();
    }
    let Some(entity) = endpoint_repo_entity(m, ep) else {
        return String::new();
    };
    let list = ep.success.list;
    let snake = Design::to_snake(entity);
    match design.endpoint_tenant_shape(m, ep) {
        // Path-scoped WRITES are scoped by the verified path tenant (T2
        // `update_for`/`remove_for`); only READS get a scope hint here, so the write
        // stubs stay byte-identical to pre-#94 for path-scoped/nested designs.
        TenantShape::PathScoped { .. } if matches!(ep.method, HttpMethod::GET) => {
            let call = if list {
                format!("{entity}Repo::all_for(_tenant.id())")
            } else {
                format!("{entity}Repo::get_for(_tenant.id(), _id)")
            };
            format!(
                "    // tenant scope (path): the guard verified membership in `_tenant`; scope this\n    // read to it via `{call}` — never the unscoped repo method (JL0006, issue #78).\n"
            )
        }
        // Flat (membership-set) READS scope to the caller's memberships.
        TenantShape::MembershipSet if matches!(ep.method, HttpMethod::GET) => {
            let call = if list {
                format!("{entity}Repo::all_for_memberships(_user.0.id)")
            } else {
                format!("{entity}Repo::get_for_memberships(_user.0.id, _id)")
            };
            format!(
                "    // tenant scope (membership-set): this flat route has no tenant in the path —\n    // scope to the CALLER'S memberships via `{call}` so a multi-tenant user sees every\n    // tenant's rows they belong to and nothing outside the set (issues #78/#79).\n"
            )
        }
        // Flat (membership-set) WRITES take the tenant fk from the BODY, so they MUST
        // verify it against the caller's memberships (RLS `WITH CHECK`, issue #94) —
        // steer to the membership-CHECKED method, never the unscoped `insert`/`update`/
        // `remove` which would let a caller write into a tenant they don't belong to.
        TenantShape::MembershipSet => match ep.method {
            HttpMethod::POST => format!(
                "    // tenant create (membership-set, issue #94): this flat create reads the tenant\n    // fk from the BODY — call `{entity}Repo::create_for_memberships(_user.0.id, {snake})`\n    // so that fk is verified ∈ your memberships (403 otherwise), NEVER the bare `insert`,\n    // which would write into a tenant the caller doesn't belong to.\n"
            ),
            HttpMethod::PUT | HttpMethod::PATCH => format!(
                "    // tenant update (membership-set, issue #94): scope the write to the caller's\n    // memberships — call `{entity}Repo::update_for_memberships(_user.0.id, _id, {snake})`\n    // (NEVER `update`); a row outside your set is 404 and moving it to another tenant is 403.\n"
            ),
            HttpMethod::DELETE => format!(
                "    // tenant delete (membership-set, issue #94): scope the delete to the caller's\n    // memberships — call `{entity}Repo::remove_for_memberships(_user.0.id, _id)` (NEVER\n    // the unscoped `remove`); a row outside your set is 404, never a cross-tenant delete.\n"
            ),
            HttpMethod::GET => String::new(),
        },
        TenantShape::PathScoped { .. } | TenantShape::Collection | TenantShape::None => {
            String::new()
        }
    }
}

/// A stub comment steering the tenant's OWN collection handlers to the membership-
/// lifecycle repo methods (issue #78, Task 3), so membership is correct-by-
/// construction: a `POST /` create to `create_with_membership(_user.0.id, …)` — NOT
/// the bare `insert`, which leaves the tenant memberless — and the tenant `GET /`
/// list to `all_for_member(_user.0.id)` — NOT the unscoped `all()`, which leaks
/// every tenant. Emitted only for a guarded db+auth `TenantShape::Collection`
/// endpoint (the tenant's own root), so every other handler — and every non-tenancy
/// design — stays byte-identical. Guarded is required: the methods key on the
/// session user, which only a guarded handler's `_user: CurrentUser` provides.
fn tenant_collection_comment(
    m: &ModuleDesign,
    ep: &Endpoint,
    mode: GenMode,
    design: &Design,
) -> String {
    if !(mode.db && mode.auth) || !ep.is_guarded() {
        return String::new();
    }
    if !matches!(design.endpoint_tenant_shape(m, ep), TenantShape::Collection) {
        return String::new();
    }
    let Some(entity) = endpoint_repo_entity(m, ep) else {
        return String::new();
    };
    let snake = Design::to_snake(entity);
    match ep.method {
        HttpMethod::POST => format!(
            "    // tenant create (issue #78): seed the creator's membership in the SAME\n    // transaction as the insert — call `{entity}Repo::create_with_membership(_user.0.id,\n    // {snake})` (NOT the bare `insert`), so a fresh {entity} is never memberless and the\n    // guard admits the creator on the next request.\n"
        ),
        HttpMethod::GET if ep.success.list => format!(
            "    // tenant list (issue #78): return ONLY the caller's tenants — call\n    // `{entity}Repo::all_for_member(_user.0.id)` (NOT the unscoped `all()`), so a user\n    // sees just the {entity}s they belong to.\n"
        ),
        _ => String::new(),
    }
}

/// A stub comment steering a per-user (identity-owned) READ handler to the
/// owner-scoped repo method (issue #79), so a cross-user read is scoped by
/// construction: a list to `all_for(_user.0.id)`, a `/{id}` read to
/// `get_for(_user.0.id, _id)`. The unscoped `all()/get()` are NOT generated on a
/// per-user repo (they don't compile), so this comment names the ONLY reachable
/// accessor. A `public_read` entity (issue #105) flips the steer: its reads are
/// PUBLIC, so the comment names the unscoped `all()`/`get(id)` the repo now
/// emits. Emitted only for a GET handler on a per-user entity in db+auth mode;
/// every other handler — and every non-per-user design — stays byte-identical.
fn owner_scope_comment(m: &ModuleDesign, ep: &Endpoint, mode: GenMode, design: &Design) -> String {
    if !(mode.db && mode.auth) || !matches!(ep.method, HttpMethod::GET) {
        return String::new();
    }
    let Some(entity) = endpoint_repo_entity(m, ep) else {
        return String::new();
    };
    let Some(e) = m.entities.iter().find(|e| e.name == entity) else {
        return String::new();
    };
    if !entity_is_per_user_owned(e, mode, design) {
        return String::new();
    }
    // public_read (#105): the read is PUBLIC — steer to the unscoped `all()`/
    // `get(id)` (which the repo now emits) instead of the owner-scoped accessors;
    // there is no `_user` param to scope by (handler_params dropped it). Writes
    // keep the owner-scoped steering through their own comments/accessors.
    if design.entity_is_public_read(entity) {
        let call = if ep.success.list {
            format!("{entity}Repo::all()")
        } else {
            format!("{entity}Repo::get(_id)")
        };
        return format!(
            "    // public read (issue #105): this {entity} is public_read — the read is UNSCOPED\n    // and serves every owner's rows. Call `{call}` (no session needed). Writes stay\n    // owner-scoped: route them through `update_for`/`remove_for` with the session\n    // user's id.\n"
        );
    }
    let call = if ep.success.list {
        format!("{entity}Repo::all_for(_user.0.id)")
    } else {
        format!("{entity}Repo::get_for(_user.0.id, _id)")
    };
    format!(
        "    // owner scope (issue #79): this {entity} belongs to the session user — scope this\n    // read via `{call}` (parse `_user.0.id`, the stringified session user id, for an\n    // integer fk). The unscoped repo method is NOT generated, so a cross-user read\n    // can't be written.\n"
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
            .any(|ep| ep.is_guarded() && endpoint_uses_tenant_guard(m, ep, design));
        // …and a public_read GET (#105) takes NO guard param at all, so it must
        // not count toward the CurrentUser import (mirror handler_params exactly,
        // or a public-read-only module emits an unused import).
        let needs_user = m.endpoints.iter().any(|ep| {
            ep.is_guarded()
                && !endpoint_uses_tenant_guard(m, ep, design)
                && !endpoint_is_public_read_get(m, ep, design)
        });
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
        let scope = tenant_scope_comment(m, ep, mode, design);
        let owner_scope = owner_scope_comment(m, ep, mode, design);
        let collection = tenant_collection_comment(m, ep, mode, design);
        let server_owned = server_owned_fk_comment(m, ep, mode, design);
        let realtime = realtime_publish_comment(ep, design);
        out.push_str(&format!(
            "pub(crate) async fn {op}({params}) -> {ret} {{\n{guard}{scope}{owner_scope}{collection}{server_owned}{realtime}{body}\n}}\n\n",
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
/// building the entity. Path-scoped handlers take `Dep<Tenant>` (no session
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
    // The DTO this endpoint's body deserializes into — `{Entity}Request` on create,
    // `{Entity}UpdateRequest` on a defaulted-entity update (issue #85 D1). Every
    // comment names the ACTUAL type the handler receives.
    let dto = request_dto_name(m, ep, mode, design).expect("endpoint_takes_request_dto ⇒ a DTO");
    let is_update = ep.method.is_update();
    let mut out = String::new();
    // Identity fk (#34): guarded + auth → the wire body omits `user_id`. The rule is
    // method-agnostic, but the STUB GUIDANCE is not (issue #42): a CREATE (POST)
    // injects the session user's id; an UPDATE (PUT/PATCH) must PRESERVE the existing
    // row's owner — reassigning it to the caller would let an admin editing another
    // user's row silently take ownership. So split the note by method.
    if mode.auth && design.endpoint_omits_identity_fk(m, ep) {
        let is_create = matches!(ep.method, HttpMethod::POST);
        // The stub wording references `_tenant` only when the handler actually has
        // that param — i.e. a path-scoped guard; a flat handler has `_user`.
        let tenant_owned = endpoint_uses_tenant_guard(m, ep, design);
        out.push_str(&match (is_create, tenant_owned) {
            (true, true) => format!(
                "    // server-owned fk: `{dto}` has NO `user_id` — the server injects the\n    // session user's id. Add a `user: CurrentUser` param and use `user.0.id` (the\n    // stringified user pk; parse it for an integer fk) when building the {entity}.\n"
            ),
            (true, false) => format!(
                "    // server-owned fk: `{dto}` has NO `user_id` — the server injects the\n    // session user's id. Use `_user.0.id` (the stringified user pk; parse it for an\n    // integer fk) when building the {entity}.\n"
            ),
            (false, true) => format!(
                "    // server-owned fk: `{dto}` has NO `user_id` — on UPDATE, PRESERVE the\n    // existing row's owner. Do NOT reassign `user_id`; scope the update through the\n    // membership (`_tenant`) so a non-owner can't take the row.\n"
            ),
            (false, false) => format!(
                "    // server-owned fk: `{dto}` has NO `user_id` — on UPDATE, PRESERVE the\n    // existing row's owner. Do NOT reassign `user_id` to `_user.0.id`; scope the\n    // UPDATE to the owner (e.g. WHERE user_id = _user.0.id) so a non-owner can't take it.\n"
            ),
        });
    }
    // Path-redundant parent fk (#53b): comes from the endpoint's own path param
    // (`_{col}`), so the handler injects it instead of reading it from the body.
    for col in design.entity_path_fk_columns(entity) {
        out.push_str(&format!(
            "    // path-owned fk: `{dto}` has NO `{col}` — inject the `_{col}` path\n    // value (the handler's Path param) when building the {entity}.\n"
        ));
    }
    // Defaults (#53a): on CREATE the DTO omits each default field and the handler
    // writes the declared value; on UPDATE the DTO KEEPS them (issue #85 D1), so the
    // handler must use the body value — resetting to the default would silently undo
    // an edit to a defaulted lifecycle field.
    if is_update {
        let names: Vec<String> = e
            .fields
            .iter()
            .filter(|f| f.default.is_some())
            .map(|f| format!("`{}`", f.name))
            .collect();
        if !names.is_empty() {
            out.push_str(&format!(
                "    // settable defaults: `{dto}` KEEPS {} — a `default` applies on CREATE only;\n    // on UPDATE use the request body's value (do NOT reset it to the default).\n",
                names.join(", ")
            ));
        }
    } else {
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
                "    // server-owned defaults: `{dto}` omits {} — set each to its\n    // declared default when building the {entity}.\n",
                defaults.join(", ")
            ));
        }
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
            out.push_str(&request_dto_rs(e, design, false));
        }
        // A defaulted entity with an UPDATE endpoint also gets `{Entity}UpdateRequest`
        // — the create DTO drops its `default` fields, but update must keep them so
        // a defaulted lifecycle enum stays settable after create (issue #85 D1).
        let needs_update_dto = e.fields.iter().any(|f| f.default.is_some())
            && m.endpoints.iter().any(|ep| {
                ep.method.is_update()
                    && design.endpoint_uses_request_dto(m, ep, auth)
                    && ep
                        .request_body
                        .as_ref()
                        .is_some_and(|rb| rb.entity == e.name)
            });
        if needs_update_dto {
            out.push_str(&request_dto_rs(e, design, true));
        }
    }
    out.push_str(&enum_deserialize_fns(&m.entities));
    Some(out)
}

/// The request DTO (issues #34 + #53 + #85): the entity's deserialization shape
/// MINUS every field the wire contract drops — the server-owned identity `user_id`
/// fk (#34) and a path-redundant parent fk (#53b) are always dropped. A `default`
/// field (#53a) is dropped on CREATE (`for_update = false`, `{Entity}Request` — the
/// server applies the value) but KEPT on UPDATE (`for_update = true`,
/// `{Entity}UpdateRequest` — a `default` is create-only, so an update must be able
/// to set it; issue #85 D1). Everything else mirrors the Model: the pk `id`
/// (synthetic → `#[serde(default)]`), the remaining fk columns (a SetNull fk is an
/// optional field with a serde default), then the declared fields with the same
/// optionality and keyword renames. Plain serde struct — only the Model touches SeaORM.
fn request_dto_rs(e: &Entity, design: &Design, for_update: bool) -> String {
    let entity = &e.name;
    let struct_name = if for_update {
        format!("{entity}UpdateRequest")
    } else {
        format!("{entity}Request")
    };
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
    // A `default` field (#53a) is server-owned on CREATE (dropped); on UPDATE it is
    // client-settable (kept), so `for_update` includes it (issue #85 D1).
    for f in e
        .fields
        .iter()
        .filter(|f| f.name != "id" && (for_update || f.default.is_none()))
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
    let doc = request_dto_doc(e, &path_fks, omit_identity, for_update);
    format!(
        "{doc}#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\npub struct {struct_name} {{\n{id_default}    pub id: {key},\n{fields}}}\n\n"
    )
}

/// The doc comment for a request DTO — one `///` line per dropped server-owned
/// field with its reason, so an agent reading the struct sees exactly which keys
/// the server supplies. Ordered identity fk → path fk → defaults (the struct-field
/// order the DTO omits them in). On UPDATE (`for_update`) the `default` fields are
/// KEPT (client-settable), so they are not listed as omitted; the doc notes they
/// are settable here, unlike on create (issue #85 D1).
fn request_dto_doc(
    e: &Entity,
    path_fks: &[String],
    omit_identity: bool,
    for_update: bool,
) -> String {
    let mut reasons = Vec::new();
    for b in &e.belongs_to {
        let col = Design::fk_column(&b.entity);
        if omit_identity && Design::is_identity_fk(b) {
            reasons.push(format!("`{col}` (the authenticated session user's id)"));
        } else if path_fks.contains(&col) {
            reasons.push(format!("`{col}` (from the request path)"));
        }
    }
    if for_update {
        // The `default` fields are KEPT here (settable on update). Only identity/
        // path fks stay server-owned; note them when present.
        let omitted = if reasons.is_empty() {
            "\n/// (none — every declared field is client input on update).".to_string()
        } else {
            format!(
                " These SERVER-OWNED fields\n/// are still omitted (the server supplies each):\n///   {}.",
                reasons.join(", ")
            )
        };
        return format!(
            "/// Update body for `{}` — like `{}Request` but KEEPS each `default` field, which\n/// is create-only server-owned yet settable on update (do NOT reset it here).{}\n",
            e.name, e.name, omitted
        );
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

/// Like `active_sets`, but the column named `pin_col` is written from `pin_expr`
/// (a path-verified value) instead of `item.{col}`. Used to pin the tenant fk to
/// the PATH param in path-scoped writes so the body cannot relocate the row (#125).
fn active_sets_pinning(e: &Entity, with_id: bool, pin_col: &str, pin_expr: &str) -> String {
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
            let ident = rust_ident(&name);
            if name == pin_col {
                out.push_str(&format!("{indent}{ident}: Set({pin_expr}),\n"));
            } else {
                out.push_str(&format!("{indent}{ident}: Set(item.{ident}),\n"));
            }
        }
    }
    out
}

/// True when a tenant-owned entity's routes are FLAT (MembershipSet) — none of its
/// endpoints carry the tenant fk in the path (the Supabase-migrated shape, and any
/// authored flat design). Such an entity takes the tenant fk from the request BODY on
/// a write, so that fk MUST be verified against the caller's membership set (RLS
/// `WITH CHECK`, spec §C, issue #94) — its repo therefore emits the membership-CHECKED
/// `create_for_memberships`/`update_for_memberships`/`remove_for_memberships`
/// accessors and its flat mutation stubs are steered to them. A PATH-SCOPED (nested)
/// tenant-owned entity is scoped by the verified path tenant instead (T2
/// `update_for`/`remove_for`), so it gets NONE of these and stays byte-identical.
/// Conservative: ANY path-scoped route on the entity ⇒ not flat (no mixed-shape
/// design exists today, and a path-scoped write is already covered).
pub(crate) fn entity_is_flat_tenant_owned(e: &Entity, design: &Design) -> bool {
    // Tenant-owned directly OR transitively (issue #102). `tenant_path` is `None`
    // when there is no tenancy, so it subsumes the old tenancy guard.
    if design.tenant_path(&e.name).is_none() {
        return false;
    }
    let Some(m) = design
        .modules
        .iter()
        .find(|m| m.entities.iter().any(|x| x.name == e.name))
    else {
        return false;
    };
    let mut flat = false;
    for ep in &m.endpoints {
        if endpoint_repo_entity(m, ep) != Some(e.name.as_str()) {
            continue;
        }
        match design.endpoint_tenant_shape(m, ep) {
            TenantShape::PathScoped { .. } => return false,
            TenantShape::MembershipSet => flat = true,
            _ => {}
        }
    }
    flat
}

/// Tenant-scoped accessors for an entity that belongs_to the design's tenancy
/// entity (empty otherwise). Keyed on the fk column so a tenant can only reach
/// its own rows: `all_for` filters the fk, `get_for` adds the id, `remove_for`
/// deletes on both. Param name = fk column; param type = the tenant pk type.
fn scoped_methods(e: &Entity, design: &Design) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    // Tenant-owned directly OR transitively (issue #102): a grandchild reached
    // through a parent chain gets the scoped accessors too. The JOIN SQL for a
    // grandchild's filter is Tasks 3/4; this only recognizes ownership.
    if design.tenant_path(&e.name).is_none() {
        return String::new();
    }
    let entity = &e.name;
    let snake = Design::to_snake(entity);
    let fk_col = Design::fk_column(&tenancy.entity);
    let fk_pascal = col_pascal(&fk_col);
    let fk_ty = design.target_key_rust_type(&tenancy.entity);
    let key = key_rust_type(e);
    // `update_for` pins the pk to the CHECKED PATH `id`, never the body `item.id`
    // (issue #92): a text pk is moved into the ownership-check query, so clone it there
    // and keep the owned `id` to `Set` below; an integer pk is `Copy` (empty suffix).
    let pk_clone = if key == "String" { ".clone()" } else { "" };
    // The membership-set filter (issues #78/#79): a FLAT tenant-owned route scopes
    // to the caller's memberships via raw SQL that mirrors the Supabase RLS policy
    // the migrator recognizes — `{fk} IN (SELECT {fk} FROM {tenant}_members WHERE
    // user_id = ?)`. `table`/`members` name the row table and the membership table.
    let table = design.table_name(entity);
    let members = format!("{}_members", Design::to_snake(&tenancy.entity));
    // The pk is `Set` from `item.id` (the synthetic pk also surfaces as a visible
    // `id` field), the rest from `item` (active_sets with the id line omitted) —
    // correct for both declared and synthetic primary keys, and `id` (the path
    // param) is consumed once by the ownership check, so no clone for text pks.
    let update_sets = active_sets(e, false);
    let insert_sets = active_sets(e, true);
    // The tenant path (issue #102): a direct child has empty joins (keep the typed
    // sea-orm builder verbatim — byte-identical to pre-#102); a grandchild+ JOINs
    // up its belongs_to chain to the anchor that carries the tenant fk. The gate
    // above guarantees `Some` here. Computed before the writes so the membership-
    // checked write branch can key on `path.joins` too (Task 4).
    let path = design.tenant_path(entity).expect("gate ensured Some");
    let join_sql = path.join_sql();
    let tenant_col = path.tenant_col();
    let tenant_fk = path.tenant_fk.as_str();
    // Membership-CHECKED writes for a FLAT tenant-owned entity (issue #94, spec §C).
    // A flat route takes the tenant fk from the BODY, so — unlike a path-scoped write,
    // where `Dep<Tenant>` already verified the path tenant — the body fk MUST be
    // verified against the caller's membership set (RLS `WITH CHECK`) before the write.
    // Emitted ONLY for a flat entity; a path-scoped (nested) entity stays byte-identical.
    // The membership-checked insert body is path-independent: it depends only on the
    // entity's own key type (a client-supplied text pk is captured up front and inserted
    // via `Entity::insert(..).exec`; an integer pk is DB-assigned and read back), so a
    // direct child and a transitive grandchild share it verbatim.
    let id_capture = if key == "String" {
        "        let id = item.id.clone();\n"
    } else {
        ""
    };
    let create_return_insert = if key == "String" {
        format!(
            "        {snake}::Entity::insert({snake}::ActiveModel {{\n{insert_sets}        }})\n        .exec(&txn)\n        .await\n        .map_err(db_error)?;\n        txn.commit().await.map_err(db_error)?;\n        Ok(id)"
        )
    } else {
        format!(
            "        let row = {snake}::ActiveModel {{\n{insert_sets}        }}\n        .insert(&txn)\n        .await\n        .map_err(db_error)?;\n        txn.commit().await.map_err(db_error)?;\n        Ok(row.id)"
        )
    };
    let membership_writes = if !entity_is_flat_tenant_owned(e, design) {
        // A path-scoped (nested) entity is scoped by the verified path tenant, so it
        // never gets these membership-checked writes (JL0006 / issue #78).
        String::new()
    } else if path.joins.is_empty() {
        // DIRECT child (byte-identical to pre-#102): the tenant fk is a real column on
        // the row, read straight from the body. The fk value is read out before the
        // insert consumes `item` — a text fk is cloned (also moved into the row), an
        // integer fk is Copy. The pk is pinned to the CHECKED PATH `id`, never `item.id`.
        let fk_clone = if fk_ty == "String" { ".clone()" } else { "" };
        format!(
            r#"
    // Membership-CHECKED writes for a FLAT tenant-owned route (issue #94, spec §C):
    // the tenant fk comes from the request BODY, so it is verified against the caller's
    // membership set (RLS `WITH CHECK`) — a create into a tenant the caller doesn't
    // belong to is 403; an update/delete of a row outside the set is 404; moving a row
    // across the tenant boundary is 403. A path-scoped (nested) entity is scoped by the
    // verified path tenant instead, so it never gets these (JL0006 / issue #78).
    pub async fn create_for_memberships(&self, user_id: String, item: {entity}) -> Result<{key}> {{
        use sea_orm::TransactionTrait;
{id_capture}        let tenant_fk = item.{fk_col}{fk_clone};
        let txn = self.db.conn().begin().await.map_err(db_error)?;
        // WITH CHECK: the body's tenant fk must be in the caller's membership set.
        if txn
            .query_one(sea_orm::Statement::from_sql_and_values(
                txn.get_database_backend(),
                self.db.sql(
                    "SELECT 1 FROM {members} WHERE user_id = ? AND {fk_col} = ? LIMIT 1",
                ),
                [user_id.into(), tenant_fk.into()],
            ))
            .await
            .map_err(db_error)?
            .is_none()
        {{
            return Err(Error::forbidden());
        }}
{create_return_insert}
    }}

    pub async fn update_for_memberships(&self, user_id: String, id: {key}, item: {entity}) -> Result<bool> {{
        // Load the row scoped to the caller's memberships: a row whose CURRENT tenant
        // is outside the set is invisible → false → 404 (no existence leak).
        let Some(existing) = self.get_for_memberships(user_id, id{pk_clone}).await? else {{
            return Ok(false);
        }};
        // WITH CHECK: forbid relocating the row to another tenant — the simplest safe
        // rule pins the tenant fk to its current value (a changed fk → 403).
        if item.{fk_col} != existing.{fk_col} {{
            return Err(Error::forbidden());
        }}
        // Pin the pk to the CHECKED PATH `id`, NOT `item.id`: the row authorized above
        // is `id`, so the UPDATE can only ever write that row (issue #92 — a body id
        // pointing at another tenant's row must not be reachable here).
        let m = {snake}::ActiveModel {{
            id: Set(id),
{update_sets}        }};
        match m.update(self.db.conn()).await {{
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(e) => Err(db_error(e)),
        }}
    }}

    pub async fn remove_for_memberships(&self, user_id: String, id: {key}) -> Result<bool> {{
        // Scope the DELETE to the membership set: a row whose tenant is outside the
        // set matches nothing → 0 rows → false → 404.
        let r = self
            .db
            .conn()
            .execute(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "DELETE FROM {table} WHERE id = ? AND {fk_col} IN (SELECT {fk_col} FROM {members} WHERE user_id = ?)",
                ),
                [id.into(), user_id.into()],
            ))
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected() > 0)
    }}
"#
        )
    } else {
        // TRANSITIVE grandchild+ (issue #102): the row carries NO tenant fk. Resolve the
        // tenant from the body's IMMEDIATE PARENT fk (a real column on the row) and JOIN
        // up the chain to the anchor that carries the tenant fk. Every identifier comes
        // from `TenantPath`/`belongs_to`; every value stays a bound `?` param.
        let parent_fk = &path.joins[0].child_fk;
        let parent_table = &path.joins[0].parent_table;
        // JOINs from the immediate parent up to the anchor (empty for a grandchild —
        // its immediate parent IS the anchor that carries the tenant fk).
        let parent_joins: String = path
            .joins
            .iter()
            .skip(1)
            .map(|j| {
                format!(
                    " JOIN {p} ON {c}.{fk} = {p}.id",
                    p = j.parent_table,
                    c = j.child_table,
                    fk = j.child_fk,
                )
            })
            .collect();
        // The parent fk value is read out of `item` before the insert consumes it — a
        // text fk must be cloned (also moved into the row); an integer fk is Copy. Its
        // type is the immediate parent entity's key type.
        let parent_entity = e
            .belongs_to
            .iter()
            .find(|b| Design::fk_column(&b.entity) == path.joins[0].child_fk)
            .map(|b| b.entity.as_str())
            .expect("the tenant path's first hop is a belongs_to of the entity");
        let parent_fk_clone = if design.target_key_rust_type(parent_entity) == "String" {
            ".clone()"
        } else {
            ""
        };
        format!(
            r#"
    // Membership-CHECKED writes for a FLAT tenant-owned GRANDCHILD (issue #102): the row
    // carries no tenant fk, so the WITH CHECK resolves the tenant from the body's
    // immediate parent fk and JOINs up to the anchor — a create under a parent outside
    // the caller's tenants is 403; an update/delete of a row outside the set is 404;
    // moving the row to another parent (a cross-tenant move) is 403.
    pub async fn create_for_memberships(&self, user_id: String, item: {entity}) -> Result<{key}> {{
        use sea_orm::TransactionTrait;
{id_capture}        let parent_fk = item.{parent_fk}{parent_fk_clone};
        let txn = self.db.conn().begin().await.map_err(db_error)?;
        // WITH CHECK: the body's parent must resolve to a tenant in the caller's set.
        if txn
            .query_one(sea_orm::Statement::from_sql_and_values(
                txn.get_database_backend(),
                self.db.sql(
                    "SELECT 1 FROM {parent_table}{parent_joins} WHERE {parent_table}.id = ? AND {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?) LIMIT 1",
                ),
                [parent_fk.into(), user_id.into()],
            ))
            .await
            .map_err(db_error)?
            .is_none()
        {{
            return Err(Error::forbidden());
        }}
{create_return_insert}
    }}

    pub async fn update_for_memberships(&self, user_id: String, id: {key}, item: {entity}) -> Result<bool> {{
        // Load the row scoped to the caller's memberships via the JOIN chain: a row whose
        // CURRENT tenant is outside the set is invisible → false → 404 (no existence leak).
        let Some(existing) = self.get_for_memberships(user_id, id{pk_clone}).await? else {{
            return Ok(false);
        }};
        // WITH CHECK: pin the immediate parent fk — a changed parent would move the row
        // to another parent (and possibly another tenant), so a changed fk → 403.
        if item.{parent_fk} != existing.{parent_fk} {{
            return Err(Error::forbidden());
        }}
        // Pin the pk to the CHECKED PATH `id`, NOT `item.id` (issue #92): the row
        // authorized above is `id`, so the UPDATE can only ever write that row.
        let m = {snake}::ActiveModel {{
            id: Set(id),
{update_sets}        }};
        match m.update(self.db.conn()).await {{
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(e) => Err(db_error(e)),
        }}
    }}

    pub async fn remove_for_memberships(&self, user_id: String, id: {key}) -> Result<bool> {{
        // Scope the DELETE to the membership set through the JOIN chain: a row whose
        // tenant is outside the set matches nothing → 0 rows → false → 404.
        let r = self
            .db
            .conn()
            .execute(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "DELETE FROM {table} WHERE id = ? AND id IN (SELECT {table}.id FROM {table}{join_sql} WHERE {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?))",
                ),
                [id.into(), user_id.into()],
            ))
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected() > 0)
    }}
"#
        )
    };
    // The four READS branch on the path: a direct child keeps the typed builder; a
    // transitive child emits a raw-SQL JOIN form scoping on the QUALIFIED tenant
    // column. Only identifiers from `TenantPath` reach the SQL; every value stays a
    // bound `?` param, wrapped in `self.db.sql(..)` exactly like the raw methods.
    let all_for_method = if path.joins.is_empty() {
        format!(
            r#"    pub async fn all_for(&self, {fk_col}: {fk_ty}) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .order_by_asc({snake}::Column::Id)
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    } else {
        format!(
            r#"    pub async fn all_for(&self, {fk_col}: {fk_ty}) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT {table}.* FROM {table}{join_sql} WHERE {tenant_col} = ? ORDER BY {table}.id",
                ),
                [{fk_col}.into()],
            ))
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    };
    let get_for_method = if path.joins.is_empty() {
        format!(
            r#"    pub async fn get_for(&self, {fk_col}: {fk_ty}, id: {key}) -> Result<Option<{entity}>> {{
        {snake}::Entity::find_by_id(id)
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .one(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    } else {
        format!(
            r#"    pub async fn get_for(&self, {fk_col}: {fk_ty}, id: {key}) -> Result<Option<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT {table}.* FROM {table}{join_sql} WHERE {table}.id = ? AND {tenant_col} = ?",
                ),
                [id.into(), {fk_col}.into()],
            ))
            .one(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    };
    let all_for_memberships_method = if path.joins.is_empty() {
        format!(
            r#"    pub async fn all_for_memberships(&self, user_id: String) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT * FROM {table} WHERE {fk_col} IN (SELECT {fk_col} FROM {members} WHERE user_id = ?) ORDER BY id",
                ),
                [user_id.into()],
            ))
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    } else {
        format!(
            r#"    pub async fn all_for_memberships(&self, user_id: String) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT {table}.* FROM {table}{join_sql} WHERE {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?) ORDER BY {table}.id",
                ),
                [user_id.into()],
            ))
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    };
    let get_for_memberships_method = if path.joins.is_empty() {
        format!(
            r#"    pub async fn get_for_memberships(&self, user_id: String, id: {key}) -> Result<Option<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT * FROM {table} WHERE id = ? AND {fk_col} IN (SELECT {fk_col} FROM {members} WHERE user_id = ?)",
                ),
                [id.into(), user_id.into()],
            ))
            .one(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    } else {
        format!(
            r#"    pub async fn get_for_memberships(&self, user_id: String, id: {key}) -> Result<Option<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT {table}.* FROM {table}{join_sql} WHERE {table}.id = ? AND {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?)",
                ),
                [id.into(), user_id.into()],
            ))
            .one(self.db.conn())
            .await
            .map_err(db_error)
    }}"#
        )
    };
    // The two PATH-SCOPED writes branch on the path exactly like the reads (issue #102):
    // a direct child keeps the typed sea-orm builder verbatim (byte-identical to
    // pre-#102); a grandchild+ scopes on the QUALIFIED tenant column reached via the JOIN
    // chain, using the PATH tenant id (`{fk_col}` param) instead of a membership subquery.
    let remove_for_method = if path.joins.is_empty() {
        format!(
            r#"    pub async fn remove_for(&self, {fk_col}: {fk_ty}, id: {key}) -> Result<bool> {{
        let r = {snake}::Entity::delete_many()
            .filter({snake}::Column::Id.eq(id))
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .exec(self.db.conn())
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected > 0)
    }}"#
        )
    } else {
        format!(
            r#"    pub async fn remove_for(&self, {fk_col}: {fk_ty}, id: {key}) -> Result<bool> {{
        // Scope the DELETE to the path tenant through the JOIN chain (issue #102): a row
        // whose tenant is not the path tenant matches nothing → 0 rows → false → 404.
        let r = self
            .db
            .conn()
            .execute(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "DELETE FROM {table} WHERE id = ? AND id IN (SELECT {table}.id FROM {table}{join_sql} WHERE {tenant_col} = ?)",
                ),
                [id.into(), {fk_col}.into()],
            ))
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected() > 0)
    }}"#
        )
    };
    // The DIRECT path-scoped `update_for` pins the tenant fk to the PATH param
    // (`{fk_col}`), never `item.{fk_col}`, so the request body cannot relocate the row
    // into another tenant (issue #125). The row is already authorized against the path
    // tenant, so writing the same fk back is a no-op on a legitimate request and a
    // make-impossible relocation on a hostile one — regardless of #82 keeping the fk in
    // the DTO. Only this direct branch needs it; the transitive branch carries no tenant
    // fk column on the row (it pins the immediate parent fk instead), and the membership
    // branches verify the body fk against the caller's set.
    let update_sets_pinned = active_sets_pinning(e, false, &fk_col, &fk_col);
    // The direct branch consumes `{fk_col}` twice: once in the ownership-check filter
    // (`.eq({fk_col})` moves a text tenant pk) and again in the pinned `Set({fk_col})`
    // below. Clone it into the filter for a text pk so the owned value survives for the
    // pin (issue #125 use-after-move — E0382); an integer pk is `Copy` (empty suffix, so
    // integer-pk output stays byte-identical).
    let pin_fk_clone = if fk_ty == "String" { ".clone()" } else { "" };
    let update_for_method = if path.joins.is_empty() {
        format!(
            r#"    pub async fn update_for(&self, {fk_col}: {fk_ty}, id: {key}, item: {entity}) -> Result<bool> {{
        // Scope the write to the tenant: only proceed if the row is already
        // theirs (a foreign or unknown id is a no-op, returning false → 404).
        if {snake}::Entity::find_by_id(id{pk_clone})
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}{pin_fk_clone}))
            .one(self.db.conn())
            .await
            .map_err(db_error)?
            .is_none()
        {{
            return Ok(false);
        }}
        // Pin the pk to the CHECKED PATH `id`, NOT `item.id` (issue #92): the row
        // authorized above is `id`, so the UPDATE can only ever write that row — a
        // body id pointing at another tenant's row must not be reachable here.
        let m = {snake}::ActiveModel {{
            id: Set(id),
{update_sets_pinned}        }};
        match m.update(self.db.conn()).await {{
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(e) => Err(db_error(e)),
        }}
    }}"#
        )
    } else {
        // The immediate parent fk (e.g. `account_id`): on a PATH-SCOPED grandchild the path
        // carries the tenant fk, NOT the parent fk, so the parent fk is not path-redundant —
        // it stays client-controllable and MUST be pinned, exactly like the membership
        // branch, or a member could relocate the row to another parent (issue #102).
        let parent_fk = &path.joins[0].child_fk;
        format!(
            r#"    pub async fn update_for(&self, {fk_col}: {fk_ty}, id: {key}, item: {entity}) -> Result<bool> {{
        // Scope the write to the path tenant via the transitive JOIN check (issue #102):
        // load through get_for — a row outside this tenant is a no-op → false → 404.
        let Some(existing) = self.get_for({fk_col}, id{pk_clone}).await? else {{
            return Ok(false);
        }};
        // WITH CHECK: pin the immediate parent fk — a changed parent would move the row to
        // another parent (and possibly another tenant), so a changed fk → 403. On a
        // path-scoped grandchild the parent fk is NOT path-redundant, so unlike the direct
        // child it must be pinned here (issue #102 cross-tenant write).
        if item.{parent_fk} != existing.{parent_fk} {{
            return Err(Error::forbidden());
        }}
        // Pin the pk to the CHECKED PATH `id`, NOT `item.id` (issue #92): the row
        // authorized above is `id`, so the UPDATE can only ever write that row.
        let m = {snake}::ActiveModel {{
            id: Set(id),
{update_sets}        }};
        match m.update(self.db.conn()).await {{
            Ok(_) => Ok(true),
            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),
            Err(e) => Err(db_error(e)),
        }}
    }}"#
        )
    };
    format!(
        r#"
    // Tenant-scoped accessors — handlers must use these for tenant-owned data (JL0006).
{all_for_method}

{get_for_method}

{remove_for_method}

{update_for_method}

    // Membership-set accessors (issues #78/#79) — a FLAT tenant-owned handler
    // (no tenant fk in its path) scopes to the CALLER'S memberships, so a user in
    // many tenants sees every tenant's rows they belong to and nothing outside the
    // set (the Supabase RLS shape, restored). `user_id` is the stringified session
    // user id (the membership table's `user_id` is TEXT). Raw SQL, entity-typed.
{all_for_memberships_method}

{get_for_memberships_method}
{membership_writes}"#
    )
}

/// Membership-lifecycle accessors for the TENANT entity's OWN repo (issue #78,
/// Task 3) — emitted only on the repo of the entity that DECLARES tenancy (empty
/// for every other entity and every non-tenancy design, so output stays
/// byte-identical there). Two methods make membership correct-by-construction:
///
/// - `create_with_membership(user_id, item)` inserts the tenant AND seeds the
///   creator into `{tenant}_members` as the FIRST declared `member_role`, in ONE
///   transaction — so a fresh tenant is never memberless and the membership-verified
///   guard admits the creator on the next request (the seed can't be dropped the way
///   a hand-written INSERT invited).
/// - `all_for_member(user_id)` lists ONLY the tenants the caller belongs to
///   (`JOIN {members} … WHERE user_id = ?`), never the unscoped `all()`.
///
/// The insert differs by pk type exactly as `insert` does (integer pk assigned by
/// the DB and read back from the row; client-supplied text pk known up front and
/// inserted via `Entity::insert(..).exec(..)`).
///
/// Issue #107 adds the member-management surface on the same repo — `members_of`
/// / `add_member` / `set_member_role` / `remove_member` / `count_admins` — real
/// SQL keyed on the path tenant fk, with role validation (422), the last-admin
/// guard (409), and duplicate adds surfacing UNIQUE as 409 via `db_error`. The
/// `{Tenant}Member` row struct + MEMBER_ROLES const live in `tenant_member_row`.
fn tenant_own_methods(e: &Entity, design: &Design) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    if e.name != tenancy.entity {
        return String::new();
    }
    let entity = &e.name;
    let snake = Design::to_snake(entity);
    let key = key_rust_type(e);
    let table = design.table_name(entity);
    let members = format!("{}_members", snake);
    let fk_col = Design::fk_column(&tenancy.entity);
    // The seed role is the FIRST declared member_role ("creator becomes organizer").
    // JC0548 guarantees a non-empty list at design time, so the `member` fallback is
    // dead code — kept (consistently with testgen/storagegen/openapi) only so an
    // unvalidated design handed straight to the generator cannot panic.
    let role = tenancy
        .member_roles
        .first()
        .map(String::as_str)
        .unwrap_or("member");
    let insert_sets = active_sets(e, true);
    // The membership INSERT is identical for both pk shapes; only the tenant insert
    // (and how its id is obtained) differs, mirroring `insert`. A text pk is cloned
    // into the bound value (it is also moved into the `Ok(id)` return); an integer
    // pk is `Copy`, so cloning it would trip `clippy::clone_on_copy` in the app.
    let id_bind = if key == "String" { "id.clone()" } else { "id" };
    let seed = format!(
        "        txn.execute(sea_orm::Statement::from_sql_and_values(\n\
         \x20           txn.get_database_backend(),\n\
         \x20           self.db.sql(\n\
         \x20               \"INSERT INTO {members} (user_id, {fk_col}, role) VALUES (?, ?, ?)\",\n\
         \x20           ),\n\
         \x20           [user_id.into(), {id_bind}.into(), \"{role}\".into()],\n\
         \x20       ))\n\
         \x20       .await\n\
         \x20       .map_err(db_error)?;"
    );
    let create_body = if key == "String" {
        format!(
            "    pub async fn create_with_membership(&self, user_id: String, item: {entity}) -> Result<{key}> {{\n\
             \x20       use sea_orm::TransactionTrait;\n\
             \x20       let id = item.id.clone();\n\
             \x20       let txn = self.db.conn().begin().await.map_err(db_error)?;\n\
             \x20       {snake}::Entity::insert({snake}::ActiveModel {{\n\
             {insert_sets}        }})\n\
             \x20       .exec(&txn)\n\
             \x20       .await\n\
             \x20       .map_err(db_error)?;\n\
             {seed}\n\
             \x20       txn.commit().await.map_err(db_error)?;\n\
             \x20       Ok(id)\n\
             \x20   }}"
        )
    } else {
        format!(
            "    pub async fn create_with_membership(&self, user_id: String, item: {entity}) -> Result<{key}> {{\n\
             \x20       use sea_orm::TransactionTrait;\n\
             \x20       let txn = self.db.conn().begin().await.map_err(db_error)?;\n\
             \x20       let row = {snake}::ActiveModel {{\n\
             {insert_sets}        }}\n\
             \x20       .insert(&txn)\n\
             \x20       .await\n\
             \x20       .map_err(db_error)?;\n\
             \x20       let id = row.id;\n\
             {seed}\n\
             \x20       txn.commit().await.map_err(db_error)?;\n\
             \x20       Ok(id)\n\
             \x20   }}"
        )
    };
    // Member-management surface (issue #107): every method keys on the PATH
    // tenant fk. A String (text-pk) fk is MOVED into the last statement's bound
    // values, so the earlier pre-check reads clone it; an integer fk is `Copy`
    // (a `.clone()` there would trip `clippy::clone_on_copy` in the app).
    let fkc = if key == "String" { ".clone()" } else { "" };
    let roles_msg = if tenancy.member_roles.is_empty() {
        "member".to_string()
    } else {
        tenancy.member_roles.join(", ")
    };
    let admin = role;
    format!(
        r#"
    // Membership lifecycle (issue #78) — the tenant entity's own repo. Create via
    // `create_with_membership` so a fresh tenant is never memberless; list via
    // `all_for_member` so a caller sees only the tenants they belong to.
{create_body}

    pub async fn all_for_member(&self, user_id: String) -> Result<Vec<{entity}>> {{
        {snake}::Entity::find()
            .from_raw_sql(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT c.* FROM {table} c JOIN {members} m ON m.{fk_col} = c.id WHERE m.user_id = ? ORDER BY c.id",
                ),
                [user_id.into()],
            ))
            .all(self.db.conn())
            .await
            .map_err(db_error)
    }}

    // Member management (issue #107) — the tool-owned member surface. Every
    // method keys on the PATH tenant fk the membership guard already verified.
    // `role` must be one of the declared MEMBER_ROLES (422 — no DB CHECK backs
    // the column); the last "{admin}" can never be removed or demoted (409), so a
    // tenant is never left admin-less; a duplicate add surfaces the
    // UNIQUE(user_id, {fk_col}) index as 409 via db_error — no second mapping here.
    pub async fn members_of(&self, fk: {key}) -> Result<Vec<{entity}Member>> {{
        {entity}Member::find_by_statement(sea_orm::Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql(
                "SELECT id, user_id, role FROM {members} WHERE {fk_col} = ? ORDER BY id",
            ),
            [fk.into()],
        ))
        .all(self.db.conn())
        .await
        .map_err(db_error)
    }}

    pub async fn add_member(&self, fk: {key}, user_id: String, role: String) -> Result<()> {{
        if !MEMBER_ROLES.contains(&role.as_str()) {{
            return Err(Error::unprocessable("role must be one of: {roles_msg}"));
        }}
        self.db
            .conn()
            .execute(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "INSERT INTO {members} (user_id, {fk_col}, role) VALUES (?, ?, ?)",
                ),
                [user_id.into(), fk.into(), role.into()],
            ))
            .await
            .map_err(db_error)?;
        Ok(())
    }}

    pub async fn set_member_role(&self, fk: {key}, user_id: String, role: String) -> Result<bool> {{
        if !MEMBER_ROLES.contains(&role.as_str()) {{
            return Err(Error::unprocessable("role must be one of: {roles_msg}"));
        }}
        let target = {entity}Member::find_by_statement(sea_orm::Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql(
                "SELECT id, user_id, role FROM {members} WHERE user_id = ? AND {fk_col} = ?",
            ),
            [user_id.clone().into(), fk{fkc}.into()],
        ))
        .one(self.db.conn())
        .await
        .map_err(db_error)?;
        if target.is_some_and(|m| m.role == "{admin}")
            && role != "{admin}"
            && self.count_admins(fk{fkc}).await? == 1
        {{
            return Err(Error::conflict("cannot demote the last {admin}"));
        }}
        let r = self
            .db
            .conn()
            .execute(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "UPDATE {members} SET role = ? WHERE user_id = ? AND {fk_col} = ?",
                ),
                [role.into(), user_id.into(), fk.into()],
            ))
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected() > 0)
    }}

    pub async fn remove_member(&self, fk: {key}, user_id: String) -> Result<bool> {{
        let target = {entity}Member::find_by_statement(sea_orm::Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql(
                "SELECT id, user_id, role FROM {members} WHERE user_id = ? AND {fk_col} = ?",
            ),
            [user_id.clone().into(), fk{fkc}.into()],
        ))
        .one(self.db.conn())
        .await
        .map_err(db_error)?;
        if target.is_some_and(|m| m.role == "{admin}")
            && self.count_admins(fk{fkc}).await? == 1
        {{
            return Err(Error::conflict("cannot remove the last {admin}"));
        }}
        let r = self
            .db
            .conn()
            .execute(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql("DELETE FROM {members} WHERE user_id = ? AND {fk_col} = ?"),
                [user_id.into(), fk.into()],
            ))
            .await
            .map_err(db_error)?;
        Ok(r.rows_affected() > 0)
    }}

    pub async fn count_admins(&self, fk: {key}) -> Result<i64> {{
        let row = self
            .db
            .conn()
            .query_one(sea_orm::Statement::from_sql_and_values(
                self.db.conn().get_database_backend(),
                self.db.sql(
                    "SELECT COUNT(*) AS n FROM {members} WHERE {fk_col} = ? AND role = ?",
                ),
                [fk.into(), "{admin}".into()],
            ))
            .await
            .map_err(db_error)?;
        match row {{
            Some(r) => r.try_get::<i64>("", "n").map_err(db_error),
            None => Ok(0),
        }}
    }}
"#
    )
}

/// The serializable member-row type + the baked `member_roles` const backing the
/// member-management surface (issue #107) — module-scope companions to
/// `tenant_own_methods`, emitted ONLY beside the tenancy entity's own repo
/// (empty everywhere else, so all other repo output stays byte-identical).
/// `MEMBER_ROLES[0]` is the admin role by convention; JC0548 guarantees a
/// non-empty, duplicate-free list at design time, so the `["member"]` fallback
/// is dead code mirroring the seed-role fallback above.
fn tenant_member_row(e: &Entity, design: &Design) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    if e.name != tenancy.entity {
        return String::new();
    }
    let entity = &e.name;
    let members = format!("{}_members", Design::to_snake(entity));
    let quoted = if tenancy.member_roles.is_empty() {
        "\"member\"".to_string()
    } else {
        tenancy
            .member_roles
            .iter()
            .map(|r| format!("\"{r}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        r#"/// Roles a `{members}` row may hold — design.json `tenancy.member_roles`. The
/// FIRST entry is the admin role: it gates member management and is protected
/// by the last-admin rule (jerrycan issue #107).
const MEMBER_ROLES: &[&str] = &[{quoted}];

/// One `{members}` row — the member-surface list/lookup shape (issue #107).
#[derive(serde::Serialize, sea_orm::FromQueryResult)]
pub struct {entity}Member {{
    pub id: i64,
    pub user_id: String,
    pub role: String,
}}

"#
    )
}

/// True when this entity is OWNER-scoped by the AUTHENTICATED USER (issue #79).
/// Such an entity's repo emits the owner-scoped
/// `all_for/get_for/remove_for/update_for` accessors and suppresses the unscoped
/// `all/get/remove/update`, so a cross-user read/delete cannot be written at all
/// — the #79 leak is made impossible by construction, not merely linted (a
/// `public_read` entity, #105, gets its unscoped READS back — see `sql_repo`).
/// The classifier itself lives on [`Design::entity_is_per_user_owned`] — the ONE
/// shared per-user predicate (#105 §F) — re-gated on `mode.auth` (generation
/// derives it from `wants_auth()`; a non-auth mode has no session to scope by).
/// db-mode only in effect (this is `sql_repo`'s concern; the memory-mode struct
/// has no fk columns to scope by, so memory output stays byte-identical).
fn entity_is_per_user_owned(e: &Entity, mode: GenMode, design: &Design) -> bool {
    mode.auth && design.entity_is_per_user_owned(e)
}

/// Owner-scoped accessors for a per-user identity-owned entity (issue #79) — keyed
/// on the fixed `user_id` fk column so a caller can only reach their OWN rows. The
/// exact four `*_for` methods a tenant-owned entity's `scoped_methods` emit, but
/// keyed on the identity fk (the session user) rather than the tenant fk, and
/// WITHOUT the membership-set variants (per-user has no membership table). Empty
/// unless the entity is per-user owned, so every other entity — tenant-owned,
/// non-auth, or non-identity — stays byte-identical. The param type is the identity
/// entity's key type (`i64` for an integer/synthetic user pk): the handler passes
/// the parsed `_user.0.id` (the stringified session user id), mirroring the #34
/// server-owned-fk guidance.
fn owner_scoped_methods(e: &Entity, mode: GenMode, design: &Design) -> String {
    if !entity_is_per_user_owned(e, mode, design) {
        return String::new();
    }
    let Some(identity) = e.belongs_to.iter().find(|b| Design::is_identity_fk(b)) else {
        return String::new();
    };
    let entity = &e.name;
    let snake = Design::to_snake(entity);
    let fk_col = Design::fk_column(&identity.entity);
    let fk_pascal = col_pascal(&fk_col);
    let fk_ty = design.target_key_rust_type(&identity.entity);
    let key = key_rust_type(e);
    let update_sets = active_sets(e, false);
    // `update_for` pins the pk to the CHECKED PATH `id`, never the body `item.id`
    // (issue #92): a text pk is moved into the ownership-check query, so clone it there
    // and keep the owned `id` to `Set` below; an integer pk is `Copy` (empty suffix).
    let pk_clone = if key == "String" { ".clone()" } else { "" };
    // A public_read entity (#105) keeps its unscoped READS (see `sql_repo`), so the
    // "unscoped … are NOT generated" claim would be wrong there — say what actually
    // holds: reads are public, WRITES must go through the owner-scoped accessors.
    let header = if design.entity_is_public_read(entity) {
        format!(
            "    // Owner-scoped accessors (issues #79/#105) — this {entity} is public_read: the\n    // unscoped all/get above serve the PUBLIC reads, but every WRITE must go through\n    // update_for/remove_for below, keyed on the session user (`_user.0.id`). The\n    // unscoped update/remove are NOT generated, so a cross-user write can't be\n    // written. `{fk_col}` is the session user's id."
        )
    } else {
        format!(
            "    // Owner-scoped accessors (issue #79) — this {entity} belongs to the authenticated\n    // user; handlers MUST scope to the session user (`_user.0.id`) via these. The\n    // unscoped all/get/remove/update are NOT generated, so a cross-user read/delete\n    // can't be written. `{fk_col}` is the session user's id."
        )
    };
    format!(
        r#"
{header}
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
        // Scope the write to the owner: only proceed if the row is already theirs
        // (a foreign or unknown id is a no-op, returning false → 404).
        if {snake}::Entity::find_by_id(id{pk_clone})
            .filter({snake}::Column::{fk_pascal}.eq({fk_col}))
            .one(self.db.conn())
            .await
            .map_err(db_error)?
            .is_none()
        {{
            return Ok(false);
        }}
        // Pin the pk to the CHECKED PATH `id`, NOT `item.id` (issue #92): the row
        // authorized above is `id`, so the UPDATE can only ever write that row — a
        // body id pointing at another user's row must not be reachable here.
        let m = {snake}::ActiveModel {{
            id: Set(id),
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

fn sql_repo(e: &Entity, design: &Design, mode: GenMode) -> String {
    let entity = &e.name;
    let snake = Design::to_snake(entity);
    let key = key_rust_type(e);
    let insert_sets = active_sets(e, true);
    let update_sets = active_sets(e, false);
    let scoped = scoped_methods(e, design);
    // Per-user (issue #79): a guarded identity-owned entity emits ONLY owner-scoped
    // accessors — the unscoped all/get/remove/update are suppressed so the leaky
    // call can't be written. Mutually exclusive with `scoped` (tenant-owned).
    // public_read (#105) is the THIRD state on top: the unscoped READS come back
    // (its GETs are public and serve every owner's rows) while the unscoped
    // update/remove stay suppressed — public read, owner write.
    let owner_scoped = owner_scoped_methods(e, mode, design);
    let per_user = !owner_scoped.is_empty();
    let public_read = per_user && design.entity_is_public_read(&e.name);
    // The tenant entity's OWN membership-lifecycle methods (empty for every other
    // entity); mutually exclusive with `scoped` (an entity can't be BOTH the tenant
    // and belong_to the tenant).
    let tenant_own = tenant_own_methods(e, design);
    // Module-scope companions to `tenant_own`: the `{Tenant}Member` row struct +
    // MEMBER_ROLES const (issue #107). Empty for every non-tenant entity.
    let member_row = tenant_member_row(e, design);
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
    // The unscoped read/write accessors. Suppressed for a per-user entity (#79) so
    // the leaky cross-user call cannot be written — only the owner-scoped `*_for`
    // methods remain — EXCEPT that a public_read entity (#105) keeps the unscoped
    // READS: its GET handlers are public and legitimately serve the whole
    // collection; its unscoped update/remove stay suppressed. `insert` is always
    // emitted (a create is scoped by the server-injected owner fk, not by the repo
    // method). Non-per-user output stays byte-identical:
    // `{reads}{insert_body}{writes}` reproduces the original layout.
    let unscoped_reads = format!(
        "    pub async fn all(&self) -> Result<Vec<{entity}>> {{\n        {snake}::Entity::find()\n            .order_by_asc({snake}::Column::Id)\n            .all(self.db.conn())\n            .await\n            .map_err(db_error)\n    }}\n\n    pub async fn get(&self, id: {key}) -> Result<Option<{entity}>> {{\n        {snake}::Entity::find_by_id(id)\n            .one(self.db.conn())\n            .await\n            .map_err(db_error)\n    }}\n\n"
    );
    let unscoped_writes = format!(
        "\n\n    pub async fn remove(&self, id: {key}) -> Result<bool> {{\n        let r = {snake}::Entity::delete_by_id(id)\n            .exec(self.db.conn())\n            .await\n            .map_err(db_error)?;\n        Ok(r.rows_affected > 0)\n    }}\n\n    pub async fn update(&self, id: {key}, item: {entity}) -> Result<bool> {{\n        let m = {snake}::ActiveModel {{\n            id: Set(id),\n{update_sets}        }};\n        match m.update(self.db.conn()).await {{\n            Ok(_) => Ok(true),\n            Err(sea_orm::DbErr::RecordNotUpdated) => Ok(false),\n            Err(e) => Err(db_error(e)),\n        }}\n    }}"
    );
    let (reads, writes) = match (per_user, public_read) {
        // Plain per-user (#79): everything unscoped is suppressed.
        (true, false) => (String::new(), String::new()),
        // public_read (#105): public reads, owner-only writes.
        (true, true) => (unscoped_reads, String::new()),
        // Not per-user: the original unscoped surface, byte-identical.
        _ => (unscoped_reads, unscoped_writes),
    };
    format!(
        r#"{member_row}pub struct {entity}Repo {{
    db: Db,
}}

/// DI factory — registered by the tool-owned lib.rs via `.provide_dep`.
pub(crate) async fn {snake}_repo(db: Dep<Db>) -> Result<{entity}Repo> {{
    Ok({entity}Repo {{ db: (*db).clone() }})
}}

// Stub handlers don't call the repo yet; remove this allow as you implement them.
#[allow(dead_code)]
impl {entity}Repo {{
{reads}{insert_body}{writes}
{scoped}{tenant_own}{owner_scoped}}}

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
    // …but only a DIRECT tenant-owned entity's accessors use the TYPED `.filter(..)`
    // builder — a TRANSITIVE (grandchild+) entity scopes entirely through raw JOIN SQL
    // (issue #102), so it needs `ConnectionTrait` (raw SQL) but NOT `ColumnTrait`/
    // `QueryFilter`. Keying the filter imports on `has_scoped` would leave them unused
    // for a transitive-only module, tripping `-D warnings` on generated code.
    let has_direct_scoped = m.entities.iter().any(|e| {
        design
            .tenant_path(&e.name)
            .is_some_and(|p| p.joins.is_empty())
    });
    // The TENANT entity's OWN repo (Task 3) also drives raw SQL — `create_with_membership`
    // and `all_for_member` call `get_database_backend()`/`execute()` — so it needs
    // `ConnectionTrait` even without the `.filter(..)` accessors a tenant-OWNED entity has.
    let has_tenant_own = m
        .entities
        .iter()
        .any(|e| !tenant_own_methods(e, design).is_empty());
    // A per-user identity-owned entity (issue #79) gets `.filter(Column::UserId.eq(..))`
    // owner-scoped accessors — so it needs `ColumnTrait`/`QueryFilter` (like a
    // tenant-owned entity) but NOT `ConnectionTrait` (it has no raw-SQL
    // membership-set method).
    let has_owner_scoped = m
        .entities
        .iter()
        .any(|e| !owner_scoped_methods(e, mode, design).is_empty());
    // Build the trait-import list from what the emitted methods actually use, in the
    // fixed alphabetical order the three pre-existing tiers used — so a
    // tenant-owned, pure-tenant, or plain module stays byte-identical, and a per-user
    // module gets `ColumnTrait`/`QueryFilter` WITHOUT the unused `ConnectionTrait`.
    let needs_filter = has_direct_scoped || has_owner_scoped; // ColumnTrait + QueryFilter
    let needs_conn = has_scoped || has_tenant_own; // ConnectionTrait (raw SQL)
    let mut imports = vec!["ActiveModelTrait", "ActiveValue::Set"];
    if needs_filter {
        imports.push("ColumnTrait");
    }
    if needs_conn {
        imports.push("ConnectionTrait");
    }
    imports.push("EntityTrait");
    if has_tenant_own {
        // `find_by_statement` (the typed `{Tenant}Member` rows, issue #107) is a
        // FromQueryResult trait method — only the tenant's own module needs it.
        imports.push("FromQueryResult");
    }
    if needs_filter {
        imports.push("QueryFilter");
    }
    imports.push("QueryOrder");
    let filter_imports = imports.join(", ");
    // The `use jerrycan::db::sea_orm;` alias resolves the bare `sea_orm::` paths
    // the repo writes (DbErr, ActiveValue::NotSet); the trait imports come through
    // the same facade so generated crates carry NO direct sea-orm dependency.
    let mut out = format!(
        "//! Data access — SeaORM over jerrycan::db (agent-owned; edit freely).\nuse jerrycan::db::sea_orm;\nuse jerrycan::db::sea_orm::{{{filter_imports}}};\nuse jerrycan::db::{{db_error, Db}};\nuse jerrycan::prelude::*;\n\nuse super::model::*;\n\n",
    );
    for e in &m.entities {
        out.push_str(&sql_repo(e, design, mode));
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

/// True when this module (or subroute) DECLARES the tenancy entity and the mode
/// carries both halves the member surface stands on: db (the `{tenant}_members`
/// table + the #107 repo methods live in the SQL repo only) and auth (the
/// membership-verifying `Dep<Tenant>` guard + `CurrentUser`). Gates every #107
/// emission site — mod decl, route lines, members.rs — so a non-tenancy design
/// (and every other module of a tenancy design) stays byte-identical.
fn emits_member_surface(m: &ModuleDesign, mode: GenMode, design: &Design) -> bool {
    mode.db
        && mode.auth
        && design
            .tenancy
            .as_ref()
            .is_some_and(|t| m.entities.iter().any(|e| e.name == t.entity))
}

/// The TOOL-OWNED member-management handlers (issue #107): `members.rs` beside
/// the tenant module's agent-owned handlers.rs — fully implemented (like
/// storagegen's bucket handlers, never agent stubs), calling the #107 repo
/// methods. `None` for every module that doesn't emit the surface.
///
/// Mounted path-scoped (`/{fk}/members…`), so the shared `Dep<Tenant>` guard
/// verifies membership in the PATH tenant and 404s outsiders before any handler
/// runs. Writes additionally require the admin role (`member_roles[0]`,
/// matching the seed-role convention above) via `tenant.require_role` — except
/// self-removal, where any member may leave (the guard already proved
/// membership; the repo's last-admin rule still applies).
pub(crate) fn members_rs(m: &ModuleDesign, mode: GenMode, design: &Design) -> Option<String> {
    if !emits_member_surface(m, mode, design) {
        return None;
    }
    let tenancy = design.tenancy.as_ref().expect("gated on tenancy");
    let entity = &tenancy.entity;
    let fk_col = Design::fk_column(entity);
    // The admin role: FIRST declared member_role, same fallback as the seed role
    // in `tenant_own_methods` (JC0548 guarantees non-empty roles at design time;
    // the dead fallbacks still must agree byte-for-byte).
    let admin = tenancy
        .member_roles
        .first()
        .map(String::as_str)
        .unwrap_or("member");
    Some(format!(
        r#"//! GENERATED by jerrycan — member management for the `{entity}` tenant
//! (issue #107). TOOL-OWNED: regenerated by `jerrycan generate`; do not
//! hand-edit (custom logic belongs in the agent-owned handlers.rs, not here).
//!
//! Every route here is path-scoped under `/{{{fk_col}}}`, so the shared
//! `Dep<Tenant>` guard verifies the caller's membership in the PATH tenant and
//! 404s outsiders before these handlers run. Writes additionally require the
//! admin role `{admin}` (`member_roles[0]`); the one exception is self-removal —
//! any member may leave. Role validation (422), duplicate adds (409), and the
//! last-admin lockout (409) live in the repo methods.
use super::repo::{{{entity}Member, {entity}Repo}};
use jerrycan::prelude::*;
use shared::{{CurrentUser, Tenant}};

/// POST /{{{fk_col}}}/members body. `user_id` is OPAQUE — no FK backs it
/// (migrated-uuid support), so existence is not DB-verified.
#[derive(serde::Deserialize)]
pub(crate) struct AddMemberRequest {{
    pub(crate) user_id: String,
    pub(crate) role: String,
}}

/// PATCH /{{{fk_col}}}/members/{{user_id}} body: the member's new role.
#[derive(serde::Deserialize)]
pub(crate) struct SetMemberRoleRequest {{
    pub(crate) role: String,
}}

/// GET /{{{fk_col}}}/members — the roster `[{{id, user_id, role}}]`. Any member
/// may read it: the guard is the whole gate.
pub(crate) async fn list_members(
    tenant: Dep<Tenant>,
    repo: Dep<{entity}Repo>,
) -> Result<Json<Vec<{entity}Member>>> {{
    Ok(Json(repo.members_of(tenant.id()).await?))
}}

/// POST /{{{fk_col}}}/members — add a member (201). Admin-gated; an out-of-set
/// role is 422, a duplicate membership 409 (the UNIQUE index via db_error).
pub(crate) async fn add_member(
    tenant: Dep<Tenant>,
    repo: Dep<{entity}Repo>,
    Json(body): Json<AddMemberRequest>,
) -> Result<Created<serde_json::Value>> {{
    tenant.require_role("{admin}")?;
    repo.add_member(tenant.id(), body.user_id.clone(), body.role.clone())
        .await?;
    Ok(Created(
        serde_json::json!({{ "user_id": body.user_id, "role": body.role }}),
    ))
}}

/// PATCH /{{{fk_col}}}/members/{{user_id}} — change a member's role (204).
/// Admin-gated; an out-of-set role is 422, demoting the last `{admin}` 409, an
/// unknown member 404.
pub(crate) async fn set_member_role(
    tenant: Dep<Tenant>,
    repo: Dep<{entity}Repo>,
    Path(user_id): Path<String>,
    Json(body): Json<SetMemberRoleRequest>,
) -> Result<NoContent> {{
    tenant.require_role("{admin}")?;
    if repo
        .set_member_role(tenant.id(), user_id, body.role)
        .await?
    {{
        Ok(NoContent)
    }} else {{
        Err(Error::not_found())
    }}
}}

/// DELETE /{{{fk_col}}}/members/{{user_id}} — remove a member (204).
/// Self-removal ("leave") needs no admin role — the guard already proved the
/// caller's membership; removing ANYONE ELSE is admin-gated. Removing the last
/// `{admin}` is 409, an unknown member 404.
pub(crate) async fn remove_member(
    tenant: Dep<Tenant>,
    user: CurrentUser,
    repo: Dep<{entity}Repo>,
    Path(user_id): Path<String>,
) -> Result<NoContent> {{
    if user_id != user.0.id {{
        tenant.require_role("{admin}")?;
    }}
    if repo.remove_member(tenant.id(), user_id).await? {{
        Ok(NoContent)
    }} else {{
        Err(Error::not_found())
    }}
}}
"#
    ))
}

fn module_body(m: &ModuleDesign, indent: &str, mode: GenMode, design: &Design) -> String {
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
    // The member-management surface (issue #107): tool-owned routes registered
    // AFTER the design's own — path-scoped on the tenant fk so the shared guard's
    // by-name lookup verifies membership in the PATH tenant. The fk param name
    // agrees with the tenant's own detail routes by construction
    // (`normalize_tenant_detail_routes` rewrote `{id}` → `{fk}` at load), so the
    // router's one-param-name-per-position rule holds.
    if emits_member_surface(m, mode, design) {
        let fk = Design::fk_column(&design.tenancy.as_ref().expect("gated").entity);
        body.push_str(&format!(
            "{indent}    .route(\"/{{{fk}}}/members\", get(members::list_members).post(members::add_member))\n{indent}    .route(\"/{{{fk}}}/members/{{user_id}}\", patch(members::set_member_role).delete(members::remove_member))\n"
        ));
    }
    for sub in &m.subroutes {
        body.push_str(&format!(
            "{indent}    .mount(\"{}\", subroutes::{}::module())\n",
            sub.effective_mount(),
            sub.name.replace('-', "_"),
        ));
    }
    body
}

fn mod_decls(m: &ModuleDesign, mode: GenMode, design: &Design) -> String {
    let mut out = String::from("mod deps;\nmod handlers;\n");
    // `members` (issue #107) sits between handlers and model: `mod` decls are
    // emitted in ALPHABETICAL order so rustfmt's `reorder_modules` is a no-op
    // (same reasoning as main.rs's module decls, issue #120).
    if emits_member_surface(m, mode, design) {
        out.push_str("mod members;\n");
    }
    if !m.entities.is_empty() {
        out.push_str("mod model;\nmod repo;\n");
    }
    if !m.subroutes.is_empty() {
        out.push_str("mod subroutes;\n");
    }
    out
}

pub(crate) fn lib_rs(m: &ModuleDesign, mode: GenMode, design: &Design) -> String {
    format!(
        "//! Route module `{name}` — TOOL-OWNED, regenerated by `jerrycan generate`.\n//! The sole public item is `module()`; agent code lives in handlers/model/repo/deps.\n#![forbid(unsafe_code)]\n\n{mods}\nuse jerrycan::prelude::*;\n\n/// Build this module's routes, subroutes, and scoped dependencies.\npub fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m, mode, design),
        body = module_body(m, "        ", mode, design),
    )
}

fn subroute_mod_rs(m: &ModuleDesign, mode: GenMode, design: &Design) -> String {
    format!(
        "//! Subroute `{name}` — TOOL-OWNED mod.rs; same fractal shape as a module.\n\n{mods}\nuse jerrycan::prelude::*;\n\npub(crate) fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m, mode, design),
        body = module_body(m, "        ", mode, design),
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
        "mod deps;" | "mod handlers;" | "mod members;" | "mod model;" | "mod repo;"
            | "mod subroutes;"
    ) || trimmed == "use jerrycan::prelude::*;"
        // members.rs (issue #107) — its two tool-emitted imports: the fixed
        // shared-guard line and the `use super::repo::{XMember, XRepo};` shape
        // (matched narrowly so an agent's own `use super::repo::…` in a lib.rs
        // is still recognized as agent wiring).
        || trimmed == "use shared::{CurrentUser, Tenant};"
        || (trimmed.starts_with("use super::repo::{")
            && trimmed.ends_with("Repo};")
            && trimmed.contains("Member, "))
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
        &lib_rs(m, mode, design),
        &mut created,
        &root,
        &mut dropped,
    )?;
    write_unit_files(&src, m, mode, design, &mut created, &root)?;
    write_members(&src, m, mode, design, &mut created, &root, &mut dropped)?;
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

/// Write — or, when the design no longer emits the surface (tenancy dropped,
/// entity moved) — REMOVE the tool-owned `members.rs` beside this module's
/// handlers (issue #107). Removal mirrors mounting's stale-crate cleanup: a
/// leftover would be unreferenced (`mod members;` is gone from lib.rs) but
/// would sit in the crate as dead tool output.
fn write_members(
    dir: &Path,
    m: &ModuleDesign,
    mode: GenMode,
    design: &Design,
    created: &mut Vec<String>,
    root: &Path,
    dropped: &mut DroppedDecls,
) -> Result<(), String> {
    let path = dir.join("members.rs");
    match members_rs(m, mode, design) {
        Some(content) => write_tool_owned(&path, &content, created, root, dropped),
        None => {
            if path.exists() {
                fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
                created.push(rel(&path, root));
            }
            Ok(())
        }
    }
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
            &subroute_mod_rs(sub, mode, design),
            created,
            root,
            dropped,
        )?;
        write_unit_files(&dir, sub, mode, design, created, root)?;
        write_members(&dir, sub, mode, design, created, root, dropped)?;
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

/// The implicit member-management routes (issue #107): the tool-owned
/// `/{fk}/members` + `/{fk}/members/{user_id}` pair `module_body` registers for
/// the tenant module, as mount-resolved `RouteEntry` rows (handlers named by
/// their OpenAPI operation ids). Deliberately NOT folded into `route_map`:
/// testgen and the OpenAPI emitter own their member-surface output separately,
/// so polluting the design-endpoint table would double-emit there. Design-time
/// conflict detection (`router_param_conflict`, JC0542) and the route listing DO
/// consume these, so a design endpoint colliding with the reserved paths fails
/// `check` instead of aborting `App::build` (JC0500), and `jerrycan routes`
/// shows the full live surface. Empty without tenancy (or without db/auth) —
/// the same `emits_member_surface` gate the generator runs.
pub fn implicit_member_routes(design: &Design) -> Vec<RouteEntry> {
    let mode = GenMode {
        db: design.wants_db(),
        auth: design.wants_auth(),
    };
    fn walk(
        m: &ModuleDesign,
        prefix: &str,
        top: &str,
        mode: GenMode,
        design: &Design,
        out: &mut Vec<RouteEntry>,
    ) {
        let base = format!("{}{}", prefix, m.effective_mount());
        if emits_member_surface(m, mode, design) {
            let tenancy = design.tenancy.as_ref().expect("gated on tenancy");
            let fk = Design::fk_column(&tenancy.entity);
            let snake = Design::to_snake(&tenancy.entity);
            let collection = format!("{}/{{{fk}}}/members", base.trim_end_matches('/'));
            let item = format!("{collection}/{{user_id}}");
            for (method, path, handler) in [
                ("GET", &collection, format!("list_{snake}_members")),
                ("POST", &collection, format!("add_{snake}_member")),
                ("PATCH", &item, format!("set_{snake}_member_role")),
                ("DELETE", &item, format!("remove_{snake}_member")),
            ] {
                out.push(RouteEntry {
                    method: method.to_string(),
                    path: path.clone(),
                    module: top.to_string(),
                    handler,
                });
            }
        }
        for sub in &m.subroutes {
            walk(sub, &base, top, mode, design, out);
        }
    }
    let mut out = Vec::new();
    for m in &design.modules {
        walk(m, "", &m.name, mode, design, &mut out);
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

    /// #107: `implicit_member_routes` is the conflict walker's + route lister's
    /// view of the tool-owned `/{fk}/members` surface `module_body` registers —
    /// mount-resolved rows for the tenant module ONLY, handlers named by their
    /// OpenAPI operation ids, and NONE without tenancy or without db/auth. It
    /// must stay OUT of `route_map` (testgen/OpenAPI own their member-surface
    /// emission), so a design that never emits the surface sees no rows at all.
    #[test]
    fn implicit_member_routes_mirror_the_registered_member_surface() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "clubs-api", "contract_version": 1,
            "auth": { "model": "session", "roles": ["owner", "member"] },
            "dependencies": ["db", "auth"],
            "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
            "modules": [{
                "name": "clubs",
                "entities": [{ "name": "Club", "fields": [
                    { "name": "id", "type": "integer" },
                    { "name": "name", "type": "string" } ]}],
                "endpoints": [
                    { "operation_id": "list_clubs", "method": "GET", "path": "/",
                      "auth_required": true,
                      "success": { "status": 200, "entity": "Club", "list": true } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let rows = implicit_member_routes(&d);
        let flat: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            flat,
            vec![
                ("GET", "/clubs/{club_id}/members"),
                ("POST", "/clubs/{club_id}/members"),
                ("PATCH", "/clubs/{club_id}/members/{user_id}"),
                ("DELETE", "/clubs/{club_id}/members/{user_id}"),
            ],
            "the four registered member routes, mount-resolved"
        );
        assert!(rows.iter().all(|r| r.module == "clubs"));
        assert_eq!(rows[0].handler, "list_club_members");
        assert_eq!(rows[3].handler, "remove_club_member");
        // The member routes never leak into the design-endpoint table.
        assert!(
            route_map(&d).iter().all(|r| !r.path.contains("/members")),
            "route_map stays design-endpoints-only"
        );
        // No tenancy (the MINIMAL demo): no surface.
        assert!(implicit_member_routes(&demo()).is_empty());
        // Memory mode (no `db` dependency): no members table, no surface.
        let mut mem = d.clone();
        mem.dependencies.retain(|dep| dep != "db");
        assert!(implicit_member_routes(&mem).is_empty());
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

    /// Issue #79 — MAKE THE PER-USER LEAK IMPOSSIBLE. A guarded entity that
    /// belongs_to the auth identity (Collection/Bookmark → `user_id`), in a
    /// non-tenancy auth design, gets ONLY the owner-scoped `*_for(user_id)`
    /// accessors — the unscoped `all()/get()/remove()/update()` are NOT generated,
    /// so a handler CANNOT write the leaky cross-user call (it would not compile).
    /// A non-identity entity (User itself) is UNCHANGED — it still has `all()`.
    /// WHY (Rule 9): the #79 leak had no backstop (no lint, no isolation test on
    /// the identity shape JC0540 steers toward); removing the leaky method makes
    /// the leak impossible by construction, not merely discouraged.
    #[test]
    fn per_user_identity_owned_entity_suppresses_unscoped_methods() {
        let d: Design = serde_json::from_str(SERVER_FK).unwrap();
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let collections = repo_rs(&d.modules[1], mode, &d).unwrap();
        // Collection belongs_to User (identity) → owner-scoped, unscoped suppressed.
        assert!(
            collections
                .contains("pub async fn all_for(&self, user_id: i64) -> Result<Vec<Collection>>"),
            "owner-scoped all_for keyed on user_id: {collections}"
        );
        assert!(
            collections.contains(
                "pub async fn get_for(&self, user_id: i64, id: i64) -> Result<Option<Collection>>"
            ),
            "owner-scoped get_for: {collections}"
        );
        assert!(
            collections.contains("pub async fn remove_for(&self, user_id: i64, id: i64)"),
            "owner-scoped remove_for: {collections}"
        );
        assert!(
            collections.contains(
                "pub async fn update_for(&self, user_id: i64, id: i64, item: Collection)"
            ),
            "owner-scoped update_for: {collections}"
        );
        assert!(
            collections.contains("Column::UserId.eq(user_id)"),
            "owner-scoped filter keys on the identity fk column: {collections}"
        );
        // The insert stays (a create is scoped by the server-injected owner fk).
        assert!(
            collections.contains("pub async fn insert(&self, item: Collection)"),
            "insert is retained: {collections}"
        );
        // The UNSCOPED leaky methods are GONE for Collection AND Bookmark.
        assert!(
            !collections.contains("pub async fn all(&self)"),
            "unscoped all() must NOT be generated for a per-user entity: {collections}"
        );
        assert!(
            !collections.contains("pub async fn get(&self, id: i64)"),
            "unscoped get() must NOT be generated: {collections}"
        );
        assert!(
            !collections.contains("pub async fn remove(&self, id: i64)"),
            "unscoped remove() must NOT be generated: {collections}"
        );
        assert!(
            !collections.contains("pub async fn update(&self, id: i64, item:"),
            "unscoped update() must NOT be generated: {collections}"
        );
        // Bookmark (belongs_to User + Collection) is also per-user → same treatment.
        assert!(
            collections
                .contains("pub async fn all_for(&self, user_id: i64) -> Result<Vec<Bookmark>>"),
            "Bookmark also owner-scoped: {collections}"
        );

        // A NON-identity entity (User — it doesn't belong_to User) is UNCHANGED:
        // it keeps its unscoped `all()` (the admin `list_users` legitimately lists
        // all users; that is not a per-user leak).
        let users = repo_rs(&d.modules[0], mode, &d).unwrap();
        assert!(
            users.contains("pub async fn all(&self) -> Result<Vec<User>>"),
            "a non-identity entity keeps unscoped all(): {users}"
        );
        assert!(
            !users.contains("pub async fn all_for("),
            "User is not owner-scoped: {users}"
        );
    }

    /// The public-read/owner-write feed design (issue #105): Post is a per-user
    /// identity-owned entity that opted into `public_read` (anyone reads, only the
    /// owner writes); Draft is the same shape WITHOUT the flag. `list_posts` is
    /// declared `auth_required` (the flag must override it); `get_post` is an
    /// unguarded detail read (legit ONLY because of the flag).
    pub(crate) const PUBLIC_READ: &str = r#"{
        "name": "feedapp",
        "contract_version": 1,
        "auth": { "model": "session", "roles": ["user"] },
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
            { "name": "posts",
              "entities": [
                  { "name": "Post", "public_read": true,
                    "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                    "fields": [{ "name": "title", "type": "string" }] },
                  { "name": "Draft",
                    "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                    "fields": [{ "name": "body", "type": "string" }] }
              ],
              "endpoints": [
                  { "operation_id": "list_posts", "method": "GET", "path": "/",
                    "auth_required": true,
                    "success": { "status": 200, "entity": "Post", "list": true } },
                  { "operation_id": "get_post", "method": "GET", "path": "/{id}",
                    "success": { "status": 200, "entity": "Post" } },
                  { "operation_id": "create_post", "method": "POST", "path": "/",
                    "auth_required": true,
                    "request_body": { "entity": "Post" },
                    "success": { "status": 201, "entity": "Post" } },
                  { "operation_id": "update_post", "method": "PUT", "path": "/{id}",
                    "auth_required": true,
                    "request_body": { "entity": "Post" },
                    "success": { "status": 200, "entity": "Post" } },
                  { "operation_id": "delete_post", "method": "DELETE", "path": "/{id}",
                    "auth_required": true,
                    "success": { "status": 204 } },
                  { "operation_id": "list_drafts", "method": "GET", "path": "/drafts",
                    "auth_required": true,
                    "success": { "status": 200, "entity": "Draft", "list": true } }
              ] }
        ]
    }"#;

    /// Issue #105 — the public-read/owner-write THIRD repo state. A `public_read`
    /// per-user entity (Post) gets the UNSCOPED `all()`/`get()` READS back (its
    /// GETs are public and serve every owner's rows) while WRITES stay owner-scoped
    /// ONLY: `update_for`/`remove_for` are kept and the unscoped `update`/`remove`
    /// stay suppressed, so a cross-user write still cannot compile. `insert` stays
    /// (a create is scoped by the server-injected owner fk). A NON-public_read
    /// sibling in the SAME module (Draft) keeps the full #79 suppression — the
    /// third state is reachable ONLY through the entity flag, so every existing
    /// per-user design is byte-identical. WHY (Rule 9): without this state the
    /// feed shape (#105) forced either a leaky handler or an unimplementable stub;
    /// keeping the write suppression is what stops "public read" from silently
    /// becoming "public write".
    #[test]
    fn public_read_entity_keeps_public_reads_and_owner_writes() {
        let d: Design = serde_json::from_str(PUBLIC_READ).unwrap();
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let src = repo_rs(&d.modules[1], mode, &d).unwrap();
        let (posts, drafts) = src
            .split_once("pub struct DraftRepo")
            .expect("both repos emitted");
        // Post (public_read): the unscoped READS are emitted…
        assert!(
            posts.contains("pub async fn all(&self) -> Result<Vec<Post>>"),
            "public_read emits the unscoped all(): {posts}"
        );
        assert!(
            posts.contains("pub async fn get(&self, id: i64) -> Result<Option<Post>>"),
            "public_read emits the unscoped get(): {posts}"
        );
        // …the writes stay owner-scoped only…
        assert!(
            posts.contains("pub async fn update_for(&self, user_id: i64, id: i64, item: Post)"),
            "owner-scoped update_for kept: {posts}"
        );
        assert!(
            posts.contains("pub async fn remove_for(&self, user_id: i64, id: i64)"),
            "owner-scoped remove_for kept: {posts}"
        );
        assert!(
            !posts.contains("pub async fn remove(&self, id: i64)"),
            "unscoped remove must stay suppressed on a public_read entity: {posts}"
        );
        assert!(
            !posts.contains("pub async fn update(&self, id: i64, item:"),
            "unscoped update must stay suppressed on a public_read entity: {posts}"
        );
        // …and the create keeps its server-scoped insert.
        assert!(
            posts.contains("pub async fn insert(&self, item: Post)"),
            "{posts}"
        );
        // Draft (NOT public_read, same module) keeps the full #79 suppression.
        assert!(
            !drafts.contains("pub async fn all(&self) -> Result<Vec<Draft>>"),
            "a non-public_read per-user entity keeps its reads suppressed: {drafts}"
        );
        assert!(
            drafts.contains("pub async fn all_for(&self, user_id: i64) -> Result<Vec<Draft>>"),
            "{drafts}"
        );
    }

    /// Issue #105 — the guarding split. A GET on a `public_read` entity takes NO
    /// `CurrentUser` — even when the design declares it `auth_required` (the entity
    /// flag drives it, correct-by-construction) — and its stub steers to the
    /// UNSCOPED `repo.all()`/`get(_id)` (the read is public; `all_for(_user.0.id)`
    /// would reference a param that no longer exists). Writes keep the guard AND
    /// the owner-scoped steering; a NON-public_read sibling GET in the same module
    /// keeps both its `CurrentUser` and the #79 owner-scope steer.
    #[test]
    fn public_read_gets_are_unguarded_and_writes_stay_guarded() {
        let d: Design = serde_json::from_str(PUBLIC_READ).unwrap();
        let h = handlers_rs(
            &d.modules[1],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        // The declared-guarded list GET loses its CurrentUser — the flag overrides.
        assert!(
            h.contains("pub(crate) async fn list_posts(_repo: Dep<PostRepo>) ->"),
            "public_read GET takes no CurrentUser even when auth_required: {h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn get_post(_repo: Dep<PostRepo>, Path(_id): Path<i64>) ->"
            ),
            "{h}"
        );
        // Writes keep the guard.
        assert!(
            h.contains("pub(crate) async fn create_post(_repo: Dep<PostRepo>, _user: CurrentUser,"),
            "a write on a public_read entity keeps its guard: {h}"
        );
        assert!(
            h.contains("pub(crate) async fn update_post(_repo: Dep<PostRepo>, _user: CurrentUser,"),
            "{h}"
        );
        assert!(
            h.contains("pub(crate) async fn delete_post(_repo: Dep<PostRepo>, _user: CurrentUser,"),
            "{h}"
        );
        // The non-public sibling GET keeps its guard.
        assert!(
            h.contains(
                "pub(crate) async fn list_drafts(_repo: Dep<DraftRepo>, _user: CurrentUser)"
            ),
            "a non-public_read GET keeps CurrentUser: {h}"
        );
        // Steering: public reads go to the UNSCOPED repo methods…
        let list_stub = h
            .split("async fn list_posts")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            list_stub.contains("PostRepo::all()"),
            "public list steers to the unscoped all(): {list_stub}"
        );
        assert!(
            !list_stub.contains("all_for"),
            "no owner-scope steer on a public read: {list_stub}"
        );
        let get_stub = h
            .split("async fn get_post")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            get_stub.contains("PostRepo::get(_id)"),
            "public detail steers to the unscoped get(): {get_stub}"
        );
        // …while the non-public sibling keeps the #79 owner-scope steer.
        let drafts_stub = h
            .split("async fn list_drafts")
            .nth(1)
            .unwrap()
            .split("async fn")
            .next()
            .unwrap();
        assert!(
            drafts_stub.contains("DraftRepo::all_for(_user.0.id)"),
            "the non-public sibling keeps the owner-scope steer: {drafts_stub}"
        );
    }

    /// A module whose ONLY guarded endpoints are public_read GETs emits no handler
    /// that takes `_user` — so `use shared::CurrentUser;` must NOT be emitted
    /// (an unused import trips `-D warnings` on freshly generated code).
    #[test]
    fn public_read_only_module_omits_the_current_user_import() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ).unwrap();
        let m = &mut d.modules[1];
        m.entities.retain(|e| e.name == "Post");
        m.endpoints
            .retain(|ep| matches!(ep.method, HttpMethod::GET) && ep.operation_id != "list_drafts");
        let h = handlers_rs(
            &d.modules[1],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        assert!(
            !h.contains("use shared::CurrentUser;"),
            "no handler takes _user, so the import must be dropped: {h}"
        );
        assert!(
            h.contains("async fn list_posts(_repo: Dep<PostRepo>)"),
            "{h}"
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

    /// Slice the `update_for` method body out of a generated repo (from its signature
    /// to the next `pub async fn`), so an `id: Set(...)` assertion targets `update_for`
    /// alone — `insert` legitimately carries `id: Set(item.id)` for a declared pk, and
    /// `update_for_memberships` is a distinct method. The `(` after `update_for`
    /// disambiguates it from `update_for_memberships`.
    fn update_for_body(repo: &str) -> String {
        let start = repo
            .find("pub async fn update_for(")
            .expect("repo has an update_for method");
        let rest = &repo[start..];
        let end = rest[1..]
            .find("    pub async fn ")
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// Issue #92 (body-id cross-scope WRITE): both the path-scoped tenant `update_for`
    /// and the per-user `update_for` authorize the row by the PATH `id` (the ownership
    /// check `find_by_id(id).filter(scope)`), so the write MUST target that same `id`,
    /// NEVER the client-controlled body `item.id`. A body id pointing at another
    /// tenant's (or user's) row would otherwise be UPDATEd after the check authorized
    /// only the caller's own row. Both families must pin `id: Set(id)` — mirroring the
    /// flat `update_for_memberships` fix (T6). RED while either emits `id: Set(item.id)`.
    #[test]
    fn update_for_pins_pk_to_the_checked_path_id_not_the_body_id() {
        let mode = GenMode {
            db: true,
            auth: true,
        };

        // Path-scoped tenant-owned entity (belongs_to the Workspace tenant).
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let scoped_update = update_for_body(&repo_rs(&d.modules[1], mode, &d).unwrap());
        assert!(
            scoped_update.contains("id: Set(id),"),
            "path-scoped update_for must pin the pk to the checked PATH id:\n{scoped_update}"
        );
        assert!(
            !scoped_update.contains("id: Set(item.id),"),
            "path-scoped update_for must NOT write the client-controlled body id (#92):\n{scoped_update}"
        );

        // Per-user owner-scoped entity (Collection belongs_to the identity User).
        let pu: Design = serde_json::from_str(SERVER_FK).unwrap();
        let owner_update = update_for_body(&repo_rs(&pu.modules[1], mode, &pu).unwrap());
        assert!(
            owner_update.contains("id: Set(id),"),
            "per-user update_for must pin the pk to the checked PATH id:\n{owner_update}"
        );
        assert!(
            !owner_update.contains("id: Set(item.id),"),
            "per-user update_for must NOT write the client-controlled body id (#92):\n{owner_update}"
        );
    }

    /// A tenant-owned entity ALSO gets MEMBERSHIP-SET accessors (issues #78/#79) so
    /// a FLAT route (no tenant fk in its path) scopes to the caller's memberships:
    /// `all_for_memberships(user_id)` lists across every tenant the user belongs to;
    /// `get_for_memberships(user_id, id)` bounds by the row id AND the set (404
    /// outside it). The filter is the Supabase RLS shape — `{fk} IN (SELECT {fk} FROM
    /// {members} WHERE user_id = ?)` — emitted as raw SQL so a multi-membership user
    /// is served faithfully, not flattened to one arbitrary tenant.
    #[test]
    fn tenant_owned_entities_get_membership_set_methods() {
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
            src.contains(
                "pub async fn all_for_memberships(&self, user_id: String) -> Result<Vec<Lead>>"
            ),
            "{src}"
        );
        assert!(
            src.contains(
                "pub async fn get_for_memberships(&self, user_id: String, id: i64) -> Result<Option<Lead>>"
            ),
            "{src}"
        );
        assert!(
            src.contains(
                "workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id"
            ),
            "membership-set filter is the RLS subquery: {src}"
        );
        // A flat get is bounded by the row id AND the membership set (404 outside it).
        assert!(
            src.contains(
                "WHERE id = ? AND workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?)"
            ),
            "{src}"
        );
    }

    /// Org (tenant) ; Account belongs_to Org ; Contact belongs_to Account — the
    /// TRANSITIVE (grandchild) tenant-ownership chain (#102). Contacts flat at
    /// `/contacts`, so the resolved path carries no tenant fk.
    const ORG_ACCOUNT_CONTACT: &str = r#"{ "name": "org-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "orgs",
              "entities": [{ "name": "Org", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Org", "list": true } }] },
            { "name": "accounts",
              "entities": [{ "name": "Account",
                  "belongs_to": [{ "entity": "Org" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "name", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_accounts", "method": "GET", "path": "/", "auth_required": true,
                  "success": { "status": 200, "entity": "Account", "list": true } }] },
            { "name": "contacts",
              "entities": [{ "name": "Contact",
                  "belongs_to": [{ "entity": "Account" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "email", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_contacts", "method": "GET", "path": "/", "auth_required": true,
                  "success": { "status": 200, "entity": "Contact", "list": true } }] }
        ] }"#;

    /// Same chain, but Contacts are mounted under their PARENT at
    /// `/accounts/{account_id}` — the resolved path carries `account_id`, NOT the
    /// tenant fk `org_id`. This is a FLAT (MembershipSet) grandchild route.
    const ORG_ACCOUNT_CONTACT_UNDER_PARENT: &str = r#"{ "name": "org-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "orgs",
              "entities": [{ "name": "Org", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Org", "list": true } }] },
            { "name": "accounts",
              "entities": [{ "name": "Account",
                  "belongs_to": [{ "entity": "Org" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "name", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_accounts", "method": "GET", "path": "/", "auth_required": true,
                  "success": { "status": 200, "entity": "Account", "list": true } }] },
            { "name": "contacts", "mount": "/accounts/{account_id}",
              "entities": [{ "name": "Contact",
                  "belongs_to": [{ "entity": "Account" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "email", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_contacts", "method": "GET", "path": "/", "auth_required": true,
                  "success": { "status": 200, "entity": "Contact", "list": true } }] }
        ] }"#;

    /// Same chain, but Contacts are mounted directly under the TENANT at
    /// `/orgs/{org_id}` — the resolved path carries the tenant fk, so this
    /// grandchild route is PATH-SCOPED.
    const ORG_ACCOUNT_CONTACT_UNDER_TENANT: &str = r#"{ "name": "org-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "orgs",
              "entities": [{ "name": "Org", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Org", "list": true } }] },
            { "name": "accounts",
              "entities": [{ "name": "Account",
                  "belongs_to": [{ "entity": "Org" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "name", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_accounts", "method": "GET", "path": "/", "auth_required": true,
                  "success": { "status": 200, "entity": "Account", "list": true } }] },
            { "name": "contacts", "mount": "/orgs/{org_id}",
              "entities": [{ "name": "Contact",
                  "belongs_to": [{ "entity": "Account" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "email", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_contacts", "method": "GET", "path": "/", "auth_required": true,
                  "success": { "status": 200, "entity": "Contact", "list": true } }] }
        ] }"#;

    /// Slice out a single generated method body — from its `pub async fn {name}(` to the
    /// next method (or the end of the emitted block) — so a bound-value assertion is scoped
    /// to THAT method. An identical `[id.into(), user_id.into()]` in a sibling method would
    /// otherwise mask a swapped bound order in the method under test (Rule 9). The trailing
    /// `(` in the signature disambiguates `remove_for` from `remove_for_memberships`.
    fn method_body<'a>(src: &'a str, name: &str) -> &'a str {
        let sig = format!("pub async fn {name}(");
        let start = src
            .find(&sig)
            .unwrap_or_else(|| panic!("method `{name}` not found in:\n{src}"));
        let rest = &src[start..];
        match rest[sig.len()..].find("\n    pub async fn ") {
            Some(i) => &rest[..sig.len() + i],
            None => rest,
        }
    }

    /// Recognition is transitive (#102): a GRANDCHILD entity (Contact belongs_to
    /// Account belongs_to the tenant Org) is now tenant-owned, so `scoped_methods`
    /// emits its tenant-scoped accessors instead of nothing. Pre-#102 the direct-only
    /// gate saw Contact belongs_to Account (not Org) and returned an empty string —
    /// the grandchild's repo had NO tenant scoping (the transitive leak). This locks
    /// RECOGNITION only; the JOIN SQL for the grandchild is Tasks 3/4.
    #[test]
    fn grandchild_entity_gets_scoped_methods() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let contact = d.find_entity("Contact").unwrap();
        assert!(
            !scoped_methods(contact, &d).is_empty(),
            "grandchild Contact must get scoped methods (was empty pre-#102)"
        );
    }

    /// Task 3 (issue #102): the grandchild's tenant-scoped READS join UP the
    /// belongs_to chain to the tenant fk instead of filtering a non-existent
    /// direct column. `all_for_memberships` scopes the outer predicate to the
    /// caller's memberships on the QUALIFIED tenant column, reached via the JOIN.
    #[test]
    fn grandchild_all_for_memberships_joins_to_tenant() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
        assert!(
            src.contains("JOIN accounts ON contacts.account_id = accounts.id"),
            "grandchild read must JOIN up to the tenant anchor:\n{src}"
        );
        assert!(
            src.contains(
                "WHERE accounts.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?)"
            ),
            "membership-set read must scope the QUALIFIED tenant col to the caller's set:\n{src}"
        );
    }

    /// Byte-identity backstop (issue #102): a DIRECT child (Book belongs_to the
    /// tenant Club) MUST keep the TYPED sea-orm builder verbatim — no JOIN, no
    /// raw-SQL read. The safety net proving the transitive rewrite never touches
    /// direct children (their emitted read bodies are identical to pre-#102).
    #[test]
    fn direct_child_reads_are_byte_identical() {
        let d: Design = serde_json::from_str(CLUBS_TENANCY).unwrap();
        let src = scoped_methods(d.find_entity("Book").unwrap(), &d);
        assert!(
            src.contains(".filter(book::Column::ClubId.eq(club_id))"),
            "direct child keeps the typed builder verbatim:\n{src}"
        );
        assert!(
            !src.contains(" JOIN "),
            "direct child must never emit a JOIN:\n{src}"
        );
    }

    /// Cross-tenant WRITE relocation (issue #125): on a direct path-scoped route
    /// (`/clubs/{club_id}/books/{id}`) the row is authorized against the PATH tenant,
    /// but the ActiveModel must also WRITE the tenant fk from the PATH param — never
    /// from `item.club_id` (the request body). Combined with #82 (the fk is retained
    /// in the DTO on mount-based nesting), a body `club_id` would let a member of club
    /// A relocate their row into club B. Pinning the fk to the path param makes that
    /// relocation impossible regardless of #82.
    #[test]
    fn direct_path_scoped_update_for_pins_tenant_fk_to_path_not_body() {
        let d: Design = serde_json::from_str(CLUBS_TENANCY).unwrap();
        // Book is the direct path-scoped child (belongs_to the tenant Club, mounted at
        // `/clubs/{club_id}`) — the same entity the read byte-identity test uses.
        let src = scoped_methods(d.find_entity("Book").unwrap(), &d);
        // the path-scoped update_for's ActiveModel must Set the tenant fk from the `club_id`
        // PATH PARAM, never from `item.club_id` (issue #125 cross-tenant relocation).
        assert!(
            src.contains("club_id: Set(club_id)"),
            "path-scoped update_for must pin the tenant fk to the path param; body:\n{src}"
        );
        assert!(
            !src.contains("club_id: Set(item.club_id)"),
            "path-scoped update_for must NOT write the tenant fk from the request body (#125)"
        );
    }

    /// Text-pk tenant use-after-move (issue #125 follow-up): when the tenant's own
    /// pk is a TEXT type (uuid/string/datetime), the tenant fk param (`team_id:
    /// String`) is consumed TWICE in the direct path-scoped `update_for` — once by
    /// the ownership-check filter (`.eq(team_id)` MOVES a `String`) and again by the
    /// pinned `team_id: Set(team_id)` in the ActiveModel (issue #125 fk pin). Without
    /// a clone that is a use-after-move (E0382) and the generated crate won't compile.
    /// The filter must clone the fk for a text pk so the owned value survives for the
    /// pin. An integer tenant pk is `Copy`, so its output stays byte-identical (the
    /// Club/Book fixtures, whose tenant pk is an integer, prove that half).
    #[test]
    fn direct_path_scoped_update_for_clones_text_tenant_fk_for_the_pin() {
        let d: Design = serde_json::from_str(TEAM_DOC_TEXT_PK).unwrap();
        // Doc is the direct path-scoped child of the TEXT-pk tenant Team (mounted at
        // `/teams/{team_id}`), so its tenant fk param `team_id` is a `String`.
        let src = scoped_methods(d.find_entity("Doc").unwrap(), &d);
        // The ownership-check filter must CLONE the String fk so the owned `team_id`
        // survives for the pinned `Set(team_id)` below — else the generated code moves
        // `team_id` twice (E0382) and won't compile.
        assert!(
            src.contains(".filter(doc::Column::TeamId.eq(team_id.clone()))"),
            "text-pk tenant fk must be cloned into the ownership filter (issue #125 use-after-move):\n{src}"
        );
        // The pin still Sets the fk from the PATH param (never `item.team_id`).
        assert!(
            src.contains("team_id: Set(team_id)"),
            "update_for must still pin the tenant fk to the path param:\n{src}"
        );
    }

    /// A nested-mount tenant child created at `POST /` (BookClubs shape, but with a
    /// create endpoint). The tenant fk `club_id` lives in the module MOUNT
    /// (`/clubs/{club_id}`), not `ep.path` (`/`). Issue #82 + the #125 CREATE vector:
    /// once the path-redundancy check is MOUNT-aware, `club_id` is recognized as
    /// path-owned, so the generated `BookRequest` DROPS it (a client can no longer
    /// POST a foreign `club_id` into another tenant — the server injects the
    /// path/tenant value) and the create handler carries the #53b inject-from-path
    /// steering. A mount-BLIND check kept `club_id` in the body (the #82 friction).
    const CLUBS_TENANCY_CREATE: &str = r#"{ "name": "clubs-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "clubs",
              "entities": [{ "name": "Club", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "create_club", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Club" },
                    "success": { "status": 201, "entity": "Club" } } ] },
            { "name": "books", "mount": "/clubs/{club_id}",
              "entities": [{ "name": "Book",
                  "belongs_to": [{ "entity": "Club" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "title", "type": "string" }] }],
              "endpoints": [
                  { "operation_id": "create_book", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Book" },
                    "success": { "status": 201, "entity": "Book" } } ] }
        ] }"#;

    #[test]
    fn nested_mount_create_drops_tenant_fk_from_dto_and_steers_injection() {
        let d: Design = serde_json::from_str(CLUBS_TENANCY_CREATE).unwrap();
        let book = d.find_entity("Book").unwrap();
        // The DTO omits the MOUNT-carried tenant fk (#82 / #125-create): a client body
        // can't carry a foreign `club_id` because the field no longer exists on the wire.
        let dto = request_dto_rs(book, &d, false);
        // The struct FIELD is gone (the doc comment still names it as a documented
        // omission — that is the intended output, so match the field declaration).
        assert!(
            !dto.contains("pub club_id:"),
            "BookRequest must drop the mount-carried tenant fk `club_id` (#82/#125-create):\n{dto}"
        );
        assert!(
            dto.contains("pub title:"),
            "non-fk declared fields stay on the DTO:\n{dto}"
        );
        // The create handler takes the trimmed DTO and carries the inject-from-path
        // steering (#53b), so the agent injects the path/tenant value, never a body fk.
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let books = handlers_rs(&d.modules[1], mode, &d);
        assert!(
            books.contains("Json(_body): Json<BookRequest>"),
            "create_book takes the trimmed DTO, not Json<Book>:\n{books}"
        );
        // #125-create closure: the DTO can no longer carry a foreign `club_id`, and the
        // create handler holds the membership-verified `Dep<Tenant>` as the ONLY tenant
        // source — the server injects the path tenant, never a client-supplied fk.
        assert!(
            books.contains(
                "pub(crate) async fn create_book(_repo: Dep<BookRepo>, _tenant: Dep<Tenant>, Json(_body): Json<BookRequest>)"
            ),
            "path-scoped create carries Dep<Tenant> (the injection source) + the trimmed DTO:\n{books}"
        );
        assert!(
            books.contains(
                "// path-owned fk: `BookRequest` has NO `club_id` — inject the `_club_id` path"
            ),
            "create handler carries the #53b inject-from-path steering:\n{books}"
        );
    }

    /// Task 4 (issue #102): a grandchild's membership-CHECKED create resolves the tenant
    /// from the BODY's immediate parent fk (a real column) and JOINs up to the anchor —
    /// it can NOT filter a non-existent direct `org_id` column. The WITH CHECK proves the
    /// parent belongs to a tenant in the caller's membership set (403 otherwise).
    #[test]
    fn grandchild_create_verifies_parent_resolves_to_member_tenant() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
        assert!(
            src.contains("SELECT 1 FROM accounts WHERE accounts.id = ? AND accounts.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?)"),
            "grandchild WITH CHECK resolves the tenant via the parent fk, not a direct column:\n{src}"
        );
        assert!(
            src.contains("let parent_fk = item.account_id"),
            "the body's immediate parent fk is read out for the CHECK:\n{src}"
        );
        // Rule 9: pin the bound-value ORDER, not just the SQL text — a swapped
        // `[user_id.into(), parent_fk.into()]` would bind the parent id where the SQL
        // expects user_id (and vice-versa), a silent cross-tenant hole the SQL-only
        // assert above cannot catch. `parent_fk.into()` is unique to this method.
        assert!(
            method_body(&src, "create_for_memberships")
                .contains("[parent_fk.into(), user_id.into()]"),
            "create binds [parent_fk, user_id] in that exact order:\n{src}"
        );
    }

    /// Task 4 (issue #102): a grandchild's membership-CHECKED update pins the IMMEDIATE
    /// PARENT fk (the safe generalization of the direct "pin the tenant fk" rule) — a
    /// changed parent → 403, which blocks moving the row across the tenant boundary.
    #[test]
    fn grandchild_update_pins_parent_fk() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
        assert!(
            src.contains("if item.account_id != existing.account_id"),
            "grandchild update pins the immediate parent fk:\n{src}"
        );
    }

    /// Task 4 (issue #102): a grandchild's membership-CHECKED delete scopes through the
    /// JOIN chain — a self-referential subquery joins up to the anchor and keeps the row
    /// only if its tenant is in the caller's set (0 rows → false → 404 outside the set).
    #[test]
    fn grandchild_remove_deletes_via_membership_subquery() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
        assert!(
            src.contains("DELETE FROM contacts WHERE id = ? AND id IN (SELECT contacts.id FROM contacts JOIN accounts ON contacts.account_id = accounts.id WHERE accounts.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?))"),
            "grandchild delete scopes through the JOIN chain to the membership set:\n{src}"
        );
        // Rule 9: pin the bound-value ORDER within THIS method — `get_for_memberships`
        // binds the same `[id.into(), user_id.into()]`, so scope the assert to the
        // `remove_for_memberships` body or a swap here would pass on the sibling's copy.
        assert!(
            method_body(&src, "remove_for_memberships").contains("[id.into(), user_id.into()]"),
            "remove_for_memberships binds [id, user_id] in that exact order:\n{src}"
        );
    }

    /// Task 4 (issue #102): the PATH-SCOPED writes (`remove_for`/`update_for`) of a
    /// grandchild scope on the QUALIFIED tenant column reached via the JOIN chain, using
    /// the PATH tenant id — they can NOT filter a non-existent direct `org_id` column, so
    /// a transitive repo now COMPILES. `update_for` loads through the transitive `get_for`.
    #[test]
    fn grandchild_path_scoped_writes_join_to_tenant() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
        assert!(
            src.contains("DELETE FROM contacts WHERE id = ? AND id IN (SELECT contacts.id FROM contacts JOIN accounts ON contacts.account_id = accounts.id WHERE accounts.org_id = ?)"),
            "path-scoped remove_for scopes on the qualified tenant col via the JOIN:\n{src}"
        );
        // Rule 9: pin the bound-value ORDER within the path-scoped `remove_for` — the
        // transitive `get_for` binds the same `[id.into(), org_id.into()]`, so scope the
        // assert to the `remove_for` body or a swap here would pass on the sibling's copy.
        assert!(
            method_body(&src, "remove_for").contains("[id.into(), org_id.into()]"),
            "path-scoped remove_for binds [id, org_id] in that exact order:\n{src}"
        );
        assert!(
            src.contains("let Some(existing) = self.get_for(org_id, id).await? else"),
            "path-scoped update_for loads the existing row through the transitive get_for:\n{src}"
        );
        // The grandchild must NEVER reference a direct tenant-fk column that doesn't exist.
        assert!(
            !src.contains("contact::Column::OrgId") && !src.contains("Column::OrgId"),
            "a grandchild has no direct org_id column — must not reference it:\n{src}"
        );
    }

    /// FIX 1 / issue #102 (cross-tenant WRITE): the PATH-SCOPED transitive `update_for`
    /// of a grandchild mounted UNDER THE TENANT (`/orgs/{org_id}/contacts/{id}`) must pin
    /// the immediate parent fk. The path carries `org_id`, not `account_id`, so
    /// `account_id` stays client-controllable and `active_sets` emits `account_id:
    /// Set(item.account_id)` unpinned — without this guard a member of org A could
    /// `PUT /orgs/{A}/contacts/{id}` with body `account_id = <an account in org B>` to
    /// relocate the row into org B. Under-tenant → PathScoped → NO membership writes are
    /// emitted, so the guard string can ONLY come from the path-scoped `update_for`: this
    /// test fails outright if the pin is dropped (it would otherwise hide behind the
    /// membership branch's identical guard on a flat design).
    #[test]
    fn path_scoped_transitive_update_for_pins_parent_fk() {
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT_UNDER_TENANT).unwrap();
        let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
        // Sanity: path-scoped → the membership-checked writes (which ALSO pin the parent
        // fk) are not emitted, so the guard asserted below is unambiguously the one under
        // test — the path-scoped `update_for`.
        assert!(
            !src.contains("create_for_memberships"),
            "an under-tenant grandchild is PathScoped → no membership writes:\n{src}"
        );
        assert!(
            method_body(&src, "update_for").contains("if item.account_id != existing.account_id"),
            "path-scoped transitive update_for must pin the immediate parent fk to block a \
             cross-tenant relocation (issue #102):\n{src}"
        );
    }

    /// Byte-identity backstop (issue #102): a DIRECT flat child (Customer belongs_to the
    /// tenant Club) MUST keep every WRITE body identical to pre-#102 — the membership
    /// writes read the tenant fk straight from the body, and `remove_for`/`update_for`
    /// keep the TYPED sea-orm builder. No JOIN, no raw-SQL subquery in any write.
    #[test]
    fn direct_flat_child_writes_are_byte_identical() {
        let d: Design = serde_json::from_str(CLUBS_TENANCY).unwrap();
        let src = scoped_methods(d.find_entity("Customer").unwrap(), &d);
        // Membership-checked create/update/delete read the direct tenant fk from the body.
        assert!(
            src.contains("let tenant_fk = item.club_id")
                && src.contains(
                    "SELECT 1 FROM club_members WHERE user_id = ? AND club_id = ? LIMIT 1"
                ),
            "direct create checks the body's own tenant fk:\n{src}"
        );
        assert!(
            src.contains("if item.club_id != existing.club_id"),
            "direct update pins the tenant fk:\n{src}"
        );
        assert!(
            src.contains("DELETE FROM customers WHERE id = ? AND club_id IN (SELECT club_id FROM club_members WHERE user_id = ?)"),
            "direct delete filters the direct tenant fk:\n{src}"
        );
        // Path-scoped writes keep the typed builder; no JOIN anywhere in a direct child.
        assert!(
            src.contains(".filter(customer::Column::ClubId.eq(club_id))"),
            "direct remove_for/update_for keep the typed builder:\n{src}"
        );
        assert!(
            !src.contains(" JOIN "),
            "a direct child must never emit a JOIN in any write:\n{src}"
        );
    }

    /// Task 4 (issue #102): a TRANSITIVE (grandchild) module scopes purely through raw
    /// JOIN SQL — it never emits a typed `.filter(Column::..eq())`, so its repo must NOT
    /// import `ColumnTrait`/`QueryFilter` (they would be unused → `-D warnings` on
    /// generated code), while still importing `ConnectionTrait` for the raw SQL. A DIRECT
    /// tenant-owned module keeps the typed builder, so its imports are unchanged.
    #[test]
    fn transitive_repo_drops_the_unused_typed_filter_imports() {
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let d: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT).unwrap();
        let contacts = repo_rs(&d.modules[2], mode, &d).unwrap();
        assert!(
            !contacts.contains("ColumnTrait") && !contacts.contains("QueryFilter"),
            "a transitive repo must not import the unused typed-filter traits:\n{contacts}"
        );
        assert!(
            contacts.contains("ConnectionTrait"),
            "a transitive repo still needs ConnectionTrait for the raw JOIN SQL:\n{contacts}"
        );
        // Byte-identity backstop: a DIRECT tenant-owned module keeps the typed builder,
        // so its import line still carries ColumnTrait + QueryFilter.
        let cd: Design = serde_json::from_str(CLUBS_TENANCY).unwrap();
        let customers = repo_rs(&cd.modules[2], mode, &cd).unwrap();
        assert!(
            customers.contains("ColumnTrait") && customers.contains("QueryFilter"),
            "a direct tenant-owned repo keeps the typed-filter imports:\n{customers}"
        );
    }

    /// LOAD-BEARING INVARIANT (#78 × #102): recognition going transitive must NOT
    /// change the GUARD SHAPE — the guard shape follows the resolved PATH, never mere
    /// ownership. A grandchild whose path carries its PARENT'S fk
    /// (`/accounts/{account_id}/contacts`) but NOT the tenant fk is `MembershipSet`,
    /// and its handler takes the bare session (`_user: CurrentUser`), NEVER the
    /// membership-checked `Dep<Tenant>` (a flat handler must not trust an arbitrary
    /// path membership). Only a grandchild whose path carries the TENANT fk
    /// (`/orgs/{org_id}/contacts`) is `PathScoped` and gets `Dep<Tenant>`.
    #[test]
    fn grandchild_guard_shape_follows_path_not_ownership() {
        let mode = GenMode {
            db: true,
            auth: true,
        };

        // (a) Nested under its PARENT (account_id, not the tenant fk) → MembershipSet,
        //     bare session, NEVER Dep<Tenant>.
        let flat: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT_UNDER_PARENT).unwrap();
        let contacts = &flat.modules[2];
        assert_eq!(
            flat.endpoint_tenant_shape(contacts, &contacts.endpoints[0]),
            TenantShape::MembershipSet,
            "grandchild under its parent carries account_id, not the tenant fk"
        );
        let handlers = handlers_rs(contacts, mode, &flat);
        assert!(
            handlers.contains("_user: CurrentUser") && !handlers.contains("Dep<Tenant>"),
            "flat grandchild handler must take the bare session, NEVER Dep<Tenant>:\n{handlers}"
        );

        // (b) Mounted directly under the TENANT (org_id in the path) → PathScoped,
        //     Dep<Tenant>.
        let scoped: Design = serde_json::from_str(ORG_ACCOUNT_CONTACT_UNDER_TENANT).unwrap();
        let sc = &scoped.modules[2];
        assert!(
            matches!(
                scoped.endpoint_tenant_shape(sc, &sc.endpoints[0]),
                TenantShape::PathScoped { .. }
            ),
            "grandchild under the tenant carries the tenant fk → PathScoped"
        );
        let scoped_handlers = handlers_rs(sc, mode, &scoped);
        assert!(
            scoped_handlers.contains("_tenant: Dep<Tenant>"),
            "path-scoped grandchild handler takes Dep<Tenant>:\n{scoped_handlers}"
        );
    }

    /// A path-nested + flat tenancy design (BookClubs shape): nested books under
    /// `/clubs/{club_id}`, flat customers, and the clubs collection root.
    const CLUBS_TENANCY: &str = r#"{ "name": "clubs-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "clubs",
              "entities": [{ "name": "Club", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "create_club", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Club" },
                    "success": { "status": 201, "entity": "Club" } },
                  { "operation_id": "get_club", "method": "GET", "path": "/{club_id}", "auth_required": true,
                    "success": { "status": 200, "entity": "Club" } } ] },
            { "name": "books", "mount": "/clubs/{club_id}",
              "entities": [{ "name": "Book",
                  "belongs_to": [{ "entity": "Club" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "title", "type": "string" }] }],
              "endpoints": [
                  { "operation_id": "list_books", "method": "GET", "path": "/", "auth_required": true,
                    "success": { "status": 200, "entity": "Book", "list": true } },
                  { "operation_id": "get_book", "method": "GET", "path": "/{id}", "auth_required": true,
                    "success": { "status": 200, "entity": "Book" } } ] },
            { "name": "customers",
              "entities": [{ "name": "Customer",
                  "belongs_to": [{ "entity": "Club" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "email", "type": "string" }] }],
              "endpoints": [
                  { "operation_id": "list_customers", "method": "GET", "path": "/", "auth_required": true,
                    "success": { "status": 200, "entity": "Customer", "list": true } },
                  { "operation_id": "get_customer", "method": "GET", "path": "/{id}", "auth_required": true,
                    "success": { "status": 200, "entity": "Customer" } },
                  { "operation_id": "create_customer", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Customer" },
                    "success": { "status": 201, "entity": "Customer" } },
                  { "operation_id": "update_customer", "method": "PUT", "path": "/{id}", "auth_required": true,
                    "request_body": { "entity": "Customer" },
                    "success": { "status": 200, "entity": "Customer" } },
                  { "operation_id": "delete_customer", "method": "DELETE", "path": "/{id}", "auth_required": true,
                    "success": { "status": 204 } } ] }
        ] }"#;

    /// Like CLUBS_TENANCY but the tenant `Team` has a TEXT (uuid) primary key, so its
    /// fk column type is `String` — the shape that trips the #125 use-after-move.
    /// `Doc` is a direct path-scoped child mounted under the tenant at
    /// `/teams/{team_id}`, so its `update_for` both filters on and pins the `team_id`
    /// fk. Kept minimal (GET-only child, mirroring Book) — `scoped_methods` emits
    /// `update_for` for any path-scoped tenant-owned entity regardless of endpoints.
    const TEAM_DOC_TEXT_PK: &str = r#"{ "name": "team-docs-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Team", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "teams",
              "entities": [{ "name": "Team", "fields": [
                  { "name": "id", "type": "uuid" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "create_team", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Team" },
                    "success": { "status": 201, "entity": "Team" } },
                  { "operation_id": "get_team", "method": "GET", "path": "/{team_id}", "auth_required": true,
                    "success": { "status": 200, "entity": "Team" } } ] },
            { "name": "docs", "mount": "/teams/{team_id}",
              "entities": [{ "name": "Doc",
                  "belongs_to": [{ "entity": "Team" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "title", "type": "string" }] }],
              "endpoints": [
                  { "operation_id": "list_docs", "method": "GET", "path": "/", "auth_required": true,
                    "success": { "status": 200, "entity": "Doc", "list": true } },
                  { "operation_id": "get_doc", "method": "GET", "path": "/{id}", "auth_required": true,
                    "success": { "status": 200, "entity": "Doc" } } ] }
        ] }"#;

    /// The guard PARAM and the read scope-hint comment follow `endpoint_tenant_shape`
    /// (issue #78): PathScoped → `Dep<Tenant>` + name the path-scoped repo method;
    /// MembershipSet → `CurrentUser` (never `Dep<Tenant>`) + name the membership-set
    /// method; Collection → `CurrentUser` (create/list are Task 3). This is the whole
    /// point of the fix — a flat tenant-owned handler must NOT trust an arbitrary
    /// membership via `Dep<Tenant>`; it scopes to the membership SET.
    #[test]
    fn guard_param_and_scope_comment_follow_tenant_shape() {
        let d: Design = serde_json::from_str(CLUBS_TENANCY).unwrap();
        let mode = GenMode {
            db: true,
            auth: true,
        };

        // PathScoped (nested) → Dep<Tenant>; scope comment names all_for / get_for.
        let books = handlers_rs(&d.modules[1], mode, &d);
        assert!(
            books.contains(
                "pub(crate) async fn list_books(_repo: Dep<BookRepo>, _tenant: Dep<Tenant>)"
            ),
            "path-scoped list takes Dep<Tenant>:\n{books}"
        );
        assert!(
            books.contains("BookRepo::all_for(_tenant.id())"),
            "path-scoped list comment names all_for:\n{books}"
        );
        assert!(
            books.contains("BookRepo::get_for(_tenant.id(), _id)"),
            "path-scoped detail comment names get_for:\n{books}"
        );

        // MembershipSet (flat) → CurrentUser, never Dep<Tenant>; comment names the set methods.
        let customers = handlers_rs(&d.modules[2], mode, &d);
        assert!(
            customers.contains(
                "pub(crate) async fn list_customers(_repo: Dep<CustomerRepo>, _user: CurrentUser)"
            ),
            "flat list takes CurrentUser, not Dep<Tenant>:\n{customers}"
        );
        assert!(
            !customers.contains("Dep<Tenant>"),
            "flat handlers must never take Dep<Tenant>:\n{customers}"
        );
        assert!(
            customers.contains("CustomerRepo::all_for_memberships(_user.0.id)"),
            "membership-set list comment names all_for_memberships:\n{customers}"
        );
        assert!(
            customers.contains("CustomerRepo::get_for_memberships(_user.0.id, _id)"),
            "membership-set detail comment names get_for_memberships:\n{customers}"
        );

        // Collection root → CurrentUser; the tenant's OWN detail route is path-scoped.
        let clubs = handlers_rs(&d.modules[0], mode, &d);
        assert!(
            clubs.contains(
                "pub(crate) async fn create_club(_repo: Dep<ClubRepo>, _user: CurrentUser"
            ),
            "collection create takes CurrentUser:\n{clubs}"
        );
        assert!(
            clubs.contains(
                "pub(crate) async fn get_club(_repo: Dep<ClubRepo>, _tenant: Dep<Tenant>, Path(_club_id): Path<i64>)"
            ),
            "the tenant's own detail route is path-scoped:\n{clubs}"
        );
    }

    /// The flat cross-tenant WRITE leak fix (issue #94, spec §C `WITH CHECK`). A FLAT
    /// tenant-owned entity's repo emits membership-CHECKED write accessors so a user
    /// can't `POST {tenant_fk: not-mine}` into a tenant they don't belong to: the
    /// create verifies the BODY fk ∈ the caller's memberships (403 otherwise), and
    /// update/delete are scoped to the set (404 outside it, 403 on a cross-tenant move).
    /// A PATH-SCOPED (nested) entity is scoped by the verified path tenant instead, so
    /// it must NOT get these methods — that write path stays byte-identical (T2).
    #[test]
    fn flat_tenant_owned_entity_gets_membership_checked_write_methods() {
        let d: Design = serde_json::from_str(CLUBS_TENANCY).unwrap();
        let mode = GenMode {
            db: true,
            auth: true,
        };

        // FLAT `customers`: the repo emits the three membership-CHECKED writes.
        let customers = repo_rs(&d.modules[2], mode, &d).unwrap();
        assert!(
            customers.contains(
                "pub async fn create_for_memberships(&self, user_id: String, item: Customer) -> Result<i64>"
            ),
            "flat create is membership-checked:\n{customers}"
        );
        assert!(
            customers.contains(
                "pub async fn update_for_memberships(&self, user_id: String, id: i64, item: Customer) -> Result<bool>"
            ),
            "flat update is membership-checked:\n{customers}"
        );
        assert!(
            customers.contains(
                "pub async fn remove_for_memberships(&self, user_id: String, id: i64) -> Result<bool>"
            ),
            "flat delete is membership-checked:\n{customers}"
        );
        // The create's WITH CHECK is the membership EXISTS probe; out-of-set → 403.
        assert!(
            customers
                .contains("SELECT 1 FROM club_members WHERE user_id = ? AND club_id = ? LIMIT 1")
                && customers.contains("return Err(Error::forbidden());"),
            "create verifies the body fk against the caller's memberships (403 else):\n{customers}"
        );
        // Delete is scoped to the set by the RLS subquery (0 rows outside it → 404).
        assert!(
            customers.contains(
                "DELETE FROM customers WHERE id = ? AND club_id IN (SELECT club_id FROM club_members WHERE user_id = ?)"
            ),
            "delete is scoped to the membership set:\n{customers}"
        );

        // The flat mutation handlers are STEERED to the checked methods (never the
        // unscoped insert/update/remove), and still take `CurrentUser` — not `Dep<Tenant>`.
        let handlers = handlers_rs(&d.modules[2], mode, &d);
        assert!(
            handlers.contains("CustomerRepo::create_for_memberships(_user.0.id, customer)"),
            "flat create stub is steered to create_for_memberships:\n{handlers}"
        );
        assert!(
            handlers.contains("CustomerRepo::update_for_memberships(_user.0.id, _id, customer)"),
            "flat update stub is steered to update_for_memberships:\n{handlers}"
        );
        assert!(
            handlers.contains("CustomerRepo::remove_for_memberships(_user.0.id, _id)"),
            "flat delete stub is steered to remove_for_memberships:\n{handlers}"
        );
        assert!(
            handlers.contains(
                "pub(crate) async fn create_customer(_repo: Dep<CustomerRepo>, _user: CurrentUser"
            ),
            "flat create handler takes CurrentUser, not Dep<Tenant>:\n{handlers}"
        );

        // NO-DRIFT: the PATH-SCOPED nested `books` entity must NOT gain these methods —
        // its writes are scoped by the verified path tenant (byte-identical to pre-#94).
        let books = repo_rs(&d.modules[1], mode, &d).unwrap();
        assert!(
            !books.contains("create_for_memberships")
                && !books.contains("update_for_memberships")
                && !books.contains("remove_for_memberships"),
            "path-scoped entity must not get membership-checked writes:\n{books}"
        );
    }

    /// A tenant module carrying BOTH collection routes: the membership-filtered
    /// list (`GET /`) and the auto-seeding create (`POST /`). CLUBS_TENANCY lacks
    /// the list route, so Task 3 (issue #78) tests use this shape.
    const CLUBS_LIFECYCLE: &str = r#"{ "name": "clubs-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "clubs",
              "entities": [{ "name": "Club", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "list_clubs", "method": "GET", "path": "/", "auth_required": true,
                    "success": { "status": 200, "entity": "Club", "list": true } },
                  { "operation_id": "create_club", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Club" },
                    "success": { "status": 201, "entity": "Club" } } ] }
        ] }"#;

    /// Task 3 / issue #78: the TENANT entity's own repo emits `create_with_membership`,
    /// which seeds the creator into `{tenant}_members` as the FIRST declared member_role
    /// in the SAME transaction as the tenant insert — so a freshly created tenant is
    /// never memberless and the membership-verified guard admits the creator on the very
    /// next request. The seed lives inside ONE generated method, so an agent can't drop
    /// it the way a hand-written INSERT (the old docs pattern) invited.
    #[test]
    fn tenant_entity_repo_seeds_creator_membership_atomically_on_create() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains(
                "pub async fn create_with_membership(&self, user_id: String, item: Club) -> Result<i64>"
            ),
            "tenant repo exposes create_with_membership:\n{src}"
        );
        // One transaction: the tenant insert and the membership seed commit together.
        assert!(
            src.contains("begin()") && src.contains("commit()"),
            "atomic (begin/commit):\n{src}"
        );
        // The membership row: user_id + tenant fk + role, into the members table.
        assert!(
            src.contains("INSERT INTO club_members (user_id, club_id, role)"),
            "seeds the membership row:\n{src}"
        );
        // role = member_roles[0] = "owner".
        assert!(
            src.contains("\"owner\""),
            "role is the first member_role:\n{src}"
        );
    }

    /// Task 3 / issue #78: the tenant entity's own repo emits `all_for_member`, a
    /// membership-filtered list — a caller sees ONLY the tenants they belong to
    /// (`JOIN {members} … WHERE user_id = ?`), never the unscoped `all()`.
    #[test]
    fn tenant_entity_repo_lists_only_the_callers_tenants() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains(
                "pub async fn all_for_member(&self, user_id: String) -> Result<Vec<Club>>"
            ),
            "tenant repo exposes all_for_member:\n{src}"
        );
        assert!(
            src.contains("JOIN club_members"),
            "list joins the members table:\n{src}"
        );
        assert!(
            src.contains("WHERE m.user_id = ?"),
            "list filters by the caller:\n{src}"
        );
    }

    /// Issue #107 / 0.6.0 Task 1: the tenant entity's own repo carries the full
    /// member-management surface — list/add/re-role/remove plus the
    /// `count_admins` helper backing the last-admin guard — as REAL SQL keyed on
    /// the PATH tenant fk the membership guard already verified. Without these,
    /// every tenancy app hand-writes `INSERT INTO {tenant}_members …` (the #107
    /// finding), which is exactly the raw-SQL drift a generated surface kills.
    #[test]
    fn tenant_entity_repo_gets_member_management_methods() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains("pub async fn members_of(&self, fk: i64) -> Result<Vec<ClubMember>>"),
            "members_of lists the roster as typed rows:\n{src}"
        );
        assert!(
            src.contains(
                "SELECT id, user_id, role FROM club_members WHERE club_id = ? ORDER BY id"
            ),
            "members_of reads only the path tenant's rows:\n{src}"
        );
        assert!(
            src.contains(
                "pub async fn add_member(&self, fk: i64, user_id: String, role: String) -> Result<()>"
            ),
            "add_member exists:\n{src}"
        );
        assert!(
            src.contains(
                "pub async fn set_member_role(&self, fk: i64, user_id: String, role: String) -> Result<bool>"
            ),
            "set_member_role exists:\n{src}"
        );
        assert!(
            src.contains("UPDATE club_members SET role = ? WHERE user_id = ? AND club_id = ?"),
            "set_member_role updates scoped to (user, tenant):\n{src}"
        );
        assert!(
            src.contains(
                "pub async fn remove_member(&self, fk: i64, user_id: String) -> Result<bool>"
            ),
            "remove_member exists:\n{src}"
        );
        assert!(
            src.contains("DELETE FROM club_members WHERE user_id = ? AND club_id = ?"),
            "remove_member deletes scoped to (user, tenant):\n{src}"
        );
        assert!(
            src.contains("pub async fn count_admins(&self, fk: i64) -> Result<i64>"),
            "count_admins helper exists:\n{src}"
        );
        assert!(
            src.contains("SELECT COUNT(*) AS n FROM club_members WHERE club_id = ? AND role = ?"),
            "count_admins counts one tenant's admins:\n{src}"
        );
    }

    /// #107 review nit (Rule 9): `add_member`'s INSERT column order AND its
    /// bound-value order must agree POSITIONALLY — a swapped bind array (e.g.
    /// `[fk, user_id, role]`) still compiles and passes every shape-only test,
    /// but writes the tenant id into `user_id` and the user id into the fk,
    /// silently corrupting every membership it creates. Pin both the SQL column
    /// list and the exact bind array, in the add_member body specifically.
    #[test]
    fn add_member_binds_match_the_insert_column_order() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        let add_body =
            &src[src.find("fn add_member").unwrap()..src.find("fn set_member_role").unwrap()];
        assert!(
            add_body.contains("INSERT INTO club_members (user_id, club_id, role) VALUES (?, ?, ?)"),
            "add_member inserts (user_id, fk, role) in that column order:\n{add_body}"
        );
        assert!(
            add_body.contains("[user_id.into(), fk.into(), role.into()]"),
            "add_member's bind array must be [user_id, fk, role] — the SAME order as \
             the INSERT columns (a swapped bind compiles but corrupts memberships):\n{add_body}"
        );
    }

    /// The member surface's row type and role rule are BAKED into the generated
    /// code: a serializable `{Tenant}Member` row (a later handler returns
    /// `[{id, user_id, role}]` without hand-rolling a DTO), and the design's
    /// `member_roles` as a const validated on add/re-role → 422 (no DB CHECK
    /// backs the column). A duplicate add is NOT pre-checked in the repo — the
    /// UNIQUE(user_id, fk) index fires and jerrycan-db's `db_error` maps it to
    /// 409, so there is exactly ONE conflict mapping and no drift.
    #[test]
    fn member_row_type_roles_const_and_validation_are_baked_in() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains("#[derive(serde::Serialize, sea_orm::FromQueryResult)]")
                && src.contains("pub struct ClubMember {"),
            "typed serializable member row:\n{src}"
        );
        assert!(
            src.contains("pub id: i64,")
                && src.contains("pub user_id: String,")
                && src.contains("pub role: String,"),
            "row shape is id + user_id + role:\n{src}"
        );
        assert!(
            src.contains("const MEMBER_ROLES: &[&str] = &[\"owner\", \"member\"];"),
            "declared member_roles baked as a const:\n{src}"
        );
        assert!(
            src.contains("if !MEMBER_ROLES.contains(&role.as_str())")
                && src.contains("Error::unprocessable(\"role must be one of: owner, member\")"),
            "an out-of-set role is refused as 422:\n{src}"
        );
        // `find_by_statement` (typed member rows) is a FromQueryResult trait
        // method, so the trait joins the facade imports for this module.
        assert!(
            src.contains(
                "use jerrycan::db::sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, EntityTrait, FromQueryResult, QueryOrder};"
            ),
            "FromQueryResult joins the trait imports:\n{src}"
        );
        // No second conflict path: add_member carries no Error::conflict of its
        // own — the UNIQUE index + db_error IS the 409.
        let add_body =
            &src[src.find("fn add_member").unwrap()..src.find("fn set_member_role").unwrap()];
        assert!(
            !add_body.contains("Error::conflict"),
            "duplicate adds must surface via db_error, not a second mapping:\n{add_body}"
        );
    }

    /// Integrity (#107 design §B): the LAST admin can neither be demoted nor
    /// removed — both paths 409 — otherwise the tenant is permanently locked out
    /// of member management (nobody left holding member_roles[0], and the write
    /// gate is admin-only). Re-affirming the admin role on the last admin stays
    /// legal: the demote guard fires only when the role actually changes.
    #[test]
    fn member_writes_enforce_the_last_admin_guard() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains("Error::conflict(\"cannot demote the last owner\")"),
            "demoting the last admin is refused:\n{src}"
        );
        assert!(
            src.contains("Error::conflict(\"cannot remove the last owner\")"),
            "removing the last admin is refused:\n{src}"
        );
        // Both writes consult the per-tenant admin count.
        assert_eq!(
            src.matches("self.count_admins(fk).await? == 1").count(),
            2,
            "both writes check the per-tenant admin count:\n{src}"
        );
        // The admin role is the FIRST declared member_role, bound in count_admins.
        assert!(
            src.contains("[fk.into(), \"owner\".into()]"),
            "count_admins counts the first declared role:\n{src}"
        );
        assert!(
            src.contains("&& role != \"owner\""),
            "re-setting the same admin role is not a demotion:\n{src}"
        );
    }

    /// NO-DRIFT (byte-identity): the member surface exists ONLY on the tenancy
    /// entity's own repo. A non-tenant entity in a tenancy design, and every
    /// entity in a non-tenancy design, emit exactly the pre-#107 repo — no
    /// member methods, no row struct, no roles const, no FromQueryResult import.
    #[test]
    fn member_surface_is_confined_to_the_tenant_repo() {
        let mode = GenMode {
            db: true,
            auth: true,
        };
        // (a) tenancy design, NON-tenant module: leads (V1_FULL modules[1]).
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let leads = repo_rs(&d.modules[1], mode, &d).unwrap();
        for needle in [
            "members_of",
            "add_member",
            "set_member_role",
            "remove_member",
            "count_admins",
            "MEMBER_ROLES",
            "LeadMember",
        ] {
            assert!(
                !leads.contains(needle),
                "non-tenant repo must not carry `{needle}`:\n{leads}"
            );
        }
        // (b) the SAME tenant module with tenancy stripped: nothing member-shaped
        // remains, including the FromQueryResult import — byte-identical output.
        let mut d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        d.tenancy = None;
        let clubs = repo_rs(&d.modules[0], mode, &d).unwrap();
        for needle in [
            "members_of",
            "add_member",
            "set_member_role",
            "remove_member",
            "count_admins",
            "MEMBER_ROLES",
            "ClubMember",
            "FromQueryResult",
        ] {
            assert!(
                !clubs.contains(needle),
                "non-tenancy repo must not carry `{needle}`:\n{clubs}"
            );
        }
    }

    /// A TEXT-pk tenant (uuid/string id — the migrated-Supabase shape): the fk
    /// param is `String`, so the guard pre-checks clone it and the final bound
    /// statement still receives the owned value.
    #[test]
    fn member_methods_take_a_text_tenant_fk_when_the_tenant_pk_is_text() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "orgs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["admin", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Org", "member_roles": ["admin", "member"] },
                "modules": [
                    { "name": "orgs",
                      "entities": [{ "name": "Org", "fields": [
                          { "name": "id", "type": "string" },
                          { "name": "name", "type": "string" } ]}],
                      "endpoints": [
                          { "operation_id": "create_org", "method": "POST", "path": "/", "auth_required": true,
                            "request_body": { "entity": "Org" },
                            "success": { "status": 201, "entity": "Org" } } ] }
                ] }"#,
        )
        .unwrap();
        let src = repo_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        )
        .unwrap();
        assert!(
            src.contains("pub async fn members_of(&self, fk: String) -> Result<Vec<OrgMember>>"),
            "text tenant pk means a String fk param:\n{src}"
        );
        assert!(
            src.contains("self.count_admins(fk.clone()).await? == 1"),
            "a String fk is cloned into the admin-count pre-check:\n{src}"
        );
    }

    /// Issue #107 / 0.6.0 Task 2: the tenant module gains a TOOL-OWNED, fully
    /// implemented members.rs (like storagegen's bucket handlers — never agent
    /// stubs) whose four handlers call the Task-1 repo methods under the right
    /// gates: `Dep<Tenant>` everywhere (the path-scoped guard 404s non-members),
    /// `require_role(member_roles[0])` on every write, and the self-removal
    /// exception on DELETE (comparing the path `user_id` to the CALLER'S id, so
    /// any member can leave without holding the admin role).
    #[test]
    fn tenant_module_emits_the_tool_owned_member_surface() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let src = members_rs(&d.modules[0], mode, &d).expect("tenant module emits members.rs");
        // list: guard-only (no require_role), scoped to the PATH tenant.
        assert!(
            src.contains("pub(crate) async fn list_members(")
                && src.contains("Ok(Json(repo.members_of(tenant.id()).await?))"),
            "list_members reads the path tenant's roster:\n{src}"
        );
        let list_body =
            &src[src.find("fn list_members").unwrap()..src.find("fn add_member").unwrap()];
        assert!(
            !list_body.contains("require_role"),
            "any member may list — no role gate on reads:\n{list_body}"
        );
        // add: admin gate BEFORE the repo call.
        let add_body =
            &src[src.find("fn add_member").unwrap()..src.find("fn set_member_role").unwrap()];
        let gate = add_body
            .find("tenant.require_role(\"owner\")?;")
            .expect("add is admin-gated");
        let call = add_body
            .find("repo.add_member(tenant.id(), body.user_id.clone(), body.role.clone())")
            .expect("add calls the T1 repo method");
        assert!(
            gate < call,
            "the role gate must run BEFORE the write:\n{add_body}"
        );
        // set-role: admin gate + 404 on an unknown member.
        let set_body =
            &src[src.find("fn set_member_role").unwrap()..src.find("fn remove_member").unwrap()];
        let gate = set_body
            .find("tenant.require_role(\"owner\")?;")
            .expect("set is admin-gated");
        let call = set_body
            .find(".set_member_role(tenant.id(), user_id, body.role)")
            .expect("set calls the T1 repo method");
        assert!(
            gate < call,
            "the role gate must run BEFORE the write:\n{set_body}"
        );
        assert!(
            set_body.contains("Err(Error::not_found())"),
            "an unknown member is 404:\n{set_body}"
        );
        // remove: the SELF-REMOVAL exception — the admin gate applies ONLY when the
        // target is someone else, so any member can leave.
        let rm_body = &src[src.find("fn remove_member").unwrap()..];
        assert!(
            rm_body.contains(
                "if user_id != user.0.id {\n        tenant.require_role(\"owner\")?;\n    }"
            ),
            "self-removal skips the admin gate; removing others requires it:\n{rm_body}"
        );
        assert!(
            rm_body.contains("repo.remove_member(tenant.id(), user_id).await?"),
            "remove calls the T1 repo method:\n{rm_body}"
        );
        // remove is the only handler needing the caller's identity for the compare.
        assert!(
            rm_body.contains("user: CurrentUser"),
            "remove takes the caller to compare ids:\n{rm_body}"
        );
        // Every handler is guard-scoped: 4 handlers, 4 `Dep<Tenant>` params.
        assert_eq!(
            src.matches("tenant: Dep<Tenant>").count(),
            4,
            "all four handlers take the membership-verifying guard:\n{src}"
        );
    }

    /// The member routes are REGISTERED path-scoped in the tenant module's
    /// tool-owned lib.rs — `mod members;` + two `.route` lines on the tenant fk
    /// param, so the shared guard's by-name fk lookup verifies membership in the
    /// PATH tenant (the whole point of mounting under `/{fk}`). The fk name
    /// matches the tenant's own detail routes by construction
    /// (`normalize_tenant_detail_routes`), keeping the router's
    /// one-param-name-per-position rule intact.
    #[test]
    fn member_routes_are_mounted_in_the_tenant_module_lib() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let lib = lib_rs(&d.modules[0], mode, &d);
        assert!(
            lib.contains("mod members;\n"),
            "members module declared:\n{lib}"
        );
        assert!(
            lib.contains(
                ".route(\"/{club_id}/members\", get(members::list_members).post(members::add_member))"
            ),
            "collection routes on the tenant fk param:\n{lib}"
        );
        assert!(
            lib.contains(
                ".route(\"/{club_id}/members/{user_id}\", patch(members::set_member_role).delete(members::remove_member))"
            ),
            "item routes on the tenant fk param:\n{lib}"
        );
        // Alphabetical mod order (rustfmt reorder_modules must stay a no-op).
        assert!(
            lib.contains("mod handlers;\nmod members;\nmod model;"),
            "mod decls stay alphabetically sorted:\n{lib}"
        );
    }

    /// NO-DRIFT (byte-identity): the member surface is emitted ONLY for the
    /// module that declares the tenancy entity, in db+auth mode. A non-tenant
    /// module, a tenancy-stripped design, and memory/no-auth modes all emit
    /// exactly the pre-#107 lib.rs (no `members` token) and no members.rs.
    #[test]
    fn member_surface_is_confined_to_the_tenant_module_and_db_auth_mode() {
        let mode = GenMode {
            db: true,
            auth: true,
        };
        // (a) tenancy design, NON-tenant module.
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        assert!(
            members_rs(&d.modules[1], mode, &d).is_none(),
            "leads is not the tenant"
        );
        assert!(
            !lib_rs(&d.modules[1], mode, &d).contains("members"),
            "non-tenant lib.rs must not mention members"
        );
        // (b) the SAME tenant module with tenancy stripped: byte-identical lib.rs.
        let mut dt: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let with = lib_rs(&dt.modules[0], mode, &dt);
        assert!(members_rs(&dt.modules[0], mode, &dt).is_some());
        dt.tenancy = None;
        let without = lib_rs(&dt.modules[0], mode, &dt);
        assert!(members_rs(&dt.modules[0], mode, &dt).is_none());
        assert!(
            !without.contains("members"),
            "stripped design: no members:\n{without}"
        );
        assert_eq!(
            with.replace(
                "            .route(\"/{club_id}/members\", get(members::list_members).post(members::add_member))\n            .route(\"/{club_id}/members/{user_id}\", patch(members::set_member_role).delete(members::remove_member))\n",
                ""
            )
            .replace("mod members;\n", ""),
            without,
            "the ONLY lib.rs delta is the member surface"
        );
        // (c) mode gates: no db (no members table/repo) or no auth (no guard) → none.
        let dt: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        for (db, auth) in [(false, true), (true, false), (false, false)] {
            let mode = GenMode { db, auth };
            assert!(
                members_rs(&dt.modules[0], mode, &dt).is_none(),
                "db={db} auth={auth} must not emit the surface"
            );
            assert!(
                !lib_rs(&dt.modules[0], mode, &dt).contains("members"),
                "db={db} auth={auth} lib.rs must not mention members"
            );
        }
    }

    /// The tenant's own COLLECTION handlers are steered to the membership-lifecycle
    /// methods by construction: `create_*` to `create_with_membership` (never the bare
    /// `insert`, which would leave the tenant memberless), and the tenant `list_*` to
    /// `all_for_member` (never the unscoped `all()`), both keyed on `_user.0.id`.
    #[test]
    fn tenant_collection_handlers_are_steered_to_membership_methods() {
        let d: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let src = handlers_rs(
            &d.modules[0],
            GenMode {
                db: true,
                auth: true,
            },
            &d,
        );
        assert!(
            src.contains("ClubRepo::create_with_membership(_user.0.id"),
            "create handler names create_with_membership:\n{src}"
        );
        assert!(
            src.contains("ClubRepo::all_for_member(_user.0.id)"),
            "tenant list handler names all_for_member:\n{src}"
        );
    }

    /// Issue #78 (the cross-tenant leak this fix closes): a tenant module authored
    /// with the CONVENTIONAL `/{id}` detail route is normalized to `/{club_id}`, and
    /// that rename must ripple through BOTH the route table (so the router captures
    /// `club_id` and the guard's path branch fires) AND the handler param (name +
    /// type). Without the ripple the guard reads `params.get("club_id") == None`,
    /// falls back to an arbitrary first membership, and the handler reads another
    /// tenant's row.
    #[test]
    fn tenant_own_conventional_id_route_is_normalized_through_route_and_handler() {
        let conventional = r#"{ "name": "clubs-api", "contract_version": 1,
            "auth": { "model": "session", "roles": ["owner", "member"] },
            "dependencies": ["db", "auth"],
            "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
            "modules": [
                { "name": "clubs",
                  "entities": [{ "name": "Club", "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "name", "type": "string" } ]}],
                  "endpoints": [
                      { "operation_id": "create_club", "method": "POST", "path": "/", "auth_required": true,
                        "request_body": { "entity": "Club" },
                        "success": { "status": 201, "entity": "Club" } },
                      { "operation_id": "get_club", "method": "GET", "path": "/{id}", "auth_required": true,
                        "success": { "status": 200, "entity": "Club" } },
                      { "operation_id": "delete_club", "method": "DELETE", "path": "/{id}", "auth_required": true,
                        "success": { "status": 204 } } ] }
            ] }"#;
        let mut d: Design = serde_json::from_str(conventional).unwrap();
        // The design as-loaded still carries the raw `/{id}` (the parser doesn't
        // normalize; `from_path`/the MCP entry points do). Normalize as a real load
        // would.
        d.normalize_tenant_detail_routes();
        let mode = GenMode {
            db: true,
            auth: true,
        };

        // Route table: the router now captures `club_id`, so the guard verifies it.
        let lib = lib_rs(&d.modules[0], mode, &d);
        assert!(
            lib.contains(
                ".route(\"/{club_id}\", get(handlers::get_club).delete(handlers::delete_club))"
            ),
            "tenant-own detail route must register /{{club_id}}:\n{lib}"
        );
        assert!(
            !lib.contains("/{id}"),
            "no `/{{id}}` may survive on the tenant module:\n{lib}"
        );

        // Handler: the path param is renamed AND keeps the tenant entity key type.
        let clubs = handlers_rs(&d.modules[0], mode, &d);
        assert!(
            clubs.contains(
                "pub(crate) async fn get_club(_repo: Dep<ClubRepo>, _tenant: Dep<Tenant>, Path(_club_id): Path<i64>)"
            ),
            "tenant-own detail handler binds Dep<Tenant> + Path(_club_id):\n{clubs}"
        );

        // Ripple check for a TEXT-pk tenant: the renamed param must type as the
        // tenant entity KEY (String), not a blind i64 — `entity_key_param`.
        let text_pk = conventional.replace(
            r#"{ "name": "id", "type": "integer" }"#,
            r#"{ "name": "id", "type": "string" }"#,
        );
        let mut dt: Design = serde_json::from_str(&text_pk).unwrap();
        dt.normalize_tenant_detail_routes();
        let clubs_text = handlers_rs(&dt.modules[0], mode, &dt);
        assert!(
            clubs_text.contains("Path(_club_id): Path<String>"),
            "text-pk tenant-own detail param must be Path<String>:\n{clubs_text}"
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
        let lib = lib_rs(&m, GenMode::default(), &demo());
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
        let lib = lib_rs(&d.modules[1], mode, &d);
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

    /// Linking guard for the #69a drift detector: EVERY salient `mod`/`use` decl
    /// the generator itself emits (`mod_decls`, `lib_rs`, `subroute_mod_rs`, and the
    /// subroutes `pub(crate) mod <sub>;` list) MUST satisfy `is_tool_decl`. If a
    /// future generator change emits a new tool decl that `is_tool_decl` doesn't
    /// recognize, a clean regeneration would FALSELY report it as dropped agent
    /// work — this test fails the moment the two drift apart (kills the
    /// cross-version false-positive risk). WHY (Rule 9): the detector's whole value
    /// is that it fires ONLY on real agent loss, never on the tool's own output.
    #[test]
    fn every_generator_emitted_decl_is_recognized_as_tool_owned() {
        let mode = GenMode {
            db: true,
            auth: true,
        };
        let d = demo();
        let m = todos(); // has entities + a `comments` subroute
        let comments = &m.subroutes[0];

        // The subroutes decls file (`pub(crate) mod <sub>;`) is emitted inline in
        // `write_subroutes`; reproduce that line so this guard covers it too.
        let sub_decl = format!("pub(crate) mod {};\n", comments.name.replace('-', "_"));

        // The tenant module's emissions (issue #107): `mod members;` in its lib.rs
        // plus members.rs's own tool-emitted imports must all be recognized, or a
        // clean regeneration of a TENANCY app would false-positive.
        let dt: Design = serde_json::from_str(CLUBS_LIFECYCLE).unwrap();
        let tenant_lib = lib_rs(&dt.modules[0], mode, &dt);
        let tenant_members = members_rs(&dt.modules[0], mode, &dt).expect("tenant module emits");

        let emissions = [
            mod_decls(&m, mode, &d),
            lib_rs(&m, mode, &d),
            subroute_mod_rs(comments, mode, &d),
            mod_decls(comments, mode, &d),
            sub_decl,
            tenant_lib,
            tenant_members,
        ];

        let mut seen = std::collections::HashSet::new();
        for emitted in &emissions {
            for line in emitted.lines() {
                let t = line.trim();
                if is_salient_decl(t) {
                    seen.insert(t.to_string());
                    assert!(
                        is_tool_decl(t),
                        "generator emits salient decl `{t}` that is_tool_decl does not \
                         recognize — clean regen would falsely flag it as dropped agent work"
                    );
                }
            }
        }
        // Non-vacuous: the fixture must actually exercise every is_tool_decl branch,
        // so the assertion above had real work to do.
        for expected in [
            "mod deps;",
            "mod handlers;",
            "mod members;",
            "mod model;",
            "mod repo;",
            "mod subroutes;",
            "use jerrycan::prelude::*;",
            "use shared::{CurrentUser, Tenant};",
            "use super::repo::{ClubMember, ClubRepo};",
            "pub(crate) mod comments;",
        ] {
            assert!(
                seen.contains(expected),
                "fixture should have emitted `{expected}` — otherwise this guard is vacuous"
            );
        }
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

    /// Issue #85 (D1): a `default` field is dropped from the CREATE request DTO
    /// (the server applies the declared value) but MUST stay in the UPDATE request
    /// DTO — otherwise a defaulted lifecycle enum (`status`) can NEVER be changed
    /// after creation. So a defaulted entity used by BOTH a create and an update
    /// endpoint emits two DTOs: `{Entity}Request` (create, omits the default) and
    /// `{Entity}UpdateRequest` (update, keeps it), and each handler binds the
    /// matching body type. WHY (Rule 9): binding the create DTO on update makes the
    /// field unsettable — the exact greenability bug this fix closes.
    const DEFAULT_UPDATE: &str = r#"{
        "name": "news", "contract_version": 0, "dependencies": ["db"],
        "modules": [{ "name": "subscribers",
            "entities": [{ "name": "Subscriber", "fields": [
                { "name": "id", "type": "integer" },
                { "name": "email", "type": "string" },
                { "name": "status", "type": "string", "values": ["active", "expired"], "default": "active" } ] }],
            "endpoints": [
                { "operation_id": "create_subscriber", "method": "POST", "path": "/",
                  "request_body": { "entity": "Subscriber" },
                  "success": { "status": 201, "entity": "Subscriber" } },
                { "operation_id": "update_subscriber", "method": "PUT", "path": "/{id}",
                  "request_body": { "entity": "Subscriber" },
                  "success": { "status": 200, "entity": "Subscriber" } } ] }]
    }"#;

    #[test]
    fn default_field_stays_in_the_update_dto_but_not_the_create_dto() {
        let d: Design = serde_json::from_str(DEFAULT_UPDATE).unwrap();
        let m = &d.modules[0];
        let model = model_rs_db(m, &d, false).unwrap();

        // CREATE DTO omits the default (the server applies it).
        let create_dto = model
            .split("pub struct SubscriberRequest {")
            .nth(1)
            .expect("SubscriberRequest emitted")
            .split('}')
            .next()
            .unwrap();
        assert!(
            !create_dto.contains("status"),
            "CREATE DTO must omit the default field: {create_dto}"
        );

        // UPDATE DTO keeps the default (it must be settable after create).
        let update_dto = model
            .split("pub struct SubscriberUpdateRequest {")
            .nth(1)
            .expect(
                "SubscriberUpdateRequest emitted for a defaulted entity with an update endpoint",
            )
            .split('}')
            .next()
            .unwrap();
        assert!(
            update_dto.contains("pub status: String,"),
            "UPDATE DTO must keep the default field so it can be changed: {update_dto}"
        );

        // Each handler binds the matching body type.
        let mode = GenMode {
            db: true,
            auth: false,
        };
        let h = handlers_rs(m, mode, &d);
        assert!(
            h.contains("async fn create_subscriber(_repo: Dep<SubscriberRepo>, Json(_body): Json<SubscriberRequest>)"),
            "create binds the create DTO (omits the default): {h}"
        );
        assert!(
            h.contains("async fn update_subscriber(_repo: Dep<SubscriberRepo>, Path(_id): Path<i64>, Json(_body): Json<SubscriberUpdateRequest>)"),
            "update binds the update DTO (keeps the default): {h}"
        );
    }

    /// Issue #85 (D2): a non-`{id}` path param must type from the entity it
    /// references, not a hardcoded `i64`. `/{site_id}/pages` references `Site`,
    /// whose pk is a `String`, so the generated handler binds `Path<String>`. WHY
    /// (Rule 9): a hardcoded `i64` makes the router 400/deserialize-fail every
    /// string-pk reference — the whole nested group is un-greenable.
    const STRING_PK_PATH_PARAM: &str = r#"{
        "name": "cms", "contract_version": 0, "dependencies": ["db"],
        "modules": [{ "name": "pages",
            "entities": [
                { "name": "Site", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "name", "type": "string" } ] },
                { "name": "Page", "belongs_to": [{ "entity": "Site" }],
                  "fields": [
                    { "name": "id", "type": "integer" },
                    { "name": "title", "type": "string" } ] } ],
            "endpoints": [
                { "operation_id": "list_pages", "method": "GET", "path": "/{site_id}/pages",
                  "success": { "status": 200, "entity": "Page", "list": true } } ] }]
    }"#;

    #[test]
    fn non_id_path_param_types_from_its_referenced_entity_pk() {
        let d: Design = serde_json::from_str(STRING_PK_PATH_PARAM).unwrap();
        let m = &d.modules[0];
        let mode = GenMode {
            db: true,
            auth: false,
        };
        let h = handlers_rs(m, mode, &d);
        assert!(
            h.contains("async fn list_pages(_repo: Dep<PageRepo>, Path(_site_id): Path<String>)"),
            "a non-id path param referencing a string-pk entity must be Path<String>, not Path<i64>: {h}"
        );
    }
}
