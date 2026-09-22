#!/usr/bin/env bash
# End-to-end system test on a Mac: the real host binary (synthetic capture,
# libx264, headless) streams over localhost UDP to the real iPad app running
# in the Simulator. Success = the app decodes and renders >= EM_WANT_DECODED
# frames at the synthetic source's resolution, proven via the app's
# machine-readable E2E log milestones.
#
# Requirements: Xcode (DEVELOPER_DIR aware), xcodegen, Rust toolchain,
# the ffmpeg@7 Homebrew keg. See README "Development on macOS".
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"

SIM_NAME="${EM_SIM_NAME:-iPad Pro 11-inch (M4)}"
PORT="${EM_PORT:-9876}"
WANT_DECODED="${EM_WANT_DECODED:-120}"
TIMEOUT_SECS="${EM_TIMEOUT:-120}"
# EM_CODEC=hevc runs the same system test over H.265: the host prefers HEVC
# (ETERNAL_HEVC=1 → libx265 via the variant table) and the app's VideoToolbox
# session software-decodes it in the simulator.
CODEC="${EM_CODEC:-h264}"
SIZE="${EM_SIZE:-640x360}"
SYNTH_W="${SIZE%x*}"
SYNTH_H="${SIZE#*x}"
SCENARIO="${EM_SCENARIO:-$CODEC-udp}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/$SCENARIO}"
SHOT="${EM_SCREENSHOT:-$ROOT/build/screenshots/e2e-$SCENARIO.png}"
REMOTE_HOST="${EM_REMOTE_HOST:-}"
MIN_FPS="${EM_MIN_FPS:-55}"
MIN_SECONDS="${EM_DURATION:-0}"
STARTED=$SECONDS
mkdir -p "$OUT" "$(dirname "$SHOT")"
APP_LOG="$OUT/app.log"
HOST_LOG="$OUT/host.log"
rm -f "$OUT/result.json"

HOST_PID=""
LOG_PID=""
UDID=""
INSTALL_DIR=""
cleanup() {
    status=$?
    [ -n "$LOG_PID" ] && kill "$LOG_PID" 2>/dev/null || true
    [ -n "$UDID" ] && xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
    [ -n "$INSTALL_DIR" ] && rm -rf "$INSTALL_DIR"
    if [ "$status" -ne 0 ]; then
        python3 - "$OUT/result.json" "$SCENARIO" "$((SECONDS - STARTED))" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
r=json.loads(p.read_text()) if p.exists() else {}
r.update(scenario=sys.argv[2], status='FAIL', elapsed=int(sys.argv[3]))
p.write_text(json.dumps(r, indent=2)+'\n')
PY
        echo "FAIL: evidence at $OUT" >&2
    fi
}
trap cleanup EXIT

if [ "${EM_SKIP_BUILD:-0}" != 1 ]; then
echo "==> Generating Xcode project"
(cd "$ROOT/ios" && xcodegen generate >/dev/null)

echo "==> Building app for the simulator"
xcodebuild build \
    -project "$ROOT/ios/EternalMonitor.xcodeproj" \
    -scheme EternalMonitor \
    -destination "platform=iOS Simulator,name=$SIM_NAME" \
    -derivedDataPath "$ROOT/ios/build/e2e" \
    CODE_SIGNING_ALLOWED=NO -quiet
echo "==> Building host"
(cd "$ROOT" && cargo build -q --release -p eternal-host --locked)
fi
APP="$ROOT/ios/build/e2e/Build/Products/Debug-iphonesimulator/EternalMonitor.app"
[ -d "$APP" ] || { echo "FAIL: app bundle not found at $APP"; exit 1; }

HEVC_FLAG=0
[ "$CODEC" = "hevc" ] && HEVC_FLAG=1
if [ -z "$REMOTE_HOST" ]; then
mkdir -p "$OUT/state/EternalMonitor"
python3 - "$OUT/state/EternalMonitor/settings.json" "${EM_BITRATE_MBPS:-15}" <<'PY'
import json,sys
with open(sys.argv[1], 'w') as f:
    json.dump(dict(bitrate_mbps=float(sys.argv[2]), target_fps=60, start_on_boot=False), f)
PY
echo "==> Starting host on 127.0.0.1:$PORT (synthetic ${SYNTH_W}x${SYNTH_H}, codec=$CODEC, headless)"
APPDATA="$OUT/state" \
ETERNAL_HEADLESS=1 \
ETERNAL_CAPTURE=synthetic \
ETERNAL_SYNTH_SIZE="${SYNTH_W}x${SYNTH_H}" \
ETERNAL_ENCODER=libx264 \
ETERNAL_HEVC="$HEVC_FLAG" \
ETERNAL_FPS="${ETERNAL_FPS:-60}" \
    "$ROOT/target/release/eternal-host" "$PORT" >"$HOST_LOG" 2>&1 &
HOST_PID=$!
fi
CONNECT_HOST="${REMOTE_HOST:-127.0.0.1}"

echo "==> Booting simulator: $SIM_NAME"
UDID=$(xcrun simctl list -j devices available | /usr/bin/python3 -c '
import json, sys
data = json.load(sys.stdin)["devices"]
name = sys.argv[1]
for devices in data.values():
    for device in devices:
        if device["name"] == name:
            print(device["udid"]); sys.exit(0)
sys.exit(1)
' "$SIM_NAME")
xcrun simctl bootstatus "$UDID" -b >/dev/null

echo "==> Installing app"
# Simulator's helper can stall copying directly from a protected Desktop
# checkout. Stage only the built app in the temporary directory first.
INSTALL_DIR="$(mktemp -d /tmp/em-e2e-install.XXXXXX)"
ditto "$APP" "$INSTALL_DIR/EternalMonitor.app"
xcrun simctl install "$UDID" "$INSTALL_DIR/EternalMonitor.app"
xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true

echo "==> Streaming app E2E log"
xcrun simctl spawn "$UDID" log stream --style compact \
    --predicate 'subsystem == "com.eternal.monitor.e2e"' >"$APP_LOG" 2>&1 &
LOG_PID=$!
sleep 2 # let the log stream attach before the milestones start

echo "==> Launching app with EM_AUTOCONNECT=$CONNECT_HOST:$PORT"
SIMCTL_CHILD_EM_AUTOCONNECT="$CONNECT_HOST:$PORT" \
SIMCTL_CHILD_EM_E2E_LOG=1 \
SIMCTL_CHILD_EM_UDP_BACKEND="${EM_UDP_BACKEND:-}" \
    xcrun simctl launch "$UDID" com.eternal.monitor >/dev/null

echo "==> Waiting for $WANT_DECODED decoded frames (timeout ${TIMEOUT_SECS}s)"
elapsed=0
decoded=0
until python3 "$ROOT/scripts/e2e_stats.py" "$APP_LOG" --min-frames "$WANT_DECODED" --duration "$MIN_SECONDS"; do
    sleep 2
    elapsed=$((elapsed + 2))
    decoded=$(grep -o 'decoded=[0-9]*' "$APP_LOG" | tail -1 | cut -d= -f2 || true)
    decoded=${decoded:-0}
    if [ "$elapsed" -ge "$TIMEOUT_SECS" ]; then
        echo "FAIL: only $decoded decoded frames after ${TIMEOUT_SECS}s"
        echo "----- app milestones -----"; tail -20 "$APP_LOG"
        echo "----- host log -----"; tail -30 "$HOST_LOG"
        exit 1
    fi
done

first_frame=$(grep -m1 'E2E_FIRST_FRAME' "$APP_LOG" || true)
decoder_kind=$(grep -m1 'E2E_DECODER' "$APP_LOG" || true)
last_stats=$(grep 'E2E_STATS' "$APP_LOG" | tail -1)

if ! grep -q "w=$SYNTH_W h=$SYNTH_H" <<<"$last_stats"; then
    echo "FAIL: decoded resolution mismatch: $last_stats (expected ${SYNTH_W}x${SYNTH_H})"
    exit 1
fi

if [ -z "$REMOTE_HOST" ] && [ "$CODEC" = "hevc" ] && ! grep -q "libx265" "$HOST_LOG"; then
    echo "FAIL: hevc requested but the host never opened an HEVC encoder session"
    echo "----- host log -----"; tail -30 "$HOST_LOG"
    exit 1
fi

fps=$(sed -E 's/.* fps=([0-9]+).*/\1/' <<<"$last_stats")
if [ "$fps" -lt "$MIN_FPS" ]; then
    echo "FAIL: decoded fps $fps is below $MIN_FPS"
    exit 1
fi
"$ROOT/scripts/screenshot.sh" "$UDID" "$SHOT"
xcrun swift "$ROOT/scripts/px.swift" "$SHOT" --video "$SIZE" --assert-pattern > "$OUT/pixels.json"
python3 "$ROOT/scripts/e2e_stats.py" "$APP_LOG" --output "$OUT/result.json" \
    --scenario "$SCENARIO" --screenshot "$SHOT" --elapsed "$((SECONDS - STARTED))" \
    --min-frames "$WANT_DECODED" --duration "$MIN_SECONDS" --min-fps "$MIN_FPS" \
    --max-drop-ratio "${EM_MAX_DROP_RATIO:-0.02}" --require-repairs "${EM_REQUIRE_REPAIRS:-0}"
echo "PASS: $(grep 'E2E_STATS' "$APP_LOG" | tail -1)"
echo "      $first_frame"
echo "      $decoder_kind"
echo "      Evidence: $OUT"
