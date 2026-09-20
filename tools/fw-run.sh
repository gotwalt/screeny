#!/usr/bin/env bash
# Flash an ELF to the Tidbyt, log serial output for a while, and grab a camera still.
# Hardware-owner only (see docs/README.md).
# Usage: tools/fw-run.sh <elf> <name> [seconds=20]
#   -> captures/<name>.log (ANSI stripped) and captures/<name>.jpg
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ELF=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
NAME=${2:?name}
SECS=${3:-20}
PORT=${SCREENY_PORT:-/dev/cu.usbserial-2140}
cd "$ROOT"
mkdir -p captures

[[ -n $(find backup -name 'tidbyt-stock-*.bin' -size +8000k 2>/dev/null) ]] || {
  echo "refusing to flash: no stock backup in backup/" >&2; exit 1; }

# 230400 is the fastest baud this bench's serial link survives.
# --partition-table: two OTA slots + the settings partition (docs/research/006).
# --erase-data-parts ota: espflash never touches otadata, so without this a stale
# OTA selection survives a serial flash and the device boots the *other* slot.
# With it, a serial flash always wins. --flash-size: the table needs more than
# espflash's 4 MB assumption; say so rather than rely on detection.
espflash flash --chip esp32 --port "$PORT" --baud 230400 --non-interactive \
  --flash-size 8mb --partition-table firmware/partitions.csv --erase-data-parts ota \
  "$ELF" 2>&1 | tail -2

espflash monitor --chip esp32 --port "$PORT" --non-interactive --elf "$ELF" \
  > "captures/$NAME.raw.log" 2>&1 &
MON=$!
sleep "$SECS"
tools/cam-request.sh "$NAME" || true
kill $MON 2>/dev/null || true
wait $MON 2>/dev/null || true
sed $'s/\x1b\\[[0-9;]*m//g' "captures/$NAME.raw.log" > "captures/$NAME.log"
rm -f "captures/$NAME.raw.log"
echo "captures/$NAME.log"
