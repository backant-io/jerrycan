# Set up jerrycan and build my backend

**Paste into any AI agent:**

> Fetch https://jerrycan.cc/start and follow it to set up jerrycan and build my backend.

If you are the agent that just fetched this, do these three steps in order.

1. **Identify your agent id** — one of `claude-code`, `cursor`, `codex`, `windsurf`,
   `generic` (use `generic` if none of the others fit). On Windows, install and
   enter **WSL** first and run everything inside it — native Windows is not supported.

2. **Install jerrycan** (installs the CLI, a Rust toolchain if one is missing, and
   the guided skill for your agent):

   ```sh
   curl -fsSL https://jerrycan.cc/install.sh | bash -s -- --agent <your-id>
   ```

   Mirror, if that host is unreachable:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/backant-io/jerrycan/main/scripts/install.sh | bash -s -- --agent <your-id>
   ```

3. **Run the guided build** and follow its output exactly:

   ```sh
   jerrycan onboard
   ```

That is the whole runbook. Everything else — the questions to ask, the design,
the build phases — lives inside `jerrycan onboard`, versioned with the CLI, so
this page never goes stale.
