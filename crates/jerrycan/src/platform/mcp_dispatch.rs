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
                let template = include_str!("../../embedded/designs/todo-api.design.json");
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
            let mut design: Design = match serde_json::from_value(draft.clone()) {
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
            // Match `from_path`: the tenant entity's own detail route is stored
            // (and generated) as `/{tenant_fk}`, so the guard path-verifies it (#78).
            design.normalize_tenant_detail_routes();
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
            // #27: reject a fatal design-shape conflict before scaffolding, the
            // same rule the CLI runs — the MCP twin must not die mid-scaffold on
            // a half-written tree with a raw SQLite error.
            if let Some(conflict) = questions::design_conflict(&design) {
                return (
                    true,
                    json!({ "error": conflict.message, "code": conflict.code, "hint": conflict.hint }),
                );
            }
            match scaffold::scaffold(Path::new(directory), &design) {
                Ok(mut created) => {
                    // db apps ship a derived schema.json contract.
                    match super::schema::write_schema(Path::new(directory), &design) {
                        Ok(Some(rel)) => created.push(rel),
                        Ok(None) => {}
                        Err(e) => return err_payload(e),
                    }
                    (
                        false,
                        json!({
                            "created": created,
                            "next_step": "implement the handler stubs (see jerrycan_list_routes), then jerrycan_check",
                        }),
                    )
                }
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
                    // A merged design_slice can carry the tenant module's own
                    // `/{id}` detail route un-normalized (it's parsed by
                    // `from_value`, not `from_path`); re-normalize so the slice
                    // path can't reopen the #78 leak.
                    design.normalize_tenant_detail_routes();
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
                        auth: design.wants_auth(),
                    };
                    // #69: use the reporting variant so agent-added `mod`/`use`
                    // wiring dropped from the tool-owned lib.rs/mod.rs is surfaced
                    // loudly on the MCP channel too — never silently lost (the CLI
                    // already does this; the MCP twin must match).
                    let (created, dropped) = match genroute::write_module_reporting(
                        &root.join("crates/routes"),
                        top,
                        mode,
                        &design,
                    ) {
                        Ok(cd) => cd,
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
                    // Append each dropped-wiring warning to next_step so an agent
                    // reading only that field can't miss it.
                    for (file, lines) in &dropped {
                        next_step.push_str(&format!(
                            " — warning: regenerating tool-owned {file} dropped {} agent-added line(s) NOT preserved ({}); re-home this wiring in an AGENT-owned file (handlers.rs, or a module's model.rs) — see `jerrycan_docs_get page=database`. lib.rs/mod.rs are tool-owned and rewritten on every generate.",
                            lines.len(),
                            lines.join(", ")
                        ));
                    }
                    let mut payload = json!({
                        "created": created,
                        "modified": modified,
                        "next_step": next_step,
                    });
                    // Structured mirror of the CLI `--json` envelope: an array of
                    // { file, dropped_lines }. Omitted entirely when nothing dropped.
                    if !dropped.is_empty() {
                        payload["warnings"] = Value::Array(
                            dropped
                                .iter()
                                .map(
                                    |(file, lines)| json!({ "file": file, "dropped_lines": lines }),
                                )
                                .collect(),
                        );
                    }
                    (false, payload)
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
            // `full_report` is the MCP alias for the CLI's `--full-report` flag.
            let no_fail_fast = args["no_fail_fast"].as_bool().unwrap_or(false)
                || args["full_report"].as_bool().unwrap_or(false);
            match checkpipe::run_all(&root, &design, args["module"].as_str(), no_fail_fast) {
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

        "jerrycan_gen_tests" => {
            let root = root_from(args);
            let design = match Design::from_path(&root.join("design.json")) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            let Some(module) = args["module"].as_str() else {
                return err_payload("`module` is required");
            };
            match super::testgen::write_acceptance(&root, &design, module) {
                Ok((rel, count)) => {
                    // The declared jobs get their own tool-owned acceptance tests
                    // (direct task-fn calls); like the HTTP ones they fail on the
                    // stubs, so they count toward `expected_failing` too.
                    match super::jobsgen::write_jobs_acceptance(&root, &design) {
                        Ok(jobs) => {
                            let mut tests_created = vec![rel];
                            let mut count = count;
                            // Jobs are top-level (not per-module): their acceptance
                            // tests are written ONCE, so `jobs_count` is added to the
                            // total exactly once.
                            let has_jobs = jobs.is_some();
                            if let Some((jobs_rel, jobs_count)) = jobs {
                                tests_created.push(jobs_rel);
                                count += jobs_count;
                            }
                            // With jobs, the jobs acceptance tests live in package
                            // `jobs`; point the operator at them too.
                            let next_step = if has_jobs {
                                format!(
                                    "run the tests to see them fail, implement crates/routes/{module}/src/handlers.rs and crates/jobs/src/*.rs job tasks, iterate until green (jerrycan test --module {module} && cargo test -p jobs)"
                                )
                            } else {
                                format!(
                                    "run the tests to see them fail, implement crates/routes/{module}/src/handlers.rs, iterate until green (jerrycan test --module {module})"
                                )
                            };
                            (
                                false,
                                json!({
                                    "tests_created": tests_created,
                                    "expected_failing": count,
                                    "next_step": next_step,
                                }),
                            )
                        }
                        Err(e) => err_payload(e),
                    }
                }
                Err(e) => err_payload(e),
            }
        }
        "jerrycan_package" => {
            let root = root_from(args);
            let design = match Design::from_path(&root.join("design.json")) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            // The frozen contract takes ONE singular `target`; map it to the
            // matching bool and run the same orchestration as the CLI.
            let (docker, k8s, systemd, binary) = match args["target"].as_str() {
                Some("docker") => (true, false, false, false),
                Some("k8s") => (false, true, false, false),
                Some("systemd") => (false, false, true, false),
                Some("binary") => (false, false, false, true),
                other => {
                    return err_payload(format!(
                        "`target` must be one of docker|binary|k8s|systemd, got {other:?}"
                    ));
                }
            };
            match super::package::run_package(&root, &design, docker, k8s, systemd, binary) {
                Ok((artifacts, sbom)) => (
                    false,
                    json!({
                        "artifacts": artifacts,
                        "sbom": sbom,
                        "next_step": "deploy with your own tooling (kubectl apply -f deploy/k8s.yaml, docker build, scp the binary + systemd unit)",
                    }),
                ),
                Err(e) => err_payload(e),
            }
        }

        "jerrycan_schema" => {
            let root = root_from(args);
            let design = match Design::from_path(&root.join("design.json")) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            if !design.wants_db() {
                return err_payload(
                    "this app has no `db` dependency — there is no schema contract to derive",
                );
            }
            // Derive on a throwaway runtime (dispatch is sync), as `db migrate` does.
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => return err_payload(e.to_string()),
            };
            match runtime.block_on(super::schema::derive_schema(&root, &design)) {
                // Return the contract JSON directly as the structured payload.
                Ok(contract) => (
                    false,
                    serde_json::to_value(&contract).expect("contract serializes"),
                ),
                Err(e) => err_payload(e),
            }
        }

        other => (true, json!({ "error": format!("unknown tool `{other}`") })),
    }
}
