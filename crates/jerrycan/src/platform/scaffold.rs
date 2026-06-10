//! `jerrycan new`: design → complete crate-per-module workspace on disk.

use super::design::Design;
use super::genroute;
use super::mounting;
use super::templates::*;
use std::fs;
use std::path::Path;

/// Canonical on-disk form of design.json (pretty, trailing newline) — both
/// scaffold and the MCP design tool write exactly this, so diffs stay clean.
pub fn canonical_design_json(design: &Design) -> String {
    let mut s = serde_json::to_string_pretty(design).expect("design serializes");
    s.push('\n');
    s
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

    let dep_line = jerrycan_dep_spec();
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
    write("crates/shared/Cargo.toml", SHARED_CARGO)?;
    write("crates/shared/src/lib.rs", SHARED_LIB)?;

    let routes_dir = target.join("crates/routes");
    for m in &design.modules {
        created.extend(genroute::write_module(&routes_dir, m)?);
    }
    created.extend(mounting::regenerate(target, design)?);
    created.sort();
    created.dedup();
    Ok(created)
}
