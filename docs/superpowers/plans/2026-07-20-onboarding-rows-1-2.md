# Onboarding Rows 1–2 Implementation Plan (release pipeline + `jerrycan onboard`)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship spec §5 rows 1–2 of `docs/superpowers/specs/2026-07-19-onboarding-design.md`: prebuilt release binaries + binstall metadata, and the `jerrycan onboard` subcommand with `--emit-skill --agent <id>`, with the skill twins gaining 3-way entry branching and a Supabase-migration phase.

**Architecture:** Row 1 is a new tag-triggered GitHub workflow (taiki-e actions matrix) plus `[package.metadata.binstall]`. Row 2 embeds `docs/SKILL.md` into the CLI binary via a new byte-identical twin (`crates/jerrycan/embedded/SKILL.md`, enforced by the existing `embedded_sync` tripwire) and a **bin-private** module `crates/jerrycan/src/onboard.rs` (declared `mod onboard;` in `main.rs`, NOT in the lib) — zero new public lib API, so no semver bump is needed on these branches.

**Tech Stack:** GitHub Actions (taiki-e/create-gh-release-action@v1, taiki-e/upload-rust-binary-action@v1, dtolnay/rust-toolchain@1.97), clap derive, `include_str!`, std-only file IO.

## Global Constraints

- Toolchain pinned **1.97** (rust-toolchain.toml); pre-commit hook runs `cargo fmt --all --check` + `cargo clippy --workspace --all-features -- -D warnings` — both must pass before every commit.
- Twins must stay **byte-identical**: `docs/SKILL.md` ↔ `.claude/skills/jerrycan-backend/SKILL.md` ↔ `crates/jerrycan/embedded/SKILL.md` (third pair added by Task 3). Sync with `cp`, never re-type.
- CLI contract (docs/contracts/cli-ux.md): exit codes 0 ok · 1 gate failed · 2 usage · 3 environment; `--json` = exactly one JSON document on stdout; human progress → stderr, results → stdout; workflow commands include `next_step` in JSON.
- Commits: Pavel's git identity (already configured), plain "what changed" messages, **no co-author/Claude lines ever**. PR bodies plain, no generated-with footer.
- Two branches off `main`: `feat/release-pipeline` (Task 1), `feat/onboard-cli` (Tasks 2–6). Task 7 gates and opens one PR per branch.
- Agent ids (exact strings): `claude-code`, `cursor`, `codex`, `windsurf`, `generic`.
- After any change under `docs/` or `crates/jerrycan/embedded/`, run `cargo test -p jerrycan --test embedded_sync`.

---

### Task 1: Release workflow + binstall metadata (branch `feat/release-pipeline`)

**Files:**
- Create: `.github/workflows/release.yml`
- Modify: `crates/jerrycan/Cargo.toml` (append `[package.metadata.binstall]` at end of file)

**Interfaces:**
- Produces: GitHub release assets named `jerrycan-<target>.tar.gz` + `.sha256` per target, on every `v*` tag — the URL shape row 3's `install.sh` will download (`{ repo }/releases/download/v{ version }/jerrycan-{ target }.tar.gz`).

- [ ] **Step 1: Branch**

```bash
git checkout main && git pull && git checkout -b feat/release-pipeline
```

- [ ] **Step 2: Write the workflow**

Create `.github/workflows/release.yml` with exactly:

```yaml
name: Release

# Prebuilt `jerrycan` binaries on every version tag (onboarding spec §4.1:
# taiki-e matrix). crates.io publishing stays scripts/publish.sh with its
# fail-fast eval gate — this workflow only attaches binaries + sha256 checksums
# to the GitHub release, for install.sh and cargo-binstall. Tag AFTER the
# crates.io publish succeeds, so a binary release can never lead the registry.
on:
  push:
    tags: ["v[0-9]+.[0-9]+.[0-9]+*"]

# create-gh-release / upload-rust-binary write release assets; nothing else.
permissions:
  contents: write

jobs:
  create-release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Tag must match the workspace version
        run: |
          tag="${GITHUB_REF_NAME#v}"
          ver="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
          if [ "$tag" != "$ver" ]; then
            echo "tag v$tag != workspace version $ver" >&2
            exit 1
          fi
      - uses: taiki-e/create-gh-release-action@v1
        with:
          token: ${{ secrets.GITHUB_TOKEN }}

  upload-assets:
    needs: create-release
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: aarch64-apple-darwin
            os: macos-14
          - target: x86_64-apple-darwin
            os: macos-13
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
          - target: aarch64-unknown-linux-musl
            os: ubuntu-24.04-arm
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      # Same pin discipline as ci.yml: keep in sync with rust-toolchain.toml.
      - uses: dtolnay/rust-toolchain@1.97
        with:
          targets: ${{ matrix.target }}
      - name: Install musl tools
        if: contains(matrix.target, 'musl')
        run: sudo apt-get update && sudo apt-get install -y musl-tools
      - uses: taiki-e/upload-rust-binary-action@v1
        with:
          bin: jerrycan
          target: ${{ matrix.target }}
          checksum: sha256
          token: ${{ secrets.GITHUB_TOKEN }}
```

- [ ] **Step 3: Lint the workflow YAML**

Run: `npx --yes js-yaml .github/workflows/release.yml > /dev/null && echo YAML-OK`
Expected: `YAML-OK` (js-yaml exits non-zero on parse errors).

- [ ] **Step 4: Add binstall metadata**

Append to the END of `crates/jerrycan/Cargo.toml` (after the last existing section):

```toml
# `cargo binstall jerrycan` resolves the prebuilt binary from the GitHub
# release produced by .github/workflows/release.yml (asset naming is
# taiki-e/upload-rust-binary-action's default: <bin>-<target>.tar.gz).
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz"
pkg-fmt = "tgz"
```

- [ ] **Step 5: Verify the manifest still parses and the crate builds**

Run: `cargo check -p jerrycan 2>&1 | tail -3`
Expected: `Finished` line, no errors (metadata tables are inert but must be valid TOML).

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml crates/jerrycan/Cargo.toml
git commit -m "release: tag-triggered binary matrix (mac arm64/x64, linux x64/arm64 musl) + cargo-binstall metadata"
```

**Note for the PR body (fail loud):** this workflow's first live proof is the next version tag; it cannot be exercised hermetically from a PR. The tag-vs-version guard and YAML lint are the pre-merge verification.

---

### Task 2: SKILL.md entry-path branching + Phase 1c (branch `feat/onboard-cli`)

**Files:**
- Modify: `docs/SKILL.md` (Phase 1 opening, ~line 51; new Phase 1c after Phase 1b, after ~line 110)
- Modify: `.claude/skills/jerrycan-backend/SKILL.md` (cp twin)

**Interfaces:**
- Produces: the exact strings `## Entry path` content and `### Phase 1c — Migrating from Supabase` heading that Task 4's tests assert on.

- [ ] **Step 1: Branch**

```bash
git checkout main && git pull && git checkout -b feat/onboard-cli
```

- [ ] **Step 2: Insert the entry-path block**

In `docs/SKILL.md`, Phase 1 currently opens (lines 50–52):

```markdown
Goal: a precise list of **resources, operations, actors, and cross-cutting needs**.
Ask **one question at a time**, prefer multiple-choice. Cover, in roughly this order
(skip what's already obvious from the request):
```

Immediately AFTER that paragraph and BEFORE the `- **The product in one sentence.**` bullet, insert:

```markdown
- **Entry path — settle this before anything else.** Ask which of these it is:
  **(a) Build the backend for an existing project/frontend** in this workspace →
  work the questions below, then Phase 1b derives the contract from the code.
  **(b) Start from scratch** → the questions below are the whole elicitation.
  **(c) Migrate from Supabase** → jump to Phase 1c; the migrator replaces
  Phases 3–4. **(d) Migrate from another backend/framework** → scope wall:
  only Supabase migration is automated today. Say so plainly, and offer
  (a)/(b) with the old system as reference material instead.
```

- [ ] **Step 3: Add Phase 1c**

In `docs/SKILL.md`, after the Phase 1b section's closing blockquote (`> ... becomes a hand-written `Json<Value>` handler (see Phase 2 + the gotchas).`) and BEFORE `## Phase 2 — Scope check`, insert:

```markdown
### Phase 1c — Migrating from Supabase (entry path (c) only)

The migrator does Phases 3–4 for you — it authors the design AND scaffolds the
app deterministically from the Supabase project. Your job is the gaps.

- **Read `jerrycan docs migrate-supabase` now** — the complete reference for
  the export layout, what translates, and what becomes a gap item.
- With the user, produce the **offline export** (schema.sql, per-table CSVs,
  storage/buckets.json, auth users + identities) exactly as that page
  prescribes. `--live` is opt-in and the user's explicit call — never in CI.
- Run `jerrycan migrate --from supabase <export-dir>` (`--out`/`--name` if the
  user wants a specific target). It emits the app, a resumable data seed,
  `gap-report.json`, and `MIGRATION.md`.
- **Walk `gap-report.json` with the user item by item.** Each gap is something
  the migrator refused to guess (unrecognized RLS, plpgsql/Edge bodies, exotic
  types). Decide per item: hand-write it in a handler (Phase 5), descope, or
  keep it external.
- Surface `MIGRATION.md`'s **secret-rotation checklist** before anything runs:
  no Supabase secret was copied; the user must set fresh values.
- Rejoin the loop at Phase 4's tail: `jerrycan gen-tests --module <m>` per
  module, `jerrycan db migrate` + `jerrycan db seed` against the target
  database, then Phase 5 with the gap list as the implementation queue. Skip
  Phases 3–4 (design and scaffold already exist); a Supabase migration's auth
  model is always `jwt`.
```

- [ ] **Step 4: Sync the twin and verify byte-identity**

Run:
```bash
cp docs/SKILL.md .claude/skills/jerrycan-backend/SKILL.md
diff docs/SKILL.md .claude/skills/jerrycan-backend/SKILL.md && echo TWINS-OK
```
Expected: `TWINS-OK`.

- [ ] **Step 5: Commit**

```bash
git add docs/SKILL.md .claude/skills/jerrycan-backend/SKILL.md
git commit -m "skill: explicit entry-path branching (existing project / scratch / supabase migration) + Phase 1c migration runbook"
```

---

### Task 3: Embedded SKILL.md twin + sync tripwire (branch `feat/onboard-cli`)

**Files:**
- Modify: `crates/jerrycan/tests/embedded_sync.rs` (add one pair after the mcp-tools pair, ~line 26)
- Create: `crates/jerrycan/embedded/SKILL.md` (by `cp`, never by hand)

**Interfaces:**
- Produces: `crates/jerrycan/embedded/SKILL.md` — the file Task 4's `include_str!` reads.

- [ ] **Step 1: Write the failing tripwire first**

In `crates/jerrycan/tests/embedded_sync.rs`, after the `mcp-tools.json` pair push:

```rust
    pairs.push((
        "docs/contracts/mcp-tools.json".into(),
        "embedded/contracts/mcp-tools.json".into(),
    ));
```

add:

```rust
    pairs.push(("docs/SKILL.md".into(), "embedded/SKILL.md".into()));
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p jerrycan --test embedded_sync`
Expected: FAIL with `embedded copy is stale: cp docs/SKILL.md crates/jerrycan/embedded/SKILL.md`.

- [ ] **Step 3: Create the copy exactly as the failure instructs**

```bash
cp docs/SKILL.md crates/jerrycan/embedded/SKILL.md
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p jerrycan --test embedded_sync`
Expected: PASS (1 passed).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/tests/embedded_sync.rs crates/jerrycan/embedded/SKILL.md
git commit -m "embed SKILL.md in the jerrycan crate, tripwired byte-identical to docs/SKILL.md"
```

---

### Task 4: `jerrycan onboard` prints the runbook (branch `feat/onboard-cli`)

**Files:**
- Create: `crates/jerrycan/src/onboard.rs` (bin-private module — main.rs declares it; the lib does NOT)
- Modify: `crates/jerrycan/src/main.rs` (`mod onboard;` after the `use` block ~line 9; new `Cmd::Onboard` variant after `Migrate { .. }` ~line 136; dispatch arm after `Cmd::Migrate` ~line 279; `fn cmd_onboard` near `cmd_docs` ~line 284)
- Test: `crates/jerrycan/tests/cli.rs` (append)

**Interfaces:**
- Consumes: `crates/jerrycan/embedded/SKILL.md` from Task 3; heading strings from Task 2.
- Produces: `onboard::runbook() -> &'static str` (frontmatter-stripped skill markdown); `Cmd::Onboard { emit_skill: bool, agent: Option<String>, dir: Option<PathBuf> }`; `fn cmd_onboard(emit_skill: bool, agent: Option<&str>, dir: Option<PathBuf>, json_mode: bool) -> Result<(), Failure>`. Task 5 extends both.

- [ ] **Step 1: Write failing CLI tests**

Append to `crates/jerrycan/tests/cli.rs`:

```rust
#[test]
fn onboard_prints_the_runbook_without_frontmatter() {
    let out = jerrycan().arg("onboard").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("# Building a backend with jerrycan"),
        "runbook must start at the H1, not YAML frontmatter: {}",
        &stdout[..stdout.len().min(80)]
    );
    assert!(stdout.contains("Entry path"), "3-way entry branching missing");
    assert!(
        stdout.contains("Phase 1c — Migrating from Supabase"),
        "migration phase missing"
    );
}

#[test]
fn onboard_json_is_one_document_with_next_step() {
    let out = jerrycan().args(["--json", "onboard"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["markdown"].as_str().unwrap().contains("Entry path"));
    assert!(v["next_step"].as_str().is_some());
}
```

(If `serde_json` is not already in `[dev-dependencies]` of `crates/jerrycan/Cargo.toml`, add `serde_json = "1"` there — check first: `grep -n serde_json crates/jerrycan/Cargo.toml`.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p jerrycan --test cli onboard`
Expected: FAIL — clap rejects the unknown `onboard` subcommand (exit 2, `status.success()` false).

- [ ] **Step 3: Implement the module**

Create `crates/jerrycan/src/onboard.rs`:

```rust
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
```

- [ ] **Step 4: Wire the subcommand**

In `crates/jerrycan/src/main.rs`:

(a) after the `use std::path::{Path, PathBuf};` line add:

```rust
mod onboard;
```

(b) in `enum Cmd`, after the `Migrate { .. }` variant and before `/// Serve MCP over stdio`:

```rust
    /// Print the guided build runbook (design → scaffold → implement → check)
    Onboard {
        /// Write the skill/rules files for an agent instead of printing
        #[arg(long, requires = "agent")]
        emit_skill: bool,
        /// Target agent: claude-code | cursor | codex | windsurf | generic
        #[arg(long)]
        agent: Option<String>,
        /// Directory for project-level files (default: current directory)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
```

(c) in the dispatch `match`, after the `Cmd::Migrate { .. } => …` arm:

```rust
        Cmd::Onboard {
            emit_skill,
            agent,
            dir,
        } => cmd_onboard(emit_skill, agent.as_deref(), dir, cli.json),
```

(d) next to `cmd_docs`, add (Task 5 replaces the `emit_skill` stub arm):

```rust
fn cmd_onboard(
    emit_skill: bool,
    agent: Option<&str>,
    _dir: Option<PathBuf>,
    json_mode: bool,
) -> Result<(), Failure> {
    if emit_skill {
        let _ = agent;
        return Err(Failure::usage("--emit-skill lands in the next commit"));
    }
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "markdown": onboard::runbook(),
                "next_step": "follow the runbook phases in order, starting with the entry-path question",
            })
        );
    } else {
        println!("{}", onboard::runbook());
    }
    Ok(())
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p jerrycan --test cli onboard && cargo test -p jerrycan --bin jerrycan`
Expected: both PASS (2 cli tests + 2 unit tests).

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/src/onboard.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/cli.rs
git commit -m "cli: jerrycan onboard prints the embedded guided runbook (frontmatter-stripped skill)"
```

---

### Task 5: `--emit-skill --agent <id>` (branch `feat/onboard-cli`)

**Files:**
- Modify: `crates/jerrycan/src/onboard.rs` (agent enum + emit)
- Modify: `crates/jerrycan/src/main.rs` (`cmd_onboard` emit arm)
- Test: unit tests in `onboard.rs` + one CLI test in `crates/jerrycan/tests/cli.rs`

**Interfaces:**
- Consumes: `SKILL_MD`, `runbook()` from Task 4.
- Produces: `onboard::Agent` (`FromStr`, ids `claude-code|cursor|codex|windsurf|generic`); `onboard::emit_skill(agent: Agent, project_dir: &std::path::Path, home_dir: &std::path::Path) -> std::io::Result<Emitted>` with `pub struct Emitted { pub written: Vec<std::path::PathBuf>, pub unchanged: Vec<std::path::PathBuf>, pub instructions: Option<String> }`. Row 3's `install.sh` will call `jerrycan onboard --emit-skill --agent <id>`.

- [ ] **Step 1: Write failing unit tests**

Append inside `mod tests` in `crates/jerrycan/src/onboard.rs` (uses `tempfile`, already a workspace dev-dependency — confirm with `grep -rn tempfile crates/jerrycan/Cargo.toml`, add `tempfile = "3"` to `[dev-dependencies]` if absent):

```rust
    #[test]
    fn claude_code_emit_writes_the_skill_verbatim_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        let first = emit_skill(Agent::ClaudeCode, proj.path(), home.path()).unwrap();
        let path = home
            .path()
            .join(".claude/skills/jerrycan-backend/SKILL.md");
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
        assert!(agents.starts_with("# Mine\n\nkeep me\n"), "foreign content clobbered");
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p jerrycan --bin jerrycan`
Expected: COMPILE FAIL — `Agent`, `emit_skill`, `Emitted` not defined.

- [ ] **Step 3: Implement**

Add to `crates/jerrycan/src/onboard.rs` (above `mod tests`):

```rust
use std::path::{Path, PathBuf};

const MARKER_START: &str = "<!-- jerrycan-backend:start -->";
const MARKER_END: &str = "<!-- jerrycan-backend:end -->";
const MCP_SNIPPET: &str = r#"{ "mcpServers": { "jerrycan": { "command": "jerrycan", "args": ["mcp"] } } }"#;

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
            out.instructions = Some(format!("MCP: add this stdio server to {hint}:\n{MCP_SNIPPET}"));
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
```

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p jerrycan --bin jerrycan`
Expected: PASS (6 unit tests).

- [ ] **Step 5: Wire the emit arm + a CLI test**

In `main.rs`, replace the whole `if emit_skill { … }` stub block in `cmd_onboard` with:

```rust
    if emit_skill {
        let agent: onboard::Agent = agent
            .expect("clap `requires` guarantees --agent")
            .parse()
            .map_err(Failure::usage)?;
        let project_dir = dir.unwrap_or_else(|| PathBuf::from("."));
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Failure::environment("HOME is not set"))?;
        let out = onboard::emit_skill(agent, &project_dir, &home)
            .map_err(|e| Failure::environment(format!("emit-skill: {e}")))?;
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    // PathBuf → display strings: independent of serde feature flags.
                    "written": out.written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "unchanged": out.unchanged.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "instructions": out.instructions,
                    "next_step": "run `jerrycan onboard` and follow the runbook",
                })
            );
        } else {
            for p in &out.written {
                println!("wrote {}", p.display());
            }
            for p in &out.unchanged {
                println!("unchanged {}", p.display());
            }
            if let Some(i) = &out.instructions {
                println!("{i}");
            }
        }
        return Ok(());
    }
```

(Keep `_dir` renamed to `dir` in the signature now that it's used. If `Failure::usage`/`Failure::environment` take `String` vs `&str` differently than shown, match the call style used at `main.rs:327` and `main.rs:280`.)

Append to `crates/jerrycan/tests/cli.rs`:

```rust
#[test]
fn onboard_emit_skill_claude_code_writes_under_home() {
    let home = tempfile::tempdir().unwrap();
    let out = jerrycan()
        .env("HOME", home.path())
        .args(["--json", "onboard", "--emit-skill", "--agent", "claude-code"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["written"].as_array().unwrap().len(), 1);
    assert!(
        home.path()
            .join(".claude/skills/jerrycan-backend/SKILL.md")
            .exists()
    );
}

#[test]
fn onboard_emit_skill_without_agent_is_usage_error() {
    let out = jerrycan().args(["onboard", "--emit-skill"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn onboard_emit_skill_unknown_agent_is_usage_error_naming_ids() {
    let out = jerrycan()
        .args(["onboard", "--emit-skill", "--agent", "zed"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("claude-code"));
}
```

(`tempfile` in cli.rs needs it under `[dev-dependencies]` — same check as Step 1.)

- [ ] **Step 6: Run all onboard tests**

Run: `cargo test -p jerrycan --test cli onboard && cargo test -p jerrycan --bin jerrycan`
Expected: PASS (5 cli + 6 unit).

- [ ] **Step 7: Commit**

```bash
git add crates/jerrycan/src/onboard.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/cli.rs crates/jerrycan/Cargo.toml
git commit -m "cli: onboard --emit-skill --agent claude-code|cursor|codex|windsurf|generic (skill file / AGENTS.md marker block / printed instructions)"
```

---

### Task 6: CLI contract row + CHANGELOG (branch `feat/onboard-cli`)

**Files:**
- Modify: `docs/contracts/cli-ux.md` (commands table, after the `jerrycan docs` row)
- Modify: `CHANGELOG.md` (read its existing format first; add entries under the unreleased/next section following that format exactly)

**Interfaces:**
- Consumes: flag surface exactly as shipped in Tasks 4–5.

- [ ] **Step 1: Add the contract row**

In the `## Commands` table of `docs/contracts/cli-ux.md`, after the `jerrycan docs` row:

```markdown
| `jerrycan onboard` | `--emit-skill --agent <claude-code\|cursor\|codex\|windsurf\|generic>` `--dir <d>` | Print the guided build runbook (embedded SKILL.md, frontmatter stripped); with `--emit-skill`, write the agent's skill/rules files (claude-code → `~/.claude/skills/jerrycan-backend/`, cursor/codex/windsurf → `AGENTS.md` marker block, generic → print only) | — |
```

- [ ] **Step 2: CHANGELOG entry**

Read `CHANGELOG.md`'s head; under its current unreleased/next heading (create one matching the existing style if absent) add, in its list style:

```markdown
- `jerrycan onboard`: prints the guided build runbook; `--emit-skill --agent <id>` installs the jerrycan-backend skill for claude-code / cursor / codex / windsurf / generic.
- Skill: explicit entry-path branching (existing project / from scratch / migrate from Supabase) + Phase 1c Supabase-migration runbook.
- Release: tag-triggered prebuilt binaries (macOS arm64/x64, Linux x64/arm64 musl) + `cargo binstall jerrycan` support.
```

- [ ] **Step 3: Contract tests still green**

Run: `cargo test -p jerrycan --test contracts --test contract_compat --test embedded_sync`
Expected: PASS (cli-ux.md is not embedded; this proves no tripwire regressed).

- [ ] **Step 4: Commit**

```bash
git add docs/contracts/cli-ux.md CHANGELOG.md
git commit -m "contract+changelog: jerrycan onboard surface"
```

---

### Task 7: Gate and open the two PRs

- [ ] **Step 1: Full gate on `feat/onboard-cli`**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p jerrycan --all-features
RUSTDOCFLAGS='-D warnings' cargo doc -p jerrycan --no-deps
```
Expected: all clean. (The doc gate is a known trap — run it, don't assume.)

- [ ] **Step 2: Gate on `feat/release-pipeline`**

```bash
git checkout feat/release-pipeline && cargo fmt --all --check && cargo check -p jerrycan
```
Expected: clean (workflow YAML + inert metadata only).

- [ ] **Step 3: Push and open PRs**

```bash
git push -u origin feat/release-pipeline
gh pr create --head feat/release-pipeline --title "Release pipeline: prebuilt binaries + cargo-binstall (onboarding spec §4.1)" --body "Tag-triggered taiki-e matrix (mac arm64/x64, linux x64/arm64 musl), sha256 checksums, tag-vs-version guard. First live proof = next version tag (cannot be exercised from a PR). Spec: docs/superpowers/specs/2026-07-19-onboarding-design.md §4.1, plan row 1."
git checkout feat/onboard-cli && git push -u origin feat/onboard-cli
gh pr create --head feat/onboard-cli --title "jerrycan onboard: embedded guided runbook + --emit-skill per agent (onboarding spec §4.4)" --body "Bin-private module (no lib API, no semver impact). SKILL.md gains entry-path branching + Phase 1c Supabase migration; embedded twin tripwired. Spec §4.2 step 3 + §4.4, plan rows described in docs/superpowers/plans/2026-07-20-onboarding-rows-1-2.md."
```

- [ ] **Step 4: Report**

State plainly: tests run + counts, gates run, PR URLs, and that the release workflow's live proof is deferred to the next tag.
