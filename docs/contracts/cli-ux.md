# jerrycan CLI — UX Specification (contract v0)

One binary, two audiences: humans debugging, agents working. Every command has
a `--json` mode whose output is the same data the MCP tool returns.

## Global conventions

- **Output:** human-readable progress → stderr; results → stdout.
  With `--json`: stdout is exactly one JSON document matching the MCP tool's
  outputSchema (docs/contracts/mcp-tools.json); stderr stays human.
- **`next_step`:** every workflow command's JSON output includes `next_step`,
  the golden-path hint (e.g. after `new` → "run jerrycan gen-tests --module todos").
- **Exit codes:** `0` success · `1` the gate failed (check/test failures, conflicts) ·
  `2` usage error (unknown flag, missing arg) · `3` environment error (no cargo, no git).
- **Color:** auto (TTY only); `NO_COLOR` honored.
- **Env:** `JERRYCAN_ADDR` (serve bind), `JERRYCAN_ENV=dev|prod` (error verbosity; prod is the default when packaged).
- **Exit 3 and stdout:** on environment errors (exit 3) stdout is empty — agents must branch on the exit code before parsing stdout. With `--module`, audit/deny are skipped (workspace-global gates); run a full check before packaging.

## Commands (v0 surface — mirrors spec §7.1)

| Command | Args/Flags | Behavior | MCP twin |
|---|---|---|---|
| `jerrycan new <name>` | `--design <file>` (required) | Scaffold workspace from validated design: `app/`, `shared/`, one route crate per module | jerrycan_scaffold |
| `jerrycan generate route <path>` | alias `g`; `<path>`=`todos` or `todos/comments` | New module crate or subroute; rewires mounting + workspace deterministically; emits failing tests | jerrycan_generate |
| `jerrycan generate dep <name>` | `--module <m>` (required) | Module-scoped dependency stub (factory fn + registration) | jerrycan_generate |
| `jerrycan gen-tests` | `--module <m>` (required) | Failing acceptance tests from the module's design slice | jerrycan_gen_tests |
| `jerrycan list routes` | `--json` | Route tree: METHOD path → module::handler | jerrycan_list_routes |
| `jerrycan dev` | `--addr <a>` | Run with auto-reload (debounced rebuild) | — |
| `jerrycan check` | `--module <m>` | build → clippy(-D warnings) → cargo-audit → cargo-deny → tests → jerrycan lints; first failure class reported, all diagnostics collected | jerrycan_check |
| `jerrycan test` | `--module <m>` | The app's test suite only (subset of check) | — |
| `jerrycan package` | `--docker\|--binary\|--k8s\|--systemd` | Hardened artifact + CycloneDX SBOM; refuses unless full check is green | jerrycan_package |
| `jerrycan docs <topic>` | `--search <q>` | Render docs page in terminal / search | jerrycan_docs_get / _search |
| `jerrycan add <extension>` | `db` or `validate` | Wire an extension: flips the design dependency, regenerates mounting + policy files | — |
| `jerrycan db migrate` | `--url <db-url>` (or env) | Apply module-owned migrations via the tracking-table runner | — |
| `jerrycan mcp` | | Serve MCP over stdio (Phase 1) | — |

## Diagnostics format (check, human mode)

```
error[JC0405]: mutating route without auth guard
  --> crates/routes/todos/src/lib.rs:14
   = note: POST /todos has no Dep<…> guard and auth.model = "session"
   = help: add a guard dependency, e.g. `_user: Dep<CurrentUser>`
   = docs: jerrycan docs dependencies#guards
```

Same payload in `--json`: `{code, file, line, message, suggestion, doc_url}` —
identical to the MCP outputSchema. One diagnostics pipeline, two renderings.

## Non-goals (v0)

- No interactive prompts, ever — agents can't answer TTY prompts. Missing
  input = exit 2 with the exact flag to provide.
- No telemetry.
- No deploy execution (`kubectl`, ssh) — `package` ends at artifacts + SBOM.
