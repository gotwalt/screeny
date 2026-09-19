#!/usr/bin/env bash
# Capture-on-request daemon. Run this from a terminal app that has macOS camera
# permission (Terminal.app / iTerm); processes that lack it can then request
# captures through the filesystem:
#
#   echo snap    > captures/req/foo.req   ->  captures/foo.jpg
#   echo "clip 3" > captures/req/bar.req  ->  captures/bar.mp4   (3 seconds)
#
# The .req file is removed when the capture is complete (or failed; see foo.err).
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p captures/req
echo "cam-daemon: watching captures/req (ctrl-c to stop)"
while true; do
  for req in captures/req/*.req; do
    [[ -e "$req" ]] || continue
    name=$(basename "$req" .req)
    read -r kind secs < "$req" || true
    case "$kind" in
      clip) out="captures/$name.mp4"; args=("$out" "${secs:-2}") ;;
      *)    out="captures/$name.jpg"; args=("$out") ;;
    esac
    if tools/cam-snap.sh "${args[@]}" 2> "captures/$name.err"; then
      rm -f "captures/$name.err"
      echo "captured $out"
    else
      echo "FAILED $name (see captures/$name.err)"
    fi
    rm -f "$req"
  done
  sleep 0.2
done
