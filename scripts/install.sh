#!/usr/bin/env bash
# jerrycan installer — one script, three jobs:
#   1. install the prebuilt `jerrycan` CLI (or `cargo install` fallback),
#   2. bootstrap a Rust toolchain if the machine has none (generated apps need it),
#   3. wire the guided `jerrycan-backend` skill/rules into your agent(s).
#
# It is idempotent, never rewrites foreign config, and never reimplements the
# skill: per-agent files are written by `jerrycan onboard --emit-skill`, so the
# installer and the skill can never drift.
#
# Human progress goes to stderr; with --json a single JSON document is the only
# thing printed to stdout, so an agent can machine-read the result.
#
# Env overrides (used by the hermetic self-test, harmless in production):
#   JERRYCAN_INSTALL_BASE_URL  release download base   (default: GitHub Releases)
#   JERRYCAN_INSTALL_VERSION   version/tag to install  (default: latest release)
#   JERRYCAN_INSTALL_DIR       install directory       (default: ~/.jerrycan/bin)
#   JERRYCAN_NO_MODIFY_PATH=1  do not touch a shell rc file
#   JERRYCAN_NO_RUSTUP=1       do not bootstrap rustup when cargo is absent
set -euo pipefail

REPO="backant-io/jerrycan"
DEFAULT_BASE_URL="https://github.com/${REPO}/releases/download"
API_LATEST="https://api.github.com/repos/${REPO}/releases/latest"

# --- output helpers: humans read stderr, --json owns stdout --------------------
log()  { printf '%s\n' "[jerrycan] $*" >&2; }
die()  { printf '%s\n' "[jerrycan] error: $*" >&2; exit 1; }

usage() {
  cat <<'EOF'
jerrycan installer

Usage: install.sh [options]

Options:
  --agent <ids>   Comma-separated agent ids to wire up:
                  claude-code,cursor,codex,windsurf,generic.
                  Default: prompt on a TTY, otherwise "generic".
  --dir <path>    Project directory for agent files (default: current directory).
  --json          Print a single machine-readable JSON summary on stdout.
  -h, --help      Show this help and exit.

Environment overrides:
  JERRYCAN_INSTALL_BASE_URL   Release download base URL.
  JERRYCAN_INSTALL_VERSION    Version/tag to install (default: latest release).
  JERRYCAN_INSTALL_DIR        Install directory (default: ~/.jerrycan/bin).
  JERRYCAN_NO_MODIFY_PATH=1   Do not append a PATH line to your shell rc file.
  JERRYCAN_NO_RUSTUP=1        Do not bootstrap rustup when cargo is absent.
EOF
}

usage_err() {
  printf '%s\n\n' "[jerrycan] error: $*" >&2
  usage >&2
  exit 2
}

# --- flag parsing --------------------------------------------------------------
AGENTS_CSV=""
JSON=0
PROJECT_DIR=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --agent)   [ "$#" -ge 2 ] || usage_err "--agent requires a value"; AGENTS_CSV="$2"; shift 2 ;;
    --agent=*) AGENTS_CSV="${1#*=}"; shift ;;
    --dir)     [ "$#" -ge 2 ] || usage_err "--dir requires a value"; PROJECT_DIR="$2"; shift 2 ;;
    --dir=*)   PROJECT_DIR="${1#*=}"; shift ;;
    --json)    JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *)         usage_err "unknown option: $1" ;;
  esac
done

# --- platform detection --------------------------------------------------------
# Returns a Rust target triple for the 4 published assets, or empty for
# "unsupported here" (caller then tries the cargo fallback). Windows is refused.
uname_s="$(uname -s 2>/dev/null || echo unknown)"
uname_m="$(uname -m 2>/dev/null || echo unknown)"
case "$uname_s" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    die "native Windows is not supported — install and enter WSL (Ubuntu), then re-run this inside WSL. See https://learn.microsoft.com/windows/wsl/install" ;;
esac

TARGET=""
case "$uname_s" in
  Darwin)
    case "$uname_m" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64)        TARGET="x86_64-apple-darwin" ;;
    esac ;;
  Linux)
    case "$uname_m" in
      x86_64|amd64)   TARGET="x86_64-unknown-linux-musl" ;;
      arm64|aarch64)  TARGET="aarch64-unknown-linux-musl" ;;
    esac ;;
esac

INSTALL_DIR="${JERRYCAN_INSTALL_DIR:-$HOME/.jerrycan/bin}"
PATH_MODIFIED=false
RUSTUP_BOOTSTRAPPED=false

# --- agent selection: flag, else TTY prompt, else generic ----------------------
# Resolved and validated up front so a bad --agent fails before any download.
if [ -z "$AGENTS_CSV" ]; then
  if [ -t 0 ]; then
    printf '%s' "[jerrycan] which agent(s)? [claude-code,cursor,codex,windsurf,generic] (default generic): " >&2
    read -r AGENTS_CSV || AGENTS_CSV=""
  fi
  [ -n "$AGENTS_CSV" ] || AGENTS_CSV="generic"
fi
AGENTS="$(printf '%s' "$AGENTS_CSV" | tr ',' ' ')"
for id in $AGENTS; do
  case "$id" in
    claude-code|cursor|codex|windsurf|generic) : ;;
    *) usage_err "unknown agent id: $id (expected claude-code, cursor, codex, windsurf, or generic)" ;;
  esac
done

# --- checksum tool selection ---------------------------------------------------
sha256_of() {
  # Print the lowercase hex digest (first field only) of "$1".
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "need sha256sum or shasum to verify the download"
  fi
}

fetch() {
  # fetch <url> <dest-file> — fail loudly on any HTTP or transport error.
  command -v curl >/dev/null 2>&1 || die "curl is required but was not found on PATH"
  curl -fsSL "$1" -o "$2" || die "download failed: $1"
}

# --- binary install: prebuilt asset, or cargo fallback -------------------------
BIN=""
VERSION=""
if [ -n "$TARGET" ]; then
  command -v curl >/dev/null 2>&1 || die "curl is required to download the release but was not found on PATH"

  VERSION="${JERRYCAN_INSTALL_VERSION:-}"
  if [ -z "$VERSION" ]; then
    log "resolving latest release tag…"
    api_body="$(curl -fsSL "$API_LATEST")" || die "could not query the latest release from GitHub"
    # jq may be absent on target machines — parse tag_name with grep/sed only.
    VERSION="$(printf '%s\n' "$api_body" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/' || true)"
    [ -n "$VERSION" ] || die "could not parse a release tag from the GitHub API response"
  fi
  VERSION="${VERSION#v}"   # normalize: URLs always re-add the leading v

  base_url="${JERRYCAN_INSTALL_BASE_URL:-$DEFAULT_BASE_URL}"
  base_url="${base_url%/}"
  tarball_url="${base_url}/v${VERSION}/jerrycan-${TARGET}.tar.gz"
  sha_url="${tarball_url}.sha256"

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/jerrycan-install.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  log "downloading jerrycan ${VERSION} (${TARGET})…"
  fetch "$tarball_url" "$tmp/jerrycan.tar.gz"
  fetch "$sha_url"     "$tmp/jerrycan.tar.gz.sha256"

  log "verifying checksum…"
  computed="$(sha256_of "$tmp/jerrycan.tar.gz")"
  expected="$(awk '{print $1}' "$tmp/jerrycan.tar.gz.sha256")"
  [ -n "$expected" ] || die "checksum file was empty: $sha_url"
  if [ "$computed" != "$expected" ]; then
    die "checksum mismatch — refusing to install (expected $expected, got $computed)"
  fi

  log "extracting…"
  tar -xzf "$tmp/jerrycan.tar.gz" -C "$tmp"
  [ -f "$tmp/jerrycan" ] || die "tarball did not contain a 'jerrycan' binary at its root"

  mkdir -p "$INSTALL_DIR"
  cp "$tmp/jerrycan" "$INSTALL_DIR/jerrycan"
  chmod 755 "$INSTALL_DIR/jerrycan"
  BIN="$INSTALL_DIR/jerrycan"
  log "installed $BIN"
else
  # Unsupported platform: prebuilt asset is not published for this OS/arch.
  if command -v cargo >/dev/null 2>&1; then
    log "no prebuilt binary for ${uname_s}/${uname_m} — falling back to 'cargo install jerrycan'…"
    if [ -n "${JERRYCAN_INSTALL_VERSION:-}" ]; then
      VERSION="${JERRYCAN_INSTALL_VERSION#v}"
      cargo install jerrycan --version "$VERSION" 1>&2 || die "cargo install jerrycan failed"
    else
      VERSION="latest"
      cargo install jerrycan 1>&2 || die "cargo install jerrycan failed"
    fi
    TARGET="cargo-install"
    BIN="${CARGO_HOME:-$HOME/.cargo}/bin/jerrycan"
    INSTALL_DIR="$(dirname "$BIN")"
    log "installed $BIN"
  else
    die "no prebuilt jerrycan binary for ${uname_s}/${uname_m}, and cargo is not installed. Install Rust from https://rustup.rs then re-run, or open an issue for a native build."
  fi
fi

# --- PATH wiring: one marker-guarded line, never duplicated --------------------
if [ "$TARGET" != "cargo-install" ] && [ "${JERRYCAN_NO_MODIFY_PATH:-0}" != "1" ]; then
  case "${SHELL:-}" in
    */zsh)  rc="$HOME/.zshrc" ;;
    */bash) rc="$HOME/.bashrc" ;;
    *)      rc="$HOME/.profile" ;;
  esac
  marker="# jerrycan installer"
  if [ -f "$rc" ] && grep -qF "$marker" "$rc" 2>/dev/null; then
    log "PATH already wired in $rc — leaving it untouched"
  else
    {
      printf '\n%s\n' "$marker"
      # SC2016: $PATH must land in the rc file LITERALLY, expanded at shell start.
      # shellcheck disable=SC2016
      printf 'export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    } >>"$rc"
    PATH_MODIFIED=true
    log "added $INSTALL_DIR to PATH in $rc — open a new shell or 'source $rc'"
  fi
fi

# --- toolchain: generated apps are cargo workspaces, so they need cargo --------
if ! command -v cargo >/dev/null 2>&1 && [ "${JERRYCAN_NO_RUSTUP:-0}" != "1" ]; then
  command -v curl >/dev/null 2>&1 || die "curl is required to bootstrap rustup but was not found on PATH"
  log "cargo not found — bootstrapping rustup (stable toolchain, non-interactive)…"
  curl -fsSL https://sh.rustup.rs | sh -s -- --default-toolchain stable -y 1>&2 || die "rustup bootstrap failed"
  RUSTUP_BOOTSTRAPPED=true
fi

# --- per-agent wiring: delegate to the CLI, never reimplement it ---------------
for id in $AGENTS; do
  log "wiring agent: $id"
  if [ -n "$PROJECT_DIR" ]; then
    "$BIN" onboard --emit-skill --agent "$id" --dir "$PROJECT_DIR" 1>&2 || die "emit-skill failed for $id"
  else
    "$BIN" onboard --emit-skill --agent "$id" 1>&2 || die "emit-skill failed for $id"
  fi
  # Claude Code additionally gets the MCP server registered, best-effort.
  if [ "$id" = "claude-code" ] && command -v claude >/dev/null 2>&1; then
    if claude mcp get jerrycan >/dev/null 2>&1; then
      log "claude MCP server 'jerrycan' already present — skipping"
    elif claude mcp add jerrycan -- "$BIN" mcp >/dev/null 2>&1; then
      log "registered claude MCP server 'jerrycan'"
    else
      log "note: 'claude mcp add' did not succeed (non-fatal — add it manually if you want MCP)"
    fi
  fi
done

# --- summary -------------------------------------------------------------------
if [ "$JSON" -eq 1 ]; then
  agents_json=""
  for id in $AGENTS; do
    [ -z "$agents_json" ] || agents_json="${agents_json},"
    agents_json="${agents_json}\"${id}\""
  done
  # Single-quoted format string: the backticks in next_step are literal JSON
  # text, NOT command substitution — SC2016 is exactly the behavior we want.
  # shellcheck disable=SC2016
  printf '{"ok":true,"version":"%s","target":"%s","bin":"%s","agents":[%s],"path_modified":%s,"rustup_bootstrapped":%s,"next_step":"run `jerrycan onboard` and follow it"}\n' \
    "$VERSION" "$TARGET" "$BIN" "$agents_json" "$PATH_MODIFIED" "$RUSTUP_BOOTSTRAPPED"
else
  log "done."
  log "  version: $VERSION   target: $TARGET"
  log "  binary:  $BIN"
  log "  agents:  $AGENTS"
  log "next: run \`jerrycan onboard\` and follow it"
fi
