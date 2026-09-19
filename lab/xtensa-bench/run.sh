#!/usr/bin/env bash
# Count the Xtensa instructions one frame decode executes, per wire mode.
#
# For each payload in lab/out/payloads (written by `cargo run --release` in
# lab/), build this crate twice -- once decoding the payload once, once
# decoding it 11 times -- run both under qemu-system-xtensa with one
# instruction per translation block, and take (n11 - n1) / 10. Startup, the
# payload load and the exit all cancel, so what is left is one decode.
#
# qemu's `sim` machine models a dc232b rather than the ESP32's LX6. That is
# fine for *counting* instructions -- this code is base-ISA integer work -- but
# it says nothing about cycles; see docs/research/002-frame-encoding.md for how
# the count is turned into a time.
#
# Usage: ./run.sh            (writes a markdown table on stdout)
set -euo pipefail
cd "$(dirname "$0")"

ESP_BIN=/Users/aaron/.rustup/toolchains/esp/xtensa-esp-elf/esp-15.2.0_20250920/xtensa-esp-elf/bin
export PATH="$ESP_BIN:$PATH"
PAYLOADS=../out/payloads
LOG=$(mktemp -d)
trap 'rm -rf "$LOG"' EXIT

ELF=target/xtensa-esp32-none-elf/release/xtensa-bench

count() { # $1 = payload path, $2 = reps -> instructions executed
  BENCH_PAYLOAD="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")" BENCH_REPS="$2" \
    cargo +esp build --release -q 2>/dev/null
  qemu-system-xtensa -machine sim -cpu dc232b -m 128M -nographic \
    -monitor none -serial none -semihosting \
    -accel tcg,one-insn-per-tb=on -kernel "$ELF" \
    -d exec,nochain -D "$LOG/q.log" >/dev/null 2>&1 || true
  grep -c . "$LOG/q.log"
}

echo "| mode | payload B | instructions/frame | us @240 MHz, CPI 1.0 | us @240 MHz, CPI 1.5 | % of a 33.3 ms frame (CPI 1.5) |"
echo "|---|---|---|---|---|---|"
for f in "$PAYLOADS"/*.bin; do
  m=$(basename "$f" .bin)
  b=$(wc -c < "$f" | tr -d ' ')
  n1=$(count "$f" 1)
  n11=$(count "$f" 11)
  per=$(( (n11 - n1) / 10 ))
  awk -v m="$m" -v b="$b" -v n="$per" 'BEGIN{
    t1 = n/240.0; t15 = n*1.5/240.0;
    printf "| `%s` | %d | %d | %.1f | %.1f | %.2f%% |\n", m, b, n, t1, t15, t15/33333.0*100.0
  }'
done
