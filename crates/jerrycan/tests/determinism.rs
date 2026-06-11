//! Same design => byte-identical generated output. Agents diff regenerations;
//! nondeterminism would poison that loop (and the schema.json check gate).
use jerrycan::platform::{design::Design, scaffold};
use std::path::Path;

fn tree_digest(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push((
                    p.strip_prefix(base).unwrap().display().to_string(),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    walk(root, root, &mut files);
    files
}

#[test]
fn generation_is_byte_deterministic() {
    let src = include_str!("../../../conformance/designs/kolli-slice.design.json");
    let d: Design = serde_json::from_str(src).unwrap();
    let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (ta.path().join("app"), tb.path().join("app"));
    scaffold::scaffold(&a, &d).unwrap();
    scaffold::scaffold(&b, &d).unwrap();
    let (da, db) = (tree_digest(&a), tree_digest(&b));
    assert_eq!(da.len(), db.len());
    for ((pa, ba), (pb, bb)) in da.iter().zip(db.iter()) {
        assert_eq!(pa, pb, "tree shape differs");
        assert_eq!(ba, bb, "bytes differ in {pa}");
    }
}

#[tokio::test]
async fn schema_derivation_is_deterministic() {
    use jerrycan::platform::schema;
    let src = include_str!("../../../conformance/designs/kolli-slice.design.json");
    let d: Design = serde_json::from_str(src).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    scaffold::scaffold(&root, &d).unwrap();
    let one = schema::render(&schema::derive_schema(&root, &d).await.unwrap());
    let two = schema::render(&schema::derive_schema(&root, &d).await.unwrap());
    assert_eq!(one, two);
}
