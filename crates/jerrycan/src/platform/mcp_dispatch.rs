//! tools/call dispatch — the MCP twins of the CLI commands.

use super::design::Design;
use super::{checkpipe, genroute, mounting, questions, scaffold};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn root_from(args: &Value) -> PathBuf {
    args["directory"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn err_payload(msg: impl Into<String>) -> (bool, Value) {
    (true, json!({ "error": msg.into() }))
}

pub fn dispatch(name: &str, args: &Value) -> (bool, Value) {
    match name {
        "jerrycan_docs_search" => {
            let query = args["query"].as_str().unwrap_or("");
            let limit = args["limit"].as_u64().unwrap_or(5) as usize;
            (
                false,
                json!({ "results": super::docsidx::search(query, limit) }),
            )
        }
        "jerrycan_docs_get" => {
            let page = args["page"].as_str().unwrap_or("");
            match super::docsidx::get(page, args["anchor"].as_str()) {
                Some(md) => (false, json!({ "markdown": md })),
                None => (
                    true,
                    json!({ "error": format!("unknown docs page `{page}`") }),
                ),
            }
        }

        "jerrycan_design" => {
            let Some(draft) = args.get("draft").filter(|d| !d.is_null()) else {
                let template = include_str!("../../../../conformance/designs/todo-api.design.json");
                return (
                    false,
                    json!({
                        "status": "questions",
                        "questions": [{
                            "id": "/",
                            "question": format!(
                                "Provide a structured `draft` conforming to design-schema.json. Be specific: modules, entities+fields, endpoints with operation_id/method/path/success/errors. Worked example:\n{template}"
                            ),
                        }],
                        "next_step": "author the draft from the requirements, then call jerrycan_design again with it",
                    }),
                );
            };
            let design: Design = match serde_json::from_value(draft.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return (
                        false,
                        json!({
                            "status": "questions",
                            "questions": [{ "id": "/", "question": format!("draft does not parse against design-schema.json: {e}") }],
                            "next_step": "fix the draft and call jerrycan_design again",
                        }),
                    );
                }
            };
            let qs = questions::validate(&design);
            if !qs.is_empty() {
                return (
                    false,
                    json!({
                        "status": "questions",
                        "questions": qs,
                        "next_step": "answer each question by fixing the draft, then call jerrycan_design again",
                    }),
                );
            }
            let path = args["revision_of"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("design.json"));
            if let Err(e) = std::fs::write(&path, scaffold::canonical_design_json(&design)) {
                return err_payload(format!("cannot write {}: {e}", path.display()));
            }
            let abs = path.canonicalize().unwrap_or(path);
            (
                false,
                json!({
                    "status": "complete",
                    "design": serde_json::to_value(&design).expect("design serializes"),
                    "design_path": abs.display().to_string(),
                    "next_step": "call jerrycan_scaffold with this design_path and a target directory",
                }),
            )
        }

        "jerrycan_scaffold" => {
            let (Some(design_path), Some(directory)) =
                (args["design_path"].as_str(), args["directory"].as_str())
            else {
                return err_payload("design_path and directory are required");
            };
            let design = match Design::from_path(Path::new(design_path)) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            let qs = questions::validate(&design);
            if !qs.is_empty() {
                return (
                    true,
                    json!({ "error": "design is incomplete", "questions": qs }),
                );
            }
            match scaffold::scaffold(Path::new(directory), &design) {
                Ok(created) => (
                    false,
                    json!({
                        "created": created,
                        "next_step": "implement the handler stubs (see jerrycan_list_routes), then jerrycan_check",
                    }),
                ),
                Err(e) => err_payload(e),
            }
        }

        "jerrycan_generate" => {
            let root = root_from(args);
            let design_path = root.join("design.json");
            let mut design = match Design::from_path(&design_path) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            let kind = args["kind"].as_str().unwrap_or("");
            let path = args["path"].as_str().unwrap_or("");
            match kind {
                "route" | "subroute" => {
                    let routes_before = genroute::route_map(&design).len();
                    if let Some(slice) = args.get("design_slice").filter(|s| !s.is_null()) {
                        let module: super::design::ModuleDesign =
                            match serde_json::from_value(slice.clone()) {
                                Ok(m) => m,
                                Err(e) => {
                                    return err_payload(format!(
                                        "design_slice does not parse: {e}"
                                    ));
                                }
                            };
                        if kind == "route" && module.name != path {
                            return err_payload(format!(
                                "design_slice.name `{}` does not match path `{path}` — set path to the module the slice replaces (slices replace the WHOLE module)",
                                module.name
                            ));
                        }
                        if kind == "route" {
                            design.modules.retain(|m| m.name != module.name);
                            design.modules.push(module);
                        } else {
                            let Some((parent_path, _)) = path.rsplit_once('/') else {
                                return err_payload("subroute path must be parent/child");
                            };
                            let Some(parent) =
                                genroute::module_by_path_mut(&mut design, parent_path)
                            else {
                                return err_payload(format!(
                                    "parent module `{parent_path}` not found"
                                ));
                            };
                            parent.subroutes.retain(|s| s.name != module.name);
                            parent.subroutes.push(module);
                        }
                    }
                    let qs = questions::validate(&design);
                    if !qs.is_empty() {
                        return (
                            true,
                            json!({ "error": "design would become incomplete", "questions": qs }),
                        );
                    }
                    if genroute::module_by_path(&design, path).is_none() {
                        return err_payload(format!(
                            "module `{path}` not in design.json — pass a design_slice or edit the design first"
                        ));
                    }
                    if let Err(e) =
                        std::fs::write(&design_path, scaffold::canonical_design_json(&design))
                    {
                        return err_payload(e.to_string());
                    }
                    let top_name = path.split('/').next().expect("nonempty");
                    let top = design
                        .modules
                        .iter()
                        .find(|m| m.name == top_name)
                        .expect("validated above");
                    let mode = genroute::GenMode {
                        db: design.wants_db(),
                    };
                    let created =
                        match genroute::write_module(&root.join("crates/routes"), top, mode) {
                            Ok(c) => c,
                            Err(e) => return err_payload(e),
                        };
                    let modified = match mounting::regenerate(&root, &design) {
                        Ok(m) => m,
                        Err(e) => return err_payload(e),
                    };
                    let routes_after = genroute::route_map(&design).len();
                    let mut next_step = format!(
                        "implement crates/routes/{top_name}/src/handlers.rs, then jerrycan_check"
                    );
                    if routes_after < routes_before {
                        next_step.push_str(&format!(
                            " — warning: route count dropped {routes_before} → {routes_after}; a partial design_slice REPLACES the whole module (stale agent files are not deleted)"
                        ));
                    }
                    (
                        false,
                        json!({
                            "created": created,
                            "modified": modified,
                            "next_step": next_step,
                        }),
                    )
                }
                "dependency" => {
                    let Some(module) = args["module"].as_str() else {
                        return err_payload("`module` is required for kind=dependency");
                    };
                    if let Err(e) = genroute::add_dependency(&mut design, module, path) {
                        return err_payload(e);
                    }
                    if let Err(e) =
                        std::fs::write(&design_path, scaffold::canonical_design_json(&design))
                    {
                        return err_payload(e.to_string());
                    }
                    (
                        false,
                        json!({
                            "created": [],
                            "modified": ["design.json"],
                            "next_step": format!("define `{path}` in the module's deps.rs configure() hook"),
                        }),
                    )
                }
                other => err_payload(format!("unknown kind `{other}`")),
            }
        }

        "jerrycan_check" => {
            let root = root_from(args);
            let design = match Design::from_path(&root.join("design.json")) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            match checkpipe::run_all(&root, &design, args["module"].as_str()) {
                Ok(report) => (
                    false,
                    serde_json::to_value(&report).expect("report serializes"),
                ),
                Err(env) => err_payload(env),
            }
        }

        "jerrycan_list_routes" => {
            let root = root_from(args);
            match Design::from_path(&root.join("design.json")) {
                Ok(design) => (false, json!({ "routes": genroute::route_map(&design) })),
                Err(e) => err_payload(e),
            }
        }

        "jerrycan_gen_tests" => (
            true,
            json!({
                "error": "jerrycan_gen_tests arrives in Phase 2 (per the roadmap)",
                "next_step": "implement the handler stubs and verify with jerrycan_check",
            }),
        ),
        "jerrycan_package" => (
            true,
            json!({
                "error": "jerrycan_package arrives in Phase 3 (per the roadmap)",
                "next_step": "verify with jerrycan_check; packaging targets land with jerrycan-observe/auth",
            }),
        ),

        other => (true, json!({ "error": format!("unknown tool `{other}`") })),
    }
}
