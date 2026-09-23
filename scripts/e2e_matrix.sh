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
write_report() {
python3 - "$REPORT" "${rows[@]}" <<'PY'
import datetime,json,pathlib,sys
lines=['# EternalMonitor system tests', '', datetime.datetime.now(datetime.timezone.utc).isoformat(), '',
       '| Scenario | Result | Average FPS | FPS gate | Decoded | Dropped | Repaired | NACKs | Stream seconds | Audio decoded | Audio lost | Tone dBFS | Recovery seconds | Elapsed seconds | Screenshot |',
       '| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |']
for filename in sys.argv[2:]:
    p=pathlib.Path(filename)
    r=json.loads(p.read_text()) if p.exists() else dict(scenario=p.parent.name,status='FAIL')
    shot=r.get('screenshot','')
    cells=[r['scenario'],r['status'],r.get('average_fps',r.get('fps','')),r.get('fps_gate',''),r.get('decoded',''),
           r.get('dropped',''),r.get('repaired',''),r.get('nacks',''),r.get('measured_seconds',''),
           r.get('audio_decoded',''),r.get('audio_lost',''),r.get('audio_tone1k_db',''),
           r.get('recovery_seconds',''),r.get('elapsed',''),
           f'[PNG]({shot})' if shot else '']
    lines.append('| '+' | '.join(map(str,cells))+' |')
pathlib.Path(sys.argv[1]).write_text('\n'.join(lines)+'\n')
print(sys.argv[1])
PY
}
finish() {
    status=$?
    trap - EXIT
    write_report || status=1
    exit "$status"
}
trap finish EXIT
if [ "$MODE" = --real ]; then
    while IFS= read -r scenario; do
        rows+=("$OUT/$scenario/result.json")
    done < <(python3 "$ROOT/scripts/e2e_real.py" --list)
    python3 "$ROOT/scripts/e2e_real.py" || failed=1

else
    skip=0
    scenarios=(h264-udp hevc-udp h264-udp-loss3 h264-udp-burst h264-udp-burst-bsd h264-usb usb-takeover h264-udp-audio h264-usb-audio pairing keyboard reconnect background-resume stream-ui)
    if [ "$MODE" = --ci ]; then
        # The full local gate also compares both socket paths and USB audio.
        scenarios=(h264-udp hevc-udp h264-udp-loss3 h264-udp-burst usb-takeover h264-udp-audio pairing keyboard reconnect background-resume stream-ui)
    fi
    for scenario in "${scenarios[@]}"; do
        runner=""
        case "$scenario" in
            pairing|keyboard) runner="e2e_$scenario.sh" ;;
            reconnect|background-resume) runner=e2e_lifecycle.sh ;;
            stream-ui) runner=e2e_stream_ui.sh ;;
        esac
        if [ -n "$runner" ]; then
            rows+=("$OUT/$scenario/result.json")
            echo "==> $scenario"
            if ! EM_SCENARIO="$scenario" EM_OUTPUT_DIR="$OUT/$scenario" \
                "$ROOT/scripts/$runner" > "$OUT/$scenario-run.log" 2>&1; then failed=1; fi
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
