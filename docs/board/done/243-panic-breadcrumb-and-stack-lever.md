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

### Step 6: `http-selftest` fits the floor again, by not owning anything

Measured first: with steps 1-5 on the branch, `--features http-selftest` had `.stack`
**20,552** against the 24,576 floor - 4,024 bytes short, and `.bss` 5,640 bytes above the
default build. That 5,640 is what `selftest_task` cost as a task: its own `rx` (256), `tx`
(192), reply buffer (256) and picoserve request buffer (768), plus - much the larger half -
a second copy of picoserve's whole `serve` future, held across an `await` in a task future,
which is `.bss`, which is core 0's stack.

The card's instruction is "make its buffers the workers'", and it turns out to be worth
more than the byte arrays suggest. `selftest` is now an ordinary `async fn` called from
**HTTP worker 0**, which serves the LAN for the 75 s the self-test wants to wait and then
runs it with `&mut http_buf`, `&mut rx`, `&mut tx` - the worker's own, free because that
worker is not listening while it runs. The TCP half reads into `http_buf`; the in-memory
half hands `http_buf` to picoserve and collects the reply in `rx`. The big win is the one
the byte count does not show: the self-test's `Server::serve` future and the worker's
`listen_and_serve` future are two states of the *same* generator and never live at once,
so the compiler overlaps them.

`.stack` **20,552 -> 25,920**, `.bss` for the feature **5,640 -> 272 bytes**. The device
under test differs from the shipping build in one visible way while the self-test runs:
worker 0 is busy, so the server is worker 1 alone for a couple of seconds - which is the
one-worker configuration card 222 shipped and card 227 measured, and the `fallback
evidence` line now says `HTTP_TASKS - 1` rather than claiming both.

### Steps 7-8: the ride-alongs, and 0.5.2

All seven done, each named in the commit. Two are worth a line here rather than just a
diff:

* **`provision::step` logs outside the `MACHINE` critical section** now: the transition is
  noted into a local inside the lock and the `info!` runs after it. The file's own rules
  (its lines 31-35) forbid formatting while that lock is held, because a critical section
  masks the interrupt core 1's HUB75 DMA runs on, and this was the one place it
  contradicted itself.
* **The PSK input's `maxlength` 63 -> 64**, on both forms. Spec 8.2 types `psk_len` as
  `0..=64` and `screeny_proto::control::MAX_PSK_LEN` is 64 - a WPA2 PSK written as 64 hex
  characters is the case, and a `maxlength` of 63 ate the last one *silently*, in a
  captive mini-browser where there is no other way to find out.

The spec edits are the one clause allowed in 7.3 (a frame under the setup-screen overlay
is admitted, decoded and counted, and not shown - so `frames_rx` moving while
`frames_shown` does not is the expected reading in `PROVISIONING`) and the new status
fields in 8.6, including the sentence that makes them safe: **a reader MUST treat an
absent one as `0` / `null`, because firmware older than 0.5.2 does not send them.**

Firmware clippy is back to the **same 9 warnings** card 234 recorded (plus the 2
pre-existing build-script ones): the three this card added were two `doc list item without
indentation` in my own new doc comments and `empty loop {}` in `halt()`, which now carries
an `#[allow]` and the one-line reason (`esp-backtrace`'s own `abort()` spells it the same
way).

### The four builds, and `cargo test`

| build | `.stack` | `.bss` | image |
|---|---|---|---|
| default | **26,272** | 111,056 | 976,125 |
| `--features panic-test` | **26,208** | 111,120 | 977,001 |
| `--features http-selftest` | **25,968** | 111,328 | 1,009,665 |
| `--features start-in-portal` | **26,272** | 111,056 | 976,117 |

Floor 24,576; `main` was 27,376 with `http-selftest` failing at 22,056. The default build
spends 1,104 bytes of ceiling on the whole card (the breadcrumb itself spends none - it is
52 bytes of `.rtc_slow.persistent` at `0x5000_0000`), and the demand it removes from the
boot path is ~3 KB.

`timeout 1200 cargo test` (workspace): **green** - 84 `test result: ok` lines, **752
passed, 0 failed, 1 ignored**, no flakes in three runs of the affected crates.
`cargo clippy --workspace --all-targets`: silent.

### What the orchestrator should see on the bench

**Flashing re-arms everything by itself.** `espflash` resets the chip with the EN pin,
which this SoC reports as `ChipPowerOn`, and esp-hal zero-fills `.rtc_slow.persistent` on
exactly that reason - so every flash starts from boot #1 with an empty breadcrumb. A
`POST /api/v1/reboot` does **not** (it is a software reset, which is the whole point), so
the counters keep climbing across one.

#### The `panic-test` build, in order

Flash `screeny-fw-0.5.2-panic-test.elf` with the monitor attached and logging, capped.

1. The ROM banner, the bootloader, then **the first two lines this firmware prints**:
   ```
   INFO - boot: #1 since power-on, reset reason power_on (the boot before it started with -)
   INFO - boot: no panic on record
   ```
2. The ordinary boot: `store: 'screeny' partition at 0x410000 ...`, `store: loaded schema
   ...`, `http: fw slot Ota0 state Valid`, `display: core 1, ...`, `device: mac ... id
   4a00a4`, and once the tasks are up:
   ```
   WARN - panic-test: BENCH BUILD - panicking on core 0 in 20 s, once (card 243)
   ```
   Then the join, `net: http worker 0/1 listening on tcp/80 (lan)`, and the 5 s
   `telemetry:` lines.
3. At **~20 s of uptime**, the panic. Blank lines are real (they are esp-backtrace's
   shape, kept):
   ```

   ====================== PANIC ======================
   panicked at src/panic.rs:600:5:
   panic-test: deliberate panic on core 0 (card 243)

   Backtrace:

   0x400d....
   0x400d....            (several frames)

   panic: resetting the chip (card 243; the breadcrumb is in RTC memory).
   ```
   `src/panic.rs:600` is **the `panic!()` in the test task**, not the handler failing:
   that is where the deliberate panic is written (line 600 as the file stands at this
   commit; read it as "the line `panic-test: deliberate panic` is on"). The reset follows
   ~120 ms later.
4. The ROM banner again, and then **the evidence**:
   ```
   INFO - boot: #2 since power-on, reset reason software (the boot before it started with power_on)
   WARN - boot: last panic was boot #1 at uptime 20xxx ms, panic.rs:600, 1 in a row (1 panic(s) since power-on)
   INFO - panic-test: already fired since power-on; this boot runs normally
   ```
   The device then joins as usual, ~15 s from the reset, and does **not** panic again.
5. Over HTTP, once it is back: `curl -s http://192.168.7.221/api/v1/status` must carry
   ```json
   "reset_reason":"software","boot_count":2,"panic_count":1,
   "last_panic":{"uptime_ms":20xxx,"boot":1,"file":"panic.rs","line":600,"consecutive":1}
   ```
   and `http://192.168.7.221/` must show one row: `panic  panic.rs:600 at 20 s, boot 1 of
   2 (1 in a row, 1 total)`.
6. **If step 4 says `boot: #1 ... power_on` and `no panic on record`**, the breadcrumb did
   not survive the reset and that is the one thing this card cannot test without hardware.
   The suspect is the custom ESP-IDF bootloader (card 242) clearing or reserving RTC slow
   memory; the fallbacks, in order, are to move the region to the *top* of RTC slow, or to
   `rtc_fast` - which works but gives up a panic on core 1, since RTC fast is PRO-CPU only
   on this chip. Everything else in the card stands either way.

#### The default build

Flash `screeny-fw-0.5.2-default.elf`. It should look exactly like 0.5.1 plus two lines at
the top of the log:

```
INFO - boot: #1 since power-on, reset reason power_on (the boot before it started with -)
INFO - boot: no panic on record
```

and `fw` reading `0.5.2` in `GET_INFO`, the mDNS TXT and the page. Nothing else about the
device's behaviour changes while it does not panic. `GET /api/v1/status` carries
`"boot_count":1,"panic_count":0,"last_panic":null`, and the page's new row reads
`none in 1 boot(s) since power-on`.

Then card 234's bench procedure, once, on this build - it is the run that was waiting for
this firmware, and it now has both the bounded send and the breadcrumb. Two extra things
to read the log for:

* `stack: core 0 main high-water N of 26272 bytes` at the 60 s mark. **The number to
  compare is card 227's 13,056**; step 5 predicts ~9,984. Whatever it says is the card's
  measurement of the lever.
* a `boot: #N` line with N > 1 during a soak means the device rebooted by itself, and the
  line after it says why. That is the whole point of the card: 0.5.1 could not have told
  you.

**A device that has stopped with `CRASHED` on the panel** (five quick panics in a row) is
recovered by a **power cycle** - unplug and replug, or the EN button. A software reboot
does not clear the latch, and there is no HTTP on a device in that state.

### 2026-09-20, worker-243, the bench regression: +4,832 bytes of core 0's stack

The orchestrator's 0.5.2 bench run found the panic path working exactly as predicted and
**one regression**: after `screeny-probe http` the core 0 high-water read **17,440 of
26,272** (`stack_free` 7,808), against **14,240 of 27,520** (13,280 free) on 0.5.1. The
`stack:` line landed right after a settings commit, so the first suspicion was the store -
the `Parts`/`read_partitions` change of step 5. **It was not the store.**

#### How it was found, without hardware

`xtensa-esp32-elf-objdump -d` gives every function's frame size (`entry a1, N`, plus any
`addi a1, a1, -N`), so a build is a table of frames. I built **0.5.1 from
`git archive 3186cd2`** into a scratch tree and diffed the two tables. Every Rust frame
over 700 bytes, 0.5.1 -> 0.5.2:

| frame | 0.5.1 | 0.5.2 | delta |
|---|---|---|---|
| `Select<serve_on, wait_ap_pub>::poll` (the HTTP worker) | 4,608 | 5,312 | +704 |
| picoserve `handle_request` poll | 1,728 | 4,384 | **+2,656** |
| `http::route_request` | 1,552 | 1,680 | +128 |
| **the HTTP serve chain** | **7,888** | **11,376** | **+3,488** |
| `main`'s future poll | 5,104 | 3,056 | -2,048 |
| the partition-table read | 3,200 | 3,360 | +160 |

+3,488 on the HTTP path against a measured +3,200 in the device's peak: that is the
regression, and the store is innocent. The commit line in the log is a coincidence of
timing - `screeny-probe http` posts its settings **over HTTP**, and the 4 Hz stack watcher
printed on the next tick. Step 5 did what it claimed: the boot path's own peak fell
(13,056 -> 12,608 on the device) and `main`'s frame lost the 3 KB buffer.

#### What cost 3,488 bytes: 44 bytes of `StatusReply`

Six ablation builds, each one measurement:

| build | `StatusReply` grows by | serve chain |
|---|---|---|
| 0.5.1 | - | 7,888 |
| 0.5.2 as merged (`Option<PanicRecord>` + 2 counters) | +44 | 11,376 |
| the same, page row simplified to one `{}` | +44 | 11,376 (**the page row costs nothing**) |
| flat fields instead of the nested record | +44 | 11,360 (**the nesting costs nothing**) |
| `Option<PanicFileText>` + 2 counters | +28 | 10,832 |
| `Option<u32>` + 2 counters | +16 | 8,304 |
| the 2 counters alone | +8 | 8,080 |

So it is the **size** of `StatusReply`, and the multiplier is enormous and non-linear:
`Reply` is an enum whose `Page` and `Api` variants both carry one, `Reply::write_to` is a
single `async fn` whose three arms are three inlined response chains, and rustc lays a
future's states side by side rather than overlapping them. Every byte added to this reply
is paid for a dozen times over, in one frame.

**One thing that did not work, recorded so nobody tries it twice:** collapsing the three
`write_to` arms into one `Content` (the natural continuation of card 233) made it
**worse** - chain 13,648, `.stack` 25,744 - because the merged `match` in `write_content`
put both formatting chains in one frame. picoserve's JSON serializer is private, so the
JSON arm could not have joined them anyway.

#### The fix

**`StatusReply` goes back to its 0.5.1 shape, byte for byte**, and the breadcrumb gets a
route of its own: `GET /api/v1/panic` -> `PanicReply { boot_count, panic_count,
last_panic }`. It costs nothing, because `ApiBody`'s size is its largest variant and that
is still `Status`. The right argument for it is not the bytes, though - it is that **the
breadcrumb cannot change while the device runs** (a panic reboots it), so it does not
belong on the one route that is polled every four seconds. `boot_id` already tells a
reader that the device restarted; `/api/v1/panic` says why, once.

Three more recoveries of what the card had spent, found by diffing the `.data`/`.bss`
symbol tables the same way:

* `http_task::POOL` +128 -> back: the two worker futures shrink with the reply.
* `__embassy_main::POOL` +128 -> `panic::boot()` returns a `bool` instead of a `Report`;
  `main` is a task, so what it holds across an `await` is `.bss`, which is core 0's stack.
  The crash-loop branch reads the breadcrumb again where it uses it.
* `store::STORE` +76 -> `Parts` no longer keeps the `screeny` entry, which `Flash::entry`
  already is; `read_partitions` returns it separately to its one boot-time caller.

The status **page** keeps its `panic` row in full, rendered from `panic::report()` inside
`Page::fmt`. That is safe here and nowhere else on that page: picoserve formats a body
twice (once to measure it, once to send it) and the two passes must produce the same
bytes, and the breadcrumb is the only thing on the page that cannot change between them.
It costs `Reply` nothing.

#### Where it lands

| | 0.5.1 | 0.5.2 as merged | 0.5.2b |
|---|---|---|---|
| HTTP serve chain (static) | 7,888 | 11,376 | **7,872** |
| `.stack` | 27,376 | 26,272 | **27,088** |
| device high-water after the HTTP suite | 14,240 | 17,440 | **~14,200 (predicted)** |

The chain is **16 bytes below 0.5.1**. The remaining 288 bytes of ceiling are the panic
handler's own statics (`IN_PANIC`, the `otadata` entry, section padding) - the breadcrumb
array itself is 52 bytes of `.rtc_slow.persistent` and costs main DRAM nothing - so the
predicted `stack_free` is **~12,900 of 27,088**, against 13,280 on 0.5.1 and 4.7 KB above
the Studio's warning line. If the orchestrator wants the last 400 bytes, the lever I would
pull - and did not, because it changes a tuned network parameter I cannot test - is
`TCP_TX` 1024 -> 768 in `firmware/src/http.rs`: 512 bytes of ceiling, at the price of a
few more TCP segments per page load.

All four builds: default **27,088**, `panic-test` **27,024**, `http-selftest` **26,784**,
`start-in-portal` **27,088** (floor 24,576). `timeout 1200 cargo test`: **758 passed, 0
failed, 1 ignored**. `cargo clippy --workspace --all-targets`: silent. Firmware clippy: the
same 9 warnings as `main`.

#### Also in this branch

* **The garbled first `boot:` line.** The ROM logs at 74880 baud and the second-stage
  bootloader at 115200, so when `main` starts there are still bytes in flight at a
  different bit rate and the monitor is midway through a character - which is why the
  *first* line is mangled and the second is clean. `panic::boot()` now waits 20 ms (three
  characters at the slower rate) and prints a blank line before the first word this
  firmware says. It costs each boot 20 ms, once.
* `crates/probe` gains rule **39**, `GET /api/v1/panic`: it parses, and a device reporting
  no panic reports `panic_count` 0. It is numbered last rather than inserted in sequence
  because `firmware/src/http.rs` and spec 8.7 cite rules 27, 34 and 37 by number.
* Spec 8.6 gains the `panic` route and a sentence that is the real lesson: **nothing else
  goes in `status`** - it is the polled route, and 44 bytes in it cost 3,488 bytes of
  core 0's stack.

**What the orchestrator should see on the bench.** `stack: core 0 main high-water` around
**14,200 of 27,088** after `screeny-probe http` (0.5.1: 14,240 of 27,520), and no jump at
the settings commit. `curl http://192.168.7.221/api/v1/panic` answers
`{"boot_count":N,"panic_count":0,"last_panic":null}` on a device that has not panicked and
the full record on one that has; `GET /api/v1/status` no longer carries those fields. The
`panic-test` build's evidence is unchanged except for where the record is read: the serial
lines are the same, and the HTTP check moves from `/api/v1/status` to `/api/v1/panic`.

**Orchestrator, after the merges (2026-09-20): b2060b3 (243) and 2c276c0 (243b). fw 0.5.2 is on the device.**
- `panic-test` build, on the device: exactly the Log's prediction. Panic at 20,101 ms ->
  banner, `panicked at src/panic.rs:600`, backtrace -> `rst:0x3 (SW_RESET)` ->
  `boot: #2 ... reset reason software` / `last panic was boot #1 at uptime 20101 ms,
  panic.rs:600, 1 in a row` -> rejoined from the store, the Studio took the panel back.
  **RTC slow memory survives the reset with the card-242 bootloader.** The status API (then)
  and `/api/v1/panic` (now) report it.
- Default 0.5.2 (first merge): HTTP 30/0/8, UDP 60/0/4 **with the bench Mac's WiFi on** -
  card 234's procedure, run once, serial attached for all 11 minutes: no stall, no WARN,
  no ERROR. But core 0's high-water went 12,608 -> 17,440 at the first HTTP settings post
  (`stack_free` 7,808; 0.5.1: 13,280). Sent back; 243b found it (StatusReply +44 bytes =
  +3.5 KB of serve-chain frame) and moved the breadcrumb to `GET /api/v1/panic`.
- Default 0.5.2 (243b, the build on the device): `.stack` 27,088; after `screeny-probe http`
  `stack_free` **12,352**; `GET /api/v1/panic` -> `{"boot_count":1,"panic_count":0,
  "last_panic":null}`. Boot-path high-water after the join 12,608 (0.5.1: 13,056).
- Not fixed: the first `boot:` serial line is still garbled after the 20 ms wait. Cosmetic;
  left (decision 10). The crashed-screen branch (five quick panics) has never run.
- **Found while checking this card, not caused by it**: `screeny-probe http` now gets 9-17
  `Connection refused` per run on *any* build, including the 0.5.2 ELF that passed 30/0/8
  an hour earlier. No error on the device; the probe's source-port sequence has one gap per
  refusal. The workers are slow to get back to `accept` after a response and how slow
  depends on the client's TCP timing. That is card 236. The UDP suite and the stream are
  unaffected.
