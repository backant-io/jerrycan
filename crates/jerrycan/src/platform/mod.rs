//! The jerrycan platform: shared core consumed by both the CLI (main.rs) and
//! the MCP server (platform::mcp). One pipeline, two renderings (cli-ux.md).
#![allow(clippy::module_name_repetitions)]

pub mod checkpipe;
pub mod codes;
pub mod deploy;
pub mod design;
pub mod docsidx;
pub mod genroute;
pub mod jobsgen;
pub mod lints;
pub mod mcp;
pub mod mcp_dispatch;
pub mod migrate;
pub mod mounting;
pub mod openapi;
pub mod package;
pub mod questions;
pub mod realtimegen;
pub mod sbom;
pub mod scaffold;
pub mod schema;
pub mod storagegen;
pub mod templates;
pub mod testgen;

/// Exit codes per docs/contracts/cli-ux.md.
pub const EXIT_OK: i32 = 0;
pub const EXIT_GATE_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_ENVIRONMENT: i32 = 3;

/// A platform-level failure that knows its exit code and (in --json mode) its
/// machine envelope. Every `--json` failure surfaces as one stdout document
/// `{ok:false, code, error, hint}`; `code`/`hint` carry a diagnostic JC code and
/// a recovery line when the failure has them (e.g. the tenancy/identity lint).
// `non_exhaustive` (since 0.4.0): adding envelope fields to a struct-literal-
// constructible pub struct was a breaking change against published 0.3.0 (the
// gate caught it). Construction goes through the `usage`/`environment`/`gate`
// constructors + `with_*` builders; downstream literals are intentionally
// impossible so future fields stay semver-minor.
#[derive(Debug)]
#[non_exhaustive]
pub struct Failure {
    pub exit: i32,
    pub message: String,
    /// A stable diagnostic code (`jerrycan explain <code>`), when this failure
    /// maps to one; `None` for plain usage/environment errors.
    pub code: Option<&'static str>,
    /// A short recovery hint, rendered as the envelope's `hint` field.
    pub hint: Option<String>,
    /// True when the failing command already wrote its own machine-readable JSON
    /// to stdout (the questions list, the `jerrycan check` report). The `--json`
    /// sink then skips the generic envelope so stdout stays exactly one document.
    pub json_emitted: bool,
}

impl Failure {
    pub fn usage(msg: impl Into<String>) -> Self {
        Self::new(EXIT_USAGE, msg)
    }
    pub fn environment(msg: impl Into<String>) -> Self {
        Self::new(EXIT_ENVIRONMENT, msg)
    }
    pub fn gate(msg: impl Into<String>) -> Self {
        Self::new(EXIT_GATE_FAILED, msg)
    }
    fn new(exit: i32, msg: impl Into<String>) -> Self {
        Self {
            exit,
            message: msg.into(),
            code: None,
            hint: None,
            json_emitted: false,
        }
    }
    /// Attach a diagnostic code surfaced in the `--json` envelope's `code` field.
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
    /// Attach a recovery hint surfaced in the `--json` envelope's `hint` field.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    /// Mark that the command already emitted its own stdout JSON, so the sink
    /// must not add the generic envelope (avoids two documents on stdout).
    pub fn mark_json_emitted(mut self) -> Self {
        self.json_emitted = true;
        self
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
