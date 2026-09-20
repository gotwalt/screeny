---
id: 243
title: Firmware 0.5.2 - a panic reboots and leaves a breadcrumb; the boot path gives 3 KB of stack back
type: build
hardware: orchestrator flashes (the worker builds and host-tests only)
depends: [234]
owner: worker-243
branch: card/243-panic-breadcrumb
---

## Goal

The owner's bar (decision 10 in `docs/design/device-web.md`): "as long as it's not running
out of memory and is pretty crash proof I'm happy". Two halves, one firmware build, 0.5.2:

1. **Crash-proof**: today a panic on core 0 ends in `interrupt_free(|| loop {})` - core 0
   spins forever, core 1 keeps the panel lit, nothing reboots, and the backtrace goes to a
   UART nobody reads (card 234's Log, candidate 2; that is very likely what the one silent
   stall of 0.5.1 was). After this card a panic **records a breadcrumb in RTC memory and
   resets the chip**, the device rejoins by itself in ~15 s, and the next boot's log and
   the status API say that a panic happened, when, and where.
2. **Memory**: the boot path's two 3 KB partition-table buffers set core 0's 13 KB stack
   demand; share one. And the `http-selftest` bench build must fit the `fw-size.sh` floor
   again.

## Context (read first)

- `docs/board/done/234-silent-stall-on-0.5.1.md` Log: "Instrument proposed" (the design
  to build: `esp-backtrace`'s `custom-halt` feature, a
  `#[esp_hal::ram(rtc_fast, persistent)]` array, `software_reset()`; cost 0 `.bss`) and the
  lock map.
- `docs/research/006-flash-store-ota.md` section G (the breadcrumb as planned for OTA) -
  card 241's revert logic will read the same breadcrumb, so shape it for that too: a
  magic/version word, a **boot counter**, the **reset reason** of the previous boot, uptime
  at the panic, the panic's `file:line` hashed or truncated into words, and a **consecutive
  panic count** (a crash loop must be recognisable: OTA's health check needs it).
- `docs/design/device-web.md`: "How to think about storage and RAM" - `.bss` comes out of
  core 0's stack; `tools/fw-size.sh <elf>` must show `.stack` >= 24,576 (27,376 on `main`).
- `firmware/src/main.rs` `read_fw_health` and `firmware/src/store.rs` `store::init`: each
  puts a ~3 KB partition-table buffer on main's stack.

## Steps

1. Panic path: custom halt -> write the breadcrumb -> reset. It must be safe from any
   context (interrupts off, either core, no allocation, no locks, no `await`, no logging
   after the backtrace has printed). Keep `esp-backtrace` printing the backtrace first.
   A panic on **core 1** (the display core) must take the same path.
2. Crash-loop guard: if the breadcrumb shows N consecutive panics within a short uptime
   (propose N and the window; e.g. 5 panics each under 60 s), stop resetting and halt with
   the panel showing a plain "crashed" screen - a device that reboots forever on USB power
   is worse than one that says so. Say in the Log what you chose and why.
3. Boot: log one line from the breadcrumb (previous reset reason, boot count, and the
   panic record if there is one). Add the same facts to `GET /api/v1/status`
   (`crates/device-api` owns the shape: add the fields there, with golden-file tests, and
   keep the simulator serving them - null/zero on the sim is fine) and one line on the
   status page. Tell the orchestrator in your report exactly which shared files changed.
4. A bench-only way to prove it: a cargo feature `panic-test` (off by default) that panics
   on core 0 some seconds after boot **once** (use the breadcrumb to not do it again), so
   the orchestrator can watch panic -> reset -> rejoin -> breadcrumb reported.
5. Stack lever: one shared partition-table buffer for `read_fw_health` and `store::init`
   (or read only what each needs). Report `stack: core 0 high-water` before/after from
   the code's own arithmetic; the orchestrator measures it on the device.
6. `http-selftest`: make its buffers the workers' (5.3 KB of `.bss` today) so
   `cargo build --release --features http-selftest` passes `tools/fw-size.sh`.
7. Ride-alongs (tiny, from cards 225 and 234): `provision::step` must not log inside the
   `MACHINE` critical section (`firmware/src/provision.rs` ~648); the setup form's
   `maxlength=63` -> 64 for the PSK; stale "spec section 8.3"/"6.9" citations for the
   `GET_WIFI` byte and `REBOOT` magic (`crates/device-api/src/enums.rs:65`,
   `crates/device-api/src/request.rs:135`, `crates/sim/src/wifi.rs:173,271`,
   `firmware/src/main.rs:500`, `firmware/src/provision.rs:180`) -> section 6.3;
   `firmware/src/http.rs:53` no longer says the scan is "also 223" (card 229 is dropped);
   one clause in `docs/design/protocol-v1.md` 7.3 saying frames under the setup-screen
   overlay are counted but not shown (8.1 already says it).
8. `FW_VERSION` -> "0.5.2".

## Exit

- `. ~/export-esp.sh && cd firmware && cargo build --release` clean (no new warnings), and
  the same with `--features panic-test`, `--features http-selftest`,
  `--features start-in-portal`; `tools/fw-size.sh` on each: `.stack` >= 24,576, numbers in
  the Log. **RTC memory, not `.bss`**, for the breadcrumb.
- `timeout 1200 cargo test` (workspace) green.
- The Log says exactly what the orchestrator should see on the bench for the `panic-test`
  build (the serial lines, in order) and for the default build.

## Rules

Branch `card/243-panic-breadcrumb` in your own worktree; commit and Log as you go, by
explicit path; do not merge, do not push. **You do not flash**: no serial port, no LAN, no
camera - the orchestrator flashes your ELFs and reports back. Bounded commands only; never
write a real SSID or password anywhere (dummies `Example-Wifi1` / `password9`; do not read
`~/.config/screeny/wifi.env`; never quote `captures/` lines containing an SSID). Leave
`crates/art` and `crates/studio` alone. `crates/device-api` and `crates/sim` are shared
with the `software` session: additive changes only, listed in your report.

## Log

### 2026-09-20, worker-243, steps 1-2: the panic path and the crash-loop guard

`firmware/src/panic.rs` (new, ~430 lines with the reasoning) is now this crate's
`#[panic_handler]`. A panic records a breadcrumb in RTC memory, prints, and resets.

**Four decisions that differ from the card's sketch, each for a reason found in the
sources:**

1. **RTC *slow*, not RTC fast.** Card 234's instrument says
   `#[esp_hal::ram(rtc_fast, persistent)]`. That is wrong for this device: esp-hal's own
   `ld/esp32/memory.x` lines 59-64 say RTC fast is "Only for core 0 (PRO_CPU)" and RTC
   slow (8 KB at `0x5000_0000`) is not, and the card requires that **a panic on core 1
   takes the same path**. The section is `.rtc_slow.persistent`, `52` bytes, confirmed
   with `xtensa-esp32-elf-size -A`: `.rtc_slow.persistent 52 @ 0x50000000`. **Zero bytes
   of `.bss`**, which is what the card asked for.
2. **This file owns `#[panic_handler]`; `esp-backtrace` keeps `println` and loses
   `panic-handler`.** `custom_halt()` takes no arguments (`esp-backtrace-0.20.0/src/lib.rs`
   lines 200-210), so by the time it runs the `&PanicInfo` - and with it the `file:line`
   step 3 has to report - is gone. The backtrace is still esp-backtrace's:
   `Backtrace::capture()` is public and not behind the `panic-handler` feature, and the
   handler prints the same banner, the same `PanicInfo` line and the same `0x4...` frame
   list, so `espflash monitor` symbolises it exactly as before. What owning the handler
   buys: the location, the ordering below, and a nesting guard.
3. **The breadcrumb is written before anything is printed.** `esp-println`'s
   `critical-section` feature is on by default (`esp-println-0.18.0/Cargo.toml:105`) and
   `esp_sync::RawMutex` panics `"lock is not reentrant"` (`esp-sync-0.3.0/src/lib.rs:449`).
   So printing is the step that can deadlock (the other core holds the lock and is itself
   wedged) or double-panic (we panicked inside a `println!`). The record is in RTC memory
   before either can happen. A nested panic - same core re-entering, or the other core
   arriving - prints **nothing at all** and goes straight to the reset.
4. **`software_reset()`, and what it does to the other core and to a flash write.**
   `esp_hal::system::software_reset` -> `esp_rom_sys::rom::software_reset` -> the ROM's
   `SW_SYS_RST`: it resets **both** CPUs and the digital peripherals together, and leaves
   the RTC domain (and therefore the breadcrumb) alone. That is the property this needs:
   unlike `software_reset_cpu`, there is no window in which core 1 goes on driving the
   panel against a rebooting core 0, and unlike today's `interrupt_free(|| loop {})` there
   is no window at all. It does **not** reset the flash chip, and that is the hazard: a
   panic can land inside `esp-storage`'s 4 KB sector erase, which the chip finishes by
   itself (~50 ms, research 006 section 4) while the ROM bootloader's first act after the
   reset is to read that same chip. Hence `SETTLE_MS` = **120 ms** before the reset: 11 ms
   of it is the UART FIFO draining at 115200 (the ESP32 printer is the ROM's
   `uart_tx_one_char`, which waits for FIFO *space*, not for the line), and the rest
   covers a worst-case erase. The wait is counted in **CPU cycles**
   (`xtensa_lx::timer::delay`), not `Instant`s: `CCOUNT` runs from reset, so a panic that
   arrives before `esp_hal::init` has configured TIMG0's LACT counter still gets a bounded
   wait rather than a spin on a clock that never moves. Nothing in the path allocates,
   awaits, or takes a lock of ours.

**The crash-loop guard: N = 5 panics, each at an uptime below 60 s.** Five because a
device with nobody in the room must be allowed to survive a transient, and each attempt
costs one boot: five quick cycles is at most ~2 minutes of flapping. Sixty seconds because
that is already the "too early to count as healthy" threshold research 006 section 6 gives
the OTA health criterion, and the two questions should not disagree about what an early
death is. A panic later than 60 s resets the run to one, so a device that panics once a
day never reaches the guard and no separate "mark healthy" tick is needed.

What the guard does, in two stages, because a panic handler cannot safely paint a panel:
the 5th quick panic **latches** a flag and still resets, and the next boot reads the flag,
draws `screens::crashed` (CRASHED / the file / `line N xM` / "power cycle") and stops
before the radio - no HTTP, no mDNS, no stream. Only a panic that finds the flag *already*
latched - i.e. the crashed screen's own boot could not be reached - ends the way every
panic ended before 0.5.2, halting with interrupts off, having said so on the log. The
recovery is a power cycle, which is what clears the region: esp-hal zero-fills
`.rtc_slow.persistent` only when the reset reason is `ChipPowerOn` or unknown
(`esp-hal-1.2.2/src/soc/mod.rs:106-110`), and on this chip the external reset pin reads as
a power-on too.

Breadcrumb layout, 13 words, checksummed (XOR + salt, written last so a reset in the
middle of an update reads back as invalid rather than as half a record): magic+version,
boots, panics, consecutive-quick-panics, panic uptime, panic line, 12 bytes of the
panicking file's **base name** (the base name and not the path because the string goes
into JSON and picoserve escapes `/` where the other two writers do not - the API's golden
files are the form all three agree on), the boot number that panicked, flags, and the
previous boot's reset reason as a code of this module's own (not `SocResetReason as u32`:
the breadcrumb outlives a firmware update, and a hardware discriminant from a pinned crate
is not a number to write into a region card 241 will read across one).

`tools/fw-size.sh`, default build: `.stack` **27,376 -> 27,096** (`.data` +224, `.bss`
+56 for the one `AtomicU32` nesting guard; the breadcrumb itself costs nothing here).
`--features panic-test`: 27,032. Floor 24,576.

### Step 3: the boot line, the API shape, the page

**The boot line**, from `panic::boot()`, before anything that can fail, so that a panic in
the boot path is counted like any other:

```
boot: #2 since power-on, reset reason software (the boot before it started with power_on)
boot: last panic was boot #1 at uptime 20123 ms, main.rs:1034, 1 in a row (1 panic(s) since power-on)
```

**`crates/device-api` (shared, additive).** `StatusReply` gains three fields at the end -
`boot_count`, `panic_count` and `last_panic: Option<PanicRecord>` - and `PanicRecord` is
`{uptime_ms, boot, file, line, consecutive}`. `file` is a `String<12>`: the base name, not
the path, because `tests/common/mod.rs` refuses a golden containing `/` (picoserve escapes
it as `\/` and the other two writers do not), and twelve bytes is what three words of a
breadcrumb hold. `"provision.rs"` is exactly twelve.

**All three are `#[serde(default)]`, and that is load-bearing rather than tidy.** The
first `cargo test` after adding them failed in `crates/studio/tests/device_status.rs`:
its canned reply is a firmware-**0.4.2**-shaped body, and serde refused it with
`missing field 'boot_count'`. That is not a test being fussy - it is the real
compatibility case, because a Studio built from this commit still has to read the panel
running 0.5.1 while it is being flashed. Spec §8.6 says "a new optional field does not
bump `api`", which is only true if the reader tolerates the field's absence. With the
`default`s the Studio test passes **unchanged and `crates/studio` is not touched by this
card at all**.

Golden files: `status.json` and `status_portal.json` gain the three fields, and
`status_panicked.json` is new - the same device having panicked once and rebooted by
itself, which is the shape the Studio will read to tell "it restarted for a reason" from
"somebody pulled the plug". `tests/sizes.rs`: `StatusReply::MAX_JSON_LEN` 1018 -> **1142**,
a real reply 364 -> **434** bytes, and one carrying a panic record **520**; the sanity
check now pins both.

**`crates/sim` (shared, additive):** `boot_count: 1, panic_count: 0, last_panic: None`, with
a comment saying why that is the honest answer for a process whose panics the operating
system reports. Nothing in the simulator reads them back.

**The page** gets one row, `panic`, server-rendered and repainted by the script:
`none in 3 boot(s) since power-on`, or
`net.rs:321 at 94 s, boot 1 of 2 (1 in a row, 1 total)`. The generic paint loop skips
`last_panic` (it is an object, and `typeof null` is `"object"` too), so the row has its own
line in the script.

`.stack` **26,392** after this step (the `StatusReply` grew, and it is held in a handler
future).

### Step 5: the stack lever - one partition-table read for the whole boot

The card's premise needed checking before it could be acted on, and it is **nearly** right.
The two 3 KB buffers are not alive at the same time - `store::find_partition` returned
before `http::read_fw_health` was called - so "share one buffer" on its own would have
saved nothing, because the peak is the max of the two chains and not their sum. What is
real is *where* the second buffer sat: `read_fw_health` declared
`buf: [u8; PARTITION_TABLE_MAX_LEN]` (3,072 bytes) and then, with the table still
borrowing it, called `Ota::new` and `current_ota_state` - i.e. **esp-storage's read path
ran on top of it**, and research 010 already blames esp-storage's ~4 KB frames for the
deepest chain this firmware has. So the lever the card is really asking for is its own
parenthesis: *read only what each needs*.

`store::read_partitions` (renamed from `find_partition`) now reads the table **once**, in
one non-`async` `#[inline(never)]` frame, and keeps three things: the `screeny` entry, the
`otadata` entry and the booted partition's offset - 68 bytes of `Copy` `store::Parts`.
`read_fw_health` takes them from beside the flash handle (it already holds the `STORE`
lock) and does two `otadata` reads with **nothing large underneath**.

**The arithmetic the card asks for, from the code:** the boot path's deepest chain loses
exactly `PARTITION_TABLE_MAX_LEN` = **3,072 bytes** from under `Ota::new` /
`current_ota_state` / esp-storage. Card 227 measured the boot path's high-water at
**13,056** bytes and identified it as the boot path's own; the arithmetic therefore
predicts **~9,984** on the device. That is a prediction, not a measurement: it assumes the
`otadata` chain is the deepest of the boot path's three (store load, table read, ota read)
and the orchestrator's `stack: core 0 main high-water ...` line at the 60 s mark is what
settles it. If the number does not move, the store's own `load` chain was the peak all
along and the next lever is there.

`Parts` lives in `store::Flash`, behind the `STORE` mutex, and not as a local in `main`:
a local would be held across `main`'s first `await` and so be `.bss` anyway - and
everything that wants a partition has to take that lock regardless. Everything is looked
up **by label**, never `partition_type()`, which `unwrap!`s its conversion and would panic
on a subtype this crate's enums do not know (research 006 section 3, the rule the settings
partition has always followed; `read_fw_health` used to break it by walking the table with
`find_partition(PartitionType::Data(Ota))`).

`.stack` **26,392 -> 26,240**: the *ceiling* went down by 152 (`Parts` is 68 bytes of
`.bss` inside the store handle, plus padding and 80 bytes of `.data`) while the *demand*
goes down by ~3 KB. `fw-size.sh` measures the ceiling; the device measures the demand.
That is the trade, and it is the right way round on a chip where the ceiling has 1.7 KB of
margin and the demand has 11.
