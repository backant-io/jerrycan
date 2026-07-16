//! Proves every JSON example design in the `20-designing-examples.md` appendix
//! page is real: the page's job is to let an agent author a valid design from the
//! docs alone, so a shipped example that doesn't validate (or scaffold) would teach
//! the wrong schema. JSON code fences are NOT doctested, so this test parses the
//! fences straight out of the embedded page bytes (the same bytes served by
//! `jerrycan docs designing-examples`) and runs each through the real validator and
//! scaffolder — the page can never drift from a working design.

use jerrycan::platform::design::Design;
use jerrycan::platform::{docsidx, questions, scaffold};

/// Extract every ```json … ``` fenced block from a markdown page.
fn json_fences(md: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_json = false;
    let mut buf = String::new();
    for line in md.lines() {
        if in_json {
            if line.trim_start().starts_with("```") {
                blocks.push(std::mem::take(&mut buf));
                in_json = false;
            } else {
                buf.push_str(line);
                buf.push('\n');
            }
        } else if line.trim_start() == "```json" {
            in_json = true;
        }
    }
    blocks
}

/// The only fences that are full designs (not field/top-level snippets) are the
/// "Worked examples" — each is a complete object with a top-level `name`. The
/// top-level/module/entity/field/endpoint illustration fences are partial and
/// carry comment placeholders (`/* … */`) that aren't valid JSON, so we key on
/// "a complete design parses": parse-failures are skipped, and the count of real
/// designs is asserted so a dropped example can't pass silently.
#[test]
fn every_worked_example_validates_and_scaffolds() {
    let page =
        docsidx::get("designing-examples", None).expect("designing-examples page is registered");
    let fences = json_fences(&page);
    assert!(!fences.is_empty(), "page has json fences");

    let mut designs = 0usize;
    for block in &fences {
        // Partial illustration fences contain `/* … */` and won't parse — skip.
        let Ok(d) = serde_json::from_str::<Design>(block) else {
            continue;
        };
        designs += 1;
        let label = d.name.clone();

        let qs = questions::validate(&d);
        assert!(
            qs.is_empty(),
            "worked example `{label}` does not validate: {qs:?}"
        );

        // A valid design must also scaffold to a fresh directory.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(&label);
        let created = scaffold::scaffold(&root, &d)
            .unwrap_or_else(|e| panic!("worked example `{label}` does not scaffold: {e}"));
        assert!(
            !created.is_empty(),
            "worked example `{label}` scaffolded nothing"
        );
    }

    // The page ships seven worked examples (minimal, crud, relations, tenancy,
    // auth+public, jobs, webhook). If one is dropped or broken, this catches it.
    assert_eq!(
        designs, 7,
        "expected 7 complete worked-example designs in the designing page, found {designs}"
    );
}
