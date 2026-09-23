#!/usr/bin/env bash
# Compare standalone software-codec capacity with the simulator matrix.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/build/e2e/codec-probe"
mkdir -p "$OUT"
FFMPEG="$(brew --prefix ffmpeg@7)/bin/ffmpeg"
for row in h264-small hevc-small h264-burst; do
    size=640x360; bitrate=15M; codec=libx264
    extra=(-profile:v baseline)
    if [ "$row" = hevc-small ]; then
        codec=libx265; extra=(-x265-params repeat-headers=1:log-level=error)
    elif [ "$row" = h264-burst ]; then
        size=2560x1440; bitrate=40M
    fi
    "$FFMPEG" -hide_banner -nostdin -nostats -benchmark -f lavfi \
        -i "testsrc2=size=$size:rate=60" -frames:v 240 \
        -c:v "$codec" -preset ultrafast -tune zerolatency "${extra[@]}" \
        -b:v "$bitrate" -bf 0 -g 60 -f null - > "$OUT/$row.log" 2>&1
    tail -5 "$OUT/$row.log"
done
