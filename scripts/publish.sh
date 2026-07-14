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
  cargo test -p jerrycan --all-features --test reference_eval -- --include-ignored --nocapture
  cargo test -p jerrycan --all-features --test conformance -- --include-ignored
  cargo test -p jerrycan --test eval -- --include-ignored
  echo "=== eval gate GREEN — proceeding to publish ==="
else
  echo "!!! SKIP_EVAL_GATE=1 — skipping the eval gate (emergency republish only) !!!"
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
