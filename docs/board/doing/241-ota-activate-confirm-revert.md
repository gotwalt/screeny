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
