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

```sh
# In an app, depend on the framework facade:
cargo add jerrycan --features db,auth,validate,observe

# Install the CLI / MCP server:
cargo install jerrycan
```

## Learn more

- Homepage: <https://jerrycan.cc>
- Source, docs, and the full design spec:
  <https://github.com/backant-io/jerrycan>
- AI-native docs ship in the binary: `jerrycan docs`

Licensed under MIT OR Apache-2.0.
