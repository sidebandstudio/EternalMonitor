#!/usr/bin/env bash
# Renders what the iPad's camera sees for the pairing film (camera.html) to
# camera.mp4 in the given folder. The simulator has no camera, so the film's
# scratch build of the iPad app plays this file in its real QR scanner.
#   site-demos/capture/capture-camera.sh <out-dir>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?out dir}
# The dialog as the Windows app drew it, 2.6 s into its footage.
ffmpeg -v error -y -ss 2.6 -i "$here/../footage/host-pair.mp4" -frames:v 1 "$here/../footage/pair-dialog.png"
(cd "$here" && npx --yes hyperframes@0.8.220 render -c camera.html --fps 30 --crf 16 --quiet -o "$out/camera.mp4")
echo "$out/camera.mp4"
