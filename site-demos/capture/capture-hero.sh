#!/usr/bin/env bash
# Renders the Windows app stills the home page hero's laptop shows:
#   footage/hero-{mirror,second}-{usb,wifi}.webp  2080 x 1440, drawn by the app
# The same scratch-copy run as capture-host.sh, keeping each still's last frame.
# Needs cwebp.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
scratch=$(mktemp -d /tmp/em-film-src.XXXX)
frames=$(mktemp -d /tmp/em-film-frames.XXXX)
git -C "$repo" archive HEAD | tar -x -C "$scratch"
cat "$here/host_film.rs" >> "$scratch/host/tests/gui_snapshots.rs"
(cd "$scratch" && PKG_CONFIG_PATH=${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig} EM_FILM_DIR="$frames" EM_FILM_ONLY=hero \
  cargo test -p eternal-host --test gui_snapshots home_page_films -- --ignored --exact --nocapture)
for dir in "$frames"/hero-*/; do
  name=$(basename "$dir")
  cwebp -quiet -lossless -z 9 "$dir/0029.png" -o "$here/../footage/$name.webp"
  echo "footage/$name.webp"
done
rm -rf "$scratch"
