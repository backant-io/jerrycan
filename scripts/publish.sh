#!/usr/bin/env bash
# Publish the jerrycan workspace to crates.io in dependency order (at whatever
# version Cargo.toml currently carries).
#
# The crates form a DAG: jerrycan-core and jerrycan-macros have no internal
# deps; jerrycan-db/auth/validate/observe depend on core; the `jerrycan` facade
# depends on ALL the others (hence it publishes LAST). Each crate must be
# indexed on crates.io before the next one resolves it as a `^0.2.0` dependency,
# so this script waits between publishes for the index to update. (This is why a
# pre-publish `cargo publish --dry-run` of a dependent crate cannot fully verify
# while its sibling is still at 0.0.0 — it only resolves once the dep is live.)
#
# Prerequisites: `cargo login` with a publish-scoped token; verified email.
# Run from the repo root. Rate-limit tolerant (waits between new-crate pushes).
set -euo pipefail

# ---------------------------------------------------------------------------
# PRE-PUBLISH EVAL GATE (v2.5) — a release cannot ship if the eval is red.
#
# The Reference live HTTP battery is the permanent v2.5 gate: it scaffolds the Reference
# reference backend, serves it live, and drives every v2 feature over real HTTP
# (tenant isolation, webhook signature rejection, multipart import, API-key
# scopes, both crons under the test clock, OAuth via the mock IdP) plus the
# schema.json data-structure assertions. We run it FIRST, fail-fast, alongside
# the scripted conformance/eval reference apps. If any of these is red the
# script exits before a single `cargo publish` runs — so a broken release is
# impossible to push. (Override only for an emergency republish of crates that
# are already indexed: SKIP_EVAL_GATE=1.)
# ---------------------------------------------------------------------------
if [ "${SKIP_EVAL_GATE:-0}" != "1" ]; then
  echo "=== pre-publish eval gate: reference live battery + scripted conformance/eval ==="
  # --test-threads=1: every scaffolded app emits the SAME shared-target binary
  # path (target/conformance-apps/debug/app); parallel build+serve races serve a
  # stale binary from another test's design (the exact flake heavy.yml
  # serializes away — and which aborted the first 0.4.1 publish attempt here).
  cargo test -p jerrycan --all-features --test reference_eval -- --include-ignored --nocapture --test-threads=1
  cargo test -p jerrycan --all-features --test conformance -- --include-ignored --test-threads=1
  cargo test -p jerrycan --test eval -- --include-ignored --test-threads=1
  # #203: genroute_compile builds real crates from inline designs under strict
  # clippy — it lives ONLY in the manual heavy.yml, so a stale fixture (e.g. one
  # that predates a new lint like JC0558) went red for ~20 releases unnoticed. Run
  # it at publish time so that class of red is caught before a release ships.
  cargo test -p jerrycan --test genroute_compile -- --include-ignored --test-threads=1
  echo "=== eval gate GREEN — proceeding to publish ==="
else
  echo "!!! SKIP_EVAL_GATE=1 — skipping the eval gate (emergency republish only) !!!"
fi

# ---------------------------------------------------------------------------
# PG BEHAVIORAL GATE (#214/#215) — the security+correctness concurrency
# guarantees run release-blocking here.
#
# The oversell primitive (#108), the last-admin transaction guard (#138), and
# the jerrycan-db migrator/reservation races are #[ignore]d (they need a live
# Postgres) and lived ONLY in the manual `heavy.yml`, which is workflow_dispatch
# + nightly — it gates NO release (#215). A regression in the atomic reserve, the
# last-admin race, or the migrate advisory lock could therefore ship green. Run
# them here so a broken concurrency guarantee blocks `cargo publish`.
#
# Each of these tests DROP-IF-EXISTS (or uniquely names) its own tables, so they
# are repeatable against a persistent throwaway test DB — no schema reset needed.
# (The migrate capstone #152 and the conformance PG TDD loop need a FRESH schema
# + the full migrator; they stay in heavy.yml's ephemeral-Postgres lane, not
# here.) Set JERRYCAN_TEST_PG_URL to a throwaway test database. SKIP_PG_GATE=1 is
# the emergency-republish escape (mirrors SKIP_EVAL_GATE).
# ---------------------------------------------------------------------------
if [ "${SKIP_PG_GATE:-0}" != "1" ]; then
  : "${JERRYCAN_TEST_PG_URL:?the PG behavioral gate needs a live Postgres — set JERRYCAN_TEST_PG_URL to a throwaway test DB, or SKIP_PG_GATE=1 to skip (emergency republish of already-indexed crates only)}"
  echo "=== PG behavioral gate: oversell #108 + last-admin #138 + jerrycan-db races ==="
  cargo test -p jerrycan-db --lib -- --ignored --test-threads=1
  cargo test -p jerrycan --all-features --test reserve_capacity_concurrency -- --ignored --test-threads=1
  cargo test -p jerrycan --all-features --test last_admin_concurrency -- --ignored --test-threads=1
  # #227: the realtime CDC change-delivery path (pgoutput WAL decode + slot/
  # publication reconcile; trigger LISTEN/NOTIFY + the tenant-move refetch race)
  # was #[ignore]d behind env vars NO gate set, so a regression could ship green.
  # Both self-manage their schema (uniquely-named table, drop-slot on teardown)
  # so they are repeatable against the throwaway DB. They read JERRYCAN_TEST_PG /
  # JERRYCAN_TEST_PG_LOGICAL (NOT _URL); the local gate container is
  # wal_level=logical, so both point at the same DB. (bus_redis needs Redis →
  # heavy.yml only.)
  echo "=== PG behavioral gate: realtime CDC (triggers + logical replication) ==="
  JERRYCAN_TEST_PG="$JERRYCAN_TEST_PG_URL" \
    cargo test -p jerrycan-realtime --lib \
      changes::triggers::tests::triggers_install_idempotently_and_stream_insert_update_delete \
      -- --ignored --test-threads=1
  JERRYCAN_TEST_PG_LOGICAL="$JERRYCAN_TEST_PG_URL" \
    cargo test -p jerrycan-realtime --lib \
      changes::replication::tests::replication_streams_insert_and_reconcile_is_idempotent \
      -- --ignored --test-threads=1
  echo "=== PG behavioral gate GREEN ==="
else
  echo "!!! SKIP_PG_GATE=1 — skipping the Postgres behavioral gate (emergency republish only) !!!"
fi

# jerrycan-storage and jerrycan-realtime depend on core+db, so they publish
# after those and before the facade. They were added in the 0.3.0 line and were
# missing from this list, so the release pipeline never shipped them (issue #20).
CRATES=(jerrycan-core jerrycan-macros jerrycan-db jerrycan-auth jerrycan-validate jerrycan-observe jerrycan-ratelimit jerrycan-jobs jerrycan-storage jerrycan-realtime jerrycan)

# Publish-completeness guard (issue #20): every crate under crates/ MUST appear
# in CRATES, or it silently never ships — exactly how storage/realtime were
# missed. Fail loudly before any publish if a workspace crate is absent.
for dir in crates/*/; do
  name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)
  case " ${CRATES[*]} " in
    *" $name "*) : ;;
    *) echo "ERROR: workspace crate '$name' ($dir) is not in the publish list — add it in dependency order before the facade."; exit 1 ;;
  esac
done

for c in "${CRATES[@]}"; do
  echo "=== publishing $c ==="
  tries=0
  until out=$(cargo publish -p "$c" 2>&1); do
    echo "$out"
    if echo "$out" | grep -qi "already.*uploaded\|already exists"; then
      echo "SKIP $c (already published)"; break
    fi
    tries=$((tries+1))
    if [ "$tries" -ge 10 ]; then echo "GIVING UP on $c after $tries tries"; exit 1; fi
    echo "retry $c in 60s (rate limit or index lag)…"; sleep 60
  done
  # Let the index update so the next crate resolves this one.
  echo "waiting 30s for crates.io index…"; sleep 30
done
echo "All crates published."
