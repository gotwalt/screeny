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
