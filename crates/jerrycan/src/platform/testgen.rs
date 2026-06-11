//! design module → tests/acceptance.rs (TOOL-owned). One success test per
//! endpoint, one test per generatable error case (404 on parameterized paths),
//! an AGENT TODO comment for the rest. Stubs fail everything (expected_failing
//! = test_count) — green = the design contract is implemented.

use super::design::*;

fn fixture_value(t: FieldType) -> &'static str {
    match t {
        FieldType::String => "\"test-value\"",
        FieldType::Integer => "1",
        FieldType::Float => "1.0",
        FieldType::Boolean => "false",
        FieldType::Datetime => "\"2026-01-01T00:00:00Z\"",
        FieldType::Uuid => "\"00000000-0000-0000-0000-000000000000\"",
        FieldType::Json => "{}",
    }
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

fn fixture_json(design: &Design, m: &ModuleDesign, entity: &str) -> String {
    let Some(e) = m.entities.iter().find(|e| e.name == entity) else {
        return "{}".to_string();
    };
    // belongs_to fk columns first: a tenant-owned entity's body must carry the
    // fk (NOT NULL) so the handler's Json<Entity> deserializes (else 422 before
    // the stub), valued at the seeded tenant so a scoped query can resolve it.
    let fks = e.belongs_to.iter().map(|b| {
        format!(
            "\"{}\": {}",
            Design::fk_column(&b.entity),
            fk_fixture_value(design, &b.entity)
        )
    });
    let cols = e
        .fields
        .iter()
        .map(|f| format!("\"{}\": {}", f.name, fixture_value(f.field_type)));
    let fields = fks.chain(cols).collect::<Vec<_>>().join(", ");
    format!("{{{fields}}}")
}

/// The module's root creator (POST with body at "/"), used to seed id 1.
fn creator(m: &ModuleDesign) -> Option<&Endpoint> {
    m.endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::POST && ep.path == "/" && ep.request_body.is_some())
}

fn param_count(ep: &Endpoint) -> usize {
    ep.path.matches('{').count()
}

struct TestOut {
    code: String,
    todos: Vec<String>,
    count: usize,
    /// Auth mode: success tests on guarded endpoints carry a session cookie and
    /// every guarded endpoint also gets a no-cookie 401 test.
    auth: bool,
}

/// A request expression `t.<verb>(...)`. In auth mode a guarded endpoint threads
/// the test cookie via the `_with` helper variants; otherwise the plain verb.
fn request_expr(
    design: &Design,
    unit: &ModuleDesign,
    ep: &Endpoint,
    path: &str,
    guarded_and_auth: bool,
) -> String {
    let body = || {
        ep.request_body
            .as_ref()
            .map(|rb| fixture_json(design, unit, &rb.entity))
            .unwrap_or_else(|| "{}".to_string())
    };
    if guarded_and_auth {
        let cookie = "&[(\"cookie\", &test_cookie())]";
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
    // The path-param value a seeded row answers to: the fixture id for entities
    // that declare one (text pks seed their fixture string), "1" otherwise
    // (matching the synthetic/integer pk the creator's fixture inserts).
    let seed_id = creator(unit)
        .and_then(|ep| ep.request_body.as_ref())
        .and_then(|rb| unit.entities.iter().find(|e| e.name == rb.entity))
        .and_then(|e| e.fields.iter().find(|f| f.name == "id"))
        .map(|f| fixture_value(f.field_type).trim_matches('"'))
        .unwrap_or("1");
    let seed = creator(unit).map(|ep| {
        let body = fixture_json(
            design,
            unit,
            &ep.request_body.as_ref().expect("creator has body").entity,
        );
        // In auth mode a guarded creator needs the cookie to seed successfully.
        if auth && ep.is_guarded() {
            format!(
                "    t.post_json_with(\"{base}/\", &serde_json::json!({body}), &[(\"cookie\", &test_cookie())]).await; // seed id 1\n"
            )
        } else {
            format!("    t.post_json(\"{base}/\", &serde_json::json!({body})).await; // seed id 1\n")
        }
    });

    for ep in &unit.endpoints {
        let full_path = format!("{}{}", base.trim_end_matches('/'), ep.path);
        let fn_base = &ep.operation_id;
        let status = ep.success.status;
        let guarded = auth && ep.is_guarded();

        if param_count(ep) == 0 {
            let request = request_expr(design, unit, ep, &full_path, guarded);
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
                    fixture_value(f.field_type), ep.success.entity.as_deref().unwrap_or("entity")
                ))
                .unwrap_or_default();
            out.code.push_str(&format!(
                "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n{id_echo}}}\n\n"
            ));
            out.count += 1;
            if guarded {
                push_401_test(design, out, unit, ep, &full_path, false);
            }
        } else if param_count(ep) == 1 && seed.is_some() {
            let seeded_path = full_path.replacen(&regex_free_param(&ep.path), seed_id, 1);
            let request = request_expr(design, unit, ep, &seeded_path, guarded);
            out.code.push_str(&format!(
                "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n}}\n\n",
                seed = seed.as_deref().unwrap_or("")
            ));
            out.count += 1;
            if guarded {
                push_401_test(design, out, unit, ep, &seeded_path, seed.is_some());
            }
        } else if param_count(ep) >= 1 {
            out.todos.push(format!(
                "// AGENT TODO: {fn_base} ({:?} {full_path}) needs a creator at \"/\" to seed ids — encode its success case in your own test file.",
                ep.method
            ));
        }

        for ec in &ep.errors {
            if ec.status == 404 && param_count(ep) == 1 {
                let missing_path = full_path.replacen(&regex_free_param(&ep.path), "999999", 1);
                // Guarded endpoints run the auth guard before not-found logic, so
                // a credential-less request would 401; thread the cookie here.
                let request = if guarded {
                    match ep.method {
                        HttpMethod::DELETE => format!(
                            "t.delete_with(\"{missing_path}\", &[(\"cookie\", &test_cookie())]).await"
                        ),
                        _ => format!(
                            "t.get_with(\"{missing_path}\", &[(\"cookie\", &test_cookie())]).await"
                        ),
                    }
                } else {
                    match ep.method {
                        HttpMethod::DELETE => format!("t.delete(\"{missing_path}\").await"),
                        _ => format!("t.get(\"{missing_path}\").await"),
                    }
                };
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
    let request = request_expr(design, unit, ep, path, false); // no cookie
    out.code.push_str(&format!(
        "#[tokio::test]\nasync fn {fn_base}_without_auth_is_401() {{\n    let t = app().await;\n    let res = {request};\n    assert_eq!(res.status().as_u16(), 401, \"design: {fn_base} is guarded — no cookie must 401; body: {{}}\", res.text());\n}}\n\n"
    ));
    out.count += 1;
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

/// In auth mode: a test-only login shim that mints a session cookie directly via
/// the `Auth` extension (no app `/login` route needed), plus the `.extend(Auth)`
/// the app() helper adds so the SAME secret decrypts the cookie. `test_cookie_for`
/// mints a cookie for any user id (isolation tests act as a second user);
/// `test_cookie()` keeps minting user 1's for back-compat with the success tests.
fn auth_preamble_login() -> String {
    format!(
        "fn test_cookie_for(user_id: i64) -> String {{\n    let auth = jerrycan::auth::Auth::with_secret(\"{TEST_SECRET}\");\n    let token = auth.sessions().encode(&shared::SessionUser {{ id: user_id, role: \"admin\".into() }}).expect(\"encode\");\n    format!(\"jerrycan_session={{token}}\")\n}}\n\nfn test_cookie() -> String {{\n    test_cookie_for(1)\n}}\n\n"
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

/// A `migrate` entry for the tenant module's tables, referenced from THIS test
/// crate (cross-crate relative include) so the `{tenant}_members` table the
/// `tenant` guard queries exists. Empty if the tenant module IS this module
/// (its own migration is already included) or there is no tenancy.
fn tenant_migration_item(design: &Design, module: &ModuleDesign) -> String {
    let Some(t) = tenant_module(design) else {
        return String::new();
    };
    if t.name == module.name || t.entities.is_empty() {
        return String::new();
    }
    let t_snake = t.name.replace('-', "_");
    format!(
        "        jerrycan::db::Migration {{\n            name: \"{t_snake}_0001_create_tables\",\n            sqlite: include_str!(\"../../{t}/migrations/sqlite/0001_create_tables.sql\"),\n            postgres: include_str!(\"../../{t}/migrations/postgres/0001_create_tables.sql\"),\n        }},\n",
        t = t.name,
    )
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
    let table = format!("{}s", tenancy.entity.to_lowercase());
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
    let (cols, vals) = tenant_row_cols_vals(entity, "1");
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
/// kolli-slice Workspace), so the literal is numeric.
fn tenant_row_cols_vals(entity: &Entity, pk: &str) -> (String, String) {
    let mut cols = vec!["id".to_string()];
    let mut vals = vec![pk.to_string()];
    for f in entity.fields.iter().filter(|f| f.name != "id") {
        cols.push(format!("\\\"{}\\\"", f.name));
        vals.push(seed_sql_value(f));
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
    let table = format!("{}s", tenancy.entity.to_lowercase());
    let members = format!("{}_members", Design::to_snake(&tenancy.entity));
    let fk = Design::fk_column(&tenancy.entity);
    let role = isolation_member_role(design, module);
    let (cols, vals) = tenant_row_cols_vals(entity, "2");
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
    let body = fixture_json(design, module, &entity.name);
    let create_path = format!("{base}/");

    // user 1 (cookie 1) creates a row in tenant 1, then we read the id it echoes.
    let mut t = String::new();
    t.push_str(&format!(
        "/// SECURITY: a tenant must not reach another tenant's {entity} rows. User 1\n/// creates a row in tenant 1; user 2 (tenant 2) must be denied read/list/delete.\n/// Passes only with the SCOPED repo accessors (get_for/all_for/remove_for).\n#[tokio::test]\nasync fn tenant_a_cannot_read_tenant_b_{plural}() {{\n    let t = app().await;\n",
        entity = entity.name,
    ));
    t.push_str(&format!(
        "    let created = t.post_json_with(\"{create_path}\", &serde_json::json!({body}), &[(\"cookie\", &test_cookie_for(1))]).await;\n    assert_eq!(created.status().as_u16(), {status}, \"setup: user 1 creates a {entity}; body: {{}}\", created.text());\n    let row: serde_json::Value = serde_json::from_str(&created.text()).expect(\"created json\");\n    let id = &row[\"id\"];\n    let cookie2 = test_cookie_for(2);\n",
        status = create.success.status,
        entity = entity.name,
    ));

    // A GET "/{id}" lets us assert the foreign row 404s for user 2 and survives
    // for user 1; a DELETE "/{id}" (role-gated → user 2's membership carries the
    // role) must also 404 without destroying user 1's row.
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

    if let Some(_get) = get_one {
        t.push_str(&format!(
            "    let foreign = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"cookie\", &cookie2)]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-tenant get must 404 (use get_for, not get); body: {{}}\", foreign.text());\n",
        ));
    }
    if list.is_some() {
        // Always cookied: even an unguarded list is safe to call with a cookie,
        // and a guarded one needs it. user 2 sees only tenant 2's (empty) rows.
        t.push_str(&format!(
            "    let listed = t.get_with(\"{base}/\", &[(\"cookie\", &cookie2)]).await;\n    assert_eq!(listed.status().as_u16(), 200, \"user 2 lists their own {plural}; body: {{}}\", listed.text());\n    let rows: serde_json::Value = serde_json::from_str(&listed.text()).expect(\"list json\");\n    let absent = rows.as_array().map(|a| a.iter().all(|r| &r[\"id\"] != id)).unwrap_or(true);\n    assert!(absent, \"cross-tenant list must NOT contain tenant 1's row (use all_for); body: {{}}\", listed.text());\n",
        ));
    }
    if let Some(_del) = delete_one {
        t.push_str(&format!(
            "    let del = t.delete_with(&format!(\"{base}/{{id}}\"), &[(\"cookie\", &cookie2)]).await;\n    assert_eq!(del.status().as_u16(), 404, \"cross-tenant delete must 404 (use remove_for, not remove); body: {{}}\", del.text());\n",
        ));
        if get_one.is_some() {
            t.push_str(&format!(
                "    let survives = t.get_with(&format!(\"{base}/{{id}}\"), &[(\"cookie\", &test_cookie_for(1))]).await;\n    assert_eq!(survives.status().as_u16(), 200, \"tenant 1's row must survive a cross-tenant delete; body: {{}}\", survives.text());\n",
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
    if let Some(values) = &f.values {
        if let Some(first) = values.first() {
            return format!("'{first}'");
        }
    }
    match f.field_type {
        FieldType::String | FieldType::Datetime | FieldType::Uuid => "'test-value'".to_string(),
        FieldType::Integer => "1".to_string(),
        FieldType::Float => "1.0".to_string(),
        FieldType::Boolean => "false".to_string(),
        FieldType::Json => "'{}'".to_string(),
    }
}

fn preamble(design: &Design, module: &ModuleDesign) -> String {
    let mount = module.effective_mount();
    let auth_login = if design.wants_auth() {
        auth_preamble_login()
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
        let mut migration_items = String::new();
        collect_migration_items(module, &mut migration_items);
        // Tenancy: the guarded handlers of a tenant-owned module take
        // `Dep<Tenant>`. The test app must (a) migrate the tenant module's tables
        // (the `{tenant}_members` table the guard queries), (b) seed a membership
        // row so the guard resolves a tenant (not 403), and (c) register the
        // `tenant` factory app-wide so `Dep<Tenant>` resolves at all.
        migration_items.push_str(&tenant_migration_item(design, module));
        let seed = tenant_seed(design, module);
        let tenant_dep = if module_needs_tenant(design, module) {
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
        format!(
            "{seed_use}{auth_login}{second_seed_fn}async fn app() -> TestApp {{\n    let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n    db.migrate(&[\n{migration_items}    ])\n    .await\n    .expect(\"migrations\");\n{seed}{second_seed_call}    App::new(){auth_extend}.extend(db){tenant_dep}.mount(\"{mount}\", module()).into_test()\n}}\n"
        )
    } else {
        format!(
            "{auth_login}async fn app() -> TestApp {{\n    App::new(){auth_extend}.mount(\"{mount}\", module()).into_test()\n}}\n"
        )
    }
}

fn collect_migration_items(module: &ModuleDesign, out: &mut String) {
    if !module.entities.is_empty() {
        out.push_str(&format!(
            "        jerrycan::db::Migration {{\n            name: \"{m}_0001_create_tables\",\n            sqlite: include_str!(\"../migrations/sqlite/0001_create_tables.sql\"),\n            postgres: include_str!(\"../migrations/postgres/0001_create_tables.sql\"),\n        }},\n",
            m = module.name.replace('-', "_")
        ));
    }
    fn subs(module: &ModuleDesign, out: &mut String) {
        for sub in &module.subroutes {
            if !sub.entities.is_empty() {
                let s = sub.name.replace('-', "_");
                out.push_str(&format!(
                    "        jerrycan::db::Migration {{\n            name: \"{s}_0001_create_tables\",\n            sqlite: include_str!(\"../migrations/sqlite/0001_create_tables_{s}.sql\"),\n            postgres: include_str!(\"../migrations/postgres/0001_create_tables_{s}.sql\"),\n        }},\n"
                ));
            }
            subs(sub, out);
        }
    }
    subs(module, out);
}

/// The full tests/acceptance.rs for one top-level module.
pub fn acceptance_rs(design: &Design, module: &ModuleDesign) -> String {
    let mut out = TestOut {
        code: String::new(),
        todos: Vec::new(),
        count: 0,
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
    format!(
        "//! GENERATED by jerrycan gen-tests — TOOL-OWNED acceptance criteria from design.json.\n//! Regenerated on demand; add your own tests in sibling files, not here.\nuse jerrycan::prelude::*;\nuse {ident}::module;\n\n{preamble}\n{code}{todos}",
        ident = super::genroute::crate_ident(&module.name),
        preamble = preamble(design, module),
        code = out.code,
    )
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
    let content = acceptance_rs(design, module);
    let rel = format!("crates/routes/{module_name}/tests/acceptance.rs");
    let path = root.join(&rel);
    std::fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
    std::fs::write(&path, &content).map_err(|e| e.to_string())?;
    Ok((rel, test_count(&content)))
}

/// How many #[tokio::test] functions a generated file contains.
pub fn test_count(generated: &str) -> usize {
    generated.matches("#[tokio::test]").count()
}
