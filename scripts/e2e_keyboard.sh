#!/usr/bin/env bash
# Exercise the visible keyboard and assert the host's deduplicated injections.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/keyboard}"
SCENARIO="${EM_SCENARIO:-keyboard}"
STAMP="$(date +%Y%m%d-%H%M%S)"
STARTED=$SECONDS
mkdir -p "$OUT"
rm -f "$OUT/result.json"
status=0
EM_TEST_STAMP="$STAMP" "$ROOT/scripts/test_ios.sh" \
    -only-testing:EternalMonitorUITests/KeyboardRelayTests/testKeyboardRelay > "$OUT/ui-run.log" 2>&1 || status=$?
RESULT="$ROOT/build/ios-tests-$STAMP.xcresult"
SHOTS="$ROOT/build/screenshots/ui-$STAMP"
[ ! -f "$ROOT/build/ios-tests-$STAMP.log" ] || cp "$ROOT/build/ios-tests-$STAMP.log" "$OUT/tests.log"
[ ! -f "$ROOT/build/ios-ui-host-$STAMP.log" ] || cp "$ROOT/build/ios-ui-host-$STAMP.log" "$OUT/host.log"
if [ -d "$RESULT" ]; then
    xcrun xcresulttool get test-results summary --path "$RESULT" --format json > "$OUT/tests.json" || status=1
fi
python3 - "$ROOT" "$OUT" "$SHOTS" "$SCENARIO" "$status" "$((SECONDS-STARTED))" <<'PY'
import json,pathlib,re,shutil,subprocess,sys
root,out,shots=map(pathlib.Path,sys.argv[1:4])
errors=[]
if sys.argv[5]!='0': errors.append('Keyboard UI test failed; see ui-run.log')
summary=json.loads((out/'tests.json').read_text()) if (out/'tests.json').exists() else {}
if summary.get('passedTests',0)!=1 or summary.get('failedTests',0)!=0:
    errors.append('Expected one passing keyboard test')
manifest=json.loads((shots/'manifest.json').read_text()) if (shots/'manifest.json').exists() else []
for name in ['keyboard-relay','keyboard-dismissed']:
    found=False
    for test in manifest:
        for item in test.get('attachments',[]):
            if item.get('suggestedHumanReadableName','').startswith(name+'_') and item['exportedFileName'].endswith('.png'):
                shutil.copy2(shots/item['exportedFileName'],out/(name+'.png')); found=True
    if not found: errors.append('Missing screenshot: '+name)
    else:
        result=subprocess.run(['swift',str(root/'scripts/px.swift'),str(out/(name+'.png')),'--assert-pattern'],capture_output=True,text=True)
        (out/(name+'-pixels.log')).write_text(result.stdout+result.stderr)
        if result.returncode: errors.append('Pixel assertion failed: '+name)
log=(out/'host.log').read_text() if (out/'host.log').exists() else ''
log=re.sub(r'\x1b\[[0-9;]*m','',log)
commands=re.findall(r'Input injection command=(.+)',log)
expected=['Unicode(72)','Unicode(105)','Unicode(33)',
    'KeyDown { scan: 28, extended: false }','KeyUp { scan: 28, extended: false }',
    'KeyDown { scan: 29, extended: false }','KeyDown { scan: 75, extended: true }',
    'KeyUp { scan: 75, extended: true }','KeyUp { scan: 29, extended: false }']
if commands!=expected: errors.append('Unexpected injection sequence: '+repr(commands))
report=dict(scenario=sys.argv[4],status='FAIL' if errors else 'PASS',elapsed=int(sys.argv[6]),
    tests_passed=summary.get('passedTests',0),screenshot=str(out/'keyboard-relay.png'),commands=commands,errors=errors)
(out/'result.json').write_text(json.dumps(report,indent=2)+'\n')
if errors: sys.exit('; '.join(errors))
print('PASS: Hi! and Enter, Ctrl+Left, duplicate suppression and keyboard dismissal')
print('Evidence: '+str(out))
PY
