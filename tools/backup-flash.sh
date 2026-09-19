#!/usr/bin/env bash
# Dump the full ESP32 flash in chunks, retrying each chunk on serial corruption.
# esptool verifies every read against an MD5 computed on-device, so a chunk that
# succeeds is known-good. Usage: tools/backup-flash.sh <port> <out.bin> [flash_size_hex]
set -euo pipefail

PORT=${1:?port}
OUT=${2:?output file}
SIZE=$(( ${3:-0x800000} ))
CHUNK=$(( 0x40000 ))
BAUDS=(460800 230400 230400 115200 115200 115200)

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

for (( off = 0; off < SIZE; off += CHUNK )); do
  part=$(printf '%s/part-%08x.bin' "$TMP" "$off")
  ok=0
  for baud in "${BAUDS[@]}"; do
    if esptool --port "$PORT" --baud "$baud" read-flash "$off" "$CHUNK" "$part" >"$TMP/log" 2>&1; then
      ok=1
      printf 'ok   0x%08x @ %d\n' "$off" "$baud"
      break
    fi
    printf 'retry 0x%08x @ %d: %s\n' "$off" "$baud" "$(grep -i error "$TMP/log" | tail -1)"
    sleep 1
  done
  if [[ $ok -ne 1 ]]; then
    echo "FAILED at offset $off" >&2
    exit 1
  fi
done

cat "$TMP"/part-*.bin > "$OUT"
ls -l "$OUT"
shasum -a 256 "$OUT"
