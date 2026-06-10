//! Fast CLI contract tests. Exit codes per docs/contracts/cli-ux.md:
//! 0 ok · 1 gate failed · 2 usage error · 3 environment error.

use std::process::Command;

fn jerrycan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jerrycan"))
}

#[test]
fn version_prints_and_exits_zero() {
    let out = jerrycan().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_flag_is_usage_error_exit_2() {
    let out = jerrycan().arg("--definitely-not-a-flag").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn missing_required_arg_is_usage_error_exit_2() {
    // `new` requires --design; no interactive prompts ever (cli-ux.md non-goals).
    let out = jerrycan().args(["new", "demo"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--design"),
        "must name the exact missing flag: {err}"
    );
}
