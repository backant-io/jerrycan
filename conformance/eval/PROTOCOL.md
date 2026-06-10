# Phase 4 agent eval — protocol

Goal: measure whether jerrycan's **docs + CLI/MCP alone** are sufficient for an
agent that has never seen the framework internals to build the reference backend
apps. The spec's success metric is **≥ 90 %** (≥ 5/5; 4/5 is the floor).

## Isolation rules (what makes this a real eval)

Allowed inputs only:

- The `jerrycan` binary: CLI subcommands, `jerrycan docs <page>`, `jerrycan explain <code>`.
- The design specs in `conformance/eval/specs/*.design.json`.

Forbidden inputs:

- jerrycan's own source (`crates/*/src/**`).
- The conformance reference fixtures (`conformance/fixtures/**`, `conformance/eval/fixtures/**`).
- Any plan / design / spec markdown for the framework itself.

If you want to open framework source to learn an API — **stop** and run
`jerrycan docs <topic>` instead. Learning the API from the docs is the point.

Knowledge of the API comes only from the docs pages (`app`, `modules`,
`extractors`, `dependencies`, `errors`, `middleware`, `testing`, `database`,
`validation`, `auth`, `observability`, `packaging`, `error-codes`) and
`jerrycan explain`.

## Setup (allowed)

```sh
cargo build -p jerrycan --bin jerrycan        # produces target/debug/jerrycan
export JERRYCAN_FRAMEWORK_DEP='jerrycan = { path = "<repo>/crates/jerrycan", default-features = false }'
```

The `JERRYCAN_FRAMEWORK_DEP` env var points scaffolded `Cargo.toml`s at the
local crate (pre-publish).

## Per spec (blog, tasks, shortener, inventory, notes)

1. **Scaffold**
   `jerrycan new <tmpdir>/<spec> --design conformance/eval/specs/<spec>.design.json`
   (with the env dep set).
2. **Read the relevant docs** for the design surface
   (`jerrycan docs modules`, `extractors`, `dependencies`, `errors`, `testing`,
   plus `validation`/`auth`/`database` when the design lists those dependencies).
   Optionally run `jerrycan gen-tests --module <m>` to materialise the
   design's acceptance criteria as runnable tests in your own project (this is a
   tool output, not a forbidden fixture).
3. **Implement every generated handler stub from scratch** using only what the
   docs taught you. The generated `repo.rs` is a complete in-memory store; wire
   handlers to it (`all`/`get`/`insert`/`update`/`remove`) and map a missing id
   to `Error::not_found()` (→ `404 JC0404`). Do **not** copy conformance
   fixtures.
4. **`jerrycan check` until green.** For any diagnostic, try
   `jerrycan explain <code>` and the docs. Log any case where the docs /
   diagnostic did **not** give you enough to fix it.
5. **Run the app and verify a round-trip over raw HTTP.**
   `JERRYCAN_ADDR=127.0.0.1:<port> cargo run -p app`, then `curl` (or a
   `TcpStream`) a create → read → update → delete sequence and confirm the
   status codes and bodies match the design (incl. `404` for unknown ids).
6. **Score** `PASS` if `check` is green **and** the round-trip behaves;
   otherwise `FAIL` with the reason.

## Scoring & loop

- Record a per-spec table (check green / round-trip / result) and the overall
  pass rate `N/5` in `results.md`.
- If pass rate `< 90 %`: root-cause each failure — **docs gap** or **real
  framework bug**. For a docs gap, improve the relevant `docs/ai/*.md` page or
  `jerrycan explain` text (never the eval), note it under "Docs/diagnostics gaps
  surfaced", and re-run that spec. Loop until `≥ 90 %` or the remaining failure
  is a genuine framework limitation (then ticket it in `docs/phase1-backlog.md`
  and record honestly).

## Gates before committing

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features` (docs changes are doc-tested — keep
  them green)

## Hygiene

Run the eval in a throwaway tmpdir (e.g. `/tmp/jc-eval/<spec>`). Kill any app
processes you start and remove the tmpdirs when done.
