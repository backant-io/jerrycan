#!/usr/bin/env bash
# Hermetic self-test for scripts/install.sh. Not run in the per-PR gate (it is
# heavy: it stands up a local HTTP server and a temp HOME). Run it locally:
#
#   bash scripts/install-test.sh
#
# It never touches the network or the real HOME: a stub `jerrycan` binary is
# packed into a tarball, served over 127.0.0.1 via `python3 -m http.server`, and
# installed into a throwaway HOME using every JERRYCAN_INSTALL_* override. It
# asserts: exit 0, a parseable JSON summary, an executable binary on disk, an
# idempotent PATH line on re-run, and a hard failure when the checksum is wrong.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
INSTALL_SH="$HERE/install.sh"
[ -f "$INSTALL_SH" ] || { echo "FATAL: $INSTALL_SH not found" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || { echo "FATAL: python3 required" >&2; exit 1; }

PASS=0
FAIL=0
ok()   { printf 'PASS: %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf 'FAIL: %s\n' "$1"; FAIL=$((FAIL + 1)); }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/jerrycan-install-test.XXXXXX")"
SERVE="$WORK/serve"
FAKE_HOME="$WORK/home"
SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# --- target detection must mirror install.sh so we serve the right asset -------
s="$(uname -s)"; m="$(uname -m)"
case "$s" in
  Darwin) case "$m" in arm64|aarch64) TARGET="aarch64-apple-darwin" ;; x86_64) TARGET="x86_64-apple-darwin" ;; esac ;;
  Linux)  case "$m" in x86_64|amd64) TARGET="x86_64-unknown-linux-musl" ;; arm64|aarch64) TARGET="aarch64-unknown-linux-musl" ;; esac ;;
esac
[ -n "${TARGET:-}" ] || { echo "FATAL: unsupported test platform $s/$m" >&2; exit 1; }

VER="0.0.0-selftest"
ASSET_DIR="$SERVE/v$VER"
mkdir -p "$ASSET_DIR" "$FAKE_HOME"

# --- build a tiny stub `jerrycan` and pack it exactly like the real asset ------
STUB="$WORK/jerrycan"
cat >"$STUB" <<'STUBEOF'
#!/bin/sh
# Stub jerrycan for the installer self-test: answers only what install.sh calls.
case "${1:-}" in
  --version) echo "jerrycan $VER (selftest stub)" ;;
  onboard)
    shift
    case " $* " in
      *" --emit-skill "*) echo "stub: emitted skill for$*" ;;
      *)                  echo "stub: guided runbook" ;;
    esac ;;
  mcp) echo "stub: mcp server would start here" ;;
  *) echo "stub: unhandled args: $*" >&2 ;;
esac
STUBEOF
# Inline the version so `jerrycan --version` reports it (stub has no build step).
sed -i.bak "s/\$VER/$VER/g" "$STUB" && rm -f "$STUB.bak"
chmod 755 "$STUB"

TARBALL="$ASSET_DIR/jerrycan-$TARGET.tar.gz"
tar -czf "$TARBALL" -C "$WORK" jerrycan
if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$ASSET_DIR" && sha256sum "jerrycan-$TARGET.tar.gz" >"jerrycan-$TARGET.tar.gz.sha256" )
else
  ( cd "$ASSET_DIR" && shasum -a 256 "jerrycan-$TARGET.tar.gz" >"jerrycan-$TARGET.tar.gz.sha256" )
fi
GOOD_SHA="$(cat "$ASSET_DIR/jerrycan-$TARGET.tar.gz.sha256")"

# --- serve the asset dir over loopback ----------------------------------------
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$SERVE" >/dev/null 2>&1 &
SERVER_PID=$!
BASE_URL="http://127.0.0.1:$PORT"

# Wait for readiness (poll the actual asset URL; no fixed sleep).
ready=0
for _ in $(seq 1 50); do
  if curl -fsS "$BASE_URL/v$VER/jerrycan-$TARGET.tar.gz" -o /dev/null 2>/dev/null; then ready=1; break; fi
  sleep 0.1
done
[ "$ready" -eq 1 ] || { echo "FATAL: local http server never became ready on $PORT" >&2; exit 1; }

BIN_DIR="$FAKE_HOME/.jerrycan/bin"

# run_install <extra-args...> — runs install.sh with the hermetic env, prints
# stdout, returns install.sh's exit code. NO_RUSTUP so we never touch rustup;
# SHELL=/bin/zsh so the PATH line lands in a predictable rc file.
run_install() {
  env -i \
    HOME="$FAKE_HOME" \
    PATH="$PATH" \
    SHELL="/bin/zsh" \
    JERRYCAN_INSTALL_BASE_URL="$BASE_URL" \
    JERRYCAN_INSTALL_VERSION="$VER" \
    JERRYCAN_INSTALL_DIR="$BIN_DIR" \
    JERRYCAN_NO_RUSTUP=1 \
    bash "$INSTALL_SH" "$@"
}

echo "== test 1: first install (--agent generic --json) =="
set +e
OUT1="$(run_install --agent generic --json 2>"$WORK/err1.log")"
RC1=$?
set -e
if [ "$RC1" -eq 0 ]; then ok "exit 0 on first install"; else bad "expected exit 0, got $RC1 (see $WORK/err1.log)"; fi

if printf '%s' "$OUT1" | python3 -c 'import sys,json; d=json.load(sys.stdin); assert d["ok"] is True; assert d["agents"]==["generic"]; assert d["target"]=="'"$TARGET"'"; assert d["path_modified"] is True; assert d["rustup_bootstrapped"] is False' >/dev/null 2>"$WORK/json.err"; then
  ok "stdout is one parseable JSON summary with the expected fields"
else
  bad "JSON summary did not parse / assertions failed ($(cat "$WORK/json.err"))"
fi

if [ -x "$BIN_DIR/jerrycan" ]; then ok "binary installed and executable at $BIN_DIR/jerrycan"; else bad "binary missing or not executable"; fi

if "$BIN_DIR/jerrycan" --version 2>/dev/null | grep -q "$VER"; then ok "installed binary runs and reports the served version"; else bad "installed binary did not run --version"; fi

# stdout must be JSON ONLY — no human progress leaking into it.
case "$OUT1" in
  '{'*) ok "stdout carries only the JSON doc (progress went to stderr)" ;;
  *)    bad "stdout was polluted with non-JSON output" ;;
esac

echo "== test 2: second install is idempotent (no duplicate PATH line) =="
set +e
OUT2="$(run_install --agent generic --json 2>"$WORK/err2.log")"
RC2=$?
set -e
if [ "$RC2" -eq 0 ]; then ok "exit 0 on second install"; else bad "second install exit $RC2"; fi

MARKERS="$(grep -cF '# jerrycan installer' "$FAKE_HOME/.zshrc" 2>/dev/null || true)"
[ -n "$MARKERS" ] || MARKERS=0
if [ "$MARKERS" -eq 1 ]; then ok "PATH marker present exactly once after two runs"; else bad "expected 1 PATH marker, found $MARKERS"; fi

if printf '%s' "$OUT2" | python3 -c 'import sys,json; d=json.load(sys.stdin); assert d["path_modified"] is False; print("ok")' >/dev/null 2>&1; then
  ok "second run reports path_modified=false (marker already present)"
else
  bad "second run should report path_modified=false"
fi

echo "== test 3: checksum tamper must fail install =="
# Corrupt the published checksum so it no longer matches the tarball.
printf '%s  jerrycan-%s.tar.gz\n' "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef" "$TARGET" \
  >"$ASSET_DIR/jerrycan-$TARGET.tar.gz.sha256"
set +e
run_install --agent generic --json >/dev/null 2>"$WORK/err3.log"
RC3=$?
set -e
if [ "$RC3" -ne 0 ]; then ok "tampered checksum causes non-zero exit ($RC3)"; else bad "install SUCCEEDED on a tampered checksum — security hole"; fi
if grep -qi "checksum mismatch" "$WORK/err3.log"; then ok "checksum failure is reported clearly on stderr"; else bad "no clear checksum-mismatch message"; fi
# Restore the good checksum for any later reruns.
printf '%s' "$GOOD_SHA" >"$ASSET_DIR/jerrycan-$TARGET.tar.gz.sha256"

echo "== test 4: --help exits 0 with usage on stdout =="
set +e
HELP_OUT="$(bash "$INSTALL_SH" --help 2>/dev/null)"
RCH=$?
set -e
if [ "$RCH" -eq 0 ] && printf '%s' "$HELP_OUT" | grep -q "Usage: install.sh"; then ok "--help prints usage and exits 0"; else bad "--help behavior wrong (rc=$RCH)"; fi

echo "== test 5: unknown flag exits non-zero with usage on stderr =="
set +e
ERR_OUT="$(bash "$INSTALL_SH" --bogus 2>&1 1>/dev/null)"
RCU=$?
set -e
if [ "$RCU" -ne 0 ] && printf '%s' "$ERR_OUT" | grep -q "Usage: install.sh"; then ok "unknown flag exits non-zero with usage on stderr"; else bad "unknown-flag handling wrong (rc=$RCU)"; fi

echo
echo "==================== $PASS passed, $FAIL failed ===================="
[ "$FAIL" -eq 0 ]
