//! The AI-native Rust backend platform. Generated apps depend on this one
//! crate (plus tokio) and write `use jerrycan::prelude::*;`.
//! The CLI/MCP binary joins this package in Phase 1 behind a `cli` feature.
#![forbid(unsafe_code)]

pub use jerrycan_core::*;
pub use jerrycan_macros::main;

pub mod prelude {
    pub use jerrycan_core::prelude::*;
    pub use jerrycan_macros::main;
}
