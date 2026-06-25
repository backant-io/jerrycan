//! Top-level jobs-crate generation: the typed task stubs + the dispatch registry
//! + the wired `Jobs` extension, generated from `design.jobs`.
//!
//! Jobs are TOP-LEVEL in the design (not per-module), so this emits ONE crate at
//! `crates/jobs/` (mirroring the per-module `crates/routes/<m>/` layout). Ownership
//! rule matches genroute.rs: `Cargo.toml` + `src/lib.rs` (the registry/wiring) are
//! TOOL-owned (always rewritten); each `src/{name}.rs` task module is AGENT-owned
//! (create-once, never clobbered).
//!
//! Two job shapes, decided by `schedule`:
//! - `schedule.is_some()` ⇒ CRON job. The leader enqueues it each due tick, so the
//!   task fn takes only an owned `TaskContext` (no payload): `{name}(mut ctx)`. The
//!   registry closure wraps it `|ctx, _payload| Box::pin({name}::{name}(ctx))`.
//! - `schedule.is_none()` ⇒ QUEUE-only job (enqueued programmatically). The task fn
//!   takes `(mut ctx, payload: {Name}Payload)`; a `{Name}Payload` struct is
//!   generated alongside it. The registry closure deserializes the JSON payload
//!   into it before calling the task.
//!
//! Determinism: jobs are emitted in `design.jobs` array order; the distinct queues
//! are sorted before the `.queue(...)` calls. The output is byte-identical across
//! runs.

use super::design::{Design, JobDesign};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Worker-loop concurrency per declared queue. Conservative by default re: the
/// DB pool: each worker loop can hold a `Db` connection for a job's whole
/// runtime, and `Db`'s pool is small (5 connections), shared with the cron leader
/// and request handlers — so a high default could starve handlers under a few
/// long jobs. `2` keeps headroom in the pool; agents raise it (and the pool)
/// after profiling. Documented in the generated registry so the choice is
/// visible where it bites.
const DEFAULT_CONCURRENCY: u32 = 2;

/// The queue a job runs on: its declared `queue`, or `"default"` when absent.
/// A cron job runs on its named queue just like a queue job.
fn job_queue(job: &JobDesign) -> &str {
    job.queue.as_deref().unwrap_or("default")
}

/// PascalCase a snake_case job name for its `{Name}Payload` struct:
/// `send_welcome_email` -> `SendWelcomeEmail`. (Job names are validated
/// `^[a-z][a-z0-9_]*$`, so each underscore-separated word capitalizes cleanly.)
fn pascal(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    for word in snake.split('_') {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// The tool-owned `crates/jobs/Cargo.toml`. Depends on the facade `jerrycan`
/// (workspace dep, carrying the app's `jobs` feature) for the `Jobs` builder +
/// `JOBS_MIGRATIONS`, plus `serde_json` (queue closures deserialize payloads).
/// No `shared` dep — jobs don't touch the cross-module DTO crate. `tokio` is a
/// dev-dependency so the tool-owned `tests/acceptance.rs` can use `#[tokio::test]`
/// (mirrors ROUTE_CARGO's dev-deps). Regenerated each run.
pub fn cargo_toml() -> String {
    "[package]\nname = \"jobs\"\nversion.workspace = true\nedition.workspace = true\npublish = false\n\n[dependencies]\njerrycan.workspace = true\nserde.workspace = true\nserde_json.workspace = true\n\n[dev-dependencies]\ntokio.workspace = true\n".to_string()
}

/// The tool-owned registry + wiring `src/lib.rs`. Declares one agent-owned task
/// module per job, then exports `jobs(db)` building the fully-wired `Jobs`:
/// `.queue(...)` per distinct queue (sorted), `.register(...)` per job (array
/// order), `.cron(...)` per cron job (array order). Byte-identical across runs.
pub fn registry_rs(design: &Design) -> String {
    // Agent-owned task module declarations, in array order. `pub` so the
    // tool-owned `tests/acceptance.rs` integration test can reach each task fn
    // as `jobs::{name}::{name}` (an integration test sees only the crate's
    // public surface).
    let mods: String = design
        .jobs
        .iter()
        .map(|j| format!("pub mod {};\n", j.name))
        .collect();

    // Distinct queues, sorted deterministically — each gets one worker pool.
    let queues: BTreeSet<&str> = design.jobs.iter().map(job_queue).collect();
    let queue_lines: String = queues
        .iter()
        .map(|q| format!("        .queue(\"{q}\", DEFAULT_CONCURRENCY)\n"))
        .collect();

    // One `.register(...)` per job, in array order. Cron jobs ignore the payload;
    // queue jobs deserialize it into their `{Name}Payload` before calling.
    let register_lines: String = design
        .jobs
        .iter()
        .map(|j| {
            let name = &j.name;
            if j.schedule.is_some() {
                // Cron: owned ctx, no payload.
                format!(
                    "        .register(\n            \"{name}\",\n            std::sync::Arc::new(|ctx: jerrycan::TaskContext, _payload: serde_json::Value| -> jerrycan::jobs::JobFuture<'static, ()> {{\n                Box::pin({name}::{name}(ctx))\n            }}),\n        )\n"
                )
            } else {
                // Queue: deserialize the JSON payload into the task module's
                // `{Name}Payload` (qualified by the module path — the struct lives
                // in the agent-owned `mod {name}`, not at the crate root).
                let payload = format!("{name}::{}Payload", pascal(name));
                format!(
                    "        .register(\n            \"{name}\",\n            std::sync::Arc::new(|ctx: jerrycan::TaskContext, payload: serde_json::Value| -> jerrycan::jobs::JobFuture<'static, ()> {{\n                Box::pin(async move {{\n                    // A no-payload enqueue carries `Value::Null` (NewJob's default);\n                    // `from_value(Null)` into a struct fails, so treat null as the\n                    // default payload (the struct derives Default) rather than\n                    // erroring → retries → dead-letter.\n                    let p: {payload} = if payload.is_null() {{\n                        Default::default()\n                    }} else {{\n                        serde_json::from_value(payload)\n                            .map_err(|e| jerrycan::Error::unprocessable(format!(\"bad job payload: {{e}}\")))?\n                    }};\n                    {name}::{name}(ctx, p).await\n                }})\n            }}),\n        )\n"
                )
            }
        })
        .collect();

    // One `.cron(...)` per cron job, in array order, on its queue.
    let cron_lines: String = design
        .jobs
        .iter()
        .filter_map(|j| {
            j.schedule.as_ref().map(|expr| {
                format!(
                    "        .cron(\"{name}\", \"{expr}\", \"{queue}\")\n",
                    name = j.name,
                    queue = job_queue(j),
                )
            })
        })
        .collect();

    format!(
        "//! GENERATED by jerrycan — the job dispatch registry + the wired `Jobs`\n\
         //! extension. TOOL-OWNED: `jerrycan generate` rewrites this file. The task\n\
         //! fns (and `{{Name}}Payload` structs) live in the agent-owned per-job modules.\n\
         #![forbid(unsafe_code)]\n\n\
         {mods}\n\
         /// Worker-loop concurrency per declared queue. Tune per queue after profiling.\n\
         const DEFAULT_CONCURRENCY: u32 = {DEFAULT_CONCURRENCY};\n\n\
         /// Build the fully-wired background-job extension: one worker pool per declared\n\
         /// queue, every task fn registered, and each cron job scheduled on its queue.\n\
         /// `db` backs the durable Postgres store — jobs are at-least-once.\n\
         pub fn jobs(db: jerrycan::db::Db) -> jerrycan::jobs::Jobs {{\n\
         \x20   jerrycan::jobs::Jobs::postgres(db)\n\
         {queue_lines}{register_lines}{cron_lines}}}\n"
    )
}

/// An agent-owned per-job task module. Cron jobs get a 1-arg owned-ctx stub;
/// queue jobs get a `{Name}Payload` struct + a 2-arg stub. The stub returns a
/// 500 until implemented and carries the at-least-once idempotency reminder.
pub fn task_rs(job: &JobDesign) -> String {
    let name = &job.name;
    let idempotency =
        "    // jobs are at-least-once — make this idempotent (it may run more than once).\n";
    let unimpl = format!(
        "    Err(jerrycan::Error::internal(\"{name} not implemented — replace this stub\"))\n"
    );
    if job.schedule.is_some() {
        // Cron: owned ctx, no payload (JobFn passes an owned TaskContext).
        format!(
            "//! Background job `{name}` (cron). Agent-owned: implement the task here.\n\
             //! Regeneration never clobbers this file.\n\n\
             use jerrycan::TaskContext;\n\n\
             /// The `{name}` cron task. The leader enqueues it each due tick.\n\
             pub async fn {name}(mut _ctx: TaskContext) -> jerrycan::Result<()> {{\n\
             {idempotency}{unimpl}}}\n"
        )
    } else {
        // Queue: payload struct + 2-arg stub.
        let payload = format!("{}Payload", pascal(name));
        format!(
            "//! Background job `{name}` (queue). Agent-owned: implement the task here.\n\
             //! Regeneration never clobbers this file.\n\n\
             use jerrycan::TaskContext;\n\
             use serde::{{Deserialize, Serialize}};\n\n\
             /// The typed payload `{name}` is enqueued with. Add the fields the job needs.\n\
             /// `Default` lets the tool-owned acceptance test call the task with an\n\
             /// empty payload (`{payload}::default()`); keep it derivable as fields grow.\n\
             #[derive(Debug, Clone, Default, Serialize, Deserialize)]\n\
             pub struct {payload} {{}}\n\n\
             /// The `{name}` queue task, run with its deserialized payload.\n\
             pub async fn {name}(mut _ctx: TaskContext, _payload: {payload}) -> jerrycan::Result<()> {{\n\
             {idempotency}{unimpl}}}\n"
        )
    }
}

/// The tool-owned `crates/jobs/tests/acceptance.rs` — the TDD-red contract for
/// the declared jobs, mirroring `crates/routes/<m>/tests/acceptance.rs`. One
/// `#[tokio::test]` per job that calls the task fn DIRECTLY with a `TaskContext`
/// and asserts it succeeds.
///
/// Why direct task-fn calls (not the HTTP flow): a job runs in an `on_serve`
/// loop, and `App::into_test` DROPS the `on_serve` registrations — so a job can
/// never be reached through `TestApp`'s request path. The test instead builds a
/// `TestApp` purely for its app-level deps (the `Db` every job resolves), takes a
/// `TaskContext` via `t.task_context()`, and invokes the task fn itself:
/// - cron job: `jobs::{name}::{name}(t.task_context())`
/// - queue job: `jobs::{name}::{name}(t.task_context(), Default::default())`
///   (the `{Name}Payload` derives `Default`).
///
/// The stub returns `Err(...)` ⇒ RED; an implemented job returns `Ok(())` ⇒
/// GREEN. Jobs are emitted in `design.jobs` array order; byte-identical runs.
pub fn acceptance_rs(design: &Design) -> String {
    let body: String = design
        .jobs
        .iter()
        .map(|job| {
            let name = &job.name;
            // at-least-once reminder mirrors the stub: an implemented job must be
            // idempotent because the engine may run it more than once.
            let call = if job.schedule.is_some() {
                format!("jobs::{name}::{name}(t.task_context()).await")
            } else {
                format!("jobs::{name}::{name}(t.task_context(), Default::default()).await")
            };
            format!(
                "/// Job `{name}` must succeed once implemented (jobs are at-least-once —\n\
                 /// the implementation must be idempotent). RED on the stub (it returns Err).\n\
                 #[tokio::test]\n\
                 async fn {name}_succeeds() {{\n\
                 \x20   let t = app().await;\n\
                 \x20   let res = {call};\n\
                 \x20   assert!(res.is_ok(), \"design: job {name} must succeed; got {{res:?}}\");\n\
                 }}\n\n"
            )
        })
        .collect();
    // The jobs `app()` preamble: every job resolves the app-level `Db` (jobs
    // require `db`), so the test app connects an in-memory db, applies the
    // framework `JOBS_MIGRATIONS` (so the `jerrycan_jobs*` tables an implemented
    // job may touch exist), and extends the db. `into_test` would drop any
    // `on_serve` loops, but we call the task fns directly so that's irrelevant.
    let _ = design; // jobs preamble is design-independent (jobs always need db)
    format!(
        "//! GENERATED by jerrycan gen-tests — TOOL-OWNED acceptance criteria for the\n\
         //! declared jobs. One test per job, calling the task fn directly with a\n\
         //! TaskContext (a job's on_serve loop is dropped by into_test, so the HTTP\n\
         //! flow can't reach it). Regenerated on demand; add your own tests in sibling\n\
         //! files, not here. Green = the design's jobs are implemented.\n\
         use jerrycan::prelude::*;\n\n\
         async fn app() -> TestApp {{\n\
         \x20   let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n\
         \x20   db.migrate(jerrycan::jobs::JOBS_MIGRATIONS).await.expect(\"jobs migrations\");\n\
         \x20   App::new().extend(db).into_test()\n\
         }}\n\n\
         {body}"
    )
}

/// Write (or refresh) the top-level `crates/jobs/` crate under `target` (the app
/// root). TOOL-owned `Cargo.toml` + `src/lib.rs` are rewritten every run; each
/// AGENT-owned `src/{name}.rs` task module is create-once (never clobbered).
/// Returns the paths written, relative to `target`. Precondition: the design has
/// passed `questions::validate` (validated, db-backed job names).
pub fn write_jobs(target: &Path, design: &Design) -> Result<Vec<String>, String> {
    let crate_dir = target.join("crates/jobs");
    let src = crate_dir.join("src");
    fs::create_dir_all(&src).map_err(|e| e.to_string())?;
    let mut created = Vec::new();

    let mut write_tool = |rel: &str, content: &str| -> Result<(), String> {
        let path = crate_dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        created.push(format!("crates/jobs/{rel}"));
        Ok(())
    };
    write_tool("Cargo.toml", &cargo_toml())?;
    write_tool("src/lib.rs", &registry_rs(design))?;
    // The tool-owned acceptance tests (the TDD-red contract). Rewritten each run
    // like the registry; the gen-tests path rewrites the SAME file and reports
    // its count toward `expected_failing`.
    write_tool("tests/acceptance.rs", &acceptance_rs(design))?;

    // Agent-owned task modules: never clobber an existing one.
    for job in &design.jobs {
        let rel = format!("src/{}.rs", job.name);
        let path = crate_dir.join(&rel);
        if path.exists() {
            continue;
        }
        fs::write(&path, task_rs(job)).map_err(|e| format!("write {}: {e}", path.display()))?;
        created.push(format!("crates/jobs/{rel}"));
    }
    Ok(created)
}

/// Write the tool-owned `crates/jobs/tests/acceptance.rs` and return its
/// `(rel_path, expected_failing)` — the count of generated job tests that fail on
/// the stubs. The `gen-tests` command threads this into the same
/// `expected_failing` total as the HTTP acceptance tests (testgen::write_acceptance).
/// Returns `None` when the design declares no jobs (nothing to write or count).
pub fn write_jobs_acceptance(
    root: &Path,
    design: &Design,
) -> Result<Option<(String, usize)>, String> {
    if !design.wants_jobs() {
        return Ok(None);
    }
    let content = acceptance_rs(design);
    let rel = "crates/jobs/tests/acceptance.rs".to_string();
    let path = root.join(&rel);
    fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
    fs::write(&path, &content).map_err(|e| e.to_string())?;
    let count = content.matches("#[tokio::test]").count();
    Ok(Some((rel, count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> Design {
        Design::from_path(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../conformance/designs/reference-slice.design.json"
        )))
        .unwrap()
    }

    /// A queue-only job (no schedule) we graft onto reference to exercise the payload
    /// path without touching the frozen fixture.
    fn queue_job() -> JobDesign {
        serde_json::from_str(r#"{ "name": "send_welcome_email" }"#).unwrap()
    }

    /// The registry for reference's two CRON jobs is byte-identical across two calls
    /// (determinism is the contract: JL0003 compares against exactly this output).
    /// Both jobs carry a schedule, so both register the 1-arg cron closure.
    #[test]
    fn registry_is_deterministic_and_wires_cron_jobs() {
        let d = reference();
        let a = registry_rs(&d);
        let b = registry_rs(&d);
        assert_eq!(
            a, b,
            "registry generation must be byte-identical across runs"
        );

        // expire_trials → queue "billing"; overdue_callbacks → default "default".
        // Distinct queues sorted: billing before default.
        let billing = a.find(".queue(\"billing\", DEFAULT_CONCURRENCY)").unwrap();
        let default = a.find(".queue(\"default\", DEFAULT_CONCURRENCY)").unwrap();
        assert!(
            billing < default,
            "queues must be sorted deterministically: {a}"
        );

        // Both are cron: 1-arg closures + `.cron(...)` lines on their queues.
        assert!(
            a.contains("Box::pin(expire_trials::expire_trials(ctx))"),
            "cron closure wraps the 1-arg task: {a}"
        );
        assert!(
            a.contains(".cron(\"expire_trials\", \"0 * * * *\", \"billing\")"),
            "{a}"
        );
        assert!(
            a.contains(".cron(\"overdue_callbacks\", \"*/5 * * * *\", \"default\")"),
            "{a}"
        );
        // Cron jobs carry NO payload deserialization.
        assert!(!a.contains("from_value"), "cron jobs take no payload: {a}");
        // Registrations follow array order (expire_trials before overdue_callbacks).
        assert!(
            a.find("\"expire_trials\"").unwrap() < a.find("\"overdue_callbacks\"").unwrap(),
            "register order follows design.jobs: {a}"
        );
        // Task modules declared `pub` for each job (so the acceptance integration
        // test can reach `jobs::{name}::{name}` through the crate's public surface).
        assert!(a.contains("pub mod expire_trials;") && a.contains("pub mod overdue_callbacks;"));
    }

    /// A queue-only job (no schedule) registers the 2-arg closure that
    /// deserializes the JSON payload into its `{Name}Payload`, and gets NO
    /// `.cron(...)` line. Its queue defaults to "default".
    #[test]
    fn registry_wires_queue_job_with_payload_deserialization() {
        let mut d = reference();
        d.jobs.push(queue_job());
        let r = registry_rs(&d);
        assert!(
            r.contains("let p: send_welcome_email::SendWelcomeEmailPayload = if payload.is_null()"),
            "queue closure binds the module-qualified payload struct, handling null: {r}"
        );
        assert!(
            r.contains("serde_json::from_value(payload)"),
            "queue closure deserializes a non-null payload: {r}"
        );
        // A no-payload enqueue carries Value::Null; from_value(Null) into a struct
        // fails, so the closure must fall back to Default rather than erroring →
        // retries → dead-letter.
        assert!(
            r.contains("if payload.is_null()") && r.contains("Default::default()"),
            "queue closure treats a null payload as the default (no-payload enqueue): {r}"
        );
        assert!(
            r.contains("send_welcome_email::send_welcome_email(ctx, p).await"),
            "queue closure calls the 2-arg task: {r}"
        );
        // No schedule → no cron line for this job.
        assert!(
            !r.contains(".cron(\"send_welcome_email\""),
            "a queue-only job is never scheduled: {r}"
        );
    }

    /// A cron task stub takes a single owned `TaskContext`, returns a 500, and
    /// carries the at-least-once idempotency reminder. No payload struct.
    #[test]
    fn cron_task_stub_is_one_arg_owned_ctx() {
        let d = reference();
        let stub = task_rs(&d.jobs[0]); // expire_trials (cron)
        assert!(
            stub.contains(
                "pub async fn expire_trials(mut _ctx: TaskContext) -> jerrycan::Result<()>"
            ),
            "cron stub is 1-arg owned ctx: {stub}"
        );
        assert!(
            !stub.contains("Payload"),
            "cron stub has no payload struct: {stub}"
        );
        assert!(
            stub.contains("jobs are at-least-once — make this idempotent"),
            "idempotency reminder: {stub}"
        );
        assert!(
            stub.contains("expire_trials not implemented — replace this stub"),
            "{stub}"
        );
    }

    /// A queue task stub generates a `{Name}Payload` struct + a 2-arg stub taking
    /// owned ctx and the typed payload.
    #[test]
    fn queue_task_stub_has_payload_struct_and_two_args() {
        let stub = task_rs(&queue_job());
        assert!(
            stub.contains("pub struct SendWelcomeEmailPayload {}"),
            "payload struct: {stub}"
        );
        // The payload derives `Default` so the tool-owned acceptance test can call
        // the task with `Default::default()`.
        assert!(
            stub.contains("#[derive(Debug, Clone, Default, Serialize, Deserialize)]"),
            "payload derives Default for the acceptance test: {stub}"
        );
        assert!(
            stub.contains(
                "pub async fn send_welcome_email(mut _ctx: TaskContext, _payload: SendWelcomeEmailPayload) -> jerrycan::Result<()>"
            ),
            "queue stub is 2-arg: {stub}"
        );
        assert!(
            stub.contains("jobs are at-least-once — make this idempotent"),
            "{stub}"
        );
    }

    /// The tool-owned `tests/acceptance.rs` for reference's two CRON jobs: one
    /// `#[tokio::test]` per job, each calling the task fn DIRECTLY (1-arg cron
    /// shape) and asserting the result `is_ok()`. This is the TDD-red contract
    /// the `gen-tests` `expected_failing` count comes from.
    #[test]
    fn acceptance_emits_one_is_ok_test_per_cron_job() {
        let d = reference();
        let a = acceptance_rs(&d);
        // Exactly two tests — one per declared job.
        assert_eq!(
            a.matches("#[tokio::test]").count(),
            2,
            "one #[tokio::test] per job: {a}"
        );
        // Each is a direct 1-arg cron call asserting is_ok (RED on the Err stub).
        assert!(
            a.contains("async fn expire_trials_succeeds()")
                && a.contains("jobs::expire_trials::expire_trials(t.task_context()).await"),
            "cron job calls the 1-arg task fn directly: {a}"
        );
        assert!(
            a.contains("async fn overdue_callbacks_succeeds()")
                && a.contains("jobs::overdue_callbacks::overdue_callbacks(t.task_context()).await"),
            "second cron job: {a}"
        );
        assert!(
            a.matches("assert!(res.is_ok()").count() == 2,
            "every job test asserts the result is_ok: {a}"
        );
        // No 2-arg payload call for cron jobs.
        assert!(
            !a.contains("Default::default()"),
            "cron jobs take no payload: {a}"
        );
        // The at-least-once idempotency note rides along in the generated tests.
        assert!(
            a.contains("jobs are at-least-once"),
            "idempotency note present in the generated tests: {a}"
        );
        // Tests are emitted in design.jobs array order.
        assert!(
            a.find("expire_trials_succeeds").unwrap()
                < a.find("overdue_callbacks_succeeds").unwrap(),
            "test order follows design.jobs: {a}"
        );
    }

    /// Acceptance generation is byte-identical across two calls — determinism is
    /// the contract (the gen-tests count and the file content must be stable).
    #[test]
    fn acceptance_is_deterministic() {
        let d = reference();
        assert_eq!(
            acceptance_rs(&d),
            acceptance_rs(&d),
            "acceptance generation must be byte-identical across runs"
        );
    }

    /// A QUEUE-only job (no schedule) gets the 2-arg acceptance call with
    /// `Default::default()` as the payload — which compiles because the generated
    /// `{Name}Payload` derives `Default` (see queue_task_stub test).
    #[test]
    fn acceptance_emits_two_arg_default_payload_call_for_queue_job() {
        let mut d = reference();
        d.jobs.push(queue_job());
        let a = acceptance_rs(&d);
        assert!(
            a.contains(
                "jobs::send_welcome_email::send_welcome_email(t.task_context(), Default::default()).await"
            ),
            "queue job calls the 2-arg task fn with a default payload: {a}"
        );
        assert!(
            a.contains("async fn send_welcome_email_succeeds()"),
            "queue job test fn: {a}"
        );
    }

    /// `write_jobs` also writes the tool-owned `tests/acceptance.rs` (so the
    /// generated jobs crate's acceptance tests are on disk and compile under the
    /// `--all-targets` clippy/compile gate).
    #[test]
    fn write_jobs_writes_the_acceptance_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let created = write_jobs(tmp.path(), &reference()).unwrap();
        assert!(
            created.contains(&"crates/jobs/tests/acceptance.rs".to_string()),
            "write_jobs reports the acceptance file: {created:?}"
        );
        let acc = tmp.path().join("crates/jobs/tests/acceptance.rs");
        assert!(acc.exists(), "tests/acceptance.rs written to disk");
        assert!(
            fs::read_to_string(&acc).unwrap().contains("#[tokio::test]"),
            "the written acceptance file carries the job tests"
        );
    }

    /// `write_jobs_acceptance` writes the file and returns its `(rel, count)` —
    /// the count of `#[tokio::test]` fns that the gen-tests command adds to
    /// `expected_failing`. For reference that is its two cron jobs.
    #[test]
    fn write_jobs_acceptance_returns_path_and_failing_count() {
        let tmp = tempfile::tempdir().unwrap();
        let (rel, count) = write_jobs_acceptance(tmp.path(), &reference())
            .unwrap()
            .unwrap();
        assert_eq!(rel, "crates/jobs/tests/acceptance.rs");
        assert_eq!(
            count, 2,
            "reference's two jobs each contribute one failing test"
        );
        assert!(
            tmp.path().join(&rel).exists(),
            "the acceptance file is written to disk"
        );
    }

    /// A design with no jobs writes nothing and contributes nothing to the count
    /// (`None`) — gen-tests must not add a phantom jobs total.
    #[test]
    fn write_jobs_acceptance_is_none_without_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut d = reference();
        d.jobs.clear();
        assert!(!d.wants_jobs());
        assert!(
            write_jobs_acceptance(tmp.path(), &d).unwrap().is_none(),
            "no jobs ⇒ no acceptance file, no count"
        );
        assert!(
            !tmp.path().join("crates/jobs/tests/acceptance.rs").exists(),
            "nothing written when there are no jobs"
        );
    }

    /// `write_jobs` rewrites the tool-owned registry but never clobbers an
    /// agent-edited task module (the ownership rule, mirroring write_module).
    #[test]
    fn write_jobs_respects_the_ownership_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let d = reference();
        let created = write_jobs(tmp.path(), &d).unwrap();
        assert!(created.contains(&"crates/jobs/Cargo.toml".to_string()));
        assert!(created.contains(&"crates/jobs/src/lib.rs".to_string()));
        assert!(created.contains(&"crates/jobs/src/expire_trials.rs".to_string()));

        // Agent edits a task module; tool hand-edits lib.rs (illegally).
        let task = tmp.path().join("crates/jobs/src/expire_trials.rs");
        fs::write(&task, "// AGENT CODE\n").unwrap();
        let lib = tmp.path().join("crates/jobs/src/lib.rs");
        fs::write(&lib, "// hand edit\n").unwrap();

        write_jobs(tmp.path(), &d).unwrap();
        assert_eq!(
            fs::read_to_string(&task).unwrap(),
            "// AGENT CODE\n",
            "agent-owned task module: preserved"
        );
        assert!(
            fs::read_to_string(&lib).unwrap().contains("pub fn jobs("),
            "tool-owned registry: restored"
        );
    }
}
