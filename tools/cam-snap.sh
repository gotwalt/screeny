#!/usr/bin/env bash
# Grab a still (or a short clip) of the Tidbyt from the bench camera.
# Usage: tools/cam-snap.sh [out.jpg]            one frame
#        tools/cam-snap.sh out.mp4 <seconds>    clip at 30 fps
# Hardware-owner only (see docs/README.md). Requires macOS camera permission for
# the app that launches it.
set -euo pipefail

CAM_NAME=${SCREENY_CAM:-"Anker PowerConf C200"}
OUT=${1:-captures/snap-$(date +%Y%m%d-%H%M%S).jpg}
SECS=${2:-}
mkdir -p "$(dirname "$OUT")"

# -nostdin: ffmpeg otherwise stops when run from a background shell.
# The C200 only offers uyvy422/yuyv422/nv12, so ask for one explicitly.
IN=(-hide_banner -nostdin -loglevel error -f avfoundation -framerate 30
    -pixel_format uyvy422 -video_size 1920x1080 -i "${CAM_NAME}:none")

if [[ -n "$SECS" ]]; then
  ffmpeg "${IN[@]}" -t "$SECS" -c:v libx264 -preset ultrafast -crf 12 -y "$OUT"
else
  # Skip the first ~20 frames so auto-exposure settles, then keep one.
  ffmpeg "${IN[@]}" -vf "select=gte(n\,20)" -frames:v 1 -update 1 -q:v 2 -y "$OUT"
fi
echo "$OUT"
