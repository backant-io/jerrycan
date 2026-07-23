# jerrycan

**The AI-native Rust backend platform.**

`jerrycan` is two inseparable halves shipped as one crate:

1. **A backend framework** — a ground-up rewrite of the Flask/Werkzeug concept
   space in Rust: a lean core, async-only on tokio + hyper, trait-based
   extensions (`db`, `auth`, `validate`, `observe`), secure by default.
2. **A generation platform** — one `jerrycan` binary that is both a CLI and an
   MCP server, through which AI agents **design → generate test-first → verify →
   package** complete, deployable backends.

## Install

The intended path: point your coding agent at jerrycan with one line. It
installs the CLI, wires jerrycan into the agent (MCP), and leaves a guided
runbook behind:

```sh
curl -fsSL https://jerrycan.cc/install.sh | bash -s -- --agent claude-code
```

Agent ids: `claude-code` · `cursor` · `codex` · `windsurf` · `generic`.

Prefer to install the CLI / MCP server directly:

```sh
cargo binstall jerrycan   # prebuilt binaries
cargo install jerrycan    # or build from source
```

Or add the framework to a Rust app:

```sh
cargo add jerrycan --features db,auth,validate,observe
```

## Learn more

- Homepage: <https://jerrycan.cc>
- Source, docs, and the full design spec:
  <https://github.com/backant-io/jerrycan>
- AI-native docs ship in the binary: `jerrycan docs`

Licensed under MIT.
