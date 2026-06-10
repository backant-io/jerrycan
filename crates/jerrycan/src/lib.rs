//! The AI-native Rust backend platform. Generated apps depend on this one
//! crate (plus tokio) and write `use jerrycan::prelude::*;`.
//! The CLI/MCP binary joins this package in Phase 1 behind a `cli` feature.
#![forbid(unsafe_code)]

pub use jerrycan_core::*;
pub use jerrycan_macros::main;

#[cfg(feature = "db")]
pub use jerrycan_db as db;

#[cfg(feature = "validate")]
pub use jerrycan_validate as validate;

#[cfg(feature = "cli")]
pub mod platform;

pub mod prelude {
    pub use jerrycan_core::prelude::*;
    pub use jerrycan_macros::main;
}

/// Compile-checks every example in docs/ai/*.md (spec §8: executable docs).
#[cfg(doctest)]
mod doc_tests {
    macro_rules! doc_page {
        ($name:ident, $path:literal) => {
            #[doc = include_str!($path)]
            mod $name {}
        };
    }
    doc_page!(page_01_app, "../../../docs/ai/01-app.md");
    doc_page!(page_02_modules, "../../../docs/ai/02-modules.md");
    doc_page!(page_03_extractors, "../../../docs/ai/03-extractors.md");
    doc_page!(page_04_dependencies, "../../../docs/ai/04-dependencies.md");
    doc_page!(page_05_errors, "../../../docs/ai/05-errors.md");
    doc_page!(page_06_middleware, "../../../docs/ai/06-middleware.md");
    doc_page!(page_07_testing, "../../../docs/ai/07-testing.md");
    #[cfg(feature = "db")]
    doc_page!(page_08_database, "../../../docs/ai/08-database.md");
    #[cfg(feature = "validate")]
    doc_page!(page_09_validation, "../../../docs/ai/09-validation.md");
}
