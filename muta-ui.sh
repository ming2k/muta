#!/usr/bin/env bash
#
# muta-ui.sh — one-click desktop/shell launcher for Muta Web UI
#
# Ensures the local `muta` daemon is running and opens the Web UI in the default browser.
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Ensure muta binary can be found
if [ -x "${SCRIPT_DIR}/muta" ]; then
    export PATH="${SCRIPT_DIR}:${PATH}"
elif [ -x "${HOME}/.local/bin/muta" ]; then
    export PATH="${HOME}/.local/bin:${PATH}"
fi

if ! command -v muta >/dev/null 2>&1; then
    echo "Error: 'muta' binary not found. Please install muta or run this script from the muta directory." >&2
    exit 1
fi

# 1. Start daemon if not already running
if ! muta status >/dev/null 2>&1; then
    echo "Starting Muta daemon in background..."
    muta start
    sleep 0.8
fi

# 2. Retrieve local token and construct Web URL
TOKEN="$(muta token 2>/dev/null || true)"
PORT="9800"
URL="http://127.0.0.1:${PORT}"

if [ -n "$TOKEN" ]; then
    URL="http://127.0.0.1:${PORT}/?token=${TOKEN}"
fi

echo "Opening Muta Web UI at: ${URL}"

# 3. Open in default browser
if command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$URL" >/dev/null 2>&1 &
elif command -v open >/dev/null 2>&1; then
    open "$URL"
else
    echo "Please open the URL above in your browser."
fi
