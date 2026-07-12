# Phase 4 agent eval — protocol

Goal: measure whether jerrycan's **docs + CLI/MCP alone** are sufficient for an
agent that has never seen the framework internals to build the reference backend
apps. The spec's success metric is **≥ 90 %** (≥ 5/5; 4/5 is the floor).

## Isolation rules (what makes this a real eval)

Allowed inputs only:

- The `jerrycan` binary: CLI subcommands, `jerrycan docs <page>`, `jerrycan explain <code>`.
- The design specs in `conformance/eval/specs/*.design.json`.

Forbidden inputs:

- jerrycan's own source (`crates/*/src/**`).
- The conformance reference fixtures (`conformance/fixtures/**`, `conformance/eval/fixtures/**`).
- Any plan / design / spec markdown for the framework itself.

If you want to open framework source to learn an API — **stop** and run
`jerrycan docs <topic>` instead. Learning the API from the docs is the point.

Knowledge of the API comes only from the docs pages (`app`, `modules`,
`extractors`, `dependencies`, `errors`, `middleware`, `testing`, `database`,
`validation`, `auth`, `observability`, `packaging`, `error-codes`) and
`jerrycan explain`.

## Setup (allowed)

```sh
cargo build -p jerrycan --bin jerrycan        # produces target/debug/jerrycan
export JERRYCAN_FRAMEWORK_DEP='jerrycan = { path = "<repo>/crates/jerrycan", default-features = false }'
```

The `JERRYCAN_FRAMEWORK_DEP` env var points scaffolded `Cargo.toml`s at the
local crate (pre-publish).

## Per spec (blog, tasks, shortener, inventory, notes)

1. **Scaffold**
   `jerrycan new <tmpdir>/<spec> --design conformance/eval/specs/<spec>.design.json`
   (with the env dep set).
2. **Read the relevant docs** for the design surface
   (`jerrycan docs modules`, `extractors`, `dependencies`, `errors`, `testing`,
   plus `validation`/`auth`/`database` when the design lists those dependencies).
   Optionally run `jerrycan gen-tests --module <m>` to materialise the
   design's acceptance criteria as runnable tests in your own project (this is a
   tool output, not a forbidden fixture).
3. **Implement every generated handler stub from scratch** using only what the
   docs taught you. The generated `repo.rs` is a complete in-memory store; wire
   handlers to it (`all`/`get`/`insert`/`update`/`remove`) and map a missing id
   to `Error::not_found()` (→ `404 JC0404`). Do **not** copy conformance
   fixtures.
4. **`jerrycan check` until green.** For any diagnostic, try
   `jerrycan explain <code>` and the docs. Log any case where the docs /
   diagnostic did **not** give you enough to fix it.
5. **Run the app and verify a round-trip over raw HTTP.**
   `JERRYCAN_ADDR=127.0.0.1:<port> cargo run -p app`, then `curl` (or a
   `TcpStream`) a create → read → update → delete sequence and confirm the
   status codes and bodies match the design (incl. `404` for unknown ids).
6. **Score** `PASS` if `check` is green **and** the round-trip behaves;
   otherwise `FAIL` with the reason.

## Scoring & loop

- Record a per-spec table (check green / round-trip / result) and the overall
  pass rate `N/5` in `results.md`.
- If pass rate `< 90 %`: root-cause each failure — **docs gap** or **real
  framework bug**. For a docs gap, improve the relevant `docs/ai/*.md` page or
  `jerrycan explain` text (never the eval), note it under "Docs/diagnostics gaps
  surfaced", and re-run that spec. Loop until `≥ 90 %` or the remaining failure
  is a genuine framework limitation (then ticket it in `docs/phase1-backlog.md`
  and record honestly).

## Gates before committing

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features` (docs changes are doc-tested — keep
  them green)

## Hygiene

Run the eval in a throwaway tmpdir (e.g. `/tmp/jc-eval/<spec>`). Kill any app
processes you start and remove the tmpdirs when done.

---

# v2.5 eval target — the Reference slice

The five reference apps above probe CRUD-shaped backends. The **Reference slice** is
the v2 showcase: it exercises every v2 primitive on a single, real backend —
tenancy + JWT/session auth, tenant-scoped CRUD, multipart CSV import, raw-body
webhook signature verification, scoped API keys, OAuth (connect + callback)
against a mock IdP, and two cron jobs.

## Isolation rules (Reference)

Allowed inputs only:

- The `jerrycan` binary: CLI subcommands, `jerrycan docs <page>`,
  `jerrycan explain <code>`, `jerrycan schema --json`.
- The Reference design: `conformance/designs/reference-slice.design.json`.

Forbidden inputs:

- jerrycan's own source (`crates/*/src/**`).
- The Reference **reference handlers** (`conformance/eval/fixtures/reference/**`) — these
  are the answer key; rebuilding them docs-only is the point.
- Any plan / design / spec markdown for the framework itself (including the
  v2.5 eval-gate plan).

## Pass criteria (Reference)

A run passes when, on the scaffolded slice:

1. `jerrycan check` is **green** (build + clippy + tests + lints + schema).
2. The generated acceptance suite is **green**, including the cross-tenant
   isolation tests (`tenant_a_cannot_read_tenant_b_leads` / `…_api_keys`).
3. The app, served **live**, answers the full HTTP battery: register/login
   (JWT session cookie), live cross-tenant isolation (B gets `404` on A's lead
   and it is absent from B's list), webhook signature `200`/`400`, multipart CSV
   import `202` with the rows visible afterward, scoped API keys `200`/`403`/`401`,
   and OAuth connect `302` + callback `200`/`400` against the mock IdP.
4. Both declared crons (`expire_trials`, `overdue_callbacks`) fire under a
   controlled test clock.
5. `schema.json` alone answers the data-structure questions (FK targets +
   `on_delete`, unique/index, enums, enforcement state) — read via
   `jerrycan schema --json`, no source.
6. **Realtime** (contract v2, requires a `wal_level=logical` Postgres):
   1. serve the migrated slice against the eval's logical-replication Postgres;
   2. log in two users in two different workspaces (tenants) over HTTP;
   3. open two WebSocket clients (`?token=`), both `join` `changes:Lead`;
   4. `POST` a lead as tenant A → tenant A's socket receives the `insert` event
      with the row body within 10s;
   5. **negative control** — tenant B's socket receives nothing for it
      (a heartbeat round-trip proves silence); a leak turns the gate red;
   6. broadcast round-trip on `deal_room` within tenant A, cross-tenant silence
      on tenant B; presence `track` on `editors`, a second same-tenant client
      sees the state + join/leave diffs;
   7. repeat steps 4–5 once against a **stock** Postgres (the trigger fallback)
      to prove identical client-visible behavior — only the source differs.
   The generated `crates/realtime/tests/acceptance.rs` encodes the per-app
   subscribe/receive tests and the `cross_tenant_change_never_arrives_*`
   negative control; run them with
   `JERRYCAN_TEST_DATABASE_URL=… cargo test -p realtime -- --ignored`.

## The automated gate

The deterministic `reference_eval` battery
(`crates/jerrycan/tests/reference_eval.rs::reference_slice_live_battery`) encodes the
pass criteria above as a single `#[ignore]`d test that scaffolds the slice,
applies the reference handlers, and runs the whole battery end-to-end. It is the
**un-skippable gate**: CI runs it in the `--include-ignored` heavy step, and
`scripts/publish.sh` runs it as a fail-fast pre-publish block (with a documented
`SKIP_EVAL_GATE=1` emergency escape). A fresh **docs-only LLM rebuild** of the
slice under the isolation rules above is the periodic manual eval, recorded in
`results.md`.
