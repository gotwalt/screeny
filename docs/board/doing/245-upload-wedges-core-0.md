---
id: 245
title: Firmware - staging an upload wedges core 0 at a random sector: find the race in the flash-write / core-parking path and remove it
type: build
hardware: orchestrator flashes and uploads (the worker reads, builds and host-tests)
depends: [240, 241]
owner: worker-245
branch: card/245-upload-wedge
---

## Goal

`POST /api/v1/firmware` wedges the device part-way through staging, almost every time. It
is **a race, latent since card 240** - fw 0.6.0's one clean upload on the bench was luck.
Find the mechanism, remove it, and leave the settings store's own flash writes (the same
path, exercised ~once a day instead of ~500 times in 25 s) safe by the same argument.
OTA (cards 240/241, the owner's "full plan") is blocked on this and on nothing else.

## Context: everything measured (orchestrator, 2026-09-20, serial attached, Mac wired)

- fw 0.7.0 (`main`, with card 241b's always-on MWDT0 liveness watchdog, 20 s, fed from
  `telemetry_task` and from the staging loop per sector): three stage-only uploads
  (`screeny-probe fw-upload screeny-fw-0.7.1-good.bin`, 1,014,288 B, 248 sectors) died
  after **102, 47 and 161 sectors**. Each time: probe `write: Broken pipe`, serial goes
  completely quiet (no WARN/ERROR/panic banner), then `rst:0x7 (TG0WDT_SYS_RESET)` and the
  next boot's `boot: the previous boot was reset while a FIRMWARE UPLOAD was in flight -
  reset reason timer_wdt, after N sector(s)`. `GET /api/v1/panic` ->
  `{"boot_count":4,"panic_count":0,"last_panic":null,"update":null,"last_reset":"wdt"}`.
  **Drifting sector = a time/race, not an offset. `panic_count` 0 = the panic handler never
  ran** (its breadcrumb write comes before any printing), so this is a wedge, not a panic
  whose print deadlocked.
- **The liveness watchdog is fed per sector from `stage_sector`/`read_back`, and it still
  fired**: so core 0 is not looping through sectors slowly - it stops *inside* something
  and never returns for 20 s. The telemetry task (core 0) stops too.
- fw 0.6.0 (card 240, no watchdog), this evening: hung on the **first** upload, with **no
  stream running** (telemetry `0 fps rx` before it), and logged nothing after
  `ota: upload started` - i.e. it died in under 5 s. Earlier the same day the same build
  staged 998,448 B cleanly once (244 sectors, erase 34/40/55 ms min/mean/max, write
  7/9.5/11 ms, `link downs +0`). So: not the stream, not 241's changes, not the image.
- Not stack (12-14 KB free during an upload), not heap (flat ~50 KB of 90 KB).
- Before 0.7.0's watchdog, the wedged device stayed silent for minutes: nothing resets it.
  What the **panel** shows while wedged has not been observed (lit and moving = core 1
  alive; frozen or dark = core 1 parked for ever) - the owner can say if asked, but design
  the fix without needing it.
- Card 241b's removal of the `Rtc`-in-a-mutex-awaited-before-the-RTOS did not fix it
  (suspect eliminated).

## Where to look first (research 006 section 4 predicted this)

`esp-storage`'s `multicore_auto_park` (the feature this firmware relies on so the HUB75
refresh on core 1 cannot touch flash-resident code during an erase/write): read exactly
how it parks the other core, what it waits for, with which interrupts masked, and how it
un-parks. Then enumerate every lock/critical section both cores can hold - `esp_sync`
`RawMutex`/critical-section (global, cross-core), esp-println's lock, embassy mutexes,
the display triple-buffer's atomics, `STORE`, the time driver's alarm handling, esp-rtos's
scheduler lock and its cross-core interrupt - and find the interleaving where core 0,
inside or around the ROM call with interrupts masked, waits on something only a parked (or
interrupt-starved) core 1 can release, **or** where core 1 is parked while holding a
global critical section that core 0's park routine itself then needs. Also check: the
WiFi/timer interrupts that fire on core 0 the moment interrupts are unmasked after ~40 ms;
esp-rtos timeslicing and the MWDT feed; whether `FlashStorage` is constructed per sector
(and what its constructor/destructor does with the other core); `#[ram]` placement of
everything the display ISR and the park handler touch (anything in flash = instant wedge
when the cache is off). Card 240's and 243's Logs have the lock map and the staging
design; `firmware/src/ota.rs`, `store.rs`, `display.rs`, `main.rs` (core 1 start-up).

## Steps

1. Reproduce by reasoning: name the interleaving(s), with file:line in the registry
   sources and in `firmware/`. If more than one is plausible, rank them and say what bench
   observation separates them.
2. Fix it properly (examples, not instructions: park/un-park by our own protocol that core
   1 acknowledges from a safe point *outside* any critical section; never take a global
   critical section on core 1 around work that can be interrupted by a park; run the ROM
   call from a context that holds no lock; keep the display ISR and everything it touches
   in IRAM/DRAM and drop auto-park altogether if that is what 006 section 4 allows).
   The panel may blank or freeze for the ~50 ms of a sector; it must not tear into garbage
   and must come back. Settings commits must use the same fixed path.
3. A stress hook for the bench, off by default (cargo feature `flash-stress`): N
   erase+write+read-back cycles on sectors of the **inactive slot only** at full speed with
   a stream running, reporting cycles done / max masked window / anything odd - so the
   orchestrator can prove "500 cycles, three times, no wedge" in a minute instead of
   inferring it from uploads.
4. Keep the liveness watchdog and the upload breadcrumb exactly as they are.
5. `FW_VERSION` stays "0.7.0" unless you change behaviour a client can see.

## Exit

All builds over the `fw-size.sh` floor (`.stack` >= 24,576; 26,200 now) with numbers in the
Log; `timeout 1200 cargo test` green; rebuilt artefacts in the scratchpad with the same
names as card 241 (`screeny-fw-0.7.0-default.elf`, `screeny-fw-0.7.1-good.bin`,
`-unhealthy.bin`, `-panic.bin` + ELFs) plus `screeny-fw-0.7.0-flash-stress.elf`; a bench
procedure: stress build first (what to run, what to expect), then three stage-only uploads,
then card 241's steps 1-3 unchanged.

## Rules

Branch `card/245-upload-wedge` from current `main`, in your own worktree; commit and Log as
you go, by explicit path; do not merge, do not push. **You do not touch hardware**: no
flashing, no serial port, no LAN, no camera. Bounded commands only; nothing left running.
Never write a real SSID or password (dummies `Example-Wifi1` / `password9`; never quote
`captures/` lines containing an SSID). Leave `crates/art` and `crates/studio` alone.
Writes stay inside the inactive app slot and `otadata`; the stress hook must be
structurally unable to touch anything else (use `store::InactiveSlot`).
**If you cannot find the mechanism, say so plainly** and give the orchestrator the
cheapest bench experiments that would split the candidates - do not ship a guess as a fix.

## Log
