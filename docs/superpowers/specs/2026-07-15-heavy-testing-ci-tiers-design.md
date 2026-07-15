# Heavy testing: three-tier CI restructure

**Date:** 2026-07-15
**Status:** Approved design, pre-implementation
**Branch context:** supersedes the approach on `perf/conformance-shared-target`

## Problem

The GitHub Actions `gate` job runs a **"Conformance (heavy)"** step that, for each of
several `#[ignore]`d tests, scaffolds a full application into a temp dir, then
`cargo build` / `cargo run -p app` / `cargo test`s it — compiling the entire
tokio/sqlx/sea-orm/hyper/libsqlite3-sys tree — and drives it over real HTTP
against real Postgres / Redis / MinIO service containers.

This step is **flaky and blocks every PR**. Two distinct problems:

1. **A concrete race (the immediate red).** Commit `e9e2533` pointed every
   scaffolded app at one shared `CARGO_TARGET_DIR` (`target/conformance-apps`) to
   compile the dependency tree once instead of N times. But every scaffolded
   workspace names its runnable crate `app`, so they all emit their final binary
   to the **same path**, `target/conformance-apps/debug/app`. `cargo test` runs
   the heavy tests in parallel; cargo's file lock serializes the *builds* but not
   the window between "build done / lock released" and "`cargo run` execs the
   binary." So one test can exec a `debug/app` another test just overwrote from a
   **different design** → the wrong server boots → runtime `500`s
   (`agent_builds_postgres_backed_api_test_first`,
   `agent_generates_working_crud_service_via_mcp_only`). The `common/mod.rs`
   doc-comment states the flawed assumption aloud: *"each spawned server holds its
   own binary inode."* False across the build→exec gap. Signature confirmed: the
   same code passed as a PR run but failed on the merge-to-main run — a
   timing-dependent race, not a deterministic break.

2. **The class of test is wrong for the per-PR hot path (the real issue).** Even
   with the race fixed, "scaffold → full compile → spawn a real server → poll a
   port for up to 180s → drive real HTTP against real service containers" is
   inherently fragile on a shared CI runner (rebuild cost, disk pressure, port
   binding, service-container readiness) and slow. It will keep finding new ways
   to flake, and a flake there blocks unrelated PRs.

## Goals

- PRs get **fast, deterministic** signal. No external services, no scaffolded-app
  builds, in the per-PR path.
- The heavy end-to-end guarantee (generated apps really build, boot, serve, and
  talk to a real database) is **preserved** — just moved off the hot path and made
  reliable when it does run.
- A **minimal local pre-commit** catches the obvious stuff before a push.

## Non-goals

- Rewriting the heavy tests to run in-process (`tower::oneshot`). Their *value is
  the real-binary, real-server, real-DB end-to-end proof* — that is why they are
  heavy. We keep the method and fix the isolation.
- Changing what the heavy tests assert.

## Design: three tiers

### Tier 1 — Local pre-commit (minimal, on the developer's machine)

A committed hook, installed manually (no auto-installer, no new dependency):

- `scripts/hooks/pre-commit` — a plain shell script that runs:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-features -- -D warnings`
- `scripts/install-hooks.sh` — one-liner a developer runs once:
  `git config core.hooksPath scripts/hooks`. Documented in the README's
  contributing section.

No tests, no scaffolded-app builds, no network. Seconds on a warm cache. Matches
the repo's existing plain-script convention (`scripts/publish.sh`). Pre-commit is a
convenience, **not** an authority — CI (Tier 2) remains the source of truth, so a
developer who hasn't installed the hook is never blocked by its absence.

### Tier 2 — Per-PR CI (`.github/workflows/ci.yml`) — fast + deterministic

Keep the `gate` job, made fully hermetic:

- **Remove** the `postgres`, `redis`, and `minio` service containers and the
  `JERRYCAN_TEST_PG_URL` workflow env.
- **Remove** these steps (they move to Tier 3):
  - "Conformance (heavy: …)"
  - "Durable-store behavioral tests (ignored: real Postgres + Redis)"
  - "Storage S3 behavioral tests (real MinIO …)"
- **Keep** (all hermetic): Format, Clippy, Audit, Deny, Semver checks,
  `cargo test --workspace --all-features` (runs non-ignored tests only — already
  skips every heavy test), Build benches, Build facade `--no-default-features`,
  Docs build.
- `fuzz-build` job unchanged.

Result: no service-container readiness to flake on, no full app builds, materially
faster PRs.

### Tier 3 — Heavy suite (`.github/workflows/heavy.yml`) — manual now, nightly later

A new workflow:

```yaml
on:
  workflow_dispatch:        # manual "Run workflow" button — the mode we start in
  # schedule:               # flip on later for nightly
  #   - cron: '0 6 * * *'
```

One job that owns everything heavy or service-dependent:

- Service containers: `postgres`, `redis`, `minio` (moved verbatim from `ci.yml`,
  including health-checks and the MinIO readiness note).
- Workflow env: `JERRYCAN_TEST_PG_URL`; step env for S3 (`JERRYCAN_TEST_S3`,
  `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`) exactly as today.
- Toolchain / musl / `Swatinem/rust-cache` setup mirrored from `gate`.
- Steps (moved verbatim except for the reliability changes below):
  - Conformance heavy — the 4 binaries with `--include-ignored`.
  - Durable-store behavioral (Postgres + Redis).
  - Storage-S3 behavioral (MinIO).

Because it is `workflow_dispatch`, it never blocks a PR; a flake is legible
("someone ran the heavy suite and it went red") instead of gating unrelated work.
The commented `schedule` block is the one-line switch to nightly when we're ready.

## Harness fix (makes Tier 3 reliable when it runs)

Two targeted changes; both live in the test harness, not the framework.

### 1. Kill the shared-binary race via serialization

Run each heavy conformance binary single-threaded:

```
cargo test -p jerrycan --all-features --test conformance -- --include-ignored --test-threads=1
```

…and likewise for `genroute_compile`, `eval`, `reference_eval`. The dependency
tree still compiles **once** into the shared `target/conformance-apps` (the entire
point of the shared target dir is preserved), but only one scaffolded app builds
and serves at a time, so the `debug/app` output path is never contended → the race
is gone without renaming crates or copying binaries. Wall-clock grows; acceptable
for a manual/nightly suite. Update the `common/mod.rs` doc-comment to state the
real invariant (single-threaded execution), replacing the incorrect "own binary
inode" claim.

If nightly wall-clock later becomes a problem, the follow-up is per-app unique
binary names to restore parallelism — explicitly out of scope now.

### 2. Per-test Postgres isolation

Scope is narrow: only `agent_builds_postgres_backed_api_test_first` uses the shared
`JERRYCAN_TEST_PG_URL`. `reference_eval` already isolates via a **per-test temp
SQLite file** (`sqlite://<tmp>?mode=rwc`) and is left unchanged. In-memory
conformance tests need nothing.

For each Postgres-backed heavy test, isolate by **unique database**:

1. Parse the base `JERRYCAN_TEST_PG_URL`.
2. Connect to it, `CREATE DATABASE jerrycan_test_<unique>` (unique suffix derived
   from the test name / a counter — **not** `rand`/time, so it stays
   reproducible).
3. Rewrite the URL's database path to the fresh name; use that for
   `db migrate --url` and the served app's `JERRYCAN_DATABASE_URL`.
4. `DROP DATABASE` on completion (best-effort in a teardown guard).

This keeps each DB-backed test starting from a clean, migrated schema regardless of
what ran before it, so the suite is correct and repeatable as more Postgres-backed
heavy tests are added. (Unique *database* over unique *schema* to avoid depending
on `search_path`/`options` passthrough through the connection layer.)

## Rollout

1. Add Tier 1 (`scripts/hooks/pre-commit`, `scripts/install-hooks.sh`, README note).
2. Create `heavy.yml` (Tier 3) with the moved service containers + steps and the
   `--test-threads=1` change.
3. Apply the per-test Postgres-isolation change in `conformance.rs`.
4. Strip the heavy steps + service containers from `ci.yml` (Tier 2).
5. Manually dispatch `heavy.yml` once and confirm green before relying on it.

Order matters: land Tier 3 green **before** removing coverage from Tier 2, so there
is never a window where the heavy guarantee is unrun.

## Verification / success criteria

- `ci.yml` on a PR: green, no service containers spun up, wall-clock down vs. today.
- `heavy.yml` via manual dispatch: green, and green on a **re-run** (proves the
  race and the DB-residue flakiness are actually gone, not just re-rolled).
- Local: `scripts/install-hooks.sh` then a commit that fails `fmt`/`clippy` is
  blocked by the hook; a clean commit passes.

## Risks / tradeoffs

- **Slower heavy feedback.** Breaking a heavy test is now caught at dispatch/nightly
  cadence, not per-PR. Accepted: that is the explicit goal. The manual button lets
  anyone run it on demand before a risky merge.
- **Two workflows can drift** (toolchain, service versions). Mitigation: keep the
  toolchain pin (`rust-toolchain.toml`) and service-container blocks identical;
  note the coupling in both files.
- **`CREATE/DROP DATABASE` needs privileges** on the CI Postgres. The service
  container's `postgres` superuser has them; documented in the harness.
