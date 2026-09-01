//! Spec §cron: `cron.schedule('<name>','<sched>',$$<body>$$)` calls — and the
//! `jobname\tschedule\tcommand` dump docs/ai/19-migrate-supabase.md tells users
//! to produce — → design `jobs[]`. The 5-field schedule check is textually
//! identical to questions.rs (kept in lockstep). The job BODY is agent work — a
//! `pg_function` gap; a non-5-field schedule (`@hourly` etc.) is a `cron_job`
//! gap, never guessed.

use super::gaps::{GapItem, GapKind, Severity};
use super::parse::{self, RawStatement};
use crate::platform::design::JobDesign;
use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, SelectItem, SetExpr, Statement, Value,
};

pub struct JobsOutput {
    pub jobs: Vec<JobDesign>,
    pub gaps: Vec<GapItem>,
}

/// The same 5-field cron-shape predicate questions.rs uses (kept identical).
fn cron_shaped(schedule: &str) -> bool {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    fields.len() == 5
        && fields.iter().all(|f| {
            !f.is_empty()
                && f.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '*' | ',' | '/' | '-'))
        })
}

/// snake_case a cron job name for questions.rs (`nightly-digest` → `nightly_digest`).
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out
}

fn literal_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) => Some(s.clone()),
            Value::DollarQuotedString(d) => Some(d.value.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Extract a `cron.schedule('name','sched',$$body$$)` call's three string args.
fn cron_schedule_args(stmt: &Statement) -> Option<(String, String, String)> {
    let Statement::Query(query) = stmt else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let [SelectItem::UnnamedExpr(Expr::Function(f))] = select.projection.as_slice() else {
        return None;
    };
    let name_parts: Vec<String> = f
        .name
        .0
        .iter()
        .filter_map(|p| p.as_ident().map(|i| i.value.to_lowercase()))
        .collect();
    if name_parts != ["cron", "schedule"] {
        return None;
    }
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    let mut vals = Vec::new();
    for arg in &list.args {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) = arg else {
            return None;
        };
        vals.push(literal_string(e)?);
    }
    match vals.as_slice() {
        [name, sched, body] => Some((name.clone(), sched.clone(), body.clone())),
        _ => None,
    }
}

/// One row of the `\copy (select jobname, schedule, command from cron.job) to
/// stdout` dump docs/ai/19-migrate-supabase.md tells users to produce: exactly
/// three TAB-separated fields. Splitting only on TAB keeps a command that
/// contains spaces intact; COPY TEXT escapes any real tab or newline in the
/// data, so "exactly three fields" is exact, and a NULL arrives as `\N`.
fn dump_row(line: &str) -> Option<[&str; 3]> {
    let line = line.trim_end();
    if line.is_empty() || line.to_ascii_lowercase().contains("cron.schedule(") {
        return None;
    }
    match line.split('\t').collect::<Vec<_>>()[..] {
        [name, sched, command] => Some([name, sched, command]),
        _ => None,
    }
}

/// Turn one recognized job (from either input form) into a `jobs[]` entry plus
/// its body gap, or into a `cron_job` gap when the schedule is not 5-field.
fn emit_job(
    name: &str,
    sched: String,
    body: String,
    line: usize,
    jobs: &mut Vec<JobDesign>,
    gaps: &mut Vec<GapItem>,
) {
    if cron_shaped(&sched) {
        jobs.push(JobDesign {
            name: snake(name),
            schedule: Some(sched),
            queue: None,
        });
        gaps.push(GapItem {
            kind: GapKind::PgFunction,
            source: format!("cron job `{name}`"),
            location: format!("cron.sql:{line}"),
            reason: "the scheduled SQL body is agent work — the generated task fn is a stub".into(),
            original: body,
            suggested: format!(
                "implement crates/jobs task `{}` with this SQL's behavior",
                snake(name)
            ),
            severity: Severity::Advisory,
        });
    } else {
        gaps.push(GapItem {
            kind: GapKind::CronJob,
            source: format!("cron job `{name}`"),
            location: format!("cron.sql:{line}"),
            reason: format!(
                "schedule `{sched}` is not a 5-field cron expression questions.rs accepts"
            ),
            original: sched,
            suggested: "translate to a 5-field cron expression (minute hour day month weekday)"
                .into(),
            severity: Severity::Blocking,
        });
    }
}

pub fn build_jobs(cron_sql: &str) -> JobsOutput {
    let mut jobs = Vec::new();
    let mut gaps = Vec::new();
    // Anything in cron.sql that is not a recognized 3-arg cron.schedule call
    // (2-arg form, INSERT INTO cron.job dumps, unparseable statements) is a
    // scheduled job we could not read — losing it silently would drop the job.
    let unrecognized = |sql: &str, line: usize| GapItem {
        kind: GapKind::CronJob,
        source: "cron.sql statement".into(),
        location: format!("cron.sql:{line}"),
        reason:
            "statement is not a cron.schedule('name','schedule',$$body$$) call the translator reads"
                .into(),
        original: sql.to_string(),
        suggested:
            "re-express as jobs[] entries (name + 5-field schedule) and port the body by hand"
                .into(),
        severity: Severity::Blocking,
    };
    // Pass 1: the tab-separated cron.job dump. A consumed row is blanked (not
    // removed) so the SQL pass below still reports the cron.sql line numbers it
    // always did; with no dump rows it sees the original text unchanged.
    let mut without_rows = String::with_capacity(cron_sql.len());
    let mut saw_dump_row = false;
    for (idx, text) in cron_sql.lines().enumerate() {
        match dump_row(text) {
            Some(fields @ [name, sched, command]) => {
                saw_dump_row = true;
                if fields == ["jobname", "schedule", "command"] {
                    // A header line the user added by hand — not a job.
                } else if fields.iter().any(|f| f.is_empty() || *f == r"\N") {
                    // A NULL/empty jobname, schedule or command is a job we
                    // cannot name, schedule or run — gap it rather than guess.
                    gaps.push(GapItem {
                        kind: GapKind::CronJob,
                        source: "cron.job dump row".into(),
                        location: format!("cron.sql:{}", idx + 1),
                        reason:
                            r"the row has an empty or NULL (`\N`) jobname, schedule or command"
                                .into(),
                        original: text.trim_end().to_string(),
                        suggested:
                            "give the job a name and a 5-field schedule, then port the command by hand"
                                .into(),
                        severity: Severity::Blocking,
                    });
                } else {
                    emit_job(
                        name,
                        sched.to_string(),
                        command.to_string(),
                        idx + 1,
                        &mut jobs,
                        &mut gaps,
                    );
                }
            }
            None => without_rows.push_str(text),
        }
        without_rows.push('\n');
    }
    let cron_sql = if saw_dump_row {
        without_rows.as_str()
    } else {
        cron_sql
    };

    // Pass 2: cron.schedule() calls.
    for raw in parse::split_and_parse(cron_sql) {
        let RawStatement::Parsed { stmt, sql, line } = &raw else {
            let RawStatement::Unparsed { sql, line } = &raw else {
                continue;
            };
            gaps.push(unrecognized(sql, *line));
            continue;
        };
        let Some((name, sched, body)) = cron_schedule_args(stmt) else {
            gaps.push(unrecognized(sql, *line));
            continue;
        };
        emit_job(&name, sched, body, *line, &mut jobs, &mut gaps);
    }
    JobsOutput { jobs, gaps }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRON_SQL: &str = r#"
select cron.schedule('nightly-digest', '0 3 * * *', $$select public.send_digest()$$);
select cron.schedule('hourly-sync', '@hourly', $$select public.sync()$$);
"#;

    #[test]
    fn five_field_cron_rows_become_jobs_with_body_gaps() {
        let out = build_jobs(CRON_SQL);
        assert_eq!(out.jobs.len(), 1);
        assert_eq!(
            out.jobs[0].name, "nightly_digest",
            "snake_cased for questions.rs"
        );
        assert_eq!(out.jobs[0].schedule.as_deref(), Some("0 3 * * *"));
        // The job BODY is agent work — the generated task fn is a stub.
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::PgFunction
            && g.original.contains("send_digest")));
        // @hourly is not the 5-field shape questions.rs accepts → cron_job gap, never guessed.
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::CronJob
            && g.source.contains("hourly-sync")));
    }

    #[test]
    fn the_prescribed_cron_job_dump_yields_the_same_job_as_the_cron_schedule_call() {
        // docs/ai/19-migrate-supabase.md tells users to export pg_cron with
        // `\copy (select jobname, schedule, command from cron.job) to stdout`,
        // which emits TAB-separated rows — following the manual literally must
        // migrate the jobs, not blow up into a blocking cron_job gap.
        let dump = "jobname\tschedule\tcommand\n\
                    nightly-digest\t0 3 * * *\tselect public.send_digest()\n\
                    weekly-report\t0 0 * * 0\tselect public.build_report('weekly', now())\n";
        let out = build_jobs(dump);
        let calls = build_jobs(CRON_SQL);

        assert_eq!(out.jobs.len(), 2, "{:?}", out.gaps);
        // Same job as the cron.schedule() form: name snake_cased, schedule kept.
        assert_eq!(out.jobs[0].name, calls.jobs[0].name);
        assert_eq!(out.jobs[0].schedule, calls.jobs[0].schedule);
        // The header row is not a job, and a command with spaces is not split.
        assert_eq!(out.jobs[1].name, "weekly_report");
        assert_eq!(out.jobs[1].schedule.as_deref(), Some("0 0 * * 0"));
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::PgFunction
            && g.original == "select public.build_report('weekly', now())"));
        assert!(
            !out.gaps
                .iter()
                .any(|g| g.kind == crate::platform::migrate::gaps::GapKind::CronJob),
            "no job was lost: {:?}",
            out.gaps
        );
    }

    #[test]
    fn unrecognized_cron_statements_gap_instead_of_silently_dropping_the_job() {
        // The 2-arg cron.schedule form and raw cron.job INSERT dumps were
        // silently skipped — a scheduled job simply vanished from the app.
        let sql = r#"
select cron.schedule('*/5 * * * *', $$select public.tick()$$);
insert into cron.job (schedule, command) values ('0 4 * * *', 'select public.rotate()');
"#;
        let out = build_jobs(sql);
        assert!(out.jobs.is_empty());
        let cron_gaps: Vec<_> = out
            .gaps
            .iter()
            .filter(|g| g.kind == crate::platform::migrate::gaps::GapKind::CronJob)
            .collect();
        assert_eq!(cron_gaps.len(), 2, "{:?}", out.gaps);
        assert!(
            cron_gaps
                .iter()
                .all(|g| g.severity == crate::platform::migrate::gaps::Severity::Blocking)
        );
    }
}
