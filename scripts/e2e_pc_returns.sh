#!/usr/bin/env bash
# The PC app isn't open when the iPad connects, and opens afterwards. The real
# app streams once from the real host, so it knows the PC's name and pairing.
# The host is then stopped and the app opened again: it connects to the saved
# PC, which doesn't answer, and shows "didn't answer". The host then starts
# again and advertises with Bonjour, and the app should connect and stream by
# itself, with no tap. The app opens a second after the host stopped, while
# Bonjour may still list the PC unchanged: the listing is asked until the PC
# proves its pairing, so a return that changes nothing on Bonjour still counts.
#
# On a shared Mac this is an iOS e2e run: hold the machine's e2e lock and use
# your own simulator (EM_SIM_UDID). The PC's port is picked free when the run
# starts; EM_PORT chooses it instead, and the run stops before anything starts
# if it is taken.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"

UDID="${EM_SIM_UDID:?set EM_SIM_UDID to your own simulator}"
TIMEOUT_SECS="${EM_TIMEOUT:-90}"
# How long the card is up before the PC app opens.
CLOSED_SECS="${EM_CLOSED_SECS:-8}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/pc-returns}"
HOST_BIN="${EM_HOST_BIN:-$ROOT/target/release/eternal-host}"
# The PC's name: its Bonjour service and HELLO_ACK both carry it. A new one
# each run, so no earlier run's Bonjour record stands in for it.
PC_NAME="EMReturnPC$$"
# A test pairing token, written to the host's settings and handed to the
# app in a pairing link, as the PC's QR code would.
TOKEN="0123456789abcdef0123456789abcdef"
mkdir -p "$OUT"
rm -f "$OUT/result.json"
APP_LOG="$OUT/app.log"
HOST_LOG="$OUT/host.log"

# This Mac's address on its home network, as an iPad on the same Wi-Fi dials it.
IFACE=$(route -n get default 2>/dev/null | awk '/interface:/ {print $2}')
LAN=$(ipconfig getifaddr "$IFACE" 2>/dev/null || true)
[ -n "$LAN" ] || { echo "FAIL: no address on the default interface" >&2; exit 1; }

PORT=$(python3 - "${EM_PORT:-0}" <<'PY'
import socket,sys
wanted=int(sys.argv[1])
try:
    udp=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); udp.bind(('0.0.0.0',wanted))
    port=udp.getsockname()[1]
    tcp=socket.socket(socket.AF_INET,socket.SOCK_STREAM); tcp.bind(('0.0.0.0',port))
except OSError:
    print(f"port {wanted} is in use"); sys.exit(1)
print(port)
PY
) || { echo "FAIL: $PORT" >&2; exit 1; }

HOST_PID=""
cleanup() {
    status=$?
    trap - EXIT
    [ -n "$HOST_PID" ] && kill -TERM "$HOST_PID" 2>/dev/null || true
    [ -n "$HOST_PID" ] && wait "$HOST_PID" 2>/dev/null || true
    xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
    python3 - "$OUT/result.json" "$status" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
r=json.loads(p.read_text()) if p.exists() else {}
r.setdefault('status', 'PASS' if sys.argv[2]=='0' else 'FAIL')
p.write_text(json.dumps(r, indent=2)+'\n')
PY
    [ "$status" -eq 0 ] || echo "FAIL: evidence at $OUT" >&2
    exit "$status"
}
trap cleanup EXIT

if [ "${EM_SKIP_BUILD:-0}" != 1 ]; then
    echo "==> Building app"
    (cd "$ROOT/ios" && xcodegen generate >/dev/null)
    xcodebuild build -project "$ROOT/ios/EternalMonitor.xcodeproj" -scheme EternalMonitor -configuration Release \
        -destination "platform=iOS Simulator,id=$UDID" -derivedDataPath "$ROOT/ios/build/e2e" \
        CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- -quiet
    if [ -z "${EM_HOST_BIN:-}" ]; then
        echo "==> Building host"
        (cd "$ROOT" && cargo build -q --release -p eternal-host --locked)
    fi
fi
APP="$ROOT/ios/build/e2e/Build/Products/Release-iphonesimulator/EternalMonitor.app"
[ -d "$APP" ] || { echo "FAIL: app bundle not found at $APP"; exit 1; }
[ -x "$HOST_BIN" ] || { echo "FAIL: host binary not found"; exit 1; }

echo "==> Installing app with nothing saved"
xcrun simctl bootstatus "$UDID" -b >/dev/null
xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
xcrun simctl uninstall "$UDID" com.eternal.monitor 2>/dev/null || true
# Pairings live in the simulator's Keychain and outlast the app.
xcrun simctl keychain "$UDID" reset
STAGE="$(mktemp -d /tmp/em-pc-returns-install.XXXXXX)"
ditto "$APP" "$STAGE/EternalMonitor.app"
xcrun simctl install "$UDID" "$STAGE/EternalMonitor.app"
rm -rf "$STAGE"
APP_DATA=$(xcrun simctl get_app_container "$UDID" com.eternal.monitor data)
SIM_LOG="$APP_DATA/tmp/eternal-e2e.log"
mkdir -p "$(dirname "$SIM_LOG")"

mkdir -p "$OUT/state/EternalMonitor"
echo "{\"bitrate_mbps\": 8, \"target_fps\": 30, \"start_on_boot\": false, \"stream_audio\": false, \"require_pairing\": true, \"auth_token_hex\": \"$TOKEN\"}" \
    > "$OUT/state/EternalMonitor/settings.json"
start_host() {
    APPDATA="$OUT/state" COMPUTERNAME="$PC_NAME" ETERNAL_HEADLESS=1 ETERNAL_E2E_LOG=1 ETERNAL_USB_DIRECT= \
    ETERNAL_CAPTURE=synthetic ETERNAL_AUDIO=synthetic ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=30 \
        "$HOST_BIN" "$PORT" >>"$HOST_LOG" 2>&1 &
    HOST_PID=$!
}
stop_host() {
    # A stopped app says goodbye on Bonjour, as closing it on the PC does.
    kill -TERM "$HOST_PID"
    wait "$HOST_PID" 2>/dev/null || true
    HOST_PID=""
}
# The app writes its log afresh at each launch.
launch_app() {
    rm -f "$SIM_LOG"
    touch "$SIM_LOG"
    SIMCTL_CHILD_EM_AUTOCONNECT="$1" SIMCTL_CHILD_EM_E2E_LOG=1 SIMCTL_CHILD_EM_USB_PORT=0 \
        xcrun simctl launch "$UDID" com.eternal.monitor -didSeeOnboarding YES -allowUSB NO -autoReconnect NO \
        -autoResumeOnForeground NO -playPCaudio NO >/dev/null
}
seen() { grep -q "$1" "$SIM_LOG"; }
wait_for() {
    local what=$1 limit=$2 elapsed=0
    shift 2
    until "$@"; do
        sleep 1
        elapsed=$((elapsed + 1))
        if [ "$elapsed" -ge "$limit" ]; then
            echo "FAIL: $what after ${limit}s"
            echo "----- app milestones -----"; grep -v E2E_STATS "$SIM_LOG" | tail -40
            cp "$SIM_LOG" "$APP_LOG" 2>/dev/null || true
            exit 1
        fi
    done
}

: > "$HOST_LOG"
echo "==> Pairing with $PC_NAME at port $PORT and streaming once"
start_host
launch_app "eternaldisplay://$LAN:$PORT?t=$TOKEN"
wait_for "no first stream" "$TIMEOUT_SECS" seen E2E_FIRST_FRAME
cp "$SIM_LOG" "$OUT/app-first.log"

echo "==> Closing the PC app, then opening the iPad app on the saved PC"
stop_host
xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
launch_app "$LAN:$PORT"
wait_for "no \"didn't answer\" card" "$TIMEOUT_SECS" seen "pc_return result=watching"
sleep "$CLOSED_SECS"
xcrun simctl io "$UDID" screenshot "$OUT/didnt-answer.png" >/dev/null 2>&1 || true
seen E2E_FIRST_FRAME && { echo "FAIL: streamed with the PC app closed"; exit 1; }

echo "==> Opening the PC app; the iPad should connect by itself (timeout ${TIMEOUT_SECS}s)"
OPENED_AT=$(wc -l < "$SIM_LOG")
start_host
wait_for "no stream after the PC app opened" "$TIMEOUT_SECS" seen E2E_FIRST_FRAME
sleep 2
xcrun simctl io "$UDID" screenshot "$OUT/streaming.png" >/dev/null 2>&1 || true
cp "$SIM_LOG" "$APP_LOG"

python3 - "$APP_LOG" "$OPENED_AT" "$OUT/result.json" <<'PY'
import json,pathlib,sys
log,opened_at,result=sys.argv[1:4]
lines=pathlib.Path(log).read_text(errors='replace').splitlines()
before,after=lines[:int(opened_at)],lines[int(opened_at):]
attempt=lambda l: 'Starting connection attempt=' in l and 'transport=WiFi' in l
errors=[]
if not any('outcome=no_reply' in l for l in before): errors.append('no no_reply summary before the PC app opened')
if sum(map(attempt, before))!=1: errors.append('more than one attempt while the PC app was closed')
tries=sum('pc_return result=tried' in l for l in after)
if tries!=1: errors.append(f'{tries} automatic tries after the PC app opened, not 1')
if not any('E2E_FIRST_FRAME' in l for l in after): errors.append('no stream after the PC app opened')
data=dict(scenario='pc-returns', status='FAIL' if errors else 'PASS', errors=errors,
          attempts_while_closed=sum(map(attempt, before)), automatic_tries=tries,
          attempts_after_opening=sum(map(attempt, after)))
pathlib.Path(result).write_text(json.dumps(data, indent=2)+'\n')
print(json.dumps(data))
sys.exit(1 if errors else 0)
PY
echo "PASS. Evidence: $OUT"
