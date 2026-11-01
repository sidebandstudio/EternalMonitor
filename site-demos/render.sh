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
# simulator. A name ending in -m is the phone version: the same composition
# at 728 x 1040, the shape pair and fit already have; its script frames it
# for that width. It is rendered once at high quality, then encoded for the web:
# H.264 MP4 (yuv420p, faststart), VP9 WebM and a WebP poster, the MP4 and
# WebM together kept near 1.5 MB. Needs Node, ffmpeg and cwebp.
#
#   site-demos/render.sh readme-demo   only the README's demo
#
# readme-demo.html plays second-screen and pencil as one 1280 x 720 loop. It
# becomes docs/assets/readme-demo.webp, an animated WebP at 860 px and 15 fps
# under 4 MB, because GitHub doesn't play a repository's own video files in a
# README. Needs img2webp too. Lower quality bands the dark sky.
#
#   site-demos/render.sh launch-film   only the launch film (never by default)
#
# launch-film.html is a 1920 x 1080, 35 s film for launch posts: the site's
# hero, the second-screen and pencil films, three GPUs and the name. It is too
# large for the repository, so it goes to $LAUNCH_FILM_DIR (default
# renders/launch-film/): launch-film.mp4 (H.264, yuv420p, faststart), a poster
# and six stills.
set -euo pipefail
cd "$(dirname "$0")"
OUT=../docs/assets/demos
HF="npx --yes hyperframes@0.8.220"

prep() {
  mkdir -p ui/fonts
  cp ../docs/assets/icon.png ui/
  cp ../docs/assets/fonts/Geist-{Regular,Medium,SemiBold}.woff2 ui/fonts/
  mkdir -p ui/hero
  cp ../docs/assets/hero/{devices,cable,laptop-second-usb,laptop-second-wifi,ipad-second}@2x.webp ui/hero/
}
prep
[ "${1:-}" = "--prep" ] && exit 0

# name:poster second:MP4 CRF:WebM CRF. Busy films get higher CRFs to stay in budget.
FILMS=(second-screen:6.0:33:50 pair:4.4:26:41 fit:4.0:25:38 performance:1.3:32:49 pencil:6.6:26:40 feedback:6.5:27:43
       second-screen-m:6.0:30:46 performance-m:1.3:28:44 pencil-m:6.6:26:40 feedback-m:6.5:26:42)
mkdir -p "$OUT" renders
for entry in "${FILMS[@]}"; do
  IFS=: read -r name poster crf264 crfvp9 <<< "$entry"
  if [ $# -gt 0 ] && [[ ! " $* " == *" $name "* ]]; then continue; fi
  echo "== $name"
  master=renders/$name-master.mp4
  comp=$name.html
  if [[ $name == *-m ]]; then
    base=${name%-m}
    comp=.$name.html
    sed -e "s/data-composition-id=\"$base\"/data-composition-id=\"$name\"/" \
        -e 's/data-width="[0-9]*"/data-width="728"/' -e 's/content="width=[0-9]*/content="width=728/' \
        "$base.html" > "$comp"
  fi
  $HF render -c "$comp" --fps 30 --crf 10 --quiet -o "$master"
  ffmpeg -v error -y -i "$master" -an -c:v libx264 -preset veryslow -crf "$crf264" -tune film \
    -pix_fmt yuv420p -profile:v high -g 60 -movflags +faststart "$OUT/$name.mp4"
  ffmpeg -v error -y -i "$master" -an -c:v libvpx-vp9 -b:v 0 -crf "$crfvp9" -row-mt 1 \
    -deadline good -cpu-used 1 -pix_fmt yuv420p -g 60 "$OUT/$name.webm"
  ffmpeg -v error -y -ss "$poster" -i "$master" -frames:v 1 renders/$name-poster.png
  cwebp -quiet -q 82 renders/$name-poster.png -o "$OUT/$name.webp"
  [[ $comp == .* ]] && rm -f "$comp"
  ls -l "$OUT/$name".{mp4,webm,webp} | awk '{print "   " $5 "\t" $9}'
done

if [ $# -eq 0 ] || [[ " $* " == *" readme-demo "* ]]; then
  echo "== readme-demo"
  master=renders/readme-demo-master.mp4
  $HF render -c readme-demo.html --fps 30 --crf 10 --quiet -o "$master"
  rm -rf renders/readme-demo-frames && mkdir renders/readme-demo-frames
  ffmpeg -v error -y -i "$master" -vf "fps=15,scale=860:-2:flags=lanczos" renders/readme-demo-frames/%04d.png
  img2webp -loop 0 -sharp_yuv -lossy -q 82 -m 6 -d 67 renders/readme-demo-frames/*.png -o ../docs/assets/readme-demo.webp
  ls -l ../docs/assets/readme-demo.webp | awk '{print "   " $5 "\t" $9}'
fi

if [[ " $* " == *" launch-film "* ]]; then
  echo "== launch-film"
  dir=${LAUNCH_FILM_DIR:-renders/launch-film}
  mkdir -p "$dir"
  master=renders/launch-film-master.mp4
  $HF render -c launch-film.html --fps 30 --crf 10 --quiet -o "$master"
  ffmpeg -v error -y -i "$master" -an -c:v libx264 -preset veryslow -crf 19 -tune film \
    -pix_fmt yuv420p -profile:v high -g 60 -movflags +faststart "$dir/launch-film.mp4"
  # The poster is the hero with its headline; the stills are evenly spaced.
  ffmpeg -v error -y -ss 2.5 -i "$master" -frames:v 1 "$dir/launch-film-poster.png"
  len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$master")
  for i in 1 2 3 4 5 6; do
    ffmpeg -v error -y -ss "$(echo "$len * ($i - 0.5) / 6" | bc -l)" -i "$master" -frames:v 1 "$dir/launch-film-still-$i.png"
  done
  ls -l "$dir"/launch-film.mp4 | awk '{print "   " $5 "\t" $9}'
fi
