//! Heavy conformance tests (#[ignore]): real cargo builds of generated apps.
//! Run with: cargo test -p jerrycan --test conformance -- --include-ignored

use std::path::{Path, PathBuf};
use std::process::Command;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// Scaffold the golden app wired to the LOCAL framework (path dep).
fn scaffold_golden(tmp: &Path) -> PathBuf {
    let design = tmp.join("design.json");
    std::fs::write(&design, GOLDEN).unwrap();
    let app = tmp.join("todo-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new")
        .arg(&app)
        .arg("--design")
        .arg(&design)
        .status()
        .unwrap();
    assert!(st.success());
    app
}

#[test]
#[ignore = "heavy: full cargo build of a generated workspace"]
fn scaffolded_app_builds_with_zero_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden(tmp.path());
    let out = Command::new("cargo")
        .current_dir(&app)
        .env("RUSTFLAGS", "-D warnings")
        .args(["build", "--workspace"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "generated app must build warning-free:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
