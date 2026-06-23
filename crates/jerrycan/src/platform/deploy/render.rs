//! The Render deploy target: fills the shell/text templates with the app slug +
//! image ref and returns the artifacts. Pure templating — no network, no I/O.

use crate::platform::design::Design;

/// The Render resource base name: the design name, lowercased, non-alnum → '-',
/// collapsed, trimmed. Service = `<slug>`, DB = `<slug>-db`, image tag default
/// `<slug>`. Stable so re-runs find-or-create the same resources.
pub fn app_slug(design: &Design) -> String {
    let mut s = String::with_capacity(design.name.len());
    let mut prev_dash = false;
    for c in design.name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
            prev_dash = false;
        } else if !prev_dash && !s.is_empty() {
            s.push('-');
            prev_dash = true;
        }
    }
    s.trim_matches('-').to_string()
}

const DEPLOY_SH: &str = include_str!("templates/render-deploy.sh");
const TEARDOWN_SH: &str = include_str!("templates/render-teardown.sh");
const RENDER_YAML: &str = include_str!("templates/render.yaml");
const README_MD: &str = include_str!("templates/render-README.md");

/// `(relative_path, contents)` for the four artifacts, deterministic order.
pub fn artifacts(design: &Design) -> Vec<(String, String)> {
    let slug = app_slug(design);
    let fill = |t: &str| t.replace("{{APP_SLUG}}", &slug);
    vec![
        ("deploy/render/deploy.sh".into(), fill(DEPLOY_SH)),
        ("deploy/render/teardown.sh".into(), fill(TEARDOWN_SH)),
        ("deploy/render/render.yaml".into(), fill(RENDER_YAML)),
        ("deploy/render/README.md".into(), fill(README_MD)),
    ]
}
