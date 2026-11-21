#!/usr/bin/env bash
# End-to-end pointer test on a Mac (#183): the real host binary with the
# synthetic pointer scene (a still picture, the pointer moving every frame)
# streams over localhost UDP to the real iPad app in the Simulator.
#
# Two runs, counted from the app's E2E_POINTER lines over EM_WINDOW seconds:
# - draws:  the app draws the pointer. It must move on the iPad (positions
#           applied and drawn) while almost no video frames decode: only
#           the host's idle keepalive.
# - legacy: EM_DRAWS_POINTER=0, as apps before 0.3.5. No pointer messages,
#           and every move is a decoded video frame, as before.
#
# Requirements as for e2e_ios.sh. Uses EM_SIM_UDID (or EM_SIM_NAME), a
# private EM_PORT and a USB listener on a free port, so it shares no fixed
# port with other simulators.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"

SIM_NAME="${EM_SIM_NAME:-iPad Pro 11-inch (M4)}"
PORT="${EM_PORT:-19936}"
WINDOW="${EM_WINDOW:-10}"
FPS="${ETERNAL_FPS:-60}"
SIZE="${EM_SIZE:-1280x720}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/pointer}"
mkdir -p "$OUT"
rm -f "$OUT/result.json"

UDID="${EM_SIM_UDID:-}"
if [ -z "$UDID" ]; then
UDID=$(xcrun simctl list -j devices available | /usr/bin/python3 -c '
import json, sys
for devices in json.load(sys.stdin)["devices"].values():
    for device in devices:
        if device["name"] == sys.argv[1]:
            print(device["udid"]); sys.exit(0)
sys.exit(1)
' "$SIM_NAME")
fi

HOST_PID=""
cleanup() {
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
    xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
}
trap cleanup EXIT

if [ "${EM_SKIP_BUILD:-0}" != 1 ]; then
    echo "==> Building app and host"
    (cd "$ROOT/ios" && xcodegen generate >/dev/null)
    xcodebuild build -project "$ROOT/ios/EternalMonitor.xcodeproj" -scheme EternalMonitor \
        -configuration Release -destination "platform=iOS Simulator,id=$UDID" \
        -derivedDataPath "$ROOT/ios/build/e2e" CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- -quiet
    (cd "$ROOT" && cargo build -q --release -p eternal-host --locked)
fi
APP="$ROOT/ios/build/e2e/Build/Products/Release-iphonesimulator/EternalMonitor.app"
[ -d "$APP" ] || { echo "FAIL: app bundle not found at $APP"; exit 1; }

xcrun simctl boot "$UDID" 2>/dev/null || true
xcrun simctl bootstatus "$UDID" -b >/dev/null
INSTALL_DIR="$(mktemp -d /tmp/em-e2e-pointer.XXXXXX)"
ditto "$APP" "$INSTALL_DIR/EternalMonitor.app"
xcrun simctl install "$UDID" "$INSTALL_DIR/EternalMonitor.app"
rm -rf "$INSTALL_DIR"

run_mode() {
    local mode=$1 draws=$2
    local dir="$OUT/$mode"
    mkdir -p "$dir/state/EternalMonitor"
    python3 - "$dir/state/EternalMonitor/settings.json" "$FPS" <<'PY'
import json, sys
json.dump(dict(bitrate_mbps=15.0, target_fps=int(sys.argv[2]), start_on_boot=False,
               stream_audio=False, require_pairing=False), open(sys.argv[1], 'w'))
PY
    xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
    local data
    data=$(xcrun simctl get_app_container "$UDID" com.eternal.monitor data)
    local sim_log="$data/tmp/eternal-e2e.log"
    rm -f "$sim_log"

    echo "==> [$mode] host on 127.0.0.1:$PORT (synthetic pointer, ${SIZE}, ${FPS} fps)"
    APPDATA="$dir/state" ETERNAL_HEADLESS=1 ETERNAL_E2E_LOG=1 ETERNAL_CAPTURE=synthetic \
    ETERNAL_SYNTH_POINTER=1 ETERNAL_SYNTH_SIZE="$SIZE" ETERNAL_ENCODER=libx264 ETERNAL_FPS="$FPS" \
        "$ROOT/target/release/eternal-host" "$PORT" >"$dir/host.log" 2>&1 &
    HOST_PID=$!
    sleep 2

    echo "==> [$mode] app, EM_DRAWS_POINTER=$draws"
    SIMCTL_CHILD_EM_AUTOCONNECT="127.0.0.1:$PORT" SIMCTL_CHILD_EM_E2E_LOG=1 \
    SIMCTL_CHILD_EM_USB_PORT=0 SIMCTL_CHILD_EM_DRAWS_POINTER="$draws" \
        xcrun simctl launch "$UDID" com.eternal.monitor -didSeeOnboarding YES -allowUSB NO \
        -playPCaudio NO -targetFPS "$FPS" >/dev/null

    local waited=0
    until grep -q E2E_FIRST_FRAME "$sim_log" 2>/dev/null; do
        sleep 1; waited=$((waited + 1))
        [ "$waited" -lt 60 ] || { echo "FAIL [$mode]: no first frame"; exit 1; }
    done
    # Let the first frame's keyframe settle, then count over the window.
    sleep 2
    local start end
    start=$(grep E2E_POINTER "$sim_log" | tail -1)
    sleep "$WINDOW"
    end=$(grep E2E_POINTER "$sim_log" | tail -1)
    xcrun simctl io "$UDID" screenshot "$dir/screen.png" >/dev/null 2>&1 || true
    cp "$sim_log" "$dir/app.log"
    xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
    kill "$HOST_PID" 2>/dev/null || true
    wait "$HOST_PID" 2>/dev/null || true
    HOST_PID=""

    python3 - "$mode" "$WINDOW" "$start" "$end" "$dir/host.log" "$OUT/$mode.json" <<'PY'
import json, re, sys
mode, window, start, end, host_log, out = sys.argv[1], float(sys.argv[2]), *sys.argv[3:]
field = lambda line, name: int(re.search(rf"\b{name}=(\d+)", line).group(1))
delta = {name: field(end, name) - field(start, name) for name in ("states", "drawn", "moves", "decoded", "shapes")}
sent = [int(m) for m in re.findall(r"pointer_sent=(\d+)", re.sub(r"\x1b\[[0-9;]*m", "", open(host_log).read()))]
result = dict(mode=mode, window_s=window, **delta, pointer_states_per_s=round(delta["states"] / window, 1),
              moves_per_s=round(delta["moves"] / window, 1), frames_per_s=round(delta["decoded"] / window, 2),
              host_pointer_sent_last_summary=sent[-1] if sent else None, shapes_total=field(end, "shapes"))
json.dump(result, open(out, "w"), indent=1)
print(json.dumps(result))
problems = []
if mode == "draws":
    if delta["moves"] < 15 * window: problems.append("the pointer barely moved on the iPad")
    if field(end, "shapes") < 1: problems.append("no pointer image arrived")
    # The 750 ms keepalive, and nothing for the moves.
    if delta["decoded"] > window / 0.75 + 3: problems.append("video frames decoded while only the pointer moved")
else:
    if field(end, "states") or field(end, "drawn"): problems.append("an older app got pointer messages")
    if delta["decoded"] < 15 * window: problems.append("pointer moves did not arrive as video frames")
if problems:
    print(f"FAIL [{mode}]: " + "; ".join(problems)); sys.exit(1)
print(f"PASS [{mode}]")
PY
}

echo "load: $(uptime | sed 's/.*load averages*: //')" | tee "$OUT/load.txt"
run_mode draws 1
run_mode legacy 0
echo "load: $(uptime | sed 's/.*load averages*: //')" | tee -a "$OUT/load.txt"
python3 -c 'import json,sys; json.dump([json.load(open(p)) for p in sys.argv[2:]], open(sys.argv[1], "w"), indent=1)' \
    "$OUT/result.json" "$OUT/draws.json" "$OUT/legacy.json"
echo "PASS: $OUT/result.json"
