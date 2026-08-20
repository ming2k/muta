#!/usr/bin/env bash
# CLI-surface smoke (ADR-0119/0121): pins the command line's observable
# contract — the retired spellings teach, the noun-verb shapes parse, and
# the remote transport actually connects — against a live, isolated daemon.
#
# Complements apps/web/e2e/daemon-smoke.mjs (protocol-level) by exercising
# the binary itself: parsing, errors, exit codes, and the --remote path.
#
# Env: NEENEE_BIN (default target/debug/neenee). Exits non-zero on the
# first failed expectation; the instance root is a throwaway tempdir, so a
# host daemon is never touched (ADR-0121).

set -u

# Resolve the repo root through this script's location so it works from any
# cwd and through symlinks: apps/web/e2e → three levels up is the root.
SCRIPT_SOURCE=${BASH_SOURCE[0]}
REPO_ROOT=$(cd -P "$(dirname "$SCRIPT_SOURCE")/../../.." >/dev/null 2>&1 && pwd)
BIN=${NEENEE_BIN:-$REPO_ROOT/target/debug/neenee}
if [ ! -x "$BIN" ]; then
  echo "cli-smoke: no binary at $BIN — build it first: cargo build -p neenee-cli" >&2
  exit 127
fi
ROOT=$(mktemp -d /tmp/neenee-cli-smoke.XXXXXX)
PORT=${PORT:-9809}
cleanup() { "$BIN" --home "$ROOT" daemon stop >/dev/null 2>&1; rm -rf "$ROOT"; }
trap cleanup EXIT

pass=0
fail=0

# expect_status <want> <desc> <cmd...>  — assert an exit status
expect_status() {
  local want=$1 desc=$2
  shift 2
  if "$@" >/dev/null 2>&1; then local got=0; else local got=$?; fi
  if [ "$got" = "$want" ]; then
    pass=$((pass + 1)); echo "ok   $desc"
  else
    fail=$((fail + 1)); echo "FAIL $desc (exit $got, want $want)"
  fi
}

# expect_out <pattern> <desc> <cmd...>  — assert combined output matches
expect_out() {
  local pat=$1 desc=$2
  shift 2
  local out
  out=$("$@" 2>&1)
  if printf '%s' "$out" | grep -q "$pat"; then
    pass=$((pass + 1)); echo "ok   $desc"
  else
    fail=$((fail + 1)); echo "FAIL $desc (output: $out)"
  fi
}

echo "== retired spellings teach the canonical form =="
expect_status 2 "bare session errors" "$BIN" session
expect_out   "session rm" "session teaches rm" "$BIN" session
expect_out   "daemon status" "session ls points at daemon status" "$BIN" session ls
expect_status 2 "bare mcp errors" "$BIN" mcp
expect_out   "mcp ls" "mcp teaches ls" "$BIN" mcp
expect_status 2 "bare skill errors" "$BIN" skill
expect_out   "skill ls" "skill teaches ls" "$BIN" skill
expect_status 2 "serve is retired" "$BIN" serve
expect_status 2 "stop is retired" "$BIN" stop

echo "== noun-verb shapes parse =="
expect_status 0 "mcp ls runs" "$BIN" mcp ls
expect_status 0 "skill ls runs" "$BIN" skill ls
expect_status 2 "session rm needs an id" "$BIN" session rm

echo "== remote transport validates its inputs =="
expect_out "not host:port" "remote rejects a bare host" \
  "$BIN" --remote box.lan --token t run x
expect_out "needs --token" "remote demands the token" \
  "$BIN" --remote box.lan:9800 run x
expect_out "not a port number" "remote rejects a bad port" \
  "$BIN" --remote box.lan:notaport --token t run x

echo "== panel names its acts =="
expect_out "url" "help panel documents url/open" "$BIN" help panel

echo "== live daemon: panel url and the --remote connection =="
"$BIN" --home "$ROOT" daemon start --port "$PORT" >/dev/null 2>&1
for _ in $(seq 1 50); do
  "$BIN" --home "$ROOT" panel 2>/dev/null | grep -q "127.0.0.1:$PORT" && break
  sleep 0.2
done
expect_out "127.0.0.1:$PORT/?token=" "panel url carries port and token" \
  "$BIN" --home "$ROOT" panel
TOKEN=$("$BIN" --home "$ROOT" panel | sed 's/.*?token=//')
# The handshake must complete over TCP+bearer: with no provider configured
# the daemon's own reply is the "no provider" error — proof the remote
# path reached a live daemon rather than failing to connect.
expect_out "provider" "--remote reaches the live daemon" \
  "$BIN" --remote "127.0.0.1:$PORT" --token "$TOKEN" run "ping"

echo
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
