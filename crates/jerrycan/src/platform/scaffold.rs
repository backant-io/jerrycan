//! `jerrycan new`: design → complete crate-per-module workspace on disk.

use super::design::Design;
use super::genroute;
use super::mounting;
use super::templates::*;
use std::fs;
use std::path::Path;

/// The session-user type + guard alias appended to the shared crate's lib.rs in
/// auth mode. `CurrentUser` is what handler stubs extract, so every module's
/// guard agrees on one app-wide session payload.
fn shared_auth_types() -> &'static str {
    "\n/// The session payload (app-wide). Generated because the design declares auth.\n/// `id` is the stringified user pk (mirrors storage's TEXT `owner_id`), so both\n/// integer and uuid/string user identities — e.g. a migrated Supabase\n/// `auth.users` uuid — round-trip through the session, JWT, and tenant guard.\n#[derive(serde::Serialize, serde::Deserialize, Clone)]\npub struct SessionUser {\n    pub id: String,\n    pub role: String,\n}\n\n/// The guard extractor handlers use: a decrypted session.\npub type CurrentUser = jerrycan::auth::Session<SessionUser>;\n"
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

/// DI guard factory — registered app-wide; handlers take `Dep<Tenant>`.
/// Resolves the caller's membership or rejects 403 before the handler runs.
pub async fn tenant(
    user: CurrentUser,
    db: jerrycan::Dep<jerrycan::db::Db>,
) -> jerrycan::Result<Tenant> {{
    use jerrycan::db::sea_orm::{{ConnectionTrait, Statement}};
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
        let mut lib = format!("{SHARED_LIB}{}", shared_auth_types());
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
