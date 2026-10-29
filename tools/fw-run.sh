#!/usr/bin/env bash
# Flash an ELF to a panel (a Tidbyt unless SCREENY_BOARD says otherwise) and log serial output for a while.
# Talks to the device over serial; one process at a time.
# Usage: [SCREENY_BOARD=art-panel] tools/fw-run.sh <elf> <name> [seconds=20]
#   -> captures/<name>.log (ANSI stripped)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ELF=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
NAME=${2:?name}
SECS=${3:-20}
if [[ -z "${SCREENY_PORT:-}" ]]; then
  echo "SCREENY_PORT is not set. Candidate ports:" >&2
  ls /dev/cu.usbserial-* /dev/ttyUSB* 2>/dev/null >&2 || echo "  (none found)" >&2
  exit 1
fi
PORT=$SCREENY_PORT
cd "$ROOT"
mkdir -p captures

# SCREENY_BOARD picks the flash layout; it must match the ELF's cargo features.
#   tidbyt (default)  Tidbyt Gen 1, 8 MB.
#   art-panel         LED Art Panel (`--features board-art-panel`), 4 MB. Not a
#                     Tidbyt, so no stock backup is asked for: its Arduino sketch
#                     is rebuilt from source (owner, 2026-10-29).
case "${SCREENY_BOARD:-tidbyt}" in
  tidbyt)
    FLASH_SIZE=8mb; TABLE=firmware/partitions.csv
    [[ -n $(find backup -name 'tidbyt-stock-*.bin' -size +8000k 2>/dev/null) ]] || {
      echo "refusing to flash: no stock backup in backup/" >&2; exit 1; } ;;
  art-panel)
    FLASH_SIZE=4mb; TABLE=firmware/partitions-4mb.csv ;;
  *) echo "unknown SCREENY_BOARD: $SCREENY_BOARD" >&2; exit 1 ;;
esac

# 230400 is the fastest baud the Tidbyt's CP2102N link has been seen to
# survive; higher rates corrupted on the author's bench.
# --partition-table: two OTA slots + the settings partition (docs/research/006).
# --erase-data-parts ota: espflash never touches otadata, so without this a stale
# OTA selection survives a serial flash and the device boots the *other* slot.
# With it, a serial flash always wins. --flash-size: the table needs more than
# espflash's 4 MB assumption; say so rather than rely on detection.
# --bootloader: ESP-IDF v6.1 with app rollback enabled (firmware/bootloader/README.md);
# espflash's bundled bootloader never rolls a bad OTA image back.
espflash flash --chip esp32 --port "$PORT" --baud 230400 --non-interactive \
  --flash-size "$FLASH_SIZE" --partition-table "$TABLE" --erase-data-parts ota \
  --bootloader firmware/bootloader/esp32-rollback-bootloader.bin \
  "$ELF" 2>&1 | tail -2

espflash monitor --chip esp32 --port "$PORT" --non-interactive --elf "$ELF" \
  > "captures/$NAME.raw.log" 2>&1 &
MON=$!
sleep "$SECS"
kill $MON 2>/dev/null || true
wait $MON 2>/dev/null || true
sed $'s/\x1b\\[[0-9;]*m//g' "captures/$NAME.raw.log" > "captures/$NAME.log"
rm -f "captures/$NAME.raw.log"
echo "captures/$NAME.log"
