//! The Render deploy target: fills the shell/text templates with the app slug +
//! image ref and returns the artifacts. Pure templating — no network, no I/O.

use crate::platform::design::Design;

/// The Render resource base name: the design name, lowercased, non-alnum → '-',
/// collapsed, trimmed. Service = `<slug>`, DB = `<slug>-db`, image tag default
/// `<slug>`. Stable so re-runs find-or-create the same resources. A name with no
/// alphanumerics (slug would be empty) falls back to `"app"` so the generated
/// resource names are never empty/invalid.
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
    let slug = s.trim_matches('-');
    if slug.is_empty() {
        "app".to_string()
    } else {
        slug.to_string()
    }
}

const DEPLOY_SH: &str = include_str!("templates/render-deploy.sh");
const TEARDOWN_SH: &str = include_str!("templates/render-teardown.sh");
const RENDER_YAML: &str = include_str!("templates/render.yaml");
const README_MD: &str = include_str!("templates/render-README.md");

/// `(relative_path, contents)` for the five artifacts, deterministic order.
/// The kit is self-contained: it emits its own hardened Dockerfile (the same one
/// `jerrycan package --docker` produces) so a deploy needs no prior `package` run.
pub fn artifacts(design: &Design) -> Vec<(String, String)> {
    let slug = app_slug(design);
    let fill = |t: &str| t.replace("{{APP_SLUG}}", &slug);
    vec![
        ("deploy/render/deploy.sh".into(), fill(DEPLOY_SH)),
        ("deploy/render/teardown.sh".into(), fill(TEARDOWN_SH)),
        ("deploy/render/render.yaml".into(), fill(RENDER_YAML)),
        ("deploy/render/README.md".into(), fill(README_MD)),
        (
            "deploy/render/Dockerfile".into(),
            crate::platform::package::dockerfile(design),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn design_named(name: &str) -> Design {
        // Minimal valid design; only `name` matters for app_slug.
        serde_json::from_value(serde_json::json!({
            "name": name, "contract_version": 1, "modules": []
        }))
        .expect("minimal design")
    }

    #[test]
    fn app_slug_slugifies_and_collapses() {
        assert_eq!(app_slug(&design_named("Acme API")), "acme-api");
        assert_eq!(app_slug(&design_named("  My__Cool  App!! ")), "my-cool-app");
        assert_eq!(app_slug(&design_named("already-slug")), "already-slug");
    }

    #[test]
    fn app_slug_falls_back_to_app_when_no_alphanumerics() {
        // A name with no alphanumerics would slugify to "" — never emit an empty
        // resource name (it would make the Render service/DB names invalid).
        assert_eq!(app_slug(&design_named("!!! ___ ---")), "app");
        assert_eq!(app_slug(&design_named("")), "app");
    }
}
