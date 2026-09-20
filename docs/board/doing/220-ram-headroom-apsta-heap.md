---
id: 220
title: Firmware RAM headroom - framebuffers off core 0's stack, and what APSTA really costs in heap
type: build
hardware: yes
depends: [201, 210]
owner: worker-220
branch: card/220-ram-headroom-apsta-heap
---

## Goal

Everything in the device-web track (HTTP server, soft-AP, captive portal, OTA) is gated
on RAM. Card 201's compile-only spike left core 0 with a 13.7 KB stack while `main`
materialises two 12 KB `FrameBuffer` temporaries on it, and nobody has measured what
the radio's heap use is with the AP and the station up together. This card removes the
first problem and measures the second, on the device, so the build cards that follow
are sized against numbers instead of hopes.

## Context

Read first: `CLAUDE.md`, `docs/design/device-web.md`, then
`docs/research/007-device-web-and-portal.md` sections 2 (soft-AP API at this exact
`esp-radio` version, APSTA, the single-PHY trap, 2.4 RAM) and 6 (the RAM budget and
the three levers), `firmware/src/main.rs` (the heap comment at the allocator, the
`FrameBuffer` construction and its comment, `APP_CORE_STACK`), and
`firmware/src/web_spike/ap.rs` (card 201's compile-only APSTA config: it is evidence
that the API calls type-check, not code to keep).

Already established - do not rediscover:

- `.bss`, `.data` and core 0's main stack share one region; `.stack` is whatever is
  left below 0x3ffe0000. `xtensa-esp32-elf-size -A <elf>` shows it as `.stack`.
  Baseline on `main`: `.bss` 127,040, `.stack` 37,536. The toolchain's binutils are under
  `~/.rustup/toolchains/esp/xtensa-esp-elf/*/xtensa-esp-elf/bin/`.
- A buffer held across an `await` inside an embassy task becomes part of that task's
  static future, i.e. `.bss` (card 200 measured 11 KB of exactly that). Anything big
  and short-lived goes on the heap for the duration, or in a `ConstStaticCell`.
- Heap today: 64 KB reclaimed + 32 KB; serial telemetry prints `heap used/size` every
  5 s and shows ~45 KB in use in station mode. `esp-radio`'s docs claim station
  47-57 KB and open AP 53-63 KB; APSTA is unmeasured.
- The device carries the card 210 partition table. **The only flashing path is
  `/Users/aaron/src/screeny/tools/fw-run.sh <elf> <name> [secs]`, called by that
  absolute path in the main checkout** (it passes the partition table and clears
  otadata; never call `espflash flash` yourself). Its serial log lands in
  `/Users/aaron/src/screeny/captures/<name>.log`, which is git-ignored and **contains
  the real SSID**: never paste log lines that carry it into the card, the doc or a
  commit. Quote the heap/telemetry/boot lines only.
- You cannot reach the device over the LAN from a worker environment (unicast is
  dropped). Your evidence is the serial log. The orchestrator runs the LAN-side checks
  (conformance, a stream) after you finish.
- A Studio service streams to this panel around the clock and reconnects by itself
  after every reboot. That is useful: your serial telemetry will show ~30 fps rx
  within ~20 s of boot, which is your "the frame path still works" evidence, for free.

## Deliverables

1. **Framebuffers off the stack.** Construct the two DMA `FrameBuffer`s without a
   12 KB stack temporary (a `ConstStaticCell`, in-place init, or whatever
   `esp-hub75 0.17` / the vendored `hub75-framebuffer` allows - read the source; if
   `new()` is not `const`, say what you did instead and why it is sound). They must
   still come up dimmed before the first refresh (see the comment at the construction
   site: a single refresh at the full OE window is a power bug). Update the comments
   that explain the old constraint, including the heap-allocator comment, so they
   describe the code as it now is.
2. **A number for core 0's real stack use.** Paint-and-scan, `esp-rtos` stack
   statistics if this version exposes them, or another honest method: report the
   high-water mark of core 0's main stack after boot + WiFi join + 60 s of streaming,
   before and after deliverable 1. Log it once at the 60 s mark in the normal
   firmware if that is cheap (one line, not periodic).
3. **The APSTA heap measurement**, behind a cargo feature `apsta-probe` (off by
   default; the default build must be unaffected): configure
   `Config::AccessPointStation` with the normal station config plus an **open** AP
   named `screeny-<id>` (channel follows the station), bring up the second
   `embassy-net` stack on the AP interface with static 192.168.4.1/24 and nothing
   listening on it (no DHCP, no DNS, no HTTP - those are later cards), and log
   `esp_alloc::HEAP.stats()` (used, size, and the minimum free you observe) every 5 s.
   Evidence wanted from one bounded run (about 3 minutes): heap in APSTA idle vs the
   station-only baseline from the same boot sequence; that the station still joins;
   that the telemetry line still shows the Studio's stream arriving at ~30 fps with
   zero decode drops and an unchanged `render` time; and any radio or allocator
   warnings. If APSTA fails to come up or the heap is exhausted, that *is* the result:
   record exactly what happened and stop; do not start redesigning.
4. `docs/research/009-ram-headroom.md`: conclusions first. The table of `.bss` /
   `.stack` / heap before and after; the APSTA heap numbers; the recommended heap
   split for the build cards (is 64 + 32 KB right, given what the stack now needs?);
   the budget left for picoserve (~7.6 KB `.bss`), DHCP/DNS (~5.4 KB) and an OTA
   staging buffer (4 KB heap), using card 201's attribution table.
5. **Leave the device running the default build of your branch** (not the
   `apsta-probe` build), verified by a final boot log, and say so in the report.

## Bench discipline (orders)

- Iterate on the host side first: build, read `xtensa-esp32-elf-size`, only then flash.
  Expect three to five flashes in total, not twenty. Every flash interrupts a live
  service; make each one count and write in the Log why you flashed.
- One long run (the ~3 minute APSTA run), once, on the final probe build. Never repeat
  a passing long run without a firmware change.
- `fw-run.sh` bounds the monitor itself (`secs` argument; use <= 200). Start no other
  monitors, servers or emulators. Serial never above 230400. Never erase flash. Never
  flash if `backup/tidbyt-stock-*.bin` is missing (the script checks).
- No full-white full-brightness frames; you are not drawing anything new, keep it so.
- When you finish: `pgrep -fl espflash` must be empty.

## Out of scope

HTTP, DHCP, DNS, the portal screen, the state machine, the settings store (card 211 is
in flight in `crates/settings`), OTA, the button. `crates/*` and the spec are not
touched. If you find other work, propose it in the research doc (card numbers 221-229
are yours to *suggest*; do not write card files).

## Acceptance

The default firmware boots on the device with the framebuffers off the stack, `.stack`
and the measured high-water mark are reported before and after, the APSTA heap numbers
exist (or the precise failure does), and the device is left streaming on the default
build.

## Log

### 2026-09-19 - host side, before any flash

Claimed the card, branched `card/220-ram-headroom-apsta-heap` off `main` at a2536bc.

**Deliverable 1 - framebuffers off the stack.** `hub75_framebuffer::bitplane::plain::frame::DmaFrameBuffer::new()`
(the vendored copy, which is what `esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer`
re-exports) is already a `const fn`: it builds an all-zero `PlaneData` array and
then calls the `const fn format()`. So the fix is the cheap one the research doc
hoped for and needs no `unsafe`: two module-level
`static_cell::ConstStaticCell<FrameBuffer>`s initialised with `FrameBuffer::new()`,
and `main` just `take()`s them. The compiler produces the 12 KB values, not the
stack.

Measured with `xtensa-esp32-elf-size -A`, release, in this worktree:

| build | `.text` | `.rodata` | `.data` | `.bss` | `.stack` | image |
|---|---|---|---|---|---|---|
| `main` (card's baseline) | 531205 | 73064 | 31492 | 127040 | 37536 | 743408 |
| this branch, `fb-on-stack` (the A/B) | 531621 | 73248 | 31492 | 127064 | 37512 | 744000 |
| this branch, **default** | 531209 | 73176 | **56124** | **102424** | **37512** | 768144 |
| this branch, `apsta-probe` | 534797 | 74184 | 56164 | 106432 | 33472 | 772784 |

So 24,632 bytes moved out of `.bss` and into `.data`, and `.stack` did not move
(37536 -> 37512, and the 24 bytes are the probe's own bookkeeping, not the
framebuffers). That is the expected result and it is the *point*: `.data`,
`.bss` and the stack come out of one region, so relocating 24 KB inside that
region cannot change the remainder. What it removes is the 24 KB **transient** -
what the remainder has to be big enough for. The price is 24 KB of flash
(744,000 -> 768,144, 18.6% of a 2 MB OTA slot).

`apsta-probe` costs +4,008 `.bss` on top of that, which is the AP interface plus
`StackResources<4>`: card 201 predicted ~3.9 KB from the same pair, so that
table holds.

**Deliverable 2 - the method.** `esp-rtos 0.4.0` has no per-task stack
watermark API (it only snapshots a guard word at `stack_bottom + guard_offset`
and compares it on every switch), so: paint and scan, in `firmware/src/stack_probe.rs`.
`_stack_end_cpu0` (0x3ffd6d60, == `_bss_end`) and `_stack_start_cpu0`
(0x3ffe0000) bracket the region; the paint runs as the *first* statement of
`main`, from bottom+1024 (leaving the guard word at bottom+60 alone) up to 512
bytes below the painting function's own frame. The scan runs once, at the 60 s
telemetry tick, and logs one line. It lives in the default firmware on purpose:
the number is only worth having if it is the number the shipping build produces.

**Deliverable 3 - the probe.** `firmware/src/apsta_probe.rs` behind the
`apsta-probe` feature. One task owns the controller and the AP runner so
nothing has to be signalled across tasks: 60 s of station-only (the *same*
`station_loop` the default build runs - extracted, not copied), a heap line,
then `set_config(AccessPointStation)` with an open `screeny-<id>` AP on channel
1, then the second `embassy-net` stack at 192.168.4.1/24 with nothing listening.
Both the station-only baseline and the APSTA number therefore come from one
boot. The feature also turns on `esp-alloc/internal-heap-stats` so the log has a
true `max_usage` beside the 5 s samples; the radio's transients are exactly what
a sampler misses.

Added `fb-on-stack` as a bench-only feature (same spirit as `display-on-core0`)
so the "before" half of the stack measurement is reproducible without checking
out an older commit.

All six feature configurations build clean: default, `fb-on-stack`,
`apsta-probe`, `device-web-spike`, `spike-ota`, `gpio-probe`.

Was told by the orchestrator to hold off the serial port for ~10 minutes while
the owner was at the panel; did host-side work only until released.

### Flash 1 - the probe painted over its own caller's frame

Flashed the `fb-on-stack` build to get the "before" number and got a panic loop
instead: `esp_sync: lock is not reentrant`, on the first poll of `main`, every
boot. Mine, and a good lesson.

`stack_probe::paint()` bounded the paint at "the address of a local in this
function, minus 512". `paint` was inlined into `main`'s poll function, and in
*this particular build* that frame is the 24 KB of framebuffer temporaries the
card exists to remove. The local landed near the top of that 24 KB frame, so
`local - 512` was still thousands of bytes **above** the stack pointer and the
paint went through the live frame and its saved return addresses.

Fixed by taking the bound from a frame we own: `#[inline(never)] fn frame_mark()`
returns the address of its only local. It is a callee of `paint`, so its stack
pointer is strictly below `paint`'s, and `xtensa-esp32-elf-objdump` confirms
`entry a1, 32` with the local at `a1+4` - so the bound is SP-508, below
everything live and below the 16-byte Xtensa register-window spill area, which
is the only thing the hardware writes beneath a stack pointer. Reading `a1`
directly would be exact but inline asm on Xtensa still wants
`#![feature(asm_experimental_arch)]`, and one probe is not worth putting the
firmware on a nightly feature gate.

### Flash 2 - "before": the fear in the research doc was justified

`fb-on-stack`, 115 s, clean boot, no panic.

```
WARN - display: BENCH BUILD - framebuffers built on core 0's stack
INFO - display: core 1, 6 planes, 154 Hz refresh (driver), 12312 bytes/buffer, OE slots 0..=55 (cap 25), OE start 8
INFO - stack: core 0 main high-water 26508 of 37512 bytes, 9980 free (painted at boot)
INFO - telemetry: 30 fps rx, 30 fps shown, 154 swaps/s | drops stale 0 superseded 9 decode 0 rejected 0 gaps 0 | ia 33136 us jit 1875 us | decode 532 us (max 2346) | render 3081 us (max 3212 window, 4038 boot) | state 1 codec 0x10 rssi -71 bright 96 | heap 45612/98304
```

**26,508 bytes of 37,512 used, 11,004 left.** Two 12,312-byte buffers is 24,624
of it, so the rest of the firmware's deepest path is under 2 KB. Card 201's
"everything" build would have left `.stack` at 13,688 - less than half what this
build actually touches. It would not have booted, exactly as the research doc
suspected but could not prove.

Station-only heap steady state: **45,488-45,612 used of 98,304**, matching the
~45 KB card 007 measured. Stream healthy: 30 fps rx, 30 fps shown, 154-155
swaps/s, zero stale/decode/rejected drops, render 3,081-3,147 us.

### Flash 3 - "after": 26,508 -> 6,304

The branch's default build, 115 s, clean boot.

```
INFO - display: core 1, 6 planes, 154 Hz refresh (driver), 12312 bytes/buffer, OE slots 0..=55 (cap 25), OE start 8
INFO - stack: core 0 main high-water 6304 of 37512 bytes, 30184 free (painted at boot)
INFO - telemetry: 30 fps rx, 30 fps shown, 155 swaps/s | drops stale 0 superseded 1 decode 0 rejected 0 gaps 0 | ia 33333 us jit 1371 us | decode 489 us (max 2556) | render 3086 us (max 3214 window, 3457 boot) | state 1 codec 0x10 rssi -57 bright 96 | heap 45540/98304
```

| | `.bss` | `.stack` | high-water | free | heap used |
|---|---|---|---|---|---|
| before (`fb-on-stack`) | 127064 | 37512 | **26508** | 9980 | 45488-45612 |
| after (default) | 102424 | 37512 | **6304** | 30184 | 45540 |

**20,204 bytes came off the stack.** Slightly less than the 24,624 two buffers
weigh, so the old build was already reusing part of one slot for the other -
which is exactly why "how big is the temporary" was never a safe thing to
reason about from the source, and why the number had to be measured.

6,304 bytes is now the true depth of everything else this firmware does:
`esp_hal::init`, `esp_rtos::start`, the HUB75 construction, the WiFi
association, DHCP, mDNS, the decode path and whatever interrupts land on core 0.

Stream unchanged by the move: 30 fps rx, 30 fps shown, 154-155 swaps/s, zero
stale/decode/rejected drops, render 3,086 us against 3,081-3,147 before. Heap
unchanged at 45,540 of 98,304 - as expected, nothing moved to the heap.

### Flash 4 - the APSTA run (the one long run, 195 s)

Clean. Zero WARN and zero ERROR lines in the whole run.

```
INFO - apsta-probe: stage 1, station only, 60 s before the AP goes up
INFO - apsta-probe: stage 1 done: station-only baseline | heap used 45540 of 98304 (52764 free) | max usage 50640 | total alloc 1508912 freed 1463372
INFO - apsta-probe: stage 2, raising open AP "screeny-4a00a4"
INFO - apsta-probe: stage 2: APSTA mode set | heap used 48936 of 98304 (49368 free) | max usage 50640 | total alloc 1516456 freed 1467520
INFO - apsta-probe: stage 3, AP stack at 192.168.4.1 with nothing listening
INFO - stack: core 0 main high-water 6256 of 33472 bytes after APSTA, 26192 free
INFO - apsta-probe: stage 3: APSTA idle | heap used 48868 of 98304 (49436 free) | max usage 53876 | total alloc 3948056 freed 3899188
```

**APSTA costs 3,328 bytes of heap.** Steady used 45,540 -> 48,868 and esp-alloc's
all-allocations watermark 50,640 -> 53,968: the same 3,328 both ways, so the AP's
allocations are structural rather than churn. Free at the worst instant of the
whole run: **44,336 of 98,304**. The lowest the 5 s sampler itself ever saw was
47,840.

esp-radio's docs quote 47-57 KB for a station and 53-63 KB for an open AP. Those
are two alternative totals, not two addends - which is the misreading that most
threatened this whole track.

The switch is graceful: station drops, receiver goes `LIVE -> HOLD` and releases
the source lock, station re-associates, the Studio reconnects ~10 s later,
`HOLD -> LIVE`. Stream after the switch: 30 fps rx, 30 fps shown, 154-155
swaps/s, **zero** stale/decode/rejected drops, render 3,085-3,092 us against
3,086 us station-only. Stack high-water unmoved by the AP.

**One thing that is not explained.** RSSI was -53/-54 dBm for all 11 samples of
stage 1 and -76..-78 dBm for all 26 samples after the switch. A 24 dB step, at
the instant of the switch, sustained. It cost no frames, but the portal's job is
to get a station associated from wherever the panel is sitting, and this is
worth reproducing before anyone designs around it. Proposed as card 221.

### Flash 5 - final, the device is on this branch's default build

```
INFO - display: core 1, 6 planes, 154 Hz refresh (driver), 12312 bytes/buffer, OE slots 0..=55 (cap 25), OE start 8
INFO - device: mac [b4, 8a, 0a, 4a, 00, a4] id 4a00a4
INFO - stack: core 0 main high-water 6304 of 37512 bytes, 30184 free (painted at boot)
INFO - telemetry: 30 fps rx, 30 fps shown, 155 swaps/s | drops stale 0 superseded 0 decode 0 rejected 0 gaps 0 | ia 33317 us jit 1142 us | decode 541 us (max 2362) | render 3086 us (max 3293 window, 3461 boot) | state 1 codec 0x10 rssi -54 bright 96 | heap 45540/98304
```

No panic, no warnings, no `apsta-probe` or `BENCH BUILD` lines - the default
build, streaming, 30 fps, zero drops. `pgrep -fl espflash` is empty.

Five flashes: the panicking probe, the fixed "before", the "after", the APSTA
run, and the restore. Four were necessary; the first was my bug.

### Conclusions written up

`docs/research/009-ram-headroom.md`, conclusions first: the before/after table,
the APSTA heap numbers, the recommended heap split (**keep 64 + 32 KB; do not
take card 201's lever 3, and do not grow the heap either - it comes out of the
same region `.bss` and `.stack` share**) and the ~17.1 KB `.bss` budget for
picoserve / DHCP / DNS / the AP stack, which leaves `.stack` at ~20.4 KB against
a measured demand of 6,304. Cards 221-226 proposed there, no card files written.
