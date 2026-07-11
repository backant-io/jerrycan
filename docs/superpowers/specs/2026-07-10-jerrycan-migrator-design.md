# jerrycan migrate (Supabase) — Design Spec

**Date:** 2026-07-10
**Status:** Design approved (forks resolved at review 2026-07-10). Third of three: storage → realtime → **migrator**.
**Depends on:** `jerrycan-storage` + `jerrycan-realtime` specs (both crates translate mechanically into `design.json`). Contract v2.
**Part of:** lossless Supabase migration program (see `jerrycan-supabase-migration-roadmap` memory). This is the spine both crates were built to feed.

---

## Goal

`jerrycan migrate --from supabase <export>` turns a Supabase project into a **green, tested jerrycan backend with the same functionality** — schema, RLS→tenancy, auth, storage, realtime, cron — translating **deterministically where it's safe** and handing **judgment work to the agent** via a structured gap report, with jerrycan's isolation tests + negative controls as the correctness backstop.

## Non-goals

- Frontend migration (the agent repoints frontend calls to jerrycan's endpoints/realtime client; the migrator emits a mapping doc, not frontend code).
- Fully-automatic porting of arbitrary plpgsql / edge-function logic (ported with agent judgment).
- Two-way / ongoing sync from Supabase (this is a one-time migration).

## CLI surface

- `jerrycan migrate --from supabase <export-dir>` — **primary, offline.** Reads files only; no live credentials.
- `jerrycan migrate --from supabase --live <conn>` — **opt-in convenience;** connects to a running Supabase Postgres (and storage) directly. Never used in CI.
- Global `--json` (consistent with the rest of the CLI) for machine output.
- Produces, into a scaffolded project: `design.json`, a data seed, `gap-report.json`, and a human-readable `MIGRATION.md`. Then the normal `generate → check → package` loop runs.

## Input contract — the export directory (offline primary)

A documented layout the migrator expects (with docs listing the exact `supabase db dump` / `pg_dump` commands that produce each):

```
export/
├── schema.sql        # public + auth + storage schemas: tables, types, constraints, CREATE POLICY (RLS), functions, triggers
├── data/             # table data (CSV/COPY), incl. auth.users, storage.objects rows
├── storage/          # storage.buckets config + object bytes
├── functions/        # edge functions (Deno TS), if any
└── cron.sql          # cron.job rows (pg_cron)
```

## Deterministic translator (code — Rule 5: deterministic transforms are code)

A staged pipeline emitting a validated contract-v2 `design.json`:

1. **Schema → entities/fields.** Parse `CREATE TABLE`/types/constraints. Type map: text→string, int/bigint→integer, numeric/real/double→float, bool→boolean, timestamp(tz)→datetime, uuid→uuid, json/jsonb→json. FK→`belongs_to`+`on_delete`; unique/index→field flags; CHECK-enum / enum type→`values`. Unmappable (arrays, composite, domains, geometry)→**gap report**.
2. **Module grouping.** Cluster tables by FK graph / name prefix into modules (agent refines).
3. **Tenancy detection.** Infer the tenant entity + membership table from schema + RLS join shape → `tenancy`.
4. **RLS translation (conservative recognizer).** Only *canonical, unambiguous* shapes translate mechanically: `owner = auth.uid()`→`owner`; membership-join→`tenancy`; `storage.foldername(name)[1] = auth.uid()`→`owner_prefix`; public-read; role checks. **Anything not confidently recognized is NOT guessed** — it goes to the gap report. (Backstop below.)
5. **Auth.** `auth.users` → `auth.model` (session/jwt) + roles; detected OAuth providers → `oauth` dep. Users → seed. **Password hashes:** Supabase uses **bcrypt**; jerrycan-auth uses argon2 — see *Required jerrycan-auth enhancement* below.
6. **Storage.** `storage.buckets` → `storage.buckets[]`; bucket RLS → `owner`/`owner_prefix`/tenant; `storage.objects` → seed + byte copy.
7. **Realtime.** Tables in the `supabase_realtime` publication → `realtime.changes`. Broadcast/Presence are client-side (not in the DB) → gap report; agent reconstructs from frontend usage.
8. **Cron.** `cron.job` rows → `jobs` (schedule).
9. **Functions/triggers/edge → gap report** (agent ports to Rust handlers/jobs).
10. **Emit.** `design.json` (validated) + streamed data seed + `gap-report.json` + `MIGRATION.md`.

## Agent-judgment layer

Driven by the structured gap report, the agent: refines module grouping; implements unrecognized RLS as explicit handler guards; ports plpgsql/triggers/RPC and edge functions to handlers/jobs; reconstructs Broadcast/Presence channels; resolves unmapped types. Then runs the normal loop — `gen-tests` → `check` (isolation tests + negative controls **must** pass) → `package`.

## Gap report (structured, machine-readable)

A JSON array of deterministic work-items — not prose — so the agent gets actionable tasks (consistent with `--json` diagnostics):

```json
{ "kind": "rls_policy | pg_function | edge_function | unmapped_type | realtime_channel | broadcast | presence",
  "source": "public.orders policy \"tenant_isolation\"",
  "location": "schema.sql:1423",
  "reason": "predicate references a join we don't auto-translate",
  "original": "<original SQL / code>",
  "suggested": "implement as a Tenant guard on the orders module",
  "severity": "blocking | advisory" }
```

## Data seed — stream, don't load

Written in batches (never whole tables in memory). Very large tables emit a **resumable bulk-COPY** step instead of an inline seed, keeping the reference seed small enough to live in the eval.

## Correctness backstop (why conservative RLS translation is safe)

Every tenant-scoped table gets generated **isolation tests + cross-tenant negative controls** — in REST, storage, *and* realtime. `jerrycan check` goes red if *any* scope (auto-translated or agent-written) is wrong. **We don't trust the translation; we prove it.** A mis-translation can't silently ship.

## Security (Rule 14 — never write secrets)

- The offline default never handles live service keys.
- Secrets present in the export (JWT secret, service-role key, storage keys) are **never copied** into the generated app config. The migrator emits **placeholders + a rotation checklist** in `MIGRATION.md`. Migrated data is scanned so a stray secret in a data column is flagged, not silently embedded.

## Required jerrycan-auth enhancement (surfaced by this design)

For **lossless** auth, migrated users must log in with their **existing** passwords. Supabase stores **bcrypt** hashes; jerrycan-auth verifies **argon2**. So lossless login requires jerrycan-auth to **verify bcrypt** for migrated users (and optionally re-hash to argon2 on next successful login — transparent upgrade). Alternative (not lossless): forced password reset on first login. **Recommend adding bcrypt-verify to jerrycan-auth** — small, self-contained, and it's the difference between "your users keep working" and "everyone must reset." Flagged for the user; scoped into the storage-phase auth work or as its own small task.

## Eval gate (the program capstone)

A checked-in **reference Supabase export** — a realistic multi-tenant SaaS with storage buckets, realtime channels, auth, and cron — becomes a new un-skippable eval: `jerrycan migrate` it → `generate` → `check` must be **green**, with negative controls (cross-tenant read blocked in REST, storage, and realtime). This is the capstone gate for the whole three-crate program.

## Testing strategy

- Unit: type mapping; RLS recognizer per canonical shape (+ that unrecognized shapes are gap-reported, never guessed); gap-report emission; seed batching/resume.
- Integration: migrate the reference export end-to-end → green; secret-redaction; bcrypt-verify login of a migrated user.
- Negative controls across REST/storage/realtime.

## Resolved decisions (review 2026-07-10)

1. **Input contract:** offline export directory (primary) + `--live` opt-in; no service keys in the default path or CI.
2. **Architecture:** deterministic translator (code) + agent-judgment layer; emits `design.json` + seed + structured gap report → normal `generate/check/package`.
3. **RLS fidelity:** conservative recognizer for canonical shapes; unrecognized → gap report (never guessed); isolation tests + negative controls are the backstop.
4. **Gap report:** structured machine-readable JSON work-items.
5. **Data volume:** streamed/batched seed; resumable bulk-COPY for large tables.

## Deferred / known gaps

- Frontend migration (agent-assisted; migrator emits a mapping doc).
- Automatic porting of arbitrary plpgsql/edge logic (agent judgment).
- Supabase image-transform API, Broadcast/Presence auto-discovery from client code.
