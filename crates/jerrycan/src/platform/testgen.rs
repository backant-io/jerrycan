//! design module → tests/acceptance.rs (TOOL-owned). One success test per
//! endpoint, one test per generatable error case (404 on parameterized paths),
//! an AGENT TODO comment for the rest. Stubs fail everything (expected_failing
//! = test_count) — green = the design contract is implemented.

use super::design::*;

/// The JSON request-body fixture literal for a field. Enum fields (those with a
/// declared `values` set) use their FIRST declared value, so the generated
/// happy-path body satisfies the migration's `CHECK (... IN (...))` constraint
/// instead of tripping it with `"test-value"` (an opaque `JC0510` at run time).
/// Mirrors `seed_sql_value` on the SQL seed side — the two must agree.
fn fixture_value(f: &Field) -> String {
    if let Some(first) = f.values.as_ref().and_then(|v| v.first()) {
        return format!("\"{first}\"");
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
    bad_enum: Option<&str>,
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
        .filter(|b| !(omit_identity_fk && Design::is_identity_fk(b)))
        .filter(|b| !path_fks.contains(&Design::fk_column(&b.entity)))
        .map(|b| {
            format!(
                "\"{}\": {}",
                Design::fk_column(&b.entity),
                fk_fixture_value(design, &b.entity)
            )
        });
    // A `default` field (issue #53a) is server-owned: the probe omits it so the
    // minimal client body proves the server applies the default (not a 422).
    let cols = e.fields.iter().filter(|f| f.default.is_none()).map(|f| {
        // The reject probe (issue #47) corrupts ONE enum field to an out-of-range
        // sentinel; every other field keeps its valid fixture value so the ONLY
        // reason for a 422 is that enum.
        let value = if bad_enum == Some(f.name.as_str()) {
            format!("\"{ENUM_REJECT_SENTINEL}\"")
        } else {
            fixture_value(f)
        };
        format!("\"{}\": {}", f.name, value)
    });
    let fields = fks.chain(cols).collect::<Vec<_>>().join(", ");
    format!("{{{fields}}}")
}

/// The POST creator (with a body) mounted at a bare collection `path` — the route
/// that seeds a row addressable under `path/{id}`. `creator_at(m, "/")` is the
/// module-root creator; `creator_at(m, "/tasks")` seeds the second entity (#51).
fn creator_at<'a>(m: &'a ModuleDesign, path: &str) -> Option<&'a Endpoint> {
    m.endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::POST && ep.path == path && ep.request_body.is_some())
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
                .is_some_and(|rb| rb.entity == entity)
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
        &creator
            .request_body
            .as_ref()
            .expect("creator has body")
            .entity,
        omits_identity_fk(design, unit, creator),
        None,
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
    unit: &ModuleDesign,
    base: &str,
    ep: &Endpoint,
    auth: bool,
) -> Option<(String, String)> {
    let coll = collection_path(ep);
    let creator = creator_at(unit, &coll)?;
    let entity_name = &creator
        .request_body
        .as_ref()
        .expect("creator has body")
        .entity;
    let entity = unit.entities.iter().find(|e| &e.name == entity_name)?;

    let mut seed = String::new();
    let mut seen = vec![entity.name.clone()];
    seed_parents(design, unit, base, entity, auth, &mut seed, &mut seen);
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
/// handler injects it), the tenancy entity (already seeded), a parent with no
/// creator in this module (a cross-module target is an UNENFORCED relation — the
/// fk fixture's `1` needs no row), and already-seeded entities (cycle guard).
fn seed_parents(
    design: &Design,
    unit: &ModuleDesign,
    base: &str,
    entity: &Entity,
    auth: bool,
    seed: &mut String,
    seen: &mut Vec<String>,
) {
    let tenancy = design.tenancy.as_ref().map(|t| t.entity.as_str());
    for b in &entity.belongs_to {
        if Design::is_identity_fk(b)
            || Some(b.entity.as_str()) == tenancy
            || seen.contains(&b.entity)
        {
            continue;
        }
        let (Some(parent_creator), Some(parent)) = (
            creator_for_entity(unit, &b.entity),
            unit.entities.iter().find(|e| e.name == b.entity),
        ) else {
            continue;
        };
        seen.push(b.entity.clone());
        seed_parents(design, unit, base, parent, auth, seed, seen);
        seed.push_str(&seed_line(
            design,
            unit,
            &collection_url(base, &parent_creator.path),
            parent_creator,
            auth,
            &format!("// seed parent {} id 1", parent.name),
        ));
    }
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

/// A request expression `t.<verb>(...)`. In auth mode a guarded endpoint threads
/// the test cookie via the `_with` helper variants; otherwise the plain verb.
fn request_expr(
    design: &Design,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded_and_auth: bool,
    bad_enum: Option<&str>,
) -> String {
    let body = || {
        ep.request_body
            .as_ref()
            // The omission keys on the ENDPOINT being guarded (the design-level
            // rule), not on whether THIS request threads a cookie — a guarded
            // endpoint's 401 probe still sends the guarded body shape.
            .map(|rb| {
                fixture_json(
                    design,
                    unit,
                    &rb.entity,
                    omits_identity_fk(design, unit, ep),
                    bad_enum,
                )
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

fn unit_tests(design: &Design, unit: &ModuleDesign, base: &str, out: &mut TestOut) {
    let auth = out.auth;

    for ep in &unit.endpoints {
        let full_path = format!("{}{}", base.trim_end_matches('/'), ep.path);
        let fn_base = &ep.operation_id;
        let status = ep.success.status;
        let guarded = auth && ep.is_guarded();
        // Endpoints whose success needs a credential/signature the generator can't
        // supply (login, signed webhook, api-key route): no un-greenable success
        // probe — emit a TODO instead. Detected by heuristic OR declared
        // explicitly with `probe: skip` (issue #11) so a design the heuristic
        // misses can still reach `ok:true`. These are never session-guarded (a
        // guard would be threaded), so `guarded` is false and no 401 test is emitted.
        let probe_skip = ep.probe == ProbePolicy::Skip;
        let gated = endpoint_is_credential_gated(ep) || probe_skip;

        if gated {
            let reason = if probe_skip {
                "is marked `probe: skip` — the generator can't synthesize a credential for its success"
            } else {
                "authenticates via a credential/signature the generator can't supply"
            };
            out.todos.push(format!(
                "// AGENT TODO: {fn_base} ({:?} {full_path}) {reason} — write its success test (with a valid credential) and its 401/403 rejection test in your own test file.",
                ep.method
            ));
        } else if param_count(ep) == 0 {
            let request = request_expr(design, unit, ep, &full_path, guarded, None);
            // A creator that echoes its entity must echo the id it was given —
            // catches inserts that return a backend default (0) instead.
            let id_echo = (ep.method == HttpMethod::POST)
                .then_some(ep.request_body.as_ref())
                .flatten()
                .filter(|rb| ep.success.entity.as_deref() == Some(rb.entity.as_str()))
                .and_then(|rb| unit.entities.iter().find(|e| e.name == rb.entity))
                .and_then(|e| e.fields.iter().find(|f| f.name == "id"))
                .map(|f| format!(
                    "    let body: serde_json::Value = serde_json::from_str(&res.text()).expect(\"json body\");\n    assert_eq!(body[\"id\"], serde_json::json!({}), \"design: created {} echoes its id\");\n",
                    fixture_value(f), ep.success.entity.as_deref().unwrap_or("entity")
                ))
                .unwrap_or_default();
            out.code.push_str(&format!(
                "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n{id_echo}}}\n\n"
            ));
            out.count += 1;
            if guarded {
                push_401_test(design, out, unit, ep, &full_path, false);
            }
            // Issue #47: an enum request body gets an out-of-range reject probe.
            if let Some(field) = ep
                .request_body
                .as_ref()
                .and_then(|rb| first_enum_field(unit, &rb.entity))
            {
                push_enum_reject_test(design, out, unit, ep, &full_path, guarded, field);
            }
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
        } else if param_count(ep) == 1 {
            // Issue #51: seed the row THIS `/{id}` endpoint addresses via ITS OWN
            // entity's creator (`POST /tasks` for `/tasks/{id}`), walking belongs_to
            // parents first — not the module-root creator, which would seed the
            // wrong entity and make the probe 404 on a CORRECT handler.
            if let Some((seed, seed_id)) = seed_for_id_probe(design, unit, base, ep, auth) {
                let seeded_path = full_path.replacen(&regex_free_param(&ep.path), &seed_id, 1);
                let request = request_expr(design, unit, ep, &seeded_path, guarded, None);
                out.code.push_str(&format!(
                    "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n}}\n\n"
                ));
                out.count += 1;
                if guarded {
                    push_401_test(design, out, unit, ep, &seeded_path, true);
                }
                // Issue #47: update path (PUT/PATCH /{id}) rejects out-of-range too.
                if let Some(field) = ep
                    .request_body
                    .as_ref()
                    .and_then(|rb| first_enum_field(unit, &rb.entity))
                {
                    push_enum_reject_test(design, out, unit, ep, &seeded_path, guarded, field);
                }
            } else {
                out.todos.push(format!(
                    "// AGENT TODO: {fn_base} ({:?} {full_path}) has no creator route to seed its {{id}} — encode its success case in your own test file.",
                    ep.method
                ));
            }
        } else if param_count(ep) >= 1 {
            out.todos.push(format!(
                "// AGENT TODO: {fn_base} ({:?} {full_path}) needs a creator at \"/\" to seed ids — encode its success case in your own test file.",
                ep.method
            ));
        }

        for ec in &ep.errors {
            if ec.status == 404 && param_count(ep) == 1 && !gated {
                let missing_path = full_path.replacen(&regex_free_param(&ep.path), "999999", 1);
                // Build the probe with the endpoint's REAL method (and body/cookie)
                // via the same builder the success test uses — a GET probe at a
                // POST-only `/{id}` action would hit 405, not the 404 we assert.
                // Guarded endpoints run the auth guard before not-found logic, so
                // `request_expr` threads the cookie when guarded.
                let request = request_expr(design, unit, ep, &missing_path, guarded, None);
                out.code.push_str(&format!(
                    "#[tokio::test]\nasync fn {fn_base}_missing_id_is_404() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), 404, \"design: {fn_base} lists 404 ({when}); body: {{}}\", res.text());\n}}\n\n",
                    when = ec.when
                ));
                out.count += 1;
            } else {
                out.todos.push(format!(
                    "// AGENT TODO: design lists {} ({}) for {fn_base} — encode it in your own test file.",
                    ec.status, ec.when
                ));
            }
        }
    }

    for sub in &unit.subroutes {
        let sub_base = format!("{}{}", base, sub.effective_mount());
        unit_tests(design, sub, &sub_base, out);
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
    let request = request_expr(design, unit, ep, path, false, None); // no cookie
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
fn push_enum_reject_test(
    design: &Design,
    out: &mut TestOut,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded: bool,
    field: &str,
) {
    let fn_base = &ep.operation_id;
    let request = request_expr(design, unit, ep, path, guarded, Some(field));
    out.code.push_str(&format!(
        "#[tokio::test]\nasync fn {fn_base}_rejects_out_of_range_{field}() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), 422, \"design: out-of-range `{field}` enum must 422 at the request boundary, not 500 at the DB CHECK; body: {{}}\", res.text());\n}}\n\n"
    ));
    out.count += 1;
    out.reject += 1;
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
    let Some(tenancy) = design.tenancy.as_ref() else {
        return false;
    };
    fn walk(m: &ModuleDesign, tenant: &str) -> bool {
        m.entities
            .iter()
            .any(|e| e.belongs_to.iter().any(|b| b.entity == tenant))
            || m.subroutes.iter().any(|s| walk(s, tenant))
    }
    walk(module, &tenancy.entity)
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
fn collect_workspace_migration_items(design: &Design, current: &ModuleDesign, out: &mut String) {
    for m in &design.modules {
        let prefix = if m.name == current.name {
            "..".to_string()
        } else {
            format!("../../{}", m.name)
        };
        let m_snake = m.name.replace('-', "_");
        if !m.entities.is_empty() {
            out.push_str(&format!(
                "        jerrycan::db::Migration {{\n            name: \"{m_snake}_0001_create_tables\",\n            sqlite: include_str!(\"{prefix}/migrations/sqlite/0001_create_tables.sql\"),\n            postgres: include_str!(\"{prefix}/migrations/postgres/0001_create_tables.sql\"),\n        }},\n"
            ));
        }
        collect_subroute_migration_items(m, &m_snake, &prefix, out);
    }
}

/// Subroute create-tables migrations for one top-level module (recursive). A
/// subroute's file lives in its TOP module's migrations dir as
/// `0001_create_tables_{sub}.sql`; its name is namespaced by the top module so
/// two modules' like-named subroutes never collide in the workspace list.
fn collect_subroute_migration_items(
    module: &ModuleDesign,
    top_snake: &str,
    prefix: &str,
    out: &mut String,
) {
    for sub in &module.subroutes {
        if !sub.entities.is_empty() {
            let s = sub.name.replace('-', "_");
            out.push_str(&format!(
                "        jerrycan::db::Migration {{\n            name: \"{top_snake}_0001_create_tables_{s}\",\n            sqlite: include_str!(\"{prefix}/migrations/sqlite/0001_create_tables_{s}.sql\"),\n            postgres: include_str!(\"{prefix}/migrations/postgres/0001_create_tables_{s}.sql\"),\n        }},\n"
            ));
        }
        collect_subroute_migration_items(sub, top_snake, prefix, out);
    }
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
    let role = tenancy
        .member_roles
        .first()
        .map(String::as_str)
        .unwrap_or("owner");

    // Columns + values for the tenant row: id = 1 (so the fk resolves), then each
    // declared non-id field with a seed-safe fixture (enum fields use a declared
    // value to satisfy the CHECK constraint). Column identifiers are double-quoted
    // in the SQL; since the whole statement is a Rust string literal, those quotes
    // are escaped (`\\\"`) so the generated source stays valid.
    let (cols, vals) = tenant_row_cols_vals(entity, "1", 1);
    format!(
        "    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols}) VALUES ({vals})\")\n        .await\n        .expect(\"seed tenant row\");\n    db.conn()\n        .execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (1, 1, '{role}')\")\n        .await\n        .expect(\"seed membership\");\n"
    )
}

/// The membership role to seed for the second tenant's user: the role a
/// role-gated DELETE on this module requires (so the isolation DELETE leg clears
/// the role check and exercises the SCOPED `remove_for` — proving cross-tenant
/// isolation, not a 403 role rejection), falling back to the first member role.
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
        .unwrap_or("owner")
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
fn isolation_test(design: &Design, module: &ModuleDesign) -> String {
    let Some(tenancy) = design.tenancy.as_ref() else {
        return String::new();
    };
    // The tenant-owned entity declared directly on this module (subroute-nested
    // tenant entities are out of scope — their isolation is the agent's to test).
    let Some(entity) = module
        .entities
        .iter()
        .find(|e| e.belongs_to.iter().any(|b| b.entity == tenancy.entity))
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
                .is_some_and(|rb| rb.entity == entity.name)
    }) else {
        return String::new();
    };
    let base = module.effective_mount();
    let base = base.trim_end_matches('/');
    let plural = module.name.replace('-', "_");
    let body = fixture_json(
        design,
        module,
        &entity.name,
        omits_identity_fk(design, module, create),
        None,
    );
    let create_path = format!("{base}/");

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
    let list = module
        .endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::GET && param_count(ep) == 0);

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
        "    let created = t.post_json_with(\"{create_path}\", &serde_json::json!({body}), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n    let cookie2 = test_cookie_for(2);\n",
        status = create.success.status,
        entity = entity.name,
    ));
    // The list negative-control compares the created id as a JSON Value.
    if list.is_some() {
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
            "    let foreign = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-tenant get must 404 (use get_for, not get); body: {{}}\", foreign.text());\n",
        ));
    }
    if list.is_some() {
        // Always cookied: even an unguarded list is safe to call with a cookie,
        // and a guarded one needs it. user 2 sees only tenant 2's (empty) rows.
        t.push_str(&format!(
            "    let listed = t.get_with(\"{base}/\", &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(listed.status().as_u16(), 200, \"user 2 lists their own {plural}; body: {{}}\", listed.text());\n    let rows: serde_json::Value = serde_json::from_str(&listed.text()).expect(\"list json\");\n    let absent = rows.as_array().map(|a| a.iter().all(|r| r[\"id\"] != id_value)).unwrap_or(true);\n    assert!(absent, \"cross-tenant list must NOT contain tenant 1's row (use all_for); body: {{}}\", listed.text());\n",
        ));
    }
    if let Some(_del) = delete_one {
        t.push_str(&format!(
            "    let del = t.delete_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &cookie2)]).await;\n    assert_eq!(del.status().as_u16(), 404, \"cross-tenant delete must 404 (use remove_for, not remove); body: {{}}\", del.text());\n",
        ));
        if get_one.is_some() {
            t.push_str(&format!(
                "    let survives = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"{hk}\", &test_cookie_for(1))]).await;\n    assert_eq!(survives.status().as_u16(), 200, \"tenant 1's row must survive a cross-tenant delete; body: {{}}\", survives.text());\n",
            ));
        }
    }
    t.push_str("}\n\n");
    t
}

/// A SQL literal for seeding a tenant-row column. Enum fields use their first
/// declared value (so a CHECK constraint passes); other fields use a type-shaped
/// literal. String/text literals are single-quoted for inline DDL execution.
fn seed_sql_value(f: &Field) -> String {
    if let Some(values) = &f.values
        && let Some(first) = values.first()
    {
        return format!("'{first}'");
    }
    match f.field_type {
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
    if let Some(values) = &f.values
        && let Some(first) = values.first()
    {
        return format!("'{first}'");
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
        // Resolves `Dep<RealtimeHandle>` for realtime handlers (no JC1001). The
        // base extension is enough for the harness — stub probes never publish, so
        // the design's channels (wired in the realtime crate at serve time) aren't
        // needed here.
        extends.push_str(".extend(jerrycan::realtime::Realtime::new(db.clone()))");
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

fn preamble(design: &Design, module: &ModuleDesign, uses_cookies: bool) -> String {
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
        let mut migration_items = String::new();
        collect_workspace_migration_items(design, module, &mut migration_items);
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
        // in scope; only import it when there's a seed (else `-D warnings` trips).
        let seed_use = if seed.is_empty() {
            String::new()
        } else {
            "use jerrycan::db::sea_orm::ConnectionTrait;\n\n".to_string()
        };
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
            "{seed_use}{auth_login}{second_seed_fn}{ext_comment}async fn app() -> TestApp {{\n    let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n    db.migrate(&[\n{migration_items}    ])\n    .await\n    .expect(\"migrations\");\n{seed}{second_seed_call}    App::new(){auth_extend}{ext_extends}.extend(db){tenant_dep}.mount(\"{mount}\", module()).into_test()\n}}\n"
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

/// Renders the acceptance file AND the count of enum "reject" tests (issue #47)
/// that PASS on stubs — `write_acceptance` subtracts these from `expected_failing`
/// so the RED-on-stubs baseline stays exact.
fn render_acceptance(design: &Design, module: &ModuleDesign) -> (String, usize) {
    let mut out = TestOut {
        code: String::new(),
        todos: Vec::new(),
        count: 0,
        reject: 0,
        auth: design.wants_auth(),
    };
    unit_tests(design, module, &module.effective_mount(), &mut out);
    // Cross-tenant isolation: the security contract for tenant-owned modules.
    // Appended after the per-endpoint tests; counts toward expected_failing
    // (it fails on stubs like every other generated test).
    let isolation = isolation_test(design, module);
    out.count += isolation.matches("#[tokio::test]").count();
    out.code.push_str(&isolation);
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
        preamble = preamble(design, module, uses_cookies),
        code = out.code,
    );
    (content, out.reject)
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
    // Enum reject tests (issue #47) pass on stubs, so they are NOT part of the
    // RED-on-stubs baseline: exclude them from `expected_failing`.
    Ok((rel, test_count(&content) - reject))
}

/// How many #[tokio::test] functions a generated file contains.
pub fn test_count(generated: &str) -> usize {
    generated.matches("#[tokio::test]").count()
}
