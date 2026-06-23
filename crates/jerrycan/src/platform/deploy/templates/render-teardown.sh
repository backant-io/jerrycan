#!/usr/bin/env bash
# Tear down the Render deployment of {{APP_SLUG}} created by deploy.sh.
# DESTRUCTIVE: deletes the web service AND the database (all data). Run:
#   RENDER_API_KEY=rnd_… ./deploy/render/teardown.sh
set -euo pipefail

API="${RENDER_API_BASE:-https://api.render.com/v1}"
STATE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.deploy-state.json"
: "${RENDER_API_KEY:?set RENDER_API_KEY}"
for bin in curl jq; do command -v "$bin" >/dev/null || { echo "missing: $bin" >&2; exit 1; }; done
[ -f "$STATE" ] || { echo "no .deploy-state.json — nothing to tear down" >&2; exit 1; }

SVC_ID="$(jq -r '.service_id // empty' "$STATE")"
PG_ID="$(jq -r '.pg_id // empty' "$STATE")"
echo "This will DESTROY service ${SVC_ID:-none} and database ${PG_ID:-none} (all data)."
read -r -p "Type 'destroy' to confirm: " ans
[ "$ans" = "destroy" ] || { echo "aborted" >&2; exit 1; }

del() { curl -sS -X DELETE "${API}$1" -H "Authorization: Bearer ${RENDER_API_KEY}" -o /dev/null -w '%{http_code}\n'; }
[ -n "$SVC_ID" ] && { echo "→ deleting service ${SVC_ID}"; del "/services/${SVC_ID}"; }
[ -n "$PG_ID" ] && { echo "→ deleting database ${PG_ID}"; del "/postgres/${PG_ID}"; }
rm -f "$STATE"
echo "✓ torn down."
