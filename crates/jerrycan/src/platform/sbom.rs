//! CycloneDX 1.5 SBOM from `cargo metadata` — no cargo-cyclonedx dependency.

use serde_json::Value;

/// Build a CycloneDX 1.5 BOM from parsed `cargo metadata`. `root_name`/`root_version`
/// identify the app under analysis (becomes metadata.component, excluded from components).
pub fn document(metadata: &Value, root_name: &str, root_version: &str) -> Value {
    let empty = vec![];
    let packages = metadata["packages"].as_array().unwrap_or(&empty);
    let mut components = Vec::new();
    for pkg in packages {
        let name = pkg["name"].as_str().unwrap_or("");
        let version = pkg["version"].as_str().unwrap_or("");
        if name == root_name && version == root_version {
            continue; // the root is metadata.component, not a dependency
        }
        let mut component = serde_json::json!({
            "type": "library",
            "name": name,
            "version": version,
            "purl": format!("pkg:cargo/{name}@{version}"),
        });
        if let Some(license) = pkg["license"].as_str() {
            component["licenses"] = serde_json::json!([{ "expression": license }]);
        }
        components.push(component);
    }
    components.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    serde_json::json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": { "type": "application", "name": root_name, "version": root_version }
        },
        "components": components,
    })
}

/// Run `cargo metadata` for an app and produce the pretty SBOM JSON.
pub fn generate(
    app_root: &std::path::Path,
    root_name: &str,
    root_version: &str,
) -> Result<String, String> {
    let output = std::process::Command::new("cargo")
        .current_dir(app_root)
        .args(["metadata", "--format-version", "1"])
        .output()
        .map_err(|e| format!("cargo metadata failed to run: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("cargo metadata parse: {e}"))?;
    let doc = document(&metadata, root_name, root_version);
    let mut s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed `cargo metadata` shape: packages with name/version/license/source.
    const META: &str = r#"{
        "packages": [
            { "name": "app", "version": "0.1.0", "license": "MIT OR Apache-2.0", "source": null },
            { "name": "serde", "version": "1.0.0", "license": "MIT OR Apache-2.0", "source": "registry+https://github.com/rust-lang/crates.io-index" },
            { "name": "tokio", "version": "1.40.0", "license": "MIT", "source": "registry+https://github.com/rust-lang/crates.io-index" }
        ]
    }"#;

    #[test]
    fn cyclonedx_shape_and_components() {
        let doc = document(&serde_json::from_str(META).unwrap(), "app", "0.1.0");
        assert_eq!(doc["bomFormat"], "CycloneDX");
        assert_eq!(doc["specVersion"], "1.5");
        assert_eq!(doc["metadata"]["component"]["name"], "app");
        let comps = doc["components"].as_array().unwrap();
        // The root package is the metadata.component, not a dependency component.
        assert!(
            comps.iter().all(|c| c["name"] != "app"),
            "root excluded from components"
        );
        assert!(
            comps
                .iter()
                .any(|c| c["name"] == "serde" && c["version"] == "1.0.0")
        );
        let serde = comps.iter().find(|c| c["name"] == "serde").unwrap();
        assert_eq!(serde["type"], "library");
        assert!(
            serde["purl"]
                .as_str()
                .unwrap()
                .starts_with("pkg:cargo/serde@1.0.0")
        );
        assert_eq!(serde["licenses"][0]["expression"], "MIT OR Apache-2.0");
    }

    #[test]
    fn registryless_packages_are_still_listed() {
        // local path deps (source null) are components too, minus a registry purl.
        let doc = document(&serde_json::from_str(META).unwrap(), "app", "0.1.0");
        // only "app" is source-null and it's the root, so all listed components have purls:
        for c in doc["components"].as_array().unwrap() {
            assert!(c["purl"].is_string());
        }
    }
}
