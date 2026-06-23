#!/usr/bin/env bash
# jerrycan zero-touch deploy → Render, for {{APP_SLUG}}.
# Run:  RENDER_API_KEY=rnd_… ./deploy/render/deploy.sh
# Needs: bash, curl, jq, openssl, and (unless JERRYCAN_DEPLOY_SKIP_BUILD=1) docker
#        + a registry you can push to. Idempotent: re-run to update in place.
set -euo pipefail

APP="{{APP_SLUG}}"
DB="${APP}-db"
API="${RENDER_API_BASE:-https://api.render.com/v1}"     # overridable for tests
STATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${STATE_DIR}/.deploy-state.json"
IMAGE="${JERRYCAN_DEPLOY_IMAGE:-ghcr.io/${JERRYCAN_DEPLOY_REGISTRY_OWNER:-USER}/${APP}}"

: "${RENDER_API_KEY:?set RENDER_API_KEY (a Render API key) — see deploy/render/README.md}"
for bin in curl jq openssl; do command -v "$bin" >/dev/null || { echo "missing: $bin" >&2; exit 1; }; done

# --- helpers -----------------------------------------------------------------
redact() { sed -E 's/(rnd_[A-Za-z0-9]+|JERRYCAN_SECRET=[^ ]+)/***REDACTED***/g'; }
api() { # api METHOD PATH [JSON_BODY]  -> response body on stdout; fails on >=400
  local method="$1" path="$2" body="${3:-}"
  local args=(-sS -X "$method" "${API}${path}" -H "Authorization: Bearer ${RENDER_API_KEY}" -H "Accept: application/json")
  [ -n "$body" ] && args+=(-H "Content-Type: application/json" -d "$body")
  local out code
  out="$(curl "${args[@]}" -w $'\n%{http_code}')"
  code="${out##*$'\n'}"; out="${out%$'\n'*}"
  if [ "$code" -ge 400 ]; then echo "Render API ${method} ${path} -> ${code}: $(echo "$out" | redact)" >&2; return 1; fi
  echo "$out"
}
state_get() { [ -f "$STATE" ] && jq -r --arg k "$1" '.[$k] // empty' "$STATE" || true; }
state_set() { # state_set KEY VALUE  (no secrets — ids only)
  local tmp; tmp="$(mktemp)"
  jq --arg k "$1" --arg v "$2" '. + {($k): $v}' "${STATE:-/dev/null}" 2>/dev/null > "$tmp" \
    || jq -n --arg k "$1" --arg v "$2" '{($k): $v}' > "$tmp"
  mv "$tmp" "$STATE"
}

# --- 1. preflight ------------------------------------------------------------
echo "→ preflight: validating the Render API key"
OWNER_ID="$(api GET /owners | jq -r '.[0].owner.id')"
[ -n "$OWNER_ID" ] && [ "$OWNER_ID" != "null" ] || { echo "no owner for this key" >&2; exit 1; }

# --- 2. build + push the hardened image -------------------------------------
TAG="${JERRYCAN_DEPLOY_TAG:-$(date +%Y%m%d%H%M%S)}"
IMAGE_REF="${IMAGE}:${TAG}"
if [ "${JERRYCAN_DEPLOY_SKIP_BUILD:-0}" = "1" ]; then
  echo "→ build: skipped (JERRYCAN_DEPLOY_SKIP_BUILD=1); using ${IMAGE_REF}"
else
  command -v docker >/dev/null || { echo "missing: docker (or set JERRYCAN_DEPLOY_SKIP_BUILD=1)" >&2; exit 1; }
  echo "→ build: docker build -> ${IMAGE_REF}"
  ( cd "${STATE_DIR}/../.." && docker build -t "${IMAGE_REF}" -f Dockerfile . )
  echo "→ push: ${IMAGE_REF}"
  docker push "${IMAGE_REF}"
fi

# --- 3. managed Postgres (find-or-create) -----------------------------------
echo "→ database: find-or-create ${DB}"
PG_ID="$(state_get pg_id)"
if [ -z "$PG_ID" ]; then
  PG_ID="$(api GET "/postgres?name=${DB}" | jq -r '.[0].postgres.id // empty')"
fi
if [ -z "$PG_ID" ]; then
  PG_ID="$(api POST /postgres "$(jq -n --arg o "$OWNER_ID" --arg n "$DB" \
    '{ownerId:$o, name:$n, plan:"free", region:"oregon", version:"16"}')" | jq -r '.id // .postgres.id')"
fi
state_set pg_id "$PG_ID"
echo "→ database: waiting for ${DB} to be available"
for _ in $(seq 1 60); do
  st="$(api GET "/postgres/${PG_ID}" | jq -r '.status // .postgres.status')"
  [ "$st" = "available" ] && break
  sleep 5
done
DB_URL="$(api GET "/postgres/${PG_ID}/connection-info" | jq -r '.internalConnectionString')"
[ -n "$DB_URL" ] && [ "$DB_URL" != "null" ] || { echo "no DB connection string" >&2; exit 1; }

# --- 4. secrets (generated; never persisted to the repo) --------------------
SECRET="$(openssl rand -base64 48)"
ENV_VARS="$(jq -n --arg s "$SECRET" --arg d "$DB_URL" \
  '[{key:"JERRYCAN_ENV",value:"prod"},{key:"JERRYCAN_SECRET",value:$s},{key:"JERRYCAN_DATABASE_URL",value:$d}]')"

# --- 5. web service (find-or-create) ----------------------------------------
echo "→ service: find-or-create ${APP}"
SVC_ID="$(state_get service_id)"
[ -z "$SVC_ID" ] && SVC_ID="$(api GET "/services?name=${APP}" | jq -r '.[0].service.id // empty')"
SVC_BODY="$(jq -n --arg o "$OWNER_ID" --arg n "$APP" --arg img "$IMAGE_REF" --argjson env "$ENV_VARS" \
  '{type:"web_service", name:$n, ownerId:$o,
    image:{ownerId:$o, imagePath:$img},
    serviceDetails:{env:"image", region:"oregon", plan:"free",
      envSpecificDetails:{healthCheckPath:"/healthz"}},
    envVars:$env}')"
if [ -z "$SVC_ID" ]; then
  SVC_ID="$(api POST /services "$SVC_BODY" | jq -r '.service.id // .id')"
else
  api PATCH "/services/${SVC_ID}" "$SVC_BODY" >/dev/null
  api PUT "/services/${SVC_ID}/env-vars" "$ENV_VARS" >/dev/null
  api POST "/services/${SVC_ID}/deploys" '{"clearCache":"do_not_clear"}' >/dev/null
fi
state_set service_id "$SVC_ID"

# --- 6. deploy + poll to healthy --------------------------------------------
echo "→ deploy: waiting for ${APP} to go live"
for _ in $(seq 1 120); do
  dstat="$(api GET "/services/${SVC_ID}/deploys?limit=1" | jq -r '.[0].deploy.status // empty')"
  case "$dstat" in
    live) break ;;
    build_failed|update_failed|canceled|deactivated)
      echo "deploy failed: ${dstat} — check the Render dashboard logs" >&2; exit 1 ;;
  esac
  sleep 5
done

# --- 7. summary (secrets redacted) ------------------------------------------
URL="$(api GET "/services/${SVC_ID}" | jq -r '.service.serviceDetails.url // .serviceDetails.url // empty')"
[ -n "$URL" ] || URL="https://${APP}.onrender.com"
cat <<EOF

✓ Deployed ${APP} to Render.
  URL:       ${URL}
  Service:   ${SVC_ID}
  Database:  ${PG_ID} (${DB})
  Secrets:   JERRYCAN_SECRET + JERRYCAN_DATABASE_URL live ONLY in Render's secret store (***never printed***).
  Rotate:    set a new JERRYCAN_SECRET in the Render dashboard; keep the old as JERRYCAN_SECRET_OLD (see README).
  Teardown:  ./deploy/render/teardown.sh
EOF
