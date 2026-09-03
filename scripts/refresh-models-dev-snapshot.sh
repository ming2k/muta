#!/usr/bin/env bash
# Refresh the embedded models.dev snapshot used by the `muta-models-dev`
# crate (ADR-0171, `LiveCatalog::ModelsDev`).
#
# The snapshot is the *offline fallback* for third-party catalog providers.
# It deliberately contains ONLY the providers the client actually consumes via
# `LiveCatalog::ModelsDev` — not the full 200+ provider models.dev catalog —
# so the embedded file stays small and the binary stays lean. Adding a
# provider to this source means adding its id to PROVIDER_IDS below and
# re-running this script.
#
# The snapshot is checked into the repo: `build.rs` embeds it with
# `include_str!`, so builds never touch the network and stay deterministic.
#
# Usage:
#   bash scripts/refresh-models-dev-snapshot.sh
set -euo pipefail

cd "$(dirname "$0")/.."

# Providers the client consumes via `LiveCatalog::ModelsDev` (either as the
# primary source or as the first-party fallback). Keep this list in sync with
# the presets that declare `LiveCatalog::ModelsDev` /
# `LiveCatalog::ProviderEndpointWithFallback`.
PROVIDER_IDS=("opencode-go" "zai")

SNAPSHOT="crates/muta-models-dev/snapshot.json"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

echo "fetching models.dev catalog…"
curl --silent --show-error --fail --max-time 30 \
  "https://models.dev/api.json" > "$TMP"

python3 - "$TMP" "$SNAPSHOT" "${PROVIDER_IDS[@]}" <<'PY'
import json, sys, os

raw = sys.argv[1]
out = sys.argv[2]
want = set(sys.argv[3:])

with open(raw, encoding="utf-8") as f:
    catalog = json.load(f)

missing = sorted(pid for pid in want if pid not in catalog)
if missing:
    sys.stderr.write(
        f"models.dev has no entries for: {', '.join(missing)}\n"
    )
    sys.exit(1)

selected = {pid: catalog[pid] for pid in sorted(want)}
body = json.dumps(selected, ensure_ascii=False, indent=2, sort_keys=True) + "\n"

# Preserve the previous snapshot's byte content when nothing changed, so a
# no-op refresh does not dirty the working tree.
try:
    with open(out, encoding="utf-8") as f:
        if f.read() == body:
            print(f"snapshot unchanged: {out}")
            sys.exit(0)
except FileNotFoundError:
    pass

with open(out, "w", encoding="utf-8") as f:
    f.write(body)
print(f"wrote {out} ({len(body.encode('utf-8'))} bytes, {len(selected)} provider(s))")
PY