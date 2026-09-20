---
id: 211
title: crates/settings - the device's persisted settings, host-tested against the real map
type: build
hardware: no
depends: [200]
owner:
branch:
---

## Goal

The host-testable half of the device's settings store: a `no_std`, no-alloc crate that
owns the key set, the value encoding, the schema version and the load/save logic over
`sequential-storage`'s map, generic over any async NOR flash. The firmware will hand it
the real `screeny` partition (card 212, `hardware: yes`, not this card); the tests here
hand it `sequential-storage`'s RAM mock flash. "One implementation of each thing": when
this lands, nothing else in the repo knows how a setting is laid out in flash.

## Context

Read first: `CLAUDE.md`, `docs/design/device-web.md`, then
`docs/research/006-flash-store-ota.md` **section 7** (the design this card implements)
and section 2 (why buffers must not live in a future), `crates/receiver/Cargo.toml`
and `crates/receiver/src/lib.rs` (the precedent for a `no_std` crate shared by firmware
and host: no I/O of its own, no clock, no allocation),
`docs/board/parked/063-persist-settings.md`, spec sections 6.3 and 8.

Already established by card 200 - do not rediscover:

- `sequential-storage =8.0.1`, `map` API, async-only over
  `embedded_storage_async::nor_flash::NorFlash` (`embedded-storage-async 0.4.1`). Its
  `mock_flash` module is behind the `_test` feature. `MapConfig::try_new`, never
  `new` (it panics on a bad range). Uncached: `cache::Cache::new_uncached()` (check the
  exact path in the 8.0.1 source under `~/.cargo/registry/src/`).
- Keys are `u8`: 0 `SchemaVersion` (u8), 1 `WifiSsid` (<= 32 bytes), 2 `WifiPsk`
  (<= 64 bytes, 0 = open network), 3 `Name` (<= 32), 4 `Brightness` (u8), 5 `IdleMode`
  (u8). Leave room: the orchestrator expects to add keys later (e.g. a boot counter),
  so an unknown key must be ignored on load, not an error.
- The rule for every failure: **any** store error at load -> defaults, reported to the
  caller, never a panic, never an automatic erase. A blank (all-0xFF) partition is not
  an error: it loads as defaults with `wifi: None`.
- An unknown `SchemaVersion` means "ignore the rest, use defaults", and the next save
  writes the current version.
- The PSK invariant of spec 8.4: the PSK is never formatted. The type that holds it
  must not print it through `Debug` (write `Psk(<n bytes>)` or similar) - test that.
- Debounce and write-skipping are part of the design (card 063): a slider sends
  `SET_BRIGHTNESS` ~60 times a second and flash must see a handful of writes a minute.
  The *policy* is pure logic and belongs here, clock-free like `crates/receiver`:
  the caller passes `now_ms`. The task that sleeps and calls it is card 212's.

## Deliverables

1. `crates/settings/` (package `screeny-settings`), `#![no_std]`, no alloc, in the root
   workspace (`members = ["crates/*"]` picks it up). Dependencies limited to
   `sequential-storage`, `embedded-storage-async`, `heapless` and, only if it really
   needs a type from it, `screeny-proto`. It must also build for the firmware target:
   prove it with `cargo check` from `lab/nostd-check`-style or by adding it as a path
   dependency in a scratch check - but do **not** edit `firmware/` (card 212 does that).
   If proving the Xtensa build needs a firmware edit, say so in the Log and stop there.
2. Types: `Settings { wifi: Option<Wifi>, name, brightness, idle_mode }` (shape is
   yours; keep it `Clone + PartialEq`), `Wifi { ssid, psk }` with the non-printing PSK,
   defaults that match today's firmware (`display::DEFAULT_BRIGHTNESS`, idle `STATUS`,
   name empty = "use `screeny-<id>`"). Validation on the way in (lengths, UTF-8 for SSID
   is *not* required - SSIDs are bytes; say what you decided for `Name`).
3. `Store<F: NorFlash>`: `load() -> (Settings, LoadReport)`, `save_wifi`, `clear_wifi`,
   `save_name`, `save_brightness`, `save_idle_mode`, `erase_all` (factory reset), each
   returning a small error enum the firmware can map to `ERR_STORAGE`. Writes skip when
   the stored value already equals the new one. The caller supplies the scratch buffer
   (`&mut [u8]`) - do not put a buffer inside a future or a static; say in the docs how
   big it must be.
4. `Debounce`: pure, clock-free. `note_change(key, now_ms)`, `due(now_ms) ->
   Option<..>` after 3 s of quiet, coalescing repeated changes to the same key; WiFi
   credentials bypass it (they are written immediately, before the device disconnects).
5. Tests against `mock_flash` (16 pages x 4 KB, like the real partition): blank flash ->
   defaults; round-trip of every key; unknown schema version; unknown key ignored;
   oversize values rejected before any write; a store filled by thousands of brightness
   writes still round-trips (exercises page recycling); a write that fails mid-way
   (mock flash can inject errors - check how in 8.0.1) leaves the old value or the new
   one, never garbage, and `load` never panics on a flash full of random bytes (a
   seeded loop of random images is fine; bound it to a second or two); the 60 s
   brightness sweep of card 063's acceptance produces a handful of writes, counted;
   `Debug` of a `Wifi` does not contain the PSK.
6. `crates/settings/README.md`: the flash layout contract in twenty lines, for the next
   person who has to add a key.

## Out of scope

Anything in `firmware/`. The partition table. OTA. HTTP. `crates/proto`,
`crates/receiver` and the spec are shared with the software session and are **not**
touched by this card; if you believe one needs a change, write it in the Log.

## Acceptance

`cargo test -p screeny-settings` green, `cargo test` at the root still green (the pacing
tests in `crates/screeny` are timing-sensitive under load: re-run once before believing
a failure there), `cargo clippy -p screeny-settings` clean, and the crate checks for a
`no_std` target.

## Log
