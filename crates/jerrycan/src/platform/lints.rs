//! jerrycan-specific lints (spec §5.3 ring 3). v0 set:
//! JL0001 route-crate lib.rs exports more than `module()`
//! JL0002 handler names don't match design operation_ids
//! JL0003 a generated (tool-owned) file was hand-edited
//! JL0004 an auth design leaves a mutating route unguarded
//! JL0006 a tenant-owned handler calls an UNSCOPED repo method (cross-tenant read)
//! JL0007 agent-owned module code reaches outside the request boundary (process/fs/net)
//! JL0008 a tenant-owned handler could not be read/parsed, so its scoping is unverified

use super::checkpipe::Diagnostic;
use super::design::{Design, HandlerRef, HttpMethod, ModuleDesign};
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
    lint_boundary_escapes(root, design, &mut out);
    out
}

/// JL0007: agent-owned module code that reaches outside the request boundary —
/// process spawning, filesystem, or raw network. Handler code is agent-authored
/// untrusted input (see the threat model); the framework's contract is that I/O
/// goes through its extensions, not direct std::/tokio:: process/fs/net calls.
///
/// We scan the whole agent-owned file set of every module and subroute
/// (handlers.rs, repo.rs, deps.rs, model.rs) for the needles below. A line whose
/// trimmed start is `//` is prose, not code, and is skipped; a line ending in the
/// allow-hatch suffix is an explicit, line-scoped opt-out and is not flagged.
fn lint_boundary_escapes(root: &Path, design: &Design, out: &mut Vec<Diagnostic>) {
    const NEEDLES: [&str; 6] = [
        "std::process::",
        "std::fs::",
        "std::net::",
        "tokio::process::",
        "tokio::fs::",
        "tokio::net::",
    ];
    const ALLOW: &str = "// jerrycan:allow JL0007";
    const FILES: [&str; 4] = ["handlers.rs", "repo.rs", "deps.rs", "model.rs"];

    // Every agent-owned file, relative to root, across modules and subroutes.
    let mut rels: Vec<String> = Vec::new();
    fn collect(src_rel: &str, m: &ModuleDesign, files: &[&str], rels: &mut Vec<String>) {
        for f in files {
            rels.push(format!("{src_rel}/{f}"));
        }
        for sub in &m.subroutes {
            collect(
                &format!("{src_rel}/subroutes/{}", sub.name.replace('-', "_")),
                sub,
                files,
                rels,
            );
        }
    }
    for m in &design.modules {
        collect(
            &format!("crates/routes/{}/src", m.name),
            m,
            &FILES,
            &mut rels,
        );
    }

    for rel in rels {
        let Ok(content) = std::fs::read_to_string(root.join(&rel)) else {
            continue; // model.rs/repo.rs are absent in memory mode; that's fine
        };
        for (i, line) in content.lines().enumerate() {
            // A whole-line comment is prose, not code.
            if line.trim_start().starts_with("//") {
                continue;
            }
            if !NEEDLES.iter().any(|n| line.contains(n)) {
                continue;
            }
            // Line-scoped escape hatch.
            if line.trim_end().ends_with(ALLOW) {
                continue;
            }
            out.push(d(
                "JL0007",
                Some(rel.clone()),
                Some(i as u64 + 1),
                "handler code reaches outside the request boundary (process/fs/net)".into(),
                "use framework extensions for I/O; if this is genuinely intended, append `// jerrycan:allow JL0007` to the line",
                "jerrycan docs errors",
            ));
        }
    }
}

/// JL0006: a handler in an OWNER-SCOPED module calls an UNSCOPED repo method
/// (`repo.all()`, `repo.get(…)`, `repo.remove(…)`, `repo.update(…)`, and — on a
/// FLAT tenant module only — `repo.insert(…)`). Those read/write/delete across ALL
/// owners, leaking rows:
///   - a TENANT-owned module — an entity that resolves to a tenant path directly OR
///     transitively (#102: a grandchild through a parent chain) → another tenant's
///     rows;
///   - a per-user IDENTITY-owned module (#79 — an entity belongs_to the auth
///     identity, not tenant-scoped) → another user's rows. For these the unscoped
///     methods are additionally NOT generated (genroute make-impossible), so this
///     lint is belt-and-suspenders: it gives a precise, actionable fix instead of a
///     raw `no method all` compile error.
///
/// Detection is AST-based (`syn::parse_file` + a `Visit` walk), not a substring
/// scan (issue #103): the substring scan missed a call split across lines
/// (`repo\n  .all()`) and could be fooled by a rename/alias, and — worse — it built
/// a FLAT path (`crates/routes/{module}/src/handlers.rs`) for every module, so a
/// NESTED or transitively-owned handler resolved to a nonexistent file and was
/// silently skipped (the hole that let the #102 leak ship). The path now comes from
/// [`Design::tenant_owned_handlers`], which nests `subroutes/{seg}` exactly as the
/// scaffold writes them. A mention of `repo.all()` in a COMMENT is not a call, so
/// the AST never flags it; the `*_for`/`*_for_memberships` scoped accessors are
/// excluded for free (they are different method idents). `syn::visit` does not
/// descend into MACRO token streams, so the walk additionally scans each macro's
/// raw tokens for those same needles (`UnscopedVisitor::scan_macro`) — otherwise an
/// unscoped call wrapped in `json!`/`format!`/… would evade the lint (the pre-branch
/// substring scanner caught single-line macro-wrapped calls; this restores that).
///
/// `repo.insert(…)` is flagged ONLY on a FLAT (membership-set) tenant module (#94):
/// there the create reads the tenant fk from the request BODY, so a bare insert
/// trusts it (the create leak); the fix is `create_for_memberships`. A path-scoped
/// create pins the fk to the verified tenant, and a per-user create gets the
/// server-injected identity fk — both safe — so insert is not flagged there.
///
/// A line ending in `// jerrycan:allow JL0006` (the call's own source line) is an
/// explicit, line-scoped opt-out (e.g. a create that pins the fk to a
/// membership-verified value) — same hatch JL0007 offers.
///
/// We scan ONLY the agent-owned handlers.rs (where the call happens), never
/// repo.rs: the generated repo's own scoped methods call `Entity::...` directly,
/// not `self.all()`, so repo.rs never legitimately matches — and scanning it
/// would flag the unscoped methods the scoped ones are meant to replace.
///
/// FAIL LOUD: a tenant-owned handler that is missing, unreadable, or does NOT parse
/// becomes a [`JL0008`](scan_unscoped) diagnostic — never a silent skip, which is
/// exactly how #103 hid a leak. Per-user identity handlers are not part of that
/// hole and are skipped quietly when absent (memory-mode designs have no files).
fn lint_unscoped_tenant_queries(root: &Path, design: &Design, out: &mut Vec<Diagnostic>) {
    // Tenant-owned handlers (transitive #102, nested-path-aware #103) scan LOUD.
    let tenant = design.tenant_owned_handlers();
    let covered: BTreeSet<&str> = tenant.iter().map(|h| h.rel_path.as_str()).collect();
    for h in &tenant {
        scan_unscoped(root, h, true, true, out);
    }
    // Per-user IDENTITY-owned modules (#79) leak ACROSS USERS. Top-level only, flat
    // path, NOT fail-loud (not part of the #103 tenant hole). Skip any module a
    // tenant handler already covers at the same path.
    for module in identity_owned_modules(design) {
        let rel = format!("crates/routes/{module}/src/handlers.rs");
        if covered.contains(rel.as_str()) {
            continue;
        }
        // public_read (#105): when EVERY per-user entity this module owns is
        // public_read, its unscoped `repo.all()`/`get(` READS are legitimate (the
        // repo emits them for the public GETs) — restrict the needles to the
        // writes. A MIXED module (any non-public per-user entity) keeps the read
        // needles: the scan can't tell which repo an unscoped call targets, so it
        // stays conservative (the line-scoped allow-hatch covers the false
        // positive; a missed real read leak would have nothing).
        let reads_public = design
            .modules
            .iter()
            .find(|m| m.name == module)
            .is_some_and(|m| {
                m.entities
                    .iter()
                    .filter(|e| design.entity_is_per_user_owned(e))
                    .all(|e| design.entity_is_public_read(&e.name))
            });
        let h = HandlerRef {
            rel_path: rel,
            is_flat: false,
            owned_desc: "an identity-owned",
            leak_desc: "another user's rows",
            suggestion: if reads_public {
                "route the write through the owner-scoped accessors (update_for/remove_for) with the session user's id (_user.0.id); reads are public on this public_read module".to_string()
            } else {
                "call the owner-scoped accessor (all_for/get_for/remove_for) with the session user's id (_user.0.id)".to_string()
            },
        };
        scan_unscoped(root, &h, false, !reads_public, out);
    }
}

/// Read one handler file and flag every unscoped repo call in it. `fail_loud`
/// (tenant-owned handlers) turns a missing, unreadable, or unparseable file into a
/// LOUD JL0008 instead of a silent skip (issue #103) — a handler whose scoping
/// cannot be checked is exactly where an unscoped cross-tenant call would hide.
/// `flag_reads` is false ONLY for a public_read per-user module (#105), where the
/// unscoped `repo.all()`/`get(` reads are the generated public surface — the
/// write needles always stay armed.
fn scan_unscoped(
    root: &Path,
    h: &HandlerRef,
    fail_loud: bool,
    flag_reads: bool,
    out: &mut Vec<Diagnostic>,
) {
    let content = match std::fs::read_to_string(root.join(&h.rel_path)) {
        Ok(c) => c,
        Err(_) => {
            if fail_loud {
                out.push(jl0008(&h.rel_path));
            }
            return;
        }
    };
    let ast = match syn::parse_file(&content) {
        Ok(f) => f,
        Err(_) => {
            if fail_loud {
                out.push(jl0008(&h.rel_path));
            }
            return;
        }
    };
    let src: Vec<&str> = content.lines().collect();
    let mut v = UnscopedVisitor {
        hits: Vec::new(),
        flag_insert: h.is_flat,
        flag_reads,
        src: &src,
    };
    syn::visit::Visit::visit_file(&mut v, &ast);
    for (line, call) in v.hits {
        out.push(d(
            "JL0006",
            Some(h.rel_path.clone()),
            Some(line as u64),
            format!(
                "handler calls the unscoped `repo.{call}` on {} repo — it can read, write, or delete {}",
                h.owned_desc, h.leak_desc
            ),
            &h.suggestion,
            "jerrycan docs database",
        ));
    }
}

/// JL0008: a tenant-owned handler could not be read/parsed, so its scoping is
/// UNVERIFIED. Loud on purpose — the #103 regression was a silent skip in exactly
/// this spot.
fn jl0008(rel: &str) -> Diagnostic {
    d(
        "JL0008",
        Some(rel.to_string()),
        None,
        format!(
            "tenant-owned handler `{rel}` could not be scanned for scoping — it is missing, unreadable, or not valid Rust, so an unscoped cross-tenant call could pass unseen"
        ),
        "ensure the handler file exists and compiles (run `cargo check`); a scaffold is generated parseable — if you hand-edited it, fix the syntax so `jerrycan check` can verify tenant scoping",
        "jerrycan docs database",
    )
}

/// True when `expr` is (syntactically) the `repo` binding — a bare `repo` path,
/// possibly wrapped in parens/refs/groups. A genuinely aliased binding falls
/// through to no-hit (acceptable: the steering trains `repo.` usage).
fn receiver_is_repo(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Path(p) => p.path.is_ident("repo"),
        syn::Expr::Paren(p) => receiver_is_repo(&p.expr),
        syn::Expr::Group(g) => receiver_is_repo(&g.expr),
        syn::Expr::Reference(r) => receiver_is_repo(&r.expr),
        _ => false,
    }
}

/// Walks a parsed handler file for `repo.<unscoped>(…)` calls. Exact method-name
/// matching excludes every `*_for`/`*_for_memberships` scoped accessor for free
/// (they are different idents). `insert` is flagged only on a FLAT tenant module
/// (#94). Each hit records the call's real source line (via `span-locations`), and
/// a `// jerrycan:allow JL0006` on that line is an explicit, line-scoped opt-out.
///
/// `syn::visit` does NOT descend into MACRO token streams, so an unscoped call
/// wrapped in a macro (e.g. `Json(serde_json::json!({ "items": repo.all().await? }))`
/// — a tenant-owned handler that returns every tenant's rows) is invisible to the
/// method-call walk. The `visit_*_macro` arms below close that gap by scanning each
/// macro's raw tokens for the same needles the pre-branch substring scanner caught.
struct UnscopedVisitor<'a> {
    hits: Vec<(usize, &'static str)>,
    flag_insert: bool,
    /// False only on a public_read per-user module (#105): the unscoped
    /// `all`/`get` reads are the legitimate public surface there, so only the
    /// write needles stay armed.
    flag_reads: bool,
    src: &'a [&'a str],
}

impl<'ast> syn::visit::Visit<'ast> for UnscopedVisitor<'_> {
    fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
        let name = c.method.to_string();
        // `all` takes no args (the scoped accessors carry the owner id); the others
        // match on the exact ident, so `all_for`/`get_for`/… never match.
        let display = match name.as_str() {
            "all" if c.args.is_empty() && self.flag_reads => Some("all()"),
            "get" if self.flag_reads => Some("get(...)"),
            "remove" => Some("remove(...)"),
            "update" => Some("update(...)"),
            "insert" if self.flag_insert => Some("insert(...)"),
            _ => None,
        };
        if let Some(display) = display
            && receiver_is_repo(&c.receiver)
        {
            let line = c.method.span().start().line;
            let allowed = self
                .src
                .get(line.saturating_sub(1))
                .is_some_and(|l| l.trim_end().ends_with("// jerrycan:allow JL0006"));
            if !allowed {
                self.hits.push((line, display));
            }
        }
        // Recurse so a chain (`repo.foo().all()`) and nested calls are all visited.
        syn::visit::visit_expr_method_call(self, c);
    }

    // syn::visit stops at a macro boundary, so an unscoped `repo.<method>` call
    // inside a macro body is not reached by `visit_expr_method_call`. Scan the
    // macro's tokens for the same needles, then recurse (a macro node can itself
    // contain further exprs/stmts in non-token positions we still want to walk).
    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        self.scan_macro(&node.mac);
        syn::visit::visit_expr_macro(self, node);
    }
    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        self.scan_macro(&node.mac);
        syn::visit::visit_stmt_macro(self, node);
    }
    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        self.scan_macro(&node.mac);
        syn::visit::visit_item_macro(self, node);
    }
}

impl UnscopedVisitor<'_> {
    /// Scan a macro's raw token stream for the unscoped `repo.<method>(` calls the
    /// AST walk cannot see. `TokenStream::to_string()` inserts spacing between tokens
    /// (`repo . all ()`), so we strip whitespace first and then substring-match the
    /// SAME method set the method-call visitor flags — the trailing `(`/`()` excludes
    /// the `*_for`/`*_for_memberships` accessors exactly as the AST idents do, and
    /// `insert` is a leak only on a FLAT tenant module (#94). The macro's own source
    /// line carries the `// jerrycan:allow JL0006` hatch, same as the non-macro path.
    fn scan_macro(&mut self, mac: &syn::Macro) {
        let tokens: String = mac.tokens.to_string().split_whitespace().collect();
        // (needle, display) — mirrors the match arms of `visit_expr_method_call`,
        // including the flag_reads/flag_insert config (needle order preserved so
        // multi-hit diagnostics keep their order).
        let mut needles: Vec<(&str, &'static str)> = Vec::new();
        if self.flag_reads {
            needles.push(("repo.all()", "all()"));
            needles.push(("repo.get(", "get(...)"));
        }
        needles.push(("repo.remove(", "remove(...)"));
        needles.push(("repo.update(", "update(...)"));
        if self.flag_insert {
            needles.push(("repo.insert(", "insert(...)"));
        }
        let matched: Vec<&'static str> = needles
            .iter()
            .filter(|(needle, _)| tokens.contains(needle))
            .map(|(_, display)| *display)
            .collect();
        if matched.is_empty() {
            return;
        }
        // The macro's span line: where the invocation (`serde_json::json!`) sits.
        let line = mac
            .path
            .segments
            .last()
            .map_or(1, |s| s.ident.span().start().line);
        let allowed = self
            .src
            .get(line.saturating_sub(1))
            .is_some_and(|l| l.trim_end().ends_with("// jerrycan:allow JL0006"));
        if allowed {
            return;
        }
        for display in matched {
            self.hits.push((line, display));
        }
    }
}

/// Top-level modules that own a per-user IDENTITY-owned entity (#79): an entity
/// that belongs_to the auth identity (`user_id`) and is NOT tenant-owned. Empty
/// unless the design wants auth. Classification is [`Design::entity_is_per_user_owned`]
/// — the ONE shared per-user predicate (#105 §F) — so the lint and the
/// method-suppression agree on which modules are owner-scoped.
fn identity_owned_modules(design: &Design) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    for m in &design.modules {
        let has_per_user = m
            .entities
            .iter()
            .any(|e| design.entity_is_per_user_owned(e));
        if has_per_user {
            out.insert(m.name.as_str());
        }
    }
    out
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
            "make it pub(crate), move shared types to the shared crate, or expose via module(); to reach another module's TABLE, declare a narrow second entity in your own module (jerrycan docs database)",
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

    /// A per-user (identity-owned, no tenancy) auth design: Workout belongs_to the
    /// auth identity (User → `user_id`), so its module is owner-scoped (#79).
    fn per_user_design() -> Design {
        serde_json::from_value(serde_json::json!({
            "name": "fitness-api",
            "contract_version": 1,
            "auth": { "model": "session", "roles": ["user"] },
            "dependencies": ["db", "auth"],
            "modules": [{
                "name": "workouts",
                "entities": [{
                    "name": "Workout",
                    "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                    "fields": [{ "name": "distance", "type": "float" }]
                }],
                "endpoints": [{
                    "operation_id": "list_workouts", "method": "GET", "path": "/",
                    "auth_required": true,
                    "success": { "status": 200, "entity": "Workout", "list": true }
                }]
            }]
        }))
        .unwrap()
    }

    /// JL0006 also flags the unscoped `repo.all()` on a per-user IDENTITY-owned
    /// module (#79), naming a CROSS-USER (not cross-tenant) leak and the owner-
    /// scoped fix. WHY (Rule 9): genroute already suppresses the unscoped method
    /// (a compile error), but this belt-and-suspenders lint gives the agent a
    /// precise, actionable diagnostic; the scoped `all_for(...)` stays clean.
    #[test]
    fn jl0006_flags_unscoped_call_on_a_per_user_identity_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/workouts/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn list_workouts(repo: Dep<WorkoutRepo>) -> Result<()> {\n    let _ = repo.all().await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        let hits = jl0006_only(root, &per_user_design());
        assert_eq!(
            hits.len(),
            1,
            "one unscoped call on a per-user repo: {hits:?}"
        );
        assert_eq!(hits[0].line, Some(2), "points at the `repo.all()` line");
        assert!(
            hits[0].message.contains("another user's rows"),
            "names the cross-USER leak, not cross-tenant: {:?}",
            hits[0]
        );
        assert!(
            hits[0]
                .suggestion
                .as_deref()
                .unwrap()
                .contains("_user.0.id"),
            "carries the owner-scoped fix: {:?}",
            hits[0]
        );
    }

    /// The owner-scoped accessor on a per-user module is clean — no false positive
    /// (the `(` anchor distinguishes `all_for(` from `all()`).
    #[test]
    fn jl0006_silent_on_owner_scoped_per_user_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/workouts/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn list_workouts(repo: Dep<WorkoutRepo>, _user: CurrentUser) -> Result<()> {\n    let _ = repo.all_for(_user.0.id).await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        assert!(
            jl0006_only(root, &per_user_design()).is_empty(),
            "owner-scoped per-user handler is clean"
        );
    }

    /// JL0006 flags a bare `repo.insert(` on a FLAT tenant module (#94): the flat create
    /// takes the tenant fk from the BODY, so an unchecked insert is a cross-tenant WRITE
    /// leak. WHY (Rule 9): this backstops the create steer the same way the lint already
    /// backstops read/update/delete — the fix is `create_for_memberships`. `leads` in
    /// V1_FULL is flat tenant-owned (no tenant fk in its path).
    #[test]
    fn jl0006_flags_bare_insert_on_a_flat_tenant_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/leads/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn create_lead(repo: Dep<LeadRepo>, Json(body): Json<Lead>) -> Result<()> {\n    let _ = repo.insert(body).await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        let hits = jl0006_only(root, &tenant_design());
        assert_eq!(
            hits.len(),
            1,
            "bare insert on a flat tenant module: {hits:?}"
        );
        assert_eq!(hits[0].line, Some(2), "points at the `repo.insert(` line");
        assert!(
            hits[0]
                .suggestion
                .as_deref()
                .unwrap()
                .contains("create_for_memberships"),
            "names the membership-checked create as the fix: {:?}",
            hits[0]
        );
    }

    /// The line-scoped `// jerrycan:allow JL0006` hatch is an explicit opt-out (e.g. a
    /// create that pins the tenant fk to a membership-verified value before inserting).
    /// WHY: the reference `leads`/`api-keys` create handlers do exactly that; the hatch
    /// keeps them lint-clean without suppressing the backstop for genuinely-leaky calls.
    #[test]
    fn jl0006_insert_allow_hatch_suppresses_the_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/leads/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn create_lead(repo: Dep<LeadRepo>, tenant: Dep<Tenant>) -> Result<()> {\n    let _ = repo.insert(row).await?; // jerrycan:allow JL0006\n    Ok(())\n}\n",
        )
        .unwrap();
        assert!(
            jl0006_only(root, &tenant_design()).is_empty(),
            "an explicit allow-hatch suppresses the JL0006 insert flag"
        );
    }

    /// JL0006 does NOT flag a bare `repo.insert(` on a per-user IDENTITY-owned module:
    /// a per-user create is scoped by the SERVER-injected identity fk (the DTO drops it),
    /// so the insert is safe. Flagging it would be a false positive — insert is a leak
    /// only on a FLAT tenant module, where the fk comes from the body.
    #[test]
    fn jl0006_does_not_flag_insert_on_a_per_user_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/workouts/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn create_workout(repo: Dep<WorkoutRepo>, Json(body): Json<Workout>) -> Result<()> {\n    let _ = repo.insert(body).await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        assert!(
            jl0006_only(root, &per_user_design()).is_empty(),
            "a per-user create insert is server-scoped — not a JL0006 leak"
        );
    }

    /// The per-user design with `public_read: true` on Workout (#105): reads are
    /// public (the repo emits the unscoped `all`/`get`), writes stay owner-scoped.
    fn public_read_design() -> Design {
        let mut d = per_user_design();
        d.modules[0].entities[0].public_read = true;
        d
    }

    /// Issue #105: on a module whose per-user entities are ALL `public_read`, the
    /// unscoped `repo.all()`/`repo.get(` READS are legitimate (the repo emits them
    /// for the public GETs) — JL0006 must NOT flag them, in plain calls or macro
    /// token streams. The WRITE needles keep firing: `public_read` never exempts
    /// `repo.update(`/`repo.remove(` (#79's owner-write contract). WHY (Rule 9):
    /// without the needle split the lint would false-positive every public feed
    /// handler, training agents to scatter allow-hatches — which would ALSO mute
    /// real write leaks on those same lines.
    #[test]
    fn jl0006_public_read_module_skips_reads_but_flags_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/workouts/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn list_workouts(repo: Dep<WorkoutRepo>) -> Result<()> {\n    let _ = repo.all().await?;\n    let _ = repo.get(7).await?;\n    let _ = serde_json::json!({ \"rows\": repo.all().await? });\n    Ok(())\n}\nasync fn update_workout(repo: Dep<WorkoutRepo>) -> Result<()> {\n    let _ = repo.update(7, item).await?;\n    let _ = repo.remove(7).await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        let hits = jl0006_only(root, &public_read_design());
        assert_eq!(
            hits.len(),
            2,
            "only the WRITE needles fire on a public_read module: {hits:?}"
        );
        assert_eq!(hits[0].line, Some(8), "the `repo.update(` line: {hits:?}");
        assert_eq!(hits[1].line, Some(9), "the `repo.remove(` line: {hits:?}");
        assert!(
            hits.iter().all(|h| h
                .suggestion
                .as_deref()
                .unwrap()
                .contains("update_for/remove_for")),
            "steers writes to the owner-scoped write accessors: {hits:?}"
        );
    }

    /// A MIXED module — one `public_read` entity plus one plain per-user entity —
    /// KEEPS the read needles: the lint cannot tell which repo an unscoped
    /// `repo.all()` targets, so it stays conservative (the false positive has the
    /// line-scoped allow-hatch; a missed real read leak would have nothing).
    #[test]
    fn jl0006_mixed_module_keeps_the_read_needles() {
        let mut design = public_read_design();
        design.modules[0].entities.push(
            serde_json::from_value(serde_json::json!({
                "name": "Meal",
                "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                "fields": [{ "name": "calories", "type": "integer" }]
            }))
            .unwrap(),
        );
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/workouts/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn list_meals(repo: Dep<MealRepo>) -> Result<()> {\n    let _ = repo.all().await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        let hits = jl0006_only(root, &design);
        assert_eq!(
            hits.len(),
            1,
            "a mixed module keeps flagging unscoped reads: {hits:?}"
        );
    }

    /// The `public_read` flag NEVER exempts a write: an unguarded POST in a
    /// public_read design still trips JL0004 — the #105 contract is public READ,
    /// owner WRITE, so reads-public must never bleed into writes-public.
    #[test]
    fn jl0004_still_fires_on_an_unguarded_write_in_a_public_read_design() {
        let mut design = public_read_design();
        design.modules[0].endpoints.push(
            serde_json::from_value(serde_json::json!({
                "operation_id": "create_workout", "method": "POST", "path": "/",
                "request_body": { "entity": "Workout" },
                "success": { "status": 201, "entity": "Workout" }
            }))
            .unwrap(),
        );
        let hits = jl0004_only(&design);
        assert_eq!(
            hits.len(),
            1,
            "public_read never exempts an unguarded write: {hits:?}"
        );
    }

    // ---- JL0006 AST rewrite + nested paths + JL0008 (issue #103) ---------

    /// Org (tenant) → Account (belongs_to Org) as the top-level `accounts` module,
    /// with Contact (belongs_to Account) as a SUBROUTE of accounts. Contact is thus
    /// transitively tenant-owned (#102) and its handler nests on disk at
    /// `crates/routes/accounts/src/subroutes/contacts/handlers.rs` — the path the old
    /// flat-path scan never built (it looked at `crates/routes/contacts/src/…`, a
    /// nonexistent file) and so silently skipped (the #103 hole).
    fn nested_grandchild_design() -> Design {
        serde_json::from_value(serde_json::json!({
            "name": "org-api",
            "contract_version": 1,
            "auth": { "model": "session", "roles": ["owner", "member"] },
            "dependencies": ["db", "auth"],
            "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
            "modules": [
                { "name": "orgs",
                  "entities": [{ "name": "Org", "fields": [{ "name": "id", "type": "integer" }] }],
                  "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                      "success": { "status": 200, "entity": "Org", "list": true } }] },
                { "name": "accounts",
                  "entities": [{ "name": "Account",
                      "belongs_to": [{ "entity": "Org" }],
                      "fields": [{ "name": "id", "type": "integer" }] }],
                  "endpoints": [{ "operation_id": "list_accounts", "method": "GET", "path": "/",
                      "success": { "status": 200, "entity": "Account", "list": true } }],
                  "subroutes": [
                    { "name": "contacts",
                      "entities": [{ "name": "Contact",
                          "belongs_to": [{ "entity": "Account" }],
                          "fields": [{ "name": "id", "type": "integer" }] }],
                      "endpoints": [{ "operation_id": "show_contact", "method": "GET", "path": "/{id}",
                          "success": { "status": 200, "entity": "Contact" } }] }
                  ] }
            ]
        }))
        .unwrap()
    }

    /// JL0006 reaches a bare unscoped `repo.get(id)` in a NESTED (grandchild)
    /// handler at its REAL on-disk path. WHY (Rule 9, #103): the old scan built a
    /// FLAT `crates/routes/{module}/src/handlers.rs` for every module, so a nested
    /// or transitively-owned handler resolved to a missing file and was skipped in
    /// silence — the exact gap that let the #102 transitive leak ship undetected.
    #[test]
    fn jl0006_fires_on_unscoped_call_in_nested_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let rel = "crates/routes/accounts/src/subroutes/contacts/handlers.rs";
        let handlers = root.join(rel);
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn show_contact(repo: Dep<ContactRepo>) -> Result<()> {\n    let _ = repo.get(id).await?;\n    Ok(())\n}\n",
        )
        .unwrap();
        // The parent `accounts` handler is present and scoped, so its own scan is
        // clean (and no JL0008 for a missing file muddies the result).
        std::fs::write(
            root.join("crates/routes/accounts/src/handlers.rs"),
            "async fn list_accounts(repo: Dep<AccountRepo>) -> Result<()> {\n    let _ = repo.all_for(_tenant.id()).await?;\n    Ok(())\n}\n",
        )
        .unwrap();

        let diags = run(root, &nested_grandchild_design());
        assert!(
            diags
                .iter()
                .any(|d| d.code == "JL0006" && d.file.as_deref() == Some(rel) && d.line == Some(2)),
            "JL0006 must reach the NESTED grandchild handler (was silently skipped, #103): {diags:?}"
        );
    }

    /// JL0006 reaches an unscoped `repo.all()` wrapped in a `json!` MACRO inside a
    /// NESTED tenant-owned (grandchild) handler. WHY (Rule 9, security regression):
    /// the AST `Visit` walk does NOT descend into macro token streams, so a
    /// tenant-owned handler returning `Json(json!({ "items": repo.all().await? }))`
    /// leaks every tenant's rows while JL0006 stays silent — the coverage the
    /// pre-branch substring scanner had for single-line macro-wrapped calls. RED
    /// before the macro-token scan, GREEN after.
    #[test]
    fn jl0006_fires_on_unscoped_call_inside_a_macro_in_nested_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let rel = "crates/routes/accounts/src/subroutes/contacts/handlers.rs";
        let handlers = root.join(rel);
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        // The unscoped call lives inside the json! token stream — invisible to the
        // AST method-call walk, visible only to the macro-token scan.
        std::fs::write(
            &handlers,
            "async fn show_contact(repo: Dep<ContactRepo>) -> Result<()> {\n    Ok(Json(serde_json::json!({ \"items\": repo.all().await? })))\n}\n",
        )
        .unwrap();
        // Parent `accounts` handler present + scoped, so its own scan is clean.
        std::fs::write(
            root.join("crates/routes/accounts/src/handlers.rs"),
            "async fn list_accounts(repo: Dep<AccountRepo>) -> Result<()> {\n    let _ = repo.all_for(_tenant.id()).await?;\n    Ok(())\n}\n",
        )
        .unwrap();

        let diags = run(root, &nested_grandchild_design());
        assert!(
            diags
                .iter()
                .any(|d| d.code == "JL0006" && d.file.as_deref() == Some(rel) && d.line == Some(2)),
            "JL0006 must reach the unscoped repo.all() inside the json! macro — syn::visit does not descend into macro tokens: {diags:?}"
        );
    }

    /// A SCOPED `repo.all_for_memberships(...)` inside the same macro does NOT fire:
    /// the trailing-paren needles exclude the `*_for_memberships` accessor exactly as
    /// the AST idents do. WHY (Rule 9): a false positive on the scoped call inside a
    /// macro would make the correct fix un-passable.
    #[test]
    fn jl0006_silent_on_scoped_call_inside_a_macro() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let rel = "crates/routes/accounts/src/subroutes/contacts/handlers.rs";
        let handlers = root.join(rel);
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            "async fn show_contact(repo: Dep<ContactRepo>, u: CurrentUser) -> Result<()> {\n    Ok(Json(serde_json::json!({ \"items\": repo.all_for_memberships(u).await? })))\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/routes/accounts/src/handlers.rs"),
            "async fn list_accounts(repo: Dep<AccountRepo>) -> Result<()> {\n    let _ = repo.all_for(_tenant.id()).await?;\n    Ok(())\n}\n",
        )
        .unwrap();

        let diags = run(root, &nested_grandchild_design());
        assert!(
            !diags.iter().any(|d| d.code == "JL0006"),
            "a scoped all_for_memberships inside a macro must not fire JL0006: {diags:?}"
        );
    }

    /// Run the full lint pass over a V1_FULL `leads` (FLAT tenant-owned) handler
    /// whose body is `body` wrapped in a handler fn. Reuses the tenant fixture so
    /// the tenant-owned scan (and its fail-loud JL0008) is exercised.
    fn lints_for_leads_body(body: &str) -> Vec<Diagnostic> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let handlers = root.join("crates/routes/leads/src/handlers.rs");
        std::fs::create_dir_all(handlers.parent().unwrap()).unwrap();
        std::fs::write(
            &handlers,
            format!(
                "async fn h(repo: Dep<LeadRepo>) -> Result<()> {{\n    {body}\n    Ok(())\n}}\n"
            ),
        )
        .unwrap();
        run(root, &tenant_design())
    }

    /// A mention of `repo.all()` in a COMMENT is not a call — the AST walk never
    /// flags it, and the real call on the next line (`all_for_memberships`, a scoped
    /// accessor) is clean. WHY (Rule 9): the old substring scan would have flagged
    /// the comment; AST detection is what closes that false-positive class.
    #[test]
    fn jl0006_ast_ignores_repo_all_in_a_comment() {
        let diags = lints_for_leads_body(
            "// repo.all() is the unscoped call we must avoid\n    let _x = repo.all_for_memberships(u).await?;",
        );
        assert!(
            !diags.iter().any(|d| d.code == "JL0006"),
            "a mention in a comment is not a call: {diags:?}"
        );
    }

    /// A call split across lines (`repo\n .all()`) is caught — the substring scan
    /// missed it because `repo.all()` never appeared contiguously on one line. WHY
    /// (Rule 9, #103): multi-line chains are idiomatic Rust, so a scan that only
    /// matched one-line spellings left a real evasion path open.
    #[test]
    fn jl0006_ast_catches_multiline_chain() {
        let diags = lints_for_leads_body("let _x = repo\n        .all()\n        .await?;");
        assert!(
            diags.iter().any(|d| d.code == "JL0006"),
            "multi-line chain must be caught (substring scan missed it): {diags:?}"
        );
    }

    /// A tenant-owned handler that does NOT parse becomes a LOUD JL0008 — never a
    /// silent skip. WHY (Rule 9/12, #103): a handler whose scoping cannot be checked
    /// is exactly where an unscoped cross-tenant call would hide; failing loud makes
    /// `jerrycan check` surface it instead of passing over it.
    #[test]
    fn jl0008_when_tenant_owned_handler_unparseable() {
        let diags = lints_for_leads_body("fn broken( {{{ this does not parse");
        assert!(
            diags.iter().any(|d| d.code == "JL0008"
                && d.file.as_deref() == Some("crates/routes/leads/src/handlers.rs")),
            "unparseable tenant-owned handler → loud JL0008, never a silent skip: {diags:?}"
        );
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

    // ---- JL0007: handler code escaping the request boundary --------------

    /// A bare design with a `leads` module that has one `audit` subroute, so the
    /// JL0007 scan walks both `crates/routes/leads/src/{...}.rs` and
    /// `crates/routes/leads/src/subroutes/audit/{...}.rs`.
    fn boundary_design() -> Design {
        serde_json::from_value(serde_json::json!({
            "name": "leads-api",
            "contract_version": 1,
            "modules": [{
                "name": "leads",
                "endpoints": [{
                    "operation_id": "list_leads", "method": "GET", "path": "/",
                    "success": { "status": 200 }
                }],
                "subroutes": [{
                    "name": "audit",
                    "endpoints": [{
                        "operation_id": "list_audit", "method": "GET", "path": "/",
                        "success": { "status": 200 }
                    }]
                }]
            }]
        }))
        .unwrap()
    }

    /// Only JL0007 diagnostics from a full pass (the other lints fire on the
    /// absent lib.rs/main.rs in these bare fixtures — irrelevant here).
    fn jl0007_only(root: &Path, design: &Design) -> Vec<Diagnostic> {
        run(root, design)
            .into_iter()
            .filter(|d| d.code == "JL0007")
            .collect()
    }

    /// Write a file under root, creating parent dirs.
    fn write_at(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    /// JL0007 flags `std::process::Command` in a module's handlers.rs, with the
    /// exact file:line. WHY (Rule 9): handler code is agent-authored untrusted
    /// input; reaching process/fs/net escapes the framework's request boundary
    /// and the threat model — the lint is the mechanical guard for that class.
    #[test]
    fn jl0007_flags_process_in_handlers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_at(
            root,
            "crates/routes/leads/src/handlers.rs",
            "async fn run_it() {\n    let _ = std::process::Command::new(\"curl\");\n}\n",
        );
        let hits = jl0007_only(root, &boundary_design());
        assert_eq!(hits.len(), 1, "exactly one boundary escape: {hits:?}");
        assert_eq!(hits[0].code, "JL0007");
        assert_eq!(hits[0].line, Some(2), "points at the std::process:: line");
        assert!(
            hits[0]
                .file
                .as_deref()
                .unwrap()
                .contains("leads/src/handlers.rs"),
            "{:?}",
            hits[0]
        );
    }

    /// The scan covers the whole agent-owned set (repo.rs, deps.rs) and the
    /// tokio:: needles too, not just handlers.rs/std::. A subroute's files are
    /// scanned at their nested path.
    #[test]
    fn jl0007_flags_fs_net_across_the_agent_owned_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_at(
            root,
            "crates/routes/leads/src/repo.rs",
            "fn load() {\n    let _ = std::fs::read_to_string(\"/etc/passwd\");\n}\n",
        );
        write_at(
            root,
            "crates/routes/leads/src/deps.rs",
            "fn dial() {\n    let _ = std::net::TcpStream::connect(\"10.0.0.1:80\");\n}\n",
        );
        write_at(
            root,
            "crates/routes/leads/src/subroutes/audit/handlers.rs",
            "async fn beam() {\n    let _ = tokio::fs::read(\"x\").await;\n}\n",
        );
        let hits = jl0007_only(root, &boundary_design());
        assert_eq!(hits.len(), 3, "fs + net + tokio::fs: {hits:?}");
        let files: BTreeSet<&str> = hits.iter().map(|h| h.file.as_deref().unwrap()).collect();
        assert!(files.iter().any(|f| f.contains("repo.rs")), "{files:?}");
        assert!(files.iter().any(|f| f.contains("deps.rs")), "{files:?}");
        assert!(
            files
                .iter()
                .any(|f| f.contains("subroutes/audit/handlers.rs")),
            "subroute files are scanned: {files:?}"
        );
    }

    /// The escape hatch: a line ending with `// jerrycan:allow JL0007` is NOT
    /// flagged, but the hatch is line-scoped — the very next offending line still
    /// flags. WHY (Rule 9): a blanket file/module suppression would let one
    /// `allow` silence the whole file; line scope keeps every other escape sharp.
    #[test]
    fn jl0007_allow_hatch_is_line_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_at(
            root,
            "crates/routes/leads/src/handlers.rs",
            "async fn x() {\n    let _ = std::process::Command::new(\"ok\"); // jerrycan:allow JL0007\n    let _ = std::process::Command::new(\"bad\");\n}\n",
        );
        let hits = jl0007_only(root, &boundary_design());
        assert_eq!(hits.len(), 1, "only the un-allowed line flags: {hits:?}");
        assert_eq!(hits[0].line, Some(3), "the next line still flags");
    }

    /// Legitimate code is never flagged: jerrycan::/sea_orm:: calls, `use std::fmt`,
    /// `std::collections::HashMap`, and a comment that merely mentions std::process
    /// in prose. The needle is `std::process::` (etc.), and a line whose trimmed
    /// start is `//` is skipped entirely.
    #[test]
    fn jl0007_silent_on_legitimate_code() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_at(
            root,
            "crates/routes/leads/src/handlers.rs",
            "use std::fmt;\nuse std::collections::HashMap;\n// we never call std::process::Command here\nasync fn x() {\n    let _ = jerrycan::prelude::Json::default();\n    let _: HashMap<u8, u8> = HashMap::new();\n    let _ = sea_orm::EntityTrait::find();\n}\n",
        );
        assert!(
            jl0007_only(root, &boundary_design()).is_empty(),
            "no boundary escape in legitimate code"
        );
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
