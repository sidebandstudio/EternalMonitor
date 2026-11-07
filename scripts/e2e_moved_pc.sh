#!/usr/bin/env bash
# A saved PC changes address, as after a new lease from the router or a move
# between its cable and Wi-Fi. The real app streams from the real host at
# 127.0.0.1. The host is then killed and started again, and a silent socket
# takes 127.0.0.1:PORT: the saved address stops answering, while the PC
# still answers at its network addresses and advertises them with Bonjour
# under the same name. Success = the app streams again from another address
# by itself, after its HELLO2s to the old one went unanswered.
#
# On a shared Mac this is an iOS e2e run: hold the machine's e2e lock, and use
# your own simulator (EM_SIM_UDID) and a private port (EM_PORT).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"

UDID="${EM_SIM_UDID:?set EM_SIM_UDID to your own simulator}"
PORT="${EM_PORT:-19893}"
TIMEOUT_SECS="${EM_TIMEOUT:-90}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/moved-pc}"
HOST_BIN="${EM_HOST_BIN:-$ROOT/target/release/eternal-host}"
# The PC's name: its Bonjour service and HELLO_ACK both carry it.
PC_NAME="EMMovedPC$$"
mkdir -p "$OUT"
rm -f "$OUT/result.json" "$OUT/old-address.count"
APP_LOG="$OUT/app.log"
HOST_LOG="$OUT/host.log"

HOST_PID=""
SILENT_PID=""
APP_PID=""
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
[ -x "$HOST_BIN" ] || { echo "FAIL: host binary not found at $HOST_BIN"; exit 1; }

echo "==> Installing app with nothing saved"
xcrun simctl bootstatus "$UDID" -b >/dev/null
xcrun simctl terminate "$UDID" com.eternal.monitor 2>/dev/null || true
xcrun simctl uninstall "$UDID" com.eternal.monitor 2>/dev/null || true
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
echo '{"bitrate_mbps": 8, "target_fps": 30, "start_on_boot": false, "stream_audio": false, "require_pairing": false}' \
    > "$OUT/state/EternalMonitor/settings.json"
start_host() {
    APPDATA="$OUT/state" COMPUTERNAME="$PC_NAME" ETERNAL_HEADLESS=1 ETERNAL_E2E_LOG=1 ETERNAL_USB_DIRECT= \
    ETERNAL_CAPTURE=synthetic ETERNAL_AUDIO=synthetic ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=30 \
        "$HOST_BIN" "$PORT" >>"$HOST_LOG" 2>&1 &
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
            echo "----- app milestones -----"; tail -40 "$APP_LOG"
            exit 1
        fi
    done
}

: > "$HOST_LOG"
echo "==> Starting host $PC_NAME on 0.0.0.0:$PORT"
start_host
echo "==> Launching app, streaming from 127.0.0.1:$PORT"
LAUNCH=$(SIMCTL_CHILD_EM_AUTOCONNECT="127.0.0.1:$PORT" SIMCTL_CHILD_EM_E2E_LOG=1 SIMCTL_CHILD_EM_USB_PORT=0 \
    xcrun simctl launch "$UDID" com.eternal.monitor -didSeeOnboarding YES -allowUSB NO -autoReconnect YES -playPCaudio NO)
APP_PID="${LAUNCH##*: }"
wait_for "no first stream" "$TIMEOUT_SECS" frames_at_least 1

echo "==> Moving the PC: kill, restart, and silence 127.0.0.1:$PORT"
MOVED_AT=$(wc -l < "$APP_LOG")
kill -KILL "$HOST_PID"
wait "$HOST_PID" 2>/dev/null || true
start_host
wait_for "restarted host never bound UDP $PORT" 20 sh -c "lsof -nP -a -p $HOST_PID -iUDP:$PORT >/dev/null"
# More specific than the host's wildcard socket, so it takes every datagram
# sent to 127.0.0.1:PORT, and answers none.
python3 -u - "$PORT" "$OUT/old-address.count" <<'PY' &
import signal,socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
s.bind(('127.0.0.1',int(sys.argv[1])))
count=0
def done(*_):
    open(sys.argv[2],'w').write(str(count)); sys.exit(0)
signal.signal(signal.SIGTERM,done)
while True:
    s.recv(4096); count+=1
PY
SILENT_PID=$!

echo "==> Waiting for the app to stream again by itself (timeout ${TIMEOUT_SECS}s)"
wait_for "no stream after the move" "$TIMEOUT_SECS" frames_at_least 2
sleep 2
kill "$SILENT_PID"; wait "$SILENT_PID" 2>/dev/null || true; SILENT_PID=""

python3 - "$APP_LOG" "$MOVED_AT" "$PORT" "$OUT/old-address.count" "$OUT/result.json" <<'PY'
import json,pathlib,re,sys
lines=pathlib.Path(sys.argv[1]).read_text(errors='replace').splitlines()[int(sys.argv[2]):]
port=sys.argv[3]
starts=[m.group(1) for l in lines if (m:=re.search(r'Starting connection attempt=\S+ transport=WiFi to (\S+):'+port+' ', l))]
frame=next(i for i,l in enumerate(lines) if 'E2E_FIRST_FRAME' in l)
last=[m.group(1) for l in lines[:frame] if (m:=re.search(r'Starting connection attempt=\S+ transport=WiFi to (\S+):'+port+' ', l))][-1]
old=int(pathlib.Path(sys.argv[4]).read_text())
errors=[]
if '127.0.0.1' not in starts: errors.append('never retried the saved address')
if old==0: errors.append('the saved address got no HELLO2')
if last=='127.0.0.1': errors.append('streamed again from the saved address, not a new one')
if not any('Found the saved PC at ' in l for l in lines[:frame]): errors.append('no Bonjour lookup')
result=dict(scenario='moved-pc', status='FAIL' if errors else 'PASS', errors=errors,
            attempts_after_move=len(starts), datagrams_at_old_address=old, streamed_from_loopback=last=='127.0.0.1')
pathlib.Path(sys.argv[5]).write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps(result))
sys.exit(1 if errors else 0)
PY
xcrun simctl io "$UDID" screenshot "$OUT/streaming-after-move.png" >/dev/null 2>&1 || true
echo "PASS: streamed again after the move. Evidence: $OUT"
