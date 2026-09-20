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
# the `stack:` lines the firmware logs - once at the 60 s mark for both cores,
# and again from `watch_task` whenever a mark grows (`src/stack_probe.rs`).
# Read both: this script says how much room the linker left, the device says
# how much of it was used.
#
# THE FLOOR IS 28672 (28 KB), raised from 16384 by card 227. The script exits
# non-zero below it. That is not a budget - individual cards set tighter ones -
# it is the point past which a build should not be flashed at all, and card 227
# is what makes 28 KB the right number:
#
#   * The **measured demand** is no longer card 220's 10.7 KB. Real TCP traffic
#     takes core 0 to ~17.9 KB, because picoserve's nested-`Either` router puts
#     two frames totalling 7,872 bytes on the stack before a handler runs
#     (`docs/research/010-stack-and-ram-levers.md`, section 2). The old floor
#     was *below* the demand: a build could have passed this check and still
#     have died on the guard.
#   * 28 KB is that 17.9 KB plus the ~9 KB of `.bss` card 223's soft-AP, DHCP,
#     DNS and portal need, which is the next thing to be added. A build that
#     cannot clear the floor cannot clear the next card either.
#
# Raising it is the point: it has to fail for a build that would once have been
# shipped. If a legitimate build cannot make 28 KB, the answer is a lever from
# research 010 section 4, not a lower number here.
#
# Usage:  tools/fw-size.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw
#
# Needs `xtensa-esp32-elf-size` on PATH: `. ~/export-esp.sh` first.

set -euo pipefail

FLOOR=28672

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
