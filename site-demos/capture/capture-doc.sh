#!/usr/bin/env bash
# Captures the PC's working document and desktop in headless Chrome at 2x:
#   footage/doc.png      1520 x 1560, the reference sheet (doc.html)
#   footage/desktop.png  3840 x 2160, the desktop the iPad mirrors (desktop.html)
#   footage/desktop-pair.png  the same for the pairing film (desktop.html?pair)
#   footage/hero-laptop-<mode>-<link>.png  3142 x 1728, the hero laptop's screen
#   footage/hero-ipad.png  2880 x 1986, the hero iPad as a second screen
#     (hero-desktop.html; needs footage/hero-*.webp from capture-hero.sh)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
chrome="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
profile=$(mktemp -d /tmp/em-doc-chrome.XXXX)
shot() {
  "$chrome" --headless=new --disable-gpu --hide-scrollbars --allow-file-access-from-files \
    --user-data-dir="$profile" --window-size="$3" --force-device-scale-factor=2 \
    --virtual-time-budget=3000 --screenshot="$here/../footage/$1.png" "file://$here/$2" >/dev/null 2>&1
  echo "footage/$1.png"
}
# The pairing film's app, as its footage shows it once the iPad has joined.
ffmpeg -v error -y -ss 6.9 -i "$here/../footage/host-pair.mp4" -frames:v 1 "$here/../footage/pair-live.png"
shot doc doc.html 760,780
shot desktop desktop.html 1920,1080
shot desktop-pair 'desktop.html?pair' 1920,1080
for mode in mirror second; do
  for link in usb wifi; do
    shot "hero-laptop-$mode-$link" "hero-desktop.html?laptop&mode=$mode&link=$link" 1571,864
  done
done
shot hero-ipad 'hero-desktop.html?ipad' 1440,993
rm -rf "$profile"
