//! Per-module code generation: route crates, fractal subroutes, handler stubs.
//! Ownership rule: Cargo.toml/lib.rs/subroutes mod.rs files are TOOL-owned
//! (always rewritten); handlers/model/repo/deps are AGENT-owned (create-once).

use super::design::*;
use super::templates::{ROUTE_CARGO, jerrycan_dep_spec, render};
use std::fs;
use std::path::Path;

/// snake/ident helpers: crate `route-todo-list` → ident `route_todo_list`.
pub fn crate_ident(module_name: &str) -> String {
    format!("route_{}", module_name.replace('-', "_"))
}

fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .map(|rb| rb.entity.as_str())
        .or(ep.success.entity.as_deref())
        .or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

fn return_type(ep: &Endpoint) -> String {
    let entity = ep.success.entity.as_deref();
    match (ep.success.status, entity, ep.success.list) {
        (204, _, _) => "Result<NoContent>".to_string(),
        (201, Some(e), _) => format!("Result<Created<{e}>>"),
        (201, None, _) => "Result<Created<serde_json::Value>>".to_string(),
        (_, Some(e), true) => format!("Result<Json<Vec<{e}>>>"),
        (_, Some(e), false) => format!("Result<Json<{e}>>"),
        (_, None, _) => "Result<Json<serde_json::Value>>".to_string(),
    }
}

fn path_param(ep: &Endpoint) -> Option<String> {
    let start = ep.path.find('{')?;
    let end = ep.path[start..].find('}')? + start;
    Some(ep.path[start + 1..end].to_string())
}

fn handler_params(m: &ModuleDesign, ep: &Endpoint) -> String {
    let mut params = Vec::new();
    if let Some(e) = endpoint_repo_entity(m, ep) {
        params.push(format!("_repo: Dep<{e}Repo>"));
    }
    if let Some(p) = path_param(ep) {
        params.push(format!("Path(_{p}): Path<i64>"));
    }
    if let Some(ref rb) = ep.request_body {
        params.push(format!("Json(_body): Json<{}>", rb.entity));
    }
    params.join(", ")
}

pub(crate) fn handlers_rs(m: &ModuleDesign) -> String {
    let mut uses = String::from("use jerrycan::prelude::*;\n");
    let mentions_entities = m
        .endpoints
        .iter()
        .any(|ep| ep.request_body.is_some() || ep.success.entity.is_some());
    if mentions_entities {
        uses.push_str("use super::model::*;\n");
    }
    if !m.entities.is_empty() {
        uses.push_str("use super::repo::*;\n");
    }
    let mut out = format!(
        "//! Handlers for `{}` — thin: extract → call → respond.\n//! Generated stubs return 500 until implemented.\n{uses}\n",
        m.name
    );
    for ep in &m.endpoints {
        out.push_str(&format!(
            "pub(crate) async fn {op}({params}) -> {ret} {{\n    Err(Error::internal(\"{op} not implemented — replace this stub\"))\n}}\n\n",
            op = ep.operation_id,
            params = handler_params(m, ep),
            ret = return_type(ep),
        ));
    }
    out
}

pub(crate) fn model_rs(m: &ModuleDesign) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::from(
        "//! Entities and DTOs for this module.\nuse serde::{Deserialize, Serialize};\n\n",
    );
    for e in &m.entities {
        out.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct ");
        out.push_str(&e.name);
        out.push_str(" {\n");
        for f in &e.fields {
            if !f.required {
                out.push_str("    #[serde(default)]\n");
            }
            out.push_str(&format!(
                "    pub {}: {},\n",
                f.name,
                f.field_type.rust_type()
            ));
        }
        out.push_str("}\n\n");
    }
    Some(out)
}

pub(crate) fn repo_rs(m: &ModuleDesign) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::from(
        "//! In-memory data access (Phase 1; jerrycan-db replaces this in Phase 2).\nuse super::model::*;\nuse std::collections::BTreeMap;\nuse std::sync::Mutex;\nuse std::sync::atomic::{AtomicI64, Ordering};\n\n",
    );
    for e in &m.entities {
        let n = &e.name;
        out.push_str(&format!(
            r#"// Stub handlers don't call the repo yet; remove this allow as you implement them.
#[allow(dead_code)]
pub struct {n}Repo {{
    items: Mutex<BTreeMap<i64, {n}>>,
    next_id: AtomicI64,
}}

#[allow(dead_code)]
impl {n}Repo {{
    pub fn new() -> Self {{
        Self {{ items: Mutex::new(BTreeMap::new()), next_id: AtomicI64::new(1) }}
    }}
    pub fn all(&self) -> Vec<{n}> {{
        self.items.lock().unwrap().values().cloned().collect()
    }}
    pub fn get(&self, id: i64) -> Option<{n}> {{
        self.items.lock().unwrap().get(&id).cloned()
    }}
    pub fn insert(&self, item: {n}) -> i64 {{
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.items.lock().unwrap().insert(id, item);
        id
    }}
    pub fn remove(&self, id: i64) -> bool {{
        self.items.lock().unwrap().remove(&id).is_some()
    }}
}}

impl Default for {n}Repo {{
    fn default() -> Self {{
        Self::new()
    }}
}}

"#
        ));
    }
    Some(out)
}

pub(crate) fn deps_rs(m: &ModuleDesign) -> String {
    let mut out = String::from(
        "//! Agent-owned: module-scoped dependencies and middleware.\nuse jerrycan::prelude::*;\n\n/// Called by the tool-owned lib.rs — register module deps/middleware here;\n/// regeneration never touches this file.\npub(crate) fn configure(module: Module) -> Module {\n",
    );
    for dep in &m.dependencies {
        out.push_str(&format!(
            "    // declared dependency `{dep}`: define a type and .provide/.provide_dep it here\n"
        ));
    }
    out.push_str("    module\n}\n");
    out
}

/// Route-table lines: endpoints grouped by path (first-seen order), first
/// method via the free fn, the rest chained.
fn route_lines(m: &ModuleDesign, indent: &str) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut by_path: std::collections::HashMap<&str, Vec<&Endpoint>> =
        std::collections::HashMap::new();
    for ep in &m.endpoints {
        if !by_path.contains_key(ep.path.as_str()) {
            order.push(&ep.path);
        }
        by_path.entry(&ep.path).or_default().push(ep);
    }
    let mut out = String::new();
    for path in order {
        let eps = &by_path[path];
        let mut chain = format!(
            "{}(handlers::{})",
            eps[0].method.builder_fn(),
            eps[0].operation_id
        );
        for ep in &eps[1..] {
            chain.push_str(&format!(
                ".{}(handlers::{})",
                ep.method.builder_fn(),
                ep.operation_id
            ));
        }
        out.push_str(&format!("{indent}.route(\"{path}\", {chain})\n"));
    }
    out
}

fn module_body(m: &ModuleDesign, indent: &str) -> String {
    let mut body = format!("{indent}Module::new(\"{}\")\n", m.name);
    for e in &m.entities {
        body.push_str(&format!(
            "{indent}    .provide(repo::{}Repo::new())\n",
            e.name
        ));
    }
    body.push_str(&route_lines(m, &format!("{indent}    ")));
    for sub in &m.subroutes {
        body.push_str(&format!(
            "{indent}    .mount(\"{}\", subroutes::{}::module())\n",
            sub.effective_mount(),
            sub.name.replace('-', "_"),
        ));
    }
    body
}

fn mod_decls(m: &ModuleDesign) -> String {
    let mut out = String::from("mod deps;\nmod handlers;\n");
    if !m.entities.is_empty() {
        out.push_str("mod model;\nmod repo;\n");
    }
    if !m.subroutes.is_empty() {
        out.push_str("mod subroutes;\n");
    }
    out
}

pub(crate) fn lib_rs(m: &ModuleDesign) -> String {
    format!(
        "//! Route module `{name}` — TOOL-OWNED, regenerated by `jerrycan generate`.\n//! The sole public item is `module()`; agent code lives in handlers/model/repo/deps.\n#![forbid(unsafe_code)]\n\n{mods}\nuse jerrycan::prelude::*;\n\n/// Build this module's routes, subroutes, and scoped dependencies.\npub fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        "),
    )
}

fn subroute_mod_rs(m: &ModuleDesign) -> String {
    format!(
        "//! Subroute `{name}` — TOOL-OWNED mod.rs; same fractal shape as a module.\n\n{mods}\nuse jerrycan::prelude::*;\n\npub(crate) fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        "),
    )
}

fn write_tool_owned(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    fs::create_dir_all(path.parent().expect("file path has parent")).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    created.push(rel(path, root));
    Ok(())
}

fn write_agent_owned(
    path: &Path,
    content: &str,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    if path.exists() {
        return Ok(()); // never clobber agent work
    }
    write_tool_owned(path, content, created, root)
}

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Write (or refresh) one top-level route crate under `routes_dir`
/// (= <app>/crates/routes). Returns paths written, relative to routes_dir's parent's parent.
/// Precondition: the design has passed `questions::validate` — generation assumes
/// validated names and entity references.
pub fn write_module(routes_dir: &Path, m: &ModuleDesign) -> Result<Vec<String>, String> {
    let root = routes_dir
        .ancestors()
        .nth(2)
        .unwrap_or(routes_dir)
        .to_path_buf();
    let crate_dir = routes_dir.join(&m.name);
    let src = crate_dir.join("src");
    let mut created = Vec::new();

    let cargo = render(ROUTE_CARGO, &[("name", &m.name)])?;
    write_tool_owned(&crate_dir.join("Cargo.toml"), &cargo, &mut created, &root)?;
    write_tool_owned(&src.join("lib.rs"), &lib_rs(m), &mut created, &root)?;
    write_unit_files(&src, m, &mut created, &root)?;
    write_subroutes(&src, m, &mut created, &root)?;
    // jerrycan_dep_spec is consumed by the workspace manifest (scaffold), not here;
    // referenced so the module split stays honest:
    let _ = jerrycan_dep_spec;
    Ok(created)
}

/// The agent-owned file set shared by modules and subroutes.
fn write_unit_files(
    dir: &Path,
    m: &ModuleDesign,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    write_agent_owned(&dir.join("handlers.rs"), &handlers_rs(m), created, root)?;
    write_agent_owned(&dir.join("deps.rs"), &deps_rs(m), created, root)?;
    if let Some(model) = model_rs(m) {
        write_agent_owned(&dir.join("model.rs"), &model, created, root)?;
    }
    if let Some(repo) = repo_rs(m) {
        write_agent_owned(&dir.join("repo.rs"), &repo, created, root)?;
    }
    Ok(())
}

fn write_subroutes(
    src: &Path,
    m: &ModuleDesign,
    created: &mut Vec<String>,
    root: &Path,
) -> Result<(), String> {
    if m.subroutes.is_empty() {
        return Ok(());
    }
    let sub_root = src.join("subroutes");
    let mut decls = String::from("//! TOOL-OWNED: subroute declarations.\n");
    for sub in &m.subroutes {
        decls.push_str(&format!("pub(crate) mod {};\n", sub.name.replace('-', "_")));
    }
    write_tool_owned(&sub_root.join("mod.rs"), &decls, created, root)?;
    for sub in &m.subroutes {
        let dir = sub_root.join(sub.name.replace('-', "_"));
        write_tool_owned(&dir.join("mod.rs"), &subroute_mod_rs(sub), created, root)?;
        write_unit_files(&dir, sub, created, root)?;
        write_subroutes(&dir, sub, created, root)?; // arbitrary depth
    }
    Ok(())
}

/// `jerrycan generate dep <name> --module <m>`: record in design + remind in deps.rs.
pub fn add_dependency(design: &mut Design, module_path: &str, dep: &str) -> Result<(), String> {
    let m = module_by_path_mut(design, module_path)
        .ok_or_else(|| format!("module `{module_path}` not found in design.json"))?;
    if !m.dependencies.iter().any(|d| d == dep) {
        m.dependencies.push(dep.to_string());
    }
    Ok(())
}

/// One row of the live route tree — the same shape the CLI and MCP both emit.
#[derive(Debug, serde::Serialize)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub module: String,
    pub handler: String,
}

/// Walk the design's module tree into a flat, mount-resolved route table.
pub fn route_map(design: &Design) -> Vec<RouteEntry> {
    fn walk(m: &ModuleDesign, prefix: &str, top: &str, out: &mut Vec<RouteEntry>) {
        let base = format!("{}{}", prefix, m.effective_mount());
        for ep in &m.endpoints {
            out.push(RouteEntry {
                method: format!("{:?}", ep.method),
                path: format!("{}{}", base.trim_end_matches('/'), ep.path),
                module: top.to_string(),
                handler: ep.operation_id.clone(),
            });
        }
        for sub in &m.subroutes {
            walk(sub, &base, top, out);
        }
    }
    let mut out = Vec::new();
    for m in &design.modules {
        walk(m, "", &m.name, &mut out);
    }
    out
}

pub fn module_by_path<'a>(design: &'a Design, path: &str) -> Option<&'a ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter().find(|s| s.name == part)?;
    }
    Some(cur)
}

pub fn module_by_path_mut<'a>(design: &'a mut Design, path: &str) -> Option<&'a mut ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter_mut().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter_mut().find(|s| s.name == part)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::MINIMAL;

    fn todos() -> ModuleDesign {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        d.modules.into_iter().next().unwrap()
    }

    #[test]
    fn handler_signatures_follow_the_mapping_rules() {
        let m = todos();
        let h = handlers_rs(&m);
        assert!(
            h.contains(
                "pub(crate) async fn list_todos(_repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>>"
            ),
            "{h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn create_todo(_repo: Dep<TodoRepo>, Json(_body): Json<Todo>) -> Result<Created<Todo>>"
            ),
            "{h}"
        );
        assert!(
            h.contains(
                "pub(crate) async fn delete_todo(_repo: Dep<TodoRepo>, Path(_id): Path<i64>) -> Result<NoContent>"
            ),
            "{h}"
        );
        assert!(h.contains("not implemented — replace this stub"));
    }

    #[test]
    fn lib_rs_groups_routes_by_path_and_mounts_subroutes() {
        let m = todos();
        let lib = lib_rs(&m);
        assert!(lib.contains("pub fn module() -> Module"), "{lib}");
        assert!(
            lib.contains(".route(\"/\", get(handlers::list_todos).post(handlers::create_todo))"),
            "{lib}"
        );
        assert!(
            lib.contains(".route(\"/{id}\", delete(handlers::delete_todo))"),
            "{lib}"
        );
        assert!(
            lib.contains(".mount(\"/comments\", subroutes::comments::module())"),
            "{lib}"
        );
        assert!(lib.contains(".provide(repo::TodoRepo::new())"), "{lib}");
        assert!(
            lib.contains("deps::configure("),
            "agent hook must wrap the module: {lib}"
        );
        assert!(lib.contains("#![forbid(unsafe_code)]"));
    }

    #[test]
    fn model_and_repo_are_generated_from_entities() {
        let m = todos();
        let model = model_rs(&m).unwrap();
        assert!(model.contains("pub struct Todo"));
        assert!(model.contains("pub title: String"));
        assert!(
            model.contains("#[serde(default)]\n    pub done: bool"),
            "{model}"
        );
        let repo = repo_rs(&m).unwrap();
        assert!(repo.contains("pub struct TodoRepo"));
        assert!(
            repo.contains("#[allow(dead_code)]"),
            "stub-phase repo must pass -D warnings: {repo}"
        );
        for method in [
            "pub fn all(",
            "pub fn get(",
            "pub fn insert(",
            "pub fn remove(",
        ] {
            assert!(repo.contains(method), "{repo}");
        }
    }

    #[test]
    fn write_module_respects_the_ownership_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let routes = tmp.path().join("crates/routes");
        let m = todos();

        let created = write_module(&routes, &m).unwrap();
        assert!(created.iter().any(|p| p.ends_with("todos/src/lib.rs")));
        assert!(
            created
                .iter()
                .any(|p| p.ends_with("todos/src/subroutes/comments/mod.rs"))
        );

        // Agent edits handlers.rs; tool hand-edits lib.rs (illegally).
        let handlers = routes.join("todos/src/handlers.rs");
        fs::write(&handlers, "// AGENT CODE\n").unwrap();
        let lib = routes.join("todos/src/lib.rs");
        fs::write(&lib, "// hand edit\n").unwrap();

        write_module(&routes, &m).unwrap();
        assert_eq!(
            fs::read_to_string(&handlers).unwrap(),
            "// AGENT CODE\n",
            "agent-owned: preserved"
        );
        assert!(
            fs::read_to_string(&lib)
                .unwrap()
                .contains("pub fn module()"),
            "tool-owned: restored"
        );
    }

    #[test]
    fn subroutes_without_entities_have_no_model_or_repo() {
        let m = todos();
        let sub = &m.subroutes[0];
        assert!(model_rs(sub).is_none());
        assert!(repo_rs(sub).is_none());
        let h = handlers_rs(sub);
        assert!(
            h.contains("pub(crate) async fn list_comments() -> Result<Json<serde_json::Value>>"),
            "{h}"
        );
    }
}
