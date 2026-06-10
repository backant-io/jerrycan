//! The jerrycan platform: shared core consumed by both the CLI (main.rs) and
//! the MCP server (platform::mcp). One pipeline, two renderings (cli-ux.md).
#![allow(clippy::module_name_repetitions)]

pub mod checkpipe;
pub mod codes;
pub mod design;
pub mod docsidx;
pub mod genroute;
pub mod lints;
pub mod mcp;
pub mod mcp_dispatch;
pub mod mounting;
pub mod openapi;
pub mod package;
pub mod questions;
pub mod sbom;
pub mod scaffold;
pub mod templates;
pub mod testgen;

/// Exit codes per docs/contracts/cli-ux.md.
pub const EXIT_OK: i32 = 0;
pub const EXIT_GATE_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_ENVIRONMENT: i32 = 3;

/// A platform-level failure that knows its exit code and (in --json mode) its payload.
#[derive(Debug)]
pub struct Failure {
    pub exit: i32,
    pub message: String,
}

impl Failure {
    pub fn usage(msg: impl Into<String>) -> Self {
        Self {
            exit: EXIT_USAGE,
            message: msg.into(),
        }
    }
    pub fn environment(msg: impl Into<String>) -> Self {
        Self {
            exit: EXIT_ENVIRONMENT,
            message: msg.into(),
        }
    }
    pub fn gate(msg: impl Into<String>) -> Self {
        Self {
            exit: EXIT_GATE_FAILED,
            message: msg.into(),
        }
    }
}

pub type PResult<T> = Result<T, Failure>;

/// Newest mtime across all Rust sources + manifests under the app (skips `target/`).
/// Lives here (not main.rs) so the scanner is unit-testable; `jerrycan dev` polls it.
pub fn newest_mtime(root: &std::path::Path) -> std::time::SystemTime {
    fn walk(dir: &std::path::Path, newest: &mut std::time::SystemTime) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, newest);
            } else {
                let is_source = path.extension().is_some_and(|e| e == "rs")
                    || path
                        .file_name()
                        .is_some_and(|n| n == "Cargo.toml" || n == "design.json");
                if is_source
                    && let Ok(m) = entry.metadata().and_then(|m| m.modified())
                    && m > *newest
                {
                    *newest = m;
                }
            }
        }
    }
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    walk(root, &mut newest);
    newest
}

#[cfg(test)]
mod tests {
    #[test]
    fn newest_mtime_sees_rs_files_and_skips_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("src/a.rs"), "x").unwrap();
        let t1 = super::newest_mtime(tmp.path());
        assert!(t1 > std::time::SystemTime::UNIX_EPOCH);
        std::fs::write(tmp.path().join("target/junk.rs"), "y").unwrap();
        assert_eq!(super::newest_mtime(tmp.path()), t1, "target/ is ignored");
    }
}
