# Migrating from Supabase

`jerrycan migrate --from supabase <export-dir>` deterministically translates a
Supabase project into a scaffolded jerrycan app: a contract-v2 `design.json`
(with `storage` and `realtime` blocks), a streamed/resumable data seed, a
machine-readable `gap-report.json`, and a `MIGRATION.md` with a secret-rotation
checklist. It never guesses: anything it can't translate safely (unrecognized
RLS, plpgsql/edge bodies, exotic types) becomes a gap item for you to resolve.

## Produce the export

The offline path (the supported one — never point CI at a live database) reads a
directory you produce with `psql`/`pg_dump`/`supabase`:

```sh
mkdir -p export/data export/storage export/functions
cd export

# 1. Schema (required): public + auth + storage.
supabase db dump --schema public,auth,storage -f schema.sql
# or: pg_dump --schema-only --schema=public --schema=auth --schema=storage "$DB_URL" > schema.sql

# 2. Table data as CSV (one file per table, \N marks NULL).
psql "$DB_URL" -c "\copy (select * from public.<t>) to 'data/public.<t>.csv' with (format csv, header true, null '\N')"
# also dump auth.users and auth.identities the same way (data/auth.users.csv, data/auth.identities.csv)

# 3. Storage bucket config + object bytes.
psql "$DB_URL" -Atc "select coalesce(json_agg(b), '[]'::json) from storage.buckets b" > storage/buckets.json
# object bytes (optional): supabase storage cp -r ss:///<bucket> storage/objects/<bucket>

# 4. Scheduled jobs (pg_cron), as cron.schedule() calls or a jobname/schedule/command dump.
psql "$DB_URL" -c "\copy (select jobname, schedule, command from cron.job) to stdout" > cron.sql
```

## Directory layout

```
export/
  schema.sql                     # required
  data/<schema>.<table>.csv      # \N = NULL, header row present
  storage/
    buckets.json                 # storage.buckets as a JSON array
    objects/<bucket>/<key>       # object bytes (optional)
  functions/<name>/index.ts      # Edge Function sources (become gap items)
  cron.sql                       # pg_cron schedule() calls / job rows
```

## Run the migration

```sh
jerrycan migrate --from supabase export --out ./my-app --name my-app
cd my-app
jerrycan db migrate     # bring the schema up
jerrycan db seed        # apply the streamed seed (resumable — safe to re-run)
jerrycan gen-tests --module <module>
jerrycan check          # green gate, incl. generated cross-tenant isolation tests
```

Then work `gap-report.json` top-down (blocking items first) and follow
`MIGRATION.md` — especially the **secret rotation** checklist. No Supabase
secret (JWT secret, anon key, service-role key) is ever copied into the
generated app; you rotate them and set fresh values.

## What migrates, and what doesn't

- **Tables → entities/modules**, foreign keys → `belongs_to`, enums/`CHECK IN`
  → field `values`, indexes/unique → field flags.
- **RLS policies** → tenancy + guards, but only for a small set of canonical
  shapes (owner = `auth.uid()`, membership-join, storage folder-per-user,
  public read, authenticated). Anything else is a gap — never guessed.
- **auth.users** → a `users` module + JWT auth + a user seed that preserves
  bcrypt hashes (migrated users log in unchanged; jerrycan upgrades the hash to
  argon2 on the next login).
- **storage.buckets** → the `storage` block; **`supabase_realtime`** → the
  `realtime.changes` block; **pg_cron** → `jobs[]`.
- **Not migrated:** the frontend (repoint it with the endpoint map in
  `MIGRATION.md`), plpgsql/trigger/Edge Function bodies, and Realtime
  Broadcast/Presence topics (they live in client code).

## `--live` (opt-in, never in CI)

`jerrycan migrate --from supabase --live <conn>` reads the Postgres catalogs
directly into the same translator. Object bytes are not fetched live — the gap
report and `MIGRATION.md` tell you how to copy them offline.
