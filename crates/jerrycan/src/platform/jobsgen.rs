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
//! are sorted before the `.queue(...)` calls, and the `pub mod` declarations are
//! sorted alphabetically (rustfmt's `reorder_modules`). The output is byte-identical
//! across runs.

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

/// The queue closure's `let p: {payload} = if payload.is_null() { … } else { … };`
/// payload bind, pre-wrapped exactly as the pinned rustfmt (1.9.0, edition 2024)
/// formats it at the closure's fixed indent (24 cols). The only width-variable token
/// is the fully-qualified payload path, so there are three regimes, measured against
/// rustfmt (issue #218):
///   - the head fits on one line (`… = if payload.is_null() {`): payload ≤ 44 cols.
///   - the head minus its trailing `{` fits (payload 45–46 cols): rustfmt drops the
///     `{` onto its own line at the `let` indent.
///   - wider (payload ≥ 47 cols): rustfmt breaks after `=`, indenting the whole
///     if-expr one level deeper.
///
/// Reproducing all three keeps a fresh scaffold's registry a `cargo fmt` fixpoint for
/// every realistic job name (the earlier emitter always emitted the first form, so any
/// name whose payload exceeded 44 cols drifted). Output starts at 24 cols, ends `};\n`.
fn payload_bind(payload: &str) -> String {
    let mut out = String::new();
    let push = |out: &mut String, indent: usize, text: &str| {
        out.push_str(&" ".repeat(indent));
        out.push_str(text);
        out.push('\n');
    };
    let from_value = "serde_json::from_value(payload).map_err(|e| {";
    let inner = "jerrycan::Error::unprocessable(format!(\"bad job payload: {e}\"))";
    let head_one = format!("let p: {payload} = if payload.is_null() {{");
    let head_no_brace = format!("let p: {payload} = if payload.is_null()");
    if 24 + head_one.chars().count() <= 100 {
        push(&mut out, 24, &head_one);
        push(&mut out, 28, "Default::default()");
        push(&mut out, 24, "} else {");
        push(&mut out, 28, from_value);
        push(&mut out, 32, inner);
        push(&mut out, 28, "})?");
        push(&mut out, 24, "};");
    } else if 24 + head_no_brace.chars().count() <= 100 {
        push(&mut out, 24, &head_no_brace);
        push(&mut out, 24, "{");
        push(&mut out, 28, "Default::default()");
        push(&mut out, 24, "} else {");
        push(&mut out, 28, from_value);
        push(&mut out, 32, inner);
        push(&mut out, 28, "})?");
        push(&mut out, 24, "};");
    } else {
        push(&mut out, 24, &format!("let p: {payload} ="));
        push(&mut out, 28, "if payload.is_null() {");
        push(&mut out, 32, "Default::default()");
        push(&mut out, 28, "} else {");
        push(&mut out, 32, from_value);
        push(&mut out, 36, inner);
        push(&mut out, 32, "})?");
        push(&mut out, 28, "};");
    }
    out
}

/// The acceptance test's `let res = <call>.await;` binding, pre-wrapped exactly as the
/// pinned rustfmt formats it (issue #218). `head` is the fully-qualified task-fn path
/// and `args` its call arguments; the awaited call's layout is a pure function of their
/// combined width, so a table of width-gated regimes (measured against rustfmt) makes
/// it a `cargo fmt` fixpoint for every job name — unlike the earlier binary rule, which
/// dropped the value onto its own line but never broke the call arguments, so any name
/// long enough to overflow the value line drifted. Widest realistic name lands in the
/// first few regimes; the last two only fire for pathologically long names. In order:
///   1. the whole `let res = …await;` fits (≤ 100): one line.
///   2. value on its own line (indent 8) with `.await;` attached.
///   3. value on its own line with `.await` broken off (the call fills the line).
///   4. call broken open, one arg per line, head on the `let` line.
///   5. same, but the broken head itself overflows, so it sits under a broken `let res =`.
///   6. even the indent-8 head overflows: rustfmt gives up and keeps one line.
///
/// Output ends with a trailing newline.
fn wrap_res_await(head: &str, args: &[&str]) -> String {
    let call = format!("{head}({})", args.join(", "));
    let one = format!("    let res = {call}.await;");
    if one.chars().count() <= 100 {
        return format!("{one}\n");
    }
    let val_await = format!("        {call}.await;");
    if val_await.chars().count() <= 100 {
        return format!("    let res =\n{val_await}\n");
    }
    let val = format!("        {call}");
    if val.chars().count() < 100 {
        return format!("    let res =\n{val}\n            .await;\n");
    }
    let r3_head = format!("    let res = {head}(");
    if r3_head.chars().count() <= 100 {
        let mut out = format!("{r3_head}\n");
        for a in args {
            out.push_str(&format!("        {a},\n"));
        }
        out.push_str("    )\n    .await;\n");
        return out;
    }
    let r4_head = format!("        {head}(");
    if r4_head.chars().count() <= 100 {
        let mut out = format!("    let res =\n{r4_head}\n");
        for a in args {
            out.push_str(&format!("            {a},\n"));
        }
        out.push_str("        )\n        .await;\n");
        return out;
    }
    format!("{one}\n")
}

/// The cron closure body `Box::pin({name}::{name}(ctx))` at the fixed closure-body
/// indent (20 cols), pre-wrapped EXACTLY as the pinned rustfmt (1.9.0, edition 2024)
/// formats it (issue #221). rustfmt's wrap here is NON-MONOTONIC in the job-name length
/// `L` — the one-line candidate width is `2*L + 37`. MEASURED against the oracle (emit
/// this line at each length, run `rustfmt --edition 2024`, record the wrap):
///
///   - `L ≤ 26` (candW ≤ 89): ONE line.
///   - `L = 27, 28` (candW 91, 93): the INNER call breaks its `ctx` arg — the inner
///     `{name}::{name}(ctx)` first exceeds `fn_call_width` (60) here.
///   - `L = 29, 30, 31` (candW 95, 97, 99): ONE line AGAIN — the outer `Box::pin(…)`
///     one-line still fits `max_width` (100), so rustfmt keeps it whole.
///   - `L = 32, 33, 34` (candW 101–105): the OUTER `Box::pin(` breaks, inner one line.
///   - `L = 35, 36` (candW 107, 109): BOTH break.
///   - `L ≥ 37` (candW ≥ 111): ONE line AGAIN — breaking cannot help (the inner callee
///     `{name}::{name}(` itself overflows 100 at indent 24), so rustfmt gives up.
///
/// A naive "break when the inner call exceeds `fn_call_width` (60)" is WRONG: it would
/// regress the 29–31 and ≥37 one-line ranges. This reproduces rustfmt's actual output
/// so a fresh scaffold's cron registry is a `cargo fmt` fixpoint for every realistic
/// cron name (up to ~40 chars). (A 1-char job name inlines the whole closure body onto
/// the return-type line — a separate pathological regime not handled; realistic job
/// names are ≥ 2 chars.) Mirrors the width-sensitivity precedent of `payload_bind` /
/// `wrap_res_await`. Output starts at 20 cols, no trailing newline.
fn cron_box_pin(name: &str) -> String {
    let l = name.chars().count();
    match l {
        // INNER call breaks its `ctx` arg (the outer `Box::pin(…)` still fits).
        27 | 28 => format!(
            "                    Box::pin({name}::{name}(\n                        ctx,\n                    ))"
        ),
        // OUTER `Box::pin(` breaks; the inner call stays on one line.
        32..=34 => format!(
            "                    Box::pin(\n                        {name}::{name}(ctx),\n                    )"
        ),
        // BOTH the outer and inner calls break.
        35 | 36 => format!(
            "                    Box::pin(\n                        {name}::{name}(\n                            ctx,\n                        ),\n                    )"
        ),
        // One line: L ≤ 26, L ∈ 29..=31, and L ≥ 37 (rustfmt gives up).
        _ => format!("                    Box::pin({name}::{name}(ctx))"),
    }
}

/// The tool-owned registry + wiring `src/lib.rs`. Declares one agent-owned task
/// module per job, then exports `jobs(db)` building the fully-wired `Jobs`:
/// `.queue(...)` per distinct queue (sorted), `.register(...)` per job (array
/// order), `.cron(...)` per cron job (array order). Byte-identical across runs.
pub fn registry_rs(design: &Design) -> String {
    // Agent-owned task module declarations, SORTED alphabetically. rustfmt's
    // `reorder_modules` sorts `pub mod` declarations by name, so emitting them in
    // `design.jobs` array order drifts under `cargo fmt` whenever the design order
    // isn't already alphabetical (issue #218). Pre-sorting makes the scaffold a
    // fixpoint regardless of design order. Job names are ASCII (`^[a-z][a-z0-9_]*$`),
    // so `str` byte order matches rustfmt's. `pub` so the tool-owned
    // `tests/acceptance.rs` integration test can reach each task fn as
    // `jobs::{name}::{name}` (an integration test sees only the crate's public surface).
    let mut mod_names: Vec<&str> = design.jobs.iter().map(|j| j.name.as_str()).collect();
    mod_names.sort_unstable();
    let mods: String = mod_names
        .iter()
        .map(|n| format!("pub mod {n};\n"))
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
                // Cron: owned ctx, no payload. The closure signature is a fixed,
                // name-independent width that always exceeds `max_width` (100), so
                // rustfmt always opens `Arc::new(` and breaks the closure params
                // one per line — pre-wrapped here (issue #218) so a fresh scaffold's
                // registry is a `cargo fmt` fixpoint. The closure BODY
                // `Box::pin({name}::{name}(ctx))` wraps NON-MONOTONICALLY in the name
                // length; `cron_box_pin` reproduces rustfmt's actual output (issue #221)
                // — the earlier always-one-line form drifted for names of 27–28 and
                // 32–36 cols.
                let box_pin = cron_box_pin(name);
                format!(
                    "        .register(\n            \"{name}\",\n            std::sync::Arc::new(\n                |ctx: jerrycan::TaskContext,\n                 _payload: serde_json::Value|\n                 -> jerrycan::jobs::JobFuture<'static, ()> {{\n{box_pin}\n                }},\n            ),\n        )\n"
                )
            } else {
                // Queue: deserialize the JSON payload into the task module's
                // `{Name}Payload` (qualified by the module path — the struct lives
                // in the agent-owned `mod {name}`, not at the crate root).
                let payload = format!("{name}::{}Payload", pascal(name));
                // Pre-wrapped as the pinned rustfmt formats it (issue #218): the
                // fixed-width closure signature always breaks the params one per
                // line, deepening the body indent so the `.map_err` chain reflows
                // too. The `let p: {payload} = …` bind is width-sensitive on the
                // fully-qualified payload path; `payload_bind` reproduces rustfmt's
                // three wrap regimes so the registry is a fixpoint for every realistic
                // job name (the earlier single-line form drifted for a payload > 44 cols).
                let bind = payload_bind(&payload);
                format!(
                    "        .register(\n            \"{name}\",\n            std::sync::Arc::new(\n                |ctx: jerrycan::TaskContext,\n                 payload: serde_json::Value|\n                 -> jerrycan::jobs::JobFuture<'static, ()> {{\n                    Box::pin(async move {{\n                        // A no-payload enqueue carries `Value::Null` (NewJob's default);\n                        // `from_value(Null)` into a struct fails, so treat null as the\n                        // default payload (the struct derives Default) rather than\n                        // erroring → retries → dead-letter.\n{bind}                        {name}::{name}(ctx, p).await\n                    }})\n                }},\n            ),\n        )\n"
                )
            }
        })
        .collect();

    // One `.cron(...)` per cron job, in array order, on its queue. rustfmt breaks a
    // call's args one per line once they exceed `fn_call_width` (60) — a long cron name
    // pushes `.cron("name", "expr", "queue")` past it, so pre-wrap it (issue #221) to
    // stay a `cargo fmt` fixpoint. The args are `"name", "expr", "queue"`; their width is
    // name + expr + queue + 10 (the six quotes plus the two `, ` separators). A short
    // name keeps the one-line form, byte-identical to pre-#221.
    let cron_lines: String = design
        .jobs
        .iter()
        .filter_map(|j| {
            j.schedule.as_ref().map(|expr| {
                let name = &j.name;
                let queue = job_queue(j);
                let args_w =
                    name.chars().count() + expr.chars().count() + queue.chars().count() + 10;
                if args_w <= 60 {
                    format!("        .cron(\"{name}\", \"{expr}\", \"{queue}\")\n")
                } else {
                    format!(
                        "        .cron(\n            \"{name}\",\n            \"{expr}\",\n            \"{queue}\",\n        )\n"
                    )
                }
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
    // Pre-wrapped exactly as the pinned rustfmt formats it (issue #218): the
    // fully-qualified `jerrycan::Error::internal(...)` path is long enough that
    // rustfmt always breaks the string arg onto its own line (unlike genroute's
    // unqualified `Error::internal`, which is width-gated), so this wraps
    // unconditionally for every valid job name.
    let unimpl = format!(
        "    Err(jerrycan::Error::internal(\n        \"{name} not implemented — replace this stub\",\n    ))\n"
    );
    if job.schedule.is_some() {
        // Cron: owned ctx, no payload (JobFn passes an owned TaskContext). The 1-arg
        // signature is pre-wrapped as the pinned rustfmt does (issue #221) — one param
        // per line once the one-line form exceeds `max_width` (100), which a long cron
        // name (≥ 39 cols) hits. A short-name stub stays on one line (byte-identical to
        // pre-#221). Mirrors the queue-stub width gate below.
        let sig_one =
            format!("pub async fn {name}(mut _ctx: TaskContext) -> jerrycan::Result<()> {{");
        let signature = if sig_one.chars().count() <= 100 {
            format!("{sig_one}\n")
        } else {
            format!(
                "pub async fn {name}(\n    mut _ctx: TaskContext,\n) -> jerrycan::Result<()> {{\n"
            )
        };
        format!(
            "//! Background job `{name}` (cron). Agent-owned: implement the task here.\n\
             //! Regeneration never clobbers this file.\n\n\
             use jerrycan::TaskContext;\n\n\
             /// The `{name}` cron task. The leader enqueues it each due tick.\n\
             {signature}\
             {idempotency}{unimpl}}}\n"
        )
    } else {
        // Queue: payload struct + 2-arg stub.
        let payload = format!("{}Payload", pascal(name));
        // The 2-arg signature is wider than the cron one; pre-wrap it as the pinned
        // rustfmt does (issue #218) — one param per line once the one-line form
        // exceeds `max_width` (100). A short-name stub stays on one line.
        let sig_one = format!(
            "pub async fn {name}(mut _ctx: TaskContext, _payload: {payload}) -> jerrycan::Result<()> {{"
        );
        let signature = if sig_one.chars().count() <= 100 {
            format!("{sig_one}\n")
        } else {
            format!(
                "pub async fn {name}(\n    mut _ctx: TaskContext,\n    _payload: {payload},\n) -> jerrycan::Result<()> {{\n"
            )
        };
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
             {signature}\
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
            // The direct task-fn call: cron takes only the ctx; a queue job also takes a
            // `Default::default()` payload (its `{Name}Payload` derives Default). Jobs are
            // at-least-once, so an implemented job must be idempotent (it may run again).
            let head = format!("jobs::{name}::{name}");
            let args: &[&str] = if job.schedule.is_some() {
                &["t.task_context()"]
            } else {
                &["t.task_context()", "Default::default()"]
            };
            // The `assert!` line is pre-wrapped exactly as the pinned rustfmt does
            // (issue #218): rustfmt keeps the two args on one line until the call
            // exceeds `fn_call_width` (60) — i.e. the single-line form passes width
            // 74 — then breaks each arg onto its own line. Width-gated on the job
            // name so a short-name suite stays byte-identical.
            let assert_one = format!(
                "    assert!(res.is_ok(), \"design: job {name} must succeed; got {{res:?}}\");"
            );
            let assert_block = if assert_one.chars().count() <= 74 {
                format!("{assert_one}\n")
            } else {
                format!(
                    "    assert!(\n        res.is_ok(),\n        \"design: job {name} must succeed; got {{res:?}}\"\n    );\n"
                )
            };
            // The `let res = <call>.await;` binding, pre-wrapped exactly as the pinned
            // rustfmt formats it (issue #218). A queue job's 2-arg call can overflow the
            // line; `wrap_res_await` reproduces rustfmt's wrap regimes (value on its own
            // line / call arguments broken one per line) so a long-named job stays a
            // `cargo fmt` fixpoint — the earlier rule only ever dropped the value onto its
            // own line and drifted once even that overflowed.
            let res_block = wrap_res_await(&head, args);
            format!(
                "/// Job `{name}` must succeed once implemented (jobs are at-least-once —\n\
                 /// the implementation must be idempotent). RED on the stub (it returns Err).\n\
                 #[tokio::test]\n\
                 async fn {name}_succeeds() {{\n\
                 \x20   let t = app().await;\n\
                 {res_block}\
                 {assert_block}\
                 }}\n\n"
            )
        })
        .collect();
    // The jobs `app()` preamble: every job resolves the app-level `Db` (jobs require
    // `db`), so the test app connects an in-memory db and migrates the SAME schema
    // the real app's `App::build` applies — the framework `JOBS_MIGRATIONS` (so the
    // `jerrycan_jobs*` tables exist) PLUS every route module's create-tables
    // migration. A job commonly reads/writes a route-module table (the common case:
    // a job processing app data); migrating only `JOBS_MIGRATIONS` left those tables
    // absent, so a correct job failed `no such table` (issue #84). `include_str!`
    // reaches the route crates' migration files relative to THIS file
    // (`crates/jobs/tests/acceptance.rs` → `../../routes/<m>`). `into_test` would
    // drop any `on_serve` loops, but we call the task fns directly so that's
    // irrelevant.
    // Only emit the second migrate call when there are route tables to migrate — a
    // jobs design with no entity modules keeps the byte-identical single-migrate form.
    // `migrate_call_block` returns "" for zero items, and HUGS a single-element array
    // exactly as rustfmt does (issue #221) — a jobs design with ONE route module
    // otherwise drifted under `cargo fmt`.
    let route_items =
        super::testgen::collect_migration_items(design, |name| format!("../../routes/{name}"));
    let route_migrations = super::testgen::migrate_call_block(&route_items, "route migrations");
    // The `Db::connect(..).await.expect(..)` and `db.migrate(..).await.expect(..)`
    // chains exceed `max_width` (100), so rustfmt breaks each `.await`/`.expect(..)`
    // onto its own line — pre-wrapped here (issue #218) so the scaffold is a
    // `cargo fmt` fixpoint.
    let out = format!(
        "//! GENERATED by jerrycan gen-tests — TOOL-OWNED acceptance criteria for the\n\
         //! declared jobs. One test per job, calling the task fn directly with a\n\
         //! TaskContext (a job's on_serve loop is dropped by into_test, so the HTTP\n\
         //! flow can't reach it). Regenerated on demand; add your own tests in sibling\n\
         //! files, not here. Green = the design's jobs are implemented.\n\
         use jerrycan::prelude::*;\n\n\
         async fn app() -> TestApp {{\n\
         \x20   let db = jerrycan::db::Db::connect(\"sqlite::memory:\")\n\
         \x20       .await\n\
         \x20       .expect(\"test db\");\n\
         \x20   db.migrate(jerrycan::jobs::JOBS_MIGRATIONS)\n\
         \x20       .await\n\
         \x20       .expect(\"jobs migrations\");\n\
         {route_migrations}\
         \x20   App::new().extend(db).into_test()\n\
         }}\n\n\
         {body}"
    );
    // The last job block ends with a trailing blank line rustfmt strips; trim to
    // exactly one final newline so the scaffold's acceptance.rs is a fmt fixpoint.
    format!("{}\n", out.trim_end_matches('\n'))
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

    /// A cron job (with schedule) of a given name, grafted to exercise the long-name
    /// wrap regimes without touching the frozen fixture.
    fn cron_job(name: &str) -> JobDesign {
        serde_json::from_str(&format!(
            r#"{{ "name": "{name}", "schedule": "0 * * * *" }}"#
        ))
        .unwrap()
    }

    /// Issue #221 (residual F): the cron closure body `Box::pin({name}::{name}(ctx))`
    /// wraps NON-MONOTONICALLY in the name length. `cron_box_pin` must reproduce the
    /// pinned rustfmt's EXACT output at each regime, or a fresh cron registry drifts
    /// under `cargo fmt` (the make-impossible contract behind the no-rustfmt-at-scaffold
    /// design: green means a scaffold survives `cargo fmt` untouched). WHY these exact
    /// bytes: they were measured against `rustfmt --edition 2024` at each length; a naive
    /// monotonic rule regresses the 29–31 and ≥ 37 one-line ranges.
    #[test]
    fn cron_box_pin_reproduces_rustfmt_non_monotonic_wrap() {
        // ≤ 26: one line.
        assert_eq!(
            cron_box_pin("expire_trials"),
            "                    Box::pin(expire_trials::expire_trials(ctx))"
        );
        // 27/28: the INNER call breaks its `ctx` arg.
        assert_eq!(
            cron_box_pin("reconcile_daily_ledger_rows"),
            "                    Box::pin(reconcile_daily_ledger_rows::reconcile_daily_ledger_rows(\n                        ctx,\n                    ))"
        );
        // 29–31: one line AGAIN (the outer `Box::pin(…)` still fits max_width).
        assert_eq!(
            cron_box_pin("expire_abandoned_shopping_cart"),
            "                    Box::pin(expire_abandoned_shopping_cart::expire_abandoned_shopping_cart(ctx))"
        );
        // 32–34: the OUTER `Box::pin(` breaks, the inner call one line.
        assert_eq!(
            cron_box_pin("recompute_search_index_documents"),
            "                    Box::pin(\n                        recompute_search_index_documents::recompute_search_index_documents(ctx),\n                    )"
        );
        // 35–36: BOTH break.
        assert_eq!(
            cron_box_pin("synchronize_external_billing_ledger"),
            "                    Box::pin(\n                        synchronize_external_billing_ledger::synchronize_external_billing_ledger(\n                            ctx,\n                        ),\n                    )"
        );
        // ≥ 37: one line AGAIN (rustfmt gives up — breaking cannot make it fit).
        assert_eq!(
            cron_box_pin("regenerate_monthly_subscription_invoices"),
            "                    Box::pin(regenerate_monthly_subscription_invoices::regenerate_monthly_subscription_invoices(ctx))"
        );
    }

    /// Issue #221 (residual F, task stub): a long cron name overflows the 1-arg stub
    /// signature, which rustfmt breaks one param per line — the #218 fix wrapped the
    /// QUEUE stub but missed the cron stub. A short name stays on one line.
    #[test]
    fn cron_task_stub_signature_wraps_for_a_long_name() {
        let long = task_rs(&cron_job("regenerate_monthly_subscription_invoices"));
        assert!(
            long.contains(
                "pub async fn regenerate_monthly_subscription_invoices(\n    mut _ctx: TaskContext,\n) -> jerrycan::Result<()> {"
            ),
            "long cron stub signature must wrap one param per line: {long}"
        );
        let short = task_rs(&cron_job("expire_trials"));
        assert!(
            short.contains(
                "pub async fn expire_trials(mut _ctx: TaskContext) -> jerrycan::Result<()> {"
            ),
            "short cron stub signature stays on one line: {short}"
        );
    }

    /// Issue #221 (residual F, `.cron(…)`): a long cron name pushes the `.cron("name",
    /// "expr", "queue")` args past `fn_call_width` (60), so rustfmt breaks each arg onto
    /// its own line. A short name stays on one line (byte-identical to pre-#221).
    #[test]
    fn cron_schedule_line_wraps_when_args_exceed_fn_call_width() {
        let mut d = reference();
        d.jobs = vec![cron_job("synchronize_external_billing_ledger")]; // args 61 > 60
        let r = registry_rs(&d);
        assert!(
            r.contains(
                "        .cron(\n            \"synchronize_external_billing_ledger\",\n            \"0 * * * *\",\n            \"default\",\n        )\n"
            ),
            "a long cron name breaks the .cron args one per line: {r}"
        );
        // reference's own short cron names keep the one-line `.cron(...)`.
        let short = registry_rs(&reference());
        assert!(
            short.contains(".cron(\"expire_trials\", \"0 * * * *\", \"billing\")\n"),
            "a short cron name keeps the one-line .cron: {short}"
        );
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
        // The 2-arg signature is wider than 100, so it is pre-wrapped one param per
        // line (issue #218) — exactly as rustfmt would format it.
        assert!(
            stub.contains(
                "pub async fn send_welcome_email(\n    mut _ctx: TaskContext,\n    _payload: SendWelcomeEmailPayload,\n) -> jerrycan::Result<()> {"
            ),
            "queue stub is 2-arg (wrapped): {stub}"
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
        // The assert is pre-wrapped (issue #218) for these job names (the one-line
        // form exceeds width 74), so match the wrapped `res.is_ok(),` arg.
        assert!(
            a.matches("res.is_ok(),").count() == 2,
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

    /// Issue #84: the jobs harness must migrate every route module's tables (the
    /// SAME schema `App::build` applies via `migrations::MIGRATIONS`), not only
    /// `JOBS_MIGRATIONS`. A job that reads/writes a route-module table (the common
    /// case — a job processing app data) otherwise fails `no such table`. WHY
    /// (Rule 9): a job's data access is the whole point; a harness that can't see
    /// the app's tables tests a job against a schema the real app never runs.
    #[test]
    fn acceptance_migrates_route_module_tables_not_only_jobs() {
        let a = acceptance_rs(&reference());
        // The framework jobs tables are still migrated.
        assert!(
            a.contains("db.migrate(jerrycan::jobs::JOBS_MIGRATIONS)"),
            "jobs harness still migrates JOBS_MIGRATIONS: {a}"
        );
        // PLUS every route module's create-tables migration, reached from
        // crates/jobs/tests/acceptance.rs by the `../../routes/<m>` relative path.
        assert!(
            a.contains(
                "include_str!(\"../../routes/leads/migrations/sqlite/0001_create_tables.sql\")"
            ),
            "jobs harness must migrate a route module's tables (issue #84): {a}"
        );
        assert!(
            a.contains("db.migrate(&["),
            "route migrations are applied via a second migrate call: {a}"
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
