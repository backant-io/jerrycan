//! `jerrycan new`: design → complete crate-per-module workspace on disk.

use super::design::{AuthModel, Design};
use super::genroute;
use super::mounting;
use super::templates::*;
use std::fs;
use std::path::Path;

/// The session-user type + guard alias appended to the shared crate's lib.rs in
/// auth mode. `CurrentUser` is what handler stubs extract, so every module's
/// guard agrees on one app-wide session payload. The guard TYPE follows the
/// declared auth model (issue #29): `Session<SessionUser>` (cookie) under the
/// `session` model, `Bearer<SessionUser>` (`Authorization: Bearer <jwt>`) under
/// the `jwt` model — a jwt design gets REAL Bearer-token guards, not cookies.
fn shared_auth_types(design: &Design) -> String {
    // The `SessionUser` payload is identical for both models — its `id` is the
    // stringified user pk (mirrors storage's TEXT `owner_id`), so integer and
    // uuid/string identities round-trip through the session, JWT, and tenant
    // guard alike. Only the guard alias line differs by model, so the session /
    // none path stays byte-identical to before.
    let struct_block = "\n/// The session payload (app-wide). Generated because the design declares auth.\n/// `id` is the stringified user pk (mirrors storage's TEXT `owner_id`), so both\n/// integer and uuid/string user identities — e.g. a migrated Supabase\n/// `auth.users` uuid — round-trip through the session, JWT, and tenant guard.\n#[derive(serde::Serialize, serde::Deserialize, Clone)]\npub struct SessionUser {\n    pub id: String,\n    pub role: String,\n}\n\n";
    let alias = if design.auth_model() == AuthModel::Jwt {
        "/// The guard extractor handlers use: a verified `Authorization: Bearer` JWT.\npub type CurrentUser = jerrycan::auth::Bearer<SessionUser>;\n"
    } else {
        "/// The guard extractor handlers use: a decrypted session.\npub type CurrentUser = jerrycan::auth::Session<SessionUser>;\n"
    };
    format!("{struct_block}{alias}")
}

/// The membership-checked `Tenant` guard appended to shared/src/lib.rs when the
/// design declares tenancy (validation guarantees an active auth model). The
/// `tenant` factory is registered app-wide in main.rs; tenant-owned guarded
/// handlers take `Dep<Tenant>` instead of `CurrentUser`, so the membership
/// lookup (401 from a missing session, 403 from no membership row) runs before
/// the handler. The fk column / membership table match Task 10's DDL; the id
/// type follows the tenant pk (`target_key_rust_type`).
fn shared_tenancy_types(design: &Design) -> String {
    let tenant = &design.tenancy.as_ref().expect("tenancy present").entity;
    let fk_col = Design::fk_column(tenant);
    let tenant_snake = Design::to_snake(tenant);
    let id_ty = design.target_key_rust_type(tenant);
    // A text (String) pk must clone out of `&self`; a Copy integer pk returns by
    // value (a `.clone()` there would trip clippy::clone_on_copy under -D warnings).
    let id_body = if id_ty == "String" {
        "self.id.clone()"
    } else {
        "self.id"
    };
    // Parse the path fk into the tenant pk type, and bind it into the SQL. A text
    // pk is the path segment verbatim (clone once for the bind, keep for the id);
    // an integer pk parses (a non-numeric segment is a 404, not a 400 — no probing)
    // and, being Copy, needs no clone.
    let (fk_parse, fk_bind) = if id_ty == "String" {
        (
            format!("let {fk_col} = {fk_col}.to_string();"),
            format!("{fk_col}.clone().into()"),
        )
    } else {
        (
            format!(
                "let {fk_col}: {id_ty} = match {fk_col}.parse() {{\n            Ok(v) => v,\n            Err(_) => return Err(jerrycan::Error::not_found()),\n        }};"
            ),
            format!("{fk_col}.into()"),
        )
    };
    format!(
        r#"
/// The authenticated tenant context: membership-checked {tenant} + role.
#[derive(Clone, Debug)]
pub struct Tenant {{
    pub id: {id_ty},
    pub role: String,
}}

impl Tenant {{
    pub fn id(&self) -> {id_ty} {{
        {id_body}
    }}
    pub fn require_role(&self, role: &str) -> jerrycan::Result<()> {{
        jerrycan::auth::require_role(&self.role, role)
    }}
}}

/// DI guard factory — registered app-wide; path-scoped handlers take `Dep<Tenant>`.
///
/// The tenant a request acts on comes from the ROUTE, and membership in THAT
/// tenant is verified before the handler runs (issue #78): when the path names
/// the tenant fk `{fk_col}` (a nested mount like `/{tenant_snake}s/{{{fk_col}}}`),
/// the guard checks the caller belongs to the addressed tenant — a non-member is
/// `404` (no existence leak), never an arbitrary "member of something". A route
/// with NO tenant fk in its path (a storage bucket, or the tenant's own
/// collection) falls back to the caller's first membership, preserving the
/// pre-#78 scoping for those non-path-scoped consumers. 401 still comes from a
/// missing session (the `CurrentUser` arg); `require_role` is the 403 role gate.
pub async fn tenant(
    user: CurrentUser,
    db: jerrycan::Dep<jerrycan::db::Db>,
    params: jerrycan::extract::PathParams,
) -> jerrycan::Result<Tenant> {{
    use jerrycan::db::sea_orm::{{ConnectionTrait, Statement}};
    // Path-scoped: the URL carries the tenant fk — verify membership in it.
    if let Some({fk_col}) = params.get("{fk_col}") {{
        {fk_parse}
        let row = db
            .conn()
            .query_one(Statement::from_sql_and_values(
                db.conn().get_database_backend(),
                db.sql("SELECT role FROM {tenant_snake}_members WHERE user_id = ? AND {fk_col} = ?"),
                [user.0.id.into(), {fk_bind}],
            ))
            .await
            .map_err(jerrycan::db::db_error)?;
        let Some(row) = row else {{
            return Err(jerrycan::Error::not_found());
        }};
        return Ok(Tenant {{
            id: {fk_col},
            role: row.try_get("", "role").map_err(jerrycan::db::db_error)?,
        }});
    }}
    // No tenant fk in the path: resolve the caller's first membership (storage
    // buckets, the tenant's own collection). Behavior-identical to the pre-#78 guard.
    let row = db
        .conn()
        .query_one(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql("SELECT {fk_col}, role FROM {tenant_snake}_members WHERE user_id = ?"),
            [user.0.id.into()],
        ))
        .await
        .map_err(jerrycan::db::db_error)?;
    let Some(row) = row else {{
        return Err(jerrycan::Error::forbidden());
    }};
    Ok(Tenant {{
        id: row.try_get("", "{fk_col}").map_err(jerrycan::db::db_error)?,
        role: row.try_get("", "role").map_err(jerrycan::db::db_error)?,
    }})
}}
"#
    )
}

/// Canonical on-disk form of design.json (pretty, trailing newline) — both
/// scaffold and the MCP design tool write exactly this, so diffs stay clean.
pub fn canonical_design_json(design: &Design) -> String {
    let mut s = serde_json::to_string_pretty(design).expect("design serializes");
    s.push('\n');
    s
}

/// Mode-dependent policy artifacts (supply-chain gates). Called by scaffold AND
/// by mode flips (`jerrycan add`): db mode needs the rsa-advisory ignore and the
/// webpki-roots license; memory mode needs neither.
pub fn write_policy_files(root: &Path, design: &Design) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    let audit_path = root.join(".cargo/audit.toml");
    if design.wants_db() {
        // db mode pulls sqlx: deny.toml allows webpki-roots' CDLA license, and
        // audit.toml acknowledges the unfixable rsa advisory (RUSTSEC-2023-0071).
        fs::write(root.join("deny.toml"), DENY_TOML_DB)
            .map_err(|e| format!("write deny.toml: {e}"))?;
        written.push("deny.toml".to_string());
        fs::create_dir_all(audit_path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&audit_path, AUDIT_TOML).map_err(|e| format!("write .cargo/audit.toml: {e}"))?;
        written.push(".cargo/audit.toml".to_string());
    } else {
        fs::write(root.join("deny.toml"), DENY_TOML)
            .map_err(|e| format!("write deny.toml: {e}"))?;
        written.push("deny.toml".to_string());
        // A prior db-mode app that flipped back to memory must shed the ignore.
        if audit_path.exists() {
            fs::remove_file(&audit_path).map_err(|e| format!("remove .cargo/audit.toml: {e}"))?;
            written.push(".cargo/audit.toml".to_string());
        }
    }
    Ok(written)
}

pub fn scaffold(target: &Path, design: &Design) -> Result<Vec<String>, String> {
    if target.exists()
        && target
            .read_dir()
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!(
            "target directory {} is not empty",
            target.display()
        ));
    }
    let mut created = Vec::new();
    let mut write = |rel: &str, content: &str| -> Result<(), String> {
        let path = target.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&path, content).map_err(|e| format!("write {rel}: {e}"))?;
        created.push(rel.to_string());
        Ok(())
    };

    let features = design.facade_features();
    let dep_line = jerrycan_dep_spec(&features);
    write(
        "Cargo.toml",
        &render(
            WORKSPACE_CARGO,
            &[("members", ""), ("jerrycan_dep", &dep_line)],
        )?,
    )?;
    write(
        "jerrycan.toml",
        &render(JERRYCAN_TOML, &[("name", &design.name)])?,
    )?;
    write(".gitignore", GITIGNORE)?;
    write("design.json", &canonical_design_json(design))?;
    write(
        "crates/app/Cargo.toml",
        &render(APP_CARGO, &[("route_deps", "")])?,
    )?;
    // Auth mode: the shared crate gains `jerrycan` (for the Session alias) and a
    // session-user type + CurrentUser alias all guards across modules agree on.
    if design.wants_auth() {
        write("crates/shared/Cargo.toml", SHARED_CARGO_AUTH)?;
        // Tenancy adds the membership-checked Tenant guard after the auth types
        // (validation guarantees an active auth model alongside tenancy). The
        // shared crate inherits the workspace `jerrycan` features (incl. `db`)
        // via `jerrycan.workspace = true`, so `jerrycan::db::Db` resolves here.
        let mut lib = format!("{SHARED_LIB}{}", shared_auth_types(design));
        if design.tenancy.is_some() {
            lib.push_str(&shared_tenancy_types(design));
        }
        write("crates/shared/src/lib.rs", &lib)?;
    } else {
        write("crates/shared/Cargo.toml", SHARED_CARGO)?;
        write("crates/shared/src/lib.rs", SHARED_LIB)?;
    }
    // `write` (which borrows `created`) is unused past here, so NLL ends the
    // borrow and policy files can extend `created` directly.
    created.extend(write_policy_files(target, design)?);

    let mode = genroute::GenMode {
        db: design.wants_db(),
        auth: design.wants_auth(),
    };
    let routes_dir = target.join("crates/routes");
    for m in &design.modules {
        created.extend(genroute::write_module(&routes_dir, m, mode, design)?);
    }
    created.extend(mounting::regenerate(target, design)?);
    created.sort();
    created.dedup();
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tenancy design (Club tenant, integer pk) — enough to render the guard.
    fn clubs_design() -> Design {
        serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "clubs",
                      "entities": [{ "name": "Club", "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "name", "type": "string" } ]}],
                      "endpoints": [
                          { "operation_id": "get_club", "method": "GET", "path": "/{club_id}",
                            "success": { "status": 200, "entity": "Club" } }] }
                ] }"#,
        )
        .unwrap()
    }

    /// The generated `Tenant` guard must be MEMBERSHIP-VERIFIED against the ROUTE,
    /// not resolved from an arbitrary first membership (issue #78). When the path
    /// names the tenant fk it reads it BY NAME (via `PathParams`, not the leaf-only
    /// `Path<T>`), queries the membership row for THAT tenant, and 404s on a miss
    /// (no existence leak) — never 403. `require_role` stays the 403 role gate.
    #[test]
    fn tenant_guard_is_path_membership_verified_with_404() {
        let guard = shared_tenancy_types(&clubs_design());
        // The factory takes the additive `PathParams` extractor and reads the fk by name.
        assert!(
            guard.contains("params: jerrycan::extract::PathParams"),
            "factory must take PathParams:\n{guard}"
        );
        assert!(
            guard.contains("params.get(\"club_id\")"),
            "guard reads the tenant fk from the path by name:\n{guard}"
        );
        // Membership is verified for the PATH tenant, not "member of anything".
        assert!(
            guard.contains("WHERE user_id = ? AND club_id = ?"),
            "guard verifies membership in the path tenant:\n{guard}"
        );
        // A membership miss on the path tenant is 404 (no existence leak), not 403.
        assert!(
            guard.contains("Error::not_found()"),
            "path-tenant miss is 404:\n{guard}"
        );
        // The role gate is unchanged (403 path).
        assert!(
            guard.contains("jerrycan::auth::require_role(&self.role, role)"),
            "require_role stays the 403 gate:\n{guard}"
        );
    }
}
