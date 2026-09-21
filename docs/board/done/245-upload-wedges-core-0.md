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

### 2. The fix: `store::guarded`

One function, `firmware/src/store.rs`:

```rust
pub fn guarded<R>(f: impl FnOnce() -> R) -> R {
    critical_section::with(|_| f())
}
```

`critical_section::with` on this chip is not a third-party abstraction: esp-hal
**is** the `critical_section` implementation, and it is one global
`esp_sync::RawMutex` shared by both cores (`esp-hal-1.2.2/src/sync.rs` lines
92-111). Holding it across the whole of a flash erase or program - which means
across `pre_write`, the ROM call and `post_write` - does two things:

1. **Core 0 runs no interrupt handler while core 1 is parked.** `rsil 5` is held
   by the outer lock and the inner `wsr.ps` in esp-storage's wrapper restores it
   to level 5, not to zero, so the window at t3 above never opens. Core 0
   therefore cannot be made to wait on anything: inside the window it executes
   only esp-storage's `#[ram]` wrapper and the ROM, whose one lock is
   esp-storage's private `LOCK`, which core 1 can never hold because **nothing
   on core 1 touches flash** - `store.rs`'s rule 1, now load-bearing rather than
   decorative. That is an enumeration of the whole window's lock demand, not an
   argument from unlikelihood.
2. **Core 1 provably is not inside a `critical_section` when it is parked**,
   because core 0 is holding it. Every `CriticalSectionRawMutex` in this
   firmware and in embassy, and every embassy-time queue operation, goes through
   that one lock, so the set of places core 1 can be frozen shrinks a long way
   further than step 1 strictly needs. That is a free narrowing, not the
   argument.

The direction that would be a new deadlock - core 0 blocking *before* the park,
on a lock core 1 holds while core 1 waits for core 0 - cannot arise: core 1 runs
one task, `display_task`, which reads the triple buffer through atomics and
waits on a signal and a timer. It takes the global lock and the scheduler lock
and gives them straight back; it never waits for core 0 while holding either.

**Applied per flash operation, not per sector.** An erase and its program stay
two separate ~40 ms and ~9 ms masked windows with core 0's interrupt backlog
drained in between, which is exactly the shape card 240 measured the radio
surviving (244 erases, `link downs +0`). One guard around a whole sector would
have doubled the longest window for no benefit.

**Reads are not guarded.** `internal_read` (`common.rs` line 149) is the one
`FlashStorage` entry point that does not go through `MultiCoreStrategy` at all:
it never parks core 1, so there is no window to protect, and masking core 0 for
the ~200 us of a 4 KB read would be pure added latency on `verify_flash`'s 248
of them. The guard is for erases and programs.

#### The six call sites - and why settings commits are covered by the same words

| where | what |
|---|---|
| `store.rs`, `Counted::erase` / `Counted::write` | every settings commit |
| `ota.rs` `stage_sector` | the staging erase and the staging program |
| `ota.rs` `write_state` | the `otadata` confirm and revert writes |
| `ota.rs` `write_selection` | the `otadata` activation, both writes |
| `spike_ota.rs` `stage_chunk` | never built; it is the file somebody reads |

The settings store was the interesting one, because `sequential-storage` is
async and a critical section cannot be held across an `.await`. It did not have
to be: `Counted` already wraps the **blocking** `NorFlashRegion` to count erases
and page writes, and `BlockingAsync` calls straight through to those methods and
never suspends inside one, so the section is entered and left inside a single
poll. Putting the guard there means the settings path and the OTA path are the
same path again, and there is no spelling of a settings write - debounced,
immediate, `SET_WIFI`, `erase_all`, `sequential-storage`'s own repair pass -
that can route around it.

The `otadata` writes are three per update rather than five hundred, but they are
the three worst moments on the device to wedge at: activating, confirming and
reverting. Each is a read-modify-erase-write inside `esp-bootloader-esp-idf`,
and each parks core 1 twice. `Ota::new` is left outside the guard because it
only reads. The two writes in `write_selection` get a guard each and not one
between them, because the gap between them is research 006's interruption 5b - a
place the device must be resettable, not a 100 ms critical section.

### 3. What the fix costs, and what it does not close

**Cost.** Nothing that was not already being paid. The ROM call masked core 0's
interrupts for ~40 ms of that window anyway; the guard adds `pre_write`,
`post_write` and a handful of cache misses - microseconds. The one genuinely new
cost is on core 1: between core 0 taking the lock and parking it, and between
un-parking it and core 0 releasing, core 1 may spin on the global lock for a few
microseconds per operation where before it would have run. It is parked for the
other ~40 ms either way.

**Certainty, honestly.** The mechanism in section 1 is *certain from the
sources*: every step is a line of code, and there is no step that needs a fact
about the silicon. That the fix closes *that* mechanism is also certain, by the
enumeration in section 2. What I cannot claim from sources is that it is the
**only** mechanism. One other hazard is inherent to a hardware stall and the fix
only narrows it rather than removing it: core 1 frozen mid cache-line-fill,
holding the SPI0/SPI1 arbiter against core 0's SPI1 erase. I could not confirm
or refute that from any source in the tree, it is not needed to explain a single
observation on this card, and the two things in the tree that document the
hazards of `park_core` - `esp-rtos`'s `sleep.rs` FIXME and research 006 section
4 - both name locks and neither names the bus. So: **one mechanism, proved; one
residual hypothesis, unproved and unneeded.** The `flash-stress` build in
section 5 is the instrument that separates them, and it separates them in a
minute: 1,500 guarded cycles with no wedge leaves the residual hypothesis with
nothing to explain.

The proper fix for the residual - a cooperative rendezvous, where core 0 raises
a software interrupt on core 1 and core 1 acknowledges from an IRAM handler with
its own interrupts masked, which is what ESP-IDF does instead of
`esp_cpu_stall` - is ~150 lines including a spare `FROM_CPU_INTR`, an IRAM ISR
and a timeout policy for "core 1 never acknowledged". **It is not what this card
should ship.** It is more machinery than the proved mechanism needs, a worker
cannot run a line of it, and a rendezvous that mis-handles its timeout turns
"the device sometimes wedges during an update" into "the device cannot write its
settings at all". If the stress build wedges anyway, that is the next card and
the evidence for it will be in hand.

One boundary worth writing down: `rsil 5` does not mask level 6 (debug) or level
7 (NMI). Nothing in this firmware registers a handler at either, and
`critical_section` itself has exactly the same boundary, so the guard is as
strong as every other critical section on the device and no stronger.

### 4. What the panel does during a sector operation

Unchanged, and that is the point - decision 7 says only a firmware update may
disturb the panel, and this card must not become one that does.

The HUB75 DMA is circular and the refresh runs from descriptors it already
holds, with no CPU (`firmware/Cargo.toml`, `circular-dma` + `iram`). Core 1
being clock-stalled stops the *rewrite* that spends the dither remainder, not
the scan: the panel keeps its picture at full refresh and full brightness for
the ~40 ms, and the dither phase freezes. During an upload dither is off
anyway (`ota.rs` `Upload::start`), so the display task is asleep and the stall
costs literally nothing; during a settings commit the freeze is one sector long
and invisible. Nothing is ever half-written to the panel, because core 1 is
frozen between instructions and the DMA is reading a buffer core 1 is not
writing. On un-park core 1 resumes at the instruction it was stopped at, its
coalesced timer and DMA interrupts fire, and the next swap publishes.

The new part is only that core 1 now also spends a few microseconds per
operation spinning for the global lock instead of running. At 154 Hz refresh
that is far below one refresh period and cannot skip a swap.

And the failure mode is gone rather than hidden: before this card the panel
stayed lit and moving on a wedge (core 1 unparked and running) or lit and frozen
(core 1 parked for ever), with no way to tell from the front which - which is
why the card could not say what the panel did. After it, the sector operation
ends, core 1 is un-parked by code that is guaranteed to run, and the panel
carries on.

### 5. The numbers

`tools/fw-size.sh`, every build the card names. The floor is 24,576.

| build | `.stack` | `.bss` | `.rwtext` (IRAM) | image |
|---|---|---|---|---|
| default | **26,200** | 110,440 | 66,932 | 1,014,469 |
| panic-test | **26,120** | 110,504 | 66,932 | 1,015,329 |
| http-selftest | **25,768** | 110,824 | 66,932 | 1,049,021 |
| start-in-portal | **26,200** | 110,440 | 66,932 | 1,014,157 |
| ota-test-unhealthy | **26,200** | 110,440 | 66,932 | 1,014,565 |
| ota-test-panic | **26,120** | 110,504 | 66,932 | 1,014,997 |
| flash-stress | **26,024** | 110,616 | 66,932 | 1,018,973 |

`spike-ota` and `store-selftest` also still build (both touched: `spike_ota.rs`
took a guard, `store-selftest` goes through `Counted`).

- **The fix is free.** The default build's `.stack`, `.bss` and `.rwtext` are
  byte-for-byte what `main` has: 26,200 / 110,440 / 66,932. `critical_section`
  was already linked in - esp-hal is its implementation - so the guard adds no
  static, no allocation and nothing to IRAM.
- **IRAM is unchanged at 66,932** on every build, including `flash-stress`.
  Nothing moved into RAM; the guard runs from `.text` and the code it protects
  was already `#[ram]` inside esp-storage.
- **`flash-stress` costs 176 bytes of `.stack`**, which is the stress task's
  future, and nothing else. The 4 KB sector buffer is on the heap, exactly as
  the upload's is, so nothing large lives across an `await`.
- **Stack chain**: on the fixed default build, `stage_sector`'s frame is **128
  bytes** and `read_back`'s is **112** (`entry a1, N`, from the disassembly).
  Card 241's Log recorded 144 for `stage_sector` on both 0.6.0 and 0.7.0, so the
  guard has not grown the chain - the closure inlines into the call site. The
  deep frames on this path are still `esp-bootloader-esp-idf`'s
  `NorFlashRegion::read`/`write` at 4,144 / 4,160, which the guard does not
  touch. Nothing on the settings chain grew either: the largest frame under
  `save_*` is `Flash::save_wifi`'s 224 bytes, and `Counted::erase`/`write` do
  not appear at all - they inline.

Workspace tests: `timeout 1200 cargo test` from the repo root, **906 passed, 0
failed, 1 ignored**. One run of it before that had
`screeny-studio --test moved::the_status_poll_follows_a_panel_that_moved` fail;
it passes alone (`cargo test -p screeny-studio --test moved`, 2 passed) and it
is the known wall-clock/port-binding flake under load. 33 of the passes are
`crates/fwimage`, one of them new (`stress_sector_stays_inside_the_slot`).

### 6. The bench procedure

Three phases, in this order, and **phase A gates the rest**: if the stress build
wedges, the fix is wrong and running uploads only re-measures it more slowly.
Total: about fifteen minutes including flashes.

Artefacts are in
`/private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/`.

#### Phase A - the stress build (three runs, ~90 s of device time)

Start the stream first, so core 1 is busy - that is what makes this the hard
case:

```bash
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":true}'
```

Then, for each of three runs (the task runs **once per boot**, so each run needs
its own boot - re-flashing is the simplest way, and a `screeny-probe reboot` or
a USB re-plug does as well):

```bash
tools/fw-run.sh \
  /private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/screeny-fw-0.7.0-flash-stress.elf \
  fw-0.7.0-stress-1 90
```

The run starts 30 s after boot and takes ~25-30 s, so 90 s of monitor covers it
with margin. Expect, in `captures/fw-0.7.0-stress-1.log`:

```
flash-stress: BENCH BUILD - 500 erase+write+read cycles into the INACTIVE slot at 0x210000 (2048 KB), one sector each, stream and dither left running
flash-stress: 100 of 500 cycles, 0 mismatches, longest masked window NNNNN us, longest cycle NNNNN us
flash-stress: 200 of 500 cycles, 0 mismatches, ...
flash-stress: 300 of 500 cycles, 0 mismatches, ...
flash-stress: 400 of 500 cycles, 0 mismatches, ...
flash-stress: 500 of 500 cycles, 0 mismatches, ...
flash-stress: done - 500 cycles, 0 mismatches, ~28000 ms wall | erase us min/mean/max ~34000/~40000/~55000 | write us ~7000/~9500/~11000 | read us mean ~200 | longest masked window ~55000 us | longest cycle ~65000 us | core 1 swaps +NNNN
```

The erase and write numbers should match card 240's upload (34/40/55 ms and
7/9.5/11 ms) - they are the same ROM calls. `core 1 swaps +NNNN` should be in
the thousands: that is the proof core 1 refreshed the panel all the way through
rather than sitting parked.

**Pass**: three runs reach `done` with `mismatches 0`. That is 1,500 flash
operations against the 248 that wedged three times out of three, and it settles
the card.

**Stop conditions.**
- *Silence after the BENCH BUILD line, then `rst:0x7 (TG0WDT_SYS_RESET)` ~20 s
  later.* The fix did not work. **Stop; do not run phase B or C.** Record the
  last progress line (it gives the cycle count the wedge happened after) and
  which run it was, and put the log in `captures/`. The next card is the
  cooperative rendezvous in section 3.
- *`mismatches` greater than 0, or any `flash-stress: ... failed` warning.* A
  flash correctness problem, which is a different bug from the wedge. Stop and
  report the offset in the warning.
- *`flash-stress: this is a trial boot` / `an upload owns the slot` / `no
  inactive app slot`.* The build refused to run, and none of those should be
  true after a serial flash. `tools/fw-run.sh` passes `--erase-data-parts ota`,
  which clears `otadata`, so a trial boot here means something else is wrong -
  check the boot classification line before re-running.

**Recovery.** A wedge recovers itself: MWDT0 resets the device in 20 s and it
comes back on the same image. If it does not come back, unplug and re-plug the
USB. Nothing the stress build does can stop the device booting - it writes only
the inactive slot and never `otadata` - so a serial flash of the default build
always gets it back.

**What phase A leaves behind.** The inactive slot is full of the stress
pattern. That is harmless: `otadata` is untouched, so nothing will ever boot it,
and phase B's first upload overwrites it. Flash the default build before phase
B either way:

```bash
tools/fw-run.sh \
  /private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/screeny-fw-0.7.0-default.elf \
  fw-0.7.0-default 30
```

#### Phase B - three stage-only uploads (~2 minutes)

The same upload that wedged three times out of three, with the activation
suppressed, three times:

```bash
cargo run --release -p screeny-probe -- --addr 192.168.7.221 \
  fw-upload /private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/screeny-fw-0.7.1-good.bin
```

(no `--activate`; the probe sends `?activate=0` by default). Expect each to
finish in ~25 s with the probe reporting `ok` and 1,014,528 bytes written, and
one line per upload on the serial log:

```
ota: upload started - staging into 0x210000 (2048 KB slot), content-length Some(1014528)
ota: upload accepted - 1014528 bytes in 248 sectors, ~25000 ms wall, ~12000 ms flash-busy (~47%) | erase us min/mean/max ... | write us ... | slowest sector ... us
```

**Pass**: three uploads accepted, `link downs +0` on the radio line each time.
**Stop condition**: a `write: Broken pipe` from the probe and serial silence is
the old wedge - stop and report, exactly as in phase A.

#### Phase C - card 241's steps 1 to 3, unchanged

Only after B passes. They are in `docs/board/review/241-*.md` and this card
changes nothing about them:

1. **The good update.** `fw-upload screeny-fw-0.7.1-good.bin --activate`, after
   the player is on and a 10 s settle. Panel: "updating" ~25 s, "installing"
   ~2 s, reboot. Confirm expected at 60-120 s.
2. **The never-healthy image.** `fw-upload screeny-fw-0.7.1-unhealthy.bin
   --activate`. Expect the app-side revert at ~180 s.
3. **The panicking image.** `fw-upload screeny-fw-0.7.1-panic.bin --activate`.
   Expect one panic and the bootloader's rollback, `state Aborted` - one panic,
   not five, so the crash-loop guard is never reached.

#### The artefacts

Rebuilt from this branch, `FW_VERSION` unchanged at `0.7.0` for the device build
(nothing a client can see changed, so card step 5 is met). All three `.bin`s
pass `screeny-probe fw-scan`.

| file | `esp_app_desc.version` | bytes |
|---|---|---|
| `screeny-fw-0.7.0-default.elf` | `0.7.0` - the build to serial-flash | - |
| `screeny-fw-0.7.0-flash-stress.elf` | `0.7.0` - phase A only, never shipped | - |
| `screeny-fw-0.7.1-good.bin` / `.elf` | `0.7.1` | 1,014,528 |
| `screeny-fw-0.7.1-unhealthy.bin` / `.elf` | `0.7.2-unhealthy` | 1,014,416 |
| `screeny-fw-0.7.1-panic.bin` / `.elf` | `0.7.3-panic` | 1,015,056 |

Built with card 240's command, which is not optional in any of its parts:

```bash
espflash save-image --chip esp32 --flash-size 8mb \
  --partition-table firmware/partitions.csv \
  firmware/target/xtensa-esp32-none-elf/release/screeny-fw <out>.bin
```

### 7. What I am not sure about

- **Whether the residual hardware hypothesis exists at all** (section 3): core 1
  frozen mid cache-line-fill holding the flash arbiter. Unprovable from sources
  either way, unneeded to explain anything on this card, and phase A settles it
  empirically in ninety seconds.
- **The exact per-sector probability.** "Order 0.5%" is arithmetic from three
  observed wedges and one clean run, not a measurement of core 1's critical
  section duty cycle. It is the right order - it predicts the observed spread -
  but do not quote it as a number.
- **Nothing here has run on hardware.** This worker touched no serial port, no
  LAN and no camera. Every number in section 5 is from `fw-size.sh` and the
  disassembly; every number in section 6 is a prediction, marked with `~`.
- **The stack-chain baseline.** `stage_sector` measures 128 bytes now and card
  241's Log records 144 for the same function on `main`. I could not rebuild
  `main` in this worktree to diff it directly, so that comparison is against a
  written record rather than a build I made. It is the safe direction either
  way.
- **`spike_ota.rs` was guarded but is never compiled into anything flashed**, so
  that change is style rather than evidence.

**Orchestrator, after the merge (2026-09-20): merged as 457ea5b. The mechanism and the fix are confirmed on the device.**
- Phase A, `flash-stress` built from `main`, stream running, three boots: **500 cycles, 0
  mismatches** each - 24,858 / 25,068 / 25,246 ms wall, erase min/mean/max ~33.2/39.7/54.5 ms,
  write ~9.1/9.3/11.2 ms, longest masked window 54.5 ms, `core 1 swaps +227..232`. 1,500
  guarded operations, no wedge; before the fix 248 sectors wedged 3 runs of 3 (and fw 0.6.0
  on its first retry).
- Phase B, default 0.7.0, three stage-only uploads: `HTTP 200` in 26.4 / 26.1 / 26.1 s,
  `248 sectors`, `link downs +0` each, no reset.
- Then card 241's steps ran to the end on this build (its Log has them).
The residual hardware hypothesis (core 1 frozen mid cache-fill) did not show in ~2,700
flash operations today; left.
