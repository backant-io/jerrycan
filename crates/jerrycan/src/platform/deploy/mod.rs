//! Zero-touch deploy generation (spec 2026-06-23). jerrycan stays a pure
//! generator: `emit` returns the deploy-kit artifacts; the CLI writes them and
//! the agent runs the generated script with only the platform API key.

pub mod render;

use crate::platform::design::Design;

/// The supported deploy targets, for help + error text.
pub const TARGETS: &[&str] = &["render"];

/// Generate the deploy kit for `target`. Returns `(relative_path, contents)`
/// artifacts in a deterministic order, or an error naming the supported targets.
pub fn emit(target: &str, design: &Design) -> Result<Vec<(String, String)>, String> {
    match target {
        "render" => Ok(render::artifacts(design)),
        other => Err(format!(
            "unknown deploy target `{other}` — supported: {}",
            TARGETS.join(", ")
        )),
    }
}
