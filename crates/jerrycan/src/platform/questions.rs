//! Deterministic design validation → pointed questions (jerrycan_design's engine).

use super::design::*;
use serde::Serialize;

/// One pointed question. `id` is a JSON-pointer into the draft.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub question: String,
}

fn q(id: impl Into<String>, question: impl Into<String>) -> Question {
    Question {
        id: id.into(),
        question: question.into(),
    }
}

fn is_kebab(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_pascal(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// An enum `values` entry that is safe to interpolate unescaped into generated
/// Rust (issue #54): `^[A-Za-z0-9_-]+$`. Values reach a `"..."` string literal in
/// the generated deserialize allow-list, the 422 error text, and the testgen
/// fixture with no escaping, so anything with a quote/backslash/space would break
/// the generated crate at build time — validate the shape at design time instead.
fn is_enum_value(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A validation message when a field's server-owned `default` (issue #53a) is
/// invalid, or `None` when the default is absent or valid. Three ways to be
/// wrong: (1) declared without a `db` dependency — the default is applied via the
/// db-mode request DTO, so it is silently inert in memory mode; (2) the value
/// does not type-check against the field's `type`; (3) the value is outside the
/// field's enum `values`. The default is written into a NOT-NULL column verbatim,
/// so a mistyped or out-of-enum literal is a design-time error, not a run-time
/// surprise. A `json` field accepts any JSON value.
fn default_type_error(f: &Field, wants_db: bool) -> Option<String> {
    let value = f.default.as_ref()?;
    if !wants_db {
        return Some(format!(
            "Field `{}` declares a `default` but the design has no `db` dependency — server-owned defaults are applied through the db-mode request DTO (add `db` to `dependencies`, or drop the default).",
            f.name
        ));
    }
    // Enum membership: a default on a string field with `values` must be listed.
    if let Some(values) = &f.values {
        return match value.as_str() {
            Some(s) if values.contains(&s.to_string()) => None,
            _ => Some(format!(
                "Field `{}` default {value} is not one of its enum values [{}] — the default must be a declared value.",
                f.name,
                values.join(", ")
            )),
        };
    }
    let ok = match f.field_type {
        FieldType::String | FieldType::Datetime | FieldType::Uuid => value.is_string(),
        FieldType::Integer => value.is_i64() || value.is_u64(),
        FieldType::Float => value.is_number(),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Json => true,
    };
    if ok {
        None
    } else {
        Some(format!(
            "Field `{}` default {value} does not match its type `{:?}` — the server writes it verbatim, so it must be a valid {:?} literal.",
            f.name, f.field_type, f.field_type
        ))
    }
}

/// The entity an endpoint's repo operates on (mirrors genroute's resolution):
/// the request_body entity, else the success entity, else the module's first
/// entity. `None` when the module declares no entities. Kept in lockstep with
/// `genroute::endpoint_repo_entity` so design-time checks reason about the same
/// entity the generator wires.
fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .map(|rb| rb.entity.as_str())
        .or(ep.success.entity.as_deref())
        .or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

// The fixed `user_id` identity linkage (AUTH_IDENTITY_FK_COLUMN) lives in
// `design.rs` — shared with the server-owned-FK emission rule (issue #34).
// It reaches this module through the `use super::design::*` glob above.

/// A fatal design-shape conflict caught before any scaffolding — distinct from
/// the completeness questions `validate` returns (which a field edit can
/// answer). This one needs a structural redesign, so it carries a stable JC
/// code the CLI (`{ok:false, code, ...}`) and the MCP twin render.
#[derive(Debug)]
pub struct DesignConflict {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

/// Reject a design that cannot be generated regardless of completeness. One rule
/// today (#27): `tenancy.entity` must not BE the auth identity entity. When it
/// is, the tenant's derived fk column equals the membership table's fixed
/// `user_id` column, so the auth_0001 migration declares `user_id` twice and
/// dies with `duplicate column name: user_id` — mid-scaffold, on a half-written
/// tree. Catch it up front instead. Shared by the CLI and MCP so they can't drift.
pub fn design_conflict(d: &Design) -> Option<DesignConflict> {
    if let Some(tenancy) = &d.tenancy
        && Design::fk_column(&tenancy.entity) == AUTH_IDENTITY_FK_COLUMN
    {
        let entity = &tenancy.entity;
        return Some(DesignConflict {
            code: "JC0540",
            message: format!(
                "tenancy.entity `{entity}` is the auth identity entity — its derived foreign key column `{AUTH_IDENTITY_FK_COLUMN}` collides with the membership table's authenticated-user column, so scaffolding would die with `duplicate column name: {AUTH_IDENTITY_FK_COLUMN}`. A user cannot be their own tenant org. For per-user data, drop the `tenancy` block and give each owned entity a `belongs_to` `{entity}` plus tenant-scoped guard methods (all_for/get_for); for orgs/teams, point tenancy.entity at a separate tenant entity (e.g. Org or Workspace). See `jerrycan docs tenancy` / `jerrycan explain JC0540`."
            ),
            hint: format!(
                "per-user data → `belongs_to` `{entity}` + scoped guard methods; orgs/teams → a separate tenant entity (Org/Workspace)"
            ),
        });
    }
    // JC0541 (#44): an entity literally named `{X}Request` collides with the
    // `{X}Request` DTO/OpenAPI component generated for an entity `X` that omits a
    // server-owned field. Two `struct XRequest` would fail to compile in genroute and
    // silently overwrite each other in the OpenAPI schema map. Only a REAL collision
    // fires — `X` must actually mint the DTO (db mode + a server-owned omission) — so
    // an ordinary `*Request` name that shadows nothing is never rejected.
    if let Some(conflict) = request_dto_name_collision(d) {
        return Some(conflict);
    }
    // JC0542 (#65): sibling routes that name a shared path position's `{param}`
    // differently panic at `App::build` (JC0500), a clean-scaffold-then-mid-test
    // failure. Caught here as a fatal conflict — it needs a rename or a restructure.
    if let Some(conflict) = router_param_conflict(d) {
        return Some(conflict);
    }
    None
}

/// The JC0542 check (issue #65): the runtime router keys each path segment
/// position by a SINGLE `{param}` name (see `jerrycan-core` `router::Trie::insert`
/// — one global trie backs the whole app, so this spans every module + subroute).
/// Two routes that reach the same position through an identical static/param
/// prefix but name that position's parameter differently (`/tickets/{id}` vs
/// `/tickets/{ticket_id}/comments`) make `App::build` abort with JC0500
/// `conflicting path parameters` — after a clean scaffold, mid-test.
///
/// This is a structure-only twin of `router::Trie`, mirroring its insert EXACTLY
/// so the validator neither rejects a design the router accepts nor accepts one it
/// panics on: a static segment and a param segment DIVERGE into different children
/// (`/users/me` + `/users/{id}` is fine), two DIFFERENT literals diverge, the SAME
/// param name at a position agrees (`/{id}` + `/{id}/comments` is fine), and only
/// two DIFFERENT param names at the same node conflict. Analyzed over the
/// mount-resolved route table (`genroute::route_map`), so subroute-mount params
/// (which occupy real positions) are included.
fn router_param_conflict(d: &Design) -> Option<DesignConflict> {
    /// A structure-only twin of `router::Node`: static children keyed by literal,
    /// plus at most ONE param slot carrying its name and the first route to set it.
    #[derive(Default)]
    struct Node {
        statics: std::collections::HashMap<String, Node>,
        param: Option<(String, String, Box<Node>)>,
    }
    let mut root = Node::default();
    for entry in super::genroute::route_map(d) {
        let path = entry.path;
        let mut node = &mut root;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                // Mirror router.rs: ensure the param slot, then compare its name.
                if node.param.is_none() {
                    node.param = Some((name.to_string(), path.clone(), Box::default()));
                }
                let (existing, first_route, child) = node.param.as_mut().expect("just ensured");
                if existing != name {
                    return Some(DesignConflict {
                        code: "JC0542",
                        message: format!(
                            "routes `{first_route}` and `{path}` both take a path parameter at the same position but name it differently (`{{{existing}}}` vs `{{{name}}}`) — the router keys each path position by a single parameter name, so registering both aborts `App::build` at startup with JC0500 `conflicting path parameters` (after a clean scaffold, mid-test). Unify the name (use `{{{existing}}}` in BOTH routes, or `{{{name}}}` in both), or restructure so the position is not shared (mount the diverging routes under distinct static prefixes). See `jerrycan explain JC0542`."
                        ),
                        hint: format!(
                            "give the shared segment ONE parameter name across every sibling route (rename `{{{name}}}`→`{{{existing}}}` or vice versa), or restructure the nesting so the position is not shared"
                        ),
                    });
                }
                node = child;
            } else {
                node = node.statics.entry(seg.to_string()).or_default();
            }
        }
    }
    None
}

/// The JC0541 check (issue #44): find an entity literally named `{base}Request`
/// whose `{base}` sibling generates a `{base}Request` DTO. Returns the collision, or
/// `None` when no `*Request` entity shadows a generated DTO name.
fn request_dto_name_collision(d: &Design) -> Option<DesignConflict> {
    fn collect<'a>(m: &'a ModuleDesign, out: &mut Vec<&'a str>) {
        out.extend(m.entities.iter().map(|e| e.name.as_str()));
        for sub in &m.subroutes {
            collect(sub, out);
        }
    }
    let mut names = Vec::new();
    for m in &d.modules {
        collect(m, &mut names);
    }
    for name in &names {
        let Some(base) = name.strip_suffix("Request") else {
            continue;
        };
        if base.is_empty() || !names.contains(&base) || !d.entity_generates_request_dto(base) {
            continue;
        }
        return Some(DesignConflict {
            code: "JC0541",
            message: format!(
                "entity `{name}` collides with the request DTO generated for entity `{base}`: a `{base}` request body that omits a server-owned field (an identity fk, a `default`, or a path-redundant parent fk) emits a `{base}Request` type — a Rust struct AND an OpenAPI `{base}Request` component. With an entity also literally named `{name}`, genroute would define `struct {name}` twice (a compile error) and the OpenAPI document would clobber one schema with the other. Rename the entity (e.g. `{base}Payload` or `{base}Submission`) so it no longer shadows the generated DTO. See `jerrycan explain JC0541`."
            ),
            hint: format!(
                "rename `{name}` (e.g. `{base}Payload`/`{base}Submission`) — the `{base}Request` name is reserved for the generated request DTO"
            ),
        });
    }
    None
}

/// Validate a parsed design. Empty result == complete (status: "complete").
pub fn validate(d: &Design) -> Vec<Question> {
    let mut qs = Vec::new();

    if !is_kebab(&d.name) {
        qs.push(q(
            "/name",
            format!(
                "`{}` is not kebab-case (^[a-z][a-z0-9-]*$) — what should the app be called?",
                d.name
            ),
        ));
    }
    if d.contract_version > 2 {
        qs.push(q(
            "/contract_version",
            "contract_version must be 0, 1, or 2 for this platform version.",
        ));
    }
    if d.modules.is_empty() {
        qs.push(q(
            "/modules",
            "No modules defined — what are the resource areas of this backend (each becomes a route crate)?",
        ));
    }
    // A top-level base_path is emitted verbatim into every mount, so it must be a
    // clean absolute path (like a module mount). Empty/`/` is a documented no-op.
    if let Some(base) = &d.base_path
        && !base.is_empty()
        && base != "/"
    {
        if !base.starts_with('/') {
            qs.push(q(
                "/base_path",
                format!("App base_path `{base}` must start with '/'."),
            ));
        }
        if base.contains("//") || base.ends_with('/') {
            qs.push(q(
                "/base_path",
                format!(
                    "App base_path `{base}` must not contain `//` or end with a trailing slash."
                ),
            ));
        }
    }

    // The `cors` block is emitted into `App::cors(CorsConfig::new(..))` (issue #21).
    // Validate the origins at design time so a misconfig is a pointed question, not
    // a runtime `App::build()` failure the deploy discovers on first boot.
    if let Some(cors) = &d.cors {
        if cors.origins.is_empty() {
            qs.push(q(
                "/cors/origins",
                "CORS is declared with no origins — list the allowed origins (exact scheme://host[:port]) or `*` for any origin.",
            ));
        }
        let is_wildcard = cors.origins.iter().any(|o| o == "*");
        if is_wildcard && cors.origins.len() > 1 {
            qs.push(q(
                "/cors/origins",
                "CORS origins mixes `*` with explicit origins — use either `*` (any origin) alone or an explicit allowlist.",
            ));
        }
        // Fetch spec: a credentialed cross-origin request cannot use a wildcard
        // origin. Core's `App::build` rejects the combination; catch it here so the
        // generated app never fails to boot on it.
        if is_wildcard && cors.allow_credentials {
            qs.push(q(
                "/cors/allow_credentials",
                "CORS allow_credentials cannot be combined with `*` origins (the Fetch spec forbids it) — list explicit origins instead.",
            ));
        }
        // Each explicit origin must be a bare origin (scheme://host[:port]) — no path
        // or trailing slash — since it is matched byte-for-byte against the request's
        // Origin header.
        for (i, o) in cors.origins.iter().enumerate() {
            if o == "*" {
                continue;
            }
            let well_formed = (o.starts_with("http://") || o.starts_with("https://"))
                && !o.ends_with('/')
                && o.matches('/').count() == 2;
            if !well_formed {
                qs.push(q(
                    format!("/cors/origins/{i}"),
                    format!("CORS origin `{o}` is not a bare origin — use scheme://host[:port] with no path or trailing slash (e.g. https://app.example)."),
                ));
            }
        }
    }

    let declared_roles: Vec<&str> = d
        .auth
        .as_ref()
        .map(|a| a.roles.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let auth_declared = d.auth.is_some();

    let mut seen_module_names = std::collections::HashSet::new();
    for (i, m) in d.modules.iter().enumerate() {
        if !seen_module_names.insert(m.name.as_str()) {
            qs.push(q(
                format!("/modules/{i}/name"),
                format!(
                    "Module name `{}` is already used — module names must be unique.",
                    m.name
                ),
            ));
        }
        validate_module(
            m,
            &format!("/modules/{i}"),
            &declared_roles,
            auth_declared,
            &mut qs,
        );
    }

    // Role coherence: a guarded endpoint (auth_required or required_roles) needs
    // an active auth model — `auth.model: none`/absent can't resolve a session.
    if !d.wants_auth() {
        fn check_guards(m: &ModuleDesign, ptr: &str, qs: &mut Vec<Question>) {
            for (i, ep) in m.endpoints.iter().enumerate() {
                if ep.is_guarded() {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        format!(
                            "Endpoint `{}` is guarded (auth_required/required_roles) but the design has no active auth — set auth.model to `session` or `jwt` first.",
                            ep.operation_id
                        ),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_guards(sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_guards(m, &format!("/modules/{i}"), &mut qs);
        }
    }

    if d.wants_db() {
        // contract v1 stores json as a real column (Json); v0 has no json columns,
        // so a json field there is still an unsupported request.
        let json_ok = d.contract_version >= 1;
        fn check_db_fields(m: &ModuleDesign, ptr: &str, json_ok: bool, qs: &mut Vec<Question>) {
            for (i, e) in m.entities.iter().enumerate() {
                for (j, f) in e.fields.iter().enumerate() {
                    if !json_ok && matches!(f.field_type, FieldType::Json) {
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/type"),
                            format!("Field `{}` has type json — json fields are not yet supported in db mode (store as string, or drop the db dependency; structured json columns are a contract-v1 candidate).", f.name),
                        ));
                    } else if f.name == "id"
                        && !matches!(
                            f.field_type,
                            FieldType::Integer | FieldType::String | FieldType::Uuid
                        )
                    {
                        // A declared `id` becomes the table's primary key.
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/type"),
                            format!("Field `id` of entity `{}` becomes the table's primary key in db mode — it must be integer, string, or uuid.", e.name),
                        ));
                    }
                }
                // The fk column a belongs_to derives is generated; an explicit field
                // of the same name would collide with the derived column.
                for b in &e.belongs_to {
                    let derived = Design::fk_column(&b.entity);
                    if let Some(j) = e.fields.iter().position(|f| f.name == derived) {
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}"),
                            format!(
                                "Field `{derived}` collides with the fk column derived from belongs_to `{}` — the fk column is derived from belongs_to; remove the explicit field or the belongs_to.",
                                b.entity
                            ),
                        ));
                    }
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_db_fields(sub, &format!("{ptr}/subroutes/{i}"), json_ok, qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_db_fields(m, &format!("/modules/{i}"), json_ok, &mut qs);
        }
    }

    // Contract v1 constructs: belongs_to targets and enum-value placement.
    // Collect every declared entity name (modules + subroutes) so a belongs_to
    // may target any entity anywhere in the design.
    let mut entity_names = std::collections::HashSet::new();
    fn collect_entity_names<'a>(m: &'a ModuleDesign, out: &mut std::collections::HashSet<&'a str>) {
        for e in &m.entities {
            out.insert(e.name.as_str());
        }
        for sub in &m.subroutes {
            collect_entity_names(sub, out);
        }
    }
    for m in &d.modules {
        collect_entity_names(m, &mut entity_names);
    }

    fn check_relations_and_enums(
        m: &ModuleDesign,
        ptr: &str,
        entity_names: &std::collections::HashSet<&str>,
        wants_db: bool,
        qs: &mut Vec<Question>,
    ) {
        for (i, e) in m.entities.iter().enumerate() {
            for (k, b) in e.belongs_to.iter().enumerate() {
                if !entity_names.contains(b.entity.as_str()) {
                    qs.push(q(
                        format!("{ptr}/entities/{i}/belongs_to/{k}"),
                        format!(
                            "belongs_to target `{}` is not a declared entity anywhere in the design — define it or fix the reference.",
                            b.entity
                        ),
                    ));
                }
            }
            for (j, f) in e.fields.iter().enumerate() {
                if let Some(ref values) = f.values {
                    if !matches!(f.field_type, FieldType::String) {
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/values"),
                            format!(
                                "Field `{}` declares enum `values` but its type is not string — enum values are only allowed on string fields.",
                                f.name
                            ),
                        ));
                    } else if values.is_empty() {
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/values"),
                            format!(
                                "Field `{}` declares an empty `values` list — list at least one allowed value or drop the field.",
                                f.name
                            ),
                        ));
                    } else if let Some(bad) = values.iter().find(|v| !is_enum_value(v)) {
                        // JC0543 (#54): enum values are interpolated UNESCAPED into
                        // generated Rust (the deserialize allow-list + 422 text in
                        // genroute, the testgen fixture), so a quote or backslash
                        // emits a crate that won't compile far from the design.
                        // Constrain to an identifier-ish shape (which also excludes
                        // spaces etc. under the same interpolation-safety rule).
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/values"),
                            format!(
                                "Field `{}` enum value `{bad}` is not an identifier (^[A-Za-z0-9_-]+$) — enum values are interpolated unescaped into generated Rust (the deserialize allow-list, the 422 error text, and the test fixtures), so a quote or backslash emits a crate that fails to compile; other non-identifier characters are rejected under the same rule. Use identifier-shaped values (letters, digits, `_`, `-`). See `jerrycan explain JC0543`.",
                                f.name
                            ),
                        ));
                    }
                }
                // A server-owned `default` (issue #53a) must type-check against the
                // field type (and enum membership) — the server writes it verbatim
                // into a NOT-NULL column, so a mistyped literal would fail at run
                // time, not design time.
                if let Some(msg) = default_type_error(f, wants_db) {
                    qs.push(q(format!("{ptr}/entities/{i}/fields/{j}/default"), msg));
                }
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_relations_and_enums(
                sub,
                &format!("{ptr}/subroutes/{i}"),
                entity_names,
                wants_db,
                qs,
            );
        }
    }
    let wants_db = d.wants_db();
    for (i, m) in d.modules.iter().enumerate() {
        check_relations_and_enums(
            m,
            &format!("/modules/{i}"),
            &entity_names,
            wants_db,
            &mut qs,
        );
    }

    // JC0544 (#60): a body-carrying create/update endpoint whose entity has a
    // path-redundant parent fk (R5's `entity_path_fk_columns`) but whose OWN path
    // lacks the matching `{param}`. The request DTO is per-entity, so the fk is
    // dropped for EVERY create of the entity; on a route that doesn't carry it in
    // the path the NOT-NULL column can be set from neither the body nor the path —
    // the route is un-implementable (the stub even references a `_{col}` binding
    // that doesn't exist). Reuses the R5 resolution — no duplicated fk logic.
    fn check_dual_create_path_fk(d: &Design, m: &ModuleDesign, ptr: &str, qs: &mut Vec<Question>) {
        for (i, ep) in m.endpoints.iter().enumerate() {
            let Some(rb) = ep.request_body.as_ref() else {
                continue;
            };
            if !matches!(
                ep.method,
                HttpMethod::POST | HttpMethod::PUT | HttpMethod::PATCH
            ) {
                continue;
            }
            let token = |col: &str| ep.path.contains(&format!("{{{col}}}"));
            if let Some(col) = d
                .entity_path_fk_columns(&rb.entity)
                .into_iter()
                .find(|col| !token(col))
            {
                qs.push(q(
                    format!("{ptr}/endpoints/{i}"),
                    format!(
                        "Endpoint `{}` ({:?} {}) creates `{}`, whose parent foreign key `{col}` is supplied by a path parameter on a sibling nested route — so the generated `{}Request` body drops `{col}`, but this route's own path has no `{{{col}}}` to inject it from. The NOT-NULL `{col}` can be set from neither the body nor the path, so the route is un-implementable. Add `{{{col}}}` to this endpoint's path (mount it under the parent), or split `{}` into a separate entity for the standalone create so its request body keeps `{col}`. See `jerrycan explain JC0544`.",
                        ep.operation_id, ep.method, ep.path, rb.entity, rb.entity, rb.entity
                    ),
                ));
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_dual_create_path_fk(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
        }
    }
    for (i, m) in d.modules.iter().enumerate() {
        check_dual_create_path_fk(d, m, &format!("/modules/{i}"), &mut qs);
    }

    // Tenancy: the named entity must resolve, and the Tenant guard needs an
    // authenticated user to scope by.
    if let Some(ref tenancy) = d.tenancy {
        if !entity_names.contains(tenancy.entity.as_str()) {
            qs.push(q(
                "/tenancy/entity",
                format!(
                    "Tenancy entity `{}` is not a declared entity — define it or fix the reference.",
                    tenancy.entity
                ),
            ));
        }
        // The Tenant guard scopes by the authenticated principal, which only an
        // active auth *model* (session/jwt) produces — the bare `auth` dependency
        // stub does not.
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if !active_auth_model {
            qs.push(q(
                "/tenancy",
                "Tenancy is declared but the design has no active auth model — the Tenant guard needs an authenticated user; set auth.model to `session` or `jwt` first.",
            ));
        }

        // A `public` endpoint bypasses every guard — including the Tenant guard
        // that scopes a tenant-owned entity to its owner. If such an endpoint's
        // repo entity belongs_to the tenancy root, marking it public would expose
        // one tenant's rows to anyone. Flag the contradiction.
        fn check_public_on_tenant_owned(
            d: &Design,
            m: &ModuleDesign,
            ptr: &str,
            qs: &mut Vec<Question>,
        ) {
            for (i, ep) in m.endpoints.iter().enumerate() {
                // Tenant-owned directly OR transitively (#102): `tenant_path` resolves
                // a grandchild through its parent chain, so a public endpoint on a
                // deeply-owned entity is flagged too — matching the transitive
                // ownership the guard/lint recognize.
                if ep.public
                    && endpoint_repo_entity(m, ep).is_some_and(|name| d.tenant_path(name).is_some())
                {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        "endpoint is public but its entity is tenant-owned — public endpoints bypass the Tenant guard; remove public or move the endpoint off the tenant-owned entity".to_string(),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_public_on_tenant_owned(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_public_on_tenant_owned(d, m, &format!("/modules/{i}"), &mut qs);
        }

        // JC0545 (#102): an entity that reaches the tenant through TWO or more
        // distinct `belongs_to` chains (a diamond) is ambiguous — `tenant_path`
        // resolves it to `None`, which would leave it UNSCOPED and re-open the
        // cross-tenant leak. Generation is gated on validation, so rejecting the
        // design here keeps a half-scoped entity from ever reaching the generator.
        fn check_ambiguous_tenant_path(
            d: &Design,
            m: &ModuleDesign,
            ptr: &str,
            qs: &mut Vec<Question>,
        ) {
            for (i, e) in m.entities.iter().enumerate() {
                if d.tenant_path_branch_count(&e.name) >= 2 {
                    qs.push(q(
                        format!("{ptr}/entities/{i}"),
                        format!(
                            "Entity `{}` reaches the tenant through more than one `belongs_to` path (a diamond graph), so jerrycan cannot decide which chain defines tenant ownership — guessing would scope its reads/writes to the wrong tenant and re-open the cross-tenant leak. Collapse its tenant ownership to a SINGLE `belongs_to` path (drop the redundant parent, or split the entity), so exactly one chain reaches the tenant. See `jerrycan explain JC0545`.",
                            e.name
                        ),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_ambiguous_tenant_path(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_ambiguous_tenant_path(d, m, &format!("/modules/{i}"), &mut qs);
        }
    }

    // Jobs require a database: the engine's default store is Postgres and the
    // generated `jobs(db)` wiring + JOBS_MIGRATIONS run over `jerrycan::db::Db`.
    // A jobs-without-db design can't compile, so reject it here (one error for the
    // whole jobs list, not per-job). The shallow cron-shape check below stays as
    // the design-time guard; the engine deep-parses each expression at serve and
    // fails loud (`Jobs::cron` panics on a bad expression), so a malformed-but-
    // cron-shaped schedule is caught there rather than adding a jerrycan-jobs dep
    // to the CLI just for validation.
    if d.wants_jobs() && !d.wants_db() {
        qs.push(q(
            "/jobs".to_string(),
            "Jobs require a database dependency — add `db` to `dependencies` (background jobs run over a Postgres store).".to_string(),
        ));
    }

    // Jobs: snake_case unique names; a present schedule must look cron-shaped
    // (full cron parsing arrives with the engine in v2.3).
    let mut seen_job_names = std::collections::HashSet::new();
    for (i, job) in d.jobs.iter().enumerate() {
        if !is_snake(&job.name) {
            qs.push(q(
                format!("/jobs/{i}/name"),
                format!(
                    "Job name `{}` must be snake_case (^[a-z][a-z0-9_]*$).",
                    job.name
                ),
            ));
        }
        if !seen_job_names.insert(job.name.as_str()) {
            qs.push(q(
                format!("/jobs/{i}/name"),
                format!(
                    "Job name `{}` is already used — job names must be unique.",
                    job.name
                ),
            ));
        }
        // The queue is interpolated RAW into generated Rust string literals
        // (`.queue("{q}", ...)` / `.cron(..., "{queue}")` in jobsgen.rs), so a
        // queue with a `"` (or any non-identifier char) breaks the generated
        // crate at build time, far from the design. Validate it like every other
        // identifier interpolated into generated Rust (is_snake job names, etc.).
        if let Some(ref queue) = job.queue
            && !is_snake(queue)
        {
            qs.push(q(
                format!("/jobs/{i}/queue"),
                format!("Job queue `{queue}` must be snake_case (^[a-z][a-z0-9_]*$)."),
            ));
        }
        if let Some(ref schedule) = job.schedule {
            let fields: Vec<&str> = schedule.split_whitespace().collect();
            let cron_shaped = fields.len() == 5
                && fields.iter().all(|f| {
                    !f.is_empty()
                        && f.chars()
                            .all(|c| c.is_ascii_digit() || matches!(c, '*' | ',' | '/' | '-'))
                });
            if !cron_shaped {
                qs.push(q(
                    format!("/jobs/{i}/schedule"),
                    format!(
                        "Schedule `{schedule}` is not a 5-field cron expression (minute hour day month weekday, each [0-9*,/-]).",
                    ),
                ));
            }
        }
    }

    // Storage (contract v2). Bucket names/mime patterns are interpolated into
    // generated Rust literals and mounts, so everything is validated up front
    // (the job-queue precedent: reject at design time, not at generated-crate
    // build time). NOTE: `visibility: public` + a tenant-scoped owner is
    // deliberately allowed (public read, scoped write) — no question.
    if let Some(ref storage) = d.storage {
        if d.contract_version < 2 {
            qs.push(q(
                "/storage",
                "The storage block requires contract_version 2 — bump contract_version (v0/v1 designs stay valid without storage).",
            ));
        }
        if !d.wants_db() {
            qs.push(q(
                "/storage",
                "Storage requires a database dependency — add `db` to `dependencies` (object metadata lives in the storage_objects table).",
            ));
        }
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if !active_auth_model {
            qs.push(q(
                "/storage",
                "Storage requires an active auth model — bucket mutations (upload/delete/sign) are always guarded; set auth.model to `session` or `jwt`.",
            ));
        }
        let module_mounts: std::collections::HashSet<String> =
            d.modules.iter().map(|m| m.effective_mount()).collect();
        // A custom base_path is emitted verbatim into every bucket mount, so it
        // must be a clean absolute path (leading `/`, no trailing/`//`), like a
        // module mount.
        if let Some(base) = &storage.base_path {
            if !base.starts_with('/') {
                qs.push(q(
                    "/storage/base_path",
                    format!("Storage base_path `{base}` must start with '/'."),
                ));
            }
            if base.contains("//") || (base.len() > 1 && base.ends_with('/')) {
                qs.push(q(
                    "/storage/base_path",
                    format!("Storage base_path `{base}` must not contain `//` or end with a trailing slash."),
                ));
            }
        }
        let base_path = storage.effective_base_path();
        let mut seen_buckets = std::collections::HashSet::new();
        for (i, b) in storage.buckets.iter().enumerate() {
            let bptr = format!("/storage/buckets/{i}");
            if !is_kebab(&b.name) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` is not kebab-case (^[a-z][a-z0-9-]*$).", b.name),
                ));
            }
            let ident = b.name.replace('-', "_");
            if is_rust_keyword(&ident) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` becomes the Rust module `{ident}`, which is a keyword — rename it.", b.name),
                ));
            }
            if !seen_buckets.insert(b.name.as_str()) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!(
                        "Bucket name `{}` is already used — bucket names must be unique.",
                        b.name
                    ),
                ));
            }
            let bucket_mount = format!("{base_path}/{}", b.name);
            if module_mounts.contains(&bucket_mount) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` mounts at {bucket_mount} which collides with a module mount — rename the bucket, change storage.base_path, or remount the module.", b.name),
                ));
            }
            if let Some(ref owner) = b.owner
                && !entity_names.contains(owner.as_str())
            {
                qs.push(q(
                    format!("{bptr}/owner"),
                    format!("Bucket owner `{owner}` is not a declared entity anywhere in the design — define it or fix the reference."),
                ));
            }
            if b.owner_prefix && b.owner.is_none() {
                qs.push(q(
                    format!("{bptr}/owner_prefix"),
                    format!("Bucket `{}` sets owner_prefix without an owner — owner_prefix stores keys under {{owner_id}}/… and needs `owner`.", b.name),
                ));
            }
            if let Some(ref max) = b.max_size
                && Design::parse_size(max).is_none()
            {
                qs.push(q(
                    format!("{bptr}/max_size"),
                    format!(
                        "max_size `{max}` is not a size — use ^[0-9]+(B|KB|MB|GB)?$ (e.g. \"5MB\")."
                    ),
                ));
            }
            for (j, m) in b.allowed_mime.iter().enumerate() {
                // The runtime matcher understands exactly type/subtype, type/*
                // and */*. A wildcard TYPE with a concrete subtype (`*/png`)
                // would parse here but can never match — every upload would
                // 415 — so it is rejected as malformed too.
                let well_formed = m.split_once('/').is_some_and(|(t, sub)| {
                    let seg_ok = |s: &str| {
                        !s.is_empty()
                            && s.bytes().all(|c| {
                                c.is_ascii_lowercase()
                                    || c.is_ascii_digit()
                                    || matches!(c, b'.' | b'+' | b'-')
                            })
                    };
                    (seg_ok(t) && (seg_ok(sub) || sub == "*")) || (t == "*" && sub == "*")
                });
                if !well_formed {
                    qs.push(q(
                        format!("{bptr}/allowed_mime/{j}"),
                        format!(
                            "`{m}` is not a supported mime pattern — use type/subtype, type/* or */* (lowercase)."
                        ),
                    ));
                }
            }
        }
    }

    // Realtime (contract v2). Channel names/entities are interpolated into
    // generated wiring, so everything is validated up front. Scope-filtered
    // delivery of changes is the security pillar, so changes require an active
    // auth model; tenant-scoped topics require tenancy.
    if let Some(ref rt) = d.realtime {
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if d.contract_version < 2 {
            qs.push(q(
                "/realtime",
                "The realtime block requires contract_version 2 — bump contract_version (v0/v1 designs stay valid without realtime).",
            ));
        }
        if !d.wants_db() {
            qs.push(q(
                "/realtime",
                "Realtime requires a database dependency — add `db` to `dependencies` (Changes stream from Postgres).",
            ));
        }
        // Changes entities must exist and require an active auth model (delivery
        // is scope-filtered by the authenticated principal).
        if !rt.changes.is_empty() && !active_auth_model {
            qs.push(q(
                "/realtime/changes",
                "Realtime changes delivery is scope-filtered by the authenticated principal — set auth.model to `session` or `jwt`.",
            ));
        }
        for (i, entity) in rt.changes.iter().enumerate() {
            if !entity_names.contains(entity.as_str()) {
                qs.push(q(
                    format!("/realtime/changes/{i}"),
                    format!("Realtime changes entity `{entity}` is not a declared entity anywhere in the design — define it or fix the reference."),
                ));
            }
        }
        // Broadcast + presence topics: snake_case, unique within their list,
        // tenant scope needs tenancy, and any non-none scope needs auth.
        let mut check_topics = |topics: &[RealtimeTopic], kind: &str| {
            let mut seen = std::collections::HashSet::new();
            for (i, t) in topics.iter().enumerate() {
                let tptr = format!("/realtime/{kind}/{i}");
                if !is_snake(&t.name) {
                    qs.push(q(
                        format!("{tptr}/name"),
                        format!(
                            "Realtime {kind} topic `{}` is not snake_case (^[a-z][a-z0-9_]*$).",
                            t.name
                        ),
                    ));
                }
                if !seen.insert(t.name.as_str()) {
                    qs.push(q(
                        format!("{tptr}/name"),
                        format!("Realtime {kind} topic name `{}` is already used — topic names must be unique.", t.name),
                    ));
                }
                if t.scope == RealtimeScope::Tenant && d.tenancy.is_none() {
                    qs.push(q(
                        tptr.clone(),
                        format!("Realtime {kind} topic `{}` is tenant-scoped but the design has no tenancy — declare `tenancy` or use scope `auth`/`none`.", t.name),
                    ));
                }
                if t.scope != RealtimeScope::None && !active_auth_model {
                    qs.push(q(
                        tptr,
                        format!("Realtime {kind} topic `{}` needs an active auth model for its scope — set auth.model to `session` or `jwt` (or use scope `none`).", t.name),
                    ));
                }
            }
        };
        check_topics(&rt.broadcast, "broadcast");
        check_topics(&rt.presence, "presence");
    }

    qs
}

fn validate_module(
    m: &ModuleDesign,
    ptr: &str,
    declared_roles: &[&str],
    auth_declared: bool,
    qs: &mut Vec<Question>,
) {
    if !is_kebab(&m.name) {
        qs.push(q(
            format!("{ptr}/name"),
            format!("Module `{}` is not kebab-case — rename it.", m.name),
        ));
    }
    if let Some(ref mount) = m.mount {
        if !mount.starts_with('/') {
            qs.push(q(
                format!("{ptr}/mount"),
                format!("Mount `{mount}` must start with '/'."),
            ));
        }
        if mount.contains("//") || (mount.len() > 1 && mount.ends_with('/')) {
            qs.push(q(
                format!("{ptr}/mount"),
                format!("Mount `{mount}` must not contain `//` or end with a trailing slash."),
            ));
        }
    }
    for (i, e) in m.entities.iter().enumerate() {
        if !is_pascal(&e.name) {
            qs.push(q(
                format!("{ptr}/entities/{i}/name"),
                format!("Entity `{}` must be PascalCase.", e.name),
            ));
        }
        if is_rust_keyword(&e.name) {
            qs.push(q(
                format!("{ptr}/entities/{i}/name"),
                format!(
                    "Entity `{}` is a Rust keyword — it becomes a module/type name that no raw identifier can escape; rename it (e.g. a domain-specific name).",
                    e.name
                ),
            ));
        }
        // An explicit `table` override is used VERBATIM in DDL/queries, so it must
        // be a safe snake_case identifier — reject anything else up front.
        if let Some(table) = &e.table
            && !is_snake(table)
        {
            qs.push(q(
                format!("{ptr}/entities/{i}/table"),
                format!("Table override `{table}` must be snake_case (^[a-z][a-z0-9_]*$)."),
            ));
        }
        if e.fields.is_empty() {
            qs.push(q(
                format!("{ptr}/entities/{i}/fields"),
                format!(
                    "Entity `{}` has no fields — what data does it carry?",
                    e.name
                ),
            ));
        }
        for (j, f) in e.fields.iter().enumerate() {
            if !is_snake(&f.name) {
                qs.push(q(
                    format!("{ptr}/entities/{i}/fields/{j}/name"),
                    format!("Field `{}` must be snake_case.", f.name),
                ));
            }
            // A keyword field name is fine: codegen emits it as a raw identifier
            // (`type` → `r#type`) with a `#[serde(rename)]` so the wire name is
            // unchanged — a frozen external contract keeps its `type`/`match`/
            // `ref` field. Only `crate`/`self`/`super`, which no `r#` can escape,
            // are still rejected.
            if !can_be_rust_ident(&f.name) {
                qs.push(q(
                    format!("{ptr}/entities/{i}/fields/{j}/name"),
                    format!(
                        "Field `{name}` is a Rust keyword that no raw identifier can escape — rename (e.g. `{name}_field` or a domain-specific name).",
                        name = f.name
                    ),
                ));
            }
        }
    }
    if m.endpoints.is_empty() {
        qs.push(q(
            format!("{ptr}/endpoints"),
            format!(
                "Module `{}` has no endpoints — what operations does it expose?",
                m.name
            ),
        ));
    }

    let entity_names: Vec<&str> = m.entities.iter().map(|e| e.name.as_str()).collect();
    let mut seen_ops = std::collections::HashSet::new();
    let mut seen_routes = std::collections::HashSet::new();
    for (i, ep) in m.endpoints.iter().enumerate() {
        let eptr = format!("{ptr}/endpoints/{i}");
        if !is_snake(&ep.operation_id) {
            qs.push(q(
                format!("{eptr}/operation_id"),
                format!(
                    "operation_id `{}` must be snake_case (it becomes the handler fn name).",
                    ep.operation_id
                ),
            ));
        }
        if !seen_ops.insert(ep.operation_id.as_str()) {
            qs.push(q(
                format!("{eptr}/operation_id"),
                format!(
                    "operation_id `{}` is not unique within module `{}` — handler names must be unique.",
                    ep.operation_id, m.name
                ),
            ));
        }
        if !ep.path.starts_with('/') {
            qs.push(q(
                format!("{eptr}/path"),
                format!("Path `{}` must start with '/'.", ep.path),
            ));
        }
        let param_count = ep.path.matches('{').count();
        if param_count > 3 {
            qs.push(q(format!("{eptr}/path"), format!("Path `{}` has {param_count} parameters — at most three path parameters per endpoint are supported. Split the route or use a subroute.", ep.path)));
        }
        if ep.path.matches('{').count() != ep.path.matches('}').count() {
            qs.push(q(
                format!("{eptr}/path"),
                format!("Path `{}` has unbalanced braces.", ep.path),
            ));
        }
        if !seen_routes.insert((ep.method, ep.path.as_str())) {
            qs.push(q(
                format!("{eptr}/path"),
                format!(
                    "{:?} {} is already registered in module `{}` — routes must be unique.",
                    ep.method, ep.path, m.name
                ),
            ));
        }
        // Success is 2xx, or 3xx for a redirect endpoint (e.g. an OAuth
        // `connect` that 302s the browser to the provider). 1xx/4xx/5xx are not
        // success classes.
        if !(200..=399).contains(&ep.success.status) {
            qs.push(q(
                format!("{eptr}/success/status"),
                format!("Success status {} is not 2xx/3xx.", ep.success.status),
            ));
        }
        if let Some(ref ent) = ep.success.entity
            && !entity_names.contains(&ent.as_str())
        {
            qs.push(q(
                format!("{eptr}/success/entity"),
                format!(
                    "Entity `{ent}` is not defined in module `{}` — define it or fix the reference.",
                    m.name
                ),
            ));
        }
        if let Some(ref rb) = ep.request_body
            && !entity_names.contains(&rb.entity.as_str())
        {
            qs.push(q(
                format!("{eptr}/request_body/entity"),
                format!(
                    "Entity `{}` is not defined in module `{}` — define it or fix the reference.",
                    rb.entity, m.name
                ),
            ));
        }
        for (j, ec) in ep.errors.iter().enumerate() {
            if !(400..=599).contains(&ec.status) {
                qs.push(q(
                    format!("{eptr}/errors/{j}/status"),
                    format!("Error status {} is not 4xx/5xx.", ec.status),
                ));
            }
            if let Some(ref code) = ec.code {
                let ok = code.len() == 6
                    && code.starts_with("JC")
                    && code[2..].chars().all(|c| c.is_ascii_digit());
                if !ok {
                    qs.push(q(
                        format!("{eptr}/errors/{j}/code"),
                        format!("`{code}` does not match ^JC[0-9]{{4}}$."),
                    ));
                }
            }
        }
        for role in &ep.required_roles {
            if !declared_roles.contains(&role.as_str()) {
                let hint = if auth_declared {
                    "add it to auth.roles or fix the reference"
                } else {
                    "declare auth { model, roles } first"
                };
                qs.push(q(
                    format!("{eptr}/required_roles"),
                    format!("Role `{role}` is not declared in auth.roles — {hint}."),
                ));
            }
        }
        // `public` marks a genuinely unauthenticated route (login/register); it
        // contradicts any guard. Flag the combination so a design can't claim both.
        if ep.public && ep.auth_required {
            qs.push(q(
                eptr.clone(),
                format!(
                    "Endpoint `{}` is marked public but also auth_required — a public route is unauthenticated by design; drop one.",
                    ep.operation_id
                ),
            ));
        }
        if ep.public && !ep.required_roles.is_empty() {
            qs.push(q(
                eptr.clone(),
                format!(
                    "Endpoint `{}` is marked public but declares required_roles — a public route is unauthenticated by design; drop the roles or the public flag.",
                    ep.operation_id
                ),
            ));
        }
    }

    for (i, sub) in m.subroutes.iter().enumerate() {
        validate_module(
            sub,
            &format!("{ptr}/subroutes/{i}"),
            declared_roles,
            auth_declared,
            qs,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::{MINIMAL, V1_FULL, V2_REALTIME, V2_STORAGE};

    fn design(json: &str) -> Design {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn valid_realtime_design_is_question_free() {
        let d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn realtime_requires_contract_v2() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.contract_version = 1;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime" && q.question.contains("contract_version"))
        );
    }

    #[test]
    fn realtime_changes_entities_must_exist() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.realtime.as_mut().unwrap().changes[0] = "Ghost".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime/changes/0" && q.question.contains("Ghost"))
        );
    }

    #[test]
    fn realtime_requires_db_and_changes_require_active_auth() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.dependencies.retain(|x| x != "db");
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime" && q.question.contains("db"))
        );

        let mut d2: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d2.auth = None;
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/realtime/changes" && q.question.contains("auth"))
        );
    }

    #[test]
    fn tenant_scoped_topics_require_tenancy_and_snake_case_unique_names() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.tenancy = None;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime/broadcast/0" && q.question.contains("tenancy"))
        );

        let mut d2: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d2.realtime.as_mut().unwrap().broadcast.push(RealtimeTopic {
            name: "Deal-Room".into(),
            scope: RealtimeScope::None,
        });
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/realtime/broadcast/1/name" && q.question.contains("snake_case"))
        );

        let mut d3: Design = serde_json::from_str(V2_REALTIME).unwrap();
        let dup = d3.realtime.as_ref().unwrap().broadcast[0].clone();
        d3.realtime.as_mut().unwrap().broadcast.push(dup);
        assert!(
            validate(&d3)
                .iter()
                .any(|q| q.id == "/realtime/broadcast/1/name" && q.question.contains("unique"))
        );
    }

    #[test]
    fn contract_version_2_is_now_valid_and_3_is_not() {
        let ok: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert!(
            !validate(&ok).iter().any(|q| q.id == "/contract_version"),
            "{:?}",
            validate(&ok)
        );
        let mut bad: Design = serde_json::from_str(V2_STORAGE).unwrap();
        bad.contract_version = 3;
        assert!(validate(&bad).iter().any(|q| q.id == "/contract_version"));
    }

    #[test]
    fn v2_storage_fixture_is_question_free() {
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn storage_requires_contract_v2_db_and_an_active_auth_model() {
        // v1 + storage: rejected (v2 owns the block).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.contract_version = 1;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage" && q.question.contains("contract_version 2"))
        );
        // storage without db: rejected (metadata table).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.dependencies.retain(|dep| dep != "db");
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage" && q.question.contains("db"))
        );
        // storage without an active auth model: rejected (mutations are always guarded).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.auth = None;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage" && q.question.contains("auth"))
        );
    }

    #[test]
    fn bucket_names_owners_and_rules_are_validated() {
        // Bad kebab name.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "Avatars".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name")
        );
        // A name whose snake ident is a Rust keyword breaks the generated crate.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "match".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("keyword"))
        );
        // Duplicate bucket names.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        let dup = d.storage.as_ref().unwrap().buckets[0].clone();
        d.storage.as_mut().unwrap().buckets.push(dup);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/2/name" && q.question.contains("unique"))
        );
        // Unknown owner entity.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].owner = Some("Ghost".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/owner" && q.question.contains("Ghost"))
        );
        // owner_prefix without owner.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[1].owner = None;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/1/owner_prefix")
        );
        // Unparseable max_size.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].max_size = Some("lots".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/max_size")
        );
        // A mime entry that could break generated string literals.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime = vec!["image/\"png".into()];
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/allowed_mime/0")
        );
        // A wildcard TYPE with a concrete subtype (`*/png`) is dead: the
        // runtime matcher only understands `type/subtype`, `type/*` and `*/*`,
        // so `*/png` would silently 415 every upload — reject at design time.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime = vec!["*/png".into()];
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/allowed_mime/0"),
            "*/png must be rejected — it can never match"
        );
        // The supported wildcard shapes stay valid.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime =
            vec!["*/*".into(), "image/*".into(), "application/pdf".into()];
        assert!(
            !validate(&d)
                .iter()
                .any(|q| q.id.starts_with("/storage/buckets/0/allowed_mime")),
            "*/*, type/* and type/subtype are all valid"
        );
    }

    #[test]
    fn bucket_mounts_must_not_collide_with_module_mounts() {
        // WHY: buckets mount at {base_path}/<name> beside the modules — a
        // collision would shadow routes silently at serve time (issue #8). Under
        // the default /storage prefix, a bucket named `avatars` no longer
        // collides with a module at `/orgs`; the collision needs a module mounted
        // at the bucket's actual path (`/storage/avatars`).
        let base: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert!(
            validate(&base).is_empty(),
            "default /storage prefix keeps buckets clear of the /orgs module: {:?}",
            validate(&base)
        );
        // A module remounted onto the bucket's storage path collides.
        let mut d = base.clone();
        d.modules[0].mount = Some("/storage/avatars".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("collides")),
            "a module at /storage/avatars collides with the avatars bucket: {:?}",
            validate(&d)
        );
        // A custom base_path recomputes the collision against the new prefix.
        let mut d2: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d2.storage.as_mut().unwrap().base_path = Some("/files".into());
        d2.modules[0].mount = Some("/files/avatars".into());
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("collides")),
            "collision follows the custom base_path: {:?}",
            validate(&d2)
        );
    }

    #[test]
    fn complete_design_yields_no_questions() {
        assert!(validate(&design(MINIMAL)).is_empty());
    }

    /// Inject a `cors` block into MINIMAL (which is otherwise question-free).
    fn with_cors(cors_json: &str) -> Design {
        design(&MINIMAL.replace(
            "\"contract_version\": 0,",
            &format!("\"contract_version\": 0, \"cors\": {cors_json},"),
        ))
    }

    /// A well-formed cors block yields NO questions — a valid cross-origin SPA
    /// policy (issue #21) must validate clean.
    #[test]
    fn well_formed_cors_block_is_question_free() {
        let d = with_cors(
            r#"{ "origins": ["https://app.example", "http://localhost:3000"],
                 "methods": ["GET", "POST"], "headers": ["content-type"],
                 "allow_credentials": true }"#,
        );
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
        // `*` alone (no credentials) is also valid.
        let any = with_cors(r#"{ "origins": ["*"] }"#);
        assert!(validate(&any).is_empty(), "{:?}", validate(&any));
    }

    /// The CORS footguns become pointed questions, not runtime boot failures:
    /// empty origins, `*` mixed with an allowlist, `*` + credentials (Fetch-spec
    /// forbidden — core's App::build rejects it), and a non-bare origin.
    #[test]
    fn cors_misconfig_yields_pointed_questions() {
        // Empty origins.
        let d = with_cors(r#"{ "origins": [] }"#);
        assert!(
            validate(&d).iter().any(|q| q.id == "/cors/origins"),
            "empty origins must be a question: {:?}",
            validate(&d)
        );
        // `*` mixed with an explicit origin.
        let d = with_cors(r#"{ "origins": ["*", "https://app.example"] }"#);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/cors/origins" && q.question.contains("mixes")),
            "mixing `*` with explicit origins must be a question: {:?}",
            validate(&d)
        );
        // `*` + credentials — the Fetch-spec violation core rejects at build time.
        let d = with_cors(r#"{ "origins": ["*"], "allow_credentials": true }"#);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/cors/allow_credentials"),
            "`*` + credentials must be caught at design time: {:?}",
            validate(&d)
        );
        // A non-bare origin (has a path / trailing slash / no scheme).
        for bad in [
            "https://app.example/",
            "app.example",
            "https://app.example/app",
        ] {
            let d = with_cors(&format!(r#"{{ "origins": ["{bad}"] }}"#));
            assert!(
                validate(&d).iter().any(|q| q.id == "/cors/origins/0"),
                "malformed origin `{bad}` must be a question: {:?}",
                validate(&d)
            );
        }
    }

    #[test]
    fn bad_names_yield_pointed_questions_with_json_pointer_ids() {
        let d = design(&MINIMAL.replace("\"name\": \"demo-api\"", "\"name\": \"Demo API\""));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/name" && q.question.contains("kebab-case")),
            "{qs:?}"
        );
    }

    #[test]
    fn duplicate_operation_ids_and_routes_are_caught() {
        let d = design(&MINIMAL.replace(
            "\"operation_id\": \"create_todo\"",
            "\"operation_id\": \"list_todos\"",
        ));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id.starts_with("/modules/0/endpoints") && q.question.contains("unique"))
        );

        let d2 = design(&MINIMAL.replace(
            "{ \"operation_id\": \"create_todo\", \"method\": \"POST\", \"path\": \"/\",",
            "{ \"operation_id\": \"create_todo\", \"method\": \"GET\", \"path\": \"/\",",
        ));
        let qs2 = validate(&d2);
        assert!(
            qs2.iter()
                .any(|q| q.question.contains("GET /") && q.question.contains("already")),
            "{qs2:?}"
        );
    }

    #[test]
    fn roles_must_be_declared_and_entities_must_exist() {
        let d = design(&MINIMAL.replace(
            "\"required_roles\": [\"admin\"]",
            "\"required_roles\": [\"superuser\"]",
        ));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.question.contains("superuser") && q.question.contains("auth.roles"))
        );

        let d2 = design(&MINIMAL.replace(
            "\"request_body\": { \"entity\": \"Todo\" }",
            "\"request_body\": { \"entity\": \"Ghost\" }",
        ));
        let qs2 = validate(&d2);
        assert!(qs2.iter().any(|q| q.question.contains("Ghost")));
    }

    #[test]
    fn status_ranges_and_path_shape_are_enforced() {
        // 3xx is a valid success class (redirect endpoints, e.g. OAuth connect).
        let ok3xx = design(&MINIMAL.replace("\"status\": 204", "\"status\": 302"));
        assert!(
            !validate(&ok3xx)
                .iter()
                .any(|q| q.question.contains("success")),
            "302 is a valid (redirect) success status"
        );
        // A 5xx success status is not a success class and must be rejected.
        let d = design(&MINIMAL.replace("\"status\": 204", "\"status\": 500"));
        assert!(validate(&d).iter().any(|q| q.question.contains("2xx/3xx")));
        let d2 = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"{id}\""));
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.question.contains("start with '/'"))
        );
    }

    #[test]
    fn paths_allow_up_to_three_params_and_validate_mount_prefix() {
        // Two params: now legal (multi-param Path landed in core).
        let d = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{id}/tags/{tag}\""));
        assert!(
            !validate(&d)
                .iter()
                .any(|q| q.question.contains("path parameter")),
            "two params must be accepted now"
        );
        // Four params: rejected.
        let d4 = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{a}/{b}/{c}/{d}\""));
        assert!(
            validate(&d4)
                .iter()
                .any(|q| q.question.contains("three path parameters"))
        );

        let d2 = design(&MINIMAL.replace(
            "\"name\": \"comments\",",
            "\"name\": \"comments\", \"mount\": \"comments\",",
        ));
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id.contains("/mount") && q.question.contains("start with '/'"))
        );

        // A path parameter in a mount prefix is now fully supported: a handler's
        // single Path<T> binds the leaf-most param, tuples address all root→leaf.
        // The validator must NOT discourage it (only the syntax rules apply).
        let d3 = design(&MINIMAL.replace(
            "\"name\": \"comments\",",
            "\"name\": \"comments\", \"mount\": \"/{comment_id}\",",
        ));
        assert!(
            !validate(&d3).iter().any(|q| q.id.contains("/mount")),
            "a param-carrying mount prefix must raise no mount question now: {:?}",
            validate(&d3)
        );
    }

    #[test]
    fn nested_subroute_violations_carry_full_json_pointers() {
        let d = design(&MINIMAL.replace(
            "\"operation_id\": \"list_comments\"",
            "\"operation_id\": \"List-Comments\"",
        ));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/subroutes/0/endpoints/0/operation_id"),
            "{qs:?}"
        );
    }

    #[test]
    fn unbalanced_path_braces_yield_a_question() {
        let d = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{id\""));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("unbalanced braces")),
            "unbalanced braces must be flagged"
        );
    }

    #[test]
    fn json_fields_are_rejected_in_db_mode() {
        // MINIMAL already declares `["db"]`; flip a field to json.
        let d = design(&MINIMAL.replace("\"type\": \"boolean\"", "\"type\": \"json\""));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("json") && q.question.contains("db mode")),
            "db mode can't store json fields yet"
        );
    }

    #[test]
    fn raw_escapable_keyword_field_names_are_accepted() {
        // WHY: `type`/`match`/`ref` are common field names in frozen external
        // wire contracts. Codegen raw-escapes them (`r#type`) with a serde
        // rename, so forcing a rename would push a permanent wire↔storage
        // mapping into every handler. Validation must NOT flag them.
        for kw in ["type", "match", "ref"] {
            let d = design(&MINIMAL.replace("\"name\": \"title\"", &format!("\"name\": \"{kw}\"")));
            assert!(
                !validate(&d)
                    .iter()
                    .any(|q| q.id.contains("/fields/") && q.question.contains("keyword")),
                "keyword field `{kw}` is raw-escapable and must be accepted"
            );
        }
    }

    #[test]
    fn unescapable_keyword_field_names_are_still_rejected() {
        // `self`/`crate`/`super` are keywords no raw identifier can escape
        // (`r#self` is invalid Rust), so a field named one still can't compile.
        for kw in ["self", "crate", "super"] {
            let d = design(&MINIMAL.replace("\"name\": \"title\"", &format!("\"name\": \"{kw}\"")));
            assert!(
                validate(&d)
                    .iter()
                    .any(|q| q.id.contains("/fields/") && q.question.contains("keyword")),
                "unescapable keyword field `{kw}` must be flagged"
            );
        }
    }

    #[test]
    fn required_roles_need_a_role_in_auth_roles_and_auth_model() {
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["auth"] = serde_json::json!({ "model": "none" });
        v["modules"][0]["endpoints"][2]["required_roles"] = serde_json::json!(["admin"]);
        let d: Design = serde_json::from_value(v).unwrap();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("auth.model") || q.question.contains("auth.roles"))
        );
    }

    #[test]
    fn mount_rejects_trailing_slash_and_double_slash() {
        let d = design(&MINIMAL.replace(
            "\"name\": \"comments\",",
            "\"name\": \"comments\", \"mount\": \"/x/\",",
        ));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id.contains("/mount") && q.question.contains("trailing slash")),
            "trailing-slash mount must be flagged"
        );
    }

    #[test]
    fn belongs_to_must_target_a_declared_entity() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[1].entities[0].belongs_to[0].entity = "Ghost".into();
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/1/entities/0/belongs_to/0"
                    && q.question.contains("Ghost")),
            "{qs:?}"
        );
    }

    #[test]
    fn tenancy_entity_must_exist() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.tenancy.as_mut().unwrap().entity = "Nope".into();
        assert!(validate(&d).iter().any(|q| q.id == "/tenancy/entity"));
    }

    #[test]
    fn tenancy_requires_active_auth() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.auth = None;
        assert!(validate(&d).iter().any(|q| q.id == "/tenancy"));
    }

    /// #27: a design whose `tenancy.entity` IS the auth identity entity is
    /// otherwise complete (no completeness question), yet cannot scaffold — the
    /// generated `{tenant}_members` table would derive the same fixed `user_id`
    /// column twice. `design_conflict` rejects it up front with JC0540, so the
    /// CLI fails loud before writing a byte instead of dying mid-migration with a
    /// raw SQLite `duplicate column name: user_id`.
    #[test]
    fn tenancy_entity_as_auth_identity_is_a_design_conflict() {
        let fixture = include_str!("../../tests/fixtures/tenant-is-identity.design.json");
        let d: Design = serde_json::from_str(fixture).unwrap();
        // Completeness is clean — the conflict is what the new rule catches.
        assert!(
            validate(&d).is_empty(),
            "fixture must be otherwise complete: {:?}",
            validate(&d)
        );
        let conflict = design_conflict(&d).expect("tenant==identity must be a conflict");
        assert_eq!(conflict.code, "JC0540");
        // Names both fixes: per-user → belongs_to; orgs/teams → a separate entity.
        assert!(
            conflict.message.contains("belongs_to") && conflict.message.contains("tenant entity"),
            "{}",
            conflict.message
        );
        assert!(!conflict.hint.is_empty());
    }

    /// The comparison is derived from the fixed membership `user_id` column, so a
    /// tenancy over a SEPARATE tenant entity (the reference shape) is never
    /// flagged — only the entity whose fk column collides with the identity is.
    #[test]
    fn separate_tenant_entity_is_not_a_conflict() {
        let d: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(
            design_conflict(&d).is_none(),
            "Workspace tenancy must not be flagged"
        );
        // No tenancy at all: nothing to conflict.
        let plain: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(design_conflict(&plain).is_none());
    }

    /// Issue #44 (positive): an entity literally named `{X}Request` alongside an
    /// entity `X` whose guarded body omits the identity fk generates two `XRequest`
    /// definitions (Rust struct + OpenAPI component). `design_conflict` rejects it up
    /// front with JC0541 and names the rename fix — cheap insurance for agent-authored
    /// designs, since genroute would otherwise die with a duplicate-struct compile
    /// error mid-scaffold.
    #[test]
    fn entity_shadowing_a_generated_request_dto_is_a_conflict() {
        // Collection (db + auth + identity fk) mints a `CollectionRequest` DTO; an
        // entity literally named `CollectionRequest` collides with it.
        let d: Design = serde_json::from_str(
            r#"{
            "name": "clash", "contract_version": 1,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["db", "auth"],
            "modules": [{
                "name": "collections",
                "entities": [
                    { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                    { "name": "Collection",
                      "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                      "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "CollectionRequest", "fields": [{ "name": "note", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_collection", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Collection" },
                      "success": { "status": 201, "entity": "Collection" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let conflict = design_conflict(&d).expect("XRequest shadowing a DTO must be a conflict");
        assert_eq!(conflict.code, "JC0541");
        assert!(
            conflict.message.contains("CollectionRequest")
                && conflict.message.contains("Collection")
                && conflict.message.to_lowercase().contains("rename"),
            "message names the collision and the rename fix: {}",
            conflict.message
        );
        assert!(conflict.hint.contains("rename"), "{}", conflict.hint);
    }

    /// Issue #44 (negative): the lint fires ONLY on a REAL collision, not on any
    /// `*Request` suffix. (a) A `{X}Request` entity whose `X` sibling generates NO DTO
    /// (memory mode, no omission) is fine — nothing is shadowed. (b) A `*Request`
    /// entity with no matching base entity is fine. (c) A base `X` that mints a DTO
    /// but has no `XRequest` sibling is fine.
    #[test]
    fn request_suffix_without_a_real_collision_is_not_flagged() {
        // (a) Same names, but MEMORY mode → Collection mints no DTO → no collision.
        let mem: Design = serde_json::from_str(
            r#"{
            "name": "ok-mem", "contract_version": 1,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["auth"],
            "modules": [{
                "name": "collections",
                "entities": [
                    { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                    { "name": "Collection",
                      "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                      "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "CollectionRequest", "fields": [{ "name": "note", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_collection", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Collection" },
                      "success": { "status": 201, "entity": "Collection" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&mem).is_none(),
            "memory mode mints no DTO — no collision"
        );
        // (b) A lone `*Request` entity with no matching base entity is fine.
        let orphan: Design = serde_json::from_str(
            r#"{
            "name": "ok-orphan", "contract_version": 1, "dependencies": ["db"],
            "modules": [{
                "name": "audit",
                "entities": [
                    { "name": "AuditRequest", "fields": [{ "name": "note", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_audit", "method": "POST", "path": "/",
                      "request_body": { "entity": "AuditRequest" },
                      "success": { "status": 201, "entity": "AuditRequest" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&orphan).is_none(),
            "a `*Request` name shadowing nothing is fine"
        );
        // (c) A base that mints a DTO but has no `XRequest` sibling is fine.
        let d: Design = serde_json::from_str(SERVER_FK_LITE).unwrap();
        assert!(
            design_conflict(&d).is_none(),
            "a generated DTO with no shadowing entity is fine"
        );
    }

    /// A minimal db+auth+identity-fk design (Collection mints CollectionRequest) with
    /// NO shadowing entity — the JC0541 negative control.
    const SERVER_FK_LITE: &str = r#"{
        "name": "lite", "contract_version": 1,
        "auth": { "model": "session", "roles": ["admin"] },
        "dependencies": ["db", "auth"],
        "modules": [{
            "name": "collections",
            "entities": [
                { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                { "name": "Collection",
                  "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                  "fields": [{ "name": "title", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "create_collection", "method": "POST", "path": "/",
                  "auth_required": true,
                  "request_body": { "entity": "Collection" },
                  "success": { "status": 201, "entity": "Collection" } }
            ]
        }]
    }"#;

    #[test]
    fn jobs_validate_name_uniqueness_and_cron_shape() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.jobs[0].schedule = Some("not cron".into());
        assert!(validate(&d).iter().any(|q| q.id == "/jobs/0/schedule"));
        let mut d2: Design = serde_json::from_str(V1_FULL).unwrap();
        d2.jobs.push(d2.jobs[0].clone());
        assert!(validate(&d2).iter().any(|q| q.id == "/jobs/1/name"));
    }

    #[test]
    fn jobs_validate_queue_is_snake_case() {
        // The queue is interpolated RAW into generated Rust string literals
        // (`.queue("{q}", ...)`); a `"` in the queue would break the generated
        // crate at build time, far from the design. Validation must reject a
        // non-identifier queue up front, mirroring the job-name check.
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.jobs[0].queue = Some("not a queue\"".into());
        assert!(
            validate(&d).iter().any(|q| q.id == "/jobs/0/queue"),
            "a non-snake_case job queue must be a validation error"
        );
        // A valid snake_case queue passes.
        let mut ok: Design = serde_json::from_str(V1_FULL).unwrap();
        ok.jobs[0].queue = Some("billing".into());
        assert!(!validate(&ok).iter().any(|q| q.id == "/jobs/0/queue"));
    }

    #[test]
    fn jobs_require_a_database_dependency() {
        // Jobs run over a Postgres store; the generated `jobs(db)` wiring +
        // JOBS_MIGRATIONS need `jerrycan::db::Db`. A jobs-without-db design can't
        // compile, so validation rejects it before generation.
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.dependencies.retain(|dep| dep != "db");
        assert!(d.wants_jobs() && !d.wants_db());
        assert!(
            validate(&d).iter().any(|q| q.id == "/jobs"),
            "jobs without a db dependency must be a validation error"
        );
        // With db present (the unmodified fixture), no jobs-require-db error.
        let ok: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(!validate(&ok).iter().any(|q| q.id == "/jobs"));
    }

    #[test]
    fn enum_values_only_on_string_fields_and_nonempty() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[0].entities[0].fields[0].values = Some(vec!["x".into()]); // id: integer
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/values")
        );
        let mut d2: Design = serde_json::from_str(V1_FULL).unwrap();
        d2.modules[0].entities[0].fields[1].values = Some(vec![]); // empty
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/1/values")
        );
    }

    #[test]
    fn field_default_must_type_check_against_field_type_and_enum() {
        // Issue #53a: a server-owned `default` is written verbatim into a NOT-NULL
        // column, so a mistyped or out-of-enum literal is a design-time error, not
        // a run-time surprise. Valid defaults raise no question.
        let base = |field: &str| {
            design(&format!(
                r#"{{ "name": "news", "contract_version": 0, "dependencies": ["db"],
                    "modules": [{{ "name": "subs",
                        "entities": [{{ "name": "Subscriber", "fields": [{field}] }}],
                        "endpoints": [{{ "operation_id": "create_subscriber", "method": "POST", "path": "/",
                            "request_body": {{ "entity": "Subscriber" }},
                            "success": {{ "status": 201, "entity": "Subscriber" }} }}] }}] }}"#
            ))
        };
        // A boolean default `false` and an enum default `"active"` are valid.
        let ok = base(
            r#"{ "name": "confirmed", "type": "boolean", "default": false },
               { "name": "status", "type": "string", "values": ["active", "expired"], "default": "active" }"#,
        );
        assert!(
            !validate(&ok).iter().any(|q| q.id.ends_with("/default")),
            "valid defaults raise no question: {:?}",
            validate(&ok)
        );
        // A string literal on a boolean field is rejected.
        let bad_type = base(r#"{ "name": "confirmed", "type": "boolean", "default": "false" }"#);
        assert!(
            validate(&bad_type)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/default"),
            "mistyped default must be a question: {:?}",
            validate(&bad_type)
        );
        // A default outside the enum `values` is rejected.
        let bad_enum = base(
            r#"{ "name": "status", "type": "string", "values": ["active", "expired"], "default": "draft" }"#,
        );
        assert!(
            validate(&bad_enum)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/default"),
            "out-of-enum default must be a question: {:?}",
            validate(&bad_enum)
        );
        // A default without a `db` dependency is inert (no request DTO) → rejected.
        let no_db: Design = serde_json::from_str(
            r#"{ "name": "news", "contract_version": 0, "dependencies": [],
                "modules": [{ "name": "subs",
                    "entities": [{ "name": "Subscriber", "fields": [
                        { "name": "email", "type": "string" },
                        { "name": "confirmed", "type": "boolean", "default": false } ] }],
                    "endpoints": [{ "operation_id": "list_subs", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        assert!(
            validate(&no_db)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/1/default"
                    && q.question.contains("no `db` dependency")),
            "a default without db must be a question: {:?}",
            validate(&no_db)
        );
    }

    #[test]
    fn explicit_fk_named_field_conflicts_with_belongs_to() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[1].entities[0].fields.push(Field {
            name: "workspace_id".into(),
            field_type: FieldType::Integer,
            required: true,
            unique: false,
            index: false,
            values: None,
            default: None,
        });
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id.ends_with("/fields/3") && q.question.contains("derived")),
            "{:?}",
            validate(&d)
        );
    }

    #[test]
    fn public_endpoint_cannot_also_be_auth_required() {
        // A public endpoint that also demands auth contradicts itself: `public`
        // is the JL0004 carve-out for genuinely unauthenticated routes (login/
        // register), so combining it with a guard is a design error. WHY (Rule 9):
        // the flag exists to mark a route as needing NO credential — a guarded
        // public route would silently re-trip the very lint it claims exemption from.
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["modules"][0]["endpoints"][1]["public"] = serde_json::json!(true);
        v["modules"][0]["endpoints"][1]["auth_required"] = serde_json::json!(true);
        let d: Design = serde_json::from_value(v).unwrap();
        let qs = validate(&d);
        assert!(
            qs.iter().any(|q| q.id == "/modules/0/endpoints/1"
                && q.question.contains("public")
                && q.question.contains("auth_required")),
            "{qs:?}"
        );
    }

    #[test]
    fn public_endpoint_cannot_require_roles() {
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["modules"][0]["endpoints"][2]["public"] = serde_json::json!(true);
        // endpoints[2] (delete_todo) already declares required_roles: ["admin"].
        let d: Design = serde_json::from_value(v).unwrap();
        let qs = validate(&d);
        assert!(
            qs.iter().any(|q| q.id == "/modules/0/endpoints/2"
                && q.question.contains("public")
                && q.question.contains("required_roles")),
            "{qs:?}"
        );
    }

    #[test]
    fn reference_shaped_v1_design_is_question_free() {
        let d: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn public_endpoint_on_tenant_owned_entity_is_rejected() {
        // A public endpoint skips every guard — including the Tenant guard that
        // scopes a tenant-owned entity to its owner. WHY (Rule 9): marking such an
        // endpoint public would expose one tenant's rows to anyone, silently
        // defeating tenancy; the design must not be able to claim that exemption
        // on an entity that belongs_to the tenancy root.
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        // V1_FULL module[1] is `leads`; its `Lead` entity belongs_to Workspace
        // (the tenancy entity), and list_leads resolves to Lead via success.entity.
        d.modules[1].endpoints[0].public = true;
        let qs = validate(&d);
        assert!(
            qs.iter().any(|q| q.id == "/modules/1/endpoints/0"
                && q.question.contains("public")
                && q.question.contains("tenant-owned")),
            "{qs:?}"
        );
    }

    #[test]
    fn public_endpoints_on_non_tenant_owned_entities_do_not_false_positive() {
        // The reference-slice north-star design has public register/login in the
        // `users` module; User is NOT tenant-owned, so the resolution (request_body
        // entity for register, first-entity fallback for login) must not flag them.
        let reference = include_str!("../../../../conformance/designs/reference-slice.design.json");
        let d: Design = serde_json::from_str(reference).unwrap();
        let qs = validate(&d);
        assert!(
            qs.is_empty(),
            "reference-slice must validate question-free; public users endpoints must not false-positive: {qs:?}"
        );
    }

    // ---- #65 (JC0542): sibling routes with conflicting path-param names -------

    const CONFORMANCE_REFERENCE: &str =
        include_str!("../../../../conformance/designs/reference-slice.design.json");
    const CONFORMANCE_TODO: &str =
        include_str!("../../../../conformance/designs/todo-api.design.json");

    /// The HelpDesk repro (hit by 4/5 eval builds): `/{id}` and `/{ticket_id}/comments`
    /// in one module share segment position 2 but name its param differently. The
    /// runtime router (one global trie) aborts `App::build` with JC0500 after a clean
    /// scaffold; `design_conflict` must reject it up front with JC0542 naming BOTH
    /// routes, BOTH names, and BOTH remedies (unify / restructure).
    #[test]
    fn sibling_routes_with_different_param_names_are_a_conflict() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "helpdesk", "contract_version": 1, "dependencies": ["db"],
            "modules": [{
                "name": "tickets",
                "entities": [
                    { "name": "Ticket", "fields": [{ "name": "subject", "type": "string" }] },
                    { "name": "Comment", "fields": [{ "name": "body", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "show_ticket", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ticket" } },
                    { "operation_id": "list_comments", "method": "GET", "path": "/{ticket_id}/comments",
                      "success": { "status": 200, "entity": "Comment", "list": true } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let c = design_conflict(&d).expect("mismatched sibling param names must be a conflict");
        assert_eq!(c.code, "JC0542");
        assert!(
            c.message.contains("/tickets/{id}")
                && c.message.contains("/tickets/{ticket_id}/comments"),
            "names both conflicting routes: {}",
            c.message
        );
        assert!(
            c.message.contains("{id}") && c.message.contains("{ticket_id}"),
            "names both param names: {}",
            c.message
        );
        assert!(
            c.message.to_lowercase().contains("unify")
                && c.message.to_lowercase().contains("restructure"),
            "names both remedies: {}",
            c.message
        );
        assert!(!c.hint.is_empty());
    }

    /// The router accepts several sibling shapes the validator must NOT reject:
    /// (a) the SAME param name at a shared position (`/{id}` + `/{id}/comments`),
    /// (b) a literal vs a param at a position (`/{id}` + `/archive` — distinct trie
    /// children), and (c) a param-carrying subroute mount whose name is consistent
    /// with the parent. Plus BOTH shipped conformance designs.
    #[test]
    fn consistent_and_divergent_sibling_paths_are_not_conflicts() {
        // (a) same param name at the shared position.
        let same: Design = serde_json::from_str(
            r#"{
            "name": "ok-same", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "tickets",
                "entities": [{ "name": "Ticket", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "show_ticket", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ticket" } },
                    { "operation_id": "list_comments", "method": "GET", "path": "/{id}/comments",
                      "success": { "status": 200 } }
                ] }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&same).is_none(),
            "identical param names at the shared position are fine"
        );
        // (b) a literal segment vs a param at the same position diverges.
        let literal: Design = serde_json::from_str(
            r#"{
            "name": "ok-literal", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "tickets",
                "entities": [{ "name": "Ticket", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "show_ticket", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ticket" } },
                    { "operation_id": "list_archived", "method": "GET", "path": "/archive",
                      "success": { "status": 200 } }
                ] }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&literal).is_none(),
            "a literal and a param at the same position are distinct trie children"
        );
        // (c) a param-carrying subroute mount whose param agrees with the parent.
        let mounted: Design = serde_json::from_str(
            r#"{
            "name": "ok-mount", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "workspaces", "mount": "/ws",
                "entities": [{ "name": "Ws", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "show_ws", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ws" } }
                ],
                "subroutes": [{ "name": "leads", "mount": "/{id}/leads",
                    "endpoints": [
                        { "operation_id": "show_lead", "method": "GET", "path": "/{lead_id}",
                          "success": { "status": 200 } }
                    ] }]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&mounted).is_none(),
            "a param-mount child consistent with the parent must not be flagged: {:?}",
            design_conflict(&mounted).map(|c| c.message)
        );
        // Both shipped conformance designs (params all named `id`) stay clean.
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                design_conflict(&d).is_none(),
                "conformance design must not trip JC0542: {:?}",
                design_conflict(&d).map(|c| c.message)
            );
        }
    }

    // ---- #54 (JC0543): enum value content ------------------------------------

    /// An enum value with a space (or quote/backslash) breaks the UNESCAPED
    /// interpolation into generated Rust; validation rejects it at design time with
    /// JC0543 guidance, naming the offending value. Identifier-shaped values
    /// (letters, digits, `_`, `-`) pass.
    #[test]
    fn enum_values_must_be_identifier_shaped() {
        // V1_FULL module[0]=workspaces, entity[0]=Workspace, field[1]=plan (string enum).
        let mut bad: Design = serde_json::from_str(V1_FULL).unwrap();
        bad.modules[0].entities[0].fields[1].values = Some(vec!["in progress".into()]);
        assert!(
            validate(&bad)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/1/values"
                    && q.question.contains("JC0543")
                    && q.question.contains("in progress")),
            "a space-bearing enum value must be rejected: {:?}",
            validate(&bad)
        );
        // A quote is rejected too (the direct interpolation footgun).
        let mut quoted: Design = serde_json::from_str(V1_FULL).unwrap();
        quoted.modules[0].entities[0].fields[1].values = Some(vec!["a\"b".into()]);
        assert!(
            validate(&quoted)
                .iter()
                .any(|q| q.id.ends_with("/values") && q.question.contains("JC0543"))
        );
        // `-` and `_` and mixed case are legitimate identifier shapes.
        let mut ok: Design = serde_json::from_str(V1_FULL).unwrap();
        ok.modules[0].entities[0].fields[1].values =
            Some(vec!["in-progress".into(), "on_hold".into(), "Done2".into()]);
        assert!(
            !validate(&ok).iter().any(|q| q.id.ends_with("/values")),
            "identifier-shaped values must pass: {:?}",
            validate(&ok)
        );
        // Both shipped conformance designs' enum values stay clean.
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                !validate(&d).iter().any(|q| q.question.contains("JC0543")),
                "conformance enum values must not trip JC0543"
            );
        }
    }

    // ---- #60 (JC0544): dual-create path-fk omission --------------------------

    /// The dual-create shape from issue #60: `Checkin belongs_to Habit`, with a
    /// nested `POST /{habit_id}/checkins` AND a standalone `POST /checkins`. The
    /// per-entity `CheckinRequest` drops `habit_id` for both, so the standalone
    /// route can set the NOT-NULL fk from neither the body nor the path — it is
    /// un-implementable. Validation flags ONLY the standalone route with JC0544,
    /// naming the route and BOTH fixes; the nested route (which carries the param)
    /// is left alone.
    #[test]
    fn dual_create_standalone_route_missing_path_fk_is_flagged() {
        let d: Design = serde_json::from_str(DUAL_CREATE).unwrap();
        let qs = validate(&d);
        let flagged: Vec<&Question> = qs
            .iter()
            .filter(|q| q.question.contains("JC0544"))
            .collect();
        assert_eq!(
            flagged.len(),
            1,
            "only the standalone POST /checkins is un-implementable: {qs:?}"
        );
        let f = flagged[0];
        assert!(
            f.question.contains("create_checkin_flat") && f.question.contains("habit_id"),
            "names the un-implementable route and the fk: {}",
            f.question
        );
        assert!(
            f.question.to_lowercase().contains("split") && f.question.contains("{habit_id}"),
            "names both fixes (add the path param / split the entity): {}",
            f.question
        );
    }

    /// A nested-ONLY create (`POST /{habit_id}/checkins`, no standalone) carries the
    /// fk in its path, so it is implementable and must NOT be flagged — and both
    /// shipped conformance designs (no dual-create shape) stay JC0544-free.
    #[test]
    fn nested_only_create_and_conformance_shapes_do_not_trip_dual_create() {
        let nested: Design = serde_json::from_str(NESTED_ONLY).unwrap();
        assert!(
            !validate(&nested)
                .iter()
                .any(|q| q.question.contains("JC0544")),
            "a nested-only create carries its fk in the path: {:?}",
            validate(&nested)
        );
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                !validate(&d).iter().any(|q| q.question.contains("JC0544")),
                "conformance design must not trip JC0544"
            );
        }
    }

    /// The #60 repro: one entity created both nested and standalone.
    const DUAL_CREATE: &str = r#"{
        "name": "habits", "contract_version": 1, "dependencies": ["db"],
        "modules": [{
            "name": "habits",
            "entities": [
                { "name": "Habit", "fields": [
                    { "name": "id", "type": "integer" }, { "name": "title", "type": "string" } ] },
                { "name": "Checkin",
                  "belongs_to": [{ "entity": "Habit", "on_delete": "cascade" }],
                  "fields": [
                    { "name": "id", "type": "integer" }, { "name": "note", "type": "string" } ] }
            ],
            "endpoints": [
                { "operation_id": "create_checkin_nested", "method": "POST", "path": "/{habit_id}/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } },
                { "operation_id": "create_checkin_flat", "method": "POST", "path": "/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } }
            ]
        }]
    }"#;

    /// The same entity created ONLY under its parent's path — implementable.
    const NESTED_ONLY: &str = r#"{
        "name": "habits", "contract_version": 1, "dependencies": ["db"],
        "modules": [{
            "name": "habits",
            "entities": [
                { "name": "Habit", "fields": [
                    { "name": "id", "type": "integer" }, { "name": "title", "type": "string" } ] },
                { "name": "Checkin",
                  "belongs_to": [{ "entity": "Habit", "on_delete": "cascade" }],
                  "fields": [
                    { "name": "id", "type": "integer" }, { "name": "note", "type": "string" } ] }
            ],
            "endpoints": [
                { "operation_id": "create_checkin_nested", "method": "POST", "path": "/{habit_id}/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } }
            ]
        }]
    }"#;
}
