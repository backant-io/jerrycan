//! Every design in the conformance corpus must validate and scaffold under
//! the current code — contract v0 documents are forever-valid (additive v1).
use jerrycan::platform::{design::Design, questions, scaffold};

#[test]
fn every_corpus_design_still_validates_and_scaffolds() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/designs");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap();
        let d: Design = serde_json::from_str(&src).unwrap_or_else(|e| panic!("{p:?}: {e}"));
        let qs = questions::validate(&d);
        assert!(qs.is_empty(), "{p:?}: {qs:?}");
        let tmp = tempfile::tempdir().unwrap();
        scaffold::scaffold(&tmp.path().join("app"), &d).unwrap_or_else(|e| panic!("{p:?}: {e}"));
        checked += 1;
    }
    assert!(checked >= 2, "corpus must not silently shrink: {checked}");
}
