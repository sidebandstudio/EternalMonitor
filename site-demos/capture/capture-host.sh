#!/usr/bin/env bash
# Re-records the Windows app footage in site-demos/footage/host-*.mp4.
#
#   site-demos/capture/capture-host.sh [pair,second-screen,performance,feedback]
#
# The app draws every frame itself (egui_kittest, macOS, 2x). This copies the
# committed tree to a scratch folder, appends host_film.rs to the GUI snapshot
# test there and runs it, so host/ is never modified. Needs FFmpeg 7 (brew install ffmpeg@7). CARGO_TARGET_DIR may point
# at an existing target folder to save a full build.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
scratch=$(mktemp -d /tmp/em-film-src.XXXX)
frames=$(mktemp -d /tmp/em-film-frames.XXXX)
git -C "$repo" archive HEAD | tar -x -C "$scratch"
cat "$here/host_film.rs" >> "$scratch/host/tests/gui_snapshots.rs"
(cd "$scratch" && PKG_CONFIG_PATH=${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig} EM_FILM_DIR="$frames" EM_FILM_ONLY="${1:-}" \
  cargo test -p eternal-host --test gui_snapshots home_page_films -- --ignored --exact --nocapture)
for dir in "$frames"/*/; do
  name=$(basename "$dir")
  ffmpeg -v error -y -framerate 30 -i "$dir/%04d.png" -c:v libx264 -crf 12 -preset slow \
    -pix_fmt yuv420p "$here/../footage/host-$name.mp4"
  { echo "{\"pointer\": $(cat "$dir/pointer.json"), \"rects\": {"
    awk '{printf "%s\"%s\": [%s, %s, %s, %s]", (NR>1?", ":""), $1, $2, $3, $4, $5}' "$dir/rects.txt" 2>/dev/null
    echo "}}"; } > "$here/../footage/host-$name.json"
  echo "footage/host-$name.mp4"
done
rm -rf "$scratch"
