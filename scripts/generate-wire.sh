#!/usr/bin/env bash
# Deterministic TypeScript wire type generation and drift check.
#
# Generates apps/web/src/lib/generated/wire.gen.ts from muta-contracts
# in an isolated single-process run, verifies sanity (non-truncated),
# and updates the file atomically.
#
# Usage:
#   bash scripts/generate-wire.sh          # Generate and atomically update
#   bash scripts/generate-wire.sh --check  # Verify that wire.gen.ts is fresh without modifying
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET_FILE="apps/web/src/lib/generated/wire.gen.ts"
CHECK_MODE=false

if [ "${1:-}" = "--check" ]; then
    CHECK_MODE=true
fi

# Ensure target directory exists
mkdir -p "$(dirname "$TARGET_FILE")"

# Backup target file if in check mode
TMP_BACKUP=""
if [ "$CHECK_MODE" = true ] && [ -f "$TARGET_FILE" ]; then
    TMP_BACKUP="$(mktemp)"
    cp "$TARGET_FILE" "$TMP_BACKUP"
fi

cleanup() {
    if [ -n "$TMP_BACKUP" ] && [ -f "$TMP_BACKUP" ]; then
        cp "$TMP_BACKUP" "$TARGET_FILE"
        rm -f "$TMP_BACKUP"
    fi
}
trap cleanup EXIT

# Run ts-rs export in a single process to prevent multi-process file truncation
cargo test -p muta-contracts --lib export_bindings --locked -- --test-threads=1 >/dev/null

# Sanity check: verify the file was actually generated and has substantial content (>1000 lines)
LINE_COUNT="$(wc -l < "$TARGET_FILE")"
if [ "$LINE_COUNT" -lt 1000 ]; then
    echo "::error::generate-wire: wire.gen.ts generation produced an unexpectedly small file ($LINE_COUNT lines). Truncation suspected!" >&2
    exit 1
fi

if [ "$CHECK_MODE" = true ]; then
    if ! cmp -s "$TARGET_FILE" "$TMP_BACKUP"; then
        echo "::error::wire.gen.ts is stale — run 'bash scripts/generate-wire.sh' and commit the result." >&2
        exit 1
    fi
    echo "generate-wire: wire.gen.ts is up to date — OK"
else
    echo "generate-wire: successfully generated $TARGET_FILE ($LINE_COUNT lines)"
fi
