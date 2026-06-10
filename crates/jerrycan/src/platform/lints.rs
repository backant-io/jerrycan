//! jerrycan-specific lints (spec §5.3 ring 3). v0 set:
//! JL0001 route-crate lib.rs exports more than `module()`
//! JL0002 handler names don't match design operation_ids
//! JL0003 a generated (tool-owned) file was hand-edited

use super::checkpipe::Diagnostic;
use super::design::{Design, ModuleDesign};
use super::mounting;
use std::path::Path;

fn d(
    code: &str,
    file: Option<String>,
    line: Option<u64>,
    message: String,
    suggestion: &str,
    doc: &str,
) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        file,
        line,
        message,
        suggestion: Some(suggestion.into()),
        doc_url: Some(doc.into()),
    }
}

pub fn run(root: &Path, design: &Design) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for m in &design.modules {
        lint_public_surface(root, m, &mut out);
        lint_handlers(root, m, &format!("crates/routes/{}/src", m.name), &mut out);
    }
    lint_generated_drift(root, design, &mut out);
    out
}

/// JL0001: scan a route crate's lib.rs for public items besides `pub fn module()`.
fn lint_public_surface(root: &Path, m: &ModuleDesign, out: &mut Vec<Diagnostic>) {
    let rel = format!("crates/routes/{}/src/lib.rs", m.name);
    let Ok(content) = std::fs::read_to_string(root.join(&rel)) else {
        return;
    };
    for (i, line) in content.lines().enumerate() {
        let t = line.trim_start();
        if !t.starts_with("pub ") || t.starts_with("pub(") {
            continue;
        }
        if t.starts_with("pub fn module(") {
            continue;
        }
        out.push(d(
            "JL0001",
            Some(rel.clone()),
            Some(i as u64 + 1),
            format!(
                "route crate `{}` exports more than `module()`: `{}`",
                m.name,
                t.trim_end()
            ),
            "make it pub(crate), move shared types to the shared crate, or expose via module()",
            "jerrycan docs modules#anti-patterns",
        ));
    }
}

/// JL0002: every design endpoint needs `async fn <operation_id>(` in its unit's handlers.rs.
fn lint_handlers(root: &Path, m: &ModuleDesign, src_rel: &str, out: &mut Vec<Diagnostic>) {
    let rel = format!("{src_rel}/handlers.rs");
    let content = std::fs::read_to_string(root.join(&rel)).unwrap_or_default();
    for ep in &m.endpoints {
        if !content.contains(&format!("async fn {}(", ep.operation_id)) {
            out.push(d(
                "JL0002",
                Some(rel.clone()),
                None,
                format!(
                    "handler `{}` (from design.json) is missing in {rel}",
                    ep.operation_id
                ),
                "add the handler with that exact name, or fix the design's operation_id",
                "jerrycan docs modules",
            ));
        }
    }
    for sub in &m.subroutes {
        lint_handlers(
            root,
            sub,
            &format!("{src_rel}/subroutes/{}", sub.name.replace('-', "_")),
            out,
        );
    }
}

/// JL0003: tool-owned app/src/main.rs must equal the regenerator's output exactly.
fn lint_generated_drift(root: &Path, design: &Design, out: &mut Vec<Diagnostic>) {
    let rel = "crates/app/src/main.rs";
    let on_disk = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
    if on_disk != mounting::expected_main(design) {
        out.push(d(
            "JL0003",
            Some(rel.into()),
            None,
            "generated file drifted from the design (hand-edited, or design.json changed without regenerating)".into(),
            "run `jerrycan generate route <module>` to regenerate mounting; never hand-edit GENERATED files",
            "jerrycan docs app#anti-patterns",
        ));
    }
}
