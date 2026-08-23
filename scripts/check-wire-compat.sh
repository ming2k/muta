#!/usr/bin/env bash
# Wire-protocol bump enforcement (ADR-0134).
#
# The wire protocol number (PROTOCOL_VERSION in
# crates/neenee-contracts/src/wire.rs) governs client/daemon compatibility.
# Its bump discipline is documented there: additive changes do not bump;
# changes an older peer cannot deserialize or would misinterpret do. A
# forgotten bump is a *silent* corruption bug — exactly the hazard version
# negotiation exists to prevent — so this script makes the decision
# mechanically visible in CI.
#
# What it checks, against the PR's merge base with the default branch:
#
#   1. The three protocol mirrors agree:
#        - PROTOCOL_VERSION / MIN_PROTOCOL_VERSION (Rust, wire.rs)
#        - PROTOCOL_VERSION (web, daemon.svelte.ts)
#   2. If the wire surface changed — wire.rs, the ts-rs-exported contract
#      types, or the generated TypeScript mirror — then either
#      PROTOCOL_VERSION / MIN_PROTOCOL_VERSION changed too, or the PR is
#      labeled `wire-compatible` (the author asserts the change is purely
#      additive; a reviewer checks that assertion).
#
# Exit 0 = policy satisfied; exit 1 = explain and fail. Deliberately a
# plain POSIX-ish bash script: it must run unmodified in the CI runner and
# locally (`bash scripts/check-wire-compat.sh`).
set -euo pipefail

cd "$(dirname "$0")/.."

die() { echo "::error::check-wire-compat: $*" >&2; exit 1; }

# The default branch: `origin/main` in CI, `main` for local runs against a
# clone that has it. Fall back to HEAD~1 when neither exists (shallow
# single-branch checkouts).
BASE_REF="${WIRE_COMPAT_BASE:-origin/main}"
if ! git rev-parse --verify -q "$BASE_REF" >/dev/null; then
    BASE_REF="main"
fi
if ! git rev-parse --verify -q "$BASE_REF" >/dev/null; then
    BASE_REF="HEAD~1"
    echo "check-wire-compat: no origin/main; comparing against $BASE_REF" >&2
fi
BASE="$(git rev-parse "$BASE_REF")"

# ── 1. Mirror agreement ────────────────────────────────────────────────
rust_proto="$(sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' crates/neenee-contracts/src/wire.rs)"
rust_min="$(sed -n 's/^pub const MIN_PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' crates/neenee-contracts/src/wire.rs)"
web_proto="$(sed -n 's/^const PROTOCOL_VERSION = \([0-9]*\);/\1/p' apps/web/src/lib/stores/daemon.svelte.ts)"

[ -n "$rust_proto" ] || die "PROTOCOL_VERSION not found in crates/neenee-contracts/src/wire.rs"
[ -n "$rust_min" ] || die "MIN_PROTOCOL_VERSION not found in crates/neenee-contracts/src/wire.rs"
[ -n "$web_proto" ] || die "PROTOCOL_VERSION not found in apps/web/src/lib/stores/daemon.svelte.ts"

[ "$rust_proto" = "$web_proto" ] \
    || die "protocol mirrors disagree: Rust wire.rs=$rust_proto, web daemon.svelte.ts=$web_proto"
[ "$rust_min" -le "$rust_proto" ] \
    || die "MIN_PROTOCOL_VERSION ($rust_min) exceeds PROTOCOL_VERSION ($rust_proto)"

# ── 2. Bump-or-label on wire-surface changes ───────────────────────────
# Files whose change can alter the wire shape. wire.rs and the generated
# mirror are the envelope itself; the contracts sources are the payload
# types serde derives from.
WIRE_PATHS=(
    "crates/neenee-contracts/src/wire.rs"
    "apps/web/src/lib/generated/wire.gen.ts"
)
# Payload contract sources: everything the ts-rs export derives from.
CONTRACT_SRCS="$(git ls-files 'crates/neenee-contracts/src/*.rs')"

changed_wire=()
for path in "${WIRE_PATHS[@]}" $CONTRACT_SRCS; do
    if git diff --name-only "$BASE" -- "$path" | grep -q .; then
        changed_wire+=("$path")
    fi
done

if [ "${#changed_wire[@]}" -eq 0 ]; then
    echo "check-wire-compat: no wire-surface changes — OK"
    exit 0
fi

echo "check-wire-compat: wire surface changed:"
printf '  %s\n' "${changed_wire[@]}"

base_proto="$(git show "$BASE:crates/neenee-contracts/src/wire.rs" 2>/dev/null \
    | sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' || true)"
base_min="$(git show "$BASE:crates/neenee-contracts/src/wire.rs" 2>/dev/null \
    | sed -n 's/^pub const MIN_PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' || true)"

# First PR introducing the field: the file (and constants) are new, so the
# "did the number change?" question is vacuous — accept and move on.
if [ -z "$base_proto" ]; then
    echo "check-wire-compat: wire.rs is new at this base — nothing to compare"
    exit 0
fi

if [ "$base_proto" != "$rust_proto" ] || [ "$base_min" != "$rust_min" ]; then
    echo "check-wire-compat: protocol number moved ($base_proto/$base_min -> $rust_proto/$rust_min) — OK"
    exit 0
fi

# Number unchanged but the surface changed: demand the explicit label,
# read from the PR labels in the CI event payload when available.
labels=""
if [ -n "${GITHUB_EVENT_PATH:-}" ] && [ -f "${GITHUB_EVENT_PATH:-}" ]; then
    labels="$(node -e '
        const ev = require(process.env.GITHUB_EVENT_PATH);
        console.log((ev.pull_request?.labels ?? []).map(l => l.name).join("\n"));
    ' 2>/dev/null || true)"
fi
if printf '%s' "$labels" | grep -qx 'wire-compatible'; then
    echo "check-wire-compat: PR labeled wire-compatible — OK (reviewer: verify the change is purely additive)"
    exit 0
fi

cat >&2 <<EOF
check-wire-compat: wire surface changed but PROTOCOL_VERSION stayed $rust_proto.

Either:
  * bump PROTOCOL_VERSION in crates/neenee-contracts/src/wire.rs (and the
    web mirror in apps/web/src/lib/stores/daemon.svelte.ts) if an older
    peer cannot deserialize or would misinterpret the new frames, or
  * add the \`wire-compatible\` label to this PR if the change is purely
    additive (optional fields, new variants an older peer never receives),
    and note why in the PR description.
EOF
exit 1
