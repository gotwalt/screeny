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

if [[ -z "${ESP_BIN:-}" ]]; then
  ESP_BIN=$(ls -d "$HOME"/.rustup/toolchains/esp/xtensa-esp-elf/*/xtensa-esp-elf/bin 2>/dev/null | head -1) || true
fi
[[ -n "$ESP_BIN" ]] || { echo "run.sh: no xtensa-esp-elf toolchain found under \$HOME/.rustup; set ESP_BIN or run espup first" >&2; exit 1; }
export PATH="$ESP_BIN:$PATH"
PAYLOADS=../out/payloads
LOG=$(mktemp -d)
trap 'rm -rf "$LOG"' EXIT

ELF=target/xtensa-esp32-none-elf/release/xtensa-bench

count() { # $1 = payload path, $2 = reps -> instructions executed
  BENCH_PAYLOAD="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")" BENCH_REPS="$2" \
    cargo +esp build --release -q 2>/dev/null
  # SAFETY: a bench binary that never reaches its semihosting exit (e.g. a decode
  # error that lands in the panic handler's `loop {}`) makes qemu log forever. On
  # 2026-09-19 an orphaned run wrote a 196 GB log in two hours and nearly filled the
  # disk. So: cap the log at 2 GB (`ulimit -f`, 512-byte blocks -> SIGXFSZ) and kill
  # qemu after 120 s. A normal run is a few seconds and a few hundred MB.
  rm -f "$LOG/q.log"
  (
    ulimit -f 4194304
    qemu-system-xtensa -machine sim -cpu dc232b -m 128M -nographic \
      -monitor none -serial none -semihosting \
      -accel tcg,one-insn-per-tb=on -kernel "$ELF" \
      -d exec,nochain -D "$LOG/q.log" >/dev/null 2>&1 &
    q=$!
    ( sleep 120; kill -9 $q 2>/dev/null ) &
    w=$!
    wait $q 2>/dev/null
    rc=$?
    kill $w 2>/dev/null
    exit $rc
  )
  rc=$?
  if (( rc > 128 )); then
    echo "xtensa-bench: qemu was killed (timeout or log cap) on $1 reps=$2; the binary did not exit" >&2
    echo 0
    return
  fi
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
