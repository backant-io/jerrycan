//! jerrycan-specific lints (spec §5.3 ring 3). v0 set:
//! JL0001 route-crate lib.rs exports more than `module()`
//! JL0002 handler names don't match design operation_ids
//! JL0003 a generated (tool-owned) file was hand-edited
//! JL0004 an auth design leaves a mutating route unguarded
//! JL0006 a tenant-owned handler calls an UNSCOPED repo method (cross-tenant read)

use super::checkpipe::Diagnostic;
use super::design::{Design, HttpMethod, ModuleDesign};
use super::mounting;
use std::collections::BTreeSet;
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
    lint_unguarded_mutations(design, &mut out);
    lint_unscoped_tenant_queries(root, design, &mut out);
    out
}

/// JL0006: a handler in a module that owns tenant-owned entities calls an
/// UNSCOPED repo method (`repo.all()`, `repo.get(`, `repo.remove(`, `repo.update(`).
/// Those read/delete across ALL tenants, so a tenant could reach another tenant's
/// rows. The scoped accessors (`all_for`/`get_for`/`remove_for`) are excluded by
/// the `(` anchor (e.g. `repo.get_for(` has `_` not `(` after `get`).
///
/// We scan ONLY the agent-owned handlers.rs (where the call happens), never
/// repo.rs: the generated repo's own scoped methods call `Entity::...` directly,
/// not `self.all()`, so repo.rs never legitimately matches — and scanning it
/// would flag the unscoped methods the scoped ones are meant to replace.
fn lint_unscoped_tenant_queries(root: &Path, design: &Design, out: &mut Vec<Diagnostic>) {
    // The set of modules that own at least one tenant-owned entity.
    let modules: BTreeSet<&str> = design.tenant_owned().into_iter().map(|(m, _)| m).collect();
    // `repo.all()` takes no args; the others anchor on `(` so `*_for(` is excluded.
    const PATTERNS: [&str; 4] = ["repo.all()", "repo.get(", "repo.remove(", "repo.update("];
    for module in modules {
        let rel = format!("crates/routes/{module}/src/handlers.rs");
        let Ok(content) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            let Some(hit) = PATTERNS.iter().find(|p| line.contains(**p)) else {
                continue;
            };
            out.push(d(
                "JL0006",
                Some(rel.clone()),
                Some(i as u64 + 1),
                format!(
                    "handler in module `{module}` calls the unscoped `{hit}` on a tenant-owned repo — it can read or delete another tenant's rows"
                ),
                "call the tenant-scoped accessor (all_for/get_for/remove_for) with the current tenant's id",
                "jerrycan docs database",
            ));
        }
    }
}

/// JL0004: in an auth design, a mutating route (POST/PUT/PATCH/DELETE) whose
/// design endpoint is NOT guarded (no auth_required, no required_roles), is not
/// marked `public` (the credential-issuing carve-out — login/register can't hold
/// a session yet), AND does not carry its own signature authentication (the
/// webhook exemption — see `Endpoint::declares_signature_auth`).
fn lint_unguarded_mutations(design: &Design, out: &mut Vec<Diagnostic>) {
    if !design.wants_auth() {
        return;
    }
    fn walk(m: &ModuleDesign, out: &mut Vec<Diagnostic>) {
        for ep in &m.endpoints {
            let mutating = matches!(
                ep.method,
                HttpMethod::POST | HttpMethod::PUT | HttpMethod::PATCH | HttpMethod::DELETE
            );
            if mutating && !ep.is_guarded() && !ep.public && !ep.declares_signature_auth() {
                out.push(d(
                    "JL0004",
                    Some("design.json".into()),
                    None,
                    format!(
                        "mutating route `{}` in module `{}` has no auth guard (design declares auth)",
                        ep.operation_id, m.name
                    ),
                    "set auth_required: true or required_roles in design.json",
                    "jerrycan docs auth",
                ));
            }
        }
        for sub in &m.subroutes {
            walk(sub, out);
        }
    }
    for m in &design.modules {
        walk(m, out);
    }
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
        // Substring match can be fooled by commented-out handlers; the build class is the real guarantee (lib.rs references handlers::<op>).
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

/// JL0003: tool-owned app/src/main.rs (and, in db mode, app/src/migrations.rs)
/// must equal the regenerator's output exactly.
fn lint_generated_drift(root: &Path, design: &Design, out: &mut Vec<Diagnostic>) {
    let drift = d(
        "JL0003",
        Some("crates/app/src/main.rs".into()),
        None,
        "generated file drifted from the design (hand-edited, or design.json changed without regenerating)".into(),
        "run `jerrycan generate route <module>` to regenerate mounting; never hand-edit GENERATED files",
        "jerrycan docs app#anti-patterns",
    );
    let main_rel = "crates/app/src/main.rs";
    let on_disk = std::fs::read_to_string(root.join(main_rel)).unwrap_or_default();
    if on_disk != mounting::expected_main(design) {
        out.push(drift);
    }

    if design.wants_db()
        && let Ok(Some(expected)) = mounting::expected_migrations_rs(root, design)
    {
        let mig_rel = "crates/app/src/migrations.rs";
        let on_disk = std::fs::read_to_string(root.join(mig_rel)).unwrap_or_default();
        if on_disk != expected {
            out.push(d(
                "JL0003",
                Some(mig_rel.into()),
                None,
                "generated file drifted from the design (hand-edited, or migrations changed without regenerating)".into(),
                "run `jerrycan generate route <module>` to regenerate the migration aggregate; never hand-edit GENERATED files",
                "jerrycan docs app#anti-patterns",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A design with tenancy whose `leads` module owns a tenant-owned entity, so
    /// JL0006 scans crates/routes/leads/src/handlers.rs. (Reuses the design.rs
    /// V1_FULL fixture, where Lead belongs_to the Workspace tenancy.)
    fn tenant_design() -> Design {
        serde_json::from_str(super::super::design::tests::V1_FULL).unwrap()
    }

    /// JL0006 flags the UNSCOPED `repo.get(` call on a tenant-owned module's
    /// handler, and only that line — the clean `repo.get_for(...)` accessor (which
    /// the `(` anchor distinguishes from `get(`) must NOT be flagged. WHY: this is
    /// the cross-tenant-read guard; a false positive on the scoped call would make
    /// the correct fix un-passable.
    #[test]
    fn jl0006_flags_unscoped_repo_call_not_the_scoped_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/leads/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        // Line 3 is the offender; line 5 is the clean scoped accessor.
        let content = "\
use super::repo::*;
async fn show_lead(repo: Dep<LeadRepo>) -> Result<()> {
    let leaked = repo.get(id).await?;
    let _ = leaked;
    let scoped = repo.get_for(tenant.id(), id).await?;
    Ok(())
}
";
        std::fs::write(&handlers, content).unwrap();

        let design = tenant_design();
        let hits = jl0006_only(root, &design);
        assert_eq!(
            hits.len(),
            1,
            "exactly one unscoped call, the scoped one is clean: {hits:?}"
        );
        let only = &hits[0];
        assert_eq!(only.code, "JL0006");
        assert_eq!(only.line, Some(3), "must point at the `repo.get(` line");
        assert!(
            only.file
                .as_deref()
                .unwrap()
                .contains("leads/src/handlers.rs"),
            "{only:?}"
        );
        assert!(
            only.suggestion
                .as_deref()
                .unwrap()
                .contains("all_for/get_for/remove_for"),
            "carries the registered fix text: {only:?}"
        );
    }

    /// A module with no unscoped calls (only scoped accessors) produces no JL0006.
    #[test]
    fn jl0006_silent_when_handlers_use_scoped_accessors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/leads/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn list_leads(repo: Dep<LeadRepo>) -> Result<()> {\n    let _ = repo.all_for(tenant.id()).await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        let design = tenant_design();
        assert!(
            jl0006_only(root, &design).is_empty(),
            "scoped-only handlers are clean"
        );
    }

    /// Run the full lint pass and keep only JL0006 (the other lints fire on the
    /// absent lib.rs/main.rs in this bare fixture — irrelevant to this check).
    fn jl0006_only(root: &Path, design: &Design) -> Vec<Diagnostic> {
        run(root, design)
            .into_iter()
            .filter(|d| d.code == "JL0006")
            .collect()
    }

    /// A minimal auth design with one mutating module endpoint; the test mutates
    /// just that endpoint's guard/error shape to probe JL0004 in isolation.
    fn auth_design_with_endpoint(endpoint: serde_json::Value) -> Design {
        serde_json::from_value(serde_json::json!({
            "name": "billing-api",
            "contract_version": 1,
            "auth": { "model": "jwt", "roles": ["owner"] },
            "dependencies": ["auth"],
            "modules": [{
                "name": "billing",
                "endpoints": [endpoint]
            }]
        }))
        .unwrap()
    }

    /// Only JL0004 diagnostics from a full pass (other lints fire on the absent
    /// crate files in these bare in-memory designs).
    fn jl0004_only(design: &Design) -> Vec<Diagnostic> {
        let tmp = tempfile::tempdir().unwrap();
        run(tmp.path(), design)
            .into_iter()
            .filter(|d| d.code == "JL0004")
            .collect()
    }

    /// JL0004 must NOT flag a signature-authenticated webhook: a POST with no JWT
    /// guard but a declared `4xx … signature …` error carries its own auth, so the
    /// lint treats it as guarded. WHY (Rule 9): this is the Stripe-webhook contract
    /// — a third party signs the payload because it can't hold the app's session;
    /// flagging it would force a JWT guard that makes the webhook unreachable.
    #[test]
    fn jl0004_exempts_a_signature_authenticated_webhook() {
        let design = auth_design_with_endpoint(serde_json::json!({
            "operation_id": "stripe_webhook",
            "method": "POST",
            "path": "/webhook",
            "success": { "status": 200 },
            "errors": [{ "status": 400, "when": "Stripe signature is missing or invalid" }]
        }));
        assert!(
            jl0004_only(&design).is_empty(),
            "a signature-authed webhook is intentionally not JWT-guarded"
        );
    }

    /// JL0004 must NOT flag a PUBLIC mutating route: a credential-issuing login/
    /// register POST is genuinely unauthenticated (it has no session yet to guard
    /// by), so `public: true` is its carve-out. WHY (Rule 9): this is fix F1 — an
    /// auth design could not declare its own login/register without JL0004 firing
    /// with no escape; the flag lets a public route declare itself unguarded ON
    /// PURPOSE while the lint stays sharp on everything else.
    #[test]
    fn jl0004_exempts_a_public_credential_issuing_route() {
        let design = auth_design_with_endpoint(serde_json::json!({
            "operation_id": "register",
            "method": "POST",
            "path": "/register",
            "public": true,
            "success": { "status": 201 },
            "errors": [{ "status": 422, "when": "request body fails validation" }]
        }));
        assert!(
            jl0004_only(&design).is_empty(),
            "a public credential-issuing route is intentionally unguarded"
        );
    }

    /// The same endpoint WITHOUT `public` trips JL0004 — the exemption is the flag,
    /// nothing else (pairs with the public test above, like the signature-auth pair).
    #[test]
    fn jl0004_flags_the_same_route_without_public() {
        let design = auth_design_with_endpoint(serde_json::json!({
            "operation_id": "register",
            "method": "POST",
            "path": "/register",
            "success": { "status": 201 },
            "errors": [{ "status": 422, "when": "request body fails validation" }]
        }));
        let hits = jl0004_only(&design);
        assert_eq!(
            hits.len(),
            1,
            "without public, an unguarded mutation still trips JL0004: {hits:?}"
        );
        assert!(hits[0].message.contains("register"), "{:?}", hits[0]);
    }

    /// The exemption is narrow: a plain unguarded mutation (no guard, no signature
    /// error) still trips JL0004, so the lint stays sharp against forgotten guards.
    #[test]
    fn jl0004_still_flags_a_plain_unguarded_mutation() {
        let design = auth_design_with_endpoint(serde_json::json!({
            "operation_id": "create_charge",
            "method": "POST",
            "path": "/charges",
            "success": { "status": 201 },
            "errors": [{ "status": 400, "when": "request body is malformed" }]
        }));
        let hits = jl0004_only(&design);
        assert_eq!(
            hits.len(),
            1,
            "a non-signature 400 is no exemption: {hits:?}"
        );
        assert!(hits[0].message.contains("create_charge"), "{:?}", hits[0]);
    }
}
