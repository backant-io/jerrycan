#!/usr/bin/env bash
# Publish jerrycan v0.2.0 to crates.io in dependency order.
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

CRATES=(jerrycan-core jerrycan-macros jerrycan-db jerrycan-auth jerrycan-validate jerrycan-observe jerrycan-ratelimit jerrycan)

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
echo "All crates published at 0.2.0."
