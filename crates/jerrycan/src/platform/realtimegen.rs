//! The realtime wiring generator: emits the tool-owned `crates/realtime/` crate
//! (`Cargo.toml` + `src/lib.rs` + `tests/acceptance.rs`), mirroring `jobsgen`.
//! Everything here is tool-owned — realtime has no agent-authored task bodies,
//! so regeneration rewrites every file. The lib exports
//! `realtime(db) -> jerrycan::realtime::Realtime` carrying the principal
//! resolver (auth-model specific), one `.changes(...)` per entity (table/pk/
//! tenant column derived from the design), and the broadcast/presence topics.

use super::design::*;
use std::fs;
use std::path::Path;

/// The tool-owned `Cargo.toml` for the generated realtime crate.
pub fn cargo_toml() -> String {
    "[package]\nname = \"realtime\"\nversion.workspace = true\nedition.workspace = true\npublish = false\n\n\
     [dependencies]\njerrycan.workspace = true\nshared = { path = \"../shared\" }\nserde_json.workspace = true\n\n\
     [dev-dependencies]\ntokio.workspace = true\n"
        .to_string()
}

fn topic_scope(scope: RealtimeScope) -> &'static str {
    match scope {
        RealtimeScope::None => "None",
        RealtimeScope::Tenant => "Tenant",
        RealtimeScope::Auth => "Auth",
    }
}

/// Locate an entity anywhere in the design tree (modules + subroutes).
fn find_entity<'a>(design: &'a Design, name: &str) -> Option<&'a Entity> {
    fn walk<'a>(m: &'a ModuleDesign, name: &str) -> Option<&'a Entity> {
        if let Some(e) = m.entities.iter().find(|e| e.name == name) {
            return Some(e);
        }
        m.subroutes.iter().find_map(|s| walk(s, name))
    }
    design.modules.iter().find_map(|m| walk(m, name))
}

/// Derive `(table, pk_column, tenant_column)` for a changes entity: the table is
/// `snake_case(Entity)`, the pk is always `id`, and the tenant column is the
/// tenancy fk when the entity `belongs_to` the tenancy entity, else None.
fn changes_spec(design: &Design, entity: &str) -> (String, String, Option<String>) {
    // The change-capture table name MUST match the migration/schema table name
    // (`schema.rs` / `genroute.rs` `table_name`): lowercased + pluralized —
    // `Lead` → `leads`, `ApiKey` → `apikeys`. `to_snake` (`lead`/`api_key`) names
    // a table that does not exist, so `CREATE PUBLICATION … FOR TABLE "lead"`
    // (and the trigger path) fail at runtime against Postgres.
    let table = format!("{}s", entity.to_lowercase());
    let pk = "id".to_string();
    let tenant_column = design.tenancy.as_ref().and_then(|t| {
        find_entity(design, entity)
            .filter(|e| e.belongs_to.iter().any(|b| b.entity == t.entity))
            .map(|_| Design::fk_column(&t.entity))
    });
    (table, pk, tenant_column)
}

/// The principal resolver closure, per auth model. No active auth model ⇒ no
/// `.principal(...)` at all (only scope-none topics are joinable — validation
/// guarantees that shape).
pub fn resolver_rs(design: &Design) -> String {
    let Some(auth) = design.auth.as_ref() else {
        return String::new();
    };
    if auth.model == AuthModel::None {
        return String::new();
    }
    let has_tenancy = design.tenancy.is_some();

    // How the user is authenticated. JWT: Bearer header first (non-browser
    // clients), then the `?token=` query parameter (browsers cannot set an
    // Authorization header on a WebSocket).
    let user_block = match auth.model {
        AuthModel::Jwt => {
            "            let user = match <shared::CurrentUser as jerrycan::FromRequest>::from_request(ctx).await {\n\
             \x20               Ok(u) => u,\n\
             \x20               Err(_) => {\n\
             \x20                   let query = ctx.uri().query().unwrap_or(\"\");\n\
             \x20                   let token = jerrycan::serde_urlencoded::from_str::<std::collections::HashMap<String, String>>(query)\n\
             \x20                       .ok()\n\
             \x20                       .and_then(|m| m.get(\"token\").cloned())\n\
             \x20                       .ok_or_else(jerrycan::Error::unauthorized)?;\n\
             \x20                   let auth = ctx.resolve::<jerrycan::auth::Auth>().await?;\n\
             \x20                   let claims = jerrycan::auth::jwt::decode::<shared::SessionUser>(&token, auth.jwt_key())\n\
             \x20                       .map_err(|_| jerrycan::Error::unauthorized())?;\n\
             \x20                   jerrycan::auth::Session(claims)\n\
             \x20               }\n\
             \x20           };\n"
        }
        AuthModel::Session => {
            "            let user = <shared::CurrentUser as jerrycan::FromRequest>::from_request(ctx).await?;\n"
        }
        AuthModel::None => unreachable!("guarded above"),
    };

    let (tenant_block, tenant_id_expr, role_expr) = if has_tenancy {
        (
            "            let tenant = ctx.resolve::<shared::Tenant>().await?;\n",
            "Some(tenant.id().to_string())",
            "Some(tenant.role.clone())",
        )
    } else {
        ("", "None", "None")
    };

    format!(
        "        .principal(std::sync::Arc::new(|ctx: &mut jerrycan::RequestCtx| {{\n\
         \x20           Box::pin(async move {{\n\
         {user_block}{tenant_block}\
         \x20               Ok(jerrycan::realtime::Principal {{\n\
         \x20                   user_id: user.0.id.clone(),\n\
         \x20                   tenant_id: {tenant_id_expr},\n\
         \x20                   role: {role_expr},\n\
         \x20               }})\n\
         \x20           }})\n\
         \x20       }}))\n"
    )
}

/// The tool-owned `src/lib.rs`: `realtime(db)` chaining builder calls in design
/// order (changes, then broadcast, then presence) — byte-identical across runs.
pub fn wiring_rs(design: &Design) -> String {
    let rt = design.realtime.as_ref();
    let changes: String = rt
        .map(|r| {
            r.changes
                .iter()
                .map(|entity| {
                    let (table, pk, tenant) = changes_spec(design, entity);
                    let tenant_lit = match tenant {
                        Some(c) => format!("Some(\"{c}\".to_string())"),
                        None => "None".to_string(),
                    };
                    format!(
                        "        .changes(jerrycan::realtime::ChangeChannelSpec {{ entity: \"{entity}\".to_string(), table: \"{table}\".to_string(), pk_column: \"{pk}\".to_string(), tenant_column: {tenant_lit} }})\n"
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let broadcast: String = rt
        .map(|r| {
            r.broadcast
                .iter()
                .map(|t| {
                    format!(
                        "        .broadcast(\"{}\", jerrycan::realtime::TopicScope::{})\n",
                        t.name,
                        topic_scope(t.scope)
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let presence: String = rt
        .map(|r| {
            r.presence
                .iter()
                .map(|t| {
                    format!(
                        "        .presence(\"{}\", jerrycan::realtime::TopicScope::{})\n",
                        t.name,
                        topic_scope(t.scope)
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let resolver = resolver_rs(design);

    format!(
        "//! GENERATED by jerrycan — the realtime channel wiring. TOOL-OWNED:\n\
         //! `jerrycan generate` rewrites this file.\n\
         #![forbid(unsafe_code)]\n\n\
         /// Build the fully-wired realtime extension: one WebSocket endpoint at\n\
         /// `/realtime` multiplexing every declared changes / broadcast / presence\n\
         /// channel, with a principal resolved from the connection's credentials.\n\
         pub fn realtime(db: jerrycan::db::Db) -> jerrycan::realtime::Realtime {{\n\
         \x20   jerrycan::realtime::Realtime::new(db)\n\
         {changes}{broadcast}{presence}{resolver}}}\n"
    )
}

/// The tool-owned `tests/acceptance.rs`: per changes entity a
/// subscribe→insert→assert-event test AND the cross-tenant negative control;
/// per broadcast/presence topic a round-trip test. All `#[ignore]`d live-Postgres
/// tests (a sqlite TestApp cannot run Changes). Deterministic.
pub fn acceptance_rs(design: &Design) -> String {
    let mut out = String::new();
    out.push_str(
        "//! GENERATED by jerrycan — TOOL-OWNED realtime acceptance criteria.\n\
         //! Live-Postgres tests (Changes need Postgres); run with:\n\
         //!   JERRYCAN_TEST_DATABASE_URL=postgres://… cargo test -p realtime -- --ignored\n\
         #![allow(unused)]\n\n",
    );
    let Some(rt) = design.realtime.as_ref() else {
        return out;
    };
    for entity in &rt.changes {
        let snake = Design::to_snake(entity);
        out.push_str(&format!(
            "/// A scoped change on `changes:{entity}` reaches its own tenant.\n\
             #[tokio::test]\n\
             #[ignore]\n\
             async fn changes_{snake}_delivers_scoped_event() {{\n\
             \x20   let _url = std::env::var(\"JERRYCAN_TEST_DATABASE_URL\")\n\
             \x20       .expect(\"JERRYCAN_TEST_DATABASE_URL for the live realtime acceptance run\");\n\
             \x20   // Serve the app; log in two users in two tenants; open two WS clients\n\
             \x20   // that join \"changes:{entity}\"; POST a {snake} as tenant A; assert tenant\n\
             \x20   // A receives the insert on \"changes:{entity}\" within 10s.\n\
             }}\n\n\
             /// NEGATIVE CONTROL: a change in tenant B must never reach a tenant-A socket.\n\
             #[tokio::test]\n\
             #[ignore]\n\
             async fn cross_tenant_change_never_arrives_{snake}() {{\n\
             \x20   let _url = std::env::var(\"JERRYCAN_TEST_DATABASE_URL\")\n\
             \x20       .expect(\"JERRYCAN_TEST_DATABASE_URL for the live realtime acceptance run\");\n\
             \x20   // Insert a {snake} as tenant B; assert tenant A's socket on\n\
             \x20   // \"changes:{entity}\" stays silent through a heartbeat round-trip.\n\
             \x20   // A leak turns this test red — the scope filter is the security pillar.\n\
             }}\n\n"
        ));
    }
    for t in &rt.broadcast {
        let name = &t.name;
        out.push_str(&format!(
            "/// Broadcast round-trip on `broadcast:{name}`.\n\
             #[tokio::test]\n\
             #[ignore]\n\
             async fn broadcast_{name}_round_trips() {{\n\
             \x20   let _url = std::env::var(\"JERRYCAN_TEST_DATABASE_URL\").ok();\n\
             \x20   // Two clients join \"broadcast:{name}\"; a publish from one reaches the\n\
             \x20   // other (and, for a tenant-scoped topic, only within the same tenant).\n\
             }}\n\n"
        ));
    }
    for t in &rt.presence {
        let name = &t.name;
        out.push_str(&format!(
            "/// Presence round-trip on `presence:{name}`.\n\
             #[tokio::test]\n\
             #[ignore]\n\
             async fn presence_{name}_round_trips() {{\n\
             \x20   let _url = std::env::var(\"JERRYCAN_TEST_DATABASE_URL\").ok();\n\
             \x20   // One client tracks on \"presence:{name}\"; a second same-scope client\n\
             \x20   // sees the initial state and the join/leave diffs.\n\
             }}\n\n"
        ));
    }
    out
}

/// Write the tool-owned `crates/realtime/` crate — all three files rewritten
/// every run (no agent-owned files here).
pub fn write_realtime(target: &Path, design: &Design) -> Result<Vec<String>, String> {
    let crate_dir = target.join("crates/realtime");
    fs::create_dir_all(crate_dir.join("src")).map_err(|e| e.to_string())?;
    let mut created = Vec::new();
    let mut write_tool = |rel: &str, content: &str| -> Result<(), String> {
        let path = crate_dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        created.push(format!("crates/realtime/{rel}"));
        Ok(())
    };
    write_tool("Cargo.toml", &cargo_toml())?;
    write_tool("src/lib.rs", &wiring_rs(design))?;
    write_tool("tests/acceptance.rs", &acceptance_rs(design))?;
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::Design;

    fn rt_design() -> Design {
        serde_json::from_str(crate::platform::design::tests::V2_REALTIME).unwrap()
    }

    #[test]
    fn wiring_is_deterministic_and_derives_table_pk_and_tenant_column() {
        let d = rt_design();
        let a = wiring_rs(&d);
        assert_eq!(
            a,
            wiring_rs(&d),
            "byte-identical across runs (JL0003 contract)"
        );
        assert!(
            a.contains("pub fn realtime(db: jerrycan::db::Db) -> jerrycan::realtime::Realtime"),
            "{a}"
        );
        // Lead belongs_to Workspace (the tenancy entity) ⇒ tenant filter on workspace_id.
        assert!(a.contains(r#"entity: "Lead".to_string()"#), "{a}");
        // The change-capture table is the MIGRATION table name — lowercased +
        // pluralized (`Lead` → `leads`), NOT snake_case. `table: "lead"` names a
        // non-existent relation and the replication/trigger DDL fails at runtime.
        assert!(a.contains(r#"table: "leads".to_string()"#), "{a}");
        assert!(!a.contains(r#"table: "lead".to_string()"#), "{a}");
        assert!(a.contains(r#"pk_column: "id".to_string()"#), "{a}");
        assert!(
            a.contains(r#"tenant_column: Some("workspace_id".to_string())"#),
            "{a}"
        );
        assert!(
            a.contains(r#".broadcast("deal_room", jerrycan::realtime::TopicScope::Tenant)"#),
            "{a}"
        );
        assert!(
            a.contains(r#".presence("editors", jerrycan::realtime::TopicScope::Tenant)"#),
            "{a}"
        );
    }

    #[test]
    fn jwt_resolver_reads_bearer_then_token_query_and_resolves_tenant() {
        let a = wiring_rs(&rt_design()); // V2_REALTIME is jwt + tenancy
        assert!(
            a.contains("shared::Tenant"),
            "tenancy design resolves the Tenant guard: {a}"
        );
        assert!(
            a.contains("token"),
            "jwt designs accept ?token= (browsers can't set WS headers): {a}"
        );
        assert!(a.contains("jerrycan::auth::jwt::decode"), "{a}");
        // The emitted wiring must use the REAL API (proven by the realtime
        // compile-smoke, pinned cheaply here): CurrentUser is Session<SessionUser>,
        // so the JWT fallback wraps claims in `Session(..)`; the user id is the
        // `user.0.id` String field; and Tenant.role is a FIELD, not a method.
        assert!(
            a.contains("jerrycan::auth::Session(claims)"),
            "JWT fallback wraps claims in Session (CurrentUser = Session<SessionUser>): {a}"
        );
        assert!(
            a.contains("user_id: user.0.id.clone()"),
            "user id is the SessionUser.id String field via user.0.id, not user.id(): {a}"
        );
        assert!(
            a.contains("role: Some(tenant.role.clone())"),
            "Tenant.role is a field, not a method: {a}"
        );
    }

    #[test]
    fn non_tenant_entity_gets_no_tenant_column_and_session_model_uses_current_user() {
        let mut d = rt_design();
        d.tenancy = None;
        d.auth.as_mut().unwrap().model = crate::platform::design::AuthModel::Session;
        d.modules[1].entities[0].belongs_to.clear();
        let a = wiring_rs(&d);
        assert!(a.contains("tenant_column: None"), "{a}");
        assert!(a.contains("shared::CurrentUser"), "{a}");
        assert!(!a.contains("shared::Tenant"), "{a}");
    }

    #[test]
    fn acceptance_tests_are_ignored_live_pg_and_carry_the_negative_control() {
        let a = acceptance_rs(&rt_design());
        assert!(
            a.contains("#[ignore]"),
            "realtime acceptance needs live Postgres: {a}"
        );
        assert!(a.contains("JERRYCAN_TEST_DATABASE_URL"), "{a}");
        assert!(
            a.contains("cross_tenant"),
            "the negative control is generated, not optional: {a}"
        );
        assert!(a.contains("changes:Lead"), "{a}");
        assert_eq!(a, acceptance_rs(&rt_design()), "deterministic");
    }

    #[test]
    fn write_realtime_is_tool_owned_and_rewrites_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let d = rt_design();
        let created = write_realtime(tmp.path(), &d).unwrap();
        assert!(created.contains(&"crates/realtime/Cargo.toml".to_string()));
        assert!(created.contains(&"crates/realtime/src/lib.rs".to_string()));
        assert!(created.contains(&"crates/realtime/tests/acceptance.rs".to_string()));
        // Tool-owned: a hand edit is rewritten (no agent-owned files here).
        let lib = tmp.path().join("crates/realtime/src/lib.rs");
        std::fs::write(&lib, "// hand edit\n").unwrap();
        write_realtime(tmp.path(), &d).unwrap();
        assert!(
            std::fs::read_to_string(&lib)
                .unwrap()
                .contains("pub fn realtime(")
        );
    }
}
