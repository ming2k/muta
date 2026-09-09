#!/usr/bin/env bash
# check-tap-fidelity.sh — ADR-0200's privileged cross-check.
#
# Compares the syscall tap's read timeline against a packet capture of the same
# stream. The criterion is the one the ADR states: read boundaries must track
# segment arrivals within 1 ms p99. Inter-arrival *deltas* are compared rather
# than absolute times, because the tap runs on CLOCK_MONOTONIC and tcpdump on
# CLOCK_REALTIME — comparing offsets would measure clock skew, not fidelity.
#
# Usage: scripts/check-tap-fidelity.sh [interface]
#
# Exits 0 when the criterion holds, 1 when it does not, and 0 with a SKIP line
# when the environment cannot run the check (no tcpdump, no privileges, not
# Linux). A skip is reported loudly: an unrun gate is not a passed gate.

set -euo pipefail

cd "$(dirname "$0")/.."

IFACE="${1:-lo}"
TOLERANCE_MS="${TAP_FIDELITY_TOLERANCE_MS:-1}"
FRAMES="${TAP_FIDELITY_FRAMES:-8}"
GAP_MS="${TAP_FIDELITY_GAP_MS:-25}"

skip() {
  echo "SKIP: $1"
  echo "tap fidelity: NOT VERIFIED (this is not a pass)"
  exit 0
}

[[ "$(uname -s)" == "Linux" ]] || skip "AF_PACKET capture is Linux-only"
command -v tcpdump >/dev/null 2>&1 || skip "tcpdump is not installed"
command -v python3 >/dev/null 2>&1 || skip "python3 is required to parse the capture"

if [[ "$(id -u)" -ne 0 ]]; then
  if sudo -n true 2>/dev/null; then
    SUDO="sudo -n"
  else
    skip "packet capture needs root or passwordless sudo"
  fi
else
  SUDO=""
fi

PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PCAP="$WORK/tap.pcap"
TAP_OUT="$WORK/tap.txt"

echo "tap fidelity: capturing on $IFACE, port $PORT, $FRAMES frames, ${GAP_MS}ms apart"

# Start the capture before the client connects, so the first segment is included.
$SUDO tcpdump -i "$IFACE" -nn -q -w "$PCAP" --time-stamp-precision=nano \
  "tcp and port $PORT" >/dev/null 2>&1 &
TCPDUMP_PID=$!
sleep 1

cargo run --quiet -p muta-net --example tap_fidelity -- \
  --port "$PORT" --frames "$FRAMES" --gap-ms "$GAP_MS" > "$TAP_OUT" 2>"$WORK/tap.err" || {
  echo "FAIL: the harness did not run:"
  cat "$WORK/tap.err"
  kill "$TCPDUMP_PID" 2>/dev/null || true
  exit 1
}
sleep 1
$SUDO kill "$TCPDUMP_PID" 2>/dev/null || true
wait "$TCPDUMP_PID" 2>/dev/null || true

python3 - "$PCAP" "$TAP_OUT" "$TOLERANCE_MS" <<'PY'
import re
import subprocess
import sys

pcap, tap_out, tolerance_ms = sys.argv[1], sys.argv[2], float(sys.argv[3])
tolerance_ns = tolerance_ms * 1_000_000

# tcpdump -tt prints epoch seconds with nanosecond precision when the capture
# was written with --time-stamp-precision=nano.
text = subprocess.run(
    ["tcpdump", "-r", pcap, "-nn", "-tt", "-q"],
    capture_output=True, text=True, check=True,
).stdout
capture = []
for line in text.splitlines():
    match = re.match(r"^(\d+\.\d+) IP .*length (\d+)$", line.strip())
    if match:
        capture.append((float(match.group(1)), int(match.group(2))))

tap = []
for line in open(tap_out):
    parts = line.split()
    if len(parts) == 3 and parts[0] == "read":
        tap.append((int(parts[1]), int(parts[2])))

if len(capture) < 3 or len(tap) < 3:
    print(f"FAIL: not enough samples (capture {len(capture)}, tap {len(tap)})")
    sys.exit(1)

def deltas(samples, times):
    return [times[i + 1] - times[i] for i in range(len(times) - 1)]

capture_deltas = deltas(capture, [t for t, _ in capture])
tap_deltas = [d / 1_000_000_000 for d in deltas(tap, [t for t, _ in tap])]

# Compare the two delta sequences pairwise over the shorter length.
count = min(len(capture_deltas), len(tap_deltas))
if count < 2:
    print("FAIL: not enough deltas to compare")
    sys.exit(1)
errors = sorted(abs(capture_deltas[i] - tap_deltas[i]) for i in range(count))
p99 = errors[min(len(errors) - 1, int(len(errors) * 0.99))]
worst = errors[-1]
print(
    f"tap fidelity: {count} intervals, p99 |delta error| {p99 * 1000:.3f}ms, "
    f"worst {worst * 1000:.3f}ms (tolerance {tolerance_ms}ms)"
)
if p99 > tolerance_ms / 1000:
    print("FAIL: the tap's read boundaries do not track segment arrivals")
    sys.exit(1)
print("OK: tap read boundaries track segment arrivals within tolerance")
PY
