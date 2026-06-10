//! The jerrycan platform: shared core consumed by both the CLI (main.rs) and
//! the MCP server (platform::mcp). One pipeline, two renderings (cli-ux.md).
#![allow(clippy::module_name_repetitions)]

pub mod checkpipe;
pub mod design;
pub mod docsidx;
pub mod genroute;
pub mod lints;
pub mod mcp;
pub mod mounting;
pub mod questions;
pub mod scaffold;
pub mod templates;

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
