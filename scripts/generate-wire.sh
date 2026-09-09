#!/usr/bin/env bash
# Deterministic TypeScript wire type generation and drift check.
#
# Generates apps/web/src/lib/generated/wire.gen.ts from muta-contracts
# in an isolated single-process run, verifies sanity (non-truncated),
# and updates the committed generated file.
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
if [ "$CHECK_MODE" = true ] && [ ! -f "$TARGET_FILE" ]; then
    echo "::error::generate-wire: $TARGET_FILE is missing" >&2
    exit 1
fi
if [ "$CHECK_MODE" = true ]; then
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

# Ask nextest to build and identify the library harness without executing its
# tests. ts-rs exports must then run inside that one libtest process: nextest's
# normal one-process-per-test execution would let each export overwrite the
# shared target and leave a truncated file. Run from the contracts package,
# matching Cargo's libtest cwd so dependency imports have stable canonical paths.
LIST_JSON="$(cargo nextest list -p muta-contracts --lib --locked \
    --list-type binaries-only --message-format json)"
TEST_BINARY="$(printf '%s\n' "$LIST_JSON" \
    | sed -n 's/.*"binary-path":"\([^"]*\)".*/\1/p')"
if [ -z "$TEST_BINARY" ] || [ ! -x "$TEST_BINARY" ]; then
    echo "::error::generate-wire: could not resolve the muta-contracts test binary" >&2
    exit 1
fi
(cd crates/muta-contracts && TS_RS_LARGE_INT=number \
    "$TEST_BINARY" export_bindings --test-threads=1 >/dev/null)

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
