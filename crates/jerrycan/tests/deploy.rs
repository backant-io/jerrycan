//! `jerrycan deploy <target>` generates a self-contained deploy kit.
use jerrycan::platform::{deploy, design::Design};
use std::io::Write;
use std::process::Command;

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
    assert!(
        deploy_sh.contains("acme-api"),
        "app slug substituted: {deploy_sh}"
    );
    assert!(!deploy_sh.contains("Acme API"), "raw name not leaked");
}

#[test]
fn unknown_target_is_an_error() {
    let err = deploy::emit("heroku", &demo_design()).unwrap_err();
    assert!(err.contains("unknown deploy target"), "{err}");
    assert!(err.contains("render"), "lists the supported targets: {err}");
}

#[test]
fn render_yaml_declares_a_web_service_a_db_and_secret_envs() {
    let artifacts = deploy::emit("render", &demo_design()).unwrap();
    let yaml = &artifacts[2].1; // render.yaml
    assert!(yaml.contains("type: web_service"), "{yaml}");
    assert!(yaml.contains("name: acme-api"), "{yaml}");
    assert!(yaml.contains("healthCheckPath: /healthz"), "{yaml}");
    assert!(
        yaml.contains("databases:") && yaml.contains("name: acme-api-db"),
        "{yaml}"
    );
    // JERRYCAN_SECRET is generateValue (Render generates + stores it), never inline.
    assert!(
        yaml.contains("key: JERRYCAN_SECRET") && yaml.contains("generateValue: true"),
        "{yaml}"
    );
    assert!(
        yaml.contains("key: JERRYCAN_ENV") && yaml.contains("value: prod"),
        "{yaml}"
    );
}

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
    assert!(
        sh.contains("find_or_create") || sh.contains("?name="),
        "idempotent: {sh}"
    );
    // Secrets are redacted from output (never echo the secret value).
    assert!(sh.contains("redact") || sh.contains("***"), "redaction: {sh}");
    // No literal secret value is ever printed.
    assert!(
        !sh.contains("echo \"$JERRYCAN_SECRET\""),
        "must not echo the secret: {sh}"
    );
    // Writes resource ids (not secrets) for idempotent re-run + teardown.
    assert!(sh.contains(".deploy-state.json"), "{sh}");
}

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
    assert!(
        out.status.success(),
        "shellcheck:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
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
    assert!(
        td.starts_with("#!/usr/bin/env bash\n") && td.contains("set -euo pipefail"),
        "{td}"
    );
    assert!(td.contains("DELETE") && td.contains("/v1/services/"), "{td}");
    assert!(td.contains("/v1/postgres/"), "{td}");
    assert!(td.contains(".deploy-state.json"), "reads stored ids: {td}");
    assert!(
        td.to_lowercase().contains("destroy") || td.contains("read -r"),
        "confirmation guard: {td}"
    );
}
