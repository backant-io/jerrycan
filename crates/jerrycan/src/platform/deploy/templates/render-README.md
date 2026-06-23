# Deploy {{APP_SLUG}} to Render — zero-touch

`deploy.sh` stands this app up on Render via the Render REST API. Re-running it
updates the deployment in place (idempotent). It writes `.deploy-state.json`
(resource ids only — **no secrets**; gitignored).

## Run it
```
RENDER_API_KEY=rnd_xxx ./deploy/render/deploy.sh
```

## What you need
- **`RENDER_API_KEY`** — the only *Render* credential. Create a **least-privilege**
  API key in the Render dashboard (Account Settings → API Keys). The script uses
  only services + postgres scopes.
- **A registry the script can push to** — image-based deploy builds the hardened
  container and pushes it. Defaults to `ghcr.io`; override with
  `JERRYCAN_DEPLOY_IMAGE=registry/owner/name`. `docker` must be logged in to it.
  To skip the build (e.g. you already pushed an image), set
  `JERRYCAN_DEPLOY_SKIP_BUILD=1` and point `JERRYCAN_DEPLOY_IMAGE`/`_TAG` at it.

## Security
- `JERRYCAN_SECRET` and the database URL are generated/captured at deploy time and
  set **only** in Render's secret store — never in this repo, never printed.
- `JERRYCAN_ENV=prod` is set, so the app fails closed if a real secret is missing
  (it can never fall back to the insecure dev key).
- TLS is Render-managed. The container is the hardened, SBOM'd image.

## Rotating `JERRYCAN_SECRET`
1. In the Render dashboard, copy the current `JERRYCAN_SECRET` into a new env var
   `JERRYCAN_SECRET_OLD` (comma-separated for multiple).
2. Set a fresh `JERRYCAN_SECRET`. Redeploy. Existing sessions/tokens keep working
   (decrypted with the retired key) until you drop `JERRYCAN_SECRET_OLD`.

## Tear down (DESTRUCTIVE)
```
RENDER_API_KEY=rnd_xxx ./deploy/render/teardown.sh
```
Deletes the service and the database (all data), then removes `.deploy-state.json`.
