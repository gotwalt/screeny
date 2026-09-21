---
id: 249
title: Firmware - find 4-8 KB of DRAM, so the dark end can have sub-level planes
type: build
hardware: yes (the orchestrator flashes; the owner judges by eye)
depends: [248]
owner:
branch:
---

## Goal

Card 248 stage B designed the sub-level planes - planes below plane 0, shown once per
refresh with a narrowed output-enable window, giving a real half-level and quarter-level
at the full refresh rate instead of the dither making them by skipping whole refreshes.
It stopped on one number. Free the memory it needs, then hand card 248's stage B back to
a worker.

## The number

`tools/fw-size.sh` on `main` at firmware 0.9.0:

    .data      60108
    .bss      110704
    .stack     25792   <- the remainder of main DRAM; floor 24576

A sub-plane is `size_of::<PlaneData<16, 64>>()` = 2,052 bytes, in **each** of the two
framebuffers: **4,104 bytes** of `.data` per sub-plane. `.data`, `.bss` and core 0's
main stack share one 196,604-byte DRAM region and the linker fills `.data` and `.bss`
first, so every static byte comes off `.stack`.

| | `.stack` | against the 24,576 floor |
|---|---|---|
| today | 25,792 | +1,216 |
| one sub-plane (a half-level) | 21,688 | -2,888 |
| two sub-planes (half and quarter) | 17,584 | -6,992 |

Research 010 measures core 0's worst observed depth at **13,328 bytes**, nearly all of
it reached at boot rather than under load, so the *measured* margin would still be
8.4 KB with one sub-plane and 4.3 KB with two. The floor is the constraint, and
`fw-size.sh`'s own header explains why a floor under the demand is worse than none.
So this card is either "free 4-8 KB" or "the owner moves the floor with a reason and a
number" - not a worker's call.

## Leads, cheapest first

1. **`.bss` is 110,704 bytes and nobody has itemised it since card 227.** Start with
   `xtensa-esp32-elf-nm --size-sort -S` on the elf and find the ten biggest symbols.
   Research 010 sections 3-5 are the method.
2. **The heap arenas are `.bss`.** Card 220 measured the APSTA all-allocations
   watermark at 54,040 of 90,112 - 36 KB free at the worst instant - and card 227
   already cut the second arena from 32 KB to 24 KB on that evidence. Another 8 KB is
   the most obvious single lever, and it is the one with a measurement behind it.
3. **`NET_SOCKETS = 8` and `HTTP_TASKS = 2`** are both sized with a spare. Each socket
   slot is ~408 bytes.
4. **One sub-plane, not two.** A half-level alone removes the biggest dither step; the
   quarter is the refinement. 4,104 bytes instead of 8,208.
5. **The bootloader/partition read frames** (`store::find_partition`'s 3,200 bytes plus
   esp-storage's ~4,150) are what put the boot path at 13 KB. Making the *demand*
   smaller is as good as making the supply bigger, and it is the demand the floor is
   there to protect.

Any of these on its own may be enough. Measure before and after with `tools/fw-size.sh`
and with the device's own `stack:` lines; do not trade a linker number for a runtime
panic.

## Deliverables

- Whatever frees the memory, with the `fw-size.sh` before/after in the log.
- A note in `docs/research/010-stack-and-ram-levers.md` saying which lever was pulled
  and what it cost.
- Card 248 moved back out of `done`, or a new card, for stage B itself: the design,
  the brightness table and the host tests it wants are already written in card 248's
  log, and `firmware/vendor/README.md` has the `bcm_sequence` half.

## Acceptance

- `tools/fw-size.sh` passes with at least 4,104 bytes of room above the 24,576 floor
  (8,208 if both sub-planes are wanted), on a default build.
- The device's measured core 0 high-water is unchanged, or the change is explained.
- Nothing the firmware does today is lost.

## Log
