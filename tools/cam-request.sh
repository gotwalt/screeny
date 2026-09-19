#!/usr/bin/env bash
# Ask the capture daemon (tools/cam-daemon.sh) for a still or a clip and wait for it.
# Usage: tools/cam-request.sh NAME            -> captures/NAME.jpg
#        tools/cam-request.sh NAME clip 3     -> captures/NAME.mp4
# Prints the output path. Exits non-zero with ffmpeg's error if the capture failed.
set -euo pipefail
cd "$(dirname "$0")/.."

NAME=${1:?name}
KIND=${2:-snap}
SECS=${3:-}
PIDFILE=captures/.daemon.pid

if ! { [[ -f $PIDFILE ]] && kill -0 "$(cat $PIDFILE)" 2>/dev/null; }; then
  echo "cam-daemon is not running. Start it with: open -a Terminal tools/cam-daemon.sh" >&2
  exit 2
fi

mkdir -p captures/req
rm -f "captures/$NAME.err"
echo "$KIND $SECS" > "captures/req/$NAME.tmp"
mv "captures/req/$NAME.tmp" "captures/req/$NAME.req"

deadline=$(( $(date +%s) + ${SECS:-0} + 30 ))
while [[ -e "captures/req/$NAME.req" || -e "captures/req/$NAME.working" ]]; do
  if (( $(date +%s) > deadline )); then echo "timed out waiting for capture" >&2; exit 3; fi
  sleep 0.2
done

if [[ -s "captures/$NAME.err" ]]; then cat "captures/$NAME.err" >&2; exit 1; fi
[[ $KIND == clip ]] && echo "captures/$NAME.mp4" || echo "captures/$NAME.jpg"
