#!/usr/bin/env bash
# Exercise streaming controls that need live audio, repairs and USB takeover.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/stream-ui}"
STAMP="$(date +%Y%m%d-%H%M%S)"
STARTED=$SECONDS
mkdir -p "$OUT"
rm -f "$OUT/result.json" "$OUT/tests.json"
status=0
EM_TEST_STAMP="$STAMP" "$ROOT/scripts/test_ios.sh" \
    -only-testing:EternalMonitorUITests/AudioStreamTests \
    -only-testing:EternalMonitorUITests/StreamDiagnosticsTests \
    -only-testing:EternalMonitorUITests/USBStreamTests > "$OUT/ui-run.log" 2>&1 || status=$?
TEST_ROOT="${EM_UI_EVIDENCE_DIR:-$ROOT/build}"
RESULT="$TEST_ROOT/ios-tests-$STAMP.xcresult"
SHOTS="$TEST_ROOT/screenshots/ui-$STAMP"
for source in "$TEST_ROOT/ios-tests-$STAMP.log" "$TEST_ROOT/ios-ui-host-$STAMP.log" \
    "$TEST_ROOT/ios-ui-usb-host-$STAMP.log" "$TEST_ROOT/ios-usb-proxy-$STAMP.log"; do
    [ ! -f "$source" ] || cp "$source" "$OUT/"
done
if [ -d "$RESULT" ]; then
    xcrun xcresulttool get test-results summary --path "$RESULT" --format json > "$OUT/tests.json" || status=1
fi
python3 - "$OUT" "$SHOTS" "$status" "$((SECONDS-STARTED))" <<'PY'
import json,pathlib,shutil,sys
out,shots=map(pathlib.Path,sys.argv[1:3]); errors=[]
if sys.argv[3]!='0': errors.append('Streaming UI tests failed; see ui-run.log')
summary=json.loads((out/'tests.json').read_text()) if (out/'tests.json').exists() else {}
if summary.get('passedTests',0)!=4 or summary.get('failedTests',0)!=0:
    errors.append('Expected four passing streaming UI tests')
retained=out/'screenshots'/shots.name
if shots.exists(): shutil.copytree(shots,retained)
images=sorted(retained.glob('*.png'))
if not images: errors.append('UI screenshots are missing')
report=dict(scenario='stream-ui',status='FAIL' if errors else 'PASS',elapsed=int(sys.argv[4]),
    tests_passed=summary.get('passedTests',0),screenshot=str(images[0]) if images else '',errors=errors)
(out/'result.json').write_text(json.dumps(report,indent=2)+'\n')
if errors: sys.exit('; '.join(errors))
print('PASS: live audio, repair diagnostics, USB connect/disconnect and cable fallback')
print('Evidence: '+str(out))
PY
