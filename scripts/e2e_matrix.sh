#!/usr/bin/env bash
# Sequential rows share one simulator and one build. Logs survive every run.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-}"
case "$MODE" in ""|--ci|--real) ;; *) echo "Usage: $0 [--ci|--real]" >&2; exit 2;; esac
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
OUT="$ROOT/build/e2e"
REPORT="$ROOT/build/e2e-report.md"
if [ "$MODE" = --real ]; then
    OUT="${EM_EVIDENCE_DIR:-/Users/aldo/Desktop/EternalMonitor-Handoff/evidence}/real"
    REPORT="$ROOT/build/e2e-real-report.md"
fi
mkdir -p "$OUT" "$ROOT/build"
failed=0
rows=()
pattern_started=0
host_started=0
write_report() {
python3 - "$REPORT" "${rows[@]}" <<'PY'
import datetime,json,pathlib,sys
lines=['# EternalMonitor system tests', '', datetime.datetime.now(datetime.timezone.utc).isoformat(), '',
       '| Scenario | Result | FPS | Decoded | Seconds | Screenshot |',
       '| --- | --- | ---: | ---: | ---: | --- |']
for filename in sys.argv[2:]:
    p=pathlib.Path(filename)
    r=json.loads(p.read_text()) if p.exists() else dict(scenario=p.parent.name,status='FAIL')
    shot=r.get('screenshot','')
    cells=[r['scenario'],r['status'],r.get('fps',''),r.get('decoded',''),r.get('elapsed',''),
           f'[PNG]({shot})' if shot else '']
    lines.append('| '+' | '.join(map(str,cells))+' |')
pathlib.Path(sys.argv[1]).write_text('\n'.join(lines)+'\n')
print(sys.argv[1])
PY
}
finish() {
    status=$?
    trap - EXIT
    if [ "$host_started" = 1 ]; then
        "$ROOT/scripts/win/remote.sh" stop-host || status=1
    fi
    if [ "$pattern_started" = 1 ]; then
        "$ROOT/scripts/win/remote.sh" pattern stop || status=1
    fi
    if [ "$MODE" = --real ] && [ "$status" != 0 ]; then
        python3 - "$OUT/$scenario/result.json" <<'PYRESULT'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
r=json.loads(p.read_text()) if p.exists() else {'scenario':p.parent.name}
r['status']='FAIL'
p.parent.mkdir(parents=True,exist_ok=True)
p.write_text(json.dumps(r)+'\n')
PYRESULT
    fi
    write_report || status=1
    exit "$status"
}
trap finish EXIT
if [ "$MODE" = --real ]; then
    scenario=R-baseline
    rows+=("$OUT/$scenario/result.json")
    # A failed prerequisite must not leave a previous run looking successful.
    mkdir -p "$OUT/$scenario"
    rm -f "$OUT/$scenario/result.json"
    "$ROOT/scripts/win/remote.sh" pattern start
    pattern_started=1
    "$ROOT/scripts/win/remote.sh" run-host ETERNAL_HEADLESS=1 ETERNAL_ENCODER=h264_nvenc ETERNAL_HEVC=0 ETERNAL_FPS=60
    host_started=1
    if EM_SCENARIO="$scenario" EM_OUTPUT_DIR="$OUT/$scenario" \
       EM_SCREENSHOT="$OUT/$scenario/simulator.png" EM_REMOTE_HOST=100.81.59.48 \
       EM_PORT=19876 EM_SIZE=1920x1080 "$ROOT/scripts/e2e_ios.sh" > "$OUT/$scenario-run.log" 2>&1; then
        "$ROOT/scripts/win/remote.sh" log > "$OUT/$scenario/host.log"
        if ! grep -q 'Desktop duplication active' "$OUT/$scenario/host.log" || \
           ! grep -q 'h264_nvenc' "$OUT/$scenario/host.log"; then
            echo "FAIL: hardware capture/NVENC evidence missing" >&2
            failed=1
            python3 - "$OUT/$scenario/result.json" <<'PY'
import json,sys
p=sys.argv[1]
with open(p) as f: result=json.load(f)
result['status']='FAIL'
with open(p,'w') as f: json.dump(result,f)
PY
        fi
        "$ROOT/scripts/win/remote.sh" shot R-baseline
    else
        cat "$OUT/$scenario-run.log"
        failed=1
    fi
else
    skip=0
    for codec in h264 hevc; do
        scenario="$codec-udp"
        rows+=("$OUT/$scenario/result.json")
        echo "==> $scenario"
        if ! EM_CODEC="$codec" EM_SCENARIO="$scenario" EM_SKIP_BUILD="$skip" \
             EM_OUTPUT_DIR="$OUT/$scenario" "$ROOT/scripts/e2e_ios.sh" > "$OUT/$scenario-run.log" 2>&1; then
            failed=1
        else
            skip=1
        fi
        tail -5 "$OUT/$scenario-run.log"
    done
fi

exit "$failed"
