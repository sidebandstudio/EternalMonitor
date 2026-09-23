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
       '| Scenario | Result | Average FPS | Decoded | Dropped | Repaired | NACKs | Stream seconds | Audio decoded | Audio lost | Tone dBFS | Screenshot |',
       '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |']
for filename in sys.argv[2:]:
    p=pathlib.Path(filename)
    r=json.loads(p.read_text()) if p.exists() else dict(scenario=p.parent.name,status='FAIL')
    shot=r.get('screenshot','')
    cells=[r['scenario'],r['status'],r.get('average_fps',r.get('fps','')),r.get('decoded',''),
           r.get('dropped',''),r.get('repaired',''),r.get('nacks',''),r.get('measured_seconds',''),
           r.get('audio_decoded',''),r.get('audio_lost',''),r.get('audio_tone1k_db',''),
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
    skip=0
    for scenario in R-baseline R-nvenc-h264 R-nvenc-h264-loss3 R-nvenc-burst; do
    rows+=("$OUT/$scenario/result.json")
    # A failed prerequisite must not leave a previous run looking successful.
    mkdir -p "$OUT/$scenario"
    rm -f "$OUT/$scenario/result.json"
    if [ "$pattern_started" = 0 ]; then
        "$ROOT/scripts/win/remote.sh" pattern start
        pattern_started=1
    fi
    drop=0; reorder=0; bitrate=15; idr=0; repairs=0; duration=20
    case "$scenario" in
        R-baseline) duration=0 ;;
        R-nvenc-h264-loss3) drop=0.03; reorder=0.01; repairs=1 ;;
        R-nvenc-burst) bitrate=40; idr=60 ;;
    esac
    EM_BITRATE_MBPS="$bitrate" "$ROOT/scripts/win/remote.sh" run-host \
        ETERNAL_HEADLESS=1 ETERNAL_ENCODER=h264_nvenc ETERNAL_HEVC=0 ETERNAL_FPS=60 \
        ETERNAL_MAX_DGRAM=1200 ETERNAL_DROP="$drop" ETERNAL_REORDER="$reorder" ETERNAL_FORCE_IDR_PERIOD="$idr"
    host_started=1
    if EM_SCENARIO="$scenario" EM_OUTPUT_DIR="$OUT/$scenario" \
       EM_SCREENSHOT="$OUT/$scenario/simulator.png" EM_REMOTE_HOST=100.81.59.48 \
       EM_PORT=19876 EM_SIZE="${EM_SIZE:-1920x1080}" EM_SKIP_BUILD="$skip" EM_DURATION="$duration" \
       EM_REQUIRE_REPAIRS="$repairs" "$ROOT/scripts/e2e_ios.sh" > "$OUT/$scenario-run.log" 2>&1; then
        skip=1
        "$ROOT/scripts/win/remote.sh" log > "$OUT/$scenario/host.log"
        if ! python3 - "$OUT/$scenario" "$repairs" <<'PY'
import json,pathlib,re,sys
p=pathlib.Path(sys.argv[1]); r=json.loads((p/'result.json').read_text())
log=(p/'host.log').read_text(); errors=[]
if 'Desktop duplication active' not in log or 'h264_nvenc' not in log:
    errors.append('hardware capture/NVENC evidence missing')
requests=log.count('Keyframe request received')
retransmits=max([int(x) for x in re.findall(r'retransmits=(\d+)',log)],default=0)
if requests > max(1,int(r.get('measured_seconds',0)/10)):
    errors.append(f'keyframe request storm: {requests}')
if sys.argv[2]=='1' and retransmits==0:
    errors.append('host retransmission evidence missing')
r.update(keyframe_requests=requests,host_retransmits=retransmits)
if errors: r.update(status='FAIL',errors=errors)
(p/'result.json').write_text(json.dumps(r,indent=2)+'\n')
if errors: sys.exit('; '.join(errors))
PY
        then
            failed=1
        fi
        "$ROOT/scripts/win/remote.sh" shot "$scenario"
    else
        cat "$OUT/$scenario-run.log"
        "$ROOT/scripts/win/remote.sh" log > "$OUT/$scenario/host.log"
        failed=1
    fi
    "$ROOT/scripts/win/remote.sh" stop-host
    host_started=0
    done
else
    skip=0
    for scenario in h264-udp hevc-udp h264-udp-loss3 h264-udp-burst h264-udp-burst-bsd h264-usb usb-takeover h264-udp-audio h264-usb-audio pairing keyboard; do
        if [ "$scenario" = pairing ] || [ "$scenario" = keyboard ]; then
            rows+=("$OUT/$scenario/result.json")
            echo "==> $scenario"
            if ! EM_SCENARIO="$scenario" EM_OUTPUT_DIR="$OUT/$scenario" \
                "$ROOT/scripts/e2e_$scenario.sh" > "$OUT/$scenario-run.log" 2>&1; then failed=1; fi
            tail -5 "$OUT/$scenario-run.log"
            continue
        fi
        codec=h264; size=640x360; drop=0; reorder=0; bitrate=15; idr=0; duration=5; repairs=0; backend=nw; transport=udp; abr=1
        audio=0
        case "$scenario" in
            hevc-udp) codec=hevc ;;
            h264-udp-loss3) drop=0.03; reorder=0.01; duration=20; repairs=1 ;;
            h264-udp-burst*) size=2560x1440; bitrate=40; idr=60; duration=20; abr=0 ;;
            h264-usb) transport=usb ;;
            usb-takeover) transport=takeover ;;
            h264-udp-audio) audio=1 ;;
            h264-usb-audio) transport=usb; audio=1 ;;
        esac
        [ "$scenario" != h264-udp-burst-bsd ] || backend=bsd
        rows+=("$OUT/$scenario/result.json")
        echo "==> $scenario"
        if ! EM_CODEC="$codec" EM_SCENARIO="$scenario" EM_SKIP_BUILD="$skip" \
             EM_OUTPUT_DIR="$OUT/$scenario" EM_SIZE="$size" EM_BITRATE_MBPS="$bitrate" \
             EM_UDP_BACKEND="$backend" EM_DURATION="$duration" EM_REQUIRE_REPAIRS="$repairs" \
             EM_TRANSPORT="$transport" EM_AUDIO="$audio" \
             ETERNAL_ABR="$abr" ETERNAL_DROP="$drop" ETERNAL_REORDER="$reorder" ETERNAL_FORCE_IDR_PERIOD="$idr" \
             "$ROOT/scripts/e2e_ios.sh" > "$OUT/$scenario-run.log" 2>&1; then
            failed=1
        else
            skip=1
        fi
        tail -5 "$OUT/$scenario-run.log"
    done
fi

exit "$failed"
