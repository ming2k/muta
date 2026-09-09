#!/usr/bin/env bash
# check-egress-deps.sh — ADR-0200's dependency gate.
#
# The programme's exit criterion is "no third-party HTTP implementation in a
# production dependency graph". This script enforces the two halves of it:
#
#   1. `muta-llm-client` (the LLM egress) must not pull `reqwest` in its default
#      build. The oracle is opt-in, so the check is meaningful rather than
#      vacuous.
#   2. Every workspace crate that still declares `reqwest` directly must be on
#      the allow-list below. The list is a ratchet: it may only shrink, and each
#      entry names the reason it is still there.
#
# Usage: scripts/check-egress-deps.sh

set -euo pipefail

cd "$(dirname "$0")/.."

# Crates still permitted to declare `reqwest`, with the reason. Empty is the
# goal; the check fails on any entry that is not listed here.
ALLOWED_REQWEST_DEPENDENTS=(
  # Empty: every egress path runs on `muta-net`. Keep the array so a future
  # temporary exception has a documented place to live, and so the check keeps
  # failing until it is named here.
)

fail=0

echo "egress gate: muta-llm-client default graph"
if cargo tree -p muta-llm-client -i reqwest >/dev/null 2>&1; then
  echo "  FAIL: reqwest is in the default dependency graph"
  cargo tree -p muta-llm-client -i reqwest | head -5
  fail=1
else
  echo "  ok: no reqwest in the production graph"
fi

echo "egress gate: reqwest as the oracle only"
if cargo tree -p muta-llm-client --features reqwest-oracle -i reqwest >/dev/null 2>&1; then
  echo "  ok: the oracle still compiles when asked for"
else
  echo "  FAIL: reqwest-oracle does not resolve; the differential harness is broken"
  fail=1
fi

echo "egress gate: remaining direct dependents"
# Only a *non-optional* `reqwest` declaration counts: an optional one is the
# oracle, which is exactly what we keep.
dependents="$(grep -rl '^reqwest' --include=Cargo.toml crates apps 2>/dev/null \
  | while read -r manifest; do
      if grep -E '^reqwest[[:space:]]*=' "$manifest" | grep -q 'optional[[:space:]]*=[[:space:]]*true'; then
        continue
      fi
      echo "$manifest" | sed -E 's#^(crates|apps)/([^/]+)/Cargo.toml#\2#'
    done \
  | sort -u)"
for crate in $dependents; do
  allowed=0
  for entry in "${ALLOWED_REQWEST_DEPENDENTS[@]}"; do
    [[ "$crate" == "$entry" ]] && allowed=1
  done
  if [[ "$allowed" -eq 1 ]]; then
    echo "  allowed: $crate (see the list in this script)"
  else
    echo "  FAIL: $crate declares reqwest and is not on the allow-list"
    fail=1
  fi
done
[[ -z "$dependents" ]] && echo "  ok: no direct dependents remain"

if [[ "$fail" -ne 0 ]]; then
  echo "egress gate: FAILED"
  exit 1
fi
echo "egress gate: OK"
