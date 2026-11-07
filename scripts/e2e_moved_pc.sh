#!/usr/bin/env bash
# A saved PC changes address, as after a new lease from the router or a move
# between its cable and Wi-Fi. The real app pairs with the real host at this
# Mac's home network address, connecting as a tap on the PC in Bonjour's list
# would. The host is then killed and started on another port, which it
# advertises with Bonjour under the same name, and a silent socket takes the
# old address. The app should check that it is the same PC and stream again
# from the new port by itself (EM_EXPECT=move, the default), or keep trying
# the saved address and never contact the new one (EM_EXPECT=stay):
#   EM_PAIRING=off            the PC has pairing off, so nothing proves it
#   EM_MOVED_HOST_BIN=<path>  the PC comes back as a build before 0.3.5
#
# On a shared Mac this is an iOS e2e run: hold the machine's e2e lock, and use
# your own simulator (EM_SIM_UDID) and a private port (EM_PORT, and the next).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"

UDID="${EM_SIM_UDID:?set EM_SIM_UDID to your own simulator}"
PORT="${EM_PORT:-19893}"
NEW_PORT=$((PORT + 1))
EXPECT="${EM_EXPECT:-move}"
PAIRING="${EM_PAIRING:-on}"
TIMEOUT_SECS="${EM_TIMEOUT:-90}"
STAY_SECS="${EM_STAY_SECS:-45}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/moved-pc}"
HOST_BIN="${EM_HOST_BIN:-$ROOT/target/release/eternal-host}"
MOVED_HOST_BIN="${EM_MOVED_HOST_BIN:-$HOST_BIN}"
# The PC's name: its Bonjour service and HELLO_ACK both carry it.
PC_NAME="EMMovedPC$$"
# A test pairing token, written to the host's settings and handed to the
# app in a pairing link, as the PC's QR code would.
TOKEN="0123456789abcdef0123456789abcdef"
case "$EXPECT" in move|stay) ;; *) echo "Invalid EM_EXPECT: $EXPECT" >&2; exit 2;; esac
mkdir -p "$OUT"
rm -f "$OUT/result.json" "$OUT/old-address.count"
APP_LOG="$OUT/app.log"
HOST_LOG="$OUT/host.log"

# This Mac's address on its home network, read now: the move needs one.
IFACE=$(route -n get default 2>/dev/null | awk '/interface:/ {print $2}')
LAN=$(ipconfig getifaddr "$IFACE" 2>/dev/null || true)
python3 - "$LAN" <<'PY' || { echo "FAIL: no home network address on the default interface" >&2; exit 1; }
import ipaddress,sys
a=ipaddress.ip_address(sys.argv[1])
sys.exit(0 if a.version==4 and a.is_private and not a.is_loopback and a not in ipaddress.ip_network('100.64.0.0/10') else 1)
PY

HOST_PID=""
SILENT_PID=""
cleanup() {
    status=$?
    trap - EXIT
    [ -n "$SILENT_PID" ] && kill "$SILENT_PID" 2>/dev/null || true
    [ -n "$SILENT_PID" ] && wait "$SILENT_PID" 2>/dev/null || true
    [ -n "$HOST_PID" ] && kill -TERM "$HOST_PID" 2>/dev/null || true
    [ -n "$HOST_PID" ] && wait "$HOST_PID" 2>/dev/null || true
    xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
    if [ -f "$APP_LOG" ]; then
        cp "$APP_LOG" "$APP_LOG.snapshot" && mv "$APP_LOG.snapshot" "$APP_LOG" || status=1
    fi
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
[ -x "$HOST_BIN" ] && [ -x "$MOVED_HOST_BIN" ] || { echo "FAIL: host binary not found"; exit 1; }

echo "==> Installing app with nothing saved"
xcrun simctl bootstatus "$UDID" -b >/dev/null
xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
xcrun simctl uninstall "$UDID" com.eternal.monitor 2>/dev/null || true
# Pairings live in the simulator's Keychain and outlast the app.
xcrun simctl keychain "$UDID" reset
STAGE="$(mktemp -d /tmp/em-moved-pc-install.XXXXXX)"
ditto "$APP" "$STAGE/EternalMonitor.app"
xcrun simctl install "$UDID" "$STAGE/EternalMonitor.app"
rm -rf "$STAGE"
APP_DATA=$(xcrun simctl get_app_container "$UDID" com.eternal.monitor data)
SIM_LOG="$APP_DATA/tmp/eternal-e2e.log"
mkdir -p "$(dirname "$SIM_LOG")"
rm -f "$SIM_LOG" "$APP_LOG"
touch "$SIM_LOG"
ln "$SIM_LOG" "$APP_LOG"

mkdir -p "$OUT/state/EternalMonitor"
REQUIRE=$([ "$PAIRING" = on ] && echo true || echo false)
echo "{\"bitrate_mbps\": 8, \"target_fps\": 30, \"start_on_boot\": false, \"stream_audio\": false, \"require_pairing\": $REQUIRE, \"auth_token_hex\": \"$TOKEN\"}" \
    > "$OUT/state/EternalMonitor/settings.json"
start_host() {
    APPDATA="$OUT/state" COMPUTERNAME="$PC_NAME" ETERNAL_HEADLESS=1 ETERNAL_E2E_LOG=1 ETERNAL_USB_DIRECT= \
    ETERNAL_CAPTURE=synthetic ETERNAL_AUDIO=synthetic ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=30 \
        "$1" "$2" >>"$HOST_LOG" 2>&1 &
    HOST_PID=$!
}
frames_at_least() { [ "$(grep -c 'E2E_FIRST_FRAME' "$APP_LOG" || true)" -ge "$1" ]; }
wait_for() {
    local what=$1 limit=$2 elapsed=0
    shift 2
    until "$@"; do
        sleep 1
        elapsed=$((elapsed + 1))
        if [ "$elapsed" -ge "$limit" ]; then
            echo "FAIL: $what after ${limit}s"
            echo "----- app milestones -----"; grep -v E2E_STATS "$APP_LOG" | tail -40
            exit 1
        fi
    done
}

: > "$HOST_LOG"
echo "==> Starting host $PC_NAME on port $PORT (pairing $PAIRING)"
start_host "$HOST_BIN" "$PORT"
echo "==> Launching app: pairs and streams from the home network address, port $PORT"
LINK="$LAN:$PORT"
[ "$PAIRING" = on ] && LINK="eternaldisplay://$LINK?t=$TOKEN"
SIMCTL_CHILD_EM_AUTOCONNECT="$LINK" SIMCTL_CHILD_EM_AUTOCONNECT_FOUND=1 SIMCTL_CHILD_EM_E2E_LOG=1 SIMCTL_CHILD_EM_USB_PORT=0 \
    xcrun simctl launch "$UDID" com.eternal.monitor -didSeeOnboarding YES -allowUSB NO -autoReconnect YES -playPCaudio NO >/dev/null
wait_for "no first stream" "$TIMEOUT_SECS" frames_at_least 1

echo "==> Moving the PC to port $NEW_PORT; the old address goes silent"
MOVED_AT=$(wc -l < "$APP_LOG")
kill -KILL "$HOST_PID"
wait "$HOST_PID" 2>/dev/null || true
python3 -u - "$LAN" "$PORT" "$OUT/old-address.count" <<'PY' &
import signal,socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
s.bind((sys.argv[1],int(sys.argv[2])))
count=0
def done(*_):
    open(sys.argv[3],'w').write(str(count)); sys.exit(0)
signal.signal(signal.SIGTERM,done)
while True:
    s.recv(4096); count+=1
PY
SILENT_PID=$!
start_host "$MOVED_HOST_BIN" "$NEW_PORT"

if [ "$EXPECT" = move ]; then
    echo "==> Waiting for the app to stream again by itself (timeout ${TIMEOUT_SECS}s)"
    wait_for "no stream after the move" "$TIMEOUT_SECS" frames_at_least 2
    sleep 2
else
    echo "==> Watching for ${STAY_SECS}s: the app must not move"
    sleep "$STAY_SECS"
fi
kill "$SILENT_PID"; wait "$SILENT_PID" 2>/dev/null || true; SILENT_PID=""

python3 - "$APP_LOG" "$MOVED_AT" "$PORT" "$NEW_PORT" "$OUT/old-address.count" "$OUT/result.json" "$EXPECT" "$HOST_LOG" <<'PY'
import json,pathlib,re,sys
log,moved_at,port,new_port,count,result,expect,host_log=sys.argv[1:9]
lines=pathlib.Path(log).read_text(errors='replace').splitlines()[int(moved_at):]
starts=[m.group(1) for l in lines if (m:=re.search(r'Starting connection attempt=\S+ transport=WiFi to \S+:(\d+) ', l))]
frames=[i for i,l in enumerate(lines) if 'E2E_FIRST_FRAME' in l]
old=int(pathlib.Path(count).read_text())
host=pathlib.Path(host_log).read_text(errors='replace')
errors=[]
if port not in starts: errors.append('never retried the saved address')
if old==0: errors.append('the saved address got no HELLO2')
if expect=='move':
    before=[m.group(1) for l in lines[:frames[0]] if (m:=re.search(r'Starting connection attempt=\S+ transport=WiFi to \S+:(\d+) ', l))] if frames else []
    if not frames: errors.append('no stream after the move')
    elif before[-1]!=new_port: errors.append('streamed again, but not from the new port')
    if not any('It is the saved PC: connecting there' in l for l in lines): errors.append('no identity check before the move')
else:
    if frames: errors.append('streamed again although it must not move')
    if new_port in starts: errors.append('connected to the new port')
result_data=dict(scenario='moved-pc', expect=expect, status='FAIL' if errors else 'PASS', errors=errors,
                 attempts_after_move=len(starts), attempts_at_new_port=starts.count(new_port),
                 datagrams_at_old_address=old,
                 identity_checks=sum('Checking that the PC at' in l for l in lines),
                 lookups=sum('Looking for the saved PC by name' in l for l in lines))
pathlib.Path(result).write_text(json.dumps(result_data, indent=2)+'\n')
print(json.dumps(result_data))
sys.exit(1 if errors else 0)
PY
xcrun simctl io "$UDID" screenshot "$OUT/after-move.png" >/dev/null 2>&1 || true
echo "PASS ($EXPECT). Evidence: $OUT"
