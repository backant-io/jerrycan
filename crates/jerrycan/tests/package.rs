//! Artifact-emission assertions (fast: text generation, no real cargo/docker builds).

use jerrycan::platform::design::Design;
use jerrycan::platform::package;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn design() -> Design {
    serde_json::from_str(GOLDEN).unwrap()
}

#[test]
fn dockerfile_is_distroless_nonroot_static() {
    let df = package::dockerfile(&design(), false);
    assert!(
        df.contains("FROM rust:") && df.contains(" AS build"),
        "{df}"
    );
    assert!(
        df.contains("x86_64-unknown-linux-musl"),
        "static target: {df}"
    );
    assert!(
        df.contains("FROM gcr.io/distroless/static") || df.contains("FROM scratch"),
        "{df}"
    );
    assert!(
        df.contains("USER nonroot") || df.contains("USER 65532"),
        "non-root: {df}"
    );
    assert!(df.contains("EXPOSE 8000"));
    assert!(
        df.contains("ENV JERRYCAN_ADDR=0.0.0.0:8000"),
        "bind all interfaces in container: {df}"
    );
}

#[test]
fn k8s_manifests_are_hardened() {
    let y = package::k8s_manifests(&design());
    assert!(
        y.contains("kind: Deployment")
            && y.contains("kind: Service")
            && y.contains("kind: NetworkPolicy")
    );
    assert!(y.contains("runAsNonRoot: true"));
    assert!(y.contains("readOnlyRootFilesystem: true"));
    assert!(y.contains("allowPrivilegeEscalation: false"));
    assert!(
        y.contains("drop:\n                - ALL") || y.contains("- ALL"),
        "drop all caps: {y}"
    );
    assert!(y.contains("livenessProbe") && y.contains("/healthz"));
    assert!(y.contains("resources:") && y.contains("limits:"));
    assert!(y.contains("name: todo-api"));
}

#[test]
fn systemd_unit_is_hardened() {
    let u = package::systemd_unit(&design());
    assert!(u.contains("[Service]"));
    assert!(u.contains("DynamicUser=yes"));
    assert!(u.contains("ProtectSystem=strict"));
    assert!(u.contains("NoNewPrivileges=yes"));
    assert!(u.contains("PrivateTmp=yes"));
    assert!(u.contains("Restart=on-failure"));
}

#[test]
fn package_writes_text_targets_into_a_deploy_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    jerrycan::platform::scaffold::scaffold(&root, &design()).unwrap();
    // text-only target: no toolchain needed
    let written =
        package::emit_text_artifacts(&root, &design(), &["k8s", "systemd", "docker"]).unwrap();
    assert!(root.join("deploy/Dockerfile").exists());
    assert!(root.join("deploy/k8s.yaml").exists());
    assert!(root.join("deploy/todo-api.service").exists());
    assert!(written.iter().any(|p| p.contains("deploy/k8s.yaml")));
}
