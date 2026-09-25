#!/usr/bin/env bash
# Grab a still (or a short clip) of the Tidbyt from a camera.
# Usage: tools/cam-snap.sh [out.jpg]            one frame
#        tools/cam-snap.sh out.mp4 <seconds>    clip at 30 fps
# Talks to the camera over avfoundation; one process at a time. Requires
# macOS camera permission for the app that launches it.
set -euo pipefail

if [[ -z "${SCREENY_CAM:-}" ]]; then
  echo "SCREENY_CAM is not set: it names the avfoundation device to open." >&2
  echo "List yours with: ffmpeg -f avfoundation -list_devices true -i ''" >&2
  exit 1
fi
CAM_NAME=$SCREENY_CAM
OUT=${1:-captures/snap-$(date +%Y%m%d-%H%M%S).jpg}
SECS=${2:-}
mkdir -p "$(dirname "$OUT")"

# -nostdin: ffmpeg otherwise stops when run from a background shell.
# SIZE/RATE below are tuned for the author's camera (an Anker PowerConf C200,
# which only offers uyvy422/yuyv422/nv12 and advertises this exact rate -
# plain "30" fails to lock the device); override both for a different camera.
SIZE=${SCREENY_CAM_SIZE:-1920x1080}
RATE=${SCREENY_CAM_RATE:-30.000030}
IN=(-hide_banner -nostdin -loglevel error -f avfoundation -framerate "$RATE"
    -pixel_format uyvy422 -video_size "$SIZE" -i "${CAM_NAME}:none")

if [[ "$OUT" == *modes* ]]; then
  # Asking for an impossible size makes ffmpeg print the supported modes.
  ffmpeg -hide_banner -nostdin -f avfoundation -video_size 1x1 -i "${CAM_NAME}:none" 2>&1 | grep -E "@|fps" >&2
  exit 1
fi

if [[ -n "$SECS" ]]; then
  ffmpeg "${IN[@]}" -t "$SECS" -c:v libx264 -preset ultrafast -crf 12 -y "$OUT"
else
  # Skip the first ~20 frames so auto-exposure settles, then keep one.
  ffmpeg "${IN[@]}" -vf "select=gte(n\,20)" -frames:v 1 -update 1 -q:v 2 -y "$OUT"
fi
echo "$OUT"
