#!/usr/bin/env bash
# Real code entry and persistent Keychain reconnect through the simulator UI.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/pairing}"
SCENARIO="${EM_SCENARIO:-pairing}"
STAMP="$(date +%Y%m%d-%H%M%S)"
STARTED=$SECONDS
mkdir -p "$OUT"
rm -f "$OUT/result.json"
status=0
EM_TEST_STAMP="$STAMP" "$ROOT/scripts/test_ios.sh" \
    -only-testing:EternalMonitorUITests/PairingFlowTests/testPairingFlow > "$OUT/ui-run.log" 2>&1 || status=$?
TEST_ROOT="${EM_UI_EVIDENCE_DIR:-$ROOT/build}"
RESULT="$TEST_ROOT/ios-tests-$STAMP.xcresult"
SHOTS="$TEST_ROOT/screenshots/ui-$STAMP"
[ ! -f "$TEST_ROOT/ios-tests-$STAMP.log" ] || cp "$TEST_ROOT/ios-tests-$STAMP.log" "$OUT/tests.log"
[ ! -f "$ROOT/build/ios-ui-pairing-host-$STAMP.log" ] || cp "$ROOT/build/ios-ui-pairing-host-$STAMP.log" "$OUT/host.log"
if [ -d "$RESULT" ]; then
    xcrun xcresulttool get test-results summary --path "$RESULT" --format json > "$OUT/tests.json" || status=1
fi
python3 - "$OUT" "$SHOTS" "$SCENARIO" "$status" "$((SECONDS-STARTED))" <<'PY'
import json,pathlib,shutil,sys
out,shots=map(pathlib.Path,sys.argv[1:3])
errors=[]
if sys.argv[4]!='0': errors.append('Pairing UI test failed; see ui-run.log')
summary=json.loads((out/'tests.json').read_text()) if (out/'tests.json').exists() else {}
if summary.get('passedTests',0)!=1 or summary.get('failedTests',0)!=0:
    errors.append('Expected one passing pairing test')
manifest=json.loads((shots/'manifest.json').read_text()) if (shots/'manifest.json').exists() else []
for name in ['pairing-wrong-code','pairing-connected','pairing-token-reconnect']:
    found=False
    for test in manifest:
        for item in test.get('attachments',[]):
            if item.get('suggestedHumanReadableName','').startswith(name+'_') and item['exportedFileName'].endswith('.png'):
                shutil.copy2(shots/item['exportedFileName'], out/(name+'.png')); found=True
    if not found: errors.append('Missing screenshot: '+name)
report=dict(scenario=sys.argv[3],status='FAIL' if errors else 'PASS',elapsed=int(sys.argv[5]),
    tests_passed=summary.get('passedTests',0),screenshot=str(out/'pairing-token-reconnect.png'),errors=errors)
(out/'result.json').write_text(json.dumps(report,indent=2)+'\n')
if errors: sys.exit('; '.join(errors))
print('PASS: wrong code rejected, code pairing streamed, Keychain reconnect streamed')
print('Evidence: '+str(out))
PY
