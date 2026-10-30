#!/usr/bin/env bash
# Renders the home page demo films into docs/assets/demos/.
#
#   site-demos/render.sh            every film
#   site-demos/render.sh pair fit   only these
#   site-demos/render.sh --prep     only copy the icon and fonts in (for preview)
#
# Each film is a HyperFrames composition in <name>.html over real footage in
# footage/: host-*.mp4 is drawn by the Windows app itself
# (capture/capture-host.sh), ipad-*.mp4 is the iPad app recorded in the
# simulator. It is rendered once at high quality, then encoded for the web:
# H.264 MP4 (yuv420p, faststart), VP9 WebM and a WebP poster, the MP4 and
# WebM together kept near 1.5 MB. Needs Node, ffmpeg and cwebp.
set -euo pipefail
cd "$(dirname "$0")"
OUT=../docs/assets/demos
HF="npx --yes hyperframes@0.8.220"

prep() {
  mkdir -p ui/fonts
  cp ../docs/assets/icon.png ui/
  cp ../docs/assets/fonts/Geist-{Regular,Medium,SemiBold}.woff2 ui/fonts/
}
prep
[ "${1:-}" = "--prep" ] && exit 0

# name:poster second:MP4 CRF:WebM CRF. Busy films get higher CRFs to stay in budget.
FILMS=(second-screen:4.2:32:48 pair:4.4:26:41 fit:4.0:25:38 performance:4.0:30:46 pencil:3.0:26:40 feedback:4.5:27:43)
mkdir -p "$OUT" renders
for entry in "${FILMS[@]}"; do
  IFS=: read -r name poster crf264 crfvp9 <<< "$entry"
  if [ $# -gt 0 ] && [[ ! " $* " == *" $name "* ]]; then continue; fi
  echo "== $name"
  master=renders/$name-master.mp4
  $HF render -c "$name.html" --fps 30 --crf 10 --quiet -o "$master"
  ffmpeg -v error -y -i "$master" -an -c:v libx264 -preset veryslow -crf "$crf264" -tune film \
    -pix_fmt yuv420p -profile:v high -g 60 -movflags +faststart "$OUT/$name.mp4"
  ffmpeg -v error -y -i "$master" -an -c:v libvpx-vp9 -b:v 0 -crf "$crfvp9" -row-mt 1 \
    -deadline good -cpu-used 1 -pix_fmt yuv420p -g 60 "$OUT/$name.webm"
  ffmpeg -v error -y -ss "$poster" -i "$master" -frames:v 1 renders/$name-poster.png
  cwebp -quiet -q 82 renders/$name-poster.png -o "$OUT/$name.webp"
  ls -l "$OUT/$name".{mp4,webm,webp} | awk '{print "   " $5 "\t" $9}'
done
