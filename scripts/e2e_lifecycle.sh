#!/usr/bin/env bash
# Drive loss/reconnect and Home/foreground through the real app UI.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
SCENARIO="${EM_SCENARIO:-reconnect}"
case "$SCENARIO" in
    reconnect) TEST=testHostRestart ;;
    background-resume) TEST=testBackgroundResume ;;
    *) echo "Unknown lifecycle scenario: $SCENARIO" >&2; exit 2 ;;
esac
OUT="${EM_OUTPUT_DIR:-$ROOT/build/e2e/$SCENARIO}"
STAMP="$(date +%Y%m%d-%H%M%S)"
STARTED=$SECONDS
mkdir -p "$OUT"
rm -f "$OUT/result.json"
status=0
EM_TEST_STAMP="$STAMP" "$ROOT/scripts/test_ios.sh" \
    "-only-testing:EternalMonitorUITests/StreamLifecycleTests/$TEST" > "$OUT/ui-run.log" 2>&1 || status=$?
RESULT="$ROOT/build/ios-tests-$STAMP.xcresult"
SHOTS="$ROOT/build/screenshots/ui-$STAMP"
[ ! -f "$ROOT/build/ios-tests-$STAMP.log" ] || cp "$ROOT/build/ios-tests-$STAMP.log" "$OUT/app.log"
[ ! -f "$ROOT/build/ios-ui-host-$STAMP.log" ] || cp "$ROOT/build/ios-ui-host-$STAMP.log" "$OUT/host.log"
if [ -d "$RESULT" ]; then
    xcrun xcresulttool get test-results summary --path "$RESULT" --format json > "$OUT/tests.json" || status=1
fi
python3 - "$ROOT" "$OUT" "$SHOTS" "$SCENARIO" "$status" "$((SECONDS-STARTED))" <<'PY'
import json,pathlib,re,shutil,subprocess,sys
root,out,shots=map(pathlib.Path,sys.argv[1:4]); scenario=sys.argv[4]
errors=[]
if sys.argv[5]!='0': errors.append('Lifecycle UI test failed; see app.log')
summary=json.loads((out/'tests.json').read_text()) if (out/'tests.json').exists() else {}
if summary.get('passedTests',0)!=1 or summary.get('failedTests',0)!=0:
    errors.append('Expected exactly one passing lifecycle test')
manifest=json.loads((shots/'manifest.json').read_text()) if (shots/'manifest.json').exists() else []
names=['signal-lost','stream-resumed'] if scenario=='reconnect' else ['background-resumed']
for name in names:
    found=False
    for test in manifest:
        for item in test.get('attachments',[]):
            if item.get('suggestedHumanReadableName','').startswith(name+'_') and item['exportedFileName'].endswith('.png'):
                shutil.copy2(shots/item['exportedFileName'],out/(name+'.png')); found=True
    if not found: errors.append('Missing screenshot: '+name)
    else:
        result=subprocess.run(['xcrun','swift',str(root/'scripts/px.swift'),str(out/(name+'.png')),'--assert-pattern'],capture_output=True,text=True)
        (out/(name+'-pixels.log')).write_text(result.stdout+result.stderr)
        if result.returncode: errors.append('Pixel assertion failed: '+name)
app=(out/'app.log').read_text(errors='replace') if (out/'app.log').exists() else ''
host=(out/'host.log').read_text(errors='replace') if (out/'host.log').exists() else ''
host=re.sub(r'\x1b\[[0-9;]*m','',host)
marker='E2E_LIFECYCLE_RESUMED' if scenario=='reconnect' else 'E2E_LIFECYCLE_FOREGROUND'
match=re.search(marker+r' elapsed=([0-9.]+)',app)
recovery=float(match[1]) if match else None
if recovery is None or recovery>=15: errors.append('Recovery was not measured below fifteen seconds')
if scenario=='reconnect':
    events=[json.loads(line.split('E2E_LIFECYCLE ',1)[1]) for line in host.splitlines() if line.startswith('E2E_LIFECYCLE ')]
    if [e['event'] for e in events]!=['started','stopped','restarted']:
        errors.append('The host kill/restart sequence is missing')
    elif events[0]['pid']==events[2]['pid']:
        errors.append('The test did not restart a new host process')
else:
    if 'Client said goodbye' not in host or 'reason=AppBackground' not in host:
        errors.append('The host did not receive the background BYE')
report=dict(scenario=scenario,status='FAIL' if errors else 'PASS',elapsed=int(sys.argv[6]),
    recovery_seconds=recovery,tests_passed=summary.get('passedTests',0),screenshot=str(out/(names[-1]+'.png')),errors=errors)
(out/'result.json').write_text(json.dumps(report,indent=2)+'\n')
if errors: sys.exit('; '.join(errors))
print(f'PASS: {scenario} recovered in {recovery:.2f} seconds')
print('Evidence: '+str(out))
PY
