#!/usr/bin/env bash
# Capture-on-request daemon. Must run from an app that has macOS camera permission
# (Terminal.app / iTerm). The Claude desktop app, including its terminal pane, does
# not have it, so start this with:
#
#   open -a Terminal tools/cam-daemon.sh
#
# Other processes then request captures through the filesystem:
#
#   echo snap     > captures/req/foo.req   ->  captures/foo.jpg
#   echo "clip 3" > captures/req/bar.req   ->  captures/bar.mp4   (3 seconds)
#
# The .req file disappears when the capture is done. On failure captures/foo.err
# holds ffmpeg's stderr. Only one daemon may run: two would open the camera at the
# same time and one would fail with "Could not lock device for configuration".
set -uo pipefail
export PATH="/opt/homebrew/bin:$PATH"
cd "$(dirname "$0")/.."
mkdir -p captures/req

LOCK=captures/.daemon.pid
if [[ -f $LOCK ]] && kill -0 "$(cat $LOCK)" 2>/dev/null; then
  echo "cam-daemon: already running as pid $(cat $LOCK); not starting a second one."
  exit 0
fi
echo $$ > $LOCK
trap 'rm -f $LOCK' EXIT

echo "cam-daemon: pid $$ watching captures/req (ctrl-c to stop)"
while true; do
  for req in captures/req/*.req; do
    [[ -e "$req" ]] || continue
    name=$(basename "$req" .req)
    work="captures/req/$name.working"
    mv "$req" "$work" 2>/dev/null || continue   # atomic claim
    read -r kind secs < "$work" || true
    case "$kind" in
      clip) out="captures/$name.mp4"; args=("$out" "${secs:-2}") ;;
      *)    out="captures/$name.jpg"; args=("$out") ;;
    esac
    if tools/cam-snap.sh "${args[@]}" > /dev/null 2> "captures/$name.err"; then
      rm -f "captures/$name.err"
      echo "$(date +%H:%M:%S) captured $out"
    else
      echo "$(date +%H:%M:%S) FAILED $name (see captures/$name.err)"
    fi
    rm -f "$work"
  done
  sleep 0.2
done
