#!/usr/bin/env bash
# Thirty-minute host/app memory and sustained-stream gate; never reuses a host.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODE=simulator
if [ "${1:-}" = --real ]; then MODE=real; shift; fi
DURATION="${1:-1800}"
if [ "$#" -gt 1 ] || [[ ! "$DURATION" =~ ^[0-9]+$ ]] || [ "$DURATION" -lt 330 ]; then
    echo "Usage: $0 [--real] [seconds >= 330, default 1800]" >&2
    exit 2
fi
if [ "$MODE" = real ]; then
    # Desktop screenshots and logs belong with private hardware evidence.
    DEFAULT_OUT="${EM_EVIDENCE_DIR:-/Users/aldo/Desktop/EternalMonitor-Handoff/evidence}/soak-real"
else
    DEFAULT_OUT="$ROOT/build/soak/$MODE"
fi
OUT="${EM_OUTPUT_DIR:-$DEFAULT_OUT}"
REPORT="$ROOT/build/soak-report.md"
HOST_STARTED=0
PATTERN_STARTED=0
mkdir -p "$OUT"
cleanup() {
    status=$?
    trap - EXIT
    if [ "$HOST_STARTED" = 1 ]; then
        "$ROOT/scripts/win/remote.sh" log > "$OUT/host.log" || status=1
        "$ROOT/scripts/win/remote.sh" stop-host || status=1
    fi
    if [ "$PATTERN_STARTED" = 1 ]; then "$ROOT/scripts/win/remote.sh" pattern stop || status=1; fi
    exit "$status"
}
trap cleanup EXIT
if [ "$MODE" = real ]; then
    REPORT="$ROOT/build/soak-real-report.md"
    "$ROOT/scripts/win/remote.sh" idle
    "$ROOT/scripts/win/remote.sh" pattern start
    PATTERN_STARTED=1
    "$ROOT/scripts/win/remote.sh" run-host ETERNAL_HEADLESS=1 ETERNAL_ENCODER=h264_nvenc \
        ETERNAL_HEVC=0 ETERNAL_FPS=60 ETERNAL_MAX_DGRAM=1200 ETERNAL_E2E_LOG=1 \
        ETERNAL_USB_DIRECT=127.0.0.1:0
    HOST_STARTED=1
    export EM_REMOTE_HOST=100.81.59.48 EM_PORT=19876 EM_SIZE=1920x1080
fi
status=0
EM_SOAK=1 EM_SCENARIO="soak-$MODE" EM_OUTPUT_DIR="$OUT" EM_SCREENSHOT="$OUT/simulator.png" \
    EM_DURATION="$DURATION" EM_TIMEOUT="$((DURATION+120))" \
    "$ROOT/scripts/e2e_ios.sh" > "$OUT/run.log" 2>&1 || status=$?
python3 "$ROOT/scripts/soak_stats.py" "$OUT/resources.jsonl" --seconds "$DURATION" \
    --stream-result "$OUT/result.json" --report "$REPORT" || status=1
cp "$REPORT" "$OUT/report.md"
cp "${REPORT%.md}.json" "$OUT/report.json"
exit "$status"
