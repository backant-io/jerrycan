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
    "\n/// The session payload (app-wide). Generated because the design declares auth.\n#[derive(serde::Serialize, serde::Deserialize, Clone)]\npub struct SessionUser {\n    pub id: i64,\n    pub role: String,\n}\n\n/// The guard extractor handlers use: a decrypted session.\npub type CurrentUser = jerrycan::auth::Session<SessionUser>;\n"
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
        write(
            "crates/shared/src/lib.rs",
            &format!("{SHARED_LIB}{}", shared_auth_types()),
        )?;
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
