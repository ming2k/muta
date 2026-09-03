#!/usr/bin/env bash
# models.dev snapshot freshness guard (ADR-0171).
#
# The `muta-models-dev` crate embeds a *pruned, committed* snapshot of the
# models.dev catalog (only the providers the client consumes via
# `LiveCatalog::ModelsDev` — currently opencode-go). If it drifts too far from
# the live catalog, users lose offline fallback coverage for newly shipped
# relay models until a maintainer refreshes it.
#
# This script compares the committed snapshot against the live models.dev
# catalog and fails when the snapshot is stale enough to matter. It is a
# *hint*, not a hard correctness gate: the client never relies on the snapshot
# when the network fetch succeeds, so a stale snapshot degrades offline
# coverage only.
#
# Exit 0 = fresh enough; exit 1 = refresh required.
# Usage: bash scripts/check-models-dev-snapshot.sh
set -euo pipefail

cd "$(dirname "$0")/.."

# Stale = the live catalog advertises a provider model the committed snapshot
# does not carry. We deliberately do NOT fail on the reverse (snapshot having
# extra models): the snapshot may legitimately be ahead after a manual prune,
# and extra offline models never harm the client.
SNAPSHOT="crates/muta-models-dev/snapshot.json"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

if ! curl --silent --show-error --fail --max-time 30 \
  "https://models.dev/api.json" > "$TMP" 2>/dev/null; then
  echo "::warning::check-models-dev-snapshot: could not reach models.dev; skipping freshness check"
  exit 0
fi

python3 - "$TMP" "$SNAPSHOT" <<'PY'
import json, sys

live_raw, snap_raw = sys.argv[1], sys.argv[2]
with open(live_raw, encoding="utf-8") as f:
    live = json.load(f)
with open(snap_raw, encoding="utf-8") as f:
    snap = json.load(f)

missing: list[str] = []
for provider_id in sorted(snap):
    live_provider = live.get(provider_id)
    if live_provider is None:
        continue
    live_models = set(live_provider.get("models", {}))
    snap_models = set(snap[provider_id].get("models", {}))
    for model in sorted(live_models - snap_models):
        missing.append(f"{provider_id}/{model}")

if missing:
    print(
        "::error::models.dev snapshot is stale; run "
        "`bash scripts/refresh-models-dev-snapshot.sh` and commit the result. "
        f"Missing {len(missing)} live model(s): {', '.join(missing[:10])}"
        + (" …" if len(missing) > 10 else "")
    )
    sys.exit(1)

print("models.dev snapshot is fresh (all live models present)")
PY