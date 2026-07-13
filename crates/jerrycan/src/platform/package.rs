//! Hardened deployment-artifact emitters for `jerrycan package`: a multi-stage
//! Dockerfile, security-hardened k8s manifests, a hardened systemd unit, and a
//! release-binary builder. Text artifacts are deterministic; binary/image builds
//! invoke the toolchain (cargo/docker) gated on availability.

use super::design::Design;
use super::{checkpipe, sbom};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const PORT: u16 = 8000;

/// The static-musl target triple `jerrycan package --binary` prefers.
const MUSL_TARGET: &str = "x86_64-unknown-linux-musl";

/// Where cargo wrote the release `app` binary, honoring `CARGO_TARGET_DIR` the
/// same way cargo does: an absolute value is used as-is; a relative value (and
/// the `target` default) resolves against the app root that cargo ran in. A
/// hardcoded `app_root/target/...` breaks in CI/monorepos that redirect the
/// target dir — the copy then fails with "No such file or directory".
fn built_binary_path(app_root: &Path, cargo_target_dir: Option<&OsStr>, musl: bool) -> PathBuf {
    let base = app_root.join(cargo_target_dir.unwrap_or_else(|| OsStr::new("target")));
    let sub = if musl {
        format!("{MUSL_TARGET}/release/app")
    } else {
        "release/app".to_string()
    };
    base.join(sub)
}

/// The exact tail of a generated handler/job stub body — genroute and jobsgen both
/// emit `Err(Error::internal("<name> not implemented — replace this stub"))`. Those
/// two generators are the only writers of this phrase, so its presence in an
/// agent-owned source file means that unit is still an unimplemented stub.
const STUB_MARKER: &str = "not implemented — replace this stub";

/// Every agent-owned source file under `crates/` that still carries the generated
/// stub marker, relative to `root` (sorted). A stubbed handler serves JC0500, so
/// shipping it is never safe.
///
/// This is a PACKAGE-ONLY gate. `jerrycan check` is deliberately green on a fresh
/// scaffold — its lints stay clean (a hard conformance contract: "jerrycan lints
/// must be clean on a fresh scaffold") and there are no tests yet, so the stub
/// guard for `check` is the gen-tests acceptance suite (stubs fail it with JC0500).
/// `package` is the ship step, so it must independently refuse an app whose
/// handlers are unimplemented, even when the agent never ran gen-tests.
fn unimplemented_stubs(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Build output is not agent-owned source; skip it.
                if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                    continue;
                }
                walk(&path, root, out);
            } else if path.extension().is_some_and(|x| x == "rs")
                && std::fs::read_to_string(&path).is_ok_and(|c| c.contains(STUB_MARKER))
            {
                out.push(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("crates"), root, &mut out);
    out.sort();
    out
}

/// Run the check gate, emit the requested artifacts (+ always an SBOM), and
/// return (artifacts, sbom_path). Shared by the CLI `package` command and the
/// MCP `jerrycan_package` tool so the two surfaces never drift.
pub fn run_package(
    root: &Path,
    design: &Design,
    docker: bool,
    k8s: bool,
    systemd: bool,
    binary: bool,
) -> Result<(Vec<String>, String), String> {
    // Gate 0 (package-only): never ship unimplemented handler stubs. Runs before
    // the heavy check build so a stubbed app is refused immediately — matching the
    // invariant that a fresh scaffold cannot be packaged (its handlers still 500).
    let stubs = unimplemented_stubs(root);
    if !stubs.is_empty() {
        return Err(format!(
            "check failed: {} handler(s) still return the generated \"not implemented\" stub ({}) — implement them before packaging",
            stubs.len(),
            stubs.join(", ")
        ));
    }

    // Gate: never package an app that doesn't pass check.
    let report = checkpipe::run_all(root, design, None).map_err(|e| e.to_string())?;
    if !report.ok {
        return Err(format!(
            "check failed ({} diagnostics) — fix before packaging",
            report.diagnostics.len()
        ));
    }

    let mut artifacts = Vec::new();
    let mut text_targets = Vec::new();
    if docker {
        text_targets.push("docker");
    }
    if k8s {
        text_targets.push("k8s");
    }
    if systemd {
        text_targets.push("systemd");
    }
    if !text_targets.is_empty() {
        artifacts.extend(emit_text_artifacts(root, design, &text_targets)?);
    }
    if binary {
        artifacts.push(build_binary(root, design)?);
    }

    // SBOM always (it's cheap and the safety pipeline wants it).
    let deploy = root.join("deploy");
    std::fs::create_dir_all(&deploy).map_err(|e| e.to_string())?;
    let sbom = sbom::generate(root, "app")?;
    std::fs::write(deploy.join("sbom.json"), &sbom).map_err(|e| e.to_string())?;
    artifacts.push("deploy/sbom.json".to_string());

    Ok((artifacts, "deploy/sbom.json".to_string()))
}

/// A hardened multi-stage Dockerfile. Always builds a static musl binary inside
/// the rust build image (no glibc/host fallback — that path is only relevant to
/// the `--binary` target's host build, not an in-image build).
pub fn dockerfile(design: &Design) -> String {
    let name = &design.name;
    format!(
        r#"# GENERATED by jerrycan package — hardened, multi-stage, non-root.
# Requires `jerrycan` published to crates.io (or vendored into this context): the
# in-container `cargo build` below fetches it like any dependency.
FROM rust:1-bookworm AS build
WORKDIR /build
RUN rustup target add x86_64-unknown-linux-musl && \
    (apt-get update && apt-get install -y musl-tools || true)
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl -p app
RUN cp target/x86_64-unknown-linux-musl/release/app /build/{name}

FROM gcr.io/distroless/static:nonroot
COPY --from=build /build/{name} /usr/local/bin/{name}
USER nonroot
EXPOSE {PORT}
ENV JERRYCAN_ADDR=0.0.0.0:{PORT}
ENTRYPOINT ["/usr/local/bin/{name}"]
"#
    )
}

/// Deployment + Service + NetworkPolicy, security-hardened.
pub fn k8s_manifests(design: &Design) -> String {
    let name = &design.name;
    format!(
        r#"# GENERATED by jerrycan package — hardened manifests. Edit the image, then `kubectl apply -f k8s.yaml`.
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {name}
  labels:
    app: {name}
spec:
  replicas: 2
  selector:
    matchLabels:
      app: {name}
  template:
    metadata:
      labels:
        app: {name}
    spec:
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: {name}
          image: {name}:latest
          ports:
            - containerPort: {PORT}
          env:
            - name: JERRYCAN_ADDR
              value: "0.0.0.0:{PORT}"
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop:
                - ALL
          livenessProbe:
            httpGet:
              path: /healthz
              port: {PORT}
            initialDelaySeconds: 2
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /healthz
              port: {PORT}
            initialDelaySeconds: 1
            periodSeconds: 5
          resources:
            requests:
              cpu: 50m
              memory: 32Mi
            limits:
              cpu: 500m
              memory: 128Mi
---
apiVersion: v1
kind: Service
metadata:
  name: {name}
spec:
  selector:
    app: {name}
  ports:
    - port: 80
      targetPort: {PORT}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {name}
spec:
  podSelector:
    matchLabels:
      app: {name}
  policyTypes:
    - Ingress
  ingress:
    - ports:
        - protocol: TCP
          port: {PORT}
"#
    )
}

/// A hardened systemd unit (binary at `/usr/local/bin/<name>`).
pub fn systemd_unit(design: &Design) -> String {
    let name = &design.name;
    format!(
        r#"# GENERATED by jerrycan package. Install: copy the binary to /usr/local/bin/{name},
# this file to /etc/systemd/system/{name}.service, then `systemctl enable --now {name}`.
[Unit]
Description={name} (jerrycan)
After=network.target

[Service]
ExecStart=/usr/local/bin/{name}
Environment=JERRYCAN_ADDR=0.0.0.0:{PORT}
Environment=JERRYCAN_ENV=prod
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
"#
    )
}

/// Write the text artifacts for the requested targets into `<app>/deploy/`.
/// Returns the relative paths written. (Binary/image builds are separate.)
pub fn emit_text_artifacts(
    app_root: &Path,
    design: &Design,
    targets: &[&str],
) -> Result<Vec<String>, String> {
    let deploy = app_root.join("deploy");
    std::fs::create_dir_all(&deploy).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    let mut write = |rel: &str, content: &str| -> Result<(), String> {
        let path = deploy.join(rel);
        std::fs::write(&path, content).map_err(|e| format!("write deploy/{rel}: {e}"))?;
        written.push(format!("deploy/{rel}"));
        Ok(())
    };
    if targets.contains(&"docker") {
        write("Dockerfile", &dockerfile(design))?;
    }
    if targets.contains(&"k8s") {
        write("k8s.yaml", &k8s_manifests(design))?;
    }
    if targets.contains(&"systemd") {
        write(&format!("{}.service", design.name), &systemd_unit(design))?;
    }
    Ok(written)
}

/// Build a release binary, preferring static musl; falls back to the host
/// target with a note. Returns the relative artifact path.
pub fn build_binary(app_root: &Path, design: &Design) -> Result<String, String> {
    let musl_ok = std::process::Command::new("rustc")
        .args(["--print", "target-list"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|t| t == MUSL_TARGET)
        })
        .unwrap_or(false)
        && target_installed(MUSL_TARGET);
    let target_args: Vec<&str> = if musl_ok {
        vec!["--target", MUSL_TARGET]
    } else {
        eprintln!(
            "jerrycan package: musl target unavailable — building a host-target binary (not fully static). Install with: rustup target add {MUSL_TARGET}"
        );
        vec![]
    };
    let mut args = vec!["build", "--release", "-p", "app"];
    args.extend(target_args);
    let status = std::process::Command::new("cargo")
        .current_dir(app_root)
        .args(&args)
        .status()
        .map_err(|e| format!("cargo build failed to run: {e}"))?;
    if !status.success() {
        return Err("release build failed".to_string());
    }
    // Locate the binary where cargo actually wrote it (honoring CARGO_TARGET_DIR),
    // not a hardcoded `app_root/target/...`.
    let built_path = built_binary_path(
        app_root,
        std::env::var_os("CARGO_TARGET_DIR").as_deref(),
        musl_ok,
    );
    let deploy = app_root.join("deploy");
    std::fs::create_dir_all(&deploy).map_err(|e| e.to_string())?;
    let dest = deploy.join(&design.name);
    std::fs::copy(&built_path, &dest).map_err(|e| format!("copy binary: {e}"))?;
    Ok(format!("deploy/{}", design.name))
}

fn target_installed(target: &str) -> bool {
    std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|t| t == target)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::Design;

    /// The binary-copy step must read the binary from where cargo wrote it,
    /// honoring `CARGO_TARGET_DIR` like cargo does. Before this was fixed the
    /// path was hardcoded to `app_root/target/release/app`, so `jerrycan package
    /// --binary` under a redirected target dir (CI/monorepo, or a shared build
    /// cache) failed with `copy binary: No such file or directory`.
    #[test]
    fn built_binary_path_honors_cargo_target_dir() {
        let app = Path::new("/app");
        // Default: app_root/target/release/app.
        assert_eq!(
            built_binary_path(app, None, false),
            Path::new("/app/target/release/app")
        );
        // A relative CARGO_TARGET_DIR resolves against the app root cargo ran in.
        assert_eq!(
            built_binary_path(app, Some(OsStr::new("shared-target")), false),
            Path::new("/app/shared-target/release/app")
        );
        // An absolute CARGO_TARGET_DIR is used as-is (not under app_root).
        assert_eq!(
            built_binary_path(app, Some(OsStr::new("/build/out")), false),
            Path::new("/build/out/release/app")
        );
        // The musl target adds its triple subdir under whichever target dir.
        assert_eq!(
            built_binary_path(app, Some(OsStr::new("/build/out")), true),
            Path::new("/build/out/x86_64-unknown-linux-musl/release/app")
        );
        // Regression guard: the default path must NOT be the bare hardcoded one
        // when a target dir is redirected.
        assert_ne!(
            built_binary_path(app, Some(OsStr::new("/build/out")), false),
            Path::new("/app/target/release/app")
        );
    }

    /// A fresh scaffold's handlers are stubs (`Err(Error::internal("… not
    /// implemented — replace this stub"))`), so the package stub-gate must list
    /// them; once a handler's body no longer carries the marker, the gate clears.
    ///
    /// WHY (Rule 9): this is the invariant `package_refuses_when_check_is_red`
    /// encodes — jerrycan must never SHIP an app whose handlers still 500. `check`
    /// is intentionally green on a fresh scaffold (lints stay clean by contract), so
    /// the refusal has to live in `package`; a gate that couldn't tell an
    /// implemented handler from a stub would let unimplemented code deploy.
    #[test]
    fn stub_gate_flags_scaffold_then_clears_when_implemented() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        let design: Design = serde_json::from_str(crate::platform::design::tests::MINIMAL).unwrap();
        crate::platform::scaffold::scaffold(&app, &design).unwrap();

        let stubs = unimplemented_stubs(&app);
        assert!(
            stubs.iter().any(|p| p.ends_with("handlers.rs")),
            "a fresh scaffold's handlers are unimplemented stubs: {stubs:?}"
        );

        // Implement every stubbed unit (drop the marker): the gate must clear.
        for rel in &stubs {
            std::fs::write(app.join(rel), "// implemented — no stub marker\n").unwrap();
        }
        assert!(
            unimplemented_stubs(&app).is_empty(),
            "no stub markers remain once handlers are implemented"
        );
    }

    /// `run_package` refuses (never emits artifacts) while any stub remains, and the
    /// error names the failed `check` gate — the exact contract the MCP/CLI
    /// `package` surfaces assert on.
    #[test]
    fn run_package_refuses_a_stub_scaffold() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        let design: Design = serde_json::from_str(crate::platform::design::tests::MINIMAL).unwrap();
        crate::platform::scaffold::scaffold(&app, &design).unwrap();

        let err = run_package(&app, &design, false, true, false, false)
            .expect_err("a stub scaffold must not package");
        assert!(err.contains("check"), "error names the check gate: {err}");
        // Refused before any artifact was written.
        assert!(
            !app.join("deploy/k8s.yaml").exists(),
            "no artifacts on a refused package"
        );
    }
}
