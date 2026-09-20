---
id: 240
title: Firmware - OTA staging: stream an upload into the inactive slot, validate it, and only then let it be booted
type: build
hardware: orchestrator flashes and uploads (the worker builds, host-tests and writes the bench procedure)
depends: [210, 242, 243, 236]
owner: worker-240
branch: card/240-ota-staging
---

## Goal

The owner chose **the full plan of `docs/research/006-flash-store-ota.md`** for OTA
(decision 10 in `docs/design/device-web.md`). This card is its first half: **getting a new
image safely into the inactive slot**. `POST /api/v1/firmware` (route, reply shape and
error codes already exist in `crates/device-api`, and the simulator already serves it)
streams a raw `application/octet-stream` body into the inactive OTA slot, never buffering
it, validates it with research 006 section 5's checks, and answers. **This card does not
switch the boot slot on its own initiative beyond what 006 says staging does** - activate /
confirm / revert, the health criterion and the "updating" screen's later states are card
241. If 006's split between the two cards is different from this sentence, **006 wins**;
say so in the Log.

## Context (read first, in this order)

- `docs/research/006-flash-store-ota.md`: **Conclusions**, section 4 (flash writes with the
  display on core 1: the two rules the build cards must not break, and the open risk -
  radio vs ~12 ms sector erases), section 5 (the API, what `esp-bootloader-esp-idf` gives
  and does not give, streaming the upload, every interruption state), section 6 (rollback,
  the app-side half), section 9 (the proposed build cards - this is 240 there too).
- `docs/design/device-web.md`: decisions 3, 7, 9, 10; "How to think about storage and RAM"
  (`.bss` is core 0's stack; nothing large across an `await`); the lessons list - in
  particular **never write flash inside an HTTP handler** (how does 006 reconcile that
  with a streamed upload? the store's `deferred_task` is the existing pattern; an upload
  needs its own answer, written down) and the card 243b lesson: **every byte added to a
  reply type that `Reply` carries is paid ~12x in the handler future** - check
  `FirmwareReply`'s size against `StatusReply`'s before and after.
- `docs/design/protocol-v1.md` 8.6: the firmware route is *reserved*; this card specifies
  it there (normative prose for what you build, nothing more).
- Code: `firmware/src/http.rs` (the dispatch, `BoundedSocket` and the accept loop from
  card 236, per-route body limits, `raw_body`), `firmware/src/store.rs` (`Flash`, `Parts`,
  `read_partitions`, how flash writes are serialised with the panel), `firmware/src/spike_ota.rs`
  (the research spike), `firmware/src/panic.rs` (the breadcrumb 241 will read),
  `crates/device-api` (`FirmwareReply`, `FirmwareError`, `route::FIRMWARE`),
  `crates/sim/src/api.rs` (the simulator's upload), `crates/probe/src/http/rules.rs`
  rules 23-24 (skipped on the device today: "this firmware does not serve that route yet").
- The image format facts: this project's images are built by `espflash`; the bootloader is
  ESP-IDF v6.1 with app rollback (`firmware/bootloader/README.md`).

## Steps

1. Design note in the Log first (short): where the bytes go from socket to flash, who owns
   the flash while an upload runs, what the frame path and the panel do meanwhile
   (decision 7 allows an "updating" screen to take the panel over; dither off per 006),
   what a second concurrent upload gets, what a stalled uploader costs (a worker is pinned:
   bound it), and what each abort leaves behind. Then build it.
2. The streaming writer: sector-aligned, erase-as-you-go or erase-ahead per 006, no buffer
   bigger than 006 allows, RAM numbers in the Log (`tools/fw-size.sh`: `.stack` >= 24,576
   on all builds, 27,232 now; say what the upload costs in `.bss` and in heap).
3. The validator: all five checks of 006 section 5, each with its own `FirmwareError` and a
   host test against real and deliberately broken images (a truncated image, a wrong chip,
   a non-screeny app, a bad checksum/hash, an image larger than the slot).
4. Instrument the per-sector timing (erase and write, min/avg/max) and what the radio did
   meanwhile (frames dropped, WiFi disconnects, `esp_wifi` errors), **logged once per
   upload, not per sector**, and reported in `FirmwareReply` only if it fits without
   growing the type. This is 006's open risk; the orchestrator measures it on the device.
5. The "updating" screen while an upload is in flight (owned by `crates/provision`'s screen
   module or a sibling - one implementation), and the panel/stream behaviour around it.
6. Unskip probe rules 23-24 for the device, add rules for the new refusals that are safe to
   run against a real device (nothing that leaves a bootable-but-wrong image active), keep
   the simulator and the device answering the same. A probe subcommand or documented
   `curl` line that uploads an image file, for the orchestrator.
7. `FW_VERSION` -> "0.6.0". Spec 8.6/8.7: specify the route.

## Exit

- All firmware builds clean and over the `fw-size.sh` floor; `timeout 1200 cargo test`
  green; numbers in the Log.
- A **bench procedure** for the orchestrator, bounded and ordered: what to flash, the exact
  upload commands (a good image, then each bad image), what serial lines and replies to
  expect, how to read the per-sector timing, what would mean "stop" (e.g. WiFi drops during
  erases), and how to get back to a known-good device in every case (serial flash with
  `tools/fw-run.sh` always wins: it erases `otadata`).
- The ELF in the scratchpad as `screeny-fw-0.6.0-default.elf`, and a second, distinguishable
  build to upload (e.g. the same tree with a version suffix) as
  `screeny-fw-0.6.0-upload.bin` in whatever form the route takes - say which form and how
  you made it.

## Rules

Branch `card/240-ota-staging` from current `main`, in your own worktree; commit and Log as
you go, by explicit path; do not merge, do not push. **You do not touch hardware**: no
flashing, no serial port, no LAN, no camera - the orchestrator runs your bench procedure.
Bounded commands only; nothing left running. Never write a real SSID or password
(dummies `Example-Wifi1` / `password9`; never quote `captures/` lines containing an SSID).
Leave `crates/art` and `crates/studio` alone; `crates/device-api`, `crates/sim` and
`crates/probe` are shared with the `software` session - list every change to them in your
report. **Never erase or write outside the inactive app slot and `otadata`**; the settings
partition and the running slot are untouchable, and a test must prove the writer refuses
an offset outside its slot.

## Log
