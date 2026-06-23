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
