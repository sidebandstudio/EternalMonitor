#!/usr/bin/env bash
# Run unit/UI tests and retain the result bundle, screenshots, and pixel checks.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
UDID="${EM_SIM_UDID:-06416ADB-C33D-4EE4-82DB-44FCD663362F}"
# CoreSimulator can stall cloning a runner out of a Desktop checkout on a
# remote Mac. Keep test products in the temporary directory, outside Desktop.
DERIVED="${EM_DERIVED_DATA:-/tmp/eternalmonitor-ios-tests}"
STAMP="$(date +%Y%m%d-%H%M%S)"
RESULT="$ROOT/build/ios-tests-$STAMP.xcresult"
SHOTS="$ROOT/build/screenshots/ui-$STAMP"
LOG="$ROOT/build/ios-tests-$STAMP.log"
mkdir -p "$ROOT/build" "$SHOTS"
HOST_PID=""
cleanup() {
    [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null || true
}
trap cleanup EXIT
# Unit-only selections do not need a streaming host. The complete suite and
# UI selections use a real stream so diagnostics are checked with live values.
NEED_STREAM=1
for argument in "$@"; do
    case "$argument" in -only-testing:EternalMonitorTests*) NEED_STREAM=0 ;; esac
done
if [ "$NEED_STREAM" = 1 ]; then
    export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}"
    (cd "$ROOT" && cargo build -q --release -p eternal-host --locked)
    APPDATA="$ROOT/build/ui-state" ETERNAL_HEADLESS=1 ETERNAL_CAPTURE=synthetic \
        ETERNAL_SYNTH_SIZE=640x360 ETERNAL_ENCODER=libx264 ETERNAL_FPS=60 \
        ETERNAL_DROP=0.03 ETERNAL_REORDER=0.01 \
        "$ROOT/target/release/eternal-host" 19875 > "$ROOT/build/ios-ui-host-$STAMP.log" 2>&1 &
    HOST_PID=$!
fi
(cd "$ROOT/ios" && xcodegen generate)
status=0
xcodebuild test -project "$ROOT/ios/EternalMonitor.xcodeproj" -scheme EternalMonitor \
    -destination "platform=iOS Simulator,id=$UDID" -parallel-testing-enabled NO \
    -derivedDataPath "$DERIVED" -resultBundlePath "$RESULT" CODE_SIGNING_ALLOWED=NO \
    "$@" > "$LOG" 2>&1 || status=$?
tail -60 "$LOG"
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
