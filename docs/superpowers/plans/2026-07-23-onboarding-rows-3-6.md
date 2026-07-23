# Onboarding Rows 3–6 Implementation Plan (+ release-matrix repair)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Task 0 is controller-inline (surgical CI repair) with a reviewer pass before PR.

**Goal:** Ship spec §5 rows 3–6 of `docs/superpowers/specs/2026-07-19-onboarding-design.md` (`install.sh`, `/start` runbook source, README overhaul, onboarding eval) and repair the two failed release-matrix legs discovered in the v0.6.0–v0.6.2 live runs.

**Context (2026-07-23):** workspace 0.6.2; releases v0.6.0–v0.6.2 exist with only 2/4 binary assets (`aarch64-apple-darwin`, `x86_64-unknown-linux-musl`). `jerrycan onboard --emit-skill --agent <id>` shipped in #135. jerrycan.cc hosting is wrangler/Cloudflare; site source location still unpinned — row 4 delivers the in-repo source of truth only.

## Global Constraints

- Toolchain 1.97; pre-commit fmt+clippy gate; commits plain, no co-author/AI lines; PR bodies plain.
- `install.sh`: POSIX-ish bash, `set -euo pipefail`, shellcheck-clean; idempotent; never overwrites foreign config wholesale; `--json` = one JSON doc on stdout.
- Asset naming consumed: `jerrycan-<target>.tar.gz` + `.sha256` from `https://github.com/backant-io/jerrycan/releases/download/v<version>/`.
- Agent ids: `claude-code|cursor|codex|windsurf|generic`; non-TTY + no `--agent` → `generic`.
- Windows: refuse with a WSL pointer (Darwin/Linux only).
- Env overrides for hermetic testing: `JERRYCAN_INSTALL_BASE_URL` (default the GH releases URL), `JERRYCAN_INSTALL_VERSION` (default: latest release tag via GitHub API), `JERRYCAN_INSTALL_DIR` (default `~/.jerrycan/bin`), `JERRYCAN_NO_MODIFY_PATH=1`, `JERRYCAN_NO_RUSTUP=1`.

## Task 0 — Release-matrix repair (branch `fix/release-matrix`, controller-inline + reviewer)

`.github/workflows/release.yml`:
- `x86_64-apple-darwin`: `os: macos-14` (macos-13 retired — leg queued 24h then cancelled), cargo cross-compile.
- All four legs: explicit `build-tool: cargo` (the action's cross-detection picked `cross` on the arm runner and tried an x86_64 toolchain).
- `timeout-minutes: 45` on upload-assets, `timeout-minutes: 10` on create-release.
- New `workflow_dispatch` with required input `tag`; `create-release` job runs only on tag push; `upload-assets` checks out `refs/tags/${{ inputs.tag }}` on dispatch and passes `ref: refs/tags/<tag>` to the upload action, so a partial release can be backfilled.
- Verify: js-yaml parse; PR; merge; then dispatch for `v0.6.2` → all 4 assets present on that release (live proof of the repair).

## Task 1 — `scripts/install.sh` (branch `feat/install-script`)

Flags: `--agent <ids>` (comma-separated), `--json`, `--dir <project-dir>` (forwarded to emit-skill), `-h/--help`. Steps: platform detect (Darwin/Linux × x86_64/arm64 → the 4 targets; anything else → cargo-install fallback if cargo exists, else fail with message); resolve version (env override, else GitHub API `releases/latest`); download tarball + `.sha256` to a mktemp dir; verify checksum (`shasum -a 256`/`sha256sum`); install to `JERRYCAN_INSTALL_DIR`; PATH: append one marker-guarded line (`# jerrycan installer`) to `~/.zshrc`/`~/.bashrc` (existing marker → skip) unless `JERRYCAN_NO_MODIFY_PATH=1`; rustup bootstrap (`--default-toolchain stable -y`) only when cargo absent and not `JERRYCAN_NO_RUSTUP=1`; per-agent wiring by shelling `"$INSTALL_DIR/jerrycan" onboard --emit-skill --agent <id>` per id (claude-code additionally best-effort `claude mcp add jerrycan -- "$INSTALL_DIR/jerrycan" mcp` when `claude` exists, skip-if-present); final summary — human lines on stderr, `--json` one doc on stdout: `{ok, version, target, bin, agents:[...], path_modified, rustup_bootstrapped, next_step:"run `jerrycan onboard` and follow it"}`.

Hermetic self-test `scripts/install-test.sh` (bash, not run in per-PR CI; heavy + local): builds a stub tarball (tiny sh script named `jerrycan` answering `--version`/`onboard --emit-skill --agent`/`onboard`), serves it from a temp dir via `python3 -m http.server`, runs `install.sh` with all env overrides + `--agent generic --json` into a temp HOME, asserts: exit 0, JSON parses (`python3 -c json.load`), binary on disk + executable, second run idempotent (no duplicate PATH line), checksum-tamper run fails non-zero. shellcheck both scripts.

## Task 2 — `/start` runbook source (branch `feat/install-script`)

`docs/start.md` (agent-readable, served later at jerrycan.cc/start; wrangler wiring deferred until the site source is pinned):
1. identify your agent id (claude-code|cursor|codex|windsurf|generic); on Windows, enter WSL first — native Windows unsupported;
2. `curl -fsSL https://jerrycan.cc/install.sh | sh -s -- --agent <your-id>` (mirror: raw.githubusercontent.com/backant-io/jerrycan/main/scripts/install.sh);
3. `jerrycan onboard` and follow it exactly.
Plus the pasteable one-liner at top: `Fetch https://jerrycan.cc/start and follow it to set up jerrycan and build my backend.`

## Task 3 — onboarding eval in heavy.yml (branch `feat/install-script`)

New independent job `onboarding-eval` in `.github/workflows/heavy.yml` (same `workflow_dispatch` trigger): job 1 builds the real release-profile CLI + packs `jerrycan-x86_64-unknown-linux-musl.tar.gz` + sha256 (musl, same naming as releases), uploads artifact; job 2 `container: debian:bookworm-slim` (no Rust, no cargo) downloads artifact, serves it locally, runs `scripts/install.sh --agent generic --json` with env overrides + `JERRYCAN_NO_RUSTUP=1`, asserts JSON ok + `jerrycan --version`, `jerrycan onboard` prints the runbook (grep "Entry path"), `--emit-skill --agent generic` exits 0, then `jerrycan new eval-app --design` the embedded todo-api design (from `crates/jerrycan/embedded/designs/todo-api.design.json`) — scaffold must succeed; full `check` needs cargo, so the container asserts scaffold + explains; a follow-on step on the host runner (with rustup) runs `jerrycan check` inside the scaffolded app for the green gate.

## Task 4 — README overhaul (branch `feat/readme-onboarding`)

Per spec §4.6: hero = the pasteable one-liner + `curl | sh` alternative; "For your agent" quickstart becomes install.sh + `jerrycan onboard` (cargo install demoted to "from source"); fix staleness: version callout (drop the hardcoded number — say "published on crates.io, prebuilt binaries on GitHub Releases"), rust badge 1.88→1.97, crate tree lists all 11 crates (add jerrycan-realtime, jerrycan-storage), "Also shipping (contract v2)" keeps current claims; structure per spec (hero → agent onboarding → features → proof → human quickstart → scope → roadmap → sponsors). Check `crates/jerrycan/README.md` (crates.io page): update its install section to match (it is a separate shorter file, not a twin — keep it consistent, not identical). `docs/marketing/jerrycan-cc-design-handoff.md`: one-line note that the CTA is superseded by the one-liner + install.sh (spec §4.6 Rule-7 conflict).

## Task 5 — Gates + PRs

fmt/clippy/tests + shellcheck; `cargo test -p jerrycan --test embedded_sync` (README not embedded — but confirm); PRs: `fix/release-matrix` first (then dispatch-backfill v0.6.2), then `feat/install-script`, then `feat/readme-onboarding` (README references install.sh, so lands last). Plain bodies.
