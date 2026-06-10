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

fn fixture_json(m: &ModuleDesign, entity: &str) -> String {
    let Some(e) = m.entities.iter().find(|e| e.name == entity) else {
        return "{}".to_string();
    };
    let fields = e
        .fields
        .iter()
        .map(|f| format!("\"{}\": {}", f.name, fixture_value(f.field_type)))
        .collect::<Vec<_>>()
        .join(", ");
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
fn request_expr(unit: &ModuleDesign, ep: &Endpoint, path: &str, guarded_and_auth: bool) -> String {
    let body = || {
        ep.request_body
            .as_ref()
            .map(|rb| fixture_json(unit, &rb.entity))
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

fn unit_tests(unit: &ModuleDesign, base: &str, out: &mut TestOut) {
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
            let request = request_expr(unit, ep, &full_path, guarded);
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
                push_401_test(out, unit, ep, &full_path, false);
            }
        } else if param_count(ep) == 1 && seed.is_some() {
            let seeded_path = full_path.replacen(&regex_free_param(&ep.path), seed_id, 1);
            let request = request_expr(unit, ep, &seeded_path, guarded);
            out.code.push_str(&format!(
                "#[tokio::test]\nasync fn {fn_base}_returns_{status}() {{\n    let t = app().await;\n{seed}    let res = {request};\n    assert_eq!(res.status().as_u16(), {status}, \"design: {fn_base} -> {status}; body: {{}}\", res.text());\n}}\n\n",
                seed = seed.as_deref().unwrap_or("")
            ));
            out.count += 1;
            if guarded {
                push_401_test(out, unit, ep, &seeded_path, seed.is_some());
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
        unit_tests(sub, &sub_base, out);
    }
}

/// A `{op}_without_auth_is_401` test: the guard extractor runs first, so a
/// credential-less request is rejected before any handler logic — no seed needed.
fn push_401_test(out: &mut TestOut, unit: &ModuleDesign, ep: &Endpoint, path: &str, _seeded: bool) {
    let fn_base = &ep.operation_id;
    let request = request_expr(unit, ep, path, false); // no cookie
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
/// the app() helper adds so the SAME secret decrypts the cookie.
fn auth_preamble_login() -> String {
    format!(
        "fn test_cookie() -> String {{\n    let auth = jerrycan::auth::Auth::with_secret(\"{TEST_SECRET}\");\n    let token = auth.sessions().encode(&shared::SessionUser {{ id: 1, role: \"admin\".into() }}).expect(\"encode\");\n    format!(\"jerrycan_session={{token}}\")\n}}\n\n"
    )
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
        format!(
            "{auth_login}async fn app() -> TestApp {{\n    let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n    db.migrate(&[\n{migration_items}    ])\n    .await\n    .expect(\"migrations\");\n    App::new(){auth_extend}.extend(db).mount(\"{mount}\", module()).into_test()\n}}\n"
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
    unit_tests(module, &module.effective_mount(), &mut out);
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
