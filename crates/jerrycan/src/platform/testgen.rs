//! design module → tests/acceptance.rs (TOOL-owned). One success test per
//! endpoint, one test per generatable error case (404 on parameterized paths),
//! an AGENT TODO comment for the rest. Stubs fail everything (expected_failing
//! = test_count) — green = the design contract is implemented.

use super::design::*;

/// The JSON request-body fixture literal for a field. Enum fields (those with a
/// declared `values` set) use their FIRST declared value, so the generated
/// happy-path body satisfies the migration's `CHECK (... IN (...))` constraint
/// instead of tripping it with `"test-value"` (an opaque `JC0510` at run time).
/// A range/length-constrained field (#80) derives an IN-RANGE value the same
/// way, so the happy-path body clears the deserialize validator AND the CHECK.
/// Mirrors `seed_sql_value` on the SQL seed side — the two must agree.
fn fixture_value(f: &Field) -> String {
    if let Some(first) = f.values.as_ref().and_then(|v| v.first()) {
        return format!("\"{first}\"");
    }
    // #80: both branches gate on a constraint being PRESENT — an unconstrained
    // field keeps the exact literals below (byte-identity for every existing
    // design).
    if has_int_range(f) {
        return int_literal(clamp_int(1, f));
    }
    if has_len_range(f) {
        return format!("\"{}\"", constrained_fixture_string(f));
    }
    match f.field_type {
        FieldType::String => "\"test-value\"",
        FieldType::Integer => "1",
        FieldType::Float => "1.0",
        FieldType::Boolean => "false",
        FieldType::Datetime => "\"2026-01-01T00:00:00Z\"",
        // A FIXED valid v4 (issue #48a): the design's declared format types must
        // yield format-VALID fixtures so the endpoint's own happy-path probe is
        // greenable against a handler that validates the format. The nil uuid
        // (`0000…`) is a valid string but NOT a valid v4 — a v4 validator would
        // reject it, making the 2xx probe un-greenable. datetime above is already
        // valid RFC3339. (email/url are NOT design-contract format types — they
        // ride on `string` via a hand-written `Valid` impl the generator can't
        // see; that case is `probe:"skip"`, see docs/ai/00-designing.md.)
        FieldType::Uuid => "\"f47ac10b-58cc-4372-a567-0e02b2c3d479\"",
        FieldType::Json => "{}",
    }
    .to_string()
}

/// True when the field declares an integer range constraint (#80) — the gate
/// every constraint-aware fixture/seed branch keys on, so an unconstrained
/// field's output stays byte-identical.
fn has_int_range(f: &Field) -> bool {
    matches!(f.field_type, FieldType::Integer) && (f.min.is_some() || f.max.is_some())
}

/// True when the field declares a string length constraint (#80). A `values`
/// field never carries one (JC0552 refuses the combination) and takes the enum
/// branch first anyway.
fn has_len_range(f: &Field) -> bool {
    matches!(f.field_type, FieldType::String) && (f.min_len.is_some() || f.max_len.is_some())
}

/// Render a constrained-integer value for embedding inside a
/// `serde_json::json!` probe body (#80): `json!` types a bare numeric literal
/// as `i32`, so a value outside i32 range (a bound like `max: 4102444800`)
/// would be a HARD compile error in the generated suite under rustc's
/// deny-by-default `overflowing_literals`. Suffix `i64` exactly when the
/// value is outside i32 range; everything in-range stays an unsuffixed
/// literal (byte-identity for every existing design). The `id` fixture never
/// routes through here — JC0552 refuses constraints on the pk — so a suffixed
/// value can never leak into a URL path.
fn int_literal(v: i64) -> String {
    if v < i64::from(i32::MIN) || v > i64::from(i32::MAX) {
        format!("{v}i64")
    } else {
        v.to_string()
    }
}

/// Clamp `v` into the field's declared `[min, max]` (#80): below-min snaps to
/// min, above-max to max — the NEAREST in-range value. Identity for an
/// unconstrained field.
fn clamp_int(v: i64, f: &Field) -> i64 {
    let v = match f.min {
        Some(mn) if v < mn => mn,
        _ => v,
    };
    match f.max {
        Some(mx) if v > mx => mx,
        _ => v,
    }
}

/// The k-th distinct in-range value for a `unique` range-constrained integer
/// field (#80): k = 0 is the fixture anchor (`clamp(1)`), higher k walks up
/// toward `max` and continues DOWNWARD from the anchor once the top of the
/// range is exhausted. JC0552 refuses a unique field whose cardinality is
/// below 3, so k <= 2 (the probe fixture + the tenant-1 and tenant-2 seeds)
/// always yields three distinct in-range values here; the saturating
/// arithmetic + final clamp keep even an unvalidated design's output in-range.
fn kth_in_range(f: &Field, k: i64) -> i64 {
    let anchor = clamp_int(1, f);
    let headroom = f.max.unwrap_or(i64::MAX).saturating_sub(anchor);
    let v = if k <= headroom {
        anchor.saturating_add(k)
    } else {
        anchor.saturating_sub(k - headroom)
    };
    clamp_int(v, f)
}

/// Fit `base` into `[min_len, max_len]` (#80): truncate to `max_len` CODE
/// POINTS (`.chars()`, the same semantics the generated validator and the
/// OpenAPI minLength/maxLength use — never `.len()` bytes) when too long, pad
/// with 'a' up to `min_len` when too short. JC0552's 4096 `min_len` ceiling
/// bounds the padding.
fn fit_string(base: &str, min_len: Option<u64>, max_len: Option<u64>) -> String {
    let len = base.chars().count() as u64;
    if let Some(mx) = max_len
        && len > mx
    {
        return base.chars().take(mx as usize).collect();
    }
    if let Some(mn) = min_len
        && len < mn
    {
        return format!("{base}{}", "a".repeat((mn - len) as usize));
    }
    base.to_string()
}

/// The in-range plain-string value for a length-constrained field (#80):
/// `test-value` when it fits `[min_len, max_len]`, `"a".repeat(min_len)` when
/// too short, a truncation to `max_len` code points when too long. Shared by
/// `fixture_value` and the non-unique seed literals so the HTTP fixture and
/// the SQL seed stay in agreement (the invariant on `fixture_value`).
fn constrained_fixture_string(f: &Field) -> String {
    const BASE: &str = "test-value";
    if let Some(mn) = f.min_len
        && (BASE.chars().count() as u64) < mn
    {
        return "a".repeat(mn as usize);
    }
    fit_string(BASE, f.min_len, f.max_len)
}

/// The Nth tenant's seed string for a length-constrained field (#80), derived
/// so the fixture (`test-value`…), the tenant-1 unique seed (`seed-…`) and the
/// tenant-2 seed stay DISTINCT after fitting: when truncation would cut the
/// trailing `-{n}` discriminator off the shared "test-value" prefix (colliding
/// with the fixture), the discriminator is front-loaded instead. The distinct
/// leading characters ('t' / 's' / a digit) survive any `max_len >= 1`; a
/// `unique` field with `max_len: 0` is refused at design time (JC0552).
fn constrained_seed_string_n(f: &Field, n: u32) -> String {
    let natural = format!("test-value-{n}");
    let base = if f
        .max_len
        .is_some_and(|mx| (natural.chars().count() as u64) > mx)
    {
        format!("{n}-test-value")
    } else {
        natural
    };
    fit_string(&base, f.min_len, f.max_len)
}

/// The fixture literal for a belongs_to fk column, valued at the SEEDED tenant
/// (id 1): "1" for an integer/synthetic key, the string fixture for a text key.
/// Mirrors the seed in `tenant_seed` so the generated body points at a row the
/// guard can actually resolve.
fn fk_fixture_value(design: &Design, target: &str) -> &'static str {
    match design.target_key_rust_type(target) {
        "String" => "\"1\"",
        _ => "1",
    }
}

/// The server-owned-FK omission (issue #34), db-gated (issue #43) so all three
/// surfaces agree: genroute only emits the `{Entity}Request` DTO in db mode (a
/// memory-mode struct carries no fk columns), so a memory-mode probe must NOT drop
/// `user_id` either — otherwise the probe body and the OpenAPI request schema would
/// diverge from the entity genroute actually deserializes. The fk it carries in
/// memory mode is serde-ignored (the struct has no such field), matching pre-#34
/// behavior; the omission is a db-mode contract only.
fn omits_identity_fk(design: &Design, unit: &ModuleDesign, ep: &Endpoint) -> bool {
    design.wants_db() && design.endpoint_omits_identity_fk(unit, ep)
}

/// `omit_identity_fk` is the server-owned-FK rule (issue #34): true for a
/// GUARDED endpoint's body in an auth design — its `user_id` fk is dropped
/// because the handler injects the session user's id, and the probe must prove
/// a clean client that OMITS it reaches the designed success (not a 422).
fn fixture_json(
    design: &Design,
    m: &ModuleDesign,
    entity: &str,
    omit_identity_fk: bool,
    overrides: &[(&str, &str)],
    keep_defaults: bool,
) -> String {
    let Some(e) = m.entities.iter().find(|e| e.name == entity) else {
        return "{}".to_string();
    };
    // belongs_to fk columns first: a tenant-owned entity's body must carry the
    // fk (NOT NULL) so the handler's Json<Entity> deserializes (else 422 before
    // the stub), valued at the seeded tenant so a scoped query can resolve it.
    // The identity fk (`user_id`) is dropped on guarded bodies (issue #34); a
    // path-redundant parent fk (`habit_id` under `/{habit_id}/checkins`) is
    // dropped because the probe carries it in the URL, not the body (issue #53b).
    let path_fks = design.entity_path_fk_columns(entity);
    let fks = e
        .belongs_to
        .iter()
        .filter(|b| !(omit_identity_fk && design.is_identity_fk(b)))
        .filter(|b| !path_fks.contains(&b.fk_column()))
        .map(|b| {
            format!(
                "\"{}\": {}",
                b.fk_column(),
                fk_fixture_value(design, &b.entity)
            )
        });
    // A STATIC `default` field (issue #53a) is server-owned on CREATE: the probe
    // omits it so the minimal client body proves the server applies the default (not
    // a 422). On UPDATE the field is client-settable (issue #85 D1), so an update
    // probe KEEPS it — the body must match `{Entity}UpdateRequest`, which requires
    // it. A `now`-default timestamp (#110) is dropped from BOTH DTOs (server-owned,
    // immutable), so both probes omit it — inert for designs without the sentinel.
    let cols = e
        .fields
        .iter()
        .filter(|f| (keep_defaults || f.default.is_none()) && !Design::field_is_now_default(f))
        .map(|f| {
            // `overrides` replaces named fields' literals: the reject probe corrupts
            // ONE field to an out-of-range value — the enum sentinel (issue #47) or a
            // #80 constraint violation — so the ONLY reason for a 422 is that field;
            // the composite-unique dup (#115) bumps every competing `unique`/pk field
            // to a DISTINCT valid value so ONLY the composite index can 409. An
            // un-named field keeps its valid fixture value.
            let value = match overrides.iter().find(|(name, _)| *name == f.name) {
                Some((_, literal)) => (*literal).to_string(),
                None => fixture_value(f),
            };
            format!("\"{}\": {}", f.name, value)
        });
    let fields = fks.chain(cols).collect::<Vec<_>>().join(", ");
    format!("{{{fields}}}")
}

/// The happy-path body for an inline-DTO custom action (issue #122): the REQUIRED
/// inline fields, each at a valid fixture value (respecting #80 constraints via
/// `fixture_value`). Optional fields are omitted — they carry `#[serde(default)]`
/// in the generated `{Op}Request`, so a minimal body still deserializes. No fk
/// columns and no entity lookup — an inline body is not a table row.
///
/// `overrides` replaces a named field's literal (issue #217): the reject probe
/// corrupts ONE field to an out-of-range value — mirroring `fixture_json`'s override
/// discipline — so the ONLY reason for a 422 is that field.
///
/// A required field is ALWAYS on the wire; an OPTIONAL field is included ONLY when it
/// is the field an override corrupts (issue #225 Gap B) — the reject body must carry
/// the field it invalidates or the boundary 422 never fires, exactly as `fixture_json`
/// keeps optional non-defaulted fields for the entity path. With no overrides (the
/// happy path) this reduces to required-only, so the happy-path body stays
/// byte-identical to the pre-#225 emission.
fn inline_fixture_json(fields: &[Field], overrides: &[(&str, &str)]) -> String {
    let cols = fields
        .iter()
        .filter(|f| f.required || overrides.iter().any(|(name, _)| *name == f.name))
        .map(|f| {
            let value = match overrides.iter().find(|(name, _)| *name == f.name) {
                Some((_, literal)) => (*literal).to_string(),
                None => fixture_value(f),
            };
            format!("\"{}\": {}", f.name, value)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{cols}}}")
}

/// The POST creator (with a body) mounted at a bare collection `path` — the route
/// that seeds a row addressable under `path/{id}`. `creator_at(m, "/")` is the
/// module-root creator; `creator_at(m, "/tasks")` seeds the second entity (#51).
fn creator_at<'a>(m: &'a ModuleDesign, path: &str) -> Option<&'a Endpoint> {
    m.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == path
            // An inline-DTO body (issue #122) seeds no row — it is not a creator.
            && ep.request_body.as_ref().is_some_and(|rb| !rb.is_inline())
    })
}

/// The POST creator (with a body) whose request body is `entity`, at a bare
/// collection path — used to seed a belongs_to PARENT before its dependent (#51).
fn creator_for_entity<'a>(m: &'a ModuleDesign, entity: &str) -> Option<&'a Endpoint> {
    m.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && param_count(ep) == 0
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity))
    })
}

fn param_count(ep: &Endpoint) -> usize {
    ep.path.matches('{').count()
}

/// The collection path a `/{id}` endpoint acts under: its path with the trailing
/// `/{param}` segment removed (`/tasks/{id}` → `/tasks`, `/{id}` → `/`). The POST
/// creator at THIS path seeds the row the probe addresses (#51).
fn collection_path(ep: &Endpoint) -> String {
    let p = &ep.path;
    let brace = p.rfind('{').expect("parameterized path");
    let cut = p[..brace].rfind('/').unwrap_or(0);
    if cut == 0 {
        "/".to_string()
    } else {
        p[..cut].to_string()
    }
}

/// The fully-qualified collection URL a creator posts to. For the root collection
/// (`"/"`) this is byte-identical to the pre-#51 module-root seed (`{base}/`), so
/// a one-entity module's output is unchanged (conformance no-drift).
fn collection_url(base: &str, coll: &str) -> String {
    if coll == "/" {
        format!("{base}/")
    } else {
        format!("{}{}", base.trim_end_matches('/'), coll)
    }
}

/// One creator POST that seeds a row, threading the credential in auth mode for a
/// guarded creator. Byte-identical to the pre-#51 module-root seed line for the
/// root-collection case.
fn seed_line(
    design: &Design,
    unit: &ModuleDesign,
    url: &str,
    creator: &Endpoint,
    auth: bool,
    comment: &str,
) -> String {
    let body = fixture_json(
        design,
        unit,
        creator
            .request_body
            .as_ref()
            .and_then(|rb| rb.entity.as_deref())
            .expect("creator has an entity body"),
        omits_identity_fk(design, unit, creator),
        &[],
        false, // seed via the creator (POST) — a create body omits defaults
    );
    if auth && creator.is_guarded() {
        let hk = design.test_auth_header();
        format!(
            "    t.post_json_with(\"{url}\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie())]).await; {comment}\n"
        )
    } else {
        format!("    t.post_json(\"{url}\", &serde_json::json!({body})).await; {comment}\n")
    }
}

/// Seed statements + the id literal for a `/{id}` probe (#51): create the entity
/// THIS endpoint operates on — via the POST creator at the endpoint's collection
/// path — preceded by each belongs_to PARENT (via its own creator) so an enforced
/// intra-module FK resolves. Returns None when the target entity has no creator
/// route (the caller emits an AGENT TODO rather than a guaranteed-red probe).
/// The identity fk (handler-injected) and the tenancy entity (seeded by
/// `tenant_seed`) are never re-created here. Byte-identical to the pre-#51 single
/// module-root seed for a one-entity module (collection `"/"`, no such parents).
fn seed_for_id_probe(
    design: &Design,
    top: &ModuleDesign,
    unit: &ModuleDesign,
    base: &str,
    ep: &Endpoint,
    auth: bool,
) -> Option<(String, String)> {
    let coll = collection_path(ep);
    let creator = creator_at(unit, &coll)?;
    let entity_name = creator
        .request_body
        .as_ref()
        .and_then(|rb| rb.entity.as_deref())
        .expect("creator has an entity body");
    let entity = unit.entities.iter().find(|e| e.name == entity_name)?;

    let mut seed = String::new();
    let mut seen = vec![entity.name.clone()];
    seed_parents(design, top, unit, base, entity, auth, &mut seed, &mut seen);
    seed.push_str(&seed_line(
        design,
        unit,
        &collection_url(base, &coll),
        creator,
        auth,
        "// seed id 1",
    ));

    let seed_id = entity
        .fields
        .iter()
        .find(|f| f.name == "id")
        .map(|f| fixture_value(f).trim_matches('"').to_string())
        .unwrap_or_else(|| "1".to_string());
    Some((seed, seed_id))
}

/// True when the POST creator that would seed this `/{id}` endpoint's row is
/// marked `probe: skip` (issue #68). A hand-written validator on that creator
/// rejects the generated seed fixture, so seeding a sibling `/{id}` probe through
/// it would 404 on a CORRECT handler. Only valid for a parameterized path (the
/// caller gates on `param_count(ep) == 1`). Mirrors `seed_for_id_probe`'s creator
/// lookup so the two never disagree about which creator seeds the probe.
fn seed_creator_is_skipped(unit: &ModuleDesign, ep: &Endpoint) -> bool {
    creator_at(unit, &collection_path(ep)).is_some_and(|c| c.probe == ProbePolicy::Skip)
}

/// Append seed lines for `entity`'s belongs_to PARENTS (grandparents first), so a
/// dependent row's fk points at a real parent row. Skips the identity fk (the
/// handler injects it), the tenancy entity (already seeded), and already-seeded
/// entities (cycle guard). A same-module parent is seeded via its own creator; a
/// CROSS-unit parent is seeded only when it is on the entity's TENANCY chain
/// (#260) — a genuinely unenforced cross-module fk still needs no row.
#[allow(clippy::too_many_arguments)]
fn seed_parents(
    design: &Design,
    top: &ModuleDesign,
    unit: &ModuleDesign,
    base: &str,
    entity: &Entity,
    auth: bool,
    seed: &mut String,
    seen: &mut Vec<String>,
) {
    let tenancy = design.tenancy.as_ref().map(|t| t.entity.as_str());
    for b in &entity.belongs_to {
        if design.is_identity_fk(b)
            || Some(b.entity.as_str()) == tenancy
            || seen.contains(&b.entity)
        {
            continue;
        }
        // Same-module parent (the #248 mechanism): its creator lives in THIS unit,
        // so seed it (grandparents first) via the current base. Byte-identical.
        if let (Some(parent_creator), Some(parent)) = (
            creator_for_entity(unit, &b.entity),
            unit.entities.iter().find(|e| e.name == b.entity),
        ) {
            seen.push(b.entity.clone());
            seed_parents(design, top, unit, base, parent, auth, seed, seen);
            seed.push_str(&seed_line(
                design,
                unit,
                &collection_url(base, &parent_creator.path),
                parent_creator,
                auth,
                &format!("// seed parent {} id 1", parent.name),
            ));
            continue;
        }
        // #260: a CROSS-unit parent on THIS entity's tenancy chain. A flat
        // tenant-owned GRANDCHILD's `create_for_memberships` verifies the immediate
        // parent row exists under the caller's membership (`SELECT 1 FROM {parent}
        // WHERE id=? AND {tenant_fk} IN(memberships)`), so that parent row must be
        // seeded even though its creator lives in another module/subroute of the
        // SAME top-level route tree (the only tree `app()` mounts) — via the parent's
        // OWN creator (itself a flat-tenant create under the same cookie, so its
        // tenant fk ∈ memberships holds, and its own grandparents seed by recursion).
        // A genuinely UNENFORCED cross-unit fk (not on the tenancy chain) still needs
        // no row and is skipped (byte-identical to the pre-#260 continue).
        if is_tenancy_chain_parent(design, entity, b)
            && let Some((parent_unit, parent_creator, parent_base)) =
                find_creator_in_tree(top, &b.entity)
            && let Some(parent) = parent_unit.entities.iter().find(|e| e.name == b.entity)
        {
            seen.push(b.entity.clone());
            seed_parents(
                design,
                top,
                parent_unit,
                &parent_base,
                parent,
                auth,
                seed,
                seen,
            );
            seed.push_str(&seed_line(
                design,
                parent_unit,
                &collection_url(&parent_base, &parent_creator.path),
                parent_creator,
                auth,
                &format!("// seed parent {} id 1", parent.name),
            ));
        }
    }
}

/// True when `parent` (a `belongs_to` of `entity`) is the IMMEDIATE tenancy-chain
/// parent whose row a flat tenant-owned GRANDCHILD's `create_for_memberships`
/// verifies — the parent queried by the membership existence check (`SELECT 1 FROM
/// {parent} WHERE id=? AND {tenant_fk} IN(memberships)`, genroute.rs). That is the
/// `belongs_to` whose fk is the entity's tenant-path FIRST hop, and only when the
/// entity is a FLAT tenant-owned grandchild: a DIRECT child's check reads the body
/// tenant fk (no parent row → byte-identical), and a PATH-SCOPED entity never emits
/// this check. Any OTHER cross-unit fk is unenforced and needs no seeded row.
fn is_tenancy_chain_parent(design: &Design, entity: &Entity, parent: &BelongsTo) -> bool {
    if !super::genroute::entity_is_flat_tenant_owned(entity, design) {
        return false;
    }
    design
        .tenant_path(&entity.name)
        .and_then(|tp| tp.joins.into_iter().next())
        .is_some_and(|first| first.child_fk == parent.fk_column())
}

/// Locate the param-count-0 POST creator for `entity` anywhere in `top`'s route
/// tree (the top-level module + its nested subroutes), returning the declaring
/// unit, that creator, and the creator's fully-accumulated mount base. Scoped to
/// `top` on PURPOSE: the generated `app()` mounts ONLY the top-level module under
/// test, so a cross-unit seed URL must resolve within that one tree.
fn find_creator_in_tree<'a>(
    top: &'a ModuleDesign,
    entity: &str,
) -> Option<(&'a ModuleDesign, &'a Endpoint, String)> {
    find_creator_in_unit(top, entity, &top.effective_mount())
}

fn find_creator_in_unit<'a>(
    unit: &'a ModuleDesign,
    entity: &str,
    base: &str,
) -> Option<(&'a ModuleDesign, &'a Endpoint, String)> {
    if unit.entities.iter().any(|e| e.name == entity)
        && let Some(creator) = creator_for_entity(unit, entity)
    {
        return Some((unit, creator, base.to_string()));
    }
    for sub in &unit.subroutes {
        let sub_base = format!("{base}{}", sub.effective_mount());
        if let Some(hit) = find_creator_in_unit(sub, entity, &sub_base) {
            return Some(hit);
        }
    }
    None
}

/// Seed statements for the enforced `belongs_to` PARENTS of a create probe's entity
/// (#248), so a same-module DDL FK (an fk-alias #119 is always one) resolves at
/// INSERT instead of 500-ing the `_returns_201` probe — AND (#260) so a flat
/// tenant-owned GRANDCHILD's intermediate parent row exists for its
/// `create_for_memberships` membership check (else 403≠201). Mirrors the `/{id}`
/// probe: reuses `seed_parents`, which excludes the identity fk (handler-injected)
/// and the tenancy entity (app()-seeded), seeds a same-module parent via its own
/// creator, seeds a CROSS-unit tenancy-chain parent (in the same top-level tree)
/// via ITS creator, and still skips a genuinely unenforced cross-module fk. Aliased
/// fks (`from_account_id`/`to_account_id`) both target ONE parent entity, so it is
/// seeded once and both fks resolve to it. Empty (byte-identical) for a non-create
/// endpoint, a bodyless/inline create, or an entity with no such parent.
fn create_probe_parent_seed(
    design: &Design,
    top: &ModuleDesign,
    unit: &ModuleDesign,
    base: &str,
    ep: &Endpoint,
    auth: bool,
) -> String {
    if ep.method != HttpMethod::POST {
        return String::new();
    }
    let Some(entity_name) = ep.request_body.as_ref().and_then(|rb| rb.entity.as_deref()) else {
        return String::new();
    };
    let Some(entity) = unit.entities.iter().find(|e| e.name == entity_name) else {
        return String::new();
    };
    let mut seed = String::new();
    let mut seen = vec![entity.name.clone()];
    seed_parents(design, top, unit, base, entity, auth, &mut seed, &mut seen);
    seed
}

/// The distinct pk the tenancy entity's own create probe must post so it doesn't
/// 409 against the tenant row app() auto-seeds (#249). `app()` seeds the tenant at
/// id 1 (and id 2 for the isolation second tenant) whenever this module needs the
/// tenant seed, so a create body reusing the fixture pk `1` collides. Returns
/// `Some(("id", "3"))` — a pk past BOTH auto-seeded tenants — when: `ep` is a POST
/// whose body IS the tenancy entity, this module seeds the tenant
/// (`module_needs_tenant`, the SAME gate that also emits the second-tenant seed),
/// and the tenancy entity carries an explicit integer `id` the create body posts.
/// None otherwise — a synthetic-pk tenancy entity autoincrements past the seed for
/// free, and a non-tenancy create is untouched — so every existing suite stays
/// byte-identical.
fn tenancy_create_pk_override(
    design: &Design,
    unit: &ModuleDesign,
    ep: &Endpoint,
) -> Option<(&'static str, &'static str)> {
    if ep.method != HttpMethod::POST {
        return None;
    }
    let tenancy = design.tenancy.as_ref()?;
    let entity_name = ep
        .request_body
        .as_ref()
        .and_then(|rb| rb.entity.as_deref())?;
    if entity_name != tenancy.entity {
        return None;
    }
    if !module_needs_tenant(design, unit) {
        return None;
    }
    let entity = unit.entities.iter().find(|e| e.name == entity_name)?;
    // Only when the body carries an explicit integer pk: a synthetic pk
    // autoincrements past the seed on its own, and the integer-literal tenant seed
    // (`tenant_row_cols_vals`) already assumes an integer tenancy pk.
    entity
        .fields
        .iter()
        .any(|f| f.name == "id" && matches!(f.field_type, FieldType::Integer))
        .then_some(("id", "3"))
}

/// True when this endpoint's SUCCESS requires a credential/signature the generator
/// cannot synthesize, so a minimal-body probe can never reach the designed success
/// status. Two shapes: (a) a signature-authenticated webhook (Stripe-style — a bad
/// or missing signature 400/401s), and (b) a NON-session endpoint that declares a
/// 401/403 (a `public` login 401s bad creds; an api-key route 401/403s a missing
/// key). A session-GUARDED endpoint is excluded: the generator threads its cookie,
/// so its success test IS greenable. For these gated endpoints we emit an AGENT
/// TODO instead of an un-greenable `_returns_<status>` assertion.
fn endpoint_is_credential_gated(ep: &Endpoint) -> bool {
    ep.declares_signature_auth()
        || (!ep.is_guarded() && ep.errors.iter().any(|e| e.status == 401 || e.status == 403))
}

struct TestOut {
    code: String,
    todos: Vec<String>,
    count: usize,
    /// Issue #47: enum "reject" tests PASS on stubs (an out-of-range value 422s at
    /// deserialization, before the handler runs), so they are subtracted from
    /// `expected_failing` — they are not part of the RED-on-stubs baseline.
    reject: usize,
    /// Auth mode: success tests on guarded endpoints carry a session cookie and
    /// every guarded endpoint also gets a no-cookie 401 test.
    auth: bool,
}

/// A value guaranteed to be OUT-OF-RANGE for any declared enum `values` set — the
/// reject probe sends it in an enum field to prove the request boundary answers
/// 422 (JC0422) before the DB (issue #47).
const ENUM_REJECT_SENTINEL: &str = "__invalid_enum_value__";

/// The first enum (`values`) field of an endpoint's request-body entity that is
/// present on the wire — the field the reject probe corrupts to an out-of-range
/// value. A defaulted enum field (issue #53a) is omitted from the request DTO, so
/// a bad value would be ignored, not 422'd — skip it (there is nothing to reject
/// at the boundary).
fn first_enum_field<'a>(unit: &'a ModuleDesign, entity: &str) -> Option<&'a str> {
    unit.entities
        .iter()
        .find(|e| e.name == entity)
        .and_then(|e| {
            e.fields
                .iter()
                .find(|f| f.values.is_some() && f.default.is_none())
        })
        .map(|f| f.name.as_str())
}

/// The largest `max_len` for which the reject probe materializes an over-max
/// string at test run time (`"a".repeat(max_len + 1)`) — matches JC0552's 4096
/// `min_len` fixture ceiling, so a generated suite never allocates beyond
/// ~4KB per probe. Above it the probe falls back to the under-`min_len`
/// direction, or (min_len absent/0) emits nothing — a bound too large to
/// violate cheaply goes unprobed (0.6.5 T1 review, Important-b).
const REJECT_LEN_CAP: u64 = 4096;

/// The out-of-range literal the #80 reject probe sends for a constrained
/// field, or None when NO rejectable direction exists. Directions mirror
/// exactly the bounds the generated validator enforces (genroute's
/// `bounds_rules` gates the vacuous spellings `min: i64::MIN`,
/// `max: i64::MAX`, `min_len: 0`, `max_len: u64::MAX` out of the runtime
/// check — and here the checked arithmetic fails on precisely those, so the
/// probe never asserts a 422 the validator won't produce):
/// - integer: `max + 1` (checked), falling back to `min - 1` (checked),
///   rendered via [`int_literal`] so an out-of-i32-range value compiles
///   inside `serde_json::json!`;
/// - string: `"a".repeat(max_len + 1)` — an EXPRESSION, valid inside
///   `serde_json::json!`, so the generated file never embeds a giant literal —
///   capped by [`REJECT_LEN_CAP`], falling back to `"a".repeat(min_len - 1)`.
fn constraint_reject_literal(f: &Field) -> Option<String> {
    match f.field_type {
        FieldType::Integer => f
            .max
            .and_then(|mx| mx.checked_add(1))
            .or_else(|| f.min.and_then(|mn| mn.checked_sub(1)))
            .map(int_literal),
        // A `values` field rides the enum reject probe instead (and a
        // values+length combination is refused at design time, JC0552).
        FieldType::String if f.values.is_none() => {
            if let Some(mx) = f.max_len
                && mx <= REJECT_LEN_CAP
            {
                return Some(format!("\"a\".repeat({})", mx + 1));
            }
            f.min_len
                .filter(|&mn| mn >= 1)
                .map(|mn| format!("\"a\".repeat({})", mn - 1))
        }
        _ => None,
    }
}

/// The first request-body field carrying a #80 range/length constraint with a
/// derivable out-of-range literal — the field the constraint reject probe
/// corrupts. A defaulted field is skipped for the same reason as
/// [`first_enum_field`]: it is omitted from the create request DTO, so a bad
/// value would be dropped, not 422'd. A constrained field with no rejectable
/// direction (both extremes vacuous) is passed over — nothing violates its
/// bound, so there is nothing to probe.
fn first_constraint_reject<'a>(unit: &'a ModuleDesign, entity: &str) -> Option<(&'a str, String)> {
    unit.entities
        .iter()
        .find(|e| e.name == entity)
        .and_then(|e| {
            e.fields
                .iter()
                .filter(|f| f.default.is_none() && (has_int_range(f) || has_len_range(f)))
                .find_map(|f| constraint_reject_literal(f).map(|lit| (f.name.as_str(), lit)))
        })
}

/// A request expression `t.<verb>(...)`. In auth mode a guarded endpoint threads
/// the test cookie via the `_with` helper variants; otherwise the plain verb.
fn request_expr(
    design: &Design,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded_and_auth: bool,
    overrides: &[(&str, &str)],
) -> String {
    let body = || {
        ep.request_body
            .as_ref()
            // The omission keys on the ENDPOINT being guarded (the design-level
            // rule), not on whether THIS request threads a cookie — a guarded
            // endpoint's 401 probe still sends the guarded body shape.
            .map(|rb| match rb.entity.as_deref() {
                Some(entity) => fixture_json(
                    design,
                    unit,
                    entity,
                    omits_identity_fk(design, unit, ep),
                    overrides,
                    // An UPDATE (PUT/PATCH) probe keeps `default` fields so the body
                    // matches `{Entity}UpdateRequest` (issue #85 D1); a create omits them.
                    ep.method.is_update(),
                ),
                // An inline-DTO body (issue #122) builds from its own fields; an
                // override corrupts one field for the #217 reject probe.
                None => inline_fixture_json(&rb.fields, overrides),
            })
            .unwrap_or_else(|| "{}".to_string())
    };
    if guarded_and_auth {
        // The test credential header follows the auth model: `cookie` (session)
        // or `authorization` (jwt Bearer) — issue #29. `test_cookie()` returns the
        // matching header value.
        let cookie = format!("&[(\"{}\", &test_cookie())]", design.test_auth_header());
        match ep.method {
            HttpMethod::GET => format!("t.get_with(\"{path}\", {cookie}).await"),
            HttpMethod::DELETE => format!("t.delete_with(\"{path}\", {cookie}).await"),
            HttpMethod::POST => format!(
                "t.post_json_with(\"{path}\", &serde_json::json!({}), {cookie}).await",
                body()
            ),
            HttpMethod::PUT => format!(
                "t.put_json_with(\"{path}\", &serde_json::json!({}), {cookie}).await",
                body()
            ),
            HttpMethod::PATCH => format!(
                "t.patch_json_with(\"{path}\", &serde_json::json!({}), {cookie}).await",
                body()
            ),
        }
    } else {
        match ep.method {
            HttpMethod::GET => format!("t.get(\"{path}\").await"),
            HttpMethod::DELETE => format!("t.delete(\"{path}\").await"),
            HttpMethod::POST => {
                format!(
                    "t.post_json(\"{path}\", &serde_json::json!({})).await",
                    body()
                )
            }
            HttpMethod::PUT => {
                format!(
                    "t.put_json(\"{path}\", &serde_json::json!({})).await",
                    body()
                )
            }
            HttpMethod::PATCH => {
                format!(
                    "t.patch_json(\"{path}\", &serde_json::json!({})).await",
                    body()
                )
            }
        }
    }
}

/// The accumulated mount `base` with every mount-INHERITED path param substituted
/// by the seeded parent id `1` (issue #81). A subroute-mounted module carries its
/// ancestor's param in the MOUNT prefix (`/workspaces/{workspace_id}/channels`),
/// not in `ep.path`; left literal, the router 400/404s the whole group and a
/// correct app's tests are red by construction. Every `{param}` in `base` is a
/// mount-inherited ancestor fk (or parent pk) whose row app()'s tenant chain seeds
/// at id 1 (`tenant_seed`/`seed_tenant1_chain` — the same rows the isolation test's
/// `cbase` pins), so substituting each to `1` makes the probe URL concrete AND
/// resolvable. The endpoint's OWN `/{id}` param lives in `ep.path` (appended AFTER
/// `base`, so never touched here) and is substituted separately by the seeded row
/// id. A FLAT mount carries no `{param}`, so this is the identity — every
/// non-nested design stays byte-identical. Also reused on a FULL path to pin an
/// endpoint's own `{param}`s for the seedless 401 guard probes (issue #123b) —
/// the guard rejects before any id is looked up, so a literal `1` suffices.
fn concrete_mount_base(base: &str) -> String {
    let mut out = String::with_capacity(base.len());
    let mut rest = base;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        match rest[open..].find('}') {
            Some(rel_close) => {
                out.push('1');
                rest = &rest[open + rel_close + 1..];
            }
            // Unbalanced brace (never valid in a mount): emit the remainder verbatim.
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn unit_tests(
    design: &Design,
    top: &ModuleDesign,
    unit: &ModuleDesign,
    base: &str,
    out: &mut TestOut,
) {
    let auth = out.auth;
    // Resolve the FULL path per endpoint against a mount base whose inherited params
    // are pinned to the seeded parent id 1 (issue #81). The RAW `base` still threads
    // through the subroute recursion below so the accumulation stays intact.
    let cbase = concrete_mount_base(base);

    for ep in &unit.endpoints {
        let full_path = format!("{}{}", cbase.trim_end_matches('/'), ep.path);
        let fn_base = &ep.operation_id;
        let status = ep.success.status;
        // A public_read GET (#105) is emitted UNGUARDED regardless of its declared
        // `auth_required` (the shared `Design::endpoint_is_public_read_get` — the
        // same predicate genroute keys the handler on), so it must probe WITHOUT a
        // credential and must NOT get a 401 test: a no-cookie request to the public
        // feed correctly 200s, and asserting 401 would generate a permanently-RED
        // test on a correct app. A role-gated GET keeps its guard and its 401 probe.
        let guarded = auth && ep.is_guarded() && !design.endpoint_is_public_read_get(unit, ep);
        // Endpoints whose success needs a credential/signature the generator can't
        // supply (login, signed webhook, api-key route): no un-greenable success
        // probe — emit a TODO instead. Detected by heuristic OR declared
        // explicitly with `probe: skip` (issue #11) so a design the heuristic
        // misses can still reach `ok:true`. Heuristic shape (b) (a non-session
        // 401/403 route) is unguarded by definition, so it gets no 401 test;
        // heuristic shape (a) (a signature webhook) and a `probe: skip` endpoint
        // CAN be guarded — their 401 guard test still emits below (issue #123b).
        let probe_skip = ep.probe == ProbePolicy::Skip;
        let gated = endpoint_is_credential_gated(ep) || probe_skip;

        if gated {
            // Issue #122 Part B: the TODO is AUTH-AWARE. In a design with an active
            // auth model the credential/401 wording is accurate (a skipped success
            // needs a credential; an unguarded gated route also owns its 401/403
            // rejection test). In a NO-auth design there is no credential and no 401
            // — an inline custom action (`POST /checkout`) marked `probe: skip` just
            // needs its own success test — so the wording drops every auth reference.
            // Byte-identical for auth designs (`wants_auth()` true).
            let auth_active = design.wants_auth();
            let reason = if !auth_active {
                if probe_skip {
                    "is marked `probe: skip` — the generator can't synthesize its success"
                } else {
                    "needs a success the generator can't synthesize"
                }
            } else if probe_skip {
                "is marked `probe: skip` — the generator can't synthesize a credential for its success"
            } else {
                "authenticates via a credential/signature the generator can't supply"
            };
            // Issue #123b: dropping the un-greenable success probe must NOT also
            // drop the `_without_auth_is_401` guard test — that assertion is
            // GREENABLE (the generated guard rejects a credential-less request
            // before any handler logic) and deleting it silently un-tests a real
            // security guard. Any `{param}` is pinned to a literal id: a 401
            // rejection happens before the id is ever looked up, so no seed is
            // needed. The TODO then asks for the success test only; an UNGUARDED
            // gated endpoint (login, signed webhook) keeps the credential ask — its
            // rejection is handler logic, not a generated guard. In a NO-auth design
            // (issue #122 Part B) the ask carries no credential/401 wording.
            let ask = if guarded {
                "write its success test (with a valid credential) in your own test file; its `_without_auth_is_401` guard test is already generated"
            } else if auth_active {
                "write its success test (with a valid credential) and its 401/403 rejection test in your own test file"
            } else {
                "write your own success test for this custom action"
            };
            out.todos.push(format!(
                "// AGENT TODO: {fn_base} ({:?} {full_path}) {reason} — {ask}.",
                ep.method
            ));
            if guarded {
                push_401_test(
                    design,
                    out,
                    unit,
                    ep,
                    &concrete_mount_base(&full_path),
                    false,
                );
            }
        } else if param_count(ep) == 0 {
            // #249: the tenancy entity's own create probe must NOT reuse the pk
            // app() auto-seeds for the tenant (id 1, and id 2 for the isolation
            // second tenant) — that would 409 on the PK. Post a distinct pk so the
            // create reaches its 201. None (byte-identical) for every non-tenancy
            // create and for a synthetic-pk tenancy entity (whose create
            // autoincrements past the seed for free).
            let pk_override = tenancy_create_pk_override(design, unit, ep);
            let overrides: Vec<(&str, &str)> = pk_override.into_iter().collect();
            let request = request_expr(design, unit, ep, &full_path, guarded, &overrides);
            // #248: a create whose body carries an enforced same-module belongs_to fk
            // (e.g. an fk-alias `Transfer belongs_to Account as from/to`) needs its
            // parent rows seeded first, or the DDL FK violation 500s the 201 probe —
            // mirror the /{id} probe (`seed_parents`). Empty (byte-identical) for a
            // create without such a parent.
            let seed = create_probe_parent_seed(design, top, unit, &cbase, ep, auth);
            // A creator that echoes its entity must echo the id it was given —
            // catches inserts that return a backend default (0) instead. When the pk
            // was bumped (#249) the echo asserts the bumped value, not the fixture.
            // #263: skip the id-echo when `success.list` — a list creator responds with
            // a JSON ARRAY (`Json<Vec<X>>` / the #259 `(StatusCode, Json<Vec<X>>)` tuple),
            // so `body["id"]` (string-indexing an array) is always `null` and the probe
            // could never green on a correct handler. A list response has no single
            // canonical id to echo.
            // #266: also skip when the success status returns NO JSON body — a 204
            // (`NoContent`) or a 3xx (`Redirect`) has an EMPTY body, so
            // `from_str(&res.text())` would panic on `""`. Only a 2xx that is not 204
            // (200/201/202…) carries the JSON the echo reads.
            let body_bearing = (200..300).contains(&status) && status != 204;
            let id_echo = (ep.method == HttpMethod::POST && !ep.success.list && body_bearing)
                .then_some(ep.request_body.as_ref())
                .flatten()
                .and_then(|rb| rb.entity.as_deref())
                .filter(|entity| ep.success.entity.as_deref() == Some(entity))
                .and_then(|entity| unit.entities.iter().find(|e| e.name == entity))
                .and_then(|e| e.fields.iter().find(|f| f.name == "id"))
                .map(|f| {
                    let echoed = pk_override
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_else(|| fixture_value(f));
                    format!(
                        "    let body: serde_json::Value = serde_json::from_str(&res.text()).expect(\"json body\");\n    assert_eq!(body[\"id\"], serde_json::json!({echoed}), \"design: created {} echoes its id\");\n",
                        ep.success.entity.as_deref().unwrap_or("entity")
                    )
                })
                .unwrap_or_default();
            out.code.push_str(&format!(
                "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n{id_echo}}}\n\n"
            ));
            out.count += 1;
            if guarded {
                push_401_test(design, out, unit, ep, &full_path, false);
            }
            // Issue #47: an enum request body gets an out-of-range reject probe.
            if let Some(field) = ep
                .request_body
                .as_ref()
                .and_then(|rb| rb.entity.as_deref())
                .and_then(|entity| first_enum_field(unit, entity))
            {
                push_enum_reject_test(design, out, unit, ep, &full_path, guarded, field, "");
            }
            // #80: a range/length-constrained request body gets one too.
            if let Some((field, literal)) = ep
                .request_body
                .as_ref()
                .and_then(|rb| rb.entity.as_deref())
                .and_then(|entity| first_constraint_reject(unit, entity))
            {
                push_constraint_reject_test(
                    design, out, unit, ep, &full_path, guarded, field, &literal, "",
                );
            }
            // #217: an inline-DTO custom action (`rb.entity == None`) rejects an
            // out-of-range inline field too (no-op for entity/bodyless endpoints).
            // A collection create (`POST /`) is never a path-scoped tenant detail
            // route, so it needs no #267 membership seed ("").
            push_inline_reject_test(design, out, unit, ep, &full_path, guarded, "");
        } else if param_count(ep) == 1 && seed_creator_is_skipped(unit, ep) {
            // Issue #68: the creator that would seed this `/{id}` probe is marked
            // `probe: skip` — a hand-written validator on it rejects the generated
            // fixture (JR4: url must start http/https), so the seed POST would fail
            // and every downstream sibling probe would 404 on a CORRECT handler.
            // Emit an AGENT TODO instead of a guaranteed-red probe (excluded from
            // expected_failing). The missing-id 404 probe below needs no seed, so it
            // still emits — the creator's validator never touches the getter.
            out.todos.push(format!(
                "// AGENT TODO: {fn_base} ({:?} {full_path}) — its seed creator is `probe: skip` (a hand-written validator rejects the generated fixture), so an auto-seeded {{id}} would 404. Seed a valid row and encode its success case in your own test file.",
                ep.method
            ));
            // Issue #123b: the guard test survives the skipped seed — the guard
            // rejects a credential-less request before the id lookup, so a
            // literal id stands in and no seeded row is needed.
            if guarded {
                push_401_test(
                    design,
                    out,
                    unit,
                    ep,
                    &concrete_mount_base(&full_path),
                    false,
                );
            }
            // #236: an inline-DTO custom action on this `/{id}` path still gets its
            // 422 reject probes even though the seed creator is `probe: skip` — the
            // inline 422 precedes any id lookup (needs no seeded row), so the
            // concrete mount base (its own `{id}` pinned to `1`, as the 401 above)
            // suffices. No-op for entity/bodyless endpoints. The seed creator is
            // `probe: skip` here, so there is no reusable membership seed to prepend
            // (#267) — pass "" (this un-seedable shape is a separate residual).
            push_inline_reject_test(
                design,
                out,
                unit,
                ep,
                &concrete_mount_base(&full_path),
                guarded,
                "",
            );
        } else if param_count(ep) == 1 {
            // Issue #51: seed the row THIS `/{id}` endpoint addresses via ITS OWN
            // entity's creator (`POST /tasks` for `/tasks/{id}`), walking belongs_to
            // parents first — not the module-root creator, which would seed the
            // wrong entity and make the probe 404 on a CORRECT handler. Seeds/probes
            // resolve against the mount-substituted `cbase` (issue #81) so a nested
            // module's seed POST + `/{id}` probe both hit the concrete parent URL.
            if let Some((seed, seed_id)) = seed_for_id_probe(design, top, unit, &cbase, ep, auth) {
                let seeded_path = full_path.replacen(&regex_free_param(&ep.path), &seed_id, 1);
                let request = request_expr(design, unit, ep, &seeded_path, guarded, &[]);
                out.code.push_str(&format!(
                    "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n}}\n\n"
                ));
                out.count += 1;
                if guarded {
                    push_401_test(design, out, unit, ep, &seeded_path, true);
                }
                // #267: on a PATH-SCOPED route of the tenant entity's OWN module —
                // whose `app()` seeds NO membership (`module_needs_tenant` is false
                // for the tenant root; a child module's `app()` DOES pre-seed) — the
                // membership-verified `Dep<Tenant>` guard 404s a non-member BEFORE
                // body deserialization, so a reject probe would 404 instead of the
                // 422 validator. Prepend the SAME `seed` the 2xx probe uses (the
                // create that seeds user 1's membership at `seed_id`), so the reject
                // body reaches the validator. Empty ("") — hence byte-identical — for
                // a child module (its `app()` pre-seeds), an unguarded route (no
                // guard to 404), and every non-tenant / MembershipSet route.
                let reject_seed: &str = if guarded
                    && !module_needs_tenant(design, unit)
                    && matches!(
                        design.endpoint_tenant_shape(unit, ep),
                        TenantShape::PathScoped { .. }
                    ) {
                    &seed
                } else {
                    ""
                };
                // Issue #47: update path (PUT/PATCH /{id}) rejects out-of-range too.
                if let Some(field) = ep
                    .request_body
                    .as_ref()
                    .and_then(|rb| rb.entity.as_deref())
                    .and_then(|entity| first_enum_field(unit, entity))
                {
                    push_enum_reject_test(
                        design,
                        out,
                        unit,
                        ep,
                        &seeded_path,
                        guarded,
                        field,
                        reject_seed,
                    );
                }
                // #80: the update path rejects a constraint violation too.
                if let Some((field, literal)) = ep
                    .request_body
                    .as_ref()
                    .and_then(|rb| rb.entity.as_deref())
                    .and_then(|entity| first_constraint_reject(unit, entity))
                {
                    push_constraint_reject_test(
                        design,
                        out,
                        unit,
                        ep,
                        &seeded_path,
                        guarded,
                        field,
                        &literal,
                        reject_seed,
                    );
                }
                // #217: an inline-DTO custom action at `/{id}/…` rejects an
                // out-of-range inline field too (the 422 precedes any id lookup, so
                // the concrete seeded path suffices). No-op for entity bodies. The
                // #267 seed applies here too (a tenant-entity inline action under
                // `/{tenant_fk}/…` is path-scoped).
                push_inline_reject_test(design, out, unit, ep, &seeded_path, guarded, reject_seed);
            } else {
                out.todos.push(format!(
                    "// AGENT TODO: {fn_base} ({:?} {full_path}) has no creator route to seed its {{id}} — encode its success case in your own test file.",
                    ep.method
                ));
                // Issue #153: no creator drops only the un-seedable success
                // probe — the guard test survives (the guard rejects a
                // credential-less request before the id lookup, so a literal
                // id stands in and no seeded row is needed; same as #123b).
                if guarded {
                    push_401_test(
                        design,
                        out,
                        unit,
                        ep,
                        &concrete_mount_base(&full_path),
                        false,
                    );
                }
                // #236: an inline-DTO custom action on this `/{id}` path with no
                // creator to seed still gets its 422 reject probes — the inline 422
                // precedes any id lookup (needs no seeded row), so the concrete mount
                // base (its own `{id}` pinned to `1`, as the seedless 401 above)
                // suffices. No-op for entity/bodyless endpoints. No creator ⇒ no
                // reusable membership seed to prepend (#267) — pass "".
                push_inline_reject_test(
                    design,
                    out,
                    unit,
                    ep,
                    &concrete_mount_base(&full_path),
                    guarded,
                    "",
                );
            }
        } else if param_count(ep) >= 1 {
            out.todos.push(format!(
                "// AGENT TODO: {fn_base} ({:?} {full_path}) needs a creator at \"/\" to seed ids — encode its success case in your own test file.",
                ep.method
            ));
            // Issue #153: a multi-param path blocks only the seeded success
            // probe — the guard test survives with every `{param}` pinned to a
            // literal id (a 401 rejection precedes any id lookup; same as #123b).
            if guarded {
                push_401_test(
                    design,
                    out,
                    unit,
                    ep,
                    &concrete_mount_base(&full_path),
                    false,
                );
            }
        }

        // #247 residual: a role-gated FLAT (membership-set) `/{id}` mutation gates on
        // the caller's MEMBERSHIP role in the ROW's tenant BEFORE any existence check —
        // `require_membership_role` 403s an empty JOIN, so a MISSING id answers 403, not
        // 404 (a row nobody owns is a row you are not an owner of). This is the SAME
        // "the gate precedes the id lookup" reason a credential-`gated` route skips its
        // `_missing_id_is_404` probe (line above): emitting it here would be un-greenable
        // against a correct impl. The cross-tenant 403 is already covered by the
        // isolation test, and a missing id is the same secure 403 — so we neither assert
        // a 404 (unsatisfiable) nor push the generic "encode 404" TODO (it would mis-steer
        // toward that un-greenable test).
        let membership_role_gated = !ep.required_roles.is_empty()
            && matches!(
                design.endpoint_tenant_shape(unit, ep),
                TenantShape::MembershipSet
            );
        for ec in &ep.errors {
            if ec.status == 404 && param_count(ep) == 1 && !gated && !membership_role_gated {
                let missing_path = full_path.replacen(&regex_free_param(&ep.path), "999999", 1);
                // Build the probe with the endpoint's REAL method (and body/cookie)
                // via the same builder the success test uses — a GET probe at a
                // POST-only `/{id}` action would hit 405, not the 404 we assert.
                // Guarded endpoints run the auth guard before not-found logic, so
                // `request_expr` threads the cookie when guarded.
                let request = request_expr(design, unit, ep, &missing_path, guarded, &[]);
                out.code.push_str(&format!(
                    "#[tokio::test]\nasync fn {fn_base}_missing_id_is_404() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), 404, \"design: {fn_base} lists 404 ({when}); body: {{}}\", res.text());\n}}\n\n",
                    when = ec.when
                ));
                out.count += 1;
            } else if !(ec.status == 404 && membership_role_gated) {
                out.todos.push(format!(
                    "// AGENT TODO: design lists {} ({}) for {fn_base} — encode it in your own test file.",
                    ec.status, ec.when
                ));
            }
        }
    }

    // #115: composite / multi-column UNIQUE 409 conflict tests for this unit's
    // entities (appended after the per-endpoint probes, before recursing).
    push_composite_unique_tests(design, top, unit, base, out);

    for sub in &unit.subroutes {
        let sub_base = format!("{}{}", base, sub.effective_mount());
        unit_tests(design, top, sub, &sub_base, out);
    }
}

/// Composite / multi-column UNIQUE 409 tests (#115). For each entity in this unit
/// that declares a `unique` group AND has a probeable collection creator (POST),
/// emit `{entity}_composite_unique_{ordinal}_is_409` per group: seed the belongs_to
/// parents once, create a first row, then POST a SECOND row that AGREES on the
/// group's columns but is bumped to a DISTINCT value on every OTHER competing
/// unique key — each single-column `unique` field and the explicit client-supplied
/// pk (a synthetic pk auto-increments, already distinct) — so the ONLY constraint a
/// duplicate can trip is THIS composite `CREATE UNIQUE INDEX`, via `db_error`. That
/// isolation is what keeps the test honest: without it, an entity carrying a second
/// unique key (another field, another group, or a constant pk in the body) would
/// 409 on THAT key and pass GREEN even if the composite index were missing.
///
/// RED on stubs (the first create never inserts, or a stub echo makes the second a
/// 201, not a 409), so it counts toward `expected_failing` like every create probe
/// — NOT a `reject`. Skipped (creator credential-gated / `probe: skip`), or emitted
/// as an AGENT TODO when a competing unique constraint shares no isolable column
/// with the group (e.g. two fk-only groups, or a `unique` field / pk INSIDE the
/// group): the 409 could then not be attributed to the composite index alone.
fn push_composite_unique_tests(
    design: &Design,
    top: &ModuleDesign,
    unit: &ModuleDesign,
    base: &str,
    out: &mut TestOut,
) {
    let cbase = concrete_mount_base(base);
    let auth = out.auth;
    for e in &unit.entities {
        if e.unique.is_empty() {
            continue;
        }
        let Some(creator) = creator_for_entity(unit, &e.name) else {
            continue;
        };
        if endpoint_is_credential_gated(creator) || creator.probe == ProbePolicy::Skip {
            continue;
        }
        let guarded = auth && creator.is_guarded();
        let url = collection_url(&cbase, &creator.path);
        // Seed belongs_to parents so an enforced intra-module fk resolves (the
        // identity fk is session-injected, the tenancy entity is app()-seeded —
        // both skipped by `seed_parents`).
        let mut seed = String::new();
        let mut seen = vec![e.name.clone()];
        seed_parents(design, top, unit, &cbase, e, auth, &mut seed, &mut seen);
        let first = request_expr(design, unit, creator, &url, guarded, &[]);
        let snake = Design::to_snake(&e.name);
        let status = creator.success.status;
        // A field the create body actually carries: a `default`/`now`-default field
        // is server-owned and dropped from the create DTO, so it can neither be
        // bumped nor is it a body-carried competing key (mirrors fixture_json's
        // create-body cols filter). An explicit `id` field is a required, constant
        // `{Entity}Request.id` (genroute) → the body carries a fixed pk that a
        // duplicate would 409 on; a synthetic pk (no `id` field) auto-increments.
        let in_create_body = |name: &str| {
            e.fields
                .iter()
                .any(|f| f.name == name && f.default.is_none() && !Design::field_is_now_default(f))
        };
        let has_body_pk = e.fields.iter().any(|f| f.name == "id");
        for (ordinal, group) in e.unique.iter().enumerate() {
            let cols_human = group.join(", ");
            let group_cols: std::collections::BTreeSet<&str> =
                group.iter().map(String::as_str).collect();
            // Bump every competing single-column `unique` field and the explicit pk
            // to a DISTINCT value — but only when it is OUTSIDE the group (a group
            // column is held constant), IN the create body, and expressible as a
            // distinct value.
            let overrides: Vec<(String, String)> = e
                .fields
                .iter()
                .filter(|f| {
                    (f.unique || (f.name == "id" && has_body_pk))
                        && !group_cols.contains(f.name.as_str())
                        && in_create_body(&f.name)
                        && can_bump_distinct(f)
                })
                .map(|f| (f.name.clone(), distinct_fixture_value(f)))
                .collect();
            let bumped: std::collections::BTreeSet<&str> =
                overrides.iter().map(|(c, _)| c.as_str()).collect();
            // Isolation: every OTHER competing unique constraint — each single-column
            // `unique` field, the explicit pk, and each other composite group — must
            // share a bumped column, else the dup would 409 on IT (a false green for
            // this index). A constraint held fully constant can't be separated → TODO.
            let single_uniques = e.fields.iter().filter(|f| f.unique).map(|f| {
                std::iter::once(f.name.as_str()).collect::<std::collections::BTreeSet<&str>>()
            });
            let pk_constraint = has_body_pk
                .then(|| std::iter::once("id").collect::<std::collections::BTreeSet<&str>>());
            let other_groups = e
                .unique
                .iter()
                .enumerate()
                .filter(|(o, _)| *o != ordinal)
                .map(|(_, g)| {
                    g.iter()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<&str>>()
                });
            let masked = single_uniques
                .chain(pk_constraint)
                .chain(other_groups)
                .any(|c| c.is_disjoint(&bumped));
            if masked {
                out.code.push_str(&format!(
                    "// AGENT TODO: {snake} composite UNIQUE({cols_human}) (group #{ordinal}) shares no isolable column with a competing unique constraint on {name} — a duplicate row would 409 on THAT constraint, not this composite index, so an auto-generated probe could pass GREEN even if this index were missing. Encode this 409 in your own test file with a body that differs ONLY on ({cols_human}).\n\n",
                    name = e.name,
                ));
                continue;
            }
            let refs: Vec<(&str, &str)> = overrides
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect();
            let dup = request_expr(design, unit, creator, &url, guarded, &refs);
            out.code.push_str(&format!(
                "/// #115 composite UNIQUE({cols_human}) on {name}: a second row sharing the\n/// group's column values — every OTHER `unique` column and the pk bumped to a\n/// DISTINCT value — must 409 ONLY through this composite index, proving it enforces\n/// \"one row per ({cols_human})\" (a conflict, not a race).\n#[tokio::test]\nasync fn {snake}_composite_unique_{ordinal}_is_409() {{\n    let t = app().await;\n{seed}    let first = {first};\n    assert_eq!(first.status().as_u16(), {status}, \"setup: the first {snake} creates ({cols_human}); body: {{}}\", first.text());\n    let dup = {dup};\n    assert_eq!(dup.status().as_u16(), 409, \"design: a duplicate ({cols_human}) must 409 (composite UNIQUE index); body: {{}}\", dup.text());\n}}\n\n",
                name = e.name,
            ));
            out.count += 1;
        }
    }
}

/// Whether [`distinct_fixture_value`] can yield a value DIFFERENT from
/// [`fixture_value`] for `f`: every shape can, except a single-member enum — which
/// JC0552's cardinality floor already forbids for a `unique` enum. An un-bumpable
/// competing field is thus excluded from the bump set, forcing an isolation skip
/// rather than a silent false green.
fn can_bump_distinct(f: &Field) -> bool {
    f.values.as_ref().is_none_or(|v| v.len() >= 2)
}

/// A valid JSON body literal for `f` GUARANTEED DISTINCT from [`fixture_value`] —
/// used to bump a competing `unique`/pk column in the composite-unique dup body
/// (#115) so ONLY the composite index can 409. Mirrors `fixture_value`'s constraint
/// branches so the bumped value still clears the deserialize validator and any CHECK.
fn distinct_fixture_value(f: &Field) -> String {
    if let Some(values) = f.values.as_ref() {
        // The SECOND declared enum member — valid and distinct (a `unique` enum has
        // >= 3 members per JC0552; a pk is never an enum). `can_bump_distinct` has
        // already excluded a degenerate single-member enum from the bump set.
        if let Some(second) = values.get(1) {
            return format!("\"{second}\"");
        }
        if let Some(first) = values.first() {
            return format!("\"{first}\"");
        }
    }
    if has_int_range(f) {
        return int_literal(kth_in_range(f, 1));
    }
    if has_len_range(f) {
        return format!("\"{}\"", constrained_seed_string_n(f, 2));
    }
    match f.field_type {
        FieldType::String => "\"test-value-2\"".to_string(),
        FieldType::Integer => "2".to_string(),
        FieldType::Float => "2.0".to_string(),
        FieldType::Boolean => "true".to_string(),
        FieldType::Datetime => "\"2026-01-02T00:00:00Z\"".to_string(),
        // A DIFFERENT valid v4 (distinct from fixture_value's f47ac10b-… v4).
        FieldType::Uuid => "\"3fa85f64-5717-4562-b3fc-2c963f66afa6\"".to_string(),
        FieldType::Json => "{\"k\": 1}".to_string(),
    }
}

/// A `{op}_without_auth_is_401` test: the guard extractor runs first, so a
/// credential-less request is rejected before any handler logic — no seed needed.
fn push_401_test(
    design: &Design,
    out: &mut TestOut,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    _seeded: bool,
) {
    let fn_base = &ep.operation_id;
    let request = request_expr(design, unit, ep, path, false, &[]); // no cookie
    out.code.push_str(&format!(
        "#[tokio::test]\nasync fn {fn_base}_without_auth_is_401() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), 401, \"design: {fn_base} is guarded — no cookie must 401; body: {{}}\", res.text());\n}}\n\n"
    ));
    out.count += 1;
}

/// An enum "reject" probe (issue #47): sends the endpoint's fixture body with ONE
/// enum field corrupted to an out-of-range value, and asserts the request boundary
/// answers 422 (JC0422) — the generated `deserialize_with` validator refuses it at
/// deserialization, before the handler and the DB. It PASSES on stubs (the 422
/// precedes the stub), so it is NOT part of the RED-on-stubs baseline: `out.reject`
/// tracks it so gen-tests can exclude it from `expected_failing`. Guarded endpoints
/// thread the credential (via `request_expr`) so the guard doesn't 401 first.
#[allow(clippy::too_many_arguments)]
fn push_enum_reject_test(
    design: &Design,
    out: &mut TestOut,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded: bool,
    field: &str,
    // #267: a membership seed to PREPEND (or `""`). On a path-scoped tenant route
    // whose module doesn't pre-seed, the `Dep<Tenant>` guard 404s a non-member
    // BEFORE deserialization, so the reject probe never reaches the 422 validator;
    // seeding the caller's membership (the SAME seed the 2xx probe uses) lets it.
    seed: &str,
) {
    let fn_base = &ep.operation_id;
    let sentinel = format!("\"{ENUM_REJECT_SENTINEL}\"");
    let request = request_expr(design, unit, ep, path, guarded, &[(field, &sentinel)]);
    out.code.push_str(&format!(
        "#[tokio::test]\nasync fn {fn_base}_rejects_out_of_range_{field}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), 422, \"design: out-of-range `{field}` enum must 422 at the request boundary, not 500 at the DB CHECK; body: {{}}\", res.text());\n}}\n\n"
    ));
    out.count += 1;
    out.reject += 1;
}

/// The #80 constraint twin of [`push_enum_reject_test`]: sends the endpoint's
/// fixture body with ONE constrained field set to an out-of-range literal and
/// asserts the request boundary answers 422 (JC0422) — the generated
/// `deserialize_with` validator refuses it before the handler and the DB
/// CHECK. Like the enum probe it PASSES on stubs, so it increments
/// `out.reject` and is excluded from the RED-on-stubs `expected_failing`
/// baseline.
#[allow(clippy::too_many_arguments)]
fn push_constraint_reject_test(
    design: &Design,
    out: &mut TestOut,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded: bool,
    field: &str,
    literal: &str,
    // #267: see `push_enum_reject_test` — the tenant-membership seed to PREPEND
    // (or `""` for a non-tenant / pre-seeded route, keeping it byte-identical).
    seed: &str,
) {
    let fn_base = &ep.operation_id;
    let request = request_expr(design, unit, ep, path, guarded, &[(field, literal)]);
    out.code.push_str(&format!(
        "#[tokio::test]\nasync fn {fn_base}_rejects_out_of_range_{field}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), 422, \"design: out-of-range `{field}` must 422 at the request boundary (the declared min/max/min_len/max_len), not 500 at the DB CHECK; body: {{}}\", res.text());\n}}\n\n"
    ));
    out.count += 1;
    out.reject += 1;
}

/// Issue #217: the inline-DTO twin of the entity reject probes. An inline-DTO
/// custom action (issue #122) validates its `{Op}Request` fields with the SAME
/// #80/#47 machinery as an entity body — but the entity reject probes key on
/// `rb.entity`, so an inline body (`rb.entity == None`) got a happy-path test yet
/// NO boundary reject, leaving a declared inline constraint UNVERIFIED by `check`.
/// This mirrors the entity-body path exactly (`testgen.rs` create/update sites):
/// it emits an ENUM reject (first inline field with `values` && no `default`) AND,
/// INDEPENDENTLY, a #80 CONSTRAINT reject (first inline field with a derivable
/// out-of-range literal) — issue #225 Gap A retired the old `if constraint … else if
/// enum` XOR that dropped one whenever the other existed. It reuses
/// `push_enum_reject_test`/`push_constraint_reject_test`, so each corrupts exactly
/// its field (`request_expr` threads the override into `inline_fixture_json`),
/// threads the credential when guarded, asserts 422, and counts toward `out.reject`
/// (the 422 precedes the stub). Emits NOTHING when no inline field is rejectable —
/// byte-identical for unconstrained inline designs and for every entity-body /
/// bodyless endpoint (`rb` not inline).
///
/// The reject field may be REQUIRED or OPTIONAL (issue #225 Gap B): the gate is only
/// `default.is_none()`, matching the entity helpers' `first_enum_field` /
/// `first_constraint_reject`. `inline_fixture_json` now carries an overridden optional
/// field on the wire, so the corrupted value is present for the validator to reject.
/// (An enum string field yields no constraint literal and an integer/length field has
/// no `values`, so the two probes never target the same field — no duplicate fn name.)
fn push_inline_reject_test(
    design: &Design,
    out: &mut TestOut,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded: bool,
    // #267: the tenant-membership seed threaded down to the entity-reject helpers
    // (or `""`), so an inline-DTO action on a path-scoped tenant route reaches its
    // 422 validator instead of the guard's 404. Empty for non-tenant designs.
    seed: &str,
) {
    let Some(rb) = ep.request_body.as_ref().filter(|rb| rb.is_inline()) else {
        return;
    };
    // Enum reject (issue #47): first inline field with an allow-list, no default.
    if let Some(field) = rb
        .fields
        .iter()
        .find_map(|f| (f.values.is_some() && f.default.is_none()).then_some(f.name.as_str()))
    {
        push_enum_reject_test(design, out, unit, ep, path, guarded, field, seed);
    }
    // Constraint reject (issue #80): first inline field with a rejectable bound, no
    // default. `constraint_reject_literal` returns None for an enum/vacuous field, so
    // this never re-probes the enum field above.
    if let Some((field, literal)) = rb.fields.iter().find_map(|f| {
        f.default
            .is_none()
            .then(|| constraint_reject_literal(f).map(|lit| (f.name.as_str(), lit)))
            .flatten()
    }) {
        push_constraint_reject_test(design, out, unit, ep, path, guarded, field, &literal, seed);
    }
}

/// "{id}" as it appears inside the full path (the literal brace token).
fn regex_free_param(path: &str) -> String {
    let start = path.find('{').expect("parameterized path");
    let end = path[start..].find('}').expect("balanced braces") + start;
    path[start..=end].to_string()
}

/// The fixed dev secret the test app and `test_cookie()` share so the minted
/// session cookie decrypts against the app's `Auth` extension.
const TEST_SECRET: &str = "a-very-long-development-secret-string!!";

/// In auth mode: a test-only login shim that mints the guard credential directly
/// via the `Auth` extension (no app `/login` route needed), plus the
/// `.extend(Auth)` the app() helper adds so the SAME secret validates it.
/// `test_cookie_for` mints for any user id (isolation tests act as a second
/// user); `test_cookie()` keeps minting user 1's for back-compat.
///
/// The credential shape follows the auth model (issue #29): the `session` model
/// mints a `jerrycan_session=` cookie via the session store; the `jwt` model
/// mints a signed `Bearer <jwt>` over the SAME `SessionUser` payload with
/// `Auth::jwt_key()`, matching the generated `Bearer<SessionUser>` guard. The
/// helpers keep the `test_cookie` names in both models so the isolation seed and
/// probes stay untouched and the session/none output stays byte-identical.
///
/// No-`exp` (issue #45): the jwt token is minted deliberately WITHOUT an `exp`
/// claim. These are test-only, in-process credentials and must NOT be
/// time-dependent — a "helpfully" added `exp` would make the isolation tests expire
/// and flake. Keep it exp-free.
fn auth_preamble_login(design: &Design) -> String {
    // The minted `SessionUser.role` is drawn from the design (issue #67): a
    // `require_role`-guarded handler 403s a credential whose role doesn't satisfy
    // the gate, so a hardcoded "admin" left role-gated probes un-greenable for any
    // design whose roles exclude it. `test_credential_role` picks the gate's role.
    let role = design.test_credential_role();
    let mint = if design.auth_model() == AuthModel::Jwt {
        format!(
            "let token = jerrycan::auth::jwt::encode(&shared::SessionUser {{ id: user_id.to_string(), role: \"{role}\".into() }}, auth.jwt_key()).expect(\"encode\");\n    format!(\"Bearer {{token}}\")"
        )
    } else {
        format!(
            "let token = auth.sessions().encode(&shared::SessionUser {{ id: user_id.to_string(), role: \"{role}\".into() }}).expect(\"encode\");\n    format!(\"jerrycan_session={{token}}\")"
        )
    };
    format!(
        "fn test_cookie_for(user_id: i64) -> String {{\n    let auth = jerrycan::auth::Auth::with_secret(\"{TEST_SECRET}\");\n    {mint}\n}}\n\nfn test_cookie() -> String {{\n    test_cookie_for(1)\n}}\n\n"
    )
}

/// The module owning the design's tenancy entity (the `{tenant}_members` table
/// lives in its migration). None when the design has no tenancy.
fn tenant_module(design: &Design) -> Option<&ModuleDesign> {
    let tenancy = design.tenancy.as_ref()?;
    design
        .modules
        .iter()
        .find(|m| m.entities.iter().any(|e| e.name == tenancy.entity))
}

/// True when this module holds an entity that belongs_to the tenancy entity —
/// so its guarded handlers take `Dep<Tenant>` and the test app must register the
/// `tenant` factory + seed a membership row.
fn module_needs_tenant(design: &Design, module: &ModuleDesign) -> bool {
    if design.tenancy.is_none() {
        return false;
    }
    // Ownership is TRANSITIVE (issue #102): a grandchild (`Contact belongs_to
    // Account belongs_to Org`) is tenant-owned too, so its module also needs the
    // tenant/membership seed + second-tenant scaffolding its isolation test acts
    // on. `tenant_path(..).is_some()` subsumes the old direct-`belongs_to` check
    // (a direct child resolves to an empty-`joins` path), so direct designs stay
    // byte-identical; only grandchild modules gain the scaffolding.
    fn walk(design: &Design, m: &ModuleDesign) -> bool {
        m.entities
            .iter()
            .any(|e| design.tenant_path(&e.name).is_some())
            || m.subroutes.iter().any(|s| walk(design, s))
    }
    walk(design, module)
}

/// True when this module's test app must REGISTER the `tenant` DI factory so a
/// `Dep<Tenant>` handler resolves. This is broader than [`module_needs_tenant`]:
/// besides a tenant-owned child (whose guarded handlers take `Dep<Tenant>`), the
/// tenant module ITSELF needs the factory when its own detail route is a GUARDED
/// path-scoped route (normalized `/{tenant_fk}`, issue #78) — its `get`/`delete`
/// handler takes `Dep<Tenant>`. Kept SEPARATE from the membership-SEED gate
/// (`module_needs_tenant`): the tenant module creates its own rows in-test, so it
/// must not be pre-seeded (that would collide with the created id). An UNGUARDED
/// detail route (no `Dep<Tenant>`) is excluded, so a design whose tenant module
/// exposes only public reads stays byte-identical.
fn module_provides_tenant_dep(design: &Design, module: &ModuleDesign) -> bool {
    fn has_guarded_pathscoped(design: &Design, m: &ModuleDesign) -> bool {
        m.endpoints.iter().any(|ep| {
            ep.is_guarded()
                && matches!(
                    design.endpoint_tenant_shape(m, ep),
                    TenantShape::PathScoped { .. }
                )
        }) || m
            .subroutes
            .iter()
            .any(|s| has_guarded_pathscoped(design, s))
    }
    module_needs_tenant(design, module) || has_guarded_pathscoped(design, module)
}

/// A `migrate` entry for the tenant module's tables, referenced from THIS test
/// crate (cross-crate relative include) so the `{tenant}_members` table the
/// `tenant` guard queries exists. Empty if the tenant module IS this module
/// (its own migration is already included) or there is no tenancy.
/// Every module's create-tables migration for the FULL workspace schema, so a
/// module's TestApp can touch ANY module's table (issue #14): a handler that
/// legitimately writes another module's table no longer 500s with "no such
/// table". The CURRENT module includes its own files by the relative
/// `../migrations/...` path; every OTHER module by the cross-crate
/// `../../{module}/migrations/...` path (the same shape the old tenant
/// cross-include used). sqlite-memory schema is cheap, so migrating everything
/// is the simplest correct default. Deterministic: document order, skipping
/// entity-less modules (which have no migration file).
fn collect_workspace_migration_items(
    design: &Design,
    current: &ModuleDesign,
) -> Vec<MigrationItem> {
    // The current module reaches its own files by `..` (from its own tests dir);
    // every other module by the cross-crate `../../{module}` path.
    collect_migration_items(design, |name| {
        if name == current.name {
            "..".to_string()
        } else {
            format!("../../{name}")
        }
    })
}

/// Emit a `jerrycan::db::Migration { … include_str!(…) }` item for every route
/// module (and subroute) create-tables migration in the design, into `out` (design
/// order; entity-less modules skipped — they have no migration file). This is the
/// FULL workspace schema `App::build` applies (mounting.rs aggregates the same set
/// into `migrations::MIGRATIONS`). `prefix_for(module_name)` yields the
/// `include_str!` path prefix to that module's `migrations/` dir — it differs by
/// caller because their harness files sit at different depths: the route TestApp
/// (testgen) is at `crates/routes/<m>/tests/` (own module `..`, others `../../<m>`),
/// while the jobs harness (jobsgen) is at `crates/jobs/tests/` (every module
/// `../../routes/<m>`). Shared so both harnesses migrate the same tables (issue #84).
/// One `{field}: include_str!("{path}"),` line at column `indent`, pre-wrapped exactly
/// as the pinned rustfmt does (issue #218): once the single-line form exceeds
/// `max_width` (100), rustfmt breaks the `include_str!` string arg onto its own line at
/// column `indent + 4`, closing `),` back at column `indent`. A shorter line stays
/// as-is (byte-identical for short module paths). `indent` is 12 for the MULTI-line
/// migration array (item body one level below the `jerrycan::db::Migration {`) and 8
/// for rustfmt's single-element HUG, where the sole struct de-indents one level (#221).
fn migration_include_line(indent: usize, field: &str, path: &str) -> String {
    let pad = " ".repeat(indent);
    let one_line = format!("{pad}{field}: include_str!(\"{path}\"),");
    if one_line.chars().count() <= 100 {
        format!("{one_line}\n")
    } else {
        let pad4 = " ".repeat(indent + 4);
        format!("{pad}{field}: include_str!(\n{pad4}\"{path}\"\n{pad}),\n")
    }
}

/// One workspace create-tables migration: the migration NAME literal and the two
/// per-backend `include_str!` paths. Collected structurally (issue #221) so the
/// `db.migrate(&[…])` array can be rendered either MULTI-line (≥ 2 items) or as
/// rustfmt's single-element HUG (exactly 1 item), which de-indents the sole struct one
/// level. The earlier string-only collector always emitted the multi-line form, so a
/// SINGLE-route-module design drifted under `cargo fmt`.
pub(crate) struct MigrationItem {
    name: String,
    sqlite_path: String,
    postgres_path: String,
}

/// Collect every route module (and subroute) create-tables migration in the design, in
/// document order (entity-less modules skipped — they have no migration file). This is
/// the FULL workspace schema `App::build` applies. `prefix_for(module_name)` yields the
/// `include_str!` path prefix to that module's `migrations/` dir — it differs by caller
/// because their harness files sit at different depths.
pub(crate) fn collect_migration_items(
    design: &Design,
    prefix_for: impl Fn(&str) -> String,
) -> Vec<MigrationItem> {
    let mut items = Vec::new();
    for m in &design.modules {
        let prefix = prefix_for(&m.name);
        let m_snake = m.name.replace('-', "_");
        if !m.entities.is_empty() {
            items.push(MigrationItem {
                name: format!("{m_snake}_0001_create_tables"),
                sqlite_path: format!("{prefix}/migrations/sqlite/0001_create_tables.sql"),
                postgres_path: format!("{prefix}/migrations/postgres/0001_create_tables.sql"),
            });
        }
        collect_subroute_migration_items(m, &m_snake, &prefix, &mut items);
    }
    items
}

/// Subroute create-tables migrations for one top-level module (recursive). A
/// subroute's file lives in its TOP module's migrations dir as
/// `0001_create_tables_{sub}.sql`; its name is namespaced by the top module so
/// two modules' like-named subroutes never collide in the workspace list.
fn collect_subroute_migration_items(
    module: &ModuleDesign,
    top_snake: &str,
    prefix: &str,
    items: &mut Vec<MigrationItem>,
) {
    for sub in &module.subroutes {
        if !sub.entities.is_empty() {
            let s = sub.name.replace('-', "_");
            items.push(MigrationItem {
                name: format!("{top_snake}_0001_create_tables_{s}"),
                sqlite_path: format!("{prefix}/migrations/sqlite/0001_create_tables_{s}.sql"),
                postgres_path: format!("{prefix}/migrations/postgres/0001_create_tables_{s}.sql"),
            });
        }
        collect_subroute_migration_items(sub, top_snake, prefix, items);
    }
}

/// Render the `db.migrate(&[ … ]).await.expect("{expect_msg}");` block (at fn-body
/// indent 4) EXACTLY as the pinned rustfmt formats it (issue #221). rustfmt HUGS a
/// single-element array — the sole `jerrycan::db::Migration { … }` opens on the
/// `db.migrate(&[` line and its body de-indents one level (fields at column 8, include
/// paths width-checked at column 8) — but keeps a ≥ 2-element array multi-line (each
/// item at column 8, fields at column 12). Empty `items` yields the empty string (the
/// jobs harness omits the route-migrate call when there are no route tables). Byte-
/// identical to the previous multi-line output whenever `items.len() >= 2`.
pub(crate) fn migrate_call_block(items: &[MigrationItem], expect_msg: &str) -> String {
    let array = match items {
        [] => return String::new(),
        [only] => {
            // Single-element HUG: struct body at column 8; `db.migrate(&[STRUCT])`.
            let sqlite = migration_include_line(8, "sqlite", &only.sqlite_path);
            let postgres = migration_include_line(8, "postgres", &only.postgres_path);
            format!(
                "&[jerrycan::db::Migration {{\n        name: \"{}\",\n{sqlite}{postgres}    }}]",
                only.name
            )
        }
        many => {
            // Multi-line: item body at column 8, fields at column 12 (pre-#221 layout).
            let body: String = many
                .iter()
                .map(|it| {
                    let sqlite = migration_include_line(12, "sqlite", &it.sqlite_path);
                    let postgres = migration_include_line(12, "postgres", &it.postgres_path);
                    format!(
                        "        jerrycan::db::Migration {{\n            name: \"{}\",\n{sqlite}{postgres}        }},\n",
                        it.name
                    )
                })
                .collect();
            format!("&[\n{body}    ]")
        }
    };
    format!("    db.migrate({array})\n    .await\n    .expect(\"{expect_msg}\");\n")
}

/// The seed statements that put the test user (id 1) into a tenant: insert one
/// tenant row (id 1, required fields = fixtures, enum fields = first allowed
/// value so CHECKs pass) then one membership row (user_id 1, fk 1, first member
/// role). Run on the raw connection before `.into_test()` so the `tenant` guard
/// resolves a membership for every guarded request. Empty when not needed.
fn tenant_seed(design: &Design, module: &ModuleDesign) -> String {
    if !module_needs_tenant(design, module) {
        return String::new();
    }
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    let Some(t) = tenant_module(design) else {
        return String::new();
    };
    let Some(entity) = t.entities.iter().find(|e| e.name == tenancy.entity) else {
        return String::new();
    };
    let table = design.table_name(&tenancy.entity);
    let members = format!("{}_members", Design::to_snake(&tenancy.entity));
    let fk = Design::fk_column(&tenancy.entity);
    // The seed role is member_roles[0]; JC0548 guarantees a non-empty list at
    // design time, so the fallback is dead code — `"member"` to match genroute's
    // (equally dead) seed-role fallback byte-for-byte.
    let role = tenancy
        .member_roles
        .first()
        .map(String::as_str)
        .unwrap_or("member");

    // Columns + values for the tenant row: id = 1 (so the fk resolves), then each
    // declared non-id field with a seed-safe fixture (enum fields use a declared
    // value to satisfy the CHECK constraint). Column identifiers are double-quoted
    // in the SQL; since the whole statement is a Rust string literal, those quotes
    // are escaped (`\\\"`) so the generated source stays valid.
    let (cols, vals) = tenant_row_cols_vals(entity, "1", 1);
    let mut seed = format!(
        "    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols}) VALUES ({vals})\")\n        .await\n        .expect(\"seed tenant row\");\n    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (1, 1, '{role}')\")\n        .await\n        .expect(\"seed membership\");\n"
    );
    // Seed the TRANSITIVE parent chain in tenant 1 (issue #102): a grandchild's
    // create resolves its parent fk through the JOIN chain, so each intermediate
    // parent (from the anchor down to the immediate parent) must exist and be
    // linked to tenant 1. Empty for a direct child (no joins) — byte-identical.
    if let Some(path) = module
        .entities
        .iter()
        .find_map(|e| design.tenant_path(&e.name))
    {
        seed.push_str(&seed_tenant1_chain(design, &path));
    }
    seed
}

/// Seed the transitive parent chain in tenant 1 (issue #102) so a grandchild's
/// create resolves its parent fk through the JOIN chain. Emits one INSERT per
/// intermediate parent, ANCHOR FIRST (parents before children): the anchor (the
/// table that directly `belongs_to` the tenant) carries the tenant fk = 1; each
/// lower parent carries its own parent fk = 1 (the id of the row just seeded).
/// Every table is seeded at the fixed id 1. Required (NOT NULL) non-id fields are
/// seeded with a type-shaped literal so a parent with e.g. a required `name` still
/// inserts; nullable fields are omitted. Empty for a direct child (no joins).
fn seed_tenant1_chain(design: &Design, path: &TenantPath) -> String {
    let n = path.joins.len();
    let mut out = String::new();
    for i in (0..n).rev() {
        let table = &path.joins[i].parent_table;
        // The fk linking this parent upward: the anchor (top join) carries the
        // tenant fk; a lower parent carries its fk to ITS parent — the next join's
        // child fk — whose row we also seed at id 1.
        let link_fk = if i == n - 1 {
            path.tenant_fk.as_str()
        } else {
            path.joins[i + 1].child_fk.as_str()
        };
        let mut cols = vec!["id".to_string(), link_fk.to_string()];
        let mut vals = vec!["1".to_string(), "1".to_string()];
        // A parent may declare its own required columns (NOT NULL, no DB default);
        // seed them too so the INSERT satisfies the schema. Nullable columns are
        // left NULL. `id` is the fixed 1 above; the upward fk is `link_fk`.
        if let Some(e) = entity_by_table(design, table) {
            for f in e.fields.iter().filter(|f| f.name != "id" && f.required) {
                cols.push(format!("\\\"{}\\\"", f.name));
                vals.push(seed_sql_value(f));
            }
        }
        out.push_str(&format!(
            "    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols}) VALUES ({vals})\")\n        .await\n        .expect(\"seed tenant 1 {table} row\");\n",
            cols = cols.join(", "),
            vals = vals.join(", "),
        ));
    }
    out
}

/// The entity whose table is `table` — the reverse of `Design::table_name`. Table
/// names are unique per entity, so this resolves the parent `Entity` a
/// `TenantPath` join references (the join stores only table names), needed to seed
/// that parent's required columns.
fn entity_by_table<'a>(design: &'a Design, table: &str) -> Option<&'a Entity> {
    fn collect<'a>(m: &'a ModuleDesign, out: &mut Vec<&'a Entity>) {
        out.extend(m.entities.iter());
        for s in &m.subroutes {
            collect(s, out);
        }
    }
    let mut all = Vec::new();
    for m in &design.modules {
        collect(m, &mut all);
    }
    all.into_iter()
        .find(|e| design.table_name(&e.name) == table)
}

/// The membership role to seed for the second tenant's user: the role a
/// role-gated DELETE on this module requires (so the isolation DELETE leg clears
/// the role check and exercises the SCOPED `remove_for` — proving cross-tenant
/// isolation, not a 403 role rejection), falling back to the first member role.
/// The final fallback is dead code under JC0548 (member_roles is non-empty at
/// design time) — `"member"` to match genroute's equally dead seed-role fallback.
fn isolation_member_role<'a>(design: &'a Design, module: &'a ModuleDesign) -> &'a str {
    module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::DELETE && !ep.required_roles.is_empty())
        .and_then(|ep| ep.required_roles.first())
        .map(String::as_str)
        .or_else(|| {
            design
                .tenancy
                .as_ref()
                .and_then(|t| t.member_roles.first())
                .map(String::as_str)
        })
        .unwrap_or("member")
}

/// The columns + values for a tenant row seeded at the given pk: id = the pk
/// literal, then each declared non-id field with a seed-safe fixture (enum
/// fields use a declared value to satisfy the CHECK). Returns (cols, vals) as
/// the comma-joined SQL fragments. The tenant pk is an integer in practice (the
/// reference-slice Workspace), so the literal is numeric.
pub(crate) fn tenant_row_cols_vals(entity: &Entity, pk: &str, n: u32) -> (String, String) {
    let mut cols = vec!["id".to_string()];
    let mut vals = vec![pk.to_string()];
    for f in entity.fields.iter().filter(|f| f.name != "id") {
        cols.push(format!("\\\"{}\\\"", f.name));
        vals.push(seed_sql_value_n(f, n));
    }
    (cols.join(", "), vals.join(", "))
}

/// The `seed_second_tenant` helper for a tenant-owned module: inserts a SECOND
/// tenant (id 2) and a membership for user 2 (fk 2, role from
/// `isolation_member_role`). The isolation test acts as this user to prove a
/// tenant cannot reach another tenant's rows. Empty for non-tenant-owned modules.
fn seed_second_tenant_fn(design: &Design, module: &ModuleDesign) -> String {
    if !module_needs_tenant(design, module) {
        return String::new();
    }
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    let Some(t) = tenant_module(design) else {
        return String::new();
    };
    let Some(entity) = t.entities.iter().find(|e| e.name == tenancy.entity) else {
        return String::new();
    };
    let table = design.table_name(&tenancy.entity);
    let members = format!("{}_members", Design::to_snake(&tenancy.entity));
    let fk = Design::fk_column(&tenancy.entity);
    let role = isolation_member_role(design, module);
    let (cols, vals) = tenant_row_cols_vals(entity, "2", 2);
    format!(
        "async fn seed_second_tenant(db: &jerrycan::db::Db) {{\n    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols}) VALUES ({vals})\")\n        .await\n        .expect(\"seed tenant 2 row\");\n    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (2, 2, '{role}')\")\n        .await\n        .expect(\"seed tenant 2 membership\");\n}}\n\n"
    )
}

/// The cross-tenant isolation test for a tenant-owned module: user 1 (tenant 1)
/// creates a row; user 2 (tenant 2, seeded by app()) must not be able to read,
/// list, or delete it. WHY this matters (Rule 9): it encodes the SECURITY
/// contract — it fails on stubs (500), goes green only when the handler uses the
/// SCOPED accessors (get_for/all_for/remove_for), and stays RED if the agent
/// reaches for the unscoped all/get/remove (which would leak the foreign row).
///
/// Emitted only for a top-level tenant-owned entity that has a guarded creator
/// (POST "/" with a body). With a GET "/{id}" it runs the full get/list/delete
/// legs; without one it degrades to a list-only variant asserting the foreign row
/// is absent from user 2's list. Empty when there's no usable creator.
/// Every applicable isolation test for a module, concatenated (issue #78/#79,
/// spec §F). One design shape ⇒ one emitter fires; each returns "" when N/A, so
/// composing is safe. The shapes:
///   - tenant-owned (flat MembershipSet, and nested path-scoped) — a member of
///     tenant A cannot reach tenant B's rows;
///   - per-user identity-owned — user B cannot reach user A's rows (#79);
///   - tenant-collection-create (I1) — the creator's list-own returns the new
///     tenant; a second user's list is empty (the backstop for an agent who calls
///     the bare `insert` instead of `create_with_membership`);
///   - tenant-root detail (#172) — a non-member gets 404 on the tenant root's own
///     `GET /{id}` detail route (the collection test covers only the root's list).
fn isolation_test(design: &Design, module: &ModuleDesign) -> String {
    let mut out = String::new();
    out.push_str(&tenant_owned_isolation_test(design, module));
    out.push_str(&flat_write_isolation_test(design, module));
    out.push_str(&per_user_isolation_test(design, module));
    out.push_str(&public_read_isolation_test(design, module));
    out.push_str(&tenant_collection_isolation_test(design, module));
    out.push_str(&tenant_root_detail_isolation_test(design, module));
    out
}

/// The cross-tenant isolation test for a tenant-owned module: user 1 (tenant 1)
/// creates a row; user 2 (tenant 2, seeded by app()) must not be able to read,
/// list, or delete it. WHY this matters (Rule 9): it encodes the SECURITY
/// contract — it fails on stubs (500), goes green only when the handler uses the
/// SCOPED accessors (get_for/all_for/remove_for), and stays RED if the agent
/// reaches for the unscoped all/get/remove (which would leak the foreign row).
///
/// Handles BOTH route shapes:
///   - FLAT (MembershipSet, `/leads`): user 2 can list their own (empty) rows, so
///     the list leg asserts the foreign row is absent — byte-identical to before;
///   - NESTED path-scoped (`/clubs/{club_id}/books`, the #78 leak with no coverage
///     today): the tenant fk in the mount is pinned to tenant 1, and user 2 (a
///     member of tenant 2, NOT tenant 1) gets 404 on tenant 1's row. The list leg
///     asserts the SAME 404 (issue #240): the `Dep<Tenant>` path guard denies the
///     pinned collection to a non-member before the list handler runs, so user 2
///     can't even enumerate tenant 1's rows — a "list 200, absent" assertion (the
///     FLAT contract) would false-fail. A module with NO readable endpoint (create-
///     only / create+update-only) has nothing to probe, so it gets NO test rather
///     than a setup-only body that binds `row`/`cookie2` and asserts nothing.
fn tenant_owned_isolation_test(design: &Design, module: &ModuleDesign) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    // The tenant-owned entity on this module — directly OR TRANSITIVELY (issue
    // #102). `tenant_path` resolves the unique `belongs_to` chain that reaches the
    // tenant: a DIRECT child yields an empty-`joins` path (byte-identical to the
    // old direct-`belongs_to` finder); a GRANDCHILD (`Contact belongs_to Account
    // belongs_to Org`) yields the JOIN chain we seed (`seed_tenant1_chain`) and
    // pin into the mount below. (Subroute-nested tenant entities remain out of
    // scope — only top-level module entities, as before.)
    let Some((entity, path)) = module
        .entities
        .iter()
        .find_map(|e| design.tenant_path(&e.name).map(|p| (e, p)))
    else {
        return String::new();
    };
    // A guarded creator at "/" with a body for this entity is required to seed a
    // tenant-1 row to probe; without it there's nothing to isolate.
    let Some(create) = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == "/"
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity.name.as_str()))
    }) else {
        return String::new();
    };
    let base = module.effective_mount();
    let base = base.trim_end_matches('/');
    // The `fk_token`/`parent_tokens` still select the list-leg SHAPE: a NESTED mount
    // carries the TENANT fk (`/clubs/{club_id}/…`, a DIRECT child) or a PARENT fk
    // (`/accounts/{account_id}/…`, a GRANDCHILD — issue #102), and being nested means
    // user 2 can't reach tenant 1's collection so the flat list leg is skipped. The
    // probe URLs, though, pin EVERY `{param}` to the seeded id 1 via
    // `concrete_mount_base` (the same helper the per-endpoint tests use), not just the
    // fk/parent tokens: a mount whose param name differs from the canonical fk
    // (`/happenings/{org_id}` for tenancy `Organization` — issue #245) would otherwise
    // leave a literal `{org_id}` in the isolation URL that `Path<i64>` can't parse. For
    // a canonical-fk / grandchild / flat mount the result is byte-identical (those
    // tokens were already the only params, and were already pinned to 1).
    let fk_token = format!("{{{}}}", Design::fk_column(&tenancy.entity));
    let parent_tokens: Vec<String> = path
        .joins
        .iter()
        .map(|j| format!("{{{}}}", j.child_fk))
        .collect();
    let is_nested = base.contains(&fk_token) || parent_tokens.iter().any(|t| base.contains(t));
    let cbase = concrete_mount_base(base);
    let plural = module.name.replace('-', "_");
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        &[],
        false, // isolation seeds a row via create — a create body omits defaults
    );
    let create_path = format!("{cbase}/");

    // A GET "/{id}" lets us assert the foreign row 404s for user 2 and survives
    // for user 1; a DELETE "/{id}" (role-gated → user 2's membership carries the
    // role) must also 404 without destroying user 1's row. Computed first so the
    // id bindings below are only emitted when a probe consumes them (never an
    // unused-variable warning under -D warnings).
    let get_one = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::GET && param_count(ep) == 1);
    let delete_one = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::DELETE && param_count(ep) == 1);
    // The collection LIST endpoint, if any — its cross-tenant probe differs by shape:
    //   - FLAT (MembershipSet): user 2 lists their OWN (empty) collection — 200 with
    //     the foreign row absent (`flat_list`, byte-identical to before).
    //   - NESTED on the tenant fk (`/orgs/{org_id}/…`): user 2, a non-member of
    //     tenant 1, is 404'd at the `Dep<Tenant>` path guard before the list runs
    //     (`nested_list`) — previously ZERO coverage (issue #240). Scoped to a mount
    //     carrying the TENANT fk itself: a GRANDCHILD mount pins only a PARENT fk
    //     (`/accounts/{account_id}/…`, #102), which the guard does NOT key on (it
    //     falls back to the caller's own membership — no clean path 404), so its list
    //     stays uncovered here and the repo's scoped `all_for` is its backstop.
    let list_ep = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::GET && param_count(ep) == 0);
    let flat_list = list_ep.filter(|_| !is_nested);
    let nested_list = list_ep.filter(|_| base.contains(&fk_token));

    // A module with NO readable endpoint (create-only, or create + update-only) has
    // nothing to isolation-probe — no way to READ another tenant's row — so emit NO
    // test rather than a setup-only body that binds `row`/`cookie2` and asserts
    // nothing (issue #240; that body also tripped `-D warnings` on the unused
    // bindings). The write's own success + 401 probes still cover it, and the
    // `Dep<Tenant>` guard / scoped repo is the runtime enforcement.
    if get_one.is_none() && delete_one.is_none() && flat_list.is_none() && nested_list.is_none() {
        return String::new();
    }

    // The credential header follows the auth model (cookie/session, Bearer/jwt —
    // issue #29); `test_cookie_for(n)` returns the matching header value.
    let hk = design.test_auth_header();
    // user 1 (cred 1) creates a row in tenant 1, then we read the id it echoes.
    let mut t = String::new();
    t.push_str(&format!(
        "/// SECURITY: a tenant must not reach another tenant's {entity} rows. User 1\n/// creates a row in tenant 1; user 2 (tenant 2) must be denied read/list/delete.\n/// Passes only with the SCOPED repo accessors (get_for/all_for/remove_for).\n#[tokio::test]\nasync fn tenant_a_cannot_read_tenant_b_{plural}() {{\n    let t = app().await;\n",
        entity = entity.name,
    ));
    t.push_str(&format!(
        "    let created = t.post_json_with(\"{create_path}\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n",
        status = create.success.status,
        entity = entity.name,
    ));
    // `row` (the parsed create response) is read ONLY to derive the by-id URL (`id`)
    // or the flat-list absence check (`id_value`); the nested-list 404 leg needs
    // neither, so bind it only when a leg below consumes it (no unused-var, #240).
    if flat_list.is_some() || get_one.is_some() || delete_one.is_some() {
        t.push_str(
            "    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n",
        );
    }
    // user 2's credential — every remaining probe leg reads it (the early return
    // above guarantees at least one leg exists here).
    t.push_str("    let cookie2 = test_cookie_for(2);\n");
    // The FLAT list negative-control compares the created id as a JSON Value.
    if flat_list.is_some() {
        t.push_str("    let id_value = row[\"id\"].clone();\n");
    }
    // A by-id URL must carry the RAW id: a string PK's `Value::String` Display
    // includes JSON quotes (`\"uuid\"`), so a `format!(\"…/{id}\")` would 404 every
    // by-id request. Interpolate the unquoted string (a numeric PK is identical).
    if get_one.is_some() || delete_one.is_some() {
        t.push_str(
            "    let id = row[\"id\"].as_str().map(str::to_string).unwrap_or_else(|| row[\"id\"].to_string());\n",
        );
    }

    if let Some(_get) = get_one {
        t.push_str(&format!(
            "    let foreign = t.get_with(&format!(\"{cbase}/{{id}}\"), &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-tenant get must 404 (use get_for, not get); body: {{}}\", foreign.text());\n",
        ));
    }
    if flat_list.is_some() {
        // Always cookied: even an unguarded list is safe to call with a cookie,
        // and a guarded one needs it. user 2 sees only tenant 2's (empty) rows.
        t.push_str(&format!(
            "    let listed = t.get_with(\"{cbase}/\", &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(listed.status().as_u16(), 200, \"user 2 lists their own {plural}; body: {{}}\", listed.text());\n    let rows: serde_json::Value = serde_json::from_str(&listed.text()).expect(\"list json\");\n    let absent = rows.as_array().map(|a| a.iter().all(|r| r[\"id\"] != id_value)).unwrap_or(true);\n    assert!(absent, \"cross-tenant list must NOT contain tenant 1's row (use all_for); body: {{}}\", listed.text());\n",
        ));
    }
    if nested_list.is_some() {
        // NESTED on the tenant fk: user 2 (a non-member of tenant 1) is 404'd at the
        // `Dep<Tenant>` path guard before the list handler runs — a non-member can't
        // even enumerate tenant 1's collection (issue #240; mirrors the nested
        // `GET /{id}` 404 leg). This closes the previously ZERO-coverage nested list.
        t.push_str(&format!(
            "    let listed = t.get_with(\"{cbase}/\", &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(listed.status().as_u16(), 404, \"cross-tenant list must 404 — a non-member can't reach tenant 1's collection (Dep<Tenant> denies the path); body: {{}}\", listed.text());\n",
        ));
    }
    if let Some(del) = delete_one {
        // A ROLE-GATED flat delete (issue #247) checks the caller's MEMBERSHIP role in
        // the ROW's tenant BEFORE the scoped remove: user 2 is a member of tenant 2,
        // NOT tenant 1, so `require_membership_role` 403s them. This 403 is the honest
        // membership-vs-session discriminator (Rule 9) — a WRONG session-role check
        // (`_user.0.role`) would let user 2's minted `owner` session role pass the gate
        // and only 404 at `remove_for_memberships`, so an implementation that checks the
        // session role instead of the membership role FAILS this assertion. A
        // non-role-gated flat delete 404s at the scoped remove (unchanged); a nested
        // role-gated delete 404s earlier at the `Dep<Tenant>` path guard (unchanged).
        let (code, why) = if !del.required_roles.is_empty() && !is_nested {
            (
                403,
                "cross-tenant delete on a role-gated flat route must 403 — user 2 is not a member of tenant 1, so require_membership_role denies before the scoped remove (issue #247, NOT the session role)",
            )
        } else {
            (
                404,
                "cross-tenant delete must 404 (use remove_for, not remove)",
            )
        };
        t.push_str(&format!(
            "    let del = t.delete_with(&format!(\"{cbase}/{{id}}\"), &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(del.status().as_u16(), {code}, \"{why}; body: {{}}\", del.text());\n",
        ));
        if get_one.is_some() {
            t.push_str(&format!(
                "    let survives = t.get_with(&format!(\"{cbase}/{{id}}\"), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(survives.status().as_u16(), 200, \"tenant 1's row must survive a cross-tenant delete; body: {{}}\", survives.text());\n",
            ));
        }
    }
    t.push_str("}\n\n");
    t
}

/// The FLAT-write (#96) cross-tenant isolation test: a member of tenant 1 must NOT
/// be able to CREATE a row into a tenant they don't belong to. WHY (Rule 9): #97
/// makes the bare `insert` non-existent, so a flat create must go through
/// `create_for_memberships`, whose RLS `WITH CHECK` verifies the body's tenant fk is
/// in the caller's membership set (403 otherwise). This test is that contract's
/// backstop — it POSTs a create whose tenant fk is tenant 2 (a tenant `app()` seeds
/// for user 2 ONLY, via `seed_second_tenant`) as user 1 and asserts 403. RED on
/// stubs (the create 500s), green only with the membership-checked create. FLAT
/// (MembershipSet) entities only: a path-scoped/nested write takes its tenant from
/// the VERIFIED path, not the body, so it has no body-fk leak (and is covered by the
/// path-scoped read isolation test instead).
fn flat_write_isolation_test(design: &Design, module: &ModuleDesign) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    // A DIRECT flat tenant-owned entity on this module — the tenant fk is a real column
    // it carries in the BODY (empty tenant-path joins), so we can aim that fk at a
    // foreign tenant. A transitive grandchild (non-empty joins, nested `/accounts/{id}`
    // mount) resolves its tenant through the parent chain and drops the fk from the body
    // (it rides the path), so this body-fk probe does not apply — the read isolation
    // test covers it. A path-scoped entity is scoped by the verified path tenant.
    let Some(entity) = module.entities.iter().find(|e| {
        super::genroute::entity_is_flat_tenant_owned(e, design)
            && design
                .tenant_path(&e.name)
                .is_some_and(|p| p.joins.is_empty())
    }) else {
        return String::new();
    };
    // A GUARDED creator at "/" with this entity's body — the write whose body fk we
    // aim at a foreign tenant. `create_for_memberships` needs the session `_user`, so a
    // create must be guarded to prove the 403; without one there is nothing to probe.
    let Some(create) = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == "/"
            && ep.is_guarded()
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity.name.as_str()))
    }) else {
        return String::new();
    };
    let base = module.effective_mount();
    let base = base.trim_end_matches('/');
    // Defensive: a direct flat child never mounts under a path token, but if it somehow
    // did the probe URL would carry an unsubstituted `{..}` — skip rather than emit it.
    if base.contains('{') {
        return String::new();
    }
    let plural = module.name.replace('-', "_");
    let fk_col = Design::fk_column(&tenancy.entity);
    // Start from the seeded-tenant fixture body, then re-aim ONLY the tenant fk at
    // tenant 2 — a tenant user 1 is NOT a member of (`seed_second_tenant` seeds tenant
    // 2 for user 2 only). `fixture_json` values a belongs_to fk at the seeded id 1 (a
    // text pk quoted, an integer pk bare), so the swap target is exact. A field-level
    // `overrides` entry would not reach the fk (fixture_json values fks separately), so
    // we do the targeted swap here.
    let (seeded_fk, foreign_fk) = match design.target_key_rust_type(&tenancy.entity) {
        "String" => ("\"1\"", "\"2\""),
        _ => ("1", "2"),
    };
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        &[],
        false, // a create body omits defaults
    )
    .replacen(
        &format!("\"{fk_col}\": {seeded_fk}"),
        &format!("\"{fk_col}\": {foreign_fk}"),
        1,
    );
    let hk = design.test_auth_header();
    format!(
        "/// SECURITY (#96/#97): a member of one tenant must NOT create a row into a\n/// tenant they don't belong to. User 1 (tenant 1) POSTs a create whose `{fk_col}` is\n/// tenant 2 (foreign); `create_for_memberships`'s RLS WITH CHECK must reject it with\n/// 403. Passes only with the membership-checked create — the bare `insert` that would\n/// skip the check is not generated for a flat tenant entity (#97).\n#[tokio::test]\nasync fn {plural}_flat_write_into_foreign_tenant_is_403() {{\n    let t = app().await;\n    let res = t.post_json_with(\"{base}/\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 403, \"a create into a non-member tenant must 403 (create_for_memberships WITH CHECK, #94); body: {{}}\", res.text());\n}}\n\n"
    )
}

/// The per-user (#79) isolation test: user 1 creates a row (the server injects
/// user 1's id); user 2 must not be able to read, list, or delete it. WHY (Rule 9):
/// the identity-owned shape JC0540 steers agents toward had NO backstop — an
/// unscoped `repo.all()` leaked every user's rows with `check` green. This test is
/// that backstop; it passes ONLY when the handler scopes via the owner accessors
/// (`all_for`/`get_for`/`remove_for`), which are now the ONLY methods generated
/// (genroute suppresses the unscoped ones). No tenant seeding — two distinct user
/// sessions (`test_cookie_for(1)`/`(2)`) are all it needs. db+auth only.
fn per_user_isolation_test(design: &Design, module: &ModuleDesign) -> String {
    if !(design.wants_db() && design.wants_auth()) {
        return String::new();
    }
    // Per-user classification is `Design::entity_is_per_user_owned` — the ONE
    // shared predicate (#105 §F): genroute suppresses the unscoped methods for
    // exactly the entities this test covers (TENANT ownership wins and is
    // TRANSITIVE, #102 — such entities get the cross-tenant test instead). A
    // `public_read` entity is EXCLUDED: its reads legitimately serve every
    // owner's rows, so this test's cross-user read-denial legs would be RED on a
    // correct app — it gets `public_read_isolation_test` (#105) instead.
    let Some(entity) = module
        .entities
        .iter()
        .find(|e| design.entity_is_per_user_owned(e) && !design.entity_is_public_read(&e.name))
    else {
        return String::new();
    };
    // A GUARDED creator at "/" with a body — the server injects the owner id from
    // the session, so the created row is owned by user 1.
    let Some(create) = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == "/"
            && ep.is_guarded()
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity.name.as_str()))
    }) else {
        return String::new();
    };
    let base = module.effective_mount();
    // Pin every mount `{param}` to the seeded id 1 so the probe URLs are concrete — a
    // per-user module mounted under a path param (`/orgs/{org_id}/notes`) would else
    // leave a literal `{param}` no `Path<i64>` can parse (issue #245, the #240 sibling
    // of `tenant_owned_isolation_test`). Byte-identical for a flat mount (no `{param}`).
    let base = concrete_mount_base(base.trim_end_matches('/'));
    let plural = module.name.replace('-', "_");
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        &[],
        false, // isolation seeds a row via create — a create body omits defaults
    );
    let create_path = format!("{base}/");
    // Only GUARDED reads carry the owner scope — an unguarded read has no session to
    // scope by, so it can't prove isolation. Gate every probe leg on a guard.
    let guarded1 = |ep: &&Endpoint| ep.is_guarded();
    let get_one = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::GET && param_count(ep) == 1 && guarded1(ep));
    let delete_one = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::DELETE && param_count(ep) == 1 && guarded1(ep));
    let list = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::GET && param_count(ep) == 0 && guarded1(ep));

    // A module with NO readable endpoint (create-only, or create + update-only —
    // a guarded `PUT /{id}` is not a read leg) has nothing to isolation-probe, so
    // emit NO test rather than a setup-only body that binds `row`/`cookie2` and
    // asserts nothing (issue #240; that body also tripped `-D warnings` on the
    // unused bindings). Every probe leg below consumes both bindings, so this one
    // guard covers them — the write's own success + 401 probes still cover it.
    if get_one.is_none() && delete_one.is_none() && list.is_none() {
        return String::new();
    }

    let hk = design.test_auth_header();
    let mut t = String::new();
    t.push_str(&format!(
        "/// SECURITY (#79): a user must not reach another user's {entity} rows. User 1\n/// creates a row (the server injects user 1's id); user 2 must be denied read/\n/// list/delete. Passes only with the owner-scoped accessors (all_for/get_for/\n/// remove_for) — the unscoped methods are NOT generated (genroute, #79).\n#[tokio::test]\nasync fn user_a_cannot_read_user_b_{plural}() {{\n    let t = app().await;\n",
        entity = entity.name,
    ));
    t.push_str(&format!(
        "    let created = t.post_json_with(\"{create_path}\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n    let cookie2 = test_cookie_for(2);\n",
        status = create.success.status,
        entity = entity.name,
    ));
    if list.is_some() {
        t.push_str("    let id_value = row[\"id\"].clone();\n");
    }
    if get_one.is_some() || delete_one.is_some() {
        t.push_str(
            "    let id = row[\"id\"].as_str().map(str::to_string).unwrap_or_else(|| row[\"id\"].to_string());\n",
        );
    }
    if get_one.is_some() {
        t.push_str(&format!(
            "    let foreign = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-user get must 404 (use get_for(_user.0.id), not get); body: {{}}\", foreign.text());\n",
        ));
    }
    if list.is_some() {
        t.push_str(&format!(
            "    let listed = t.get_with(\"{base}/\", &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(listed.status().as_u16(), 200, \"user 2 lists their own {plural}; body: {{}}\", listed.text());\n    let rows: serde_json::Value = serde_json::from_str(&listed.text()).expect(\"list json\");\n    let absent = rows.as_array().map(|a| a.iter().all(|r| r[\"id\"] != id_value)).unwrap_or(true);\n    assert!(absent, \"cross-user list must NOT contain user 1's row (use all_for(_user.0.id)); body: {{}}\", listed.text());\n",
        ));
    }
    if delete_one.is_some() {
        t.push_str(&format!(
            "    let del = t.delete_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(del.status().as_u16(), 404, \"cross-user delete must 404 (use remove_for(_user.0.id), not remove); body: {{}}\", del.text());\n",
        ));
        if get_one.is_some() {
            t.push_str(&format!(
                "    let survives = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(survives.status().as_u16(), 200, \"user 1's row must survive a cross-user delete; body: {{}}\", survives.text());\n",
            ));
        }
    }
    t.push_str("}\n\n");
    t
}

/// The public-read/owner-write isolation test (#105) — the `public_read` sibling
/// of [`per_user_isolation_test`]. WHY (Rule 9): the flag splits the ownership
/// contract in two, and each half needs a backstop or it silently rots into the
/// other. The READ half — anyone, even anonymous, sees EVERY owner's rows (the
/// feed intent) — fails if an agent leaves the read owner-scoped (all_for) or
/// guarded. The WRITE half — creates need a session, updates/deletes 404 for a
/// non-owner with the row SURVIVING — fails if "public read" bleeds into "public
/// write" (an anon POST landing, or a foreign PUT/DELETE touching the row).
/// Emitted only for a module owning a `public_read` entity (the shared
/// `Design::entity_is_public_read` classifier) with a guarded creator; every
/// other design stays byte-identical. db+auth only.
fn public_read_isolation_test(design: &Design, module: &ModuleDesign) -> String {
    if !(design.wants_db() && design.wants_auth()) {
        return String::new();
    }
    let Some(entity) = module
        .entities
        .iter()
        .find(|e| design.entity_is_public_read(&e.name))
    else {
        return String::new();
    };
    // A GUARDED creator at "/" with a body — the server injects the owner id from
    // the session, so the created row is owned by user 1 (and the anon-POST-401
    // leg has a guard to prove).
    let Some(create) = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == "/"
            && ep.is_guarded()
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity.name.as_str()))
    }) else {
        return String::new();
    };
    let base = module.effective_mount();
    // Pin every mount `{param}` to the seeded id 1 so the probe URLs are concrete — a
    // public_read module mounted under a path param would else leave a literal
    // `{param}` no `Path<i64>` can parse (issue #245, the #240 sibling of
    // `tenant_owned_isolation_test`). Byte-identical for a flat mount (no `{param}`).
    let base = concrete_mount_base(base.trim_end_matches('/'));
    let plural = module.name.replace('-', "_");
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        &[],
        false, // isolation seeds a row via create — a create body omits defaults
    );
    let create_path = format!("{base}/");
    // The read legs use the endpoints genroute actually UNGUARDS — the shared
    // `Design::endpoint_is_public_read_get` (a role-gated GET keeps its guard and
    // is not probed anonymously). The write legs bind this entity's guarded
    // PUT/DELETE at "/{id}".
    let this_entity =
        |ep: &&Endpoint| endpoint_repo_entity(module, ep) == Some(entity.name.as_str());
    let list = module
        .endpoints
        .iter()
        .find(|ep| {
            ep.method == HttpMethod::GET
                && param_count(ep) == 0
                && this_entity(ep)
                && design.endpoint_is_public_read_get(module, ep)
        })
        .map(|ep| ep.path.clone());
    let get_one = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::GET
            && param_count(ep) == 1
            && this_entity(ep)
            && design.endpoint_is_public_read_get(module, ep)
    });
    let put_one = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::PUT && param_count(ep) == 1 && this_entity(ep) && ep.is_guarded()
    });
    let delete_one = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::DELETE
            && param_count(ep) == 1
            && this_entity(ep)
            && ep.is_guarded()
    });

    let hk = design.test_auth_header();
    let mut t = String::new();
    t.push_str(&format!(
        "/// SECURITY (#105): {entity} is public_read — reads are PUBLIC (anyone, even\n/// anonymous, sees every owner's rows), writes stay OWNER-scoped. User 1 creates a\n/// row; an anonymous reader must see it; an anonymous create must 401; user 2's\n/// update/delete must 404 with the row surviving; user 1's update succeeds.\n#[tokio::test]\nasync fn anon_reads_but_only_the_owner_writes_{plural}() {{\n    let t = app().await;\n",
        entity = entity.name,
    ));
    t.push_str(&format!(
        "    let created = t.post_json_with(\"{create_path}\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n",
        status = create.success.status,
        entity = entity.name,
    ));
    // `row` is read ONLY to derive `id_value` (the list-present check) or `id` (the
    // by-id read/write legs); the always-present anon-create 401 leg needs neither,
    // so bind it only when a leg below consumes it (no unused-var, #240).
    if list.is_some() || get_one.is_some() || put_one.is_some() || delete_one.is_some() {
        t.push_str(
            "    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n",
        );
    }
    if list.is_some() {
        t.push_str("    let id_value = row[\"id\"].clone();\n");
    }
    if get_one.is_some() || put_one.is_some() || delete_one.is_some() {
        t.push_str(
            "    let id = row[\"id\"].as_str().map(str::to_string).unwrap_or_else(|| row[\"id\"].to_string());\n",
        );
    }
    // PUBLIC READ: an anonymous list returns 200 AND contains user 1's row — the
    // whole collection, not the caller's slice (there is no caller).
    if let Some(list_path) = &list {
        t.push_str(&format!(
            "    let listed = t.get(\"{base}{list_path}\").await;\n    assert_eq!(listed.status().as_u16(), 200, \"anonymous list must 200 (public_read); body: {{}}\", listed.text());\n    let rows: serde_json::Value = serde_json::from_str(&listed.text()).expect(\"list json\");\n    let present = rows.as_array().map(|a| a.iter().any(|r| r[\"id\"] == id_value)).unwrap_or(false);\n    assert!(present, \"the anonymous list must contain ANOTHER user's row (public read serves the whole collection); body: {{}}\", listed.text());\n",
        ));
    }
    if get_one.is_some() {
        t.push_str(&format!(
            "    let detail = t.get(&format!(\"{base}/{{id}}\")).await;\n    assert_eq!(detail.status().as_u16(), 200, \"anonymous detail must 200 (public_read); body: {{}}\", detail.text());\n",
        ));
    }
    // OWNER WRITE: an anonymous create is rejected by the guard.
    t.push_str(&format!(
        "    let anon_create = t.post_json(\"{create_path}\", &serde_json::json!({body})).await;\n    assert_eq!(anon_create.status().as_u16(), 401, \"public_read never opens WRITES — an anonymous create must 401; body: {{}}\", anon_create.text());\n",
    ));
    if let Some(put) = put_one {
        let put_body = fixture_json(
            design,
            module,
            &entity.name,
            omits_identity_fk(design, module, put),
            &[],
            true, // an UPDATE body keeps `default` fields ({Entity}UpdateRequest)
        );
        t.push_str(&format!(
            "    let foreign_put = t.put_json_with(&format!(\"{base}/{{id}}\"), &serde_json::json!({put_body}), &[(\"{hk}\", &test_cookie_for(2))]).await;\n    assert_eq!(foreign_put.status().as_u16(), 404, \"a non-owner update must 404 (use update_for, not update); body: {{}}\", foreign_put.text());\n",
        ));
    }
    if delete_one.is_some() {
        t.push_str(&format!(
            "    let foreign_del = t.delete_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &test_cookie_for(2))]).await;\n    assert_eq!(foreign_del.status().as_u16(), 404, \"a non-owner delete must 404 (use remove_for, not remove); body: {{}}\", foreign_del.text());\n",
        ));
    }
    if get_one.is_some() && (put_one.is_some() || delete_one.is_some()) {
        t.push_str(&format!(
            "    let survives = t.get(&format!(\"{base}/{{id}}\")).await;\n    assert_eq!(survives.status().as_u16(), 200, \"the row must SURVIVE a non-owner write attempt; body: {{}}\", survives.text());\n",
        ));
    }
    if let Some(put) = put_one {
        let put_body = fixture_json(
            design,
            module,
            &entity.name,
            omits_identity_fk(design, module, put),
            &[],
            true,
        );
        t.push_str(&format!(
            "    let owner_put = t.put_json_with(&format!(\"{base}/{{id}}\"), &serde_json::json!({put_body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(owner_put.status().as_u16(), {status}, \"the OWNER's update must succeed; body: {{}}\", owner_put.text());\n",
            status = put.success.status,
        ));
    }
    t.push_str("}\n\n");
    t
}

/// The tenant-collection-create isolation/lifecycle test (REVIEWER I1): user 1
/// creates a tenant; user 1's immediate list-own returns it; user 2's list is
/// EMPTY of it. WHY (Rule 9): T3 left the bare `insert` reachable next to
/// `create_with_membership`; an agent who calls `insert` (skipping the membership
/// seed) leaves the tenant memberless — the creator is locked out and, worse, a
/// membership-filtered list can silently diverge. This test makes that failure
/// LOUD: it passes only when create seeds the creator's membership AND list scopes
/// to `all_for_member`. Emitted only when the tenant module has BOTH a guarded
/// `POST "/"` create and a guarded `GET "/"` list for the tenant entity — an
/// unguarded list (e.g. reference-slice `list_workspaces`) can't be membership-
/// scoped, so the test would be un-passable and is skipped.
fn tenant_collection_isolation_test(design: &Design, module: &ModuleDesign) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    // This module must DECLARE the tenant entity (be the tenant module).
    let Some(entity) = module.entities.iter().find(|e| e.name == tenancy.entity) else {
        return String::new();
    };
    let Some(create) = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == "/"
            && ep.is_guarded()
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity.name.as_str()))
    }) else {
        return String::new();
    };
    // A GUARDED list at "/" — an unguarded list has no session to membership-scope
    // by, so it can't prove the second-user-empty contract; skip it there.
    if !module.endpoints.iter().any(|ep| {
        ep.method == HttpMethod::GET && ep.path == "/" && ep.is_guarded() && ep.success.list
    }) {
        return String::new();
    }
    let base = module.effective_mount();
    let base = base.trim_end_matches('/');
    let plural = module.name.replace('-', "_");
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        &[],
        false, // isolation seeds a row via create — a create body omits defaults
    );
    let hk = design.test_auth_header();
    format!(
        "/// SECURITY (#78, I1): creating a {entity} seeds ONLY the creator's membership.\n/// User 1 creates a {entity}; user 1's own list returns it; user 2's list is empty.\n/// Passes only when create uses `create_with_membership` (NOT the bare `insert`,\n/// which leaves the tenant memberless) and list uses `all_for_member`.\n#[tokio::test]\nasync fn creating_a_{plural2}_seeds_only_the_creators_membership() {{\n    let t = app().await;\n    let created = t.post_json_with(\"{base}/\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n    let id_value = row[\"id\"].clone();\n    let own = t.get_with(\"{base}/\", &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(own.status().as_u16(), 200, \"user 1 lists their own {plural}; body: {{}}\", own.text());\n    let own_rows: serde_json::Value = serde_json::from_str(&own.text()).expect(\"own list json\");\n    let present = own_rows.as_array().map(|a| a.iter().any(|r| r[\"id\"] == id_value)).unwrap_or(false);\n    assert!(present, \"the creator's list MUST contain the new {entity} (create_with_membership seeds membership); body: {{}}\", own.text());\n    let other = t.get_with(\"{base}/\", &[(\"{hk}\", &test_cookie_for(2))]).await;\n    assert_eq!(other.status().as_u16(), 200, \"user 2 lists their own {plural}; body: {{}}\", other.text());\n    let other_rows: serde_json::Value = serde_json::from_str(&other.text()).expect(\"other list json\");\n    let absent = other_rows.as_array().map(|a| a.iter().all(|r| r[\"id\"] != id_value)).unwrap_or(true);\n    assert!(absent, \"a non-creator's list must NOT contain the new {entity} (use all_for_member); body: {{}}\", other.text());\n}}\n\n",
        entity = entity.name,
        plural2 = Design::to_snake(&entity.name),
        status = create.success.status,
    )
}

/// The tenant ROOT's own detail-route cross-tenant probe (#172): user 1 creates a
/// root {entity} (tenant 1); user 2 (a member of tenant 2 only — `app()` seeds it)
/// must NOT be able to read it by id. WHY (Rule 9): `tenant_owned_isolation_test`
/// SKIPS the root (its `tenant_path` is `None` — the root does not `belongs_to`
/// itself) and `tenant_collection_isolation_test` covers only the root's
/// COLLECTION (user 2's list is empty), so the root's own `GET /{id}` detail route
/// has NO cross-tenant probe — a regression that reads via the unscoped `get`
/// leaks ANY tenant's root row behind a fully-green suite. This probe is RED on a
/// fresh scaffold's stub (the create 500s) AND on an unscoped `get` (200 — the
/// leak), and GREEN only when the detail handler scopes the read to the caller's
/// memberships (a non-member ⇒ `None` ⇒ 404).
///
/// Emitted ONLY for the module DECLARING `tenancy.entity` that has BOTH a GUARDED
/// creator at "/" (to seed the tenant-1 row) AND a GUARDED `GET /{id}` detail
/// route. A PUBLIC root detail route (e.g. the reference-slice's `show_workspace`
/// "public discovery", which returns 200 to everyone by design) gets NO probe — a
/// cross-tenant 404 assertion would false-fail it — so every non-tenancy /
/// public-detail / detail-less design stays byte-identical. Decoupled from the
/// list-gate of `tenant_collection_isolation_test` (which early-returns without a
/// guarded LIST): a root with a guarded detail route but no guarded list still
/// gets the probe.
fn tenant_root_detail_isolation_test(design: &Design, module: &ModuleDesign) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    // This module must DECLARE the tenant entity (be the tenant/root module).
    let Some(entity) = module.entities.iter().find(|e| e.name == tenancy.entity) else {
        return String::new();
    };
    // A GUARDED creator at "/" with this entity's body seeds the tenant-1 row to
    // probe; `create_with_membership` keys on the session `_user`, so it must be
    // guarded — without one there is nothing to isolate.
    let Some(create) = module.endpoints.iter().find(|ep| {
        ep.method == HttpMethod::POST
            && ep.path == "/"
            && ep.is_guarded()
            && ep
                .request_body
                .as_ref()
                .is_some_and(|rb| rb.entity.as_deref() == Some(entity.name.as_str()))
    }) else {
        return String::new();
    };
    // A GUARDED `GET /{id}` detail route — the route this probe protects. A PUBLIC
    // detail route (200 to everyone by design) is skipped: asserting a cross-tenant
    // 404 against it would false-fail a deliberately-public discovery route.
    if !module
        .endpoints
        .iter()
        .any(|ep| ep.method == HttpMethod::GET && param_count(ep) == 1 && ep.is_guarded())
    {
        return String::new();
    }
    let base = module.effective_mount();
    let base = base.trim_end_matches('/');
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        &[],
        false, // isolation seeds a row via create — a create body omits defaults
    );
    let hk = design.test_auth_header();
    let snake = Design::to_snake(&entity.name);
    // A by-id URL must carry the RAW id: a string PK's `Value::String` Display
    // includes JSON quotes (`"uuid"`), so interpolate the unquoted string (a numeric
    // PK is identical) — mirrors the child `foreign` leg.
    format!(
        "/// SECURITY (#172): the tenant ROOT's own detail route must not leak another\n/// tenant's row. User 1 creates a {entity} (tenant 1); user 2 (tenant 2, seeded by\n/// app()) must NOT read it by id. Passes only when the detail handler scopes the\n/// read to the caller's memberships (a non-member ⇒ None ⇒ 404), NOT the unscoped\n/// `get`, which would leak any tenant's {entity}.\n#[tokio::test]\nasync fn a_non_member_cannot_read_the_{snake}_detail() {{\n    let t = app().await;\n    let created = t.post_json_with(\"{base}/\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n    let id = row[\"id\"].as_str().map(str::to_string).unwrap_or_else(|| row[\"id\"].to_string());\n    let foreign = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &test_cookie_for(2))]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-tenant get on the tenant root must 404 (scope the detail read to the caller's memberships, not the unscoped get); body: {{}}\", foreign.text());\n}}\n\n",
        entity = entity.name,
        status = create.success.status,
    )
}

/// True when this module's acceptance file carries the #107 member-surface
/// tests: db+auth+tenancy and the module DECLARES the tenancy entity — the same
/// gate genroute's `emits_member_surface` uses, so the tests exist exactly where
/// the generated member routes do and every other module (and every non-tenancy
/// design) stays byte-identical.
fn emits_member_surface_tests(design: &Design, module: &ModuleDesign) -> bool {
    design.wants_db()
        && design.wants_auth()
        && design
            .tenancy
            .as_ref()
            .is_some_and(|t| module.entities.iter().any(|e| e.name == t.entity))
}

/// The member-management surface tests (issue #107, spec §D): list/add/re-role/
/// remove plus the SECURITY rules — the admin (`member_roles[0]`) gate (403), the
/// last-admin lockout (409 on demote AND remove), self-removal without the admin
/// role (204), and the role allow-list (422). WHY (Rule 9): the member routes are
/// TOOL-OWNED with REAL generated handlers (members.rs), so unlike the stub
/// probes these PASS on a fresh scaffold and turn RED only when the generated
/// surface itself breaks — they are the runtime backstop for the #107 rules, and
/// (like the enum reject probes) they are EXCLUDED from `expected_failing`.
///
/// `member_app()` seeds the tenant + memberships via RAW SQL (the same shape as
/// `tenant_seed`), never through the module's own creator: the creator is an
/// AGENT STUB on a fresh scaffold, so an HTTP-seeded setup would 500 before the
/// member surface was ever reached. User 1 holds the admin role; user 2 (when a
/// second role is declared) is the non-admin member the 403/self-removal probes
/// act as. The tests that NEED that non-admin second role (403, re-role, remove,
/// demote-409, self-leave) are emitted only for a multi-role design; a
/// single-role design keeps list/add/last-admin-409/422.
fn member_surface_tests(design: &Design, module: &ModuleDesign) -> String {
    if !emits_member_surface_tests(design, module) {
        return String::new();
    }
    let tenancy = design.tenancy.as_ref().expect("gated on tenancy");
    let Some(entity) = module.entities.iter().find(|e| e.name == tenancy.entity) else {
        return String::new();
    };
    let mount = module.effective_mount();
    let base = mount.trim_end_matches('/').to_string();
    let snake = Design::to_snake(&tenancy.entity);
    let table = design.table_name(&tenancy.entity);
    let members = format!("{snake}_members");
    let fk = Design::fk_column(&tenancy.entity);
    // The admin role is member_roles[0] by convention (JC0548 guarantees a
    // non-empty list at design time; the dead fallback matches genroute's).
    let admin = tenancy
        .member_roles
        .first()
        .map(String::as_str)
        .unwrap_or("member");
    let second = tenancy.member_roles.get(1).map(String::as_str);
    // A single-role design can only add another admin; a multi-role design adds
    // a NON-admin member (the spec's "non-admin role" add).
    let add_role = second.unwrap_or(admin);
    let hk = design.test_auth_header();
    let (cols, vals) = tenant_row_cols_vals(entity, "1", 1);
    let migrate_block = migrate_call_block(
        &collect_workspace_migration_items(design, module),
        "migrations",
    );
    let auth_extend = format!(".extend(jerrycan::auth::Auth::with_secret(\"{TEST_SECRET}\"))");
    let (_, ext_extends) = extension_wiring(design);
    let second_seed = second
        .map(|role| {
            format!(
                "    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (2, 1, '{role}')\")\n        .await\n        .expect(\"seed non-admin membership\");\n"
            )
        })
        .unwrap_or_default();

    let mut t = format!(
        "/// #107 member surface: TOOL-OWNED routes with REAL generated handlers, so\n/// these tests pass on a fresh scaffold and turn RED only if the generated\n/// surface (admin gate, last-admin lockout, self-removal, role allow-list)\n/// breaks. Seeded via raw SQL — the HTTP surface under test is exactly what\n/// removes that need from application code.\nasync fn member_app() -> TestApp {{\n    let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n{migrate_block}    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols}) VALUES ({vals})\")\n        .await\n        .expect(\"seed tenant row\");\n    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (1, 1, '{admin}')\")\n        .await\n        .expect(\"seed admin membership\");\n{second_seed}    App::new(){auth_extend}{ext_extends}.extend(db).provide_dep(shared::tenant).mount(\"{mount}\", module()).into_test()\n}}\n\n"
    );

    // list: any member sees the roster (the membership guard is the whole gate).
    t.push_str(&format!(
        "#[tokio::test]\nasync fn list_{snake}_members_returns_200() {{\n    let t = member_app().await;\n    let res = t.get_with(\"{base}/1/members\", &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 200, \"design: any member lists the roster; body: {{}}\", res.text());\n    let rows: serde_json::Value = serde_json::from_str(&res.text()).expect(\"roster json\");\n    let has_admin = rows.as_array().map(|a| a.iter().any(|m| m[\"user_id\"] == serde_json::json!(\"1\") && m[\"role\"] == serde_json::json!(\"{admin}\"))).unwrap_or(false);\n    assert!(has_admin, \"the roster must list the seeded {admin} (user 1); body: {{}}\", res.text());\n}}\n\n"
    ));
    // add: an admin adds a member (non-admin role when one is declared) → 201.
    t.push_str(&format!(
        "#[tokio::test]\nasync fn add_{snake}_member_returns_201() {{\n    let t = member_app().await;\n    let res = t.post_json_with(\"{base}/1/members\", &serde_json::json!({{\"user_id\": \"9\", \"role\": \"{add_role}\"}}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 201, \"design: an {admin} adds a member -> 201; body: {{}}\", res.text());\n}}\n\n"
    ));
    if let Some(role2) = second {
        // SECURITY: member management is admin-gated — a non-admin add is 403.
        t.push_str(&format!(
            "/// SECURITY (#107): member management is gated on the {admin} role — a\n/// {role2} may read the roster but must NOT be able to add members.\n#[tokio::test]\nasync fn add_{snake}_member_without_the_admin_role_is_403() {{\n    let t = member_app().await;\n    let res = t.post_json_with(\"{base}/1/members\", &serde_json::json!({{\"user_id\": \"9\", \"role\": \"{role2}\"}}), &[(\"{hk}\", &test_cookie_for(2))]).await;\n    assert_eq!(res.status().as_u16(), 403, \"design: member add requires the {admin} role — a {role2} must 403; body: {{}}\", res.text());\n}}\n\n"
        ));
        // set-role: the write must PERSIST (roster reflects it), not just 204.
        t.push_str(&format!(
            "#[tokio::test]\nasync fn set_{snake}_member_role_returns_204() {{\n    let t = member_app().await;\n    let res = t.patch_json_with(\"{base}/1/members/2\", &serde_json::json!({{\"role\": \"{admin}\"}}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 204, \"design: an {admin} re-roles a member -> 204; body: {{}}\", res.text());\n    let roster = t.get_with(\"{base}/1/members\", &[(\"{hk}\", &test_cookie_for(1))]).await;\n    let rows: serde_json::Value = serde_json::from_str(&roster.text()).expect(\"roster json\");\n    let promoted = rows.as_array().map(|a| a.iter().any(|m| m[\"user_id\"] == serde_json::json!(\"2\") && m[\"role\"] == serde_json::json!(\"{admin}\"))).unwrap_or(false);\n    assert!(promoted, \"the roster must reflect the new role (the PATCH must persist, not just 204); body: {{}}\", roster.text());\n}}\n\n"
        ));
        // remove: the delete must PERSIST (the member leaves the roster).
        // Fail-closed: the roster read-back must be 200 and an ARRAY without
        // user 2 — `unwrap_or(true)` would pass vacuously if the list route
        // broke (non-array body), like the set-role twin's `unwrap_or(false)`.
        t.push_str(&format!(
            "#[tokio::test]\nasync fn remove_{snake}_member_returns_204() {{\n    let t = member_app().await;\n    let res = t.delete_with(\"{base}/1/members/2\", &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 204, \"design: an {admin} removes a member -> 204; body: {{}}\", res.text());\n    let roster = t.get_with(\"{base}/1/members\", &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(roster.status().as_u16(), 200, \"the roster read-back must succeed (the removal is verified against it); body: {{}}\", roster.text());\n    let rows: serde_json::Value = serde_json::from_str(&roster.text()).expect(\"roster json\");\n    let gone = rows.as_array().map(|a| a.iter().all(|m| m[\"user_id\"] != serde_json::json!(\"2\"))).unwrap_or(false);\n    assert!(gone, \"the removed member must leave the roster (the DELETE must persist); body: {{}}\", roster.text());\n}}\n\n"
        ));
        // SECURITY: demoting the sole admin would lock the tenant out of member
        // management (the write gate is admin-only) — 409, never applied.
        t.push_str(&format!(
            "/// SECURITY (#107): demoting the SOLE {admin} would leave nobody able to\n/// manage members (the write gate is {admin}-only) — the repo must refuse with 409.\n#[tokio::test]\nasync fn set_{snake}_member_role_last_admin_demotion_is_409() {{\n    let t = member_app().await;\n    let res = t.patch_json_with(\"{base}/1/members/1\", &serde_json::json!({{\"role\": \"{role2}\"}}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 409, \"design: demoting the sole {admin} must 409 (last-admin lockout); body: {{}}\", res.text());\n}}\n\n"
        ));
        // self-removal: any member may LEAVE without the admin role.
        t.push_str(&format!(
            "/// #107: self-removal (\"leave\") needs NO admin role — the guard already\n/// proved the caller's membership; only removing OTHERS is admin-gated.\n#[tokio::test]\nasync fn remove_{snake}_member_self_leave_returns_204() {{\n    let t = member_app().await;\n    let res = t.delete_with(\"{base}/1/members/2\", &[(\"{hk}\", &test_cookie_for(2))]).await;\n    assert_eq!(res.status().as_u16(), 204, \"design: a member removes their OWN membership without the {admin} role; body: {{}}\", res.text());\n}}\n\n"
        ));
    }
    // SECURITY: removing the sole admin is refused even as self-removal.
    t.push_str(&format!(
        "/// SECURITY (#107): removing the SOLE {admin} — even by themselves — would\n/// leave the tenant admin-less forever; the repo must refuse with 409.\n#[tokio::test]\nasync fn remove_{snake}_member_last_admin_is_409() {{\n    let t = member_app().await;\n    let res = t.delete_with(\"{base}/1/members/1\", &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 409, \"design: removing the sole {admin} must 409 (last-admin lockout); body: {{}}\", res.text());\n}}\n\n"
    ));
    // An out-of-set role must 422 (no DB CHECK backs the role column).
    t.push_str(&format!(
        "#[tokio::test]\nasync fn add_{snake}_member_rejects_out_of_range_role() {{\n    let t = member_app().await;\n    let res = t.post_json_with(\"{base}/1/members\", &serde_json::json!({{\"user_id\": \"9\", \"role\": \"{ENUM_REJECT_SENTINEL}\"}}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(res.status().as_u16(), 422, \"design: a role outside member_roles must 422 (no DB CHECK backs the column); body: {{}}\", res.text());\n}}\n\n"
    ));
    t
}

/// The SQL literal for a field's server-owned `default` (#249): a seeded row must
/// store what the DB actually would — the declared default — not the generic `1`
/// fixture, or a `reserve_against` counter (`seats_used default:0`) is seeded AT
/// capacity and its reserve probe 409s (Ok(false) → 409, never the asserted 200).
/// None when the field has no default, so every non-defaulted field keeps its prior
/// seed literal → byte-identical for designs without a defaulted seed column.
fn default_sql_literal(f: &Field) -> Option<String> {
    Some(match f.default.as_ref()? {
        serde_json::Value::String(s) => format!("'{s}'"),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        // A json/array/object/null default is unusual in a seed; render its JSON text
        // as a quoted literal (matching the Json field seed shape).
        other => format!("'{other}'"),
    })
}

/// A SQL literal for seeding a tenant-row column. Enum fields use their first
/// declared value (so a CHECK constraint passes); other fields use a type-shaped
/// literal. String/text literals are single-quoted for inline DDL execution.
fn seed_sql_value(f: &Field) -> String {
    // #249: a server-owned `default` seeds its declared value (a `default:0` counter
    // → 0), so a `reserve_against` counter is not seeded AT capacity (its reserve
    // probe would 409 instead of the asserted 200). Gated on `default` being present.
    if let Some(lit) = default_sql_literal(f) {
        return lit;
    }
    if let Some(values) = &f.values
        && let Some(first) = values.first()
    {
        return format!("'{first}'");
    }
    // #80: a constrained field seeds an IN-RANGE literal (the row must clear
    // the migration CHECK and any later validated read), a `unique` one a
    // value DISTINCT from the probe fixture — the next value after the
    // fixture anchor / the 'seed-…' base fitted to the length bounds — so a
    // create probe on a pre-seeded row still can't 409 (#85). Both branches
    // gate on a constraint being present (byte-identity).
    if has_int_range(f) {
        return if f.unique {
            kth_in_range(f, 1)
        } else {
            clamp_int(1, f)
        }
        .to_string();
    }
    if has_len_range(f) {
        let s = if f.unique {
            fit_string("seed-test-value", f.min_len, f.max_len)
        } else {
            constrained_fixture_string(f)
        };
        return format!("'{s}'");
    }
    match f.field_type {
        // A `unique` String/Integer/Float shares its literal with the create-probe
        // body (`fixture_value`), so a create probe on a pre-seeded tenant row 409s
        // (#85). Seed a DISTINCT value for those. datetime/uuid seeds ('test-value')
        // already differ from their probe fixtures (a real timestamp / v4 uuid), so
        // they stay unchanged; boolean/json are never realistic unique keys.
        FieldType::String if f.unique => "'seed-test-value'".to_string(),
        FieldType::Integer if f.unique => "1000".to_string(),
        FieldType::Float if f.unique => "1000.0".to_string(),
        FieldType::String | FieldType::Datetime | FieldType::Uuid => "'test-value'".to_string(),
        FieldType::Integer => "1".to_string(),
        FieldType::Float => "1.0".to_string(),
        FieldType::Boolean => "false".to_string(),
        FieldType::Json => "'{}'".to_string(),
    }
}

/// A SQL literal for seeding the Nth tenant's row, made DISTINCT from earlier
/// tenants so a `unique` non-PK column (e.g. `Workspace.slug`) doesn't collide
/// when the isolation test seeds tenant 2. Tenant 1 (`n == 1`) is byte-identical
/// to `seed_sql_value` (keeps every existing seed unchanged). Enum fields stay
/// fixed at the first declared value — they can't vary without violating the
/// CHECK — which is safe because an enum column is not the unique key in practice.
fn seed_sql_value_n(f: &Field, n: u32) -> String {
    if n == 1 {
        return seed_sql_value(f);
    }
    // #249: a defaulted column is the same for EVERY tenant — honor the default for
    // the Nth tenant too so tenant 1 and tenant 2 stay consistent (byte-identical
    // for a non-defaulted column).
    if let Some(lit) = default_sql_literal(f) {
        return lit;
    }
    if let Some(values) = &f.values
        && let Some(first) = values.first()
    {
        return format!("'{first}'");
    }
    // #80: the Nth tenant's constrained literals stay in-range; a `unique`
    // integer takes the Nth distinct in-range value (the fixture anchor is the
    // 0th, the tenant-1 seed the 1st), a string keeps its `-{n}` discriminator
    // through the length fit. Gated on a constraint being present.
    if has_int_range(f) {
        return if f.unique {
            kth_in_range(f, i64::from(n))
        } else {
            clamp_int(i64::from(n), f)
        }
        .to_string();
    }
    if has_len_range(f) {
        return format!("'{}'", constrained_seed_string_n(f, n));
    }
    match f.field_type {
        FieldType::String | FieldType::Datetime | FieldType::Uuid => format!("'test-value-{n}'"),
        FieldType::Integer => n.to_string(),
        FieldType::Float => format!("{n}.0"),
        FieldType::Boolean => "false".to_string(),
        FieldType::Json => "'{}'".to_string(),
    }
}

/// The mirrored-extension wiring for the db-mode `app()` harness (issue #66):
/// `(comment, extends)`. `extends` is the `.extend(...)` chain for the design's
/// declared storage/jobs/realtime extensions (in mounting.rs's order: storage,
/// jobs, realtime — all before `.extend(db)`), each test-env-safe. `comment` is a
/// documented header (only emitted when something is wired) recording the wired
/// set AND the deliberately-excluded extensions. Both are empty for a design that
/// declares none of these — that harness stays byte-identical (no-drift).
fn extension_wiring(design: &Design) -> (String, String) {
    let mut extends = String::new();
    if design.wants_storage() {
        // In-memory store + a fixed dev sign key: no `from_env`/secret env needed.
        extends.push_str(&format!(
            ".extend(jerrycan::storage::Storage::memory().with_sign_secret(\"{TEST_SECRET}\"))"
        ));
    }
    if design.wants_jobs() {
        // The worker/cron `on_serve` loops don't spawn under `into_test()`.
        extends.push_str(".extend(jerrycan::jobs::Jobs::postgres(db.clone()))");
    }
    if design.wants_realtime() {
        // Resolves `Dep<RealtimeHandle>` for realtime handlers (no JC1001) AND
        // declares the app's broadcast/presence topics on the extension — the SAME
        // topics the realtime crate wires (realtimegen::wiring_rs). Without them a
        // handler that publishes to a topic hits JC0404 (undeclared topic) on a bare
        // `Realtime::new`, so the probe is un-greenable (issue #84). Changes channels
        // are omitted: they need Postgres (never exercised by a sqlite TestApp) and
        // are not `RealtimeHandle::publish` targets.
        extends.push_str(&format!(
            ".extend(jerrycan::realtime::Realtime::new(db.clone()){})",
            super::realtimegen::topic_wiring_inline(design)
        ));
    }
    if extends.is_empty() {
        return (String::new(), String::new());
    }
    let comment =
        "// TestApp extension wiring (issue #66) mirrors main.rs so the generated probes\n\
        // exercise the SAME app the framework builds: the design's declared\n\
        // storage/jobs/realtime extensions are wired below (jobs/realtime take\n\
        // db.clone() before `.extend(db)` moves db; jobs' worker/cron loops are\n\
        // on_serve tasks that into_test() never spawns). EXCLUDED here: observe\n\
        // (no handler resolves it — only /healthz, /metrics, access-log middleware)\n\
        // and validate (its OpenApi extension include_str!s an absent openapi.json,\n\
        // and probes send valid fixtures). Cover any excluded surface yourself.\n"
            .to_string();
    (comment, extends)
}

/// `emit_app`: false when the module's only tests are the #107 member-surface
/// tests (every design endpoint is an AGENT TODO) — the regular `app()` helper
/// would then be dead code and trip the generated workspace's `-D warnings`, so
/// only the helpers the member tests use (imports, cookie mint) are emitted.
fn preamble(design: &Design, module: &ModuleDesign, uses_cookies: bool, emit_app: bool) -> String {
    let mount = module.effective_mount();
    // The cookie helpers (`test_cookie`/`test_cookie_for`) are only emitted when
    // the module's generated tests actually reference them — a module with no
    // guarded endpoint and no isolation test (e.g. a public webhook or OAuth
    // callback) would otherwise carry dead `test_cookie` fns that trip
    // `-D warnings`.
    let auth_login = if design.wants_auth() && uses_cookies {
        auth_preamble_login(design)
    } else {
        String::new()
    };
    // The auth extension must register before the guards resolve it; it leads the
    // App chain (and shares TEST_SECRET with test_cookie()).
    let auth_extend = if design.wants_auth() {
        format!(".extend(jerrycan::auth::Auth::with_secret(\"{TEST_SECRET}\"))")
    } else {
        String::new()
    };
    if design.wants_db() {
        // Migrate the FULL workspace schema (issue #14), not just this module's
        // tables: a handler may legitimately write another module's table, which
        // would 500 with "no such table" under a module-only TestApp. This also
        // subsumes the old tenant-module cross-include (the `{tenant}_members`
        // table the Tenant guard queries is now always present). Tenancy still
        // needs (b) a seeded membership row so the guard resolves a tenant (not
        // 403) and (c) the `tenant` factory registered so `Dep<Tenant>` resolves.
        let migrate_block = migrate_call_block(
            &collect_workspace_migration_items(design, module),
            "migrations",
        );
        let seed = tenant_seed(design, module);
        let tenant_dep = if module_provides_tenant_dep(design, module) {
            ".provide_dep(shared::tenant)"
        } else {
            ""
        };
        // The second-tenant seed helper exists only for tenant-owned modules (the
        // ones whose isolation test acts as a second user). app() always seeds it
        // so success tests (user 1, tenant 1) are unaffected and the isolation test
        // finds tenant 2 already present.
        let second_seed_fn = seed_second_tenant_fn(design, module);
        let second_seed_call = if second_seed_fn.is_empty() {
            String::new()
        } else {
            "    seed_second_tenant(&db).await;\n".to_string()
        };
        // The seed runs raw SQL on the connection, which needs `ConnectionTrait`
        // in scope; only import it when there's a seed OR the #107 member tests
        // (whose member_app() also seeds raw SQL) — else `-D warnings` trips.
        let seed_use = if seed.is_empty() && !emits_member_surface_tests(design, module) {
            String::new()
        } else {
            "use jerrycan::db::sea_orm::ConnectionTrait;\n\n".to_string()
        };
        if !emit_app {
            // Only the member-surface tests exist: no app(), no seeds, no
            // second-tenant helper — member_app() is self-contained.
            return format!("{seed_use}{auth_login}");
        }
        // Issue #66: the TestApp must wire the SAME design-declared extensions
        // main.rs does (mounting.rs's `extension_block`), so the generated probes
        // exercise the app the framework actually builds — a realtime handler's
        // `Dep<RealtimeHandle>` resolves (no JC1001 500) and a jobs design's app
        // constructs. Order + `db.clone()` mirror mounting.rs (extensions precede
        // `.extend(db)`, which moves `db`). Test-env realities: storage uses an
        // in-memory store (no `from_env`/secret env), and the jobs worker/cron
        // loops are `on_serve` tasks that `into_test()` never spawns, so wiring
        // Jobs starts no background loop. EXCLUDED, by design: `observe` (adds only
        // /healthz + /metrics + an access-log middleware — no handler resolves it,
        // so its absence never 500s a probe) and `validate` (its OpenApi extension
        // `include_str!`s app/openapi.json, which does not exist relative to a
        // route crate's test; and probes send valid fixtures, so wire-level
        // validation is not needed). See the harness comment emitted below.
        let (ext_comment, ext_extends) = extension_wiring(design);
        format!(
            "{seed_use}{auth_login}{second_seed_fn}{ext_comment}async fn app() -> TestApp {{\n    let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n{migrate_block}{seed}{second_seed_call}    App::new(){auth_extend}{ext_extends}.extend(db){tenant_dep}.mount(\"{mount}\", module()).into_test()\n}}\n"
        )
    } else {
        format!(
            "{auth_login}async fn app() -> TestApp {{\n    App::new(){auth_extend}.mount(\"{mount}\", module()).into_test()\n}}\n"
        )
    }
}

/// The full tests/acceptance.rs for one top-level module.
pub fn acceptance_rs(design: &Design, module: &ModuleDesign) -> String {
    render_acceptance(design, module).0
}

/// Renders the acceptance file AND the count of generated tests that PASS on
/// stubs — the enum "reject" probes (issue #47) and the #107 member-surface
/// tests (tool-owned real handlers) — which `write_acceptance` subtracts from
/// `expected_failing` so the RED-on-stubs baseline stays exact.
fn render_acceptance(design: &Design, module: &ModuleDesign) -> (String, usize) {
    let mut out = TestOut {
        code: String::new(),
        todos: Vec::new(),
        count: 0,
        reject: 0,
        auth: design.wants_auth(),
    };
    unit_tests(design, module, module, &module.effective_mount(), &mut out);
    // Cross-tenant isolation: the security contract for tenant-owned modules.
    // Appended after the per-endpoint tests; counts toward expected_failing
    // (it fails on stubs like every other generated test).
    let isolation = isolation_test(design, module);
    out.count += isolation.matches("#[tokio::test]").count();
    out.code.push_str(&isolation);
    // #107: the member-surface tests run against TOOL-OWNED real handlers, so
    // they PASS on stubs — appended after the isolation tests and, like the
    // enum reject probes, EXCLUDED from the RED-on-stubs baseline. Whether the
    // regular app() helper is needed is decided BEFORE appending them (a
    // tenant module whose every endpoint is a TODO has member tests only).
    let emit_app = out.code.contains("#[tokio::test]");
    let member = member_surface_tests(design, module);
    let member_passing = member.matches("#[tokio::test]").count();
    out.code.push_str(&member);
    let todos = if out.todos.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", out.todos.join("\n"))
    };
    let banner = "//! GENERATED by jerrycan gen-tests — TOOL-OWNED acceptance criteria from design.json.\n//! Regenerated on demand; add your own tests in sibling files, not here.\n";
    // A module whose every endpoint is a TODO (e.g. a billing module whose only
    // route is a signature-gated webhook) emits ZERO #[tokio::test] functions. The
    // preamble's `app()` helper and the `use` imports would then be dead code and
    // trip the generated workspace's `-D warnings`. Emit only the banner + the
    // TODOs in that case — there is nothing for the imports/app() to support.
    if !out.code.contains("#[tokio::test]") {
        return (format!("{banner}{todos}"), out.reject);
    }
    // Only emit the cookie helpers if the rendered tests reference them (a module
    // with no guarded endpoint and no isolation test uses neither).
    let uses_cookies = out.code.contains("test_cookie");
    let content = format!(
        "{banner}use jerrycan::prelude::*;\nuse {ident}::module;\n\n{preamble}\n{code}{todos}",
        ident = super::genroute::crate_ident(&module.name),
        preamble = preamble(design, module, uses_cookies, emit_app),
        code = out.code,
    );
    (content, out.reject + member_passing)
}

/// Write tests/acceptance.rs for a TOP-LEVEL module. Returns (rel_path, expected_failing).
pub fn write_acceptance(
    root: &std::path::Path,
    design: &Design,
    module_name: &str,
) -> Result<(String, usize), String> {
    let Some(module) = design.modules.iter().find(|m| m.name == module_name) else {
        return Err(format!(
            "module `{module_name}` not found in design.json (top-level modules only)"
        ));
    };
    let (content, reject) = render_acceptance(design, module);
    let rel = format!("crates/routes/{module_name}/tests/acceptance.rs");
    let path = root.join(&rel);
    std::fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
    std::fs::write(&path, &content).map_err(|e| e.to_string())?;
    // Enum reject tests (issue #47) and the #107 member-surface tests pass on
    // stubs, so they are NOT part of the RED-on-stubs baseline: exclude them
    // from `expected_failing`.
    Ok((rel, test_count(&content) - reject))
}

/// The outcome of generating every module's acceptance suite plus the jobs
/// suite — the shared core of the "all modules" path used by both the CLI's
/// bare `gen-tests` and the MCP `jerrycan_gen_tests` with no `module` (#159).
/// Each caller formats its own `next_step`/human envelope from these pieces.
pub struct AllAcceptance {
    /// Written suite paths, in order: one per endpoint-bearing module, then jobs.
    pub tests_created: Vec<String>,
    /// Aggregate expected-failing count (the jobs suite counted exactly once).
    pub expected_failing: usize,
    /// The cargo packages the suites live in: `route-{module}` …, then `jobs`.
    pub packages: Vec<String>,
    /// Whether the design declared jobs (a jobs suite was written).
    pub has_jobs: bool,
}

/// Generate one acceptance suite per endpoint-bearing top-level module, plus the
/// jobs suite once — the all-modules path shared by the CLI's bare `gen-tests`
/// and the MCP `jerrycan_gen_tests` with no `module`. Module selection mirrors
/// the JC0551 step (`checkpipe::missing_acceptance_tests`): a subroute's
/// endpoints count toward its parent (their tests live in the parent's crate),
/// so this clears every JC0551 the check can raise — including the jobs one on a
/// jobs-only design, which has no module name to pass. Each file is byte-identical
/// to what `write_acceptance` produces per module.
pub fn write_all_acceptance(
    root: &std::path::Path,
    design: &Design,
) -> Result<AllAcceptance, String> {
    fn endpoint_count(m: &ModuleDesign) -> usize {
        m.endpoints.len() + m.subroutes.iter().map(endpoint_count).sum::<usize>()
    }
    let mut tests_created: Vec<String> = Vec::new();
    let mut expected_failing = 0usize;
    let mut packages: Vec<String> = Vec::new();
    for m in design.modules.iter().filter(|m| endpoint_count(m) > 0) {
        let (rel, c) = write_acceptance(root, design, &m.name)?;
        tests_created.push(rel);
        expected_failing += c;
        packages.push(format!("route-{}", m.name));
    }
    // Jobs are top-level (not per-module): their suite is written ONCE, so its
    // count is added to the aggregate exactly once.
    let jobs = super::jobsgen::write_jobs_acceptance(root, design)?;
    let has_jobs = jobs.is_some();
    if let Some((jobs_rel, jobs_count)) = jobs {
        tests_created.push(jobs_rel);
        expected_failing += jobs_count;
        packages.push("jobs".to_string());
    }
    Ok(AllAcceptance {
        tests_created,
        expected_failing,
        packages,
        has_jobs,
    })
}

/// How many #[tokio::test] functions a generated file contains.
pub fn test_count(generated: &str) -> usize {
    generated.matches("#[tokio::test]").count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mig(name: &str, prefix: &str) -> MigrationItem {
        MigrationItem {
            name: format!("{name}_0001_create_tables"),
            sqlite_path: format!("{prefix}/migrations/sqlite/0001_create_tables.sql"),
            postgres_path: format!("{prefix}/migrations/postgres/0001_create_tables.sql"),
        }
    }

    /// Issue #221 (residual D): rustfmt HUGS a single-element `db.migrate(&[…])` array —
    /// the sole `Migration { … }` opens on the `db.migrate(&[` line and its body
    /// de-indents one level (fields at col 8) — but keeps a ≥ 2-element array multi-line
    /// (item body at col 8, fields at col 12). `migrate_call_block` must reproduce both,
    /// or the jobs AND route `tests/acceptance.rs` drift under `cargo fmt` for a single-
    /// route-module design. Empty items ⇒ the empty string (the jobs harness omits the
    /// route-migrate call entirely rather than emit `db.migrate(&[])`).
    #[test]
    fn migrate_call_block_hugs_single_element_and_expands_many() {
        let single = migrate_call_block(
            std::slice::from_ref(&mig("customers", "../../routes/customers")),
            "route migrations",
        );
        assert_eq!(
            single,
            "    db.migrate(&[jerrycan::db::Migration {\n        name: \"customers_0001_create_tables\",\n        sqlite: include_str!(\"../../routes/customers/migrations/sqlite/0001_create_tables.sql\"),\n        postgres: include_str!(\"../../routes/customers/migrations/postgres/0001_create_tables.sql\"),\n    }])\n    .await\n    .expect(\"route migrations\");\n"
        );
        // Two items ⇒ the pre-#221 multi-line layout (byte-identical to before).
        let two = migrate_call_block(&[mig("a", ".."), mig("b", "../../b")], "migrations");
        assert!(
            two.starts_with("    db.migrate(&[\n        jerrycan::db::Migration {\n"),
            "≥ 2 items stay multi-line: {two}"
        );
        assert!(
            two.contains("        },\n        jerrycan::db::Migration {\n"),
            "each of the two items sits at col 8: {two}"
        );
        // No items ⇒ omit the call.
        assert_eq!(migrate_call_block(&[], "migrations"), "");
    }

    /// Issue #217: an inline-DTO custom action (`request_body: {fields:[…]}`) whose
    /// required inline field carries a #80 constraint gets a boundary REJECT probe —
    /// it corrupts that field to an out-of-range value and asserts 422, so a
    /// regression that drops the inline `{Op}Request` validator turns the suite RED.
    /// The probe 422s before the handler, so it counts toward `reject` (subtracted
    /// from the RED-on-stubs baseline).
    #[test]
    fn inline_dto_constrained_action_gets_a_422_reject_probe() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "quantity", "type": "integer", "max": 100 } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "fixture must validate clean: {:?}",
            crate::platform::questions::validate(&d)
        );
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        // The reject probe exists, corrupts `quantity` to max+1 (101), and asserts 422.
        assert!(
            content.contains("async fn checkout_rejects_out_of_range_quantity()"),
            "inline constraint reject probe must be generated:\n{content}"
        );
        assert!(
            content.contains("\"quantity\": 101"),
            "the probe must send the out-of-range value (max + 1):\n{content}"
        );
        assert!(
            content.contains("as_u16(), 422"),
            "the probe must assert a 422 boundary reject:\n{content}"
        );
        // It 422s before the stub, so it is subtracted from expected_failing.
        assert_eq!(
            reject, 1,
            "the inline reject must count toward `reject` (excluded from RED-on-stubs):\n{content}"
        );
    }

    /// #236: an inline-DTO custom action on a `/{id}` path with NO creator to seed
    /// still gets BOTH its 422 reject probes (constraint + enum) — the inline 422
    /// precedes any id lookup, so it needs no seeded row. Before the fix this branch
    /// emitted an AGENT TODO only (`reject == 0`); a declared inline constraint on an
    /// `/{id}` action went UNVERIFIED by `check`.
    #[test]
    fn inline_reject_probes_emit_on_the_id_no_creator_branch() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "orders",
                "endpoints": [
                    { "operation_id": "apply", "method": "POST", "path": "/{id}/apply",
                      "request_body": { "fields": [
                          { "name": "amount", "type": "integer", "min": 1, "max": 100 },
                          { "name": "tier", "type": "string", "values": ["free", "pro"] } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "fixture must validate clean: {:?}",
            crate::platform::questions::validate(&d)
        );
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        // No creator at "/" ⇒ the un-seedable success probe is a TODO, but the two
        // reject probes (which need no seed) are now emitted.
        assert!(
            content.contains("async fn apply_rejects_out_of_range_amount()"),
            "the /{{id}} no-creator inline action must get its CONSTRAINT reject probe:\n{content}"
        );
        assert!(
            content.contains("async fn apply_rejects_out_of_range_tier()"),
            "the /{{id}} no-creator inline action must get its ENUM reject probe:\n{content}"
        );
        assert!(
            content.contains("as_u16(), 422"),
            "the probes must assert a 422 boundary reject:\n{content}"
        );
        // The success case is still a TODO (no creator), so it does NOT count; both
        // reject probes 422 before the stub, so they count toward `reject`.
        assert_eq!(
            reject, 2,
            "both inline reject probes count toward `reject` on the /{{id}} no-creator branch:\n{content}"
        );
    }

    /// Slice `content` to the body of the `#[tokio::test]` fn named `name`: from the
    /// `async fn {name}(` line to the next `#[tokio::test]` (or end). Test-only.
    fn section<'a>(content: &'a str, name: &str) -> &'a str {
        let needle = format!("async fn {name}(");
        let start = content
            .find(&needle)
            .unwrap_or_else(|| panic!("fn {name} not found in:\n{content}"));
        let rest = &content[start..];
        let end = rest[1..]
            .find("#[tokio::test]")
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        &rest[..end]
    }

    /// #266: a create's id-echo (`from_str(&res.text()).expect("json body")`) reads
    /// the response body, so it must be emitted ONLY for a body-bearing success
    /// status. A 204 (`NoContent`) or a 3xx (`Redirect`) has an EMPTY body — the
    /// echo would panic on `from_str("")`. A 200/201 create keeps the echo
    /// (byte-identical). Composes with the #263 `!list` gate.
    #[test]
    fn create_id_echo_only_for_a_body_bearing_status() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "orders-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "orders",
                "entities": [{ "name": "Order", "fields": [
                    { "name": "id", "type": "integer" },
                    { "name": "sku", "type": "string" } ]}],
                "endpoints": [
                    { "operation_id": "create_order", "method": "POST", "path": "/",
                      "request_body": { "entity": "Order" },
                      "success": { "status": 201, "entity": "Order" } },
                    { "operation_id": "create_order_redirect", "method": "POST", "path": "/redir",
                      "request_body": { "entity": "Order" },
                      "success": { "status": 303, "entity": "Order" } },
                    { "operation_id": "create_order_quiet", "method": "POST", "path": "/quiet",
                      "request_body": { "entity": "Order" },
                      "success": { "status": 204, "entity": "Order" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "fixture must validate clean: {:?}",
            crate::platform::questions::validate(&d)
        );
        let (content, _) = render_acceptance(&d, &d.modules[0]);
        // The 201 create keeps its id-echo (a JSON body to read).
        let ok = section(&content, "create_order_returns_201");
        assert!(
            ok.contains("echoes its id") && ok.contains("expect(\"json body\")"),
            "a 201 create keeps the id-echo (body-bearing):\n{ok}"
        );
        // The 303 create has an EMPTY body — NO id-echo (would panic on from_str("")).
        let redir = section(&content, "create_order_redirect_returns_303");
        assert!(
            !redir.contains("echoes its id") && !redir.contains("expect(\"json body\")"),
            "a 303 create must NOT id-echo (empty Redirect body):\n{redir}"
        );
        // The 204 create likewise has no body — NO id-echo.
        let quiet = section(&content, "create_order_quiet_returns_204");
        assert!(
            !quiet.contains("echoes its id") && !quiet.contains("expect(\"json body\")"),
            "a 204 create must NOT id-echo (empty NoContent body):\n{quiet}"
        );
    }

    /// #267: a reject probe on a PATH-SCOPED route of the tenant entity's OWN
    /// module — whose `app()` seeds NO membership — must PREPEND the same
    /// create-seed the 2xx probe uses, or the membership-verified `Dep<Tenant>`
    /// guard 404s (a non-member) BEFORE the body reaches the 422 validator. A
    /// tenant-owned CHILD module's reject probe is byte-identical (its `app()`
    /// pre-seeds the membership).
    #[test]
    fn tenant_root_pathscoped_reject_seeds_membership() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "tenant-api", "contract_version": 0,
            "auth": { "model": "jwt", "roles": ["owner", "member"] },
            "dependencies": ["db", "auth"],
            "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
            "modules": [
                { "name": "orgs",
                  "entities": [{ "name": "Org", "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "name", "type": "string", "max_len": 50 } ]}],
                  "endpoints": [
                      { "operation_id": "create_org", "method": "POST", "path": "/",
                        "auth_required": true,
                        "request_body": { "entity": "Org" },
                        "success": { "status": 201, "entity": "Org" } },
                      { "operation_id": "update_org", "method": "PUT", "path": "/{id}",
                        "auth_required": true,
                        "request_body": { "entity": "Org" },
                        "success": { "status": 200, "entity": "Org" } } ] },
                { "name": "notes",
                  "entities": [{ "name": "Note",
                      "belongs_to": [{ "entity": "Org", "on_delete": "cascade" }],
                      "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "body", "type": "string", "max_len": 50 } ]}],
                  "endpoints": [
                      { "operation_id": "create_note", "method": "POST", "path": "/",
                        "auth_required": true,
                        "request_body": { "entity": "Note" },
                        "success": { "status": 201, "entity": "Note" } },
                      { "operation_id": "update_note", "method": "PUT", "path": "/{id}",
                        "auth_required": true,
                        "request_body": { "entity": "Note" },
                        "success": { "status": 200, "entity": "Note" } } ] }
            ]
        }"#,
        )
        .unwrap();
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "fixture must validate clean: {:?}",
            crate::platform::questions::validate(&d)
        );
        // The tenant entity's OWN module: the update reject probe must seed
        // membership by prepending the create-seed (`// seed id 1`) before the
        // reject request, so the guard finds a member and the body reaches 422.
        let (orgs, _) = render_acceptance(&d, &d.modules[0]);
        let reject = section(&orgs, "update_org_rejects_out_of_range_name");
        assert!(
            reject.contains("// seed id 1") && reject.contains("post_json_with"),
            "the tenant-root path-scoped reject must PREPEND the membership seed:\n{reject}"
        );
        assert!(
            reject.contains("as_u16(), 422"),
            "the reject still asserts 422:\n{reject}"
        );
        // A tenant-owned CHILD module pre-seeds membership in app(), so its reject
        // probe is byte-identical to before — NO extra create-seed inside the probe.
        let (notes, _) = render_acceptance(&d, &d.modules[1]);
        let child = section(&notes, "update_note_rejects_out_of_range_body");
        assert!(
            !child.contains("// seed id 1"),
            "a child module's reject probe must NOT gain a seed (app() pre-seeds):\n{child}"
        );
    }

    /// Issue #217: an inline-DTO action whose required field carries a
    /// non-defaulted enum `values` set gets an out-of-range enum reject probe
    /// (the sentinel) asserting 422.
    #[test]
    fn inline_dto_enum_action_gets_a_422_reject_probe() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "tier", "type": "string", "values": ["free", "pro"] } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        assert!(
            content.contains("async fn checkout_rejects_out_of_range_tier()"),
            "inline enum reject probe must be generated:\n{content}"
        );
        assert!(
            content.contains(ENUM_REJECT_SENTINEL),
            "the enum probe must send the out-of-range sentinel:\n{content}"
        );
        assert!(
            content.contains("as_u16(), 422"),
            "must assert 422:\n{content}"
        );
        assert_eq!(reject, 1, "the inline enum reject counts toward `reject`");
    }

    /// Issue #217 (skip-when-unrejectable): an inline-DTO action whose fields carry
    /// NO #80 constraint and NO non-defaulted enum emits the happy-path test but NO
    /// reject probe — a probe would assert a 422 the validator never produces. The
    /// output is byte-identical to the pre-#217 generator for such designs.
    #[test]
    fn inline_dto_unconstrained_action_emits_no_reject_probe() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "note", "type": "string" } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        // The action IS exercised (happy path present) but nothing is rejectable.
        assert!(
            content.contains("async fn checkout_returns_200()"),
            "the inline happy-path test must still be generated:\n{content}"
        );
        assert!(
            !content.contains("_rejects_out_of_range_"),
            "no reject probe when no inline field is rejectable:\n{content}"
        );
        assert_eq!(reject, 0, "no inline reject ⇒ no `reject` contribution");
    }

    /// An entity-body endpoint's reject machinery is unchanged by #217: it still
    /// emits its own constraint reject probe (proving the inline path is additive,
    /// not a replacement).
    #[test]
    fn entity_body_reject_probe_is_unchanged() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "items",
                "entities": [{ "name": "Item", "fields": [
                    { "name": "id", "type": "integer" },
                    { "name": "qty", "type": "integer", "max": 100 } ] }],
                "endpoints": [
                    { "operation_id": "create_item", "method": "POST", "path": "/",
                      "request_body": { "entity": "Item" },
                      "success": { "status": 201, "entity": "Item" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let content = acceptance_rs(&d, &d.modules[0]);
        assert!(
            content.contains("async fn create_item_rejects_out_of_range_qty()"),
            "entity-body reject probe still fires:\n{content}"
        );
    }

    /// Issue #225 Gap A: an inline-DTO action carrying BOTH a #80-constrained field
    /// AND a #47 enum field gets BOTH reject probes independently — the pre-#225 XOR
    /// (`if constraint … else if enum`) dropped the enum probe whenever a constrained
    /// field existed, exactly the entity path never did. Both count toward `reject`.
    #[test]
    fn inline_dto_constraint_and_enum_action_gets_both_reject_probes() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "quantity", "type": "integer", "min": 1, "max": 100 },
                          { "name": "tier", "type": "string", "values": ["free", "pro", "enterprise"] } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "fixture must validate clean: {:?}",
            crate::platform::questions::validate(&d)
        );
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        assert!(
            content.contains("async fn checkout_rejects_out_of_range_quantity()"),
            "the constraint reject probe must fire (Gap A):\n{content}"
        );
        assert!(
            content.contains("async fn checkout_rejects_out_of_range_tier()"),
            "the enum reject probe must ALSO fire (Gap A — no XOR):\n{content}"
        );
        assert_eq!(
            reject, 2,
            "both inline rejects must count toward `reject`:\n{content}"
        );
    }

    /// Issue #225 Gap B: an inline-DTO action whose ENUM field is OPTIONAL still gets
    /// a reject probe — the pre-#225 `f.required` gate skipped it, and even without
    /// the gate the corrupted value must be ON THE WIRE. `inline_fixture_json` now
    /// carries the overridden optional field, so the sentinel reaches the validator.
    #[test]
    fn inline_dto_optional_enum_field_gets_a_reject_probe() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "note", "type": "string" },
                          { "name": "tier", "type": "string", "required": false, "values": ["free", "pro"] } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            crate::platform::questions::validate(&d).is_empty(),
            "fixture must validate clean: {:?}",
            crate::platform::questions::validate(&d)
        );
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        assert!(
            content.contains("async fn checkout_rejects_out_of_range_tier()"),
            "the OPTIONAL enum field must be probed (Gap B):\n{content}"
        );
        // The corrupted optional field must appear on the wire (else no 422).
        assert!(
            content.contains(&format!("\"tier\": \"{ENUM_REJECT_SENTINEL}\"")),
            "the reject body must carry the corrupted optional field:\n{content}"
        );
        assert_eq!(reject, 1, "the optional-enum reject counts toward `reject`");
    }

    /// Issue #225 Gap B (constraint twin): an inline-DTO action whose #80-constrained
    /// field is OPTIONAL still gets a reject probe carrying the out-of-range value.
    #[test]
    fn inline_dto_optional_constrained_field_gets_a_reject_probe() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "note", "type": "string" },
                          { "name": "amount", "type": "integer", "required": false, "max": 100 } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let (content, reject) = render_acceptance(&d, &d.modules[0]);
        assert!(
            content.contains("async fn checkout_rejects_out_of_range_amount()"),
            "the OPTIONAL constrained field must be probed (Gap B):\n{content}"
        );
        assert!(
            content.contains("\"amount\": 101"),
            "the reject body must carry the out-of-range optional value (max + 1):\n{content}"
        );
        assert_eq!(
            reject, 1,
            "the optional-constraint reject counts toward `reject`"
        );
    }

    /// Issue #225: the HAPPY-path inline body stays required-only (byte-identical to
    /// the pre-#225 emission) — only the REJECT body gains the optional field it
    /// corrupts. Tests `inline_fixture_json` directly on both call shapes.
    #[test]
    fn inline_fixture_happy_path_stays_required_only() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "shop-api", "contract_version": 0,
            "dependencies": [],
            "modules": [{
                "name": "checkout",
                "endpoints": [
                    { "operation_id": "checkout", "method": "POST", "path": "/",
                      "request_body": { "fields": [
                          { "name": "note", "type": "string" },
                          { "name": "tier", "type": "string", "required": false, "values": ["free", "pro"] } ] },
                      "success": { "status": 200 } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let rb = d.modules[0].endpoints[0]
            .request_body
            .as_ref()
            .expect("inline body");
        // Happy path (no overrides): required-only — the optional `tier` is absent.
        assert_eq!(
            inline_fixture_json(&rb.fields, &[]),
            "{\"note\": \"test-value\"}",
            "the happy-path body must stay required-only (byte-identical)"
        );
        // Reject path: an override on the optional field pulls it onto the wire.
        assert_eq!(
            inline_fixture_json(&rb.fields, &[("tier", "\"__invalid_enum_value__\"")]),
            "{\"note\": \"test-value\", \"tier\": \"__invalid_enum_value__\"}",
            "the reject body must carry the corrupted optional field"
        );
    }
}
