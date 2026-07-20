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

use std::path::{Path, PathBuf};

const MARKER_START: &str = "<!-- jerrycan-backend:start -->";
const MARKER_END: &str = "<!-- jerrycan-backend:end -->";
const MCP_SNIPPET: &str =
    r#"{ "mcpServers": { "jerrycan": { "command": "jerrycan", "args": ["mcp"] } } }"#;

/// Which agent to emit skill/rules files for. `generic` writes nothing and
/// returns the content as printable instructions instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Cursor,
    Codex,
    Windsurf,
    Generic,
}

impl std::str::FromStr for Agent {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            "codex" => Ok(Self::Codex),
            "windsurf" => Ok(Self::Windsurf),
            "generic" => Ok(Self::Generic),
            other => Err(format!(
                "unknown agent `{other}` — expected one of: claude-code, cursor, codex, windsurf, generic"
            )),
        }
    }
}

/// What an emit did: files created/updated, files already current, and any
/// instructions the caller should print (MCP wiring, generic block).
pub struct Emitted {
    pub written: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    pub instructions: Option<String>,
}

/// The AGENTS.md marker block: the frontmatter-stripped runbook fenced by
/// HTML markers, so re-runs replace instead of duplicate and foreign content
/// is never touched.
fn marker_block() -> String {
    format!("{MARKER_START}\n{}\n{MARKER_END}\n", runbook().trim_end())
}

/// Write `content` to `path` unless it already matches, creating parents.
fn write_if_changed(path: &Path, content: &str, out: &mut Emitted) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|cur| cur == content) {
        out.unchanged.push(path.to_path_buf());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    out.written.push(path.to_path_buf());
    Ok(())
}

/// Upsert the marker block into an AGENTS.md body, preserving everything
/// outside the markers.
fn upsert_block(existing: &str) -> String {
    let block = marker_block();
    match (existing.find(MARKER_START), existing.find(MARKER_END)) {
        (Some(start), Some(end)) if end > start => {
            let after = existing[end + MARKER_END.len()..].trim_start_matches('\n');
            format!("{}{block}{after}", &existing[..start])
        }
        _ if existing.trim().is_empty() => block,
        _ => format!("{}\n{block}", existing.trim_end_matches('\n')),
    }
}

/// Emit the skill for one agent. `project_dir` hosts project-level files
/// (AGENTS.md); `home_dir` hosts user-level ones (~/.claude). Both are
/// injected so tests never touch the real home.
pub fn emit_skill(agent: Agent, project_dir: &Path, home_dir: &Path) -> std::io::Result<Emitted> {
    let mut out = Emitted {
        written: Vec::new(),
        unchanged: Vec::new(),
        instructions: None,
    };
    match agent {
        Agent::ClaudeCode => {
            let path = home_dir.join(".claude/skills/jerrycan-backend/SKILL.md");
            write_if_changed(&path, SKILL_MD, &mut out)?;
            out.instructions = Some(
                "MCP: run `claude mcp add jerrycan -- jerrycan mcp` (skip if already added)."
                    .to_string(),
            );
        }
        Agent::Cursor | Agent::Codex | Agent::Windsurf => {
            let path = project_dir.join("AGENTS.md");
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            write_if_changed(&path, &upsert_block(&existing), &mut out)?;
            let hint = match agent {
                Agent::Cursor => ".cursor/mcp.json",
                Agent::Codex => "~/.codex/config.toml (mcp_servers section)",
                _ => "your agent's MCP config",
            };
            out.instructions = Some(format!(
                "MCP: add this stdio server to {hint}:\n{MCP_SNIPPET}"
            ));
        }
        Agent::Generic => {
            out.instructions = Some(format!(
                "Add this block to your agent's rules/AGENTS.md:\n\n{}\nMCP (stdio): {MCP_SNIPPET}",
                marker_block()
            ));
        }
    }
    Ok(out)
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

    #[test]
    fn claude_code_emit_writes_the_skill_verbatim_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        let first = emit_skill(Agent::ClaudeCode, proj.path(), home.path()).unwrap();
        let path = home.path().join(".claude/skills/jerrycan-backend/SKILL.md");
        assert_eq!(first.written, vec![path.clone()]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_MD);
        let second = emit_skill(Agent::ClaudeCode, proj.path(), home.path()).unwrap();
        assert!(second.written.is_empty());
        assert_eq!(second.unchanged, vec![path]);
    }

    #[test]
    fn cursor_emit_appends_a_marker_block_and_replaces_it_on_rerun() {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("AGENTS.md"), "# Mine\n\nkeep me\n").unwrap();
        emit_skill(Agent::Cursor, proj.path(), home.path()).unwrap();
        let agents = std::fs::read_to_string(proj.path().join("AGENTS.md")).unwrap();
        assert!(
            agents.starts_with("# Mine\n\nkeep me\n"),
            "foreign content clobbered"
        );
        assert!(agents.contains("<!-- jerrycan-backend:start -->"));
        assert!(agents.contains("Phase 1c — Migrating from Supabase"));
        // Re-run replaces the block instead of appending a second copy.
        emit_skill(Agent::Cursor, proj.path(), home.path()).unwrap();
        let again = std::fs::read_to_string(proj.path().join("AGENTS.md")).unwrap();
        assert_eq!(again.matches("jerrycan-backend:start").count(), 1);
    }

    #[test]
    fn generic_emit_writes_nothing_and_returns_instructions() {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        let out = emit_skill(Agent::Generic, proj.path(), home.path()).unwrap();
        assert!(out.written.is_empty() && out.unchanged.is_empty());
        let text = out.instructions.unwrap();
        assert!(text.contains("jerrycan-backend:start"));
        assert!(text.contains("\"command\": \"jerrycan\""));
        assert!(std::fs::read_dir(proj.path()).unwrap().next().is_none());
    }

    #[test]
    fn unknown_agent_id_lists_the_valid_ones() {
        let err = "zed".parse::<Agent>().unwrap_err();
        for id in ["claude-code", "cursor", "codex", "windsurf", "generic"] {
            assert!(err.contains(id), "{err} must list {id}");
        }
    }
}
