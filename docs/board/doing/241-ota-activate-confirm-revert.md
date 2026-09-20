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
