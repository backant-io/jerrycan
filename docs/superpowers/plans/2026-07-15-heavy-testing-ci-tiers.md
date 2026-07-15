# Heavy-testing CI restructure — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the flaky per-PR "Conformance (heavy)" step into three tiers — a minimal local pre-commit, a fast hermetic per-PR CI gate, and a manual (later nightly) heavy suite — and fix the harness so the heavy suite is reliable when it runs.

**Architecture:** Move every heavy/service-dependent test out of `ci.yml`'s `gate` job into a new `workflow_dispatch` workflow `heavy.yml` that carries the Postgres/Redis/MinIO service containers. Kill the shared-`debug/app` binary race by running the heavy conformance binaries single-threaded (`--test-threads=1`), and give the one Postgres-backed conformance test a clean-slate schema reset before it migrates. Add a committed, manually-installed git pre-commit hook.

**Tech Stack:** GitHub Actions YAML, Rust integration tests (`std::process::Command`-style harness), `psql` (postgresql-client), bash hook scripts.

**Design doc:** `docs/superpowers/specs/2026-07-15-heavy-testing-ci-tiers-design.md`

## Global Constraints

- **Toolchain:** pinned to `1.97` via `rust-toolchain.toml` (channel + `rustfmt`,`clippy` + `x86_64-unknown-linux-musl` target). Workflow toolchain refs stay `dtolnay/rust-toolchain@1.97`. Do NOT edit `rust-toolchain.toml`.
- **Commits:** author is the repo's configured git user (Pavel Hegler). NO "Co-Authored-By"/Claude mentions. Plain "what changed" messages.
- **No new Cargo dependencies.** The DB reset uses `psql` via `Command`, not `sqlx`.
- **Parity:** the service-container blocks and toolchain step in `heavy.yml` must be byte-identical to what they were in `ci.yml` (only step-level changes noted here differ), so the two workflows don't drift.
- **Heavy suite is `workflow_dispatch`-only** for now; the nightly `schedule` block stays commented.
- **Rollout ordering (correctness-critical):** `heavy.yml` must be verified green via manual dispatch BEFORE the heavy coverage is removed from `ci.yml` (Task 5). There must be no window where the heavy guarantee is unrun. Because `workflow_dispatch` workflows are only triggerable once the file exists on the default branch, land Tasks 1–3 (all additive) and merge them to `main` first, dispatch + verify (Task 4), then do the `ci.yml` strip (Task 5) as a follow-up.

---

### Task 1: Tier 1 — local pre-commit hook (committed, manually installed)

**Files:**
- Create: `scripts/hooks/pre-commit`
- Create: `scripts/install-hooks.sh`
- Modify: `README.md:198-210` (the `## Development` `<details>` block)

**Interfaces:**
- Produces: `scripts/install-hooks.sh` sets `git config core.hooksPath scripts/hooks`; `scripts/hooks/pre-commit` runs `cargo fmt --all --check` then `cargo clippy --workspace --all-features -- -D warnings`. Nothing else consumes these.

- [ ] **Step 1: Create the hook script**

Create `scripts/hooks/pre-commit`:

```bash
#!/usr/bin/env bash
# jerrycan pre-commit: fast, hermetic checks only. The heavy suites run in CI —
# this hook is a convenience, never the authority. Install once with
# ./scripts/install-hooks.sh
set -euo pipefail

echo "pre-commit: cargo fmt --all --check"
cargo fmt --all --check

echo "pre-commit: cargo clippy --workspace --all-features -- -D warnings"
cargo clippy --workspace --all-features -- -D warnings

echo "pre-commit: OK"
```

- [ ] **Step 2: Create the installer**

Create `scripts/install-hooks.sh`:

```bash
#!/usr/bin/env bash
# Point git at the repo's committed hooks. Run once after cloning.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath scripts/hooks
echo "Installed: git core.hooksPath -> scripts/hooks"
```

- [ ] **Step 3: Make both executable**

Run:
```bash
chmod +x scripts/hooks/pre-commit scripts/install-hooks.sh
```
Expected: no output, exit 0.

- [ ] **Step 4: Document it in the README Development block**

In `README.md`, replace this exact block:

```markdown
```bash
cargo test --workspace --all-features   # CI runs this, every docs example is a doc-test
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo bench                             # criterion benches (routing, extraction)
cargo +nightly fuzz run <target>        # fuzz targets live in fuzz/ (outside the workspace)
```

The project is built docs-first and test-first: documentation examples are the executable specification.
```

with:

```markdown
```bash
./scripts/install-hooks.sh              # one-time: fmt + clippy run on every commit
cargo test --workspace --all-features   # CI runs this, every docs example is a doc-test
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo bench                             # criterion benches (routing, extraction)
cargo +nightly fuzz run <target>        # fuzz targets live in fuzz/ (outside the workspace)
```

The project is built docs-first and test-first: documentation examples are the executable specification. Heavy end-to-end conformance (real builds, real Postgres/Redis/MinIO) runs off the per-PR path via the manual **Heavy suite** workflow (`.github/workflows/heavy.yml`).
```

- [ ] **Step 5: Verify the installer wires the hook**

Run:
```bash
./scripts/install-hooks.sh && git config --get core.hooksPath
```
Expected: prints `Installed: git core.hooksPath -> scripts/hooks` then `scripts/hooks`.

- [ ] **Step 6: Verify the hook passes clean and blocks a formatting error**

First prove it passes on the clean tree:
```bash
bash scripts/hooks/pre-commit ; echo "clean_exit=$?"
```
Expected: prints the three `pre-commit:` lines ending in `OK`, `clean_exit=0`.

Then prove it blocks — introduce a mis-format in a real tracked crate file (rustfmt only checks files that belong to a crate), run the hook, and revert:
```bash
probe="$(git ls-files 'crates/**/*.rs' | head -1)"
printf '\nfn  _hook_probe( ) {}\n' >> "$probe"
bash scripts/hooks/pre-commit ; echo "blocked_exit=$?"
git checkout -- "$probe"
```
Expected: the `cargo fmt --all --check` line reports a diff and `blocked_exit` is non-zero (the `set -e` hook aborts the commit). `git checkout` restores the file.

- [ ] **Step 7: Commit**

```bash
git add scripts/hooks/pre-commit scripts/install-hooks.sh README.md
git commit -m "ci: add committed pre-commit hook (fmt + clippy), installed via scripts/install-hooks.sh"
```

---

### Task 2: Tier 3 harness fix — per-test Postgres schema reset

**Files:**
- Modify: `crates/jerrycan/tests/common/mod.rs:14-18` (fix the shared-target doc-comment)
- Modify: `crates/jerrycan/tests/conformance.rs` (add a helper after `scaffold_golden_db`, ~line 67; call it in `agent_builds_postgres_backed_api_test_first`, ~line 595)

**Interfaces:**
- Produces: `fn reset_pg_public_schema(pg_url: &str)` — module-private helper in `conformance.rs`. Consumed only by `agent_builds_postgres_backed_api_test_first` in the same file.

- [ ] **Step 1: Fix the shared-target doc-comment (state the real invariant)**

The current comment claims the shared target dir is *"Safe under `cargo test`'s parallelism … each spawned server holds its own binary inode"* — the exact false assumption that caused the flake. Replace it with the real invariant (single-threaded execution).

In `crates/jerrycan/tests/common/mod.rs`, find this exact text:

```rust
/// from scratch N times and cost tens of GB. Pointing every app build at ONE dir
/// compiles the deps ONCE and reuses them. Safe under `cargo test`'s parallelism:
/// cargo locks the target dir so builds serialize, each spawned server holds its
/// own binary inode and binds a distinct port, and the tiny per-app crates just
/// rebuild. Lives under the repo's `target/` (cleaned by `cargo clean`) and is
/// reused across runs. Override with `JERRYCAN_TEST_TARGET_DIR`.
```

Replace it with:

```rust
/// from scratch N times and cost tens of GB. Pointing every app build at ONE dir
/// compiles the deps ONCE and reuses them. Every scaffolded app names its runnable
/// crate `app`, so they all emit the SAME final binary path (`.../debug/app`); the
/// heavy suite therefore runs single-threaded (`--test-threads=1`, set in
/// `heavy.yml`) so only one app builds and serves at a time and that shared output
/// path is never contended. Lives under the repo's `target/` (cleaned by `cargo
/// clean`) and is reused across runs. Override with `JERRYCAN_TEST_TARGET_DIR`.
```

- [ ] **Step 2: Add the reset helper**

In `crates/jerrycan/tests/conformance.rs`, find this exact text (end of `scaffold_golden_db`, start of the first test):

```rust
    assert!(st.success());
    app
}

#[test]
#[ignore = "heavy: db-mode golden app must build and pass the full gate"]
fn db_mode_scaffold_passes_jerrycan_check() {
```

Replace it with:

```rust
    assert!(st.success());
    app
}

/// Reset the target Postgres to a clean slate before a DB-backed heavy test
/// migrates into it. The heavy suite runs `--test-threads=1`, so there is never a
/// concurrent user of this database; dropping and recreating `public` BEFORE the
/// run (no teardown to race, unlike DROP DATABASE) fully isolates each
/// Postgres-backed test from whatever ran before. Requires `psql` — `heavy.yml`
/// installs `postgresql-client`.
fn reset_pg_public_schema(pg_url: &str) {
    let st = Command::new("psql")
        .arg(pg_url)
        .args(["-v", "ON_ERROR_STOP=1"])
        .args(["-c", "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;"])
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "psql is required to reset the Postgres schema for the DB-backed \
                 heavy test (install postgresql-client): {e}"
            )
        });
    assert!(
        st.success(),
        "failed to reset public schema on the test database"
    );
}

#[test]
#[ignore = "heavy: db-mode golden app must build and pass the full gate"]
fn db_mode_scaffold_passes_jerrycan_check() {
```

- [ ] **Step 3: Call the reset before migrating**

In the same file, find this exact text inside `agent_builds_postgres_backed_api_test_first`:

```rust
    // Apply migrations to the real Postgres, then serve against it and drive CRUD.
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["db", "migrate", "--url", &pg_url])
```

Replace it with:

```rust
    // Clean slate before migrating: isolate this test from any prior run's tables
    // (see reset_pg_public_schema). Safe because the heavy suite runs
    // single-threaded, so there is never a concurrent user of this database.
    reset_pg_public_schema(&pg_url);

    // Apply migrations to the real Postgres, then serve against it and drive CRUD.
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", common::shared_app_target())
        .args(["db", "migrate", "--url", &pg_url])
```

- [ ] **Step 4: Verify it compiles (no live DB needed)**

Run:
```bash
cargo test -p jerrycan --all-features --test conformance --no-run
```
Expected: compiles cleanly, no warnings about `reset_pg_public_schema` being unused (it is referenced by the test).

- [ ] **Step 5: (Optional) Verify against a local Postgres if Docker is available**

If Docker is present, prove the reset + full loop end-to-end:
```bash
docker run -d --rm --name jc_pg_probe -e POSTGRES_PASSWORD=jerrycan -e POSTGRES_DB=jerrycan_test -p 5432:5432 postgres:16-alpine
# wait for readiness
until docker exec jc_pg_probe pg_isready -U postgres >/dev/null 2>&1; do sleep 1; done
JERRYCAN_TEST_PG_URL=postgres://postgres:jerrycan@localhost:5432/jerrycan_test \
  cargo test -p jerrycan --all-features --test conformance agent_builds_postgres_backed_api_test_first -- --include-ignored --test-threads=1
echo "exit=$?"
docker stop jc_pg_probe
```
Expected: `test result: ok. 1 passed`. Run it **twice** back-to-back (without stopping the container between runs) to prove the reset makes it repeatable — the second run must also pass. If no Docker, rely on Step 3 plus Task 4's dispatch.

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/tests/common/mod.rs crates/jerrycan/tests/conformance.rs
git commit -m "conformance: run heavy suite single-threaded and reset the public schema before the Postgres-backed test (isolated, repeatable)"
```

---

### Task 3: Tier 3 — create `heavy.yml` (moves the heavy/service steps, single-threaded)

**Files:**
- Create: `.github/workflows/heavy.yml`

**Interfaces:**
- Consumes: `reset_pg_public_schema` (Task 2) via the conformance test; the `--test-threads=1` flags on the conformance binaries.
- Produces: a `workflow_dispatch` workflow that owns all service-container + heavy steps.

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/heavy.yml`:

```yaml
name: Heavy suite
# Real-app conformance: scaffold -> full cargo build -> serve -> drive real HTTP
# against real Postgres/Redis/MinIO. Deliberately OFF the per-PR path (that gate is
# fast + hermetic). Run on demand from the Actions "Run workflow" button; flip on
# the nightly schedule below when ready.
on:
  workflow_dispatch:
  # schedule:
  #   - cron: '0 6 * * *'   # nightly 06:00 UTC — uncomment to enable nightly runs

# Least privilege: this workflow only checks out and builds/tests.
permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always
  RUSTDOCFLAGS: -D warnings
  JERRYCAN_TEST_PG_URL: postgres://postgres:jerrycan@localhost:5432/jerrycan_test

jobs:
  heavy:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16-alpine
        env:
          POSTGRES_PASSWORD: jerrycan
          POSTGRES_DB: jerrycan_test
        ports:
          - 5432:5432
        options: >-
          --health-cmd "pg_isready -U postgres"
          --health-interval 5s
          --health-timeout 5s
          --health-retries 10
      redis:
        image: redis:7-alpine
        ports:
          - 6379:6379
        options: >-
          --health-cmd "redis-cli ping"
          --health-interval 5s
          --health-timeout 5s
          --health-retries 10
      # Service containers cannot pass container args, and minio/minio's
      # entrypoint requires `server /data` — edge-cicd is MinIO's CI image
      # whose default command starts the server. Readiness is checked by the
      # explicit wait in the storage-s3 step (the image bundles no health tool).
      minio:
        image: minio/minio:edge-cicd
        env:
          MINIO_ROOT_USER: minioadmin
          MINIO_ROOT_PASSWORD: minioadmin
        ports:
          - 9000:9000
    steps:
      - uses: actions/checkout@v4
      # Toolchain, components, and the musl target all come from
      # rust-toolchain.toml (pinned to 1.97 == the declared MSRV). Keep this
      # version in sync with that file when bumping stable.
      - uses: dtolnay/rust-toolchain@1.97
        with:
          components: rustfmt, clippy
          targets: x86_64-unknown-linux-musl
      - name: Install musl + postgres client
        # musl-tools: static-link the golden app's musl deploy binary.
        # postgresql-client: `psql`, used by the conformance harness to reset the
        # test database's public schema before the Postgres-backed test.
        run: sudo apt-get update && sudo apt-get install -y musl-tools postgresql-client
      - uses: Swatinem/rust-cache@v2
      - name: "Conformance (heavy: generated apps must build, check, serve, and pass the scripted eval)"
        # --test-threads=1: every scaffolded app emits its runnable binary to the
        # SAME shared-target path (target/conformance-apps/debug/app). Running these
        # binaries single-threaded is what keeps two apps from racing on that one
        # output path (the flake this restructure fixes). Deps still compile once.
        run: |
          cargo test -p jerrycan --all-features --test conformance -- --include-ignored --test-threads=1
          cargo test -p jerrycan --test genroute_compile -- --include-ignored --test-threads=1
          cargo test -p jerrycan --test eval -- --include-ignored --test-threads=1
          cargo test -p jerrycan --all-features --test reference_eval -- --include-ignored --test-threads=1
      - name: "Durable-store behavioral tests (ignored: real Postgres + Redis)"
        # The durable JobStore/RateLimitStore impls and the concurrent-migrator
        # guarantee are #[ignore]d (they need a live server). With the postgres +
        # redis services up, run them for real so a regression in the Redis Lua,
        # the Postgres SKIP-LOCKED leasing, the advisory-lock cron leader, or the
        # migrate advisory lock is caught here.
        run: |
          cargo test -p jerrycan-db --lib tests::concurrent_migrators_do_not_race -- --ignored
          cargo test -p jerrycan-jobs --test postgres_store -- --ignored
          cargo test -p jerrycan-jobs --features jobs-redis --test redis_store -- --ignored
          cargo test -p jerrycan-ratelimit --features rate-limit-redis --test redis_store -- --ignored
      - name: "Storage S3 behavioral tests (real MinIO: single-shot, multipart, presign)"
        # s3_minio.rs is env-gated (it silently no-ops without JERRYCAN_TEST_S3),
        # so the env MUST be set here or the suite never runs. The loopback http
        # endpoint is the one plaintext exception the S3 store allows. Credentials
        # are read by S3Store::from_url via the standard AWS_* vars.
        env:
          JERRYCAN_TEST_S3: s3://jerrycan-test?region=us-east-1&endpoint=http://127.0.0.1:9000
          AWS_ACCESS_KEY_ID: minioadmin
          AWS_SECRET_ACCESS_KEY: minioadmin
        run: |
          timeout 60 bash -c 'until curl -sf http://127.0.0.1:9000/minio/health/live; do sleep 1; done'
          cargo test -p jerrycan-storage --features storage-s3 --test s3_minio
```

- [ ] **Step 2: Validate the YAML parses**

Run:
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/heavy.yml')); print('yaml ok')"
```
Expected: `yaml ok`. If `actionlint` is installed, also run `actionlint .github/workflows/heavy.yml` and expect no errors.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/heavy.yml
git commit -m "ci: add manual heavy-suite workflow (conformance + durable-store + storage), single-threaded"
```

---

### Task 4: Verify the heavy suite is green BEFORE stripping the gate

This is a verification gate, not a code change. Do NOT proceed to Task 5 until this passes.

- [ ] **Step 1: Get Tasks 1–3 onto the default branch**

`workflow_dispatch` workflows are only triggerable once the file is on the default branch. Open a PR with the Task 1–3 commits (all additive — they don't remove any gate coverage) and merge it to `main`. The per-PR `ci.yml` gate is unchanged by these commits, so this merge is safe.

- [ ] **Step 2: Dispatch the heavy suite**

Run:
```bash
gh workflow run heavy.yml --ref main
sleep 5
gh run list --workflow=heavy.yml --limit 1
```
Expected: a run is queued/in-progress.

- [ ] **Step 3: Watch it to completion**

Run:
```bash
gh run watch "$(gh run list --workflow=heavy.yml --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status
```
Expected: exits 0 (all steps green). If it fails, read `gh run view <id> --log-failed`, fix, and re-dispatch — do not continue.

- [ ] **Step 4: Re-run to prove non-flakiness**

Dispatch it a SECOND time (Step 2 + Step 3 again). Expected: green again. Two clean back-to-back dispatches are the acceptance signal that the binary race and DB-residue flakiness are gone (the original failure passed on a PR run and only failed on re-run, so one green run is not enough).

---

### Task 5: Tier 2 — strip heavy steps + service containers from `ci.yml`

Only after Task 4 is green twice.

**Files:**
- Modify: `.github/workflows/ci.yml` (remove services block, `JERRYCAN_TEST_PG_URL` env, the musl-tools apt step, and the three heavy steps)

- [ ] **Step 1: Remove the Postgres env line**

In `.github/workflows/ci.yml`, delete this line from the top-level `env:` block:

```yaml
  JERRYCAN_TEST_PG_URL: postgres://postgres:jerrycan@localhost:5432/jerrycan_test
```

(Keep `CARGO_TERM_COLOR` and `RUSTDOCFLAGS`.)

- [ ] **Step 2: Remove the entire `services:` block from the `gate` job**

Delete this whole block (the `services:` key and everything under it — Postgres, Redis, MinIO, including the MinIO comment):

```yaml
    services:
      postgres:
        image: postgres:16-alpine
        env:
          POSTGRES_PASSWORD: jerrycan
          POSTGRES_DB: jerrycan_test
        ports:
          - 5432:5432
        options: >-
          --health-cmd "pg_isready -U postgres"
          --health-interval 5s
          --health-timeout 5s
          --health-retries 10
      redis:
        image: redis:7-alpine
        ports:
          - 6379:6379
        options: >-
          --health-cmd "redis-cli ping"
          --health-interval 5s
          --health-timeout 5s
          --health-retries 10
      # Service containers cannot pass container args, and minio/minio's
      # entrypoint requires `server /data` — edge-cicd is MinIO's CI image
      # whose default command starts the server. Readiness is checked by the
      # explicit wait in the storage-s3 step (the image bundles no health tool).
      minio:
        image: minio/minio:edge-cicd
        env:
          MINIO_ROOT_USER: minioadmin
          MINIO_ROOT_PASSWORD: minioadmin
        ports:
          - 9000:9000
```

So that `runs-on: ubuntu-latest` is directly followed by `    steps:`.

- [ ] **Step 3: Remove the musl-tools apt step**

Delete this step (the gate no longer links musl — that moved to `heavy.yml`; the musl *target* still installs via `rust-toolchain.toml`, which is fine and untouched):

```yaml
      - name: Install musl + container tools
        run: sudo apt-get update && sudo apt-get install -y musl-tools
```

Leave the `dtolnay/rust-toolchain@1.97` step (with its `targets: x86_64-unknown-linux-musl`) and its comment as-is — they mirror `rust-toolchain.toml` and keep the two workflows in sync.

- [ ] **Step 4: Remove the three heavy steps**

Delete these three steps in full:

```yaml
      - name: "Conformance (heavy: generated apps must build, check, serve, and pass the scripted eval)"
        run: |
          cargo test -p jerrycan --all-features --test conformance -- --include-ignored
          cargo test -p jerrycan --test genroute_compile -- --include-ignored
          cargo test -p jerrycan --test eval -- --include-ignored
          cargo test -p jerrycan --all-features --test reference_eval -- --include-ignored
      - name: "Durable-store behavioral tests (ignored: real Postgres + Redis)"
        # The durable JobStore/RateLimitStore impls and the concurrent-migrator
        # guarantee are #[ignore]d (they need a live server). With the postgres +
        # redis services up, run them for real so a regression in the Redis Lua,
        # the Postgres SKIP-LOCKED leasing, the advisory-lock cron leader, or the
        # migrate advisory lock is caught by CI — not just by compilation.
        run: |
          cargo test -p jerrycan-db --lib tests::concurrent_migrators_do_not_race -- --ignored
          cargo test -p jerrycan-jobs --test postgres_store -- --ignored
          cargo test -p jerrycan-jobs --features jobs-redis --test redis_store -- --ignored
          cargo test -p jerrycan-ratelimit --features rate-limit-redis --test redis_store -- --ignored
      - name: "Storage S3 behavioral tests (real MinIO: single-shot, multipart, presign)"
        # s3_minio.rs is env-gated (it silently no-ops without JERRYCAN_TEST_S3),
        # so the env MUST be set here or the suite never runs anywhere. The
        # loopback http endpoint is the one plaintext exception the S3 store
        # allows. Credentials are read by S3Store::from_url via the standard
        # AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY vars.
        env:
          JERRYCAN_TEST_S3: s3://jerrycan-test?region=us-east-1&endpoint=http://127.0.0.1:9000
          AWS_ACCESS_KEY_ID: minioadmin
          AWS_SECRET_ACCESS_KEY: minioadmin
        run: |
          timeout 60 bash -c 'until curl -sf http://127.0.0.1:9000/minio/health/live; do sleep 1; done'
          cargo test -p jerrycan-storage --features storage-s3 --test s3_minio
```

The remaining `gate` steps, in order, must be: Format, Clippy, Audit, Deny, Semver checks, Tests, Build benches, Build facade (no default features), Docs build. The `Docs build` step becomes the last step in the job.

- [ ] **Step 5: Validate YAML + confirm nothing heavy remains**

Run:
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
grep -nE "postgres:|redis:|minio:|include-ignored|s3_minio|JERRYCAN_TEST_PG_URL|musl-tools|--ignored" .github/workflows/ci.yml || echo "clean: no heavy/service references left in ci.yml"
```
Expected: `yaml ok`, then `clean: no heavy/service references left in ci.yml`.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: move heavy conformance + durable-store + storage tests off the per-PR gate into the manual heavy suite"
```

- [ ] **Step 7: Open the PR and confirm the gate is green + faster**

Push the branch, open a PR, and confirm the `gate` job: (a) passes, (b) spins up NO service containers (check the run — no postgres/redis/minio in the job), (c) is materially faster than the pre-change ~8–20 min. Read the run with `gh run view <id>` if needed.

---

## Rollout summary

1. **PR A (additive):** Task 1 + Task 2 + Task 3. Merge to `main` — the per-PR gate is unchanged, so this is safe.
2. **Verify (Task 4):** dispatch `heavy.yml` from `main`, green twice.
3. **PR B (subtractive):** Task 5. Merge once the heavy suite is proven green.

This ordering guarantees the heavy end-to-end guarantee is never unrun.
