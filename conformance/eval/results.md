# Phase 4 agent eval — results

- Date: 2026-06-10
- Agent: opus subagent (docs-only; no access to framework source or conformance fixtures)
- jerrycan @ `48f0893`
- Procedure: `conformance/eval/PROTOCOL.md`

## Per-spec results

| Spec      | Modules / shape                                   | `jerrycan check` | HTTP round-trip                          | Result |
|-----------|---------------------------------------------------|:----------------:|------------------------------------------|:------:|
| blog      | posts (CRUD) + comments subroute, authors         | green            | create→read→update→delete + 404s OK      | PASS   |
| tasks     | tasks (CRUD + PUT done-toggle), projects          | green            | create→read→toggle→delete + 404s OK      | PASS   |
| shortener | links (create/list/resolve/delete)                | green            | create→list→resolve→delete + 404s OK     | PASS   |
| inventory | items (CRUD, integer qty), categories             | green            | create→read→update→delete + 404s OK      | PASS   |
| notes     | notes (CRUD) + tags subroute                       | green            | create→read→update→delete + tags + 404s  | PASS   |

**Overall pass rate: 5/5 (100%).** Floor is 4/5; metric target is ≥ 90%.

## How it went

Every one of the five apps was scaffolded with `jerrycan new`, every generated
handler stub was implemented from scratch, `jerrycan check` came back
`all green` on the first run for all five, and each app served a real create →
read → update → delete sequence over raw HTTP (curl) with the expected status
codes (`201`/`200`/`204`) and bodies, plus correct `404 JC0404` for unknown ids.

The whole task was reconstructible from the docs:

- `jerrycan docs app` / `modules` — the generated layout (tool-owned `lib.rs`
  exposing `module()`, agent-owned `handlers`/`model`/`repo`/`deps`).
- `jerrycan docs extractors` — `Path<i64>`, `Json<T>` in handler signatures.
- `jerrycan docs dependencies` — `Dep<Repo>` injection (the repo is provided in
  the generated `lib.rs`).
- `jerrycan docs errors` — `Error::not_found()` → `404 JC0404`, `?` propagation,
  the `{"code","message"}` body shape.
- `jerrycan docs testing` — `TestApp` / `into_test`, used by the
  `jerrycan gen-tests` acceptance suite.

The response types `Json<T>`, `Created<T>`, and `NoContent` were already present
in the generated stub signatures, so the docs' coverage of them was confirmatory
rather than load-bearing. The in-memory `repo.rs` ships complete
(`all`/`get`/`insert`/`update`/`remove`), so handlers are a thin
extract → call → respond mapping exactly as the module/extractor docs describe.

No `jerrycan explain` lookups were needed — there were no diagnostics to explain,
because nothing failed `check`.

## Docs / diagnostics gaps surfaced

None.

No documented API was missing, ambiguous, or wrong for any of the five designs,
and no diagnostic required more than the docs already provide. No `docs/ai/*.md`
edits were required to reach the pass rate.

## Framework bugs

None. No failure was a framework bug; nothing was papered over.

---

# v2.5 eval gate — the Kolli slice

- Date: 2026-06-15
- Procedure: `conformance/eval/PROTOCOL.md` → "v2.5 eval target — the Kolli slice"
- Target: `conformance/designs/kolli-slice.design.json`

## Deterministic battery (the automated gate)

The deterministic Kolli battery
(`crates/jerrycan/tests/kolli_eval.rs::kolli_slice_live_battery`) **PASSES**. It
scaffolds the Kolli reference slice on jerrycan, applies the reference handlers,
gets `jerrycan --json check` green, runs the generated acceptance suite (incl.
the cross-tenant isolation tests `tenant_a_cannot_read_tenant_b_leads` /
`…_api_keys`), serves the app live on a free port (sqlite file DB), and drives
every v2 feature over a real `TcpStream`:

| Check | Driven over | Result |
|---|---|---|
| register (incl. duplicate-email `409`) + login → JWT session cookie | live HTTP | PASS |
| live cross-tenant isolation (B gets `404` on A's lead; absent from B's list) | live HTTP | PASS |
| billing webhook: no-sig `200`, wrong-sig `400`, correct HMAC-SHA256 hex `200` | live HTTP | PASS |
| multipart CSV import (2 rows) → `202`, rows visible in A's list | live HTTP | PASS |
| scoped API keys: with-scope `200`, wrong-scope `403`, unknown key `401` | live HTTP | PASS |
| OAuth connect → `302` with `state`; callback `200` (mock IdP) / `400` (bad code) | live HTTP | PASS |
| both crons (`expire_trials` hourly, `overdue_callbacks` every 5 min) become due | `due_fire` under test clock | PASS |
| `schema.json` Q&A (FK targets + `on_delete`, unique/index, enums, enforcement) | parsed `SchemaContract` | PASS |

This battery is wired as a **permanent, un-skippable gate**: CI runs it in the
`--include-ignored` heavy step (`cargo test -p jerrycan --all-features --test
kolli_eval -- --include-ignored`), and `scripts/publish.sh` runs it as a
fail-fast pre-publish block (with a documented `SKIP_EVAL_GATE=1` emergency
escape). Cold-build time (the SeaORM compile-tax baseline) is host-dependent and
recorded in the CI log — `kolli_eval` prints the from-scratch acceptance-suite
compile, and the kolli conformance test (`kolli_slice_scaffold_passes_check`)
prints `kolli-slice cold build: …`.

## Real-infra verification (Dockerized Postgres 16 + Redis 7)

Beyond CI-green (which is sqlite-only), the v2 estate was verified against real
infrastructure this cycle:

- **jobs Redis store** — 6/6 ignored live tests green (`jobs-redis`).
- **jobs Postgres store** — 2/2 ignored live tests green, run in parallel.
- **concurrent migrator** — an 8-node concurrent-migrator test green.
- **Postgres-backed generated app** — a full TDD/CRUD conformance run on real
  Postgres (SeaORM Postgres dialect, `FOR UPDATE SKIP LOCKED` leasing,
  `pg_advisory_xact_lock` cron leader) green.

A concurrency gap in `Db::migrate` was found via real Postgres and fixed — it now
runs in a single transaction under a `pg_advisory_xact_lock`.

## Docs-only LLM rebuild (periodic manual eval)

The deterministic battery above is the automated gate; a fresh **docs-only LLM
rebuild** of the Kolli slice (CLI + `jerrycan docs`/`explain`/`schema` only — no
framework source, no reference handlers) is the periodic manual eval per the
PROTOCOL. The docs-only history stands at **5/5 (100%)** on the five reference
apps (2026-06-10, above).

### Docs-only schema.json Q&A (2026-06-15)

The v2.5 "answer data-structure questions from `schema.json` alone" pass
criterion was run docs-only against the Kolli slice (agent restricted to the
`jerrycan` binary + `jerrycan schema --json`/`docs`; framework source, the schema
generator, and the reference fixtures off-limits). **Score: 6/6** —
`jerrycan schema --json` alone confidently answered every question (leads FK +
`on_delete:cascade`, `phone` unique+indexed, `status` enum `{new,called,dnc}`,
enforced vs application-enforced relations via the `enforced` flag, the
`workspaces` integer non-null PK, and the absence of the framework `jerrycan_jobs*`
tables from the application contract).

Gaps surfaced (real, fixable, non-blocking — the eval's purpose is to find these):
1. The `enforced` FK flag is in the payload but undocumented at its point of use
   — an agent reading only schema.json could misread `on_delete:cascade` +
   `enforced:false` as a DB-guaranteed cascade (it is handler-enforced).
2. No dedicated doc page specifies the schema.json contract shape (it is
   described only incidentally in `docs modules` → Relations).
3. PK "strategy" (autoincrement/identity vs app-assigned) is not expressed beyond
   `pk:true` + `type:integer` + `nullable:false`.
4. `indexes` lists index *names*, not the covered columns (column is inferable by
   convention only).
