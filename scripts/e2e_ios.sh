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
TRANSPORT="${EM_TRANSPORT:-udp}"
AUDIO="${EM_AUDIO:-0}"
case "$TRANSPORT" in udp|usb|takeover) ;; *) echo "Invalid EM_TRANSPORT: $TRANSPORT" >&2; exit 2;; esac
SIZE="${EM_SIZE:-640x360}"
SYNTH_W="${SIZE%x*}"
SYNTH_H="${SIZE#*x}"
SCENARIO="${EM_SCENARIO:-$CODEC-$TRANSPORT}"
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
MONITOR_PID=""
PROXY_PID=""
PROFILE_PID=""
UDID=""
INSTALL_DIR=""
cleanup() {
    status=$?
    [ -n "$PROFILE_PID" ] && kill "$PROFILE_PID" 2>/dev/null || true
    [ -n "$PROFILE_PID" ] && wait "$PROFILE_PID" 2>/dev/null || true
    [ -n "$LOG_PID" ] && kill "$LOG_PID" 2>/dev/null || true
    [ -n "$MONITOR_PID" ] && kill "$MONITOR_PID" 2>/dev/null || true
    [ -n "$MONITOR_PID" ] && wait "$MONITOR_PID" 2>/dev/null || true
    [ -n "$PROXY_PID" ] && kill "$PROXY_PID" 2>/dev/null || true
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

# Resolve the exact simulator before building. A name-only Xcode destination
# implicitly selects OS:latest, which may differ from the runtime under test.
UDID="${EM_SIM_UDID:-}"
if [ -z "$UDID" ]; then
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
fi

if [ "${EM_SKIP_BUILD:-0}" != 1 ]; then
echo "==> Generating Xcode project"
(cd "$ROOT/ios" && xcodegen generate >/dev/null)

echo "==> Building optimized app for simulator measurements"
xcodebuild build \
    -project "$ROOT/ios/EternalMonitor.xcodeproj" \
    -scheme EternalMonitor -configuration Release \
    -destination "platform=iOS Simulator,id=$UDID" \
    -derivedDataPath "$ROOT/ios/build/e2e" \
    CODE_SIGNING_ALLOWED=NO -quiet
echo "==> Building host"
(cd "$ROOT" && cargo build -q --release -p eternal-host --locked)
fi
APP="$ROOT/ios/build/e2e/Build/Products/Release-iphonesimulator/EternalMonitor.app"
[ -d "$APP" ] || { echo "FAIL: app bundle not found at $APP"; exit 1; }

CONNECT_HOST="${REMOTE_HOST:-127.0.0.1}"
USB_DIRECT=""
MEASURE_LINK=udp
AUTOCONNECT="$CONNECT_HOST:$PORT"
case "$TRANSPORT" in
    usb) USB_DIRECT=127.0.0.1:9877; MEASURE_LINK=usb; AUTOCONNECT="" ;;
    takeover) USB_DIRECT=127.0.0.1:19873; MEASURE_LINK=usb ;;
esac
MEASUREMENT_ARGS=(--link "$MEASURE_LINK")
if [ "$TRANSPORT" = takeover ]; then MEASUREMENT_ARGS+=(--max-switch-ms 1000); fi

echo "==> Booting simulator: $SIM_NAME"

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

HEVC_FLAG=0
[ "$CODEC" = "hevc" ] && HEVC_FLAG=1
if [ -z "$REMOTE_HOST" ]; then
mkdir -p "$OUT/state/EternalMonitor"
python3 - "$OUT/state/EternalMonitor/settings.json" "${EM_BITRATE_MBPS:-15}" "$AUDIO" <<'PY'
import json,sys
with open(sys.argv[1], 'w') as f:
    json.dump(dict(bitrate_mbps=float(sys.argv[2]), target_fps=60, start_on_boot=False,
                   stream_audio=sys.argv[3]=='1'), f)
PY
echo "==> Starting host on 127.0.0.1:$PORT (synthetic ${SYNTH_W}x${SYNTH_H}, codec=$CODEC, headless)"
APPDATA="$OUT/state" \
ETERNAL_HEADLESS=1 \
ETERNAL_E2E_LOG=1 \
ETERNAL_USB_DIRECT="$USB_DIRECT" \
ETERNAL_CAPTURE=synthetic \
ETERNAL_AUDIO=synthetic \
ETERNAL_SYNTH_SIZE="${SYNTH_W}x${SYNTH_H}" \
ETERNAL_ENCODER=libx264 \
ETERNAL_HEVC="$HEVC_FLAG" \
ETERNAL_FPS="${ETERNAL_FPS:-60}" \
    "$ROOT/target/release/eternal-host" "$PORT" >"$HOST_LOG" 2>&1 &
HOST_PID=$!
fi

python3 - "$OUT/resources.log" <<'PY' &
import datetime, signal, subprocess, sys, time
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
with open(sys.argv[1], 'w') as out:
    while True:
        out.write(datetime.datetime.now(datetime.timezone.utc).isoformat()+'\n')
        out.flush()
        # Names and resource use only; command arguments may contain secrets.
        subprocess.run(['ps','-axo','pid,pcpu,pmem,comm'], stdout=out, check=True)
        time.sleep(2)
PY
MONITOR_PID=$!

echo "==> Launching app: transport=$TRANSPORT, autoconnect=$AUTOCONNECT"
SIMCTL_CHILD_EM_AUTOCONNECT="$AUTOCONNECT" \
SIMCTL_CHILD_EM_E2E_LOG=1 \
SIMCTL_CHILD_EM_UDP_BACKEND="${EM_UDP_BACKEND:-}" \
    xcrun simctl launch "$UDID" com.eternal.monitor -didSeeOnboarding YES -allowUSB YES -playPCaudio "$AUDIO" >/dev/null

if [ "$TRANSPORT" = takeover ]; then
    echo "==> Waiting for WiFi frames before attaching the USB fixture"
    elapsed=0
    until python3 "$ROOT/scripts/e2e_stats.py" "$APP_LOG" --link udp --min-frames 120; do
        sleep 1
        elapsed=$((elapsed + 1))
        [ "$elapsed" -lt "$TIMEOUT_SECS" ] || { echo "FAIL: initial WiFi stream missing"; exit 1; }
    done
    python3 "$ROOT/scripts/usb_proxy.py" > "$OUT/usb-proxy.log" 2>&1 &
    PROXY_PID=$!
fi

# Optional bounded stack sampling for throughput investigations. The sampler
# targets only this row's host and records stacks, never process arguments.
if [ "${EM_PROFILE_HOST:-0}" = 1 ] && [ -n "$HOST_PID" ]; then
    sample "$HOST_PID" 5 10 -file "$OUT/host-stacks.txt" > "$OUT/profile.log" 2>&1 &
    PROFILE_PID=$!
fi

echo "==> Waiting for $WANT_DECODED decoded frames (timeout ${TIMEOUT_SECS}s)"
elapsed=0
decoded=0
until python3 "$ROOT/scripts/e2e_stats.py" "$APP_LOG" --link "$MEASURE_LINK" --min-frames "$WANT_DECODED" --duration "$MIN_SECONDS"; do
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

# Freeze the measured interval before screenshot and pixel-analysis work can
# consume CPU on the same machine as the simulator and synthetic host.
MEASURED_LOG="$OUT/measured.log"
cp "$APP_LOG" "$MEASURED_LOG"

first_frame=$(grep 'E2E_FIRST_FRAME' "$MEASURED_LOG" | grep -m1 "link=$MEASURE_LINK" || true)
decoder_kind=$(grep -m1 'E2E_DECODER' "$MEASURED_LOG" || true)
last_stats=$(grep 'E2E_STATS' "$MEASURED_LOG" | tail -1)
if [ -z "$first_frame" ]; then
    echo "FAIL: no first frame on $MEASURE_LINK"; exit 1
fi
if [ "$TRANSPORT" = takeover ] && ! grep -q 'superseding session in place' "$HOST_LOG"; then
    echo "FAIL: host did not preserve the session during USB takeover"; exit 1
fi

if ! grep -q "w=$SYNTH_W h=$SYNTH_H" <<<"$last_stats"; then
    echo "FAIL: decoded resolution mismatch: $last_stats (expected ${SYNTH_W}x${SYNTH_H})"
    exit 1
fi

if [ -z "$REMOTE_HOST" ] && [ "$CODEC" = "hevc" ] && ! grep -q "libx265" "$HOST_LOG"; then
    echo "FAIL: hevc requested but the host never opened an HEVC encoder session"
    echo "----- host log -----"; tail -30 "$HOST_LOG"
    exit 1
fi

"$ROOT/scripts/screenshot.sh" "$UDID" "$SHOT"
xcrun swift "$ROOT/scripts/px.swift" "$SHOT" --video "$SIZE" --assert-pattern > "$OUT/pixels.json"
python3 "$ROOT/scripts/e2e_stats.py" "$MEASURED_LOG" --output "$OUT/result.json" \
    --scenario "$SCENARIO" --screenshot "$SHOT" --elapsed "$((SECONDS - STARTED))" \
    --min-frames "$WANT_DECODED" --duration "$MIN_SECONDS" --min-fps "$MIN_FPS" \
    "${MEASUREMENT_ARGS[@]}" \
    --max-drop-ratio "${EM_MAX_DROP_RATIO:-0.02}" --require-repairs "${EM_REQUIRE_REPAIRS:-0}"
if [ "$AUDIO" = 1 ]; then
    python3 "$ROOT/scripts/e2e_audio_stats.py" "$MEASURED_LOG" --output "$OUT/result.json"
fi
echo "PASS: $(grep 'E2E_STATS' "$MEASURED_LOG" | tail -1)"
echo "      $first_frame"
echo "      $decoder_kind"
echo "      Evidence: $OUT"
