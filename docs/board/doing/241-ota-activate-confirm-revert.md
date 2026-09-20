---
id: 241
title: Firmware - OTA activate / confirm / revert: a staged image boots on trial, proves itself healthy, or the old one comes back
type: build
hardware: orchestrator flashes and uploads (the worker builds, host-tests and writes the bench procedure)
depends: [240, 242, 243]
owner: worker-241
branch: card/241-ota-activate
---

## Goal

Card 240 put a validated image in the inactive slot and deliberately stopped there. This
card is the rest of **the full plan of `docs/research/006-flash-store-ota.md`** (the owner's
choice, decision 10): after a good upload the device **activates** the staged slot and
reboots into it **on trial**; the new image must **prove itself healthy** and then
**confirms** itself; if it does not - it crashes, it never gets on the network, it wedges -
the **old image comes back by itself**, and the device says what happened. An update over
WiFi must never need a USB cable to recover from.

## Context (read first, in this order)

- `docs/research/006-flash-store-ota.md`: **Conclusions**; section 5 ("Every way it can be
  interrupted, and why each state still boots"); **section 6 (rollback: the bootloader we
  ship, the app-side half we need either way)**; section 9's cards E/F. **006 is the
  plan**; where this card and 006 differ, 006 wins and you say so in the Log.
- The health criterion the owner's orchestrator settled with 006 (check it against 006 and
  use 006's numbers if they differ): healthy = WiFi joined **and** DHCP address, **and**
  one HTTP request served **or** 120 s elapsed, **and** one display swap; **not before 60 s**
  of uptime; **revert at 180 s** if not healthy. A panic during the trial counts against it
  (card 243's breadcrumb: `consecutive`, uptime-at-panic, boot count are there for this).
- `firmware/bootloader/README.md` and card 242: the ESP-IDF v6.1 bootloader with
  `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` is on the device and is flashed by
  `tools/fw-run.sh`. Know exactly what it does with `otadata` states
  (`NEW` / `PENDING_VERIFY` / `VALID` / `INVALID` / `ABORTED`) on each kind of reset,
  **including a reset that is not a panic** (watchdog, brownout, the owner pulling USB).
- Done cards' Logs: `240-ota-staging.md` (what exists: `firmware/src/ota.rs`,
  `store::InactiveSlot`, `crates/fwimage`, the updating screen, the bench numbers),
  `243-panic-breadcrumb-and-stack-lever.md` (the panic handler resets the chip; RTC slow
  memory survives `SW_RESET` but is zeroed on power-on/EN; the crash-loop guard),
  `236-http-close-is-bounded.md`.
- `docs/design/device-web.md`: decisions 3, 7, 10, 11; the RAM section; the lessons
  (**never write flash inside an HTTP handler** - `otadata` writes included: decide where
  the activate write happens and say why it is safe; **every byte in a reply type `Reply`
  carries costs ~12x in stack**).
- `crates/device-api` (`FirmwareReply`, `StatusReply.fw_slot` / `fw_state`), `crates/sim`
  (it already models slots/states for the route - keep it in agreement), `crates/probe`.

## Steps

1. **Design note in the Log first**: the state machine across reboots (what is in
   `otadata`, what is in the RTC breadcrumb, what is in the settings store, and why each
   fact lives where it does - RTC does not survive a power cycle, `otadata` does); who
   writes `otadata` and when (after the HTTP reply is on the wire; serialised with the
   store and the panel like card 240's sector writes); every interruption point from "last
   byte staged" to "confirmed", and the state the device boots into from each. Then build.
2. Activate: after a fully verified staged image, mark it for a trial boot, answer the
   upload, show the updating screen's next state, reboot. `POST /api/v1/firmware` gains
   whatever 006 says it should about activation (a query flag, a second route, or
   activate-by-default) - the simulator and the probe follow; the spec's 8.6 is updated.
3. Confirm: the health check of the Context section, run by the image on trial, that marks
   the running slot `VALID` exactly once. It must be impossible to confirm from a state
   that is not a trial (a serial-flashed image is already valid; do not rewrite `otadata`
   on every boot).
4. Revert: at the deadline, or on a panic during the trial, or on a crash loop, the
   previous slot boots again. Decide what is the app's job and what is the bootloader's
   (with rollback enabled a `PENDING_VERIFY` image that resets is not booted again - verify
   that claim against the bootloader's source/docs, including for non-panic resets).
   The device must end up **running and reachable** in every case.
5. Report: `fw_state`/`fw_slot` in status already exist - make them tell the truth through
   the whole cycle; after a revert the device says so (one log line at boot, and a place in
   the API that does **not** grow `StatusReply`: `GET /api/v1/panic`'s reply is the natural
   home for "the last update was reverted, and why"; rename nothing).
6. Bench builds, off by default: `ota-test-unhealthy` (boots, never reports healthy - must
   be reverted at the deadline), `ota-test-panic` (panics 20 s into every boot - must be
   reverted by the panic path, without reaching the crash-loop halt), and the normal build
   with a bumped version string as the good update. Produce all three as **upload images**.
7. Probe: a bounded `fw-upload --activate`-style flow that uploads, waits for the device to
   come back, and reports the version/slot/state it finds and how long it took; rules that
   are safe on a real device stay in the default suite, anything that reboots stays behind
   `--allow-reboot`.
8. `FW_VERSION` -> "0.7.0". Spec 8.6/8.7. `docs/design/device-web.md`: how to update the
   panel over WiFi, in five lines, for the owner.

## Exit

- All firmware builds clean and over the `fw-size.sh` floor (`.stack` >= 24,576; 26,944
  now); `timeout 1200 cargo test` green (a wall-clock/port-binding test in `crates/studio`
  or `crates/screeny` failing once under load and passing alone is known - note it and move
  on); host tests for the state machine covering **every** interruption point of step 1.
- In the scratchpad
  (`/private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/`):
  `screeny-fw-0.7.0-default.elf` (to serial-flash), and the upload images
  `screeny-fw-0.7.1-good.bin`, `screeny-fw-0.7.1-unhealthy.bin`, `screeny-fw-0.7.1-panic.bin`
  (+ their ELFs for symbolising), made the way card 240's Log describes
  (`espflash save-image --chip esp32 --flash-size 8mb --partition-table firmware/partitions.csv`).
- A **bench procedure**, bounded and ordered, for the orchestrator: serial-flash 0.7.0;
  upload good -> expect reboot, trial, confirm at ~60-120 s, `fw 0.7.1`, slot `ota_1`,
  `valid`; upload unhealthy -> expect revert at ~180 s back to the previous version, and the
  report; upload panic -> expect revert via the panic path; the exact serial lines and API
  answers for each, how long each takes, the stop conditions, and recovery
  (`tools/fw-run.sh` always wins: it erases `otadata` and writes slot 0).

## Rules

Branch `card/241-ota-activate` from current `main`, in your own worktree; commit and Log as
you go, by explicit path; do not merge, do not push. **You do not touch hardware**: no
flashing, no serial port, no LAN, no camera. Bounded commands only; nothing left running.
Never write a real SSID or password (dummies `Example-Wifi1` / `password9`; never quote
`captures/` lines containing an SSID; do not read `~/.config/screeny/wifi.env`).
Leave `crates/art` and `crates/studio` alone; `crates/device-api`, `crates/sim`,
`crates/probe`, `crates/provision` are shared with the `software` session - additive where
possible, every change listed in your report. **Writes stay inside the inactive app slot
and `otadata`**; the settings partition, the bootloader, the partition table and the
running slot are untouchable. The settings (WiFi credentials included) must survive every
path - say how you know.

## Log

### 1. The design, before building (worker-241, 2026-09-20)

#### The shape, in ten lines

`POST /api/v1/firmware` stages exactly as card 240 built it and then, unless the
caller said `?activate=0`, **activates**: the reply goes out, the worker's
`BoundedSocket` waits for the peer's acknowledgement, and only then does a task
outside the HTTP path write `otadata` and reset the chip. The bootloader
promotes that entry `NEW -> PENDING_VERIFY` and boots the staged slot **on
trial**. The image on trial has to earn `VALID`: WiFi with a DHCP address, one
HTTP request served or 120 s of uptime, one display swap, never before 60 s. If
it does not by 180 s it writes `INVALID` on its own entry and resets. If it
panics, wedges or is unplugged, **the bootloader** turns `PENDING_VERIFY` into
`ABORTED` on the next reset of any kind and boots the other slot. Either way the
previous image comes back, and it says so: one boot line, `fw_state` reading
`invalid` or `aborted`, and `GET /api/v1/panic`'s new `update` object naming the
slot, the reason and the version that was rejected.

#### Where each fact lives, and why it lives there

| fact | where | survives | why there |
|---|---|---|---|
| which slot boots next | `otadata`, the two `ota_seq` entries | **everything, power loss included** | it is the only thing the bootloader reads, and the decision has to outlive the update that made it |
| is the running image on trial | `otadata`, the selected entry's state = `PENDING_VERIFY` | everything | the bootloader both sets it (from `NEW`) and clears it (to `ABORTED`); an app-side copy would be a second opinion about a fact the bootloader owns |
| has it confirmed | state = `VALID` | everything | same |
| was the last activation rolled back | state = `ABORTED` (a reset during the trial) or `INVALID` (this app gave up at the deadline) | everything | **the reason is in `otadata` itself**, so a device that was power-cycled after the revert still knows why - which RTC memory could not tell it |
| which slot is *actually* running | the MMU, `PartitionTable::booted_partition()` | read fresh each boot | after a rollback it differs from the selected slot, and that difference is the detection |
| "a trial is expected on the next boot" | RTC slow memory, one bit in card 243's breadcrumb (`F_OTA_TRIAL`) | `SW_RESET`, **not** power-on | it is needed *before* flash is up, to arm the watchdog, and it is the one fact that is safe to lose on a power cycle - a power cycle is itself a reset, so the bootloader has already aborted the trial by the time `main` runs |
| what panicked during the trial | card 243's breadcrumb, unchanged | `SW_RESET` | already there, already reported |
| the settings and the WiFi credentials | the `screeny` partition | everything | **nothing in this card reads or writes that partition**, and that is the whole guarantee: see "what this can reach" below |

No new bytes of the settings store, and **no new words of the breadcrumb**: one
spare flag bit in the existing `W_FLAGS` word. The breadcrumb's layout, its
magic and its checksum are untouched, so a 0.6.x record still reads back intact
across the update that installs 0.7.0.

#### Who writes `otadata`, when, and from where

Three writes, and **no others**. Each is a synchronous `#[inline(never)]` call
under the settings store's own `STORE` lock and its `multicore_auto_park`
`FlashStorage` - the same discipline card 240's `stage_sector` keeps - because
`FlashRegion::write` on a 32-byte entry does a read-modify-erase-write of the
whole 4 KB sector with a `[u8; 4096]` on the caller's stack (`esp-storage`'s
`FlashSectorBuffer`). That frame is the reason none of them may happen inside an
HTTP handler (card 227's lesson, device-web's lessons list).

1. **Activate** - `Ota::set_current_app_partition(staged)` then
   `set_current_ota_state(New)`. In `ota::activate_task`, woken by a signal the
   handler raises **after** it has produced the reply. The handler itself writes
   no flash and the reply is already on the wire: `Dispatch` writes it, card
   236's `BoundedSocket` closes and waits up to `CLOSE_ACK_MS` = 1500 ms for the
   peer's ACK, and the activation waits `ACTIVATE_DELAY_MS` = 2000 ms, which is
   past that bound by design. Then `software_reset()`.
2. **Confirm** - `set_current_ota_state(Valid)`, once, from `ota::trial_task`,
   and only on a boot whose state was `PENDING_VERIFY`. A serial-flashed image
   is already `VALID` (the bootloader writes `otadata[0]` itself when the
   partition is erased - card 240's bench saw `fw_state valid` after every
   flash), so the task is never even spawned and **no boot rewrites `otadata`**.
3. **Revert** - `set_current_ota_state(Invalid)` from the same task at the
   deadline, then `software_reset()`.

Plus one repair that fires only in a state nothing normal produces: a boot that
finds `booted == selected` with state `Undefined` promotes it to
`PENDING_VERIFY`. That is the signature of an activation interrupted between its
two writes, and promoting it is what puts such an image on trial instead of
letting it become permanent (interruption 5b below).

#### What this can reach, and what it cannot

Card 240's writer could address only the inactive slot, by type
(`store::InactiveSlot`, made only by `read_partitions`, only after comparing the
entry's offset with the MMU's). This card adds exactly one more target: the
`otadata` entry, reached through `parts.otadata.as_flash_region(flash)`, which
`Ota::new` refuses unless it is 0x2000 bytes and typed `Data(Ota)`, and whose
`read`/`write` are partition-relative and bounds-checked by
`esp-bootloader-esp-idf` (`partitions.rs`, `in_range`). So the reachable set is
`{ota_1 or ota_0 - whichever is not running, otadata}` and nothing else. **The
`screeny` settings partition is not in it, is not opened by any code this card
adds, and the WiFi credentials therefore cannot be touched by any path here** -
not by activate, not by confirm, not by revert, not by the bootloader's own
`write_otadata`, which writes one sector of `otadata` at
`ota_info.offset + 0x1000 * i`. The bootloader, the partition table and the
running slot are unreachable for the same reason they were in card 240.

#### The bootloader's half, established from the v6.1 source

`firmware/bootloader/esp32-rollback-bootloader.bin` is ESP-IDF v6.1 with
`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` (card 242). Read from
`components/bootloader_support/src/bootloader_utility.c` and
`bootloader_common_loader.c` at `release/v6.1`, in
`bootloader_utility_get_selected_boot_partition`:

```c
#ifdef CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE
    bool write_encrypted = esp_efuse_is_flash_encryption_enabled();
    for (int i = 0; i < 2; ++i) {
        if (otadata[i].ota_state == ESP_OTA_IMG_PENDING_VERIFY) {
            otadata[i].ota_state = ESP_OTA_IMG_ABORTED;
            write_otadata(&otadata[i], bs->ota_info.offset + FLASH_SECTOR_SIZE * i, write_encrypted);
        }
    }
#endif
```

and, after the active entry has been chosen:

```c
            if (otadata[active_otadata].ota_state == ESP_OTA_IMG_NEW) {
                otadata[active_otadata].ota_state = ESP_OTA_IMG_PENDING_VERIFY;
                write_otadata(&otadata[active_otadata], ..., write_encrypted);
            }
```

with

```c
bool bootloader_common_ota_select_invalid(const esp_ota_select_entry_t *s)
{
    return s->ota_seq == UINT32_MAX || s->ota_state == ESP_OTA_IMG_INVALID ||
           s->ota_state == ESP_OTA_IMG_ABORTED;
}
bool bootloader_common_ota_select_valid(const esp_ota_select_entry_t *s)
{
    return bootloader_common_ota_select_invalid(s) == false && s->crc == bootloader_common_ota_select_crc(s);
}
```

Three things follow, and they are the load-bearing ones:

* **The abort loop is unconditional.** It does not look at the reset reason, so
  a `PENDING_VERIFY` entry is aborted by a panic reset, a watchdog reset, a
  brownout, the EN button and somebody pulling the USB cable alike. The card's
  "including a reset that is not a panic" is answered: **any** reset.
* **`ABORTED` and `INVALID` are not "valid"**, so `bootloader_common_get_active_otadata`
  skips them and picks the other entry - which is the previous image. This is
  the whole revert, and it needs nothing from the app.
* When **both** entries are invalid and there is no factory partition, the
  bootloader logs "No factory image, trying OTA 0" and boots `ota_0`
  unconditionally. That is the floor under every interrupted write below.

What could **not** be established offline and must be read off the bench: those
`ESP_LOGD` lines are compiled out at the default INFO level, so the serial log
shows the rollback only as *which image boots*, never as a message. The bench
procedure below is written to prove it by behaviour rather than by a log line,
and its step 1 proves the plumbing before step 2 relies on it.

#### The trap in `esp-bootloader-esp-idf`'s own bookkeeping

`Ota::current_app_partition()` computes the selected slot from `max(seq0, seq1)`
and **ignores the states**, so after a bootloader rollback it names the slot that
was rolled *back from* - the aborted one - while `booted_partition()` (the MMU)
names the one really running. Three consequences, all designed around rather
than worked around:

* `fw_slot` already reads the MMU first (card 240), so it stays right.
* The **disagreement is the detection**: `booted != selected` means the
  bootloader did not run what `otadata` selected, which is a rollback (or the
  structural-image fallback, which is the same class of event).
* `set_current_ota_state` writes the entry with the highest sequence number, so
  after a rollback it targets the *aborted* entry, not the running image's. That
  is only ever reached on a trial boot (`PENDING_VERIFY`), where the two are the
  same entry, so no path in this card can write the wrong one. It is also why
  the next activation still works: `set_current_app_partition(staged)` finds
  "current" already equal to the staged slot and writes no sequence, and the
  following `set_current_ota_state(New)` clears `ABORTED` on an entry whose
  sequence is already the highest - which selects the staged slot. Verified
  against `ota.rs` lines 269-337 and the crate's own `test_multi_updates`.

#### Every interruption point, and the state booted from each

From "last byte staged" to "confirmed". 006 section 5's table covers rows 1-3;
the rest are this card's.

| # | power / link lost | flash state | next boot |
|---|---|---|---|
| 1 | mid-stream, or all bytes written but not yet validated | staged slot part-written, `otadata` untouched | **running slot** (006 rows 2-3) |
| 2 | validation failed | as above, no `otadata` write | **running slot** |
| 3 | validated, reply not yet written | as above | **running slot** |
| 4 | reply sent, before the deferred activation | as above | **running slot** - the caller was told `"activating":true` and the device comes back on the old image. Honest and repeatable: nothing is half-done, and the upload can simply be sent again. |
| 5a | during `set_current_app_partition` (the sequence write) | the entry being written is `current_slot().next()`, i.e. **never the entry the device is currently booting from**; the two entries are in different 4 KB sectors (0x0000 and 0x1000), so only one can ever be in flight. A half-written entry is `0xFF` (erase done, write not) or carries a stale CRC. | **running slot**: `ota_select_valid` rejects it and the untouched entry wins. This is the atomicity the whole design rests on. |
| 5b | between the sequence write and the `New` write | the staged slot is now selected, with whatever state that entry held before: `Undefined` (the first update after a serial flash) or `Aborted` (a slot that was rolled back) | **the staged slot, and the bootloader will not put it on trial** - `Undefined` is not `NEW`. That is the one window where an unproven image could become permanent, so the app closes it: a boot with `booted == selected` and state `Undefined` is promoted to `PENDING_VERIFY` and tried like any other. A leftover `Valid` cannot occur, because a slot's entry is `Valid` only while that slot is the *running* one, which the staged slot is not. `Aborted` is rejected by the bootloader and boots the running slot. |
| 6 | after both writes, before the reset | committed | **the staged slot, on trial** (006 row 6) |
| 7 | the trial boot hangs before `main`, or wedges with interrupts off, and nothing resets it | committed, `PENDING_VERIFY` | nothing happens until the RTC watchdog fires - see the table of mechanisms below - and then the **previous slot** |
| 8 | any reset during the trial: panic, watchdog, brownout, EN, USB pulled | committed, `PENDING_VERIFY` | **previous slot**, by the bootloader's abort loop. `fw_state` then reads `aborted`. |
| 9 | during the confirm write (`Valid`) | the entry being written **is** the running image's, and it is the active one | interrupted, it reads back `0xFF` or with a stale CRC, so the *other* entry wins: **previous slot**. A confirm that is interrupted un-installs the update rather than half-installing it, which is the safe direction, and the update is simply repeated. |
| 10 | during the revert write (`Invalid`) | the same entry, the same argument | **previous slot** - which is what the write was trying to arrange anyway |
| 11 | after the confirm | `VALID` | the new slot, for ever. Done. |
| 12 | an upload arrives while the running image is still on trial | refused `busy` before a sector is erased | n/a - and this is a safety rule, not a courtesy: the inactive slot during a trial holds **the known-good image we may have to roll back to**, and letting an upload overwrite it would remove the escape hatch (006 section 6, mitigation 3). |

#### Which mechanism covers which failure

| the trial image... | what catches it | how long |
|---|---|---|
| panics | card 243's handler resets the chip; the **bootloader** aborts the `PENDING_VERIFY` entry on that reset | ~120 ms + one boot |
| is reset by anything else (brownout, EN, USB out, a timer watchdog) | the **bootloader**, same loop, same unconditional test | one boot |
| boots but never becomes healthy (no WiFi, a wedged HTTP server, a dead panel) | the **app-side deadline**: `INVALID` at 180 s, then `software_reset()` | 180 s |
| hangs before `main`, or with interrupts off, and never resets | the **RTC watchdog**, armed from the breadcrumb bit as the first thing after `esp_hal::init` and disabled again the moment the boot classification says this is not a trial | `TRIAL_WDT_S` = 240 s |
| is structurally broken (truncated, a bad segment table) | it never reaches `otadata`: card 240's validator refuses it, and the bootloader's own image verification is the second net | during the upload |

The watchdog is not decoration: `esp_hal::init` **disables** every watchdog this
chip has - `rtc.swd.disable()`, `rtc.rwdt.disable()` and both timer-group WDTs
(`esp-hal-1.2.2/src/lib.rs` lines 768-778) - so without this there is nothing
armed on the device at all, and 006 section 6's residual risk ("hangs before our
own confirm code runs") would still be open even with the rollback bootloader.
It is armed **only on a trial boot**, it is never fed, and it is disabled on
confirm, so the shipping device carries no watchdog it has to keep alive. An
RWDT reset is a system reset, which leaves the RTC domain and therefore the
breadcrumb alone (`esp-hal` zeroes `.rtc_slow.persistent` only on
`ChipPowerOn`).

#### The health criterion, and where each input comes from

006 section 6 and the card agree, so there is nothing to reconcile: confirm when
**all** of - WiFi associated **and** a DHCP address (`http::IPV4 != 0`, which the
frame task publishes every 20 ms); **and** one HTTP request served
(`http::REQUESTS`, a new 4-byte counter) **or** 120 s of uptime; **and** at least
one display swap since boot (`crate::SWAPS`, which already exists) - and never
before 60 s. Revert at 180 s. The 60 s floor is deliberately the same number as
card 243's `QUICK_MS`, so "too early to count as healthy" and "that was a quick
death" cannot disagree about what an early death is.

#### Where 006 and this card differ

Nothing material. 006 section 9's card E is this card, in the same order
(`activate_next_partition` + `New`, the boot-time machine armed as early as
possible, the criterion, the 180 s revert). Three places where 006's letter is
extended rather than contradicted, each because the bench has since answered
something 006 could only guess at:

1. 006 section 6 is written for a bootloader **without** rollback and calls the
   app-side `INVALID` path "all we get". Card 242 shipped the rollback
   bootloader, so this card builds both halves and says which covers what.
2. 006 names `OtaUpdater::activate_next_partition()`. This card uses the lower
   `Ota::set_current_app_partition` + `set_current_ota_state` on the entry for
   `store::InactiveSlot` instead, because `OtaUpdater` decides for itself which
   partition is "next" from `booted_partition()` and would build a second view
   of a question the firmware already answered once at boot - and because card
   240's `InactiveSlot` newtype is the one place allowed to name a writable slot.
3. 006's mitigation 1 ("arm the confirm/revert logic as early as possible")
   becomes, concretely, the RTC bit plus the watchdog: the *logic* still needs
   flash and cannot run before `store::init`, but the thing that rescues a hang
   can, and does.

### 2. What was built, and the numbers

#### The pieces

**`crates/otastate`** (new, `no_std`, no alloc, no clock) is the part of this
card that is pure reasoning: `classify(booted, selected, state) -> Boot` and
`decide(Health) -> Wait | Confirm | Revert`. The firmware calls both, so the
device and `cargo test` cannot disagree about when an update has proved itself.
Behind its `model` feature - on by default, **off in the firmware** - is a paper
`otadata` and a paper ESP-IDF v6.1 bootloader: the unconditional
`PENDING_VERIFY -> ABORTED` loop, `NEW -> PENDING_VERIFY`, `ota_select_valid`,
and `esp-bootloader-esp-idf`'s own sequence arithmetic, each cited to file and
line. `tests/interruptions.rs` cuts the power at every instant of the table
above, twice over (an erased sector and a torn one), and asserts what boots.

**`firmware/src/ota.rs`** grows the three `otadata` writes and nothing else
touches it: `write_selection` (activate), `write_state` (confirm and revert),
each `#[inline(never)]` and synchronous under the store's `STORE` lock.
`note_boot` classifies the boot, promotes an interrupted activation, reads the
rejected image's version out of the other slot and logs the line that says an
update was rolled back. `trial_task` exists **only on a trial boot**;
`activate_task` sleeps on a signal, waits 2 s and resets.

**`firmware/src/main.rs`** arms the RTC watchdog from the breadcrumb bit as the
first statement after `esp_hal::init`, and disarms it a second later if the
boot classification says this is not a trial.

**`firmware/src/http.rs`** parses `?activate=`, counts requests for the health
criterion, answers `activating` and carries `update` on `GET /api/v1/panic`.

#### Where the card and 006 differed, and what I did

Only the three places the design note lists, and 006 wins in all three - it is
just that two of its sentences were written before card 242 existed and one
before there was an HTTP route. Nothing in 006's plan was dropped. The card's
own health criterion is word for word 006 section 6's, so there was nothing to
reconcile there: 60 s floor, 120 s HTTP grace, 180 s revert.

One thing the card asks for that I deliberately did **not** build: the card's
step 5 says "after a revert the device says so ... `GET /api/v1/panic`'s reply
is the natural home". It is, and it is there - but I did not put *why* in RTC
memory, because `otadata` already holds it (`INVALID` = the app gave up,
`ABORTED` = something reset it) and `otadata` survives a power cut where RTC
memory does not. The breadcrumb's contribution is one bit, and it buys the
watchdog.

#### What was established about the bootloader, and how

From ESP-IDF `release/v6.1`'s own sources (quoted in the design note above):
`bootloader_utility_get_selected_boot_partition` turns **any** `PENDING_VERIFY`
entry into `ABORTED` on **every** boot before it selects anything, without
looking at the reset reason; `bootloader_common_ota_select_invalid` treats
`ABORTED` and `INVALID` as unusable, so the other entry is chosen; and with both
unusable and no `factory` partition the bootloader boots `ota_0`. That is the
whole revert mechanism and the app contributes nothing to it.

From `esp-bootloader-esp-idf 0.6.0`'s `src/ota.rs`: `set_current_app_partition`
writes `current_slot().next()` - never the entry that is selecting the running
image - and `current_app_partition` ignores the image states, which is both this
card's detection and its trap.

From `esp-hal-1.2.2/src/lib.rs` lines 768-778: `esp_hal::init` disables the
super watchdog, the RTC watchdog and both timer-group watchdogs, so **nothing
is armed on this device by default** and the hang case needed one.

**What the bench must establish, because it cannot be established here:**

1. That the rollback bootloader on the device really behaves like its source.
   The two `ESP_LOGD` lines are compiled out at the default INFO level, so there
   is no log line to look for: the evidence is *which image boots*, and step 3
   of the procedure below is the test.
2. That the RTC watchdog's timeout really is ~240 s. `set_timeout` converts
   through the **calibrated** RTC slow-clock period, so it should be accurate,
   but nothing here has run it. A premature fire would show as
   `reset reason rtc_wdt` in the boot line and would revert a good update -
   safe, and visible.
3. That an `otadata` write during a live stream costs the radio nothing. It is
   one sector (~60 ms), against the 244 of an upload that card 240 measured with
   `link downs +0`, so this is a formality.

#### Which mechanism covers which failure

Unchanged from the design note's table, and now with the code behind each:
the bootloader's abort loop for every reset (panic included, via card 243's
handler); `ota::trial_task`'s 180 s deadline for an image that boots and is
useless; the RTC watchdog, armed from the breadcrumb bit, for an image that
hangs; and card 240's validator for one that is structurally broken.

#### RAM, measured (`tools/fw-size.sh`, floor 24,576)

| build | `.stack` 0.6.0 | `.stack` 0.7.0 | `.bss` | image |
|---|---|---|---|---|
| default | 26,944 | **26,240** | 110,464 | 1,013,501 |
| `panic-test` | 26,880 | **26,176** | 110,528 | 1,014,321 |
| `http-selftest` | 26,528 | **25,824** | 110,848 | 1,048,253 |
| `start-in-portal` | 26,944 | **26,240** | 110,464 | 1,013,369 |
| `ota-test-unhealthy` | - | **26,240** | 110,464 | 1,013,605 |
| `ota-test-panic` | - | **26,176** | 110,528 | 1,013,973 |

`.bss` 110,208 -> 110,464 (**+256**), `.data` 59,444 -> 59,892 (+448),
`.rwtext` (IRAM) unchanged at 66,932: nothing new is `#[ram]`. The ceiling
costs 704 bytes for the whole card, which is the update record's statics, the
watchdog handle, the request counter and `otastate`'s code.

**Heap: nothing at all.** Card 240's one 4 KB staging allocation is still the
only thing this firmware asks the allocator for by name, and this card adds no
allocation on any path - the `otadata` entry is 32 bytes on the stack and the
version read is 112.

#### Core 0's stack, by card 243b's method

`xtensa-esp32-elf-objdump -d`, frames from `entry a1, N` plus `addmi a1, a1, -N`
(hex included). The numbers that matter:

| frame | bytes | what it is |
|---|---|---|
| `route_request`'s `poll` | **2,128** | every HTTP request pays it; 2,304 on 0.6.0, so the query parsing and the activation branch came in **176 bytes cheaper** than what they replaced |
| `Select<serve_on, wait_ap>` | 4,576 | unchanged |
| `FlashRegion::write` | **4,176** | the `otadata` write's sector buffer |
| `ota::write_selection` | 160 | |
| `ota::write_state` | 128 | |
| `ota::read_version` | 176 | |
| `activate_task` / `trial_task` poll | 128 each | |
| `main`'s poll (holds `read_fw_health`) | 3,088 | unchanged |

So the **`otadata` write chain is ~4.5 KB**: task poll 128 + `write_selection`
160 + `FlashRegion::write` 4,176, on top of the executor. The same write inside
an HTTP handler would have sat on ~6.7 KB of serve chain instead of on nothing,
for ~11 KB - which is why it is in a task, and is card 227's lesson restated
with this card's numbers.

**One correction to card 240's Log, found while measuring.** It says that
`NorFlashRegion::read`/`write` on a word-aligned buffer avoid esp-storage's
4 KB frame. The *copy* is avoided - the fallback branch is not taken - but the
**frame is not**: `NorFlashRegion::ReadNorFlash::read` is 4,144 bytes and
`NorFlash::write` is 4,160 in this build, because the compiler reserves the
alignment-fallback buffer on entry whether or not the branch runs. So card 240's
upload path is deeper than its Log claims (~11 KB with the serve chain, which is
consistent with the `stack_free` 11,584 the bench measured after an upload).
It changes no decision here and is not a regression; it is a number that should
be right in the record.

#### Reply sizes

`FirmwareReply` 8 -> **12 bytes**, `PanicReply` grows by `UpdateRecord`. Neither
is on the hot path and `ApiBody`'s size is still `StatusReply`'s 208, which
`crates/device-api/tests/sizes.rs`'s
`no_reply_is_bigger_than_the_one_on_the_hot_path` asserts and which is why card
243b's +3,488-byte lesson is not repeated here. `MAX_JSON_LEN`: firmware
57 -> 76, panic 231 -> 410.

#### The host tests, and what they cover

`crates/otastate`, **34 tests** (11 unit + 23 integration):

| what | tests |
|---|---|
| a serial-flashed device is `Settled` and writes nothing, ever | `a_serial_flashed_device_runs_ota_0_and_has_nothing_to_prove`, `a_settled_boot_writes_nothing_to_otadata_however_many_times_it_reboots` |
| the health criterion, every clause | `nothing_confirms_before_sixty_seconds_however_healthy`, `every_part_of_the_criterion_is_needed`, `a_device_nobody_visits_confirms_at_two_minutes`, `an_image_that_never_becomes_healthy_is_reverted_at_three_minutes`, `health_arriving_on_the_deadline_tick_keeps_the_update` |
| **every interruption point** (rows 5a, 5b, 6, 9, 10), for an erased sector *and* a torn one | `every_interruption_of_an_activation_leaves_a_device_that_boots`, `an_interrupted_sequence_write_boots_the_image_that_was_already_running`, `an_activation_interrupted_before_the_state_write_still_puts_the_image_on_trial`, `an_interrupted_state_write_...`, `losing_power_between_the_last_write_and_the_reset_is_simply_the_update`, `power_lost_during_the_confirm_write_un_installs_rather_than_half_installs`, `power_lost_during_the_revert_write_still_boots_the_previous_image` |
| the three ways a trial ends badly (row 8) | `an_image_that_never_becomes_healthy_is_reverted_at_the_deadline`, `a_panic_during_the_trial_is_rolled_back_by_the_bootloader_alone`, `a_reset_that_is_not_a_panic_is_rolled_back_the_same_way` |
| the promotion of row 5b really arms the bootloader | `an_unproven_image_that_dies_is_still_rolled_back` |
| the crate's own trap: the next update after a rollback | `an_update_after_a_rollback_still_activates` |
| it never gets stuck | `a_crash_loop_cannot_happen_because_the_second_boot_is_the_old_image`, `a_run_of_bad_updates_never_leaves_the_device_off_the_air`, `updates_alternate_slots_for_as_long_as_they_keep_working` |
| the bootloader's floor | `a_structurally_broken_image_never_runs_even_if_otadata_selects_it`, `an_otadata_with_nothing_usable_in_it_boots_ota_0`, `a_device_that_cannot_read_otadata_still_runs_and_says_it_does_not_know` |
| row 12, why an upload during a trial is refused | `during_a_trial_the_inactive_slot_is_the_image_we_may_have_to_go_back_to` |

`crates/fwimage` +4 (`version_of` against a good image, an erased slot, a
half-staged one, an image with no descriptor, and somebody else's app);
`crates/device-api` +4 goldens and the doc test for `parse_activate`;
`crates/provision` +1 (the installing screen is not the updating one and has no
bar).

#### One thing this card cannot test on the host

**That the promotion in `note_boot` is the right call at all.** Interruption 5b
is a window of one sector write, and everything about closing it is reasoning
plus the model. If the orchestrator wants it on the bench it is one deliberate
power cut in the two seconds after `fw-upload --activate` prints its reply - and
it is not in the procedure below, because "pull the plug at the right
millisecond" is not a repeatable bench step and the failure it guards against is
already the least likely one in the table.

### 3. The bench procedure (for the orchestrator - the worker touched no hardware)

Four steps, about **fifteen minutes** in total, and every one of them says what
"stop" looks like. **The recovery that always works is `tools/fw-run.sh`**: it
passes `--erase-data-parts ota`, so a serial flash erases `otadata` and the
bootloader starts again from `ota_0` whatever state the device had got itself
into. Nothing in this card can prevent that, because nothing in this card can
write outside the inactive slot and `otadata`.

Before anything: `backup/tidbyt-stock-*.bin` must exist and be 8,388,608 bytes.
Bench Mac wired with its WiFi **off** (the en0/en5 stall, card 223). Serial
monitor attached and logging for the whole run - this card's evidence is almost
all serial lines. Take the source lock off the Studio and check `screeny stats`
says HOLD or IDLE:

```bash
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":false}'
```

**Artefacts** (this session's scratchpad,
`/private/tmp/claude-501/-Users-aaron-src-screeny/81d1cc11-75c9-4f5f-a521-784097e6406f/scratchpad/`):

| file | `esp_app_desc.version` | bytes | what |
|---|---|---|---|
| `screeny-fw-0.7.0-default.elf` | `0.7.0` | - | **the build to serial-flash** |
| `screeny-fw-0.7.1-good.bin` | `0.7.1` | 1,013,552 | a good update: must confirm |
| `screeny-fw-0.7.1-unhealthy.bin` | `0.7.2-unhealthy` | 1,013,376 | never reports healthy: must revert at 180 s |
| `screeny-fw-0.7.1-panic.bin` | `0.7.3-panic` | 1,013,984 | panics at 20 s: must be rolled back by the bootloader |
| `screeny-fw-0.7.1-good.elf`, `-unhealthy.elf`, `-panic.elf` | | | the ELFs, for symbolising a backtrace |

The three `.bin`s were made from those ELFs with, from the repository root and
after `. ~/export-esp.sh`:

```bash
espflash save-image --chip esp32 --flash-size 8mb \
  --partition-table firmware/partitions.csv <elf> <out>.bin
```

`--flash-size 8mb` is not optional: without it espflash assumes 4 MB and refuses
the table. **The version strings are deliberately all different**, so every
answer below names which image it is talking about without anybody having to
remember what was uploaded. All three pass `screeny-probe fw-scan`.

---

**Step 0 - flash 0.7.0 and check the baseline (about 3 minutes).**

```bash
. ~/export-esp.sh
cd firmware && cargo build --release && cd ..
tools/fw-size.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw
tools/fw-run.sh /private/tmp/.../scratchpad/screeny-fw-0.7.0-default.elf fw-0.7.0-flash 30
```

Expect `.stack 26240`, over the 24,576 floor. On the serial log, after the two
`boot:` lines:

```
store: running from 0x10000; an upload would stage into 0x210000 (2048 KB)
http: fw slot Ota0 state Valid
```

and **no `ota:` line at all** - a serial-flashed device is `Settled`, so this
card's machinery does not run and `otadata` is not written. Then:

```bash
cargo run --release -p screeny-probe -- --addr 192.168.7.221 http
cargo run --release -p screeny-probe -- --addr 192.168.7.221 conformance --slow
curl -s http://192.168.7.221/api/v1/panic | jq
```

Expect HTTP **41 passed, 0 failed, 4 skipped, 0 connects refused** (45 rules;
44 and 45 are new), UDP **60/0/4**, and
`{"boot_count":1,"panic_count":0,"last_panic":null,"update":null}`.

*Stop if* the `http: fw slot ... state ...` line says anything but
`Ota0 Valid`, or if there is an `ota: REVERTED`/`ota: ON TRIAL` line: the device
is not in the clean state the rest of this depends on. Recovery: `tools/fw-run.sh`
again, and if it repeats, the partition table or the bootloader is not what the
repository says.

---

**Step 1 - the good update (about 2 minutes, of which 60-120 s is waiting).**

Give the panel back to the Studio first, so the whole thing happens under a live
30 fps stream:

```bash
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":true}'
sleep 10
cargo run --release -p screeny-probe -- --addr 192.168.7.221 \
  fw-upload /private/tmp/.../scratchpad/screeny-fw-0.7.1-good.bin --activate
```

`--activate` is the flag that makes this reboot the device; without it
`fw-upload` stages and stops, which is card 240's behaviour.

**On the panel:** "updating" with a bar filling for ~25 s, then **"installing /
restarting"** for two seconds, then the boot.

**On the wire**, in order:

```
HTTP 200 in 25.x s (39 KB/s)
  ok true written 1013552 error None activating true
  waiting for the device to come back (up to 90 s)...
  back after ~45 s: fw 0.7.1 slot Ota1 state PendingVerify boot_id N uptime ~20000 ms
  waiting for the trial to end (up to 240 s)...
  at ~45 s: Trial slot Ota1 version Some("0.7.1") reason None
  at ~75 s: Confirmed slot Ota1 version Some("0.7.1") reason None
  CONFIRMED: the update stuck.
```

**On the serial log**, the four `ota:` lines of card 240 and then five more that
are this card's whole deliverable:

```
ota: staged image accepted - 1013552 bytes, 5 segments, version "0.7.1"
ota: ACTIVATED 0x210000 - restarting into it on trial. If it does not prove itself within 180 s, or resets before it does, the bootloader brings fw 0.7.0 back.
<the ROM banner and the bootloader>
boot: #1 since power-on, reset reason software (...)
ota: trial boot - RTC watchdog armed for 240 s (it is never fed; confirming turns it off)
store: running from 0x210000; an upload would stage into 0x10000 (2048 KB)
http: fw slot Ota1 state PendingVerify
ota: ON TRIAL - this boot is a firmware update's first run from Ota1. It confirms itself once it is healthy (never before 60 s) or reverts at 180 s.
ota: trial at 30 s - ip true, http 0, swaps NNNN (healthy false)
ota: CONFIRMED at 6x s - fw 0.7.1 is now this device's firmware (otadata says valid). ip true, http N, swaps NNNN.
ota: RTC watchdog disabled
```

The confirm lands at **60-63 s** if anything has made an HTTP request (the
probe's wait does, every second), and at **120 s** on a silent network.

Then, and this is the check that matters:

```bash
curl -s http://192.168.7.221/api/v1/status | jq '{fw, fw_slot, fw_state, boot_id}'
# fw "0.7.1", fw_slot "ota_1", fw_state "valid"
cargo run --release -p screeny-probe -- --addr 192.168.7.221 reboot
sleep 30
curl -s http://192.168.7.221/api/v1/status | jq '{fw, fw_slot, fw_state}'
# still 0.7.1 / ota_1 / valid: a confirmed update survives a reboot
curl -s http://192.168.7.221/api/v1/panic | jq '.update'
# null - a settled boot has nothing to report
```

*Stop if* the device comes back on `0.7.0`: something reverted a healthy image,
and the serial log's `ota:` lines say which deadline did it. *Stop also if* the
boot line reads `reset reason rtc_wdt`: the watchdog fired early, which means
`TRIAL_WDT_S`'s conversion is wrong on this chip - report the number and the
uptime it fired at. Either way the device is fine and running 0.7.0.

---

**Step 2 - the update that never becomes healthy (about 5 minutes).**

This is the app-side deadline on its own: the image boots, joins, serves and
draws, and simply never says it is healthy.

```bash
cargo run --release -p screeny-probe -- --addr 192.168.7.221 \
  fw-upload /private/tmp/.../scratchpad/screeny-fw-0.7.1-unhealthy.bin --activate
```

Expect the upload and the reboot exactly as in step 1, then **nothing happening
for three minutes**, then a revert. On the wire:

```
  back after ~45 s: fw 0.7.2-unhealthy slot Ota0 state PendingVerify ...
  at ~45 s: Trial slot Ota0 version Some("0.7.2-unhealthy")
  at ~230 s: Reverted slot Ota0 version Some("0.7.2-unhealthy") reason Some(Deadline)
  REVERTED: the update did not stick and the previous image is running.
```

On the serial log, the trial image's own account of itself:

```
ota-test: BENCH BUILD - this image will never report healthy, so it must be reverted at 180 s (card 241)
ota: trial at 30 s - ip false, http 0, swaps 0 (healthy false)
ota: trial at 60 s - ip false, http 0, swaps 0 (healthy false)
...
ota: REVERTING at 180 s - fw 0.7.2-unhealthy never became healthy (ip false, http requests 0, swaps 0). Marking this slot invalid and restarting into the previous image.
<reset, ROM banner, bootloader>
http: fw slot Ota1 state Invalid
ota: REVERTED - the last firmware update did not stick. ota_0 was rolled back (it booted but never became healthy, so it marked itself invalid), and this device is running Ota1 again, fw 0.7.1.
```

and then:

```bash
curl -s http://192.168.7.221/api/v1/status | jq '{fw, fw_slot, fw_state}'
# "0.7.1", "ota_1", "invalid"   <- fw_state is the *rejected* entry's; spec 8.10
curl -s http://192.168.7.221/api/v1/panic | jq '.update'
# {"outcome":"reverted","reason":"deadline","slot":"ota_0","version":"0.7.2-unhealthy"}
```

**The whole cycle is 180 s + ~20 s of boot, so the device is unreachable for
about 25 s and back on its feet inside four minutes of the upload finishing.**

*Stop if* it is still running `0.7.2-unhealthy` five minutes after the upload:
the deadline did not fire, and the serial log's `ota: trial at N s` lines say
how far it got. The device is reachable, so `screeny-probe reboot` gets you back
(the bootloader aborts a `PENDING_VERIFY` on that reset), and `tools/fw-run.sh`
always does.

---

**Step 3 - the update that panics (about 2 minutes). This is the card's point.**

Nothing in the application does the work here: the panic handler resets the
chip and the **bootloader** is what refuses to boot the image again.

```bash
cargo run --release -p screeny-probe -- --addr 192.168.7.221 \
  fw-upload /private/tmp/.../scratchpad/screeny-fw-0.7.1-panic.bin --activate
```

On the serial log, and this is the sequence to read carefully:

```
ota: ACTIVATED 0x10000 - restarting into it on trial. ...
boot: #1 since power-on, reset reason software
ota: trial boot - RTC watchdog armed for 240 s ...
http: fw slot Ota0 state PendingVerify
ota: ON TRIAL - ... from Ota0 ...
ota-test: BENCH BUILD - panicking on core 0 in 20 s, on every boot (card 241)
<at ~20 s of uptime>
====================== PANIC ======================
panicked at src/ota.rs:1347:
ota-test: deliberate panic during an OTA trial (card 241)
Backtrace: 0x400d....
panic: resetting the chip (card 243; the breadcrumb is in RTC memory).
<reset, ROM banner, bootloader>
boot: #2 since power-on, reset reason software (the boot before it started with software)
boot: last panic was boot #1 at uptime 20xxx ms, ota.rs:1347, 1 in a row (1 panic(s) since power-on)
http: fw slot Ota1 state Aborted
ota: REVERTED - the last firmware update did not stick. ota_0 was rolled back (it reset before it could confirm itself - a panic, a watchdog or a power cut; GET /api/v1/panic says which), and this device is running Ota1 again, fw 0.7.1.
```

**`state Aborted` is the bootloader's own signature** and the only evidence
there will be that the rollback half of card 242's bootloader is compiled in:
nothing in this firmware writes `ABORTED`, and the `ESP_LOGD` line that would
have said so is compiled out at INFO. If that line reads `Aborted`, the
bootloader did it.

```bash
curl -s http://192.168.7.221/api/v1/panic | jq
# {"boot_count":2,"panic_count":1,
#  "last_panic":{"uptime_ms":20xxx,"boot":1,"file":"ota.rs","line":1347,"consecutive":1},
#  "update":{"outcome":"reverted","reason":"aborted","slot":"ota_0","version":"0.7.3-panic"}}
```

**It must panic exactly once.** `ota-test-panic` panics on *every* boot on
purpose, so a second panic 20 s later would mean the device booted the bad image
again - i.e. the rollback did not happen. `panic_count` 1 and `consecutive` 1 is
the pass.

*Stop if* `panic_count` reaches 2, or if the device stops answering altogether:
the bootloader on this device does not have `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`
after all, and card 243's crash-loop guard will halt it with `CRASHED` on the
panel after five panics (~2 minutes). Recovery is a **power cycle** to clear the
latch, then `tools/fw-run.sh`. That would be the most important finding of this
card and is worth the serial log verbatim.

---

**Step 4 - put the bench back.**

The device is running `0.7.1` from `ota_1` with `fw_state invalid` or `aborted`
and `panic.update.outcome` `reverted`, which is correct and is what a device
that has survived two bad updates should say. To get back to a plain 0.7.0:

```bash
tools/fw-run.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw fw-0.7.0-final 25
curl -s -X POST http://workbench.local:8787/api/v1/player/set \
     -H 'content-type: application/json' -d '{"device":"4a00a4","on":true}'
```

Turn the Mac's WiFi back on. Nothing here leaves a background process: every
`curl` is one request and `fw-upload` exits when the trial ends or its own
bound expires.

**Recovery, by case:**

| what | the device is | do this |
|---|---|---|
| an upload refused | untouched, or with a partial image in a slot nothing boots | nothing; designed behaviour |
| `fw-upload` prints `REVERTED` | running the previous image, on the network | read `/api/v1/panic`; that is the test passing |
| `fw-upload` times out waiting for it to come back | probably mid-rollback | wait two more minutes, then `screeny-probe status` |
| unreachable for more than 5 minutes | a crash loop, or a boot that hangs | power-cycle (clears the crash-loop latch and the breadcrumb), then `tools/fw-run.sh` |
| `fw_state` reads `undefined` and uploads answer `unavailable` | running fine, `otadata` unreadable | `tools/fw-run.sh`; report it, it should not happen |
| anything at all, last resort | - | `tools/fw-run.sh`: it erases `otadata` and writes `ota_0`, and always wins |

**What this card does NOT ask the bench to do**: pull the power in the
two-second window between the reply and the `otadata` write (interruption 5b),
or in the ~60 ms of a confirm write. Both are covered by the host model, neither
is a repeatable bench step, and both fail safe by construction.
