#!/usr/bin/env bash
# Run unit/UI tests and retain the result bundle, screenshots, and pixel checks.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
UDID="${EM_SIM_UDID:-06416ADB-C33D-4EE4-82DB-44FCD663362F}"
# CoreSimulator can stall cloning a runner out of a Desktop checkout on a
# remote Mac. Keep test products in the temporary directory, outside Desktop.
DERIVED="${EM_DERIVED_DATA:-/tmp/eternalmonitor-ios-tests}"
STAMP="${EM_TEST_STAMP:-$(date +%Y%m%d-%H%M%S)}"
RESULT="$ROOT/build/ios-tests-$STAMP.xcresult"
SHOTS="$ROOT/build/screenshots/ui-$STAMP"
LOG="$ROOT/build/ios-tests-$STAMP.log"
mkdir -p "$ROOT/build" "$SHOTS"
HOST_PID=""
USB_HOST_PID=""
PAIR_HOST_PID=""
PAIR_HOST_LOG="$ROOT/build/ios-ui-pairing-host-$STAMP.log"
USB_PROXY_PID=""
cleanup() {
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
    [ -n "$PAIR_HOST_PID" ] && kill "$PAIR_HOST_PID" 2>/dev/null || true
    [ -n "$USB_HOST_PID" ] && kill "$USB_HOST_PID" 2>/dev/null || true
    [ -n "$USB_PROXY_PID" ] && kill "$USB_PROXY_PID" 2>/dev/null || true
}
trap cleanup EXIT
# Unit-only selections do not need a streaming host. The complete suite and
# UI selections use a real stream so diagnostics are checked with live values.
NEED_STREAM=1
for argument in "$@"; do
    case "$argument" in -only-testing:EternalMonitorTests*) NEED_STREAM=0 ;; esac
done
(cd "$ROOT/ios" && xcodegen generate)
xcrun simctl bootstatus "$UDID" -b
# Compiling while a software encoder and a newly booting simulator compete
# for the runner's three CPUs can starve both the host and UI automation.
if ! xcodebuild build-for-testing -project "$ROOT/ios/EternalMonitor.xcodeproj" -scheme EternalMonitor \
    -destination "platform=iOS Simulator,id=$UDID" -parallel-testing-enabled NO \
    -derivedDataPath "$DERIVED" CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- "$@" > "$LOG" 2>&1; then
    tail -60 "$LOG"
    exit 1
fi
if [ "$NEED_STREAM" = 1 ]; then
    export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"
    (cd "$ROOT" && cargo build -q --release -p eternal-host --locked)
    python3 - "$ROOT" <<'PYSETTINGS'
import json,pathlib,sys
for name in ['ui-state', 'ui-usb-state']:
    path=pathlib.Path(sys.argv[1])/'build'/name/'EternalMonitor'/'settings.json'
    path.parent.mkdir(parents=True,exist_ok=True)
    path.write_text(json.dumps(dict(bitrate_mbps=15,target_fps=60,start_on_boot=False,require_pairing=False)))
PYSETTINGS
    APPDATA="$ROOT/build/ui-state" ETERNAL_HEADLESS=1 ETERNAL_CAPTURE=synthetic \
        ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=60 \
        ETERNAL_DROP=0.03 ETERNAL_REORDER=0.01 ETERNAL_INPUT_RECORDER_LOG=1 \
        "$ROOT/target/release/eternal-host" 19875 > "$ROOT/build/ios-ui-host-$STAMP.log" 2>&1 &
    HOST_PID=$!
    python3 "$ROOT/scripts/usb_proxy.py" --control-port 19874 > "$ROOT/build/ios-usb-proxy-$STAMP.log" 2>&1 &
    USB_PROXY_PID=$!
    APPDATA="$ROOT/build/ui-usb-state" ETERNAL_HEADLESS=1 ETERNAL_CAPTURE=synthetic \
        ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=60 \
        ETERNAL_USB_DIRECT=127.0.0.1:19873 \
        "$ROOT/target/release/eternal-host" 19877 > "$ROOT/build/ios-ui-usb-host-$STAMP.log" 2>&1 &
    USB_HOST_PID=$!
    if [ -z "${EM_PAIRING_HOST:-}" ]; then
        mkdir -p "$ROOT/build/ui-pairing-state/EternalMonitor"
        python3 - "$ROOT/build/ui-pairing-state/EternalMonitor/settings.json" <<'PYPAIR'
import json,pathlib,sys
pathlib.Path(sys.argv[1]).write_text(json.dumps(dict(bitrate_mbps=15,target_fps=60,start_on_boot=False,require_pairing=True)))
PYPAIR
        APPDATA="$ROOT/build/ui-pairing-state" ETERNAL_HEADLESS=1 ETERNAL_CAPTURE=synthetic \
            ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=60 \
            "$ROOT/target/release/eternal-host" 19878 > "$PAIR_HOST_LOG" 2>&1 &
        PAIR_HOST_PID=$!
    fi
fi
# Pass the startup code to the XCTest runner, not the application. This is
# read from the real host log; there is no fixed code or app-side bypass.
RUNFILE=$(python3 - "$DERIVED/Build/Products" "$PAIR_HOST_LOG" "$NEED_STREAM" "$ROOT/build/ios-ui-host-$STAMP.log" <<'PYRUN'
import os,pathlib,plistlib,re,sys,time
products=pathlib.Path(sys.argv[1])
runfile=max(products.glob('*.xctestrun'),key=lambda p:p.stat().st_mtime)
data=plistlib.loads(runfile.read_bytes())
env=data['EternalMonitorUITests'].setdefault('EnvironmentVariables',{})
for key in ['EM_PAIRING_CODE','EM_PAIRING_HOST','EM_INPUT_HOST_LOG','EM_INPUT_HOST']:
    env.pop(key,None)
if sys.argv[3]=='1':
    env['EM_INPUT_HOST_LOG']=os.environ.get('EM_INPUT_HOST_LOG', sys.argv[4])
    env['EM_INPUT_HOST']=os.environ.get('EM_INPUT_HOST','127.0.0.1:19875')
    code=os.environ.get('EM_PAIRING_CODE')
    deadline=time.monotonic()+10
    while not code and time.monotonic()<deadline:
        text=pathlib.Path(sys.argv[2]).read_text(errors='replace') if pathlib.Path(sys.argv[2]).exists() else ''
        text=re.sub(r'\x1b\[[0-9;]*m','',text)
        match=re.search(r'pairing_code=(\d{6})',text)
        if match: code=match[1]
        else: time.sleep(.1)
    if not code: sys.exit('No pairing code in the host log')
    env.update(EM_PAIRING_CODE=code, EM_PAIRING_HOST=os.environ.get('EM_PAIRING_HOST','127.0.0.1:19878'))
runfile.write_bytes(plistlib.dumps(data))
print(runfile)
PYRUN
)
status=0
xcodebuild test-without-building -xctestrun "$RUNFILE" \
    -destination "platform=iOS Simulator,id=$UDID" -parallel-testing-enabled NO \
    -resultBundlePath "$RESULT" \
    "$@" >> "$LOG" 2>&1 || status=$?
tail -60 "$LOG"
if ! python3 - "$LOG" <<'PYCOUNT'
import pathlib,re,sys
text=pathlib.Path(sys.argv[1]).read_text(errors='replace')
if not re.search(r'Executed [1-9][0-9]* tests?\b', text):
    sys.exit('No tests executed; an empty selection is not a successful run')
PYCOUNT
then status=1; fi
if [ -d "$RESULT" ]; then
    xcrun xcresulttool export attachments --path "$RESULT" --output-path "$SHOTS" || status=1
    python3 - "$ROOT/scripts/px.swift" "$SHOTS" <<'PY' || status=1
import pathlib,subprocess,sys
paths=sorted(pathlib.Path(sys.argv[2]).glob('*.png'))
for path in paths:
    print(path, flush=True)
    result=subprocess.run(['xcrun','swift',sys.argv[1],str(path),'--assert-ui'],text=True,capture_output=True)
    path.with_suffix('.pixels.json').write_text(result.stdout)
    if result.returncode:
        sys.exit(result.stderr)
PY
fi
echo "Result bundle: $RESULT"
echo "Screenshots: $SHOTS"
exit "$status"
