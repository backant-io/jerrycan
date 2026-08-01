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

/// An `Option<String>` as a Rust source literal: `Some("x".to_string())` / `None`.
fn opt_string_lit(v: &Option<String>) -> String {
    match v {
        Some(c) => format!("Some(\"{c}\".to_string())"),
        None => "None".to_string(),
    }
}

/// Derive `(table, pk_column, tenant_column, owner_column)` for a changes entity:
/// the table is `snake_case(Entity)`, the pk is always `id`, the tenant column is
/// the tenancy fk when the entity `belongs_to` the tenancy entity (the pk itself
/// when the entity IS the tenancy entity, else None), and — ONLY when the entity
/// is not tenant-scoped — the owner (identity fk) column when the entity is
/// per-user owned (#216), else None.
fn changes_spec(design: &Design, entity: &str) -> (String, String, Option<String>, Option<String>) {
    // The change-capture table name MUST match the migration/schema table name,
    // so it goes through the SAME `Design::table_name` (snake_case + proper
    // pluralization, honoring any `table` override) — `Lead` → `leads`,
    // `ApiKey` → `api_keys`. A mismatched name would make
    // `CREATE PUBLICATION … FOR TABLE "…"` (and the trigger path) fail at runtime.
    let table = design.table_name(entity);
    let pk = "id".to_string();
    let tenant_column = design.tenancy.as_ref().and_then(|t| {
        if entity == t.entity {
            // #113 (CRITICAL): the tenant entity is its own tenant key. An
            // entity never `belongs_to` itself, so the fk branch below would
            // leave the channel UNSCOPED (`tenant_column: None`) — and the
            // runtime's `change_visible` treats `None` as world-visible,
            // broadcasting every tenant's row to every authenticated
            // principal. The tenant's own pk closes the leak: CDC extracts
            // `NEW."id"::text`, which equals `Principal.tenant_id` (the
            // stringified tenant pk), so a member receives exactly their own
            // tenant's row and non-members receive nothing.
            return Some(pk.clone());
        }
        find_entity(design, entity)
            .filter(|e| e.belongs_to.iter().any(|b| b.entity == t.entity))
            .map(|_| Design::fk_column(&t.entity))
    });
    // #216: a per-user (identity-owned, non-tenant) changes entity is
    // owner-scoped. Only when the entity is NOT tenant-scoped AND is per-user
    // owned (auth + identity fk + no tenant path) does the channel carry the
    // identity fk column (#150-aware, e.g. `user_id`) so `change_visible`
    // delivers each row only to its owner. Tenant-scoped and genuinely
    // auth-only changes entities keep `owner_column: None` (byte-identical).
    let owner_column = if tenant_column.is_none() {
        find_entity(design, entity)
            .filter(|e| design.entity_is_per_user_owned(e))
            .map(|_| design.identity_fk_column())
    } else {
        None
    };
    (table, pk, tenant_column, owner_column)
}

/// The `hidden_columns` literal for a changes entity: the DB column name (the
/// field's own `name`) of every write_only/password_hash field, in declaration
/// order, as a `vec![...]` literal. Emits `vec![]` when the entity has none — a
/// byte-identical full-row broadcast. This is what lifts the old refusal (#167): the
/// realtime engine strips these columns from the broadcast row so a response-
/// hidden secret never reaches a WebSocket subscriber, matching the REST hide.
fn hidden_columns_lit(design: &Design, entity: &str) -> String {
    let cols: Vec<String> = find_entity(design, entity)
        .map(|e| {
            e.fields
                .iter()
                .filter(|f| Design::field_is_write_only(f))
                .map(|f| format!("\"{}\".to_string()", f.name))
                .collect()
        })
        .unwrap_or_default();
    format!("vec![{}]", cols.join(", "))
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
             \x20                   jerrycan::auth::Bearer(claims)\n\
             \x20               }\n\
             \x20           };\n"
        }
        AuthModel::Session => {
            "            let user = <shared::CurrentUser as jerrycan::FromRequest>::from_request(ctx).await?;\n"
        }
        AuthModel::None => unreachable!("guarded above"),
    };

    // #104: the WS tenant leg is membership-aware. A single-membership user still
    // binds exactly their one tenant (behavior-identical to the old
    // `ctx.resolve::<shared::Tenant>()`), but a multi-membership user chooses via
    // `?tenant=` (verified, non-members refused) and a zero/many-membership user
    // connects with `tenant_id = None` (reaching only None/Auth topics) rather than
    // being 403'd off `/realtime`. The bindings are named `resolved_tenant` /
    // `resolved_role` so the Principal's field init stays non-redundant (avoids
    // clippy::redundant_field_names under -D warnings).
    let (tenant_block, tenant_id_expr, role_expr) = if has_tenancy {
        (
            tenant_resolve_block(design),
            "resolved_tenant".to_string(),
            "resolved_role".to_string(),
        )
    } else {
        (String::new(), "None".to_string(), "None".to_string())
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

/// The membership-aware WS tenant resolve (issue #104, parts 2+4). Binds
/// `(resolved_tenant, resolved_role): (Option<String>, Option<String>)`:
/// - An explicit `?tenant=<id>` in the WS connect query is VERIFIED against the
///   `{tenant}_members` table (`{fk} = ? AND user_id = ?`, the fixed
///   `MEMBERSHIP_PRINCIPAL_COLUMN`). A member ⇒ `Some(that)`; a NON-member (or a
///   malformed id) REFUSES the upgrade with a 403 — the make-impossible guard: a
///   socket can never scope to a tenant the session user is not a verified member
///   of.
/// - Absent `?tenant=`: resolve memberships. EXACTLY ONE ⇒ that tenant
///   (behavior-identical to the pre-#104 sole-membership resolve). ZERO or MORE
///   THAN ONE ⇒ `None` (connect, reaching only None/Auth topics; a Tenant topic
///   rejects a `None` principal at JOIN). It NEVER aborts for "no tenant" — only
///   for an explicit non-member `?tenant=`.
fn tenant_resolve_block(design: &Design) -> String {
    let tenant = &design.tenancy.as_ref().expect("has_tenancy checked").entity;
    let fk = Design::fk_column(tenant);
    let tenant_snake = Design::to_snake(tenant);
    let id_ty = design.target_key_rust_type(tenant);
    // Parse the `?tenant=` string into the tenant pk type and bind it typed, and
    // choose the canonical string form of the verified tenant (must equal the CDC
    // `::text` key the delivery filter compares against). A text pk is verbatim; an
    // integer pk parses (a non-numeric id reads as a non-member → 403, no probing).
    let (fk_parse, fk_bind, canonical) = if id_ty == "String" {
        (
            String::new(),
            "tenant.clone().into()".to_string(),
            "tenant".to_string(),
        )
    } else {
        (
            format!(
                "                    let tenant_key: {id_ty} = match tenant.parse() {{\n\
                 \x20                       Ok(v) => v,\n\
                 \x20                       Err(_) => return Err(jerrycan::Error::forbidden()),\n\
                 \x20                   }};\n"
            ),
            "tenant_key.into()".to_string(),
            "tenant_key.to_string()".to_string(),
        )
    };
    format!(
        "            let (resolved_tenant, resolved_role): (Option<String>, Option<String>) = {{\n\
         \x20               use jerrycan::db::sea_orm::ConnectionTrait;\n\
         \x20               let db = ctx.resolve::<jerrycan::db::Db>().await?;\n\
         \x20               let requested = jerrycan::serde_urlencoded::from_str::<std::collections::HashMap<String, String>>(\n\
         \x20                   ctx.uri().query().unwrap_or(\"\"),\n\
         \x20               )\n\
         \x20               .ok()\n\
         \x20               .and_then(|m| m.get(\"tenant\").cloned());\n\
         \x20               match requested {{\n\
         \x20                   Some(tenant) => {{\n\
         \x20                       // #104: an explicit ?tenant= is honored ONLY for a\n\
         \x20                       // VERIFIED member — a non-member REFUSES the upgrade.\n\
         {fk_parse}\
         \x20                       let row = db\n\
         \x20                           .conn()\n\
         \x20                           .query_one(jerrycan::db::sea_orm::Statement::from_sql_and_values(\n\
         \x20                               db.conn().get_database_backend(),\n\
         \x20                               db.sql(\"SELECT role FROM {tenant_snake}_members WHERE {fk} = ? AND user_id = ?\"),\n\
         \x20                               [{fk_bind}, user.0.id.clone().into()],\n\
         \x20                           ))\n\
         \x20                           .await\n\
         \x20                           .map_err(jerrycan::db::db_error)?;\n\
         \x20                       let Some(row) = row else {{\n\
         \x20                           return Err(jerrycan::Error::forbidden());\n\
         \x20                       }};\n\
         \x20                       let role: String = row.try_get(\"\", \"role\").map_err(jerrycan::db::db_error)?;\n\
         \x20                       (Some({canonical}), Some(role))\n\
         \x20                   }}\n\
         \x20                   None => {{\n\
         \x20                       // No ?tenant=: EXACTLY ONE membership ⇒ that tenant;\n\
         \x20                       // ZERO or MANY ⇒ None (connect, None/Auth topics only).\n\
         \x20                       let rows = db\n\
         \x20                           .conn()\n\
         \x20                           .query_all(jerrycan::db::sea_orm::Statement::from_sql_and_values(\n\
         \x20                               db.conn().get_database_backend(),\n\
         \x20                               db.sql(\"SELECT {fk}, role FROM {tenant_snake}_members WHERE user_id = ? LIMIT 2\"),\n\
         \x20                               [user.0.id.clone().into()],\n\
         \x20                           ))\n\
         \x20                           .await\n\
         \x20                           .map_err(jerrycan::db::db_error)?;\n\
         \x20                       if rows.len() == 1 {{\n\
         \x20                           let tenant_key: {id_ty} = rows[0].try_get(\"\", \"{fk}\").map_err(jerrycan::db::db_error)?;\n\
         \x20                           let role: String = rows[0].try_get(\"\", \"role\").map_err(jerrycan::db::db_error)?;\n\
         \x20                           (Some(tenant_key.to_string()), Some(role))\n\
         \x20                       }} else {{\n\
         \x20                           (None, None)\n\
         \x20                       }}\n\
         \x20                   }}\n\
         \x20               }}\n\
         \x20           }};\n"
    )
}

/// The design's broadcast + presence topics as an INLINE builder-method chain
/// (`.broadcast("x", jerrycan::realtime::TopicScope::Auth).presence("y", …)`), for
/// callers that splice topic declarations onto a `Realtime::new(db)` on a single
/// line — namely the route TestApp harness (testgen), whose realtime handlers may
/// publish to these topics and would otherwise hit JC0404 (undeclared topic) on a
/// bare `Realtime::new` (issue #84). The `.broadcast`/`.presence` calls and their
/// scopes match `wiring_rs` exactly. Changes channels are omitted: they are
/// Postgres-only (never exercised by a sqlite TestApp) and are not
/// `RealtimeHandle::publish` targets. Empty when the design declares no realtime
/// block or no broadcast/presence topics.
pub fn topic_wiring_inline(design: &Design) -> String {
    let Some(rt) = design.realtime.as_ref() else {
        return String::new();
    };
    let mut out = String::new();
    for t in &rt.broadcast {
        out.push_str(&format!(
            ".broadcast(\"{}\", jerrycan::realtime::TopicScope::{})",
            t.name,
            topic_scope(t.scope)
        ));
    }
    for t in &rt.presence {
        out.push_str(&format!(
            ".presence(\"{}\", jerrycan::realtime::TopicScope::{})",
            t.name,
            topic_scope(t.scope)
        ));
    }
    out
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
                    let (table, pk, tenant, owner) = changes_spec(design, entity);
                    let tenant_lit = opt_string_lit(&tenant);
                    let owner_lit = opt_string_lit(&owner);
                    let hidden_lit = hidden_columns_lit(design, entity);
                    // #216: emit the BUILDER chain (not a struct literal) — the
                    // spec is `#[non_exhaustive]` since 0.7.3, so a cross-crate
                    // literal no longer compiles, and the builder keeps future
                    // field-adds a non-breaking minor.
                    format!(
                        "        .changes(jerrycan::realtime::ChangeChannelSpec::new(\"{entity}\", \"{table}\", \"{pk}\").tenant_column({tenant_lit}).owner_column({owner_lit}).hidden_columns({hidden_lit}))\n"
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
        return format!("{}\n", out.trim_end_matches('\n'));
    };
    for entity in &rt.changes {
        let snake = Design::to_snake(entity);
        // #229: a per-user (owner-scoped) changes entity — `owner_column` Some,
        // `tenant_column` None (#216) — carries a cross-USER negative control
        // ("user B"), the twin of the tenant control. A tenant-scoped or
        // genuinely auth-only entity keeps the cross-tenant framing byte-for-byte.
        let (_, _, tenant_column, owner_column) = changes_spec(design, entity);
        let per_user = tenant_column.is_none() && owner_column.is_some();
        if per_user {
            out.push_str(&format!(
                "/// A scoped change on `changes:{entity}` reaches its own owner.\n\
                 #[tokio::test]\n\
                 #[ignore]\n\
                 async fn changes_{snake}_delivers_scoped_event() {{\n\
                 \x20   let _url = std::env::var(\"JERRYCAN_TEST_DATABASE_URL\")\n\
                 \x20       .expect(\"JERRYCAN_TEST_DATABASE_URL for the live realtime acceptance run\");\n\
                 \x20   // Serve the app; log in two users; open two WS clients that join\n\
                 \x20   // \"changes:{entity}\"; POST a {snake} as user A; assert user A receives\n\
                 \x20   // the insert on \"changes:{entity}\" within 10s.\n\
                 }}\n\n\
                 /// NEGATIVE CONTROL: a change owned by user B must never reach a user-A socket.\n\
                 #[tokio::test]\n\
                 #[ignore]\n\
                 async fn cross_user_change_never_arrives_{snake}() {{\n\
                 \x20   let _url = std::env::var(\"JERRYCAN_TEST_DATABASE_URL\")\n\
                 \x20       .expect(\"JERRYCAN_TEST_DATABASE_URL for the live realtime acceptance run\");\n\
                 \x20   // Insert a {snake} as user B; assert user A's socket on\n\
                 \x20   // \"changes:{entity}\" stays silent through a heartbeat round-trip.\n\
                 \x20   // A leak turns this test red — owner-scoping (#216) is the security pillar.\n\
                 }}\n\n"
            ));
        } else {
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
    // The last test block ends with a trailing blank line rustfmt strips (issue
    // #218); trim to exactly one final newline so the scaffold is a fmt fixpoint.
    format!("{}\n", out.trim_end_matches('\n'))
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
        // Lead belongs_to Workspace (the tenancy entity) ⇒ tenant filter on
        // workspace_id, emitted through the #216 BUILDER chain (the spec is
        // #[non_exhaustive] since 0.7.3). The change-capture table is the
        // MIGRATION table name — lowercased + pluralized (`Lead` → `leads`), NOT
        // snake_case; a singular `"lead"` names a non-existent relation.
        assert!(
            a.contains(
                r#".changes(jerrycan::realtime::ChangeChannelSpec::new("Lead", "leads", "id")"#
            ),
            "{a}"
        );
        assert!(!a.contains(r#""lead","#), "{a}");
        assert!(
            a.contains(r#".tenant_column(Some("workspace_id".to_string()))"#),
            "{a}"
        );
        // Lead is tenant-scoped, so no owner scoping (byte-identical to pre-#216).
        assert!(a.contains(".owner_column(None)"), "{a}");
        assert!(
            a.contains(r#".broadcast("deal_room", jerrycan::realtime::TopicScope::Tenant)"#),
            "{a}"
        );
        assert!(
            a.contains(r#".presence("editors", jerrycan::realtime::TopicScope::Tenant)"#),
            "{a}"
        );
        // #167: an entity with no write_only column emits an empty projection set
        // — the realtime broadcast stays byte-identical (full row).
        assert!(
            a.contains(".hidden_columns(vec![]))"),
            "no write_only column ⇒ hidden_columns(vec![]): {a}"
        );
    }

    /// #167 (SECURITY): the changes wiring lists a changes entity's write_only /
    /// password_hash columns in `ChangeChannelSpec.hidden_columns`, so the
    /// realtime engine strips them from the broadcast row (the raw-row leak the
    /// REST `skip_serializing` hide could not reach). This is what lifts the old
    /// interim refusal: the combination is now SAFE because the column is never
    /// delivered. An entity with no such column emits `vec![]` (byte-identical).
    #[test]
    fn changes_wiring_projects_write_only_columns_via_hidden_columns() {
        // Add an explicit write_only flag AND an auto-hidden `password_hash` to
        // the `Lead` changes entity → both DB column names land in
        // `hidden_columns`, in field-declaration order.
        let mut leak = rt_design();
        leak.modules[1].entities[0].fields.push(
            serde_json::from_value(serde_json::json!({
                "name": "api_token", "type": "string", "write_only": true
            }))
            .unwrap(),
        );
        leak.modules[1].entities[0].fields.push(
            serde_json::from_value(
                serde_json::json!({ "name": "password_hash", "type": "string" }),
            )
            .unwrap(),
        );
        let wired = wiring_rs(&leak);
        assert!(
            wired.contains(
                r#".hidden_columns(vec!["api_token".to_string(), "password_hash".to_string()]))"#
            ),
            "write_only + password_hash columns must be projected out via hidden_columns, in \
             declaration order: {wired}"
        );
        // Determinism holds with a non-empty projection set (JL0003 contract).
        assert_eq!(wired, wiring_rs(&leak), "byte-identical across runs");
    }

    /// Issue #84: `topic_wiring_inline` emits the design's broadcast + presence
    /// topics as a single-line builder chain (for the TestApp's `Realtime::new`),
    /// with the SAME names/scopes as `wiring_rs` and NO changes channels/resolver.
    #[test]
    fn topic_wiring_inline_lists_broadcast_and_presence_topics() {
        let d = rt_design();
        let inline = topic_wiring_inline(&d);
        assert_eq!(
            inline,
            r#".broadcast("deal_room", jerrycan::realtime::TopicScope::Tenant).presence("editors", jerrycan::realtime::TopicScope::Tenant)"#,
            "inline chain declares broadcast then presence, no newlines: {inline}"
        );
        // Changes channels are not publish targets and need Postgres — omitted.
        assert!(
            !inline.contains(".changes("),
            "no changes channels: {inline}"
        );
        assert!(!inline.contains('\n'), "single-line chain: {inline}");
        // A design with no realtime block wires nothing.
        let mut plain = d.clone();
        plain.realtime = None;
        assert_eq!(
            topic_wiring_inline(&plain),
            "",
            "no realtime block ⇒ no topic wiring"
        );
    }

    #[test]
    fn jwt_resolver_reads_bearer_then_token_query_and_resolves_tenant() {
        let a = wiring_rs(&rt_design()); // V2_REALTIME is jwt + tenancy
        assert!(
            a.contains("token"),
            "jwt designs accept ?token= (browsers can't set WS headers): {a}"
        );
        assert!(a.contains("jerrycan::auth::jwt::decode"), "{a}");
        // The emitted wiring must use the REAL API (proven by the realtime
        // compile-smoke, pinned cheaply here): under the jwt model CurrentUser is
        // Bearer<SessionUser> (issue #29), so the JWT `?token=` fallback wraps
        // claims in `Bearer(..)` — matching the alias so the `match` type-checks;
        // the user id is the `user.0.id` String field.
        assert!(
            a.contains("jerrycan::auth::Bearer(claims)"),
            "JWT fallback wraps claims in Bearer (CurrentUser = Bearer<SessionUser>): {a}"
        );
        assert!(
            !a.contains("jerrycan::auth::Session(claims)"),
            "jwt model must NOT wrap in Session — the alias is Bearer: {a}"
        );
        assert!(
            a.contains("user_id: user.0.id.clone()"),
            "user id is the SessionUser.id String field via user.0.id, not user.id(): {a}"
        );
    }

    /// #104 (parts 2+4): the WS principal resolver's tenant leg is
    /// MEMBERSHIP-AWARE — it no longer binds an arbitrary first-membership tenant
    /// via `ctx.resolve::<shared::Tenant>()`. It (1) reads an optional `?tenant=`
    /// from the WS query, (2) VERIFIES that tenant against the `{tenant}_members`
    /// table and REFUSES (403) a non-member, (3) falls back to the sole membership
    /// when absent, and (4) NEVER aborts for "no tenant" (zero/many ⇒ `None`, which
    /// reaches only None/Auth topics). This is the security invariant: a socket's
    /// `principal.tenant_id` is only ever a verified membership.
    #[test]
    fn tenant_resolver_is_membership_verified_and_never_aborts_on_no_tenant() {
        let a = wiring_rs(&rt_design()); // V2_REALTIME: jwt + Workspace (integer) tenancy
        // The old arbitrary-first-membership resolve is GONE.
        assert!(
            !a.contains("ctx.resolve::<shared::Tenant>()"),
            "the WS tenant leg must NOT bind an arbitrary first membership: {a}"
        );
        // (1) reads the optional ?tenant= query param (same channel as ?token=).
        assert!(
            a.contains(r#".and_then(|m| m.get("tenant").cloned())"#),
            "resolver reads an optional ?tenant= from the WS query: {a}"
        );
        // (2) VERIFIES membership in the requested tenant against {tenant}_members
        // (fk + the fixed MEMBERSHIP_PRINCIPAL_COLUMN user_id) and 403s a non-member.
        assert!(
            a.contains("SELECT role FROM workspace_members WHERE workspace_id = ? AND user_id = ?"),
            "explicit ?tenant= is membership-verified: {a}"
        );
        assert!(
            a.contains("return Err(jerrycan::Error::forbidden());"),
            "a non-member ?tenant= REFUSES the upgrade (make-impossible guard): {a}"
        );
        // (3) sole-membership fallback (behavior-identical to the pre-#104 resolve).
        assert!(
            a.contains(
                "SELECT workspace_id, role FROM workspace_members WHERE user_id = ? LIMIT 2"
            ) && a.contains("if rows.len() == 1 {"),
            "absent ?tenant= resolves the sole membership: {a}"
        );
        // (4) zero/many memberships ⇒ None (connect) — never a hard error.
        assert!(
            a.contains("(None, None)"),
            "zero/many memberships ⇒ None tenant (no abort, fixes zero-membership 403): {a}"
        );
        // Determinism (JL0003): byte-identical across runs.
        assert_eq!(a, wiring_rs(&rt_design()), "resolver is deterministic");
    }

    /// DRIFT-GUARD for the executed live-WS mirror. The regression test
    /// `crates/jerrycan-realtime/tests/ws_tenant_partition.rs` installs a resolver
    /// that is the BYTE-FOR-BYTE runtime twin of what `tenant_resolve_block` emits
    /// for this integer-pk Workspace design, and PROVES the resolver→delivery seam
    /// end-to-end (a non-member's `?tenant=` is refused; a socket receives only its
    /// chosen tenant's events). `jerrycan-realtime` cannot dev-depend on `jerrycan`
    /// (the dependency runs the other way), so that mirror copies these exact
    /// strings and this test is the pin that keeps them honest. If it goes red, the
    /// generated resolver's SQL or its membership-guard-BEFORE-`Some(tenant)`
    /// ordering changed — update the mirror in lockstep or the live test stops
    /// proving the current code. The ordering pin is the specific defense against
    /// the ONE leak-shaped regression that passes every structural/compile gate:
    /// hoisting `Some(tenant)` out from behind the `let Some(row) else { forbidden }`
    /// guard so a non-member gets a tenant.
    #[test]
    fn tenant_resolve_block_pins_the_ws_live_mirror_contract() {
        let block = tenant_resolve_block(&rt_design());
        // The two membership queries the mirror binds verbatim.
        assert!(
            block.contains(
                "SELECT role FROM workspace_members WHERE workspace_id = ? AND user_id = ?"
            ),
            "mirror's MEMBERSHIP_VERIFY_SQL must match the generated verify: {block}"
        );
        assert!(
            block.contains(
                "SELECT workspace_id, role FROM workspace_members WHERE user_id = ? LIMIT 2"
            ),
            "mirror's SOLE_MEMBERSHIP_SQL must match the generated fallback: {block}"
        );
        // Verify-BEFORE-Some: the membership row guard MUST precede the member
        // success tuple, and the guard body MUST refuse a non-member.
        let guard = block
            .find("let Some(row) = row else {")
            .expect("the membership row guard must be present");
        let success = block
            .find("(Some(tenant_key.to_string()), Some(role))")
            .expect("the member success tuple must be present");
        assert!(
            guard < success,
            "the non-member membership guard MUST come BEFORE Some(tenant): a \
             Some(tenant) hoisted above the guard is the cross-tenant leak the \
             ws_tenant_partition live test catches at runtime: {block}"
        );
        assert!(
            block[guard..success].contains("return Err(jerrycan::Error::forbidden());"),
            "the membership guard must REFUSE a non-member with forbidden(): {block}"
        );
    }

    #[test]
    fn non_tenant_entity_gets_no_tenant_column_and_session_model_uses_current_user() {
        // With tenancy PRESENT, a changes entity that neither IS the tenant nor
        // directly belongs_to it stays unscoped — the #113 fix keys on the
        // tenant entity itself, never blanket-scoping a tenancy design.
        let mut owned = rt_design();
        owned.modules[1].entities[0].belongs_to.clear();
        let w = wiring_rs(&owned);
        // Lead no longer belongs_to Workspace and carries no identity fk ⇒ neither
        // tenant- nor owner-scoped.
        assert!(w.contains(".tenant_column(None)"), "{w}");
        assert!(w.contains(".owner_column(None)"), "{w}");

        let mut d = rt_design();
        d.tenancy = None;
        d.auth.as_mut().unwrap().model = crate::platform::design::AuthModel::Session;
        d.modules[1].entities[0].belongs_to.clear();
        let a = wiring_rs(&d);
        assert!(a.contains(".tenant_column(None)"), "{a}");
        assert!(a.contains(".owner_column(None)"), "{a}");
        assert!(a.contains("shared::CurrentUser"), "{a}");
        assert!(!a.contains("shared::Tenant"), "{a}");
    }

    /// #113 (CRITICAL): a `changes` channel on the tenancy entity itself is
    /// scoped by the tenant's OWN pk. An entity never `belongs_to` itself, so
    /// before the fix the channel got `tenant_column: None` — which the runtime
    /// treats as world-visible, broadcasting every Workspace row to every
    /// authenticated principal, member or not. With `Some("id")` CDC extracts
    /// `NEW."id"::text`, matching the principal's stringified `tenant_id`, so a
    /// member receives exactly their own tenant's row and non-members nothing.
    #[test]
    fn tenant_entity_changes_channel_is_scoped_by_its_own_pk() {
        let mut d = rt_design();
        d.realtime
            .as_mut()
            .unwrap()
            .changes
            .push("Workspace".to_string());
        let a = wiring_rs(&d);
        assert!(
            a.contains(
                r#".changes(jerrycan::realtime::ChangeChannelSpec::new("Workspace", "workspaces", "id").tenant_column(Some("id".to_string()))"#
            ),
            "{a}"
        );
        // The direct-child channel is byte-identical to before the fix — the
        // pk branch fires ONLY for the tenant entity itself.
        assert!(
            a.contains(
                r#".changes(jerrycan::realtime::ChangeChannelSpec::new("Lead", "leads", "id").tenant_column(Some("workspace_id".to_string()))"#
            ),
            "{a}"
        );
        assert!(
            !a.contains(".tenant_column(None)"),
            "no unscoped channel may remain in this tenancy design: {a}"
        );
    }

    /// #216 (SECURITY): a per-user (identity-owned, non-tenant) changes entity
    /// derives `owner_column: Some(identity_fk)` so the runtime delivers each row
    /// only to its owner. Before the fix its channel was unscoped
    /// (`owner_column: None` didn't exist) and `change_visible` treated it as
    /// world-visible — every user received every user's rows. A tenant-scoped or
    /// genuinely auth-only entity keeps `owner_column(None)` (byte-identical).
    #[test]
    fn per_user_changes_entity_derives_owner_column() {
        // A jwt design with NO tenancy: `Note belongs_to User` is per-user owned
        // (identity fk `user_id`, no tenant path), so its changes channel is
        // owner-scoped.
        let d: Design = serde_json::from_str(
            r#"{
                "name": "note-app", "contract_version": 2,
                "auth": { "model": "jwt", "roles": ["member"] },
                "dependencies": ["db", "auth"],
                "realtime": { "changes": ["Note"], "broadcast": [], "presence": [] },
                "modules": [
                    { "name": "notes",
                      "entities": [{ "name": "Note",
                          "belongs_to": [{ "entity": "User" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "body", "type": "string" }] }],
                      "endpoints": [] }
                ]
            }"#,
        )
        .unwrap();
        let a = wiring_rs(&d);
        assert!(
            a.contains(
                r#".changes(jerrycan::realtime::ChangeChannelSpec::new("Note", "notes", "id").tenant_column(None).owner_column(Some("user_id".to_string()))"#
            ),
            "a per-user changes entity must be owner-scoped on the identity fk: {a}"
        );
        // Determinism (JL0003).
        assert_eq!(a, wiring_rs(&d), "owner derivation is deterministic");
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

    /// #229: a per-user (owner-scoped) changes entity gets a cross-USER negative
    /// control ("user B"), NOT the mislabeled cross-tenant one — so #216's
    /// owner-scoping boundary is framed by an accurately-named generated control.
    /// The control stays `#[ignore]`d (Changes need live Postgres); the live gate
    /// (publish.sh / heavy.yml, #227) plus the pure-filter test
    /// `channel::per_user_change_is_visible_only_to_its_owner` provide the assertion.
    #[test]
    fn per_user_changes_entity_emits_a_cross_user_negative_control() {
        let d: Design = serde_json::from_str(
            r#"{
                "name": "note-app", "contract_version": 2,
                "auth": { "model": "jwt", "roles": ["member"] },
                "dependencies": ["db", "auth"],
                "realtime": { "changes": ["Note"], "broadcast": [], "presence": [] },
                "modules": [
                    { "name": "notes",
                      "entities": [{ "name": "Note",
                          "belongs_to": [{ "entity": "User" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "body", "type": "string" }] }],
                      "endpoints": [] }
                ]
            }"#,
        )
        .unwrap();
        let a = acceptance_rs(&d);
        assert!(
            a.contains("async fn cross_user_change_never_arrives_note()"),
            "a per-user changes entity must emit a cross-USER negative control: {a}"
        );
        assert!(
            a.contains("owned by user B must never reach a user-A socket"),
            "the control must be framed cross-user (user B), not cross-tenant: {a}"
        );
        assert!(
            !a.contains("cross_tenant"),
            "a per-user entity must NOT emit the mislabeled cross-tenant control: {a}"
        );
        assert_eq!(a, acceptance_rs(&d), "deterministic (JL0003)");
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
