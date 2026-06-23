# Packaging & Deployment

`jerrycan package` turns a checked app into deployable artifacts. It runs the
full verification gate first (build + clippy + audit + deny + tests + lints) and
refuses to package a failing app. Nothing is deployed — artifacts land in
`deploy/`, and you push them with your own tooling.

## Targets
- `--binary` — a release binary (static musl when available, host-target
  fallback otherwise), copied to `deploy/<name>`.
- `--docker` — `deploy/Dockerfile`: multi-stage, static musl build, distroless
  non-root runtime, binds `0.0.0.0:8000`.
- `--k8s` — `deploy/k8s.yaml`: Deployment + Service + NetworkPolicy, hardened
  (`runAsNonRoot`, `readOnlyRootFilesystem`, dropped capabilities, resource
  limits, `/healthz` probes).
- `--systemd` — `deploy/<name>.service`: `DynamicUser`, `ProtectSystem=strict`,
  `NoNewPrivileges`, `PrivateTmp`, restart-on-failure.

Every run also emits `deploy/sbom.json` — a CycloneDX 1.5 software bill of
materials from the full dependency graph.

## Example
```text
jerrycan package --binary --docker --k8s --systemd
# → deploy/{<name>, Dockerfile, k8s.yaml, <name>.service, sbom.json}
docker build -f deploy/Dockerfile -t myapp .
kubectl apply -f deploy/k8s.yaml
```

> The emitted `Dockerfile` does an in-container `cargo build`, so it fetches
> `jerrycan` like any dependency — it works once `jerrycan` is published to
> crates.io (or vendored into the build context). Until then, deploy via the
> `--binary` artifact (copy it into a minimal runtime image yourself, or run it
> under the systemd unit).

## Production checklist
- Set `JERRYCAN_SECRET` (>= 32 bytes) and `JERRYCAN_ENV=prod` (the systemd unit
  sets the latter; provide the secret via your secrets manager).
- Set `JERRYCAN_DATABASE_URL` for db-backed apps.
- The container binds `0.0.0.0:8000`; the Service maps port 80 → 8000.

## Deploy (Render)

`jerrycan deploy render` generates a self-contained `deploy/render/` kit — a
pure-HTTP `deploy.sh`, a `teardown.sh`, a `render.yaml` blueprint, a `README.md`,
and the hardened `Dockerfile` the deploy builds from — that an agent runs with
only a `RENDER_API_KEY` to stand the app up on Render: hardened image, managed
Postgres, secrets in Render's store, TLS, and a health-checked service that
prints a live HTTPS URL. The kit emits its own `Dockerfile`, so no separate
`jerrycan package --docker` step is needed.

```text
jerrycan deploy render
# → deploy/render/{deploy.sh, teardown.sh, render.yaml, README.md, Dockerfile}
RENDER_API_KEY=rnd_xxx ./deploy/render/deploy.sh
```

`deploy.sh` drives Render's REST API idempotently (find-or-create), so re-running
it updates the deployment in place. It writes `deploy/render/.deploy-state.json`
(resource ids only — **no secrets**; the command appends it to `.gitignore`). The
app self-migrates on boot, so there is no separate migration step.

### Registry prereq
The deploy is image-based: `deploy.sh` builds the hardened container (from the
kit's own `deploy/render/Dockerfile`) and pushes it, then points Render at the
pushed tag.
- Set `JERRYCAN_DEPLOY_IMAGE=registry/owner/name` (or
  `JERRYCAN_DEPLOY_REGISTRY_OWNER` with the default `ghcr.io`) so the build has a
  push target. Override just the registry host with `JERRYCAN_DEPLOY_REGISTRY=<host>`
  (default `ghcr.io`). `docker` must be logged in to that registry.
- To skip the build (you already pushed an image), set
  `JERRYCAN_DEPLOY_SKIP_BUILD=1` and point `JERRYCAN_DEPLOY_IMAGE`/`_TAG` at it.
- Private images (the common GHCR case) need a Render registry credential:
  `JERRYCAN_DEPLOY_REGISTRY_USER` + `JERRYCAN_DEPLOY_REGISTRY_TOKEN` (for
  `ghcr.io` these default to the image owner + `GITHUB_TOKEN` with
  `read:packages`). Without them the image is deployed as public.

### Security
- `JERRYCAN_SECRET` is generated at deploy time and the database URL is captured
  from Render; both are set **only** in Render's secret store — never written to
  the repo, never printed (the script redacts them from all output).
- `JERRYCAN_ENV=prod` is set, so the app fails closed if a real secret is missing
  (it never falls back to the insecure dev key). TLS is Render-managed.
- Use a least-privilege Render API key (services + postgres scopes).

### Tear down (destructive)
```text
RENDER_API_KEY=rnd_xxx ./deploy/render/teardown.sh
```
Deletes the service and the database (all data), then removes the state file.
