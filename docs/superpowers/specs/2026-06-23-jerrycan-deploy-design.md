# jerrycan deploy — zero-touch secure deployment (Render reference)

## North star and definition of done

An agent that has just built a jerrycan backend can make it **live, secure, and
production-grade with effectively zero human involvement** by running a single
generated script that needs only a platform API key. The completion of the
"AI designs → builds → checks → **ships**" loop.

**Done when:** `jerrycan deploy render` generates a self-contained, idempotent
deploy script that an agent runs with only `RENDER_API_KEY` (plus a registry the
agent can already push to), and it stands up the app on Render — managed
Postgres, auto-generated secrets in Render's secret store, TLS, health-checked —
and prints a live HTTPS URL. Re-running updates in place. Nothing secret ever
touches the repo or the logs.

## Decisions log

| Decision | Choice | Rationale |
|---|---|---|
| Hosting model | **Bring-your-own-account, agent-run scripts** | jerrycan stays a tool/generator, not a host; user owns + controls + can cancel the infra; no jerrycan infra/cost/liability |
| What jerrycan does | **Generates a self-contained deploy script; the agent runs it** | Matches how an agent drives `aws`/`flyctl`/`wrangler`; auditable; keeps jerrycan a pure generator with no deploy-runtime/heavy deps |
| Reference target | **Render** | Hosts long-running containers (native fit), managed Postgres, free-to-start, and a clean **REST API** → the script is just HTTPS + the key (no CLI to install) |
| Transport | **Pure HTTP (curl + jq)** against Render's REST API | The purest "self-contained script needs only an API key" form |
| Code → platform | **Image-based (2a)**: build the hardened OCI image, push to a registry, deploy from it | Fully scriptable, no web OAuth; the registry is commonly ambient (GHCR via the repo token) |
| Migrations | **App self-migrates on boot** | `Db::migrate` is concurrency-safe (transaction + `pg_advisory_xact_lock`), so multi-instance boot is safe; no separate migration step in the deploy |
| Not in scope (v1) | Railway/Fly/AWS, custom domains, autoscaling tuning, frontend deploy, jerrycan-hosted | YAGNI; the `DeployTarget` abstraction is designed for the other targets but only Render is implemented |

**Explicitly NOT viable (documented):** Vercel and Cloudflare Workers cannot host
a jerrycan backend — they run serverless functions/edge isolates, not a
persistent `hyper` listener with continuous `on_serve` background workers and a
pooled DB connection. In a full app they host the *frontend*; the jerrycan
backend goes to a container host.

## Architecture & boundary

jerrycan remains a **pure generator** — no hosting, no deploy-runtime in the
binary, no new heavy dependencies. `jerrycan deploy <target>` writes a
`deploy/<target>/` directory into the app:

- **`deploy.sh`** — the self-contained, idempotent deploy script (POSIX
  `bash` + `curl` + `jq`).
- **`render.yaml`** — Render's native blueprint (for users who prefer
  repo-connected IaC; informational in v1).
- **`teardown.sh`** — deletes the service + database created by `deploy.sh`.
- **`README.md`** — the one required env var, the registry prerequisite, the
  secrets created and how to rotate them, and how to tear down.

A `DeployTarget` trait abstracts the flow so Railway (GraphQL API), Fly
(Machines REST API), and AWS (SigV4) slot in behind the same steps later. v1
implements only `Render`.

The agent runs `RENDER_API_KEY=… ./deploy/render/deploy.sh` and gets a live URL.

## The deploy flow (`deploy.sh`)

`set -euo pipefail`; every step is **idempotent** via find-or-create keyed on a
deterministic resource name (`<app>` / `<app>-db`); a re-run updates in place.
The script reads `RENDER_API_KEY` (required) and registry config (defaults to
GHCR; overridable via `JERRYCAN_DEPLOY_IMAGE`/`JERRYCAN_DEPLOY_REGISTRY`).

1. **Preflight** — `GET /v1/owners` (validate the key, capture `ownerId`); fail
   fast with a clear message on a bad/unscoped key. Verify `docker` + the
   registry login are available.
2. **Build + push the image** — `docker build` the hardened Dockerfile jerrycan
   already emits → `docker push <registry>/<app>:<tag>` (tag = a fresh content/
   time tag per deploy so Render pulls the new image). A **public** image needs
   no Render registry credential; a private image creates one via
   `POST /v1/registrycredentials`.
3. **Provision managed Postgres** — find-or-create `POST /v1/postgres`
   (`{ownerId, name:"<app>-db", plan, region, version}`); poll
   `GET /v1/postgres/{id}` until `status == available`; read the **internal**
   connection URL from `GET /v1/postgres/{id}/connection-info`.
4. **Generate + set secrets** — generate `JERRYCAN_SECRET`
   (`openssl rand -base64 48`, ≥ 32 bytes); set the service's secret env vars via
   the API: `JERRYCAN_SECRET`, `JERRYCAN_DATABASE_URL` (the PG internal URL),
   `JERRYCAN_ENV=prod`. **Secrets exist only in Render's store** — never in the
   repo, never echoed.
5. **Create/update the web service** — find-or-create `POST /v1/services`
   (`type:"web_service"`, the image, `healthCheckPath:"/healthz"`, plan, region,
   `numInstances`); on update, PATCH env + image.
6. **Deploy + poll to healthy** — trigger a deploy; poll
   `GET /v1/services/{id}/deploys` until the latest is `live` (or report failure +
   the Render logs link; Render keeps the prior live deploy, so a failed deploy
   is no-downtime).
7. **Output** — print the live `https://<app>.onrender.com` URL and a **redacted
   deploy summary** (resources created, where secrets live, rotate + teardown
   instructions). Write `deploy/render/.deploy-state.json` (gitignored) with the
   service + DB **ids only** (no secrets) for idempotent re-runs and teardown.

The deployed app **self-migrates on boot** (concurrency-safe), so step order has
no migration phase.

## Security model (the "production-grade" core)

- **No secret in the repo or logs.** `JERRYCAN_SECRET` and DB creds are generated
  at deploy time, set only in Render's secret store, and redacted from all
  script output. `.deploy-state.json` holds resource ids only and is gitignored.
- **Fail-closed prod.** Sets `JERRYCAN_ENV=prod`, which (per the auth hardening)
  *requires* a real `JERRYCAN_SECRET` — a misconfig can never fall back to the
  world-known dev key.
- **TLS by default** (Render-managed HTTPS); the **hardened container** jerrycan
  already produces (`forbid(unsafe_code)`, minimal base image, SBOM).
- **Least-privilege token.** The README documents the minimal Render API token
  scope required; the script uses nothing broader.
- **Rotation + teardown documented.** The summary explains the
  `JERRYCAN_SECRET` / `JERRYCAN_SECRET_OLD` rotation runbook and how to fully
  remove the deployment.
- **Auditable.** The deploy is a readable script, not an opaque binary action —
  a reviewer can see exactly what it does.

## Components built in jerrycan

- `crates/jerrycan/src/platform/deploy/` (new): the `DeployTarget` trait,
  `render.rs` (the Render target), and the script/blueprint templates
  (`include_str!` templates with deterministic substitution).
- A `jerrycan deploy <target>` CLI subcommand (and the MCP twin if it fits the
  frozen 10-tool contract — otherwise CLI-only).
- Reuses `jerrycan package`'s hardened Dockerfile generation. No runtime HTTP in
  the jerrycan binary — generation is pure templating.

## Idempotency & error handling

- Every Render resource is find-or-created by deterministic name; re-running
  `deploy.sh` reconciles (update env/image, redeploy) rather than duplicating.
- Bad key → fail fast in preflight. Provisioning waits poll with explicit
  timeouts and human-readable progress. A failed deploy reports the failure +
  the logs link and leaves the prior live deploy and the data intact.
- `teardown.sh` deletes the service + DB by the stored ids (with a confirmation
  guard, since it destroys data).

## Testing strategy

Mirrors jerrycan's existing ethos (deterministic golden + `#[ignore]`d live):

- **Golden/determinism tests** — `jerrycan deploy render` output (`deploy.sh`,
  `render.yaml`, `teardown.sh`, `README.md`) is **byte-deterministic** for a
  given app → a golden test in the determinism corpus.
- **`shellcheck`** the generated `deploy.sh` (run if available; vendored
  expectations otherwise).
- **Mock-Render-API test** — the script honors a `RENDER_API_BASE` override; a
  stub HTTP server (or recorded fixtures) exercises the full flow logic
  deterministically in CI, with no real Render and no cost.
- **`#[ignore]`d live deploy test** — with a real `RENDER_API_KEY` + registry,
  deploy a tiny generated app, GET the live URL, then `teardown.sh`. Run
  manually/periodically, like the redis/pg ignored tests.

## Out of scope (v1) / YAGNI

Railway/Fly/AWS targets (abstraction designed, not implemented); custom domains;
autoscaling/instance-count tuning beyond a sane default; blue/green beyond
Render's built-in; **frontend deploy** (jerrycan is the backend — the frontend
goes to Vercel/Pages separately); any jerrycan-hosted option.

## Risks accepted

- **The registry prerequisite** slightly dents "only an API key": image-based
  deploy needs `docker` + a registry the agent can push to (commonly ambient via
  the repo's GHCR token). The Render API key is the only *Render* credential; the
  registry is a secondary, usually-already-present one. (The repo-build path that
  avoids a registry needs a one-time GitHub↔Render OAuth — documented as an
  alternative, not implemented in v1.)
- **Render API drift** — exact endpoint shapes are verified during implementation;
  the flow above is the intended surface.
- **Free-tier shifts** — platform free tiers move; the README states current
  assumptions and the design is target-abstracted so swapping is cheap.

## Future targets (designed-for, not built)

Railway (GraphQL), Fly.io (Machines REST API), AWS App Runner (SigV4) all reuse
the `DeployTarget` flow: preflight → image → managed DB → secrets → service →
deploy/poll → summary. Each is its own follow-up spec/plan once Render proves the
abstraction.
