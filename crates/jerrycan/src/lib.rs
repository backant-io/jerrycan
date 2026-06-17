//! The AI-native Rust backend platform. Generated apps depend on this one
//! crate (plus tokio) and write `use jerrycan::prelude::*;`.
//! The CLI/MCP binary joins this package in Phase 1 behind a `cli` feature.
#![forbid(unsafe_code)]

/// `#[macro_export]` lands `path_param!` at the `jerrycan_core` crate root; this
/// re-export makes `jerrycan::path_param!` resolve through the facade.
pub use jerrycan_core::path_param;
pub use jerrycan_core::*;
pub use jerrycan_macros::main;

#[cfg(feature = "db")]
pub use jerrycan_db as db;

#[cfg(feature = "validate")]
pub use jerrycan_validate as validate;

#[cfg(feature = "auth")]
pub use jerrycan_auth as auth;

#[cfg(feature = "rate-limit")]
pub use jerrycan_ratelimit as ratelimit;

#[cfg(feature = "observe")]
pub use jerrycan_observe as observe;

#[cfg(feature = "jobs")]
pub use jerrycan_jobs as jobs;

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
    doc_page!(page_00_designing, "../../../docs/ai/00-designing.md");
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
    #[cfg(feature = "auth")]
    doc_page!(page_10_auth, "../../../docs/ai/10-auth.md");
    #[cfg(feature = "observe")]
    doc_page!(
        page_11_observability,
        "../../../docs/ai/11-observability.md"
    );
    doc_page!(page_12_packaging, "../../../docs/ai/12-packaging.md");
    #[cfg(all(feature = "db", feature = "auth"))]
    doc_page!(page_14_tenancy, "../../../docs/ai/14-tenancy.md");
    #[cfg(feature = "jobs")]
    doc_page!(page_15_jobs, "../../../docs/ai/15-jobs.md");
    // Page 16 demonstrates the `MockIdp` harness, which is test/eval-only and lives
    // behind the non-default `mock-idp` feature (kept off plain `oauth` so it can't
    // reach a prod build). The page's runnable mock snippet only compiles when that
    // feature is on, so the whole page is gated on it; run it with
    // `cargo test -p jerrycan --features mock-idp --doc`.
    #[cfg(feature = "mock-idp")]
    doc_page!(
        page_16_auth_advanced,
        "../../../docs/ai/16-auth-advanced.md"
    );
    // Page 17 uses only default-facade response types (Json/Created/NoContent/
    // Redirect/StatusCode/the (StatusCode, body) tuple), so it needs no feature.
    doc_page!(
        page_17_response_types,
        "../../../docs/ai/17-response-types.md"
    );
}
