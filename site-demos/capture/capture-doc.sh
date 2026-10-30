#!/usr/bin/env bash
# Captures the PC's working document and desktop in headless Chrome at 2x:
#   footage/doc.png      1520 x 1560, the reference sheet (doc.html)
#   footage/desktop.png  3840 x 2160, the desktop the iPad mirrors (desktop.html)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
chrome="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
profile=$(mktemp -d /tmp/em-doc-chrome.XXXX)
shot() {
  "$chrome" --headless=new --disable-gpu --hide-scrollbars --allow-file-access-from-files \
    --user-data-dir="$profile" --window-size="$2" --force-device-scale-factor=2 \
    --virtual-time-budget=3000 --screenshot="$here/../footage/$1.png" "file://$here/$1.html" >/dev/null 2>&1
  echo "footage/$1.png"
}
shot doc 760,780
shot desktop 1920,1080
rm -rf "$profile"
