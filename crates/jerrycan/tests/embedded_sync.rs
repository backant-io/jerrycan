//! The crate ships committed copies of the docs/contract files it embeds
//! (cargo package cannot include files outside the crate directory). This
//! tripwire keeps the copies byte-identical to the canonical repo files.
//! In a published tarball the canonical files are absent and the test skips.
use std::fs;
use std::path::Path;

#[test]
fn embedded_copies_match_canonical_repo_files() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.join("../..");
    if !repo_root.join("docs/ai").exists() {
        return; // published tarball: no canonical tree to compare against
    }

    let mut pairs: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(repo_root.join("docs/ai")).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        pairs.push((format!("docs/ai/{name}"), format!("embedded/ai/{name}")));
    }
    pairs.push((
        "docs/contracts/mcp-tools.json".into(),
        "embedded/contracts/mcp-tools.json".into(),
    ));
    pairs.push(("docs/SKILL.md".into(), "embedded/SKILL.md".into()));
    pairs.push((
        "conformance/designs/todo-api.design.json".into(),
        "embedded/designs/todo-api.design.json".into(),
    ));

    for (canonical, embedded) in pairs {
        let canon = fs::read_to_string(repo_root.join(&canonical)).unwrap();
        let copy = fs::read_to_string(crate_dir.join(&embedded)).unwrap_or_default();
        assert_eq!(
            canon, copy,
            "embedded copy is stale: cp {canonical} crates/jerrycan/{embedded}"
        );
    }

    // #136: the Claude Code skill twin lives at the REPO ROOT (`.claude/skills/…`),
    // not under the crate, so it resolves against repo_root — the embedded_sync
    // tripwire previously guarded docs/SKILL.md <-> embedded/SKILL.md but NOT this
    // pair, so an edit to one and not the other passed CI. Guard it too.
    let skill_canon = fs::read_to_string(repo_root.join("docs/SKILL.md")).unwrap();
    let skill_twin = fs::read_to_string(repo_root.join(".claude/skills/jerrycan-backend/SKILL.md"))
        .unwrap_or_default();
    assert_eq!(
        skill_canon, skill_twin,
        "skill twin is stale: cp docs/SKILL.md .claude/skills/jerrycan-backend/SKILL.md"
    );
}
