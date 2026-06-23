#!/usr/bin/env bash
# jerrycan zero-touch deploy → Render, for {{APP_SLUG}}.
# Run:  RENDER_API_KEY=rnd_… ./deploy/render/deploy.sh
# Needs: bash, curl, jq, openssl, and (unless JERRYCAN_DEPLOY_SKIP_BUILD=1) docker
#        + a registry you can push to. Idempotent: re-run to update in place.
set -euo pipefail

APP="{{APP_SLUG}}"
DB="${APP}-db"
API="${RENDER_API_BASE:-https://api.render.com}"        # overridable for tests (host root; /v1 is in each path)
STATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${STATE_DIR}/.deploy-state.json"

: "${RENDER_API_KEY:?set RENDER_API_KEY (a Render API key) — see deploy/render/README.md}"
for bin in curl jq openssl; do command -v "$bin" >/dev/null || { echo "missing: $bin" >&2; exit 1; }; done

# --- helpers -----------------------------------------------------------------
# Defense-in-depth scrubber for ALL error output: even non-secret calls echo
# their body through this. Masks the bearer token, any postgres:// URL (DB
# password), long base64-ish blobs (the generated secret), and a JSON
# "value":"…" (the env-var request body carries the secret + DB URL there).
redact() {
  sed -E \
    -e 's/rnd_[A-Za-z0-9]+/***REDACTED***/g' \
    -e 's#postgres(ql)?://[^ "'"'"']+#***REDACTED***#g' \
    -e 's/JERRYCAN_SECRET=[^ ]+/JERRYCAN_SECRET=***REDACTED***/g' \
    -e 's/"value"[[:space:]]*:[[:space:]]*"[^"]*"/"value":"***REDACTED***"/g' \
    -e 's#[A-Za-z0-9+/]{40,}={0,2}#***REDACTED***#g'
}
api() { # api METHOD PATH [JSON_BODY] [REDACT_BODY] -> response body on stdout; fails on >=400
  # REDACT_BODY=1 marks a call whose request body carries secrets (the secret +
  # DB URL); on error its body is NEVER echoed (a 4xx may mirror the request),
  # only "METHOD PATH -> code". Non-secret calls echo the redacted body to help.
  local method="$1" path="$2" body="${3:-}" redact_body="${4:-0}"
  local args=(-sS -X "$method" "${API}${path}" -H "Authorization: Bearer ${RENDER_API_KEY}" -H "Accept: application/json")
  [ -n "$body" ] && args+=(-H "Content-Type: application/json" -d "$body")
  local out code rc
  out="$(curl "${args[@]}" -w $'\n%{http_code}')" || rc=$?
  if [ -n "${rc:-}" ]; then
    echo "Render API ${method} ${path}: network/transport failure (curl exit ${rc})" >&2
    return 1
  fi
  code="${out##*$'\n'}"; out="${out%$'\n'*}"
  if [ "$code" -ge 400 ]; then
    if [ "$redact_body" = "1" ]; then
      echo "Render API ${method} ${path} -> ${code} (response body withheld: it may echo the request, which carries secrets)" >&2
    else
      echo "Render API ${method} ${path} -> ${code}: $(echo "$out" | redact)" >&2
    fi
    return 1
  fi
  echo "$out"
}
state_get() { [ -f "$STATE" ] && jq -r --arg k "$1" '.[$k] // empty' "$STATE" || true; }
state_set() { # state_set KEY VALUE  (no secrets — ids only)
  local tmp; tmp="$(mktemp)"
  if [ -f "$STATE" ]; then
    # A corrupt state file must ABORT, not silently start fresh: a dropped id
    # orphans the already-created service/DB (and breaks teardown).
    jq empty "$STATE" 2>/dev/null || { rm -f "$tmp"; echo "corrupt ${STATE} — refusing to overwrite (it may hold live resource ids); fix or remove it by hand" >&2; exit 1; }
    jq --arg k "$1" --arg v "$2" '. + {($k): $v}' "$STATE" > "$tmp"
  else
    jq -n --arg k "$1" --arg v "$2" '{($k): $v}' > "$tmp"
  fi
  mv "$tmp" "$STATE"
}

# --- 1. preflight ------------------------------------------------------------
echo "→ preflight: validating the Render API key"
OWNERS="$(api GET /v1/owners)"  # transport vs auth failures get distinct messages via api()
OWNER_ID="$(echo "$OWNERS" | jq -r '.[0].owner.id')"
[ -n "$OWNER_ID" ] && [ "$OWNER_ID" != "null" ] || { echo "no owner for this Render API key (the key is valid but owns no workspace)" >&2; exit 1; }

# --- 2. build + push the hardened image -------------------------------------
# IMAGE resolution: explicit JERRYCAN_DEPLOY_IMAGE wins; else build it from a
# registry owner (GHCR by default). With no image and no owner we must NOT push
# to a literal placeholder — fail fast unless the build is skipped.
REGISTRY="${JERRYCAN_DEPLOY_REGISTRY:-ghcr.io}"
REGISTRY_OWNER="${JERRYCAN_DEPLOY_REGISTRY_OWNER:-}"
if [ -n "${JERRYCAN_DEPLOY_IMAGE:-}" ]; then
  IMAGE="$JERRYCAN_DEPLOY_IMAGE"
elif [ -n "$REGISTRY_OWNER" ]; then
  IMAGE="${REGISTRY}/${REGISTRY_OWNER}/${APP}"
else
  IMAGE=""
fi
TAG="${JERRYCAN_DEPLOY_TAG:-$(date +%Y%m%d%H%M%S)}"
if [ "${JERRYCAN_DEPLOY_SKIP_BUILD:-0}" = "1" ]; then
  [ -n "$IMAGE" ] || { echo "set JERRYCAN_DEPLOY_IMAGE=registry/owner/name (or JERRYCAN_DEPLOY_REGISTRY_OWNER) — the image to deploy" >&2; exit 1; }
  IMAGE_REF="${IMAGE}:${TAG}"
  echo "→ build: skipped (JERRYCAN_DEPLOY_SKIP_BUILD=1); using ${IMAGE_REF}"
else
  [ -n "$IMAGE" ] || { echo "set JERRYCAN_DEPLOY_IMAGE=registry/owner/name (or JERRYCAN_DEPLOY_REGISTRY_OWNER) so the build has a push target" >&2; exit 1; }
  IMAGE_REF="${IMAGE}:${TAG}"
  command -v docker >/dev/null || { echo "missing: docker (or set JERRYCAN_DEPLOY_SKIP_BUILD=1)" >&2; exit 1; }
  echo "→ build: docker build -> ${IMAGE_REF}"
  ( cd "${STATE_DIR}/../.." && docker build -t "${IMAGE_REF}" -f Dockerfile . )
  echo "→ push: ${IMAGE_REF}"
  docker push "${IMAGE_REF}"
fi

# --- 2b. registry credential (private images) -------------------------------
# A private image (the common GHCR case) needs a Render registry credential or
# the deploy hangs/fails opaquely. Provide auth via JERRYCAN_DEPLOY_REGISTRY_USER
# + JERRYCAN_DEPLOY_REGISTRY_TOKEN (for ghcr.io these default to the GHCR owner +
# ${GITHUB_TOKEN}). With no auth we deploy as a public image (and say so).
REG_HOST="${IMAGE%%/*}"   # registry host from the image path
case "$REG_HOST" in
  ghcr.io)               REG_TYPE="GITHUB" ;;
  docker.io|index.docker.io|registry-1.docker.io) REG_TYPE="DOCKER" ;;
  *.pkg.dev)             REG_TYPE="GOOGLE_ARTIFACT" ;;
  *.amazonaws.com)       REG_TYPE="AWS_ECR" ;;
  *gitlab*)              REG_TYPE="GITLAB" ;;
  *)                     REG_TYPE="" ;;
esac
REG_USER="${JERRYCAN_DEPLOY_REGISTRY_USER:-}"
REG_TOKEN="${JERRYCAN_DEPLOY_REGISTRY_TOKEN:-}"
if [ "$REG_HOST" = "ghcr.io" ]; then
  # GHCR: default the user to the image owner and the token to ${GITHUB_TOKEN}.
  GHCR_OWNER="${IMAGE#ghcr.io/}"; GHCR_OWNER="${GHCR_OWNER%%/*}"
  REG_USER="${REG_USER:-$GHCR_OWNER}"
  REG_TOKEN="${REG_TOKEN:-${GITHUB_TOKEN:-}}"
fi
REG_CRED_ID=""
if [ -n "$REG_USER" ] && [ -n "$REG_TOKEN" ] && [ -n "$REG_TYPE" ]; then
  CRED_NAME="${APP}-registry"
  echo "→ registry: find-or-create credential ${CRED_NAME} (${REG_TYPE})"
  REG_CRED_ID="$(state_get registry_credential_id)"
  if [ -z "$REG_CRED_ID" ]; then
    REG_CRED_ID="$(api GET "/v1/registrycredentials?name=${CRED_NAME}" | jq -r '.[0].id // empty')"
  fi
  if [ -z "$REG_CRED_ID" ]; then
    # The create body carries the registry token → withhold its error body.
    REG_CRED_ID="$(api POST /v1/registrycredentials "$(jq -n \
      --arg o "$OWNER_ID" --arg n "$CRED_NAME" --arg r "$REG_TYPE" --arg u "$REG_USER" --arg t "$REG_TOKEN" \
      '{ownerId:$o, name:$n, registry:$r, username:$u, authToken:$t}')" "" 1 | jq -r '.id // empty')"
  fi
  [ -n "$REG_CRED_ID" ] || { echo "failed to obtain a registry credential id" >&2; exit 1; }
  state_set registry_credential_id "$REG_CRED_ID"
else
  echo "→ registry: deploying ${IMAGE_REF} as a PUBLIC image (no registry auth provided)."
  echo "  If this image is private, set JERRYCAN_DEPLOY_REGISTRY_USER + JERRYCAN_DEPLOY_REGISTRY_TOKEN"
  echo "  (for ghcr.io: the owner + a GITHUB_TOKEN with read:packages) so Render can pull it."
fi

# --- 3. managed Postgres (find-or-create) -----------------------------------
echo "→ database: find-or-create ${DB}"
PG_ID="$(state_get pg_id)"
if [ -z "$PG_ID" ]; then
  PG_ID="$(api GET "/v1/postgres?name=${DB}" | jq -r '.[0].postgres.id // empty')"
fi
if [ -z "$PG_ID" ]; then
  PG_ID="$(api POST /v1/postgres "$(jq -n --arg o "$OWNER_ID" --arg n "$DB" \
    '{ownerId:$o, name:$n, plan:"free", region:"oregon", version:"16"}')" | jq -r '.id // .postgres.id')"
fi
state_set pg_id "$PG_ID"
echo "→ database: waiting for ${DB} to be available"
for _ in $(seq 1 60); do
  st="$(api GET "/v1/postgres/${PG_ID}" | jq -r '.status // .postgres.status')"
  [ "$st" = "available" ] && break
  sleep 5
done
# The connection-info response carries the DB password in a postgres:// URL →
# withhold its error body.
DB_URL="$(api GET "/v1/postgres/${PG_ID}/connection-info" "" 1 | jq -r '.internalConnectionString')"
[ -n "$DB_URL" ] && [ "$DB_URL" != "null" ] || { echo "no DB connection string" >&2; exit 1; }

# --- 4. secrets (generated; never persisted to the repo) --------------------
SECRET="$(openssl rand -base64 48)"
ENV_VARS="$(jq -n --arg s "$SECRET" --arg d "$DB_URL" \
  '[{key:"JERRYCAN_ENV",value:"prod"},{key:"JERRYCAN_SECRET",value:$s},{key:"JERRYCAN_DATABASE_URL",value:$d}]')"

# --- 5. web service (find-or-create) ----------------------------------------
echo "→ service: find-or-create ${APP}"
SVC_ID="$(state_get service_id)"
[ -z "$SVC_ID" ] && SVC_ID="$(api GET "/v1/services?name=${APP}" | jq -r '.[0].service.id // empty')"
# Build the image object, adding registryCredentialId only when we have one.
IMAGE_OBJ="$(jq -n --arg o "$OWNER_ID" --arg img "$IMAGE_REF" --arg cred "$REG_CRED_ID" \
  '{ownerId:$o, imagePath:$img} + (if $cred == "" then {} else {registryCredentialId:$cred} end)')"
SVC_BODY="$(jq -n --arg o "$OWNER_ID" --arg n "$APP" --argjson image "$IMAGE_OBJ" --argjson env "$ENV_VARS" \
  '{type:"web_service", name:$n, ownerId:$o,
    image:$image,
    serviceDetails:{env:"image", region:"oregon", plan:"free",
      envSpecificDetails:{healthCheckPath:"/healthz"}},
    envVars:$env}')"
# The service body + env-vars body embed JERRYCAN_SECRET + the DB URL → these
# calls withhold their error body (a 4xx validation error may mirror the body).
if [ -z "$SVC_ID" ]; then
  SVC_ID="$(api POST /v1/services "$SVC_BODY" 1 | jq -r '.service.id // .id')"
else
  api PATCH "/v1/services/${SVC_ID}" "$SVC_BODY" 1 >/dev/null
  api PUT "/v1/services/${SVC_ID}/env-vars" "$ENV_VARS" 1 >/dev/null
  api POST "/v1/services/${SVC_ID}/deploys" '{"clearCache":"do_not_clear"}' >/dev/null
fi
state_set service_id "$SVC_ID"

# --- 6. deploy + poll to healthy --------------------------------------------
echo "→ deploy: waiting for ${APP} to go live"
LIVE=0
for _ in $(seq 1 120); do
  dstat="$(api GET "/v1/services/${SVC_ID}/deploys?limit=1" | jq -r '.[0].deploy.status // empty')"
  case "$dstat" in
    live) LIVE=1; break ;;
    build_failed|update_failed|canceled|deactivated)
      echo "deploy failed: ${dstat} — check the Render dashboard logs" >&2; exit 1 ;;
  esac
  sleep 5
done
# Falling out of the loop without reaching "live" is a TIMEOUT, not success.
[ "$LIVE" = "1" ] || { echo "deploy did not reach 'live' within the timeout (last status: ${dstat:-unknown}) — check the Render dashboard logs" >&2; exit 1; }

# --- 7. summary (secrets redacted) ------------------------------------------
URL="$(api GET "/v1/services/${SVC_ID}" | jq -r '.service.serviceDetails.url // .serviceDetails.url // empty')"
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
