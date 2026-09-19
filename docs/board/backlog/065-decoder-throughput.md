---
id: 065
title: decoder throughput on Xtensa - 9 cycles a byte to fill the frame buffer
type: research
hardware: yes
depends: [008]
---

## Goal

Find out why the decoders cost what they cost on the ESP32, and whether that is
worth changing. Do not change anything until the measurement says what to change.

## Context

Card 008 measured every v1 decoder on the device, over 60 s at 30 fps, as the
EWMA the spec's `decode_us` reports:

| codec | payload | decode, ewma | decode, max |
|---|---|---|---|
| `SOLID` | 3 B | **224 us** | 2635 us |
| `PAL4_LZ` | ~1200 B | 437 us | 2471 us |
| `PAL5` | 1376 B | 464 us | 2694 us |
| `PAL8_LZ` | ~900 B | 636 us | 3560 us |
| `BC1_DUAL` | 1296 B | 790 us | 3150 us |

`SOLID` is the interesting row, because it is the floor: it does no decoding at
all, just writes one colour into 6144 bytes, and takes 224 us. That is about
**9 cycles a byte** at 240 MHz. Every other codec ends in the same per-pixel
write, so 224 us is a tax all five of them pay before doing any work of their
own.

Two candidate causes, and they want different fixes:

1. **Code shape.** `decode_solid` writes `dst[i]`, `dst[i+1]`, `dst[i+2]` a byte
   at a time with bounds checks, and the firmware builds at `opt-level = "s"`.
   `crates/proto` is `#![forbid(unsafe_code)]` and must stay that way - it parses
   network input on a device with no MMU - but a chunked `copy_from_slice` over
   `chunks_exact_mut(3)` is safe, idiomatic, and may be several times faster.
   Check whether `opt-level = 3` on `screeny-proto` alone changes it.
2. **Memory.** The frame slots are ordinary internal DRAM, written by core 0
   while core 1 reads a different slot and the DMA engine streams a third buffer.
   If the cost is bus contention rather than instruction count, code changes will
   not help and the number is simply what this chip does.

Tell them apart before touching anything: time the same decode with the display
task stopped, and time a hand-written chunked fill against the current one.

**None of this is urgent.** 790 us is 2.4% of a 33 ms frame period, and card 008
showed the device holding 30 fps on every codec with the decode path nowhere near
being the constraint. It matters if anyone wants materially more than 30 fps, or
a stateful codec that decodes twice.

## Deliverables

1. A measurement that separates instruction count from memory bandwidth.
2. If it is code: a patch to `crates/proto/src/dec/`, still `no_std` and still
   `forbid(unsafe_code)`, with before/after numbers from the device and no change
   to any test vector's output.
3. Either way, a paragraph in this card's log that the next person can trust.

## Acceptance

The card says why `SOLID` costs 224 us, with evidence, and either improves it or
explains why it cannot be improved.

## Log
