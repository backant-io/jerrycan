//! `jerrycan onboard` — the guided build runbook, embedded from the same
//! bytes as the distributed jerrycan-backend skill (embedded/SKILL.md is
//! tripwired byte-identical to docs/SKILL.md).

/// The skill file verbatim, YAML frontmatter included (that form is what
/// gets written for agents that consume skill files).
pub const SKILL_MD: &str = include_str!("../embedded/SKILL.md");

/// The runbook: the skill body with the leading `---…---` frontmatter block
/// stripped, for direct terminal/agent consumption.
pub fn runbook() -> &'static str {
    let Some(rest) = SKILL_MD.strip_prefix("---\n") else {
        return SKILL_MD;
    };
    match rest.split_once("\n---\n") {
        Some((_, body)) => body.trim_start_matches('\n'),
        None => SKILL_MD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runbook_strips_frontmatter_and_keeps_the_h1() {
        assert!(runbook().starts_with("# Building a backend with jerrycan"));
        assert!(!runbook().contains("\nname: jerrycan-backend"));
    }

    #[test]
    fn runbook_carries_the_entry_branching_and_migration_phase() {
        assert!(runbook().contains("Entry path"));
        assert!(runbook().contains("Phase 1c — Migrating from Supabase"));
    }
}
