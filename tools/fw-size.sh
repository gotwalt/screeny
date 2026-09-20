#!/usr/bin/env bash
#
# fw-size.sh <elf> - what a firmware build costs in the pool that breaks first.
#
# On the ESP32 `.data`, `.bss` and core 0's main stack come out of one 196 KB
# DRAM region, and the linker fills `.data` and `.bss` first: **`.stack` is the
# remainder**, not a configured size. So every static byte a feature adds comes
# straight out of the stack, and `.stack` is the one number that says whether
# the next feature fits (`docs/design/device-web.md`, "How to think about
# storage and RAM").
#
# `.stack` is a ceiling, not a measurement. What the stack actually reaches is
# the `stack:` line the firmware logs at the 60 s mark (`src/stack_probe.rs`).
# Read both: this script says how much room the linker left, the device says
# how much of it was used.
#
# THE FLOOR IS 16384. The script exits non-zero below it. That is not the
# budget - individual cards set tighter ones (card 222's was 22 KB) - it is the
# point past which a build should not be flashed at all: card 220 measured a
# 10.7 KB high-water mark with WiFi, DHCP, mDNS and the decode path all live,
# and 16 KB leaves only ~5 KB over that for whatever goes deeper next.
#
# Usage:  tools/fw-size.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw
#
# Needs `xtensa-esp32-elf-size` on PATH: `. ~/export-esp.sh` first.

set -euo pipefail

FLOOR=16384

elf="${1:-}"
if [ -z "$elf" ]; then
  echo "usage: tools/fw-size.sh <elf>" >&2
  exit 2
fi
if [ ! -f "$elf" ]; then
  echo "fw-size: no such file: $elf" >&2
  exit 2
fi

size_bin="${XTENSA_SIZE:-xtensa-esp32-elf-size}"
if ! command -v "$size_bin" >/dev/null 2>&1; then
  echo "fw-size: $size_bin not on PATH - run '. ~/export-esp.sh' first" >&2
  exit 2
fi

# One `size -A` pass, parsed once. `image` is the sum of the sections that are
# actually written to flash (everything loadable: code, read-only data, the
# initialisers for `.data`, and the IRAM/DRAM text). `.bss`, `.stack`,
# `.noinit` and `.dram2_uninit` are runtime-only and are not in it.
"$size_bin" -A "$elf" | awk -v floor="$FLOOR" -v elf="$elf" '
  /^\./ {
    size[$1] = $2
    # NOLOAD / runtime-only sections: everything else is in the image.
    if ($1 != ".bss" && $1 != ".stack" && $1 != ".noinit" && $1 != ".dram2_uninit" &&
        $1 !~ /^\.debug/ && $1 != ".comment" && $1 != ".xtensa.info" &&
        $1 !~ /^\.espressif/ && $1 !~ /bss$/ && $1 !~ /dummy$/) {
      image += $2
    }
  }
  END {
    d  = size[".data"]  + size[".data.wifi"]
    b  = size[".bss"]
    st = size[".stack"]
    rw = size[".rwtext"] + size[".rwtext.wifi"]
    printf "fw-size: %s\n", elf
    printf "  .data   %8d  (incl. .data.wifi %d)\n", d, size[".data.wifi"]
    printf "  .bss    %8d\n", b
    printf "  .stack  %8d   <- the remainder of main DRAM; floor %d\n", st, floor
    printf "  .rwtext %8d  (incl. .rwtext.wifi %d)  IRAM\n", rw, size[".rwtext.wifi"]
    printf "  image   %8d  (loadable sections only)\n", image
    printf "  main DRAM: .data + .bss + .stack = %d\n", d + b + st
    if (st < floor) {
      printf "fw-size: FAIL - .stack %d is below the %d floor\n", st, floor
      exit 1
    }
  }
'
