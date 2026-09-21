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

### 1. The mechanism, from the sources

**Certainty: certain from the sources.** Every step below is a line of code in the
registry or in `firmware/`, not an inference about silicon. It is a three-party
deadlock between `esp-storage`'s core parking, `esp-sync`'s spin locks and
`esp-rtos`'s cross-core scheduler lock, and it explains every observation on the
card - the drifting sector, the total silence, `panic_count` 0, the dead telemetry
task, and why only a peripheral watchdog gets the device back.

#### The three facts it is built from

1. **Parking core 1 is a hardware clock stall at an arbitrary instruction.**
   `esp-storage`'s `MultiCoreStrategy::AutoPark` calls `CpuControl::park_core`
   (`esp-storage-0.10.0/src/common.rs` line 297), which is
   `internal_park_core(Cpu::AppCpu, true)` -
   `esp-hal-1.2.2/src/soc/esp32/cpu_control.rs` lines 16-37 - two RTC_CNTL writes
   that gate core 1's clock. Core 1 freezes **wherever it is**, including in the
   middle of a critical section, holding whatever lock it held.

2. **Every lock in this stack is a spin lock that masks the holder's interrupts,
   and the important ones are shared by both cores.**
   `esp_sync::RawMutex` -> `GenericRawMutex` -> `LockedState::lock`
   (`esp-sync-0.3.0/src/lib.rs` lines 147-189) is, on `multi_core`, an
   `AtomicUsize` owner plus `loop { if let Some(token) = try_lock() { return } }`
   at lines 184-188: **an unbounded spin**, each attempt doing `rsil {}, 5`
   (`raw.rs` line 73) and, on failure, `wsr.ps` (line 146) and round again.
   Two instances matter:
   - `esp-hal-1.2.2/src/sync.rs` lines 92-111: the **one global**
     `static CRITICAL_SECTION: esp_sync::RawMutex`, which is the entire
     `critical_section` implementation for this chip. Every
     `CriticalSectionRawMutex` in the firmware and in embassy goes through it.
   - `esp-rtos-0.4.0/src/scheduler.rs` line 639:
     `Scheduler { inner: Mutex<RawMutex, GlobalState> }`, taken by
     `SCHEDULER.with` / `with_shared` (lines 648-654). **Core 1 takes this
     constantly**: `esp-rtos-0.4.0/src/embassy/mod.rs` lines 80, 104, 125, 131
     (the executor's thread flag - set, get and wait), `timer/embassy.rs` line 54,
     and every `yield_task` / run-queue operation. Core 1 is the display task's
     `esp_rtos::embassy::Executor` (`firmware/src/main.rs` lines 924-938), so this
     lock is live traffic on core 1 for as long as the device is on.

3. **`esp-storage` re-enables core 0's interrupts *before* it un-parks core 1.**
   This is the bug. `MultiCoreStrategy::with` (`common.rs` lines 339-349) is

   ```rust
   let unpark = self.pre_write()?;   // park core 1
   let result = f();                 // the ROM call
   self.post_write(unpark);          // un-park core 1
   ```

   and the interrupt masking lives **inside `f()`, not around it**: `f()` is the
   closure at `common.rs` lines 173-176, which calls
   `chip_specific::spiflash_erase_sector` (`hardware.rs` lines 17-20), which is
   `maybe_with_critical_section(|| esp_rom_spiflash_erase_sector(..))`
   (`lib.rs` lines 65-77) - a `LOCK.lock(f)` on esp-storage's **own private**
   `RawMutex`. That lock's guard is dropped when `spiflash_erase_sector` returns,
   so `wsr.ps` runs, and core 0 is taking interrupts again while core 1 is
   **still parked**, for the whole of `check_rc`, the closure's return, and
   `post_write`'s own `CPU_CTRL::steal()` + two RTC_CNTL read-modify-writes
   (`common.rs` lines 314-332). Those are ordinary `.text` - flash - so with a
   cache that has just sat through a 40 ms erase they are several cache misses,
   and the RTC_CNTL un-stall itself only takes effect after the write crosses
   into the RTC slow-clock domain. The window is microseconds wide, and it opens
   with **~40 ms of interrupt backlog** queued on core 0.

#### The interleaving

| | core 0 (staging loop) | core 1 (display executor) |
|---|---|---|
| t0 | `ota.rs:327` `region.erase(..)` -> `internal_erase_sector` | running; enters `SCHEDULER.with(..)` (`esp-rtos/src/embassy/mod.rs:104`) and takes the scheduler `RawMutex` (`esp-sync/src/lib.rs:161`) |
| t1 | `common.rs:297` `park_core(AppCpu)` -> `cpu_control.rs:28-35` | **frozen, owning the scheduler lock** |
| t2 | `hardware.rs:19` `LOCK.lock(..)`: `rsil 5`, ~40 ms ROM erase | frozen |
| t3 | `lib.rs:74` lock guard drops -> `raw.rs:146` `wsr.ps` - **interrupts on** | frozen |
| t4 | the backlog fires. `esp-rtos/src/timer/mod.rs:232` `timer_tick_handler` (TIMG0 T0, `Priority1`, bound to core 0 by `esp_rtos::start` at `main.rs:776`) runs and calls `SCHEDULER.with_shared(..)` (`timer/mod.rs:240`) | frozen |
| t5 | `esp-sync/src/lib.rs:184-188`: the owner is core 1, so core 0 **spins for ever**, `rsil 5` on every attempt | frozen |
| t6 | `post_write` (`common.rs:347`) is never reached, so core 1 is **never un-parked** | frozen for ever |

Both cores are now dead and neither can be the one to free the other. Core 0 is
spinning inside an interrupt handler at `PS.INTLEVEL = 5`, so: no log line (the
UART is driven from task context), no panic (nothing faults - it is a live-lock,
which is exactly why `panic_count` is 0 and the panic breadcrumb was never
written), no telemetry, no executor. The only thing left running is TIMG0's
MWDT - a peripheral, clocked independently of both CPUs - which is why card
241b's liveness watchdog is the only thing that has ever recovered this device,
and why before 241b it stayed silent "for minutes".

#### Why it is random, and why 0.6.0 survived once

The park instant is uncorrelated with core 1's lock traffic, so the probability
per sector is just *P(core 1 is inside a cross-core critical section right now)*.
The t3-t6 half is **not** a race: 40 ms of masked time on a device with a 1 kHz
scheduler tick guarantees a pending interrupt at t4 on essentially every sector.
So the whole thing is one coin flip per sector op, weighted by core 1's critical
section duty cycle - a couple of hundred nanoseconds of lock per executor wake,
at a few hundred wakes a second, is order 0.5%.

That predicts a first wedge after ~200 sectors on average, with a very wide
spread, and a run of 244 clean sectors having roughly a 1-in-3 chance. Measured:
**47, 102, 161** of 248, and one clean run of 244. The card's "drifting sector =
a time/race, not an offset" is exactly this, and 0.6.0's clean 244-sector run was
luck of the expected size. The 0.6.0 upload that died "in under 5 s" with no
stream is the same coin: 5 s is ~85 sectors at 58 ms each, well inside the
spread, and **no stream does not mean no core 1** - the display executor wakes on
its own timer whatever the network is doing.

It also explains why this is "latent since card 240" rather than new: card 240
introduced the first code that erases flash hundreds of times in a row. The
settings store has always taken the same path, but a debounced brightness commit
is one erase plus one write - two coin flips a day instead of five hundred in
twenty-five seconds.

#### What this is *not*

- Not the stream, not card 241's changes, not the image, not stack, not heap -
  all already excluded on the card, and none of them appear in the interleaving.
- Not card 241b's `Rtc`-in-a-mutex (already eliminated on the bench, and it is
  not in this path).
- Not `multicore_ignore` territory: core 1 being parked is *correct* and
  necessary. The bug is what core 0 is allowed to do while it is parked.
- Not a flash-bus or cache hazard. I considered one - core 1 frozen mid
  cache-line-fill holding the SPI0/SPI1 arbiter - and I cannot prove or disprove
  it from sources, but it is not needed to explain anything, and the fix below
  narrows it as well (see section 3).

#### The ecosystem already knows

`esp-rtos-0.4.0/src/sleep.rs` lines 118-126 is a `FIXME` on esp-rtos's own
light-sleep hook, which hardware-stalls the other core for the same reason:

> The other core is frozen wherever it happens to be - including in the middle of
> an interrupt handler that holds a cross-core lock [...] this core will spin
> forever trying to take that lock during sleep prep, because the frozen core can
> never release it. We accept this (unlikely) deadlock risk for now [...]

Same hazard, same stack, written down by the people who wrote the park routine.
`esp-storage` accepts it too, silently and five hundred times per upload.

Research 006 section 4's "two rules" (lines 256-264) got this half right:
"`pre_write` parks core 1 *before* core 0 takes `esp-storage`'s lock. If core 1
were ever stalled while holding that same lock, core 0 would spin on it forever.
Because `RawMutex` is per-instance and not a global critical section, core 1
holding *any other* lock is harmless." The first two sentences are right. **The
third is the error**: it is true of the lock core 0 takes *explicitly*, and false
of the locks core 0 takes *implicitly, in an interrupt handler, in the window
esp-storage leaves open between the ROM call and the un-park*. That is the
sentence this card fixes, and it is a bug in the research note as much as in the
code.
