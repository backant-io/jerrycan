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

/// Reserved words that cannot appear as field/entity identifiers: generated
/// model.rs uses them verbatim as struct/field names and would not compile.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while",
];

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
                    }
                }
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_relations_and_enums(sub, &format!("{ptr}/subroutes/{i}"), entity_names, qs);
        }
    }
    for (i, m) in d.modules.iter().enumerate() {
        check_relations_and_enums(m, &format!("/modules/{i}"), &entity_names, &mut qs);
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
            m: &ModuleDesign,
            ptr: &str,
            tenant: &str,
            qs: &mut Vec<Question>,
        ) {
            for (i, ep) in m.endpoints.iter().enumerate() {
                if ep.public
                    && endpoint_repo_entity(m, ep).is_some_and(|name| {
                        m.entities
                            .iter()
                            .find(|e| e.name == name)
                            .is_some_and(|e| e.belongs_to.iter().any(|b| b.entity == tenant))
                    })
                {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        "endpoint is public but its entity is tenant-owned — public endpoints bypass the Tenant guard; remove public or move the endpoint off the tenant-owned entity".to_string(),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_public_on_tenant_owned(sub, &format!("{ptr}/subroutes/{i}"), tenant, qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_public_on_tenant_owned(m, &format!("/modules/{i}"), &tenancy.entity, &mut qs);
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
            if RUST_KEYWORDS.contains(&ident.as_str()) {
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
            if module_mounts.contains(&format!("/{}", b.name)) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` mounts at /{} which collides with a module mount — rename the bucket or remount the module.", b.name, b.name),
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
                        format!("Realtime {kind} topic `{}` is not snake_case (^[a-z][a-z0-9_]*$).", t.name),
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
        if RUST_KEYWORDS.contains(&e.name.as_str()) {
            qs.push(q(
                format!("{ptr}/entities/{i}/name"),
                format!(
                    "Entity `{}` is a Rust keyword — generated model code cannot use it; rename it (e.g. a domain-specific name).",
                    e.name
                ),
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
            if RUST_KEYWORDS.contains(&f.name.as_str()) {
                qs.push(q(
                    format!("{ptr}/entities/{i}/fields/{j}/name"),
                    format!(
                        "Field `{name}` is a Rust keyword — generated model code cannot use it; rename (e.g. `{name}_field` or a domain-specific name).",
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
        // WHY: buckets mount at /<name> beside the modules — a collision would
        // shadow routes silently at serve time.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "orgs".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("mount"))
        );
    }

    #[test]
    fn complete_design_yields_no_questions() {
        assert!(validate(&design(MINIMAL)).is_empty());
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
    fn rust_keyword_field_names_are_rejected() {
        // `type` is a Rust keyword — generated `pub type: ...` would not compile.
        let d = design(&MINIMAL.replace("\"name\": \"title\"", "\"name\": \"type\""));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("Rust keyword") && q.question.contains("`type`")),
            "keyword field name must be flagged"
        );
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
    fn explicit_fk_named_field_conflicts_with_belongs_to() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[1].entities[0].fields.push(Field {
            name: "workspace_id".into(),
            field_type: FieldType::Integer,
            required: true,
            unique: false,
            index: false,
            values: None,
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
}
