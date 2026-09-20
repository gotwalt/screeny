---
id: 212
title: Firmware - settings live in flash: loaded at boot, saved with debounce, SET_WIFI and stored credentials real
type: build
hardware: yes
depends: [210, 211, 220]
owner: worker-212
branch: card/212-firmware-settings-store
---

## Goal

The device remembers things. `crates/settings` (card 211) on the `screeny` partition
(card 210): name, brightness and idle mode survive a reboot (this delivers parked card
063), WiFi credentials come from the store first and the compile-time ones second, and
`SET_WIFI` actually stores and joins. No portal, no HTTP, no button yet - those cards
build on this one.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md` (decisions 5-7, the storage/RAM
section, "What the research settled -> Flash, store, OTA"); `crates/settings/README.md`
and `crates/settings/src/store.rs` (the API you are wiring in; its report in
`docs/board/done/211-settings-crate.md`); `docs/research/006-flash-store-ota.md`
sections 4 and 7; `docs/research/009-ram-headroom.md` (card 220: the heap split and the
stack numbers you must stay inside); `firmware/src/spike_ota.rs` (card 200's
compile-only proof of the partition-table / `FlashStorage` / `BlockingAsync` calls at
our pinned versions: evidence, not code to keep - delete it and the `spike-ota`
feature's store half when the real thing exists, or leave the feature building; say
which); `firmware/src/receiver.rs` and `crates/receiver/src/lib.rs` (`Params`,
`Event::{Renamed, ...}`, `Host::set_wifi`, `Host::wifi`); `firmware/src/main.rs`
(`wifi_task`, the credentials constants); `firmware/build.rs`; spec sections 6.3, 6.5
and 8; `docs/board/parked/063-persist-settings.md`.

Already established - do not rediscover:

- **Every flash write must park core 1**: `FlashStorage::new(..).multicore_auto_park()`
  and `esp-storage`'s `critical-section` feature. Never `multicore_ignore()`. **Only
  core 0 touches flash.** A settings write is one item (~8 ms) and occasionally a 4 KB
  sector erase (~50 ms): the circular DMA keeps the panel lit and only the dither phase
  freezes. Do nothing special for it, but *measure it* (below).
- The scratch buffer is `screeny_settings::Scratch` (128 bytes, 4-byte aligned - an
  unaligned buffer passes on the host and fails on the ESP32). It and any partition-table
  buffer must **not** live across an `await` inside a task future (that is `.bss`, and
  `.bss` comes out of core 0's stack - card 200 lost 11 KB that way). Read the partition
  table once at boot with a heap buffer that is dropped, or on `main`'s stack before the
  tasks spawn; keep only the `screeny` partition's offset and length.
- `crates/settings` cannot delete keys (ESP32 flash is not `MultiwriteNorFlash`):
  "no credentials" is a zero-length SSID, handled inside the crate. `load` never fails
  and never erases; any store error at boot means defaults + a logged report, and the
  device carries on.
- `Params` already takes `name`, `brightness` and `idle_mode`, so seeding the receiver
  from `Settings` needs no change to `crates/receiver`. `Host::wifi()` returns
  `&'static str`: serve it from a static buffer you fill at join time (lossy UTF-8 for a
  non-UTF-8 SSID is fine and should be said in a comment). **`crates/proto`,
  `crates/receiver` and the spec are shared with the software session: do not change
  them.** If you are sure one needs a change, stop and tell the orchestrator.
- Card 220 landed: the framebuffers are `ConstStaticCell`s, core 0's measured stack
  high-water is ~6 KB of 37.5 KB (`stack: core 0 main high-water ...` is logged once at
  60 s - quote it before and after your change), the heap split stays 64 + 32 KB, and
  `station_config()` / `station_loop()` are now separate functions in `main.rs`.
  `station_config()` sets `ScanMethod::AllChannels` so the station joins the strongest
  node of the owner's mesh instead of the first to answer: **keep that** in whatever
  builds a `StationConfig` from stored credentials.
- Decisions: stored credentials are tried first (3 attempts), then the compile-time
  ones if the build has any, and a build **without** compile-time credentials must now
  compile and boot (`firmware/build.rs` makes them optional; with none and an empty
  store the device shows the existing "wifi failed"-style idle screen and keeps
  retrying nothing - the portal is card 223). Compile-time credentials, when present,
  **seed an empty store on first boot** so a bench flash comes straight up and the store
  path is exercised from then on.
- `SET_WIFI` (spec 8.2): reply first, then store if `persist`, then disconnect and try
  the new network 3 times, then fall back per 8.3 to the previous stored credentials and
  set the `GET_WIFI` state to `FAILED`. `esp-radio 1.0.0-beta.1`'s runtime reconfigure
  API is in `docs/research/007-device-web-and-portal.md` section 2.1. The PSK is never
  logged, never returned, never drawn (spec 8.4) - including in a `Debug` of whatever
  you pass between tasks.
- `ERR_STORAGE` (spec 6.5) can only be honest for writes that happen before the reply:
  `SET_NAME` and `SET_WIFI` write immediately and return it on failure. `SET_BRIGHTNESS`
  and `SET_IDLE` are debounced (3 s of quiet, `screeny_settings::Debounce`), so their
  reply cannot know; a failed debounced commit is logged and counted, and the count is
  kept where card 222's status page can read it.
- You cannot reach the device over the LAN from a worker environment. Your evidence is
  the serial log; `SET_*` commands must be sent by the orchestrator. So: build a tiny
  **bench feature** (`store-selftest`, off by default) that, at boot + 20 s, exercises
  the store on the device by itself - save name/brightness/idle through the same code
  path the control handlers use, log per-write duration in microseconds and whether a
  sector erase happened, log `render` max over the window, reboot once (guarded by a
  marker key or RTC flag so it cannot loop), and print what was loaded after the reboot.
  The orchestrator does the over-the-wire acceptance (below) after merging.
- The only flashing path is `/Users/aaron/src/screeny/tools/fw-run.sh <elf> <name>
  [secs]` by that absolute path. Its log (`/Users/aaron/src/screeny/captures/<name>.log`,
  git-ignored) **contains the real SSID**: never copy that line, the SSID or any
  credential anywhere. Tests and examples use `Example-Wifi1` / `password9`.

## Deliverables

1. `firmware/src/store.rs` (or similar): open the `screeny` partition, `load` at boot
   **before** the receiver core and the first composed frame, the store task on core 0
   (debounced commits, immediate commits, a small command channel - size it and say what
   it costs), failure counting.
2. Boot wiring: `Settings` -> `Params` (name, brightness, idle mode) and the
   `BRIGHTNESS` atomic; mDNS announces the stored name from the first announcement.
3. WiFi: credentials from the store, then compile-time; seeding; `SET_WIFI` end to end;
   `GET_WIFI` reports the SSID actually in use. `build.rs` makes credentials optional.
4. The `store-selftest` bench feature and one run of it on the device.
5. Sizes: `xtensa-esp32-elf-size -A` before and after (`.bss`, `.stack`, `.rwtext`,
   image size) in the Log. The budget from research 006 is ~48 KB flash, a few hundred
   bytes of `.bss`. If `.stack` falls by more than 2 KB, find out why before going on.
6. `FW_VERSION` -> `0.3.0`. Leave the device running the default build of your branch.

## Bench discipline (orders)

Iterate on the host (build, sizes) first; expect 3-6 flashes; write in the Log why each
happened. `fw-run.sh` bounds its own monitor (secs <= 200; wrap in `timeout 400`). No
other monitors. If a build does not boot, reflash the last good build before diagnosing.
`pgrep -fl espflash` must be empty when you finish. Never erase flash by hand: a wrong
`screeny` partition is fixed with `erase_all` in code, not with `espflash erase-*`.

## Acceptance

- Worker: selftest log shows values surviving a reboot, per-write timings, no panic, the
  stream (the Studio reconnects by itself ~15 s after boot) at ~30 fps with zero decode
  drops through the writes; default build unaffected by the feature; sizes reported.
- Orchestrator, after merge, over the wire: set a name, idle mode and brightness,
  `REBOOT`, and `GET_INFO` / `TELEMETRY` / mDNS come back with them; a 60 s brightness
  sweep at 30 changes/s produces a handful of flash writes (counted in the log);
  conformance stays 60/0/4 (the suite restores what it changes, so it now also
  exercises the debounce); `SET_WIFI` with wrong credentials falls back and reports
  `FAILED`.

## Log

### Baseline, before any change (worker-212)

Branch `card/212-firmware-settings-store`, based on `5525f10` (which contains
`0d3e952`, so `crates/settings`, `crates/provision`, research 006-009, card 220's
`ConstStaticCell` framebuffers and the rollback bootloader are all present).

`xtensa-esp32-elf-size -A` on the unmodified default build:

| section | bytes |
|---|---|
| `.rwtext` | 11812 |
| `.data` | 56124 |
| `.bss` | 102424 |
| `.stack` | 37512 |
| `.text` | 531237 |
| `.rodata` | 73176 |

Flash image (`espflash save-image --flash-size 8mb --partition-table
firmware/partitions.csv`): **768,176 bytes** of the 2 MB `ota_0` slot (36.63%).

The 60 s stack line, **before**, quoted from the bench log of the build this branch
is based on (`captures/card226-scan-all-channels.log`, i.e. commit `0d3e952`,
flashed by the orchestrator) rather than from a flash of my own - the sources are
byte-identical, so a flash to re-measure an unchanged build would have been one of
my six for nothing:

```
stack: core 0 main high-water 6000 of 37512 bytes, 30488 free (painted at boot)
```

A copy of that ELF is kept outside the worktree as the known-good image to reflash
if one of my builds does not boot.

### Owner decision 2026-09-20, relayed by the orchestrator

Compiled-in WiFi credentials are going away; they survive only as an explicit bench
override behind a new cargo feature `bench-wifi`, **off by default**. With the
feature off `build.rs` must not even look for credentials and the constants must not
exist, so any use of them fails to compile. With it on they (a) seed an *empty* store
at boot through the normal store path and (b) are the spec 8.3 step-2 fallback after
stored credentials have failed three times. A default build has no step 2: stored
credentials, then the failure idle screen with periodic retries (the portal is card
223). This matches `crates/provision`'s `has_builtin: false` model. The bench flow is
therefore: flash a `bench-wifi` build once to seed the store, then flash the **default**
build and watch it join from the store alone - which is also the card's acceptance and
the state the device is left in.
