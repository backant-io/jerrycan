//! MIGRATION.md: the human hand-off. Summary, old→new endpoint mapping, the
//! seed apply order, and a secret-rotation checklist (placeholders only — no
//! secret ever lands here; redact::assert_clean gates it).

use super::gaps::{GapItem, Severity};
use crate::platform::design::Design;

pub struct SeedSummary {
    pub tables: usize,
    pub bulk_tables: usize,
    pub rows: usize,
}

pub fn render(
    design: &Design,
    gaps: &[GapItem],
    seed: &SeedSummary,
    providers: &[String],
    endpoint_map: &[(String, String)],
) -> String {
    let mut md = String::new();
    md.push_str("# Migration report\n\n");
    md.push_str(&format!(
        "Migrated Supabase project into the jerrycan app `{}`.\n\n",
        design.name
    ));

    // What migrated.
    let entity_count: usize = design.modules.iter().map(|m| m.entities.len()).sum();
    let bucket_count = design
        .storage
        .as_ref()
        .map(|s| s.buckets.len())
        .unwrap_or(0);
    let realtime_count = design
        .realtime
        .as_ref()
        .map(|r| r.changes.len())
        .unwrap_or(0);
    md.push_str("## What migrated\n\n");
    md.push_str(&format!(
        "- {entity_count} entities across {} modules\n",
        design.modules.len()
    ));
    md.push_str(&format!("- {bucket_count} storage buckets\n"));
    md.push_str(&format!("- {realtime_count} realtime change channels\n"));
    md.push_str(&format!("- {} scheduled jobs\n", design.jobs.len()));
    md.push_str(&format!(
        "- {} seed tables ({} bulk, {} rows)\n\n",
        seed.tables, seed.bulk_tables, seed.rows
    ));

    // Endpoint mapping (old PostgREST path → new jerrycan path).
    md.push_str("## Endpoint mapping\n\n");
    md.push_str("Repoint the frontend from Supabase's PostgREST routes to the generated ones:\n\n");
    md.push_str("| Supabase (PostgREST) | jerrycan |\n|---|---|\n");
    for (old, new) in endpoint_map {
        md.push_str(&format!("| GET {old} | GET {new} |\n"));
    }
    md.push_str(
        "\nStorage objects: `GET /storage/v1/object/<bucket>/<key>` → `GET /<bucket>/{id}`.\n",
    );
    md.push_str("Realtime: `supabase.channel('<table>-db-changes')` → the jerrycan realtime client at `/realtime`.\n\n");

    // Apply the data seed.
    md.push_str("## Apply the data seed\n\n");
    md.push_str(
        "Bring the schema up, then apply the streamed seed (resumable — safe to re-run):\n\n",
    );
    md.push_str("```sh\njerrycan db migrate\njerrycan db seed\n```\n\n");
    md.push_str("Large tables ride as bulk CSV and checkpoint per batch into `seed/.state.json`, so an interrupted `jerrycan db seed` resumes where it stopped.\n\n");

    // Secret rotation.
    md.push_str("## Secret rotation (do this now)\n\n");
    md.push_str(
        "None of your Supabase secrets were copied. Set fresh values and revoke the old ones:\n\n",
    );
    md.push_str("- [ ] `JERRYCAN_SECRET` — generate a new signing secret for the jerrycan app.\n");
    md.push_str("- [ ] Supabase JWT secret — rotate it; tokens minted by the old backend must stop working.\n");
    md.push_str("- [ ] Supabase anon key — revoke.\n");
    md.push_str("- [ ] Supabase service-role key — revoke (it grants full DB access).\n");
    for p in providers {
        let up = p.to_uppercase();
        md.push_str(&format!(
            "- [ ] `JERRYCAN_OAUTH_{up}_CLIENT_ID` / `JERRYCAN_OAUTH_{up}_CLIENT_SECRET=<ROTATE-ME>` — re-issue the {p} OAuth app credentials.\n"
        ));
    }
    md.push_str("- [ ] Storage backend keys (`JERRYCAN_STORAGE` credentials) — set new ones.\n\n");

    // Gap report.
    let blocking = gaps
        .iter()
        .filter(|g| g.severity == Severity::Blocking)
        .count();
    let advisory = gaps.len() - blocking;
    md.push_str("## Gap report\n\n");
    md.push_str(&format!(
        "{blocking} blocking and {advisory} advisory items are in `gap-report.json`. Work the blocking items top-down before `jerrycan check` — the translator never guessed them.\n\n"
    ));

    // What was NOT migrated.
    md.push_str("## What was NOT migrated\n\n");
    md.push_str("- The frontend (repoint it using the endpoint mapping above).\n");
    md.push_str("- plpgsql function/trigger and Edge Function bodies (ported by hand — see the gap report).\n");
    md.push_str(
        "- Realtime Broadcast and Presence topics (they live in client code, not the database).\n",
    );

    md
}
