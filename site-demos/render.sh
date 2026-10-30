#!/usr/bin/env bash
# Renders the home page demo films into docs/assets/demos/.
#
#   site-demos/render.sh            every film
#   site-demos/render.sh pair fit   only these
#   site-demos/render.sh --prep     only copy the app screenshots in (for preview)
#
# Each film is a HyperFrames composition in <name>.html. It is
# rendered once at high quality, then encoded for the web: H.264 MP4
# (yuv420p, faststart), VP9 WebM and a WebP poster. Needs Node, ffmpeg and cwebp.
set -euo pipefail
cd "$(dirname "$0")"
OUT=../docs/assets/demos
HF="npx --yes hyperframes@0.8.220"

# The films use the real app screenshots and fonts that the site already ships.
prep() {
  mkdir -p ui/fonts
  cp ../docs/assets/{icon.png,hero-pc@2x.webp,host-qr@2x.webp,host-performance@2x.webp,mode-mirror@2x.webp,mode-second@2x.webp,ipad-fit@2x.webp,ipad-fill@2x.webp,ipad-settings@2x.webp} ui/
  cp ../docs/assets/fonts/Geist-{Regular,Medium,SemiBold}.woff2 ui/fonts/
}
prep
[ "${1:-}" = "--prep" ] && exit 0

# name, then the second (from the start of the loop) the poster is taken at.
FILMS=(second-screen:0.6 pair:4.4 pencil:3.0 fit:4.0 performance:3.5 feedback:5.0)
mkdir -p "$OUT" renders
for entry in "${FILMS[@]}"; do
  name=${entry%%:*} poster=${entry#*:}
  if [ $# -gt 0 ] && [[ ! " $* " == *" $name "* ]]; then continue; fi
  echo "== $name"
  master=renders/$name-master.mp4
  $HF render -c "$name.html" --fps 30 --crf 10 --quiet -o "$master"
  ffmpeg -v error -y -i "$master" -an -c:v libx264 -preset veryslow -crf 25 -tune film \
    -pix_fmt yuv420p -profile:v high -g 60 -movflags +faststart "$OUT/$name.mp4"
  ffmpeg -v error -y -i "$master" -an -c:v libvpx-vp9 -b:v 0 -crf 38 -row-mt 1 \
    -deadline good -cpu-used 1 -pix_fmt yuv420p -g 60 "$OUT/$name.webm"
  ffmpeg -v error -y -ss "$poster" -i "$master" -frames:v 1 renders/$name-poster.png
  cwebp -quiet -q 82 renders/$name-poster.png -o "$OUT/$name.webp"
  ls -l "$OUT/$name".{mp4,webm,webp} | awk '{print "   " $5 "\t" $9}'
done
