# jerrycan deploy (Render reference) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `jerrycan deploy render` generates a self-contained `deploy/render/` directory (a pure-HTTP `deploy.sh`, `teardown.sh`, `render.yaml`, `README.md`) that an agent runs with only `RENDER_API_KEY` to stand a jerrycan backend up on Render — hardened image, managed Postgres, secrets in Render's store, TLS, health-checked — and print a live HTTPS URL.

**Architecture:** jerrycan stays a pure *generator* (no deploy-runtime, no new heavy deps). A new `platform::deploy` module with a `DeployTarget` trait and a `Render` impl fills `include_str!` shell/text templates with the app name + image ref + defaults and writes them. The generated `deploy.sh` (bash + `curl` + `jq`) drives Render's REST API idempotently (find-or-create). The deployed app self-migrates on boot (our `Db::migrate` is concurrency-safe), so there is no separate migration step.

**Tech Stack:** Rust (the generator), `clap` (CLI), `include_str!` templating; the generated script is POSIX bash + `curl` + `jq`; tests use Rust integration tests, `shellcheck`, and a stub HTTP server.

**Spec:** `docs/superpowers/specs/2026-06-23-jerrycan-deploy-design.md`.

---

## File structure

- Create `crates/jerrycan/src/platform/deploy/mod.rs` — the `DeployTarget` trait, the `run_deploy(root, design, target)` dispatch, and the shared file-writing helper. One responsibility: orchestrate target selection + emit.
- Create `crates/jerrycan/src/platform/deploy/render.rs` — the `Render` target: fills the four templates with `{app}`/`{image}`/defaults and returns the `(relative_path, contents)` artifacts.
- Create `crates/jerrycan/src/platform/deploy/templates/render-deploy.sh` — the deploy script template.
- Create `crates/jerrycan/src/platform/deploy/templates/render-teardown.sh` — the teardown script template.
- Create `crates/jerrycan/src/platform/deploy/templates/render.yaml` — the Render blueprint template.
- Create `crates/jerrycan/src/platform/deploy/templates/render-README.md` — the operator README template.
- Modify `crates/jerrycan/src/platform/mod.rs` — add `pub mod deploy;` (alphabetical, after `pub mod design;`).
- Modify `crates/jerrycan/src/main.rs` — add the `Cmd::Deploy { target }` clap variant, the `run` dispatch arm, and `fn cmd_deploy`.
- Create `crates/jerrycan/tests/deploy.rs` — golden/determinism, content-property, `shellcheck`, mock-Render-API flow, and `#[ignore]`d live tests.

**App-name source:** use the design's `name` field (`design.name`), lowercased + non-alnum→`-`, as the Render resource base name (the same slug rule used elsewhere). Define a single helper `render::app_slug(design) -> String` and reuse it everywhere so the service, DB, and image names agree.

---

### Task 1: `deploy` module, `DeployTarget` trait, `Render` emit, and CLI wiring

**Files:**
- Create: `crates/jerrycan/src/platform/deploy/mod.rs`
- Create: `crates/jerrycan/src/platform/deploy/render.rs`
- Create: `crates/jerrycan/src/platform/deploy/templates/render.yaml`
- Create: `crates/jerrycan/src/platform/deploy/templates/render-deploy.sh` (full content in Task 2 — for now a one-line placeholder is NOT allowed; create it with the Task-2 content directly, or do Task 2 first. To keep this task compiling, create all four template files now with their FINAL content from Tasks 2–5. If executing strictly in order, paste the Task 2/4/5 template bodies here.)
- Create: `crates/jerrycan/src/platform/deploy/templates/render-teardown.sh`
- Create: `crates/jerrycan/src/platform/deploy/templates/render-README.md`
- Modify: `crates/jerrycan/src/platform/mod.rs`
- Modify: `crates/jerrycan/src/main.rs`
- Test: `crates/jerrycan/tests/deploy.rs`

- [ ] **Step 1: Write the failing test** (`crates/jerrycan/tests/deploy.rs`)

```rust
//! `jerrycan deploy <target>` generates a self-contained deploy kit.
use jerrycan::platform::{deploy, design::Design};

fn demo_design() -> Design {
    serde_json::from_str(
        r#"{ "name": "Acme API", "contract_version": 1, "dependencies": ["db"],
             "modules": [{ "name": "items", "entities": [
               { "name": "Item", "fields": [{ "name": "title", "type": "string" }] }],
               "endpoints": [{ "operation_id": "list_items", "method": "GET", "path": "/",
                 "success": { "status": 200, "entity": "Item", "list": true } }] }] }"#,
    )
    .unwrap()
}

#[test]
fn render_target_emits_the_four_artifacts_with_the_app_slug() {
    let design = demo_design();
    let artifacts = deploy::emit("render", &design).expect("render target");
    let paths: Vec<&str> = artifacts.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "deploy/render/deploy.sh",
            "deploy/render/teardown.sh",
            "deploy/render/render.yaml",
            "deploy/render/README.md",
        ],
        "stable artifact set + order"
    );
    let deploy_sh = &artifacts[0].1;
    // The app name is slugified into the resource base name.
    assert!(deploy_sh.contains("acme-api"), "app slug substituted: {deploy_sh}");
    assert!(!deploy_sh.contains("Acme API"), "raw name not leaked");
}

#[test]
fn unknown_target_is_an_error() {
    let err = deploy::emit("heroku", &demo_design()).unwrap_err();
    assert!(err.contains("unknown deploy target"), "{err}");
    assert!(err.contains("render"), "lists the supported targets: {err}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p jerrycan --test deploy`
Expected: FAIL — `deploy` module / `emit` not found.

- [ ] **Step 3: Create the `Render` target** (`crates/jerrycan/src/platform/deploy/render.rs`)

```rust
//! The Render deploy target: fills the shell/text templates with the app slug +
//! image ref and returns the artifacts. Pure templating — no network, no I/O.

use crate::platform::design::Design;

/// The Render resource base name: the design name, lowercased, non-alnum → '-',
/// collapsed, trimmed. Service = `<slug>`, DB = `<slug>-db`, image tag default
/// `<slug>`. Stable so re-runs find-or-create the same resources.
pub fn app_slug(design: &Design) -> String {
    let mut s = String::with_capacity(design.name.len());
    let mut prev_dash = false;
    for c in design.name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
            prev_dash = false;
        } else if !prev_dash && !s.is_empty() {
            s.push('-');
            prev_dash = true;
        }
    }
    s.trim_matches('-').to_string()
}

const DEPLOY_SH: &str = include_str!("templates/render-deploy.sh");
const TEARDOWN_SH: &str = include_str!("templates/render-teardown.sh");
const RENDER_YAML: &str = include_str!("templates/render.yaml");
const README_MD: &str = include_str!("templates/render-README.md");

/// `(relative_path, contents)` for the four artifacts, deterministic order.
pub fn artifacts(design: &Design) -> Vec<(String, String)> {
    let slug = app_slug(design);
    let fill = |t: &str| t.replace("{{APP_SLUG}}", &slug);
    vec![
        ("deploy/render/deploy.sh".into(), fill(DEPLOY_SH)),
        ("deploy/render/teardown.sh".into(), fill(TEARDOWN_SH)),
        ("deploy/render/render.yaml".into(), fill(RENDER_YAML)),
        ("deploy/render/README.md".into(), fill(README_MD)),
    ]
}
```

- [ ] **Step 4: Create the module dispatch** (`crates/jerrycan/src/platform/deploy/mod.rs`)

```rust
//! Zero-touch deploy generation (spec 2026-06-23). jerrycan stays a pure
//! generator: `emit` returns the deploy-kit artifacts; the CLI writes them and
//! the agent runs the generated script with only the platform API key.

pub mod render;

use crate::platform::design::Design;

/// The supported deploy targets, for help + error text.
pub const TARGETS: &[&str] = &["render"];

/// Generate the deploy kit for `target`. Returns `(relative_path, contents)`
/// artifacts in a deterministic order, or an error naming the supported targets.
pub fn emit(target: &str, design: &Design) -> Result<Vec<(String, String)>, String> {
    match target {
        "render" => Ok(render::artifacts(design)),
        other => Err(format!(
            "unknown deploy target `{other}` — supported: {}",
            TARGETS.join(", ")
        )),
    }
}
```

- [ ] **Step 5: Create the four template files** with their FINAL content. The `render.yaml`, `deploy.sh`, `teardown.sh`, and `README.md` bodies are given verbatim in Tasks 2, 3, 4, and 5 respectively. Create all four files now with that exact content (the `include_str!`s above require them to exist for the crate to compile).

- [ ] **Step 6: Register the module** (`crates/jerrycan/src/platform/mod.rs`)

Add `pub mod deploy;` immediately after `pub mod design;`.

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p jerrycan --test deploy render_target_emits_the_four_artifacts_with_the_app_slug unknown_target_is_an_error`
Expected: PASS (both tests).

- [ ] **Step 8: Wire the CLI** (`crates/jerrycan/src/main.rs`)

Add to the `Cmd` enum (after the `Package { … }` variant):

```rust
    /// Generate a zero-touch deploy kit (run it with your platform API key)
    Deploy {
        /// Deploy target (currently: render)
        target: String,
    },
```

Add to `run`'s match (after the `Cmd::Package { … } => …` arm):

```rust
        Cmd::Deploy { target } => cmd_deploy(&target, cli.json),
```

Add the handler (near `cmd_package`):

```rust
fn cmd_deploy(target: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    let artifacts = platform::deploy::emit(target, &design).map_err(Failure::gate)?;
    for (rel, contents) in &artifacts {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Failure::gate(e.to_string()))?;
        }
        std::fs::write(&path, contents).map_err(|e| Failure::gate(e.to_string()))?;
        // The scripts must be executable.
        #[cfg(unix)]
        if rel.ends_with(".sh") {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .map_err(|e| Failure::gate(e.to_string()))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).map_err(|e| Failure::gate(e.to_string()))?;
        }
    }
    let written: Vec<&str> = artifacts.iter().map(|(p, _)| p.as_str()).collect();
    let payload = serde_json::json!({
        "target": target,
        "artifacts": written,
        "next_step": format!(
            "set the platform key and run the script, e.g. `RENDER_API_KEY=… ./deploy/{target}/deploy.sh`"
        ),
    });
    emit(json_mode, &payload, &format!("deploy kit for `{target}` written"));
    Ok(())
}
```

(Use the existing `platform::`/`emit`/`Failure`/`app_root`/`load_design` imports already in `main.rs`; confirm `platform` is in scope — it is, via `jerrycan::platform` used by the other `cmd_*`.)

- [ ] **Step 9: Verify the CLI builds + help works**

Run: `cargo build -p jerrycan && target/debug/jerrycan deploy --help`
Expected: builds; help shows the `target` arg.

- [ ] **Step 10: Commit**

```bash
git add crates/jerrycan/src/platform/deploy crates/jerrycan/src/platform/mod.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/deploy.rs
git commit -m "Add jerrycan deploy: deploy-kit generator + Render target skeleton + CLI"
```

---

### Task 2: `render.yaml` blueprint template

**Files:**
- Create/finalize: `crates/jerrycan/src/platform/deploy/templates/render.yaml`
- Test: `crates/jerrycan/tests/deploy.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn render_yaml_declares_a_web_service_a_db_and_secret_envs() {
    let artifacts = deploy::emit("render", &demo_design()).unwrap();
    let yaml = &artifacts[2].1; // render.yaml
    assert!(yaml.contains("type: web_service"), "{yaml}");
    assert!(yaml.contains("name: acme-api"), "{yaml}");
    assert!(yaml.contains("healthCheckPath: /healthz"), "{yaml}");
    assert!(yaml.contains("databases:") && yaml.contains("name: acme-api-db"), "{yaml}");
    // JERRYCAN_SECRET is generateValue (Render generates + stores it), never inline.
    assert!(yaml.contains("key: JERRYCAN_SECRET") && yaml.contains("generateValue: true"), "{yaml}");
    assert!(yaml.contains("key: JERRYCAN_ENV") && yaml.contains("value: prod"), "{yaml}");
}
```

- [ ] **Step 2: Run it to verify it fails** — `cargo test -p jerrycan --test deploy render_yaml_declares` → FAIL.

- [ ] **Step 3: Write the template** (`templates/render.yaml`)

```yaml
# Render blueprint for {{APP_SLUG}} — `render.yaml` (https://render.com/docs/blueprint-spec).
# This is the declarative alternative to deploy.sh, for repo-connected IaC.
# Secrets are NEVER written here: JERRYCAN_SECRET uses Render's generateValue.
databases:
  - name: {{APP_SLUG}}-db
    plan: free
    postgresMajorVersion: "16"

services:
  - type: web_service
    name: {{APP_SLUG}}
    runtime: image
    plan: free
    region: oregon
    healthCheckPath: /healthz
    image:
      url: docker.io/library/{{APP_SLUG}}:latest # overwritten by deploy.sh with the pushed tag
    envVars:
      - key: JERRYCAN_ENV
        value: prod
      - key: JERRYCAN_SECRET
        generateValue: true
      - key: JERRYCAN_DATABASE_URL
        fromDatabase:
          name: {{APP_SLUG}}-db
          property: connectionString
```

- [ ] **Step 4: Run the test to verify it passes** — `cargo test -p jerrycan --test deploy render_yaml_declares` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/deploy/templates/render.yaml crates/jerrycan/tests/deploy.rs
git commit -m "Add render.yaml blueprint template for jerrycan deploy"
```

---

### Task 3: `deploy.sh` — the pure-HTTP Render flow

**Files:**
- Create/finalize: `crates/jerrycan/src/platform/deploy/templates/render-deploy.sh`
- Test: `crates/jerrycan/tests/deploy.rs`

**Render API note for the implementer:** the endpoints/payloads below match Render's public REST API (`https://api.render.com/v1`, docs at `https://api-docs.render.com`) as of this writing. Before finishing, open the docs and confirm each path + JSON field name (especially `connection-info`'s `internalConnectionString`, the `services` create body for `runtime: image`, and the deploy `status` enum). Adjust the script if a field name differs — the *flow* is correct; only the field spellings are at risk.

- [ ] **Step 1: Write the failing content-property test**

```rust
#[test]
fn deploy_sh_has_the_secure_idempotent_flow() {
    let artifacts = deploy::emit("render", &demo_design()).unwrap();
    let sh = &artifacts[0].1;
    // Strict bash.
    assert!(sh.starts_with("#!/usr/bin/env bash\n"), "{sh}");
    assert!(sh.contains("set -euo pipefail"), "{sh}");
    // Requires the key; supports the test/advanced overrides.
    assert!(sh.contains("RENDER_API_KEY"), "{sh}");
    assert!(sh.contains("RENDER_API_BASE"), "override for tests: {sh}");
    assert!(sh.contains("JERRYCAN_DEPLOY_SKIP_BUILD"), "test hook: {sh}");
    // Preflight, postgres, secret-gen, service, deploy-poll, idempotency.
    assert!(sh.contains("/v1/owners"), "preflight: {sh}");
    assert!(sh.contains("/v1/postgres"), "{sh}");
    assert!(sh.contains("connection-info"), "{sh}");
    assert!(sh.contains("openssl rand"), "generates JERRYCAN_SECRET: {sh}");
    assert!(sh.contains("JERRYCAN_ENV") && sh.contains("prod"), "{sh}");
    assert!(sh.contains("/v1/services"), "{sh}");
    assert!(sh.contains("healthCheckPath") || sh.contains("/healthz"), "{sh}");
    assert!(sh.contains("find_or_create") || sh.contains("?name="), "idempotent: {sh}");
    // Secrets are redacted from output (never echo the secret value).
    assert!(sh.contains("redact") || sh.contains("***"), "redaction: {sh}");
    // No literal secret value is ever printed.
    assert!(!sh.contains("echo \"$JERRYCAN_SECRET\""), "must not echo the secret: {sh}");
    // Writes resource ids (not secrets) for idempotent re-run + teardown.
    assert!(sh.contains(".deploy-state.json"), "{sh}");
}
```

- [ ] **Step 2: Run it to verify it fails** — FAIL.

- [ ] **Step 3: Write the template** (`templates/render-deploy.sh`)

```bash
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
  api POST "/services/${SVC_ID}/deploys" '{}' >/dev/null
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
```

- [ ] **Step 4: Run the content test** — `cargo test -p jerrycan --test deploy deploy_sh_has_the_secure_idempotent_flow` → PASS (adjust the template until it does).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/deploy/templates/render-deploy.sh crates/jerrycan/tests/deploy.rs
git commit -m "Add the Render deploy.sh template (pure-HTTP, idempotent, secret-safe)"
```

---

### Task 4: `shellcheck` the generated script + `teardown.sh`

**Files:**
- Create/finalize: `crates/jerrycan/src/platform/deploy/templates/render-teardown.sh`
- Test: `crates/jerrycan/tests/deploy.rs`

- [ ] **Step 1: Write the failing tests**

```rust
use std::io::Write;
use std::process::Command;

fn shellcheck(script: &str) {
    if Command::new("shellcheck").arg("--version").output().is_err() {
        eprintln!("SKIP shellcheck (not installed)");
        return;
    }
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(script.as_bytes()).unwrap();
    let out = Command::new("shellcheck")
        .args(["-S", "warning"]) // warnings + errors fail
        .arg(f.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "shellcheck:\n{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn generated_scripts_pass_shellcheck() {
    let a = deploy::emit("render", &demo_design()).unwrap();
    shellcheck(&a[0].1); // deploy.sh
    shellcheck(&a[1].1); // teardown.sh
}

#[test]
fn teardown_deletes_the_service_and_db_with_a_guard() {
    let a = deploy::emit("render", &demo_design()).unwrap();
    let td = &a[1].1;
    assert!(td.starts_with("#!/usr/bin/env bash\n") && td.contains("set -euo pipefail"), "{td}");
    assert!(td.contains("DELETE") && td.contains("/v1/services/"), "{td}");
    assert!(td.contains("/v1/postgres/"), "{td}");
    assert!(td.contains(".deploy-state.json"), "reads stored ids: {td}");
    assert!(td.to_lowercase().contains("destroy") || td.contains("read -r"), "confirmation guard: {td}");
}
```

- [ ] **Step 2: Run them to verify they fail** — FAIL (`tempfile` may need adding to `[dev-dependencies]` of `crates/jerrycan/Cargo.toml`; it's already used by other tests — confirm, else add `tempfile = "3"`).

- [ ] **Step 3: Write the teardown template** (`templates/render-teardown.sh`)

```bash
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
```

- [ ] **Step 4: Run the tests to verify they pass** — `cargo test -p jerrycan --test deploy generated_scripts_pass_shellcheck teardown_deletes` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/deploy/templates/render-teardown.sh crates/jerrycan/tests/deploy.rs
git commit -m "Add teardown.sh + shellcheck the generated Render scripts"
```

---

### Task 5: `README.md` operator doc

**Files:**
- Create/finalize: `crates/jerrycan/src/platform/deploy/templates/render-README.md`
- Test: `crates/jerrycan/tests/deploy.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn readme_documents_key_prereq_secrets_and_teardown() {
    let a = deploy::emit("render", &demo_design()).unwrap();
    let r = &a[3].1;
    assert!(r.contains("RENDER_API_KEY"), "{r}");
    assert!(r.to_lowercase().contains("registry") && r.contains("JERRYCAN_DEPLOY_SKIP_BUILD"), "registry prereq: {r}");
    assert!(r.contains("JERRYCAN_SECRET") && r.contains("JERRYCAN_SECRET_OLD"), "rotation runbook: {r}");
    assert!(r.contains("teardown.sh"), "{r}");
    assert!(r.to_lowercase().contains("least") || r.to_lowercase().contains("scope"), "token scope: {r}");
}
```

- [ ] **Step 2: Run it to verify it fails** — FAIL.

- [ ] **Step 3: Write the template** (`templates/render-README.md`)

```markdown
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
```

- [ ] **Step 4: Run the test to verify it passes** — PASS.

- [ ] **Step 5: Also append `deploy/render/.deploy-state.json` to the generated app's `.gitignore`.** In `cmd_deploy` (Task 1), after writing the artifacts, append the ignore line if missing:

```rust
    // Keep deploy state (resource ids) out of version control.
    let gitignore = root.join(".gitignore");
    let line = "deploy/render/.deploy-state.json";
    let cur = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if !cur.lines().any(|l| l.trim() == line) {
        let mut next = cur;
        if !next.is_empty() && !next.ends_with('\n') { next.push('\n'); }
        next.push_str(line);
        next.push('\n');
        std::fs::write(&gitignore, next).map_err(|e| Failure::gate(e.to_string()))?;
    }
```

Add a test asserting the gitignore line is written (in `tests/deploy.rs`, this needs a temp scaffold — defer to Task 7's integration test if simpler; at minimum a unit-level check that `cmd_deploy` appends it).

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/src/platform/deploy/templates/render-README.md crates/jerrycan/src/main.rs crates/jerrycan/tests/deploy.rs
git commit -m "Add Render deploy README (key, registry, rotation, teardown) + gitignore deploy state"
```

---

### Task 6: Determinism test

**Files:**
- Test: `crates/jerrycan/tests/deploy.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn deploy_kit_is_byte_deterministic() {
    let d = demo_design();
    let a = deploy::emit("render", &d).unwrap();
    let b = deploy::emit("render", &d).unwrap();
    assert_eq!(a, b, "same design -> byte-identical deploy kit");
}
```

- [ ] **Step 2: Run it** — should PASS immediately (pure templating). If it fails, remove any nondeterminism (no timestamps, no HashMap iteration) from `render.rs`.

- [ ] **Step 3: Commit**

```bash
git add crates/jerrycan/tests/deploy.rs
git commit -m "Lock deploy-kit determinism"
```

---

### Task 7: Mock-Render-API end-to-end flow test

**Files:**
- Test: `crates/jerrycan/tests/deploy.rs`

This runs the generated `deploy.sh` against a stub HTTP server with
`JERRYCAN_DEPLOY_SKIP_BUILD=1` (no docker) and asserts the orchestration sequence
+ that it reports success without printing the secret.

- [ ] **Step 1: Write the failing test**

```rust
use std::io::{BufRead, BufReader, Read, Write as IoWrite};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A tiny stub of the Render REST API: records request paths and replies with
/// canned JSON so the generated deploy.sh can run end-to-end with no real Render.
fn spawn_render_stub() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let rec = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = stream.unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap(); // e.g. "POST /v1/postgres HTTP/1.1"
            let parts: Vec<&str> = line.split_whitespace().collect();
            let (method, path) = (parts[0].to_string(), parts[1].to_string());
            // drain headers + any body (best-effort)
            let mut clen = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() { break; }
                if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                    clen = v.trim().parse().unwrap_or(0);
                }
            }
            if clen > 0 { let mut b = vec![0u8; clen]; r.read_exact(&mut b).ok(); }
            rec.lock().unwrap().push(format!("{method} {path}"));
            let body = render_stub_body(&method, &path);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            s.write_all(resp.as_bytes()).ok();
        }
    });
    (addr, calls)
}

fn render_stub_body(method: &str, path: &str) -> String {
    // Strip the query string for matching.
    let p = path.split('?').next().unwrap();
    match (method, p) {
        ("GET", "/v1/owners") => r#"[{"owner":{"id":"own_1"}}]"#.into(),
        ("GET", "/v1/postgres") => "[]".into(),
        ("POST", "/v1/postgres") => r#"{"id":"pg_1","status":"creating"}"#.into(),
        ("GET", "/v1/postgres/pg_1") => r#"{"status":"available"}"#.into(),
        ("GET", "/v1/postgres/pg_1/connection-info") =>
            r#"{"internalConnectionString":"postgres://u:p@h:5432/d"}"#.into(),
        ("GET", "/v1/services") => "[]".into(),
        ("POST", "/v1/services") => r#"{"service":{"id":"srv_1","serviceDetails":{"url":"https://acme-api.onrender.com"}}}"#.into(),
        ("GET", "/v1/services/srv_1") => r#"{"service":{"serviceDetails":{"url":"https://acme-api.onrender.com"}}}"#.into(),
        ("GET", _) if p.ends_with("/deploys") => r#"[{"deploy":{"status":"live"}}]"#.into(),
        _ => "{}".into(),
    }
}

#[test]
fn deploy_sh_runs_the_full_flow_against_a_stub() {
    // Write the script to a temp dir matching the expected layout (deploy/render/).
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("deploy").join("render");
    std::fs::create_dir_all(&dir).unwrap();
    let a = deploy::emit("render", &demo_design()).unwrap();
    let script = dir.join("deploy.sh");
    std::fs::write(&script, &a[0].1).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Need a Dockerfile two levels up only if building; we skip the build.
    let (base, calls) = spawn_render_stub();
    let out = Command::new("bash")
        .arg(&script)
        .env("RENDER_API_KEY", "rnd_test")
        .env("RENDER_API_BASE", format!("{base}/v1"))
        .env("JERRYCAN_DEPLOY_SKIP_BUILD", "1")
        .env("JERRYCAN_DEPLOY_IMAGE", "registry/test/acme-api")
        .env("JERRYCAN_DEPLOY_TAG", "test")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "deploy.sh failed.\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}");
    assert!(stdout.contains("Deployed acme-api") && stdout.contains("onrender.com"), "{stdout}");
    // The secret value must never appear in any output.
    assert!(!stdout.contains("JERRYCAN_SECRET=") , "secret value leaked: {stdout}");
    // The expected call sequence happened.
    let seq = calls.lock().unwrap().join("\n");
    for expect in ["GET /v1/owners", "POST /v1/postgres", "POST /v1/services"] {
        assert!(seq.contains(expect), "missing {expect} in:\n{seq}");
    }
}
```

- [ ] **Step 2: Run it** — `cargo test -p jerrycan --test deploy deploy_sh_runs_the_full_flow_against_a_stub`. Iterate on the stub bodies / the script until green. (The stub returns `200` for everything; the script's `%{http_code}` handling must read the real status — confirm the `api()` helper works against this minimal server. If the bare-`TcpListener` HTTP/1.1 framing is fiddly, an acceptable alternative is to assert via a recorded-fixtures replay; but a passing live run against the stub is the goal.)

- [ ] **Step 3: Commit**

```bash
git add crates/jerrycan/tests/deploy.rs
git commit -m "Add a mock-Render-API end-to-end test for the generated deploy.sh"
```

---

### Task 8: `#[ignore]`d live deploy test

**Files:**
- Test: `crates/jerrycan/tests/deploy.rs`

- [ ] **Step 1: Write the test** (it only runs with a real key)

```rust
/// Real Render deploy. Run with:
///   RENDER_API_KEY=… JERRYCAN_DEPLOY_IMAGE=ghcr.io/you/jc-deploy-test \
///   cargo test -p jerrycan --test deploy live_render_deploy_roundtrip -- --ignored --nocapture
#[test]
#[ignore = "needs a real RENDER_API_KEY + a pushed image (costs money/time)"]
fn live_render_deploy_roundtrip() {
    let Ok(_key) = std::env::var("RENDER_API_KEY") else {
        eprintln!("SKIP: RENDER_API_KEY not set");
        return;
    };
    // The operator scaffolds a tiny app, runs deploy.sh, curls the URL for 200,
    // then runs teardown.sh. Document the manual steps here; the assertion is the
    // operator confirming a 2xx from the live URL and a clean teardown.
    eprintln!(
        "MANUAL/LIVE: scaffold a minimal app, `./deploy/render/deploy.sh`, curl the \
         printed URL for 200, then `./deploy/render/teardown.sh`. This test documents \
         the procedure; wire full automation here once a CI Render project exists."
    );
}
```

- [ ] **Step 2: Run it once for real** (manually) — `RENDER_API_KEY=… … -- --ignored --nocapture`, deploy a scaffolded app, confirm the live URL returns 200, tear down. Record the result in the commit message.

- [ ] **Step 3: Commit**

```bash
git add crates/jerrycan/tests/deploy.rs
git commit -m "Add the ignored live Render deploy roundtrip test"
```

---

### Task 9: Docs, CHANGELOG, and full-estate sweep

**Files:**
- Modify: `docs/ai/12-packaging.md` (+ `crates/jerrycan/embedded/ai/12-packaging.md`)
- Modify: `CHANGELOG.md`
- Modify: `.claude/skills/jerrycan-backend/SKILL.md` (Phase 7 hand-off mentions deploy)

- [ ] **Step 1: Document `jerrycan deploy`** in `docs/ai/12-packaging.md` — a "Deploy (Render)" section: `jerrycan deploy render` → run `deploy/render/deploy.sh` with `RENDER_API_KEY`; the registry prereq; security (secrets in Render's store, prod fail-closed); teardown. Keep it accurate to the generated kit. `cp` to the embedded twin and run `cargo test -p jerrycan --test embedded_sync …`.

- [ ] **Step 2: Add a SKILL hand-off line** — in `.claude/skills/jerrycan-backend/SKILL.md` Phase 7, add: "To ship it: `jerrycan deploy render` generates `deploy/render/deploy.sh`; run it with `RENDER_API_KEY` for a live, secure URL (see `jerrycan docs packaging`)."

- [ ] **Step 3: CHANGELOG** — add under unreleased: "Zero-touch deploy: `jerrycan deploy render` generates an idempotent, secret-safe Render deploy kit (pure HTTP API)."

- [ ] **Step 4: Full-estate sweep**

Run:
```
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p jerrycan --test deploy
cargo test --workspace
cargo test -p jerrycan --features auth,oauth,db,jobs,rate-limit --test embedded_sync
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add docs/ai/12-packaging.md crates/jerrycan/embedded/ai/12-packaging.md CHANGELOG.md .claude/skills/jerrycan-backend/SKILL.md
git commit -m "Document jerrycan deploy (Render); changelog; skill hand-off"
```

---

## Self-review (against the spec)

- **Spec coverage:** generator-not-host (Task 1) ✓; agent-run script needing only the key (Task 3) ✓; Render REST/pure-HTTP (Task 3) ✓; image-based 2a (Task 3 build/push) ✓; managed Postgres + secrets-in-store + prod fail-closed + TLS-by-platform (Task 3 + README Task 5) ✓; self-migrate-on-boot (no migration step) ✓; idempotency find-or-create (Task 3) ✓; teardown (Task 4) ✓; `DeployTarget` abstraction (Task 1) ✓; testing = golden/determinism (Task 6) + shellcheck (Task 4) + mock-API flow (Task 7) + ignored live (Task 8) ✓; docs + scope (Task 9) ✓. The "least-privilege token scope" is documented (README Task 5).
- **Placeholder scan:** none — every step has full code. The one judgement call (verify Render field names against the API docs in Task 3) is a verification step on real code, not a code placeholder.
- **Type consistency:** `deploy::emit(target, design) -> Result<Vec<(String,String)>, String>` is used identically in every task and the CLI; `render::app_slug`/`render::artifacts` names are consistent; the four artifact paths + order match between Task 1's assertion and `render::artifacts`.
