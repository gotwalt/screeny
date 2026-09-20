---
id: 211
title: crates/settings - the device's persisted settings, host-tested against the real map
type: build
hardware: no
depends: [200]
owner: worker-211
branch: card/211-settings-crate
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

### 2026-09-19, worker-211

**Claimed.** Branch `card/211-settings-crate` off `main` at 4280802, which already
had 2af4e47 (the card) and `docs/research/006-flash-store-ota.md`; no merge needed.

**Read first**, as the card said: `CLAUDE.md`, `docs/README.md`,
`docs/design/device-web.md`, research 006 sections 2, 7 and 9C, spec sections 6.3
and 8, `docs/board/parked/063-persist-settings.md`, `crates/receiver`'s manifest and
module docs, and the `sequential-storage 8.0.1` sources under
`~/.cargo/registry/src/` (`lib.rs`, `map.rs`, `item.rs`, `mock_flash.rs`,
`cache/mod.rs`) rather than trusting memory about the API.

**Built** `crates/settings` (package `screeny-settings`), three modules:

- `value` - `Settings { wifi, name, brightness, idle_mode }`, `Wifi`, `Ssid`,
  `Psk`, `Name`, `SettingError`, `DEFAULT_BRIGHTNESS`.
- `store` - `key::*`, `SCHEMA_VERSION`, `Scratch`, `Store<F: NorFlash>`,
  `LoadReport`, `SchemaState`, `Fields`, `StoreError`, `Write`.
- `debounce` - `Debounce`, `Field`, `QUIET_MS`.

Dependencies exactly as the card allowed: `screeny-proto` (for `IdleMode` and the
three length limits - the store must not be able to hold something the wire could
not carry), `sequential-storage =8.0.1`, `embedded-storage-async 0.4.1`,
`heapless 0.9.3`. Dev-dependency: the same `sequential-storage` with `_test`.

**Decisions the card left to me**

- **`Name` is UTF-8, `Ssid` is not.** A name goes into `screeny-receiver`'s
  `heapless::String<32>` and into the mDNS instance name, so a non-UTF-8 name is
  not representable downstream; a stored one is reported and defaulted. 802.11
  SSIDs are an opaque byte string and plenty of real access points are not UTF-8,
  so refusing them would be a bug. Limits are bytes, not characters.
- **`clear_wifi` writes a zero-length SSID** instead of removing the item.
  `sequential-storage`'s `remove_item` and `remove_all_items` are bounded on
  `MultiwriteNorFlash`, which the ESP32's internal flash is not. Spec 8.2 says
  `ssid_len` is `1..=32`, so empty is unambiguous. Consequence, recorded in the
  README and tested: the old PSK bytes stay physically in flash until that page
  recycles. Spec 8.4 says the PSK is not a secret; `erase_all` is the real reset.
- **`save_wifi`'s commit point is the SSID.** Two keys cannot be written
  atomically, and "new SSID with the old PSK" is a device that silently will not
  join. So the sequence is clear-SSID, write-PSK, write-SSID: a power cut leaves
  the old credentials, none (the device comes up in the portal), or the new ones.
  There is a test that sweeps a shutoff through every byte offset of the write and
  asserts exactly that.
- **`StoreError` is not generic over `F::Error`.** The firmware maps all of it to
  `ERR_STORAGE` (spec 6.5); a `Copy`, comparable enum is what a log line and a
  telemetry field want. The flash's own error value is dropped at the boundary.
- **A save that finds a foreign `SCHEMA_VERSION` erases the range** before writing
  the current version. Those items were written by an encoding this build does not
  know and are already being ignored at load; leaving them would let a later save
  produce a half-v1, half-foreign store. This is the only erase this crate does on
  its own, it is a migration rather than error recovery, and the card's "never an
  automatic erase" rule is about `load`, which never erases.
- **Adding a key does not bump `SCHEMA_VERSION`** - an old build ignores keys it
  does not fetch and a new build reads a missing key as "not set". Written down in
  the README so the next person does not bump it out of politeness.

**The scratch buffer, and the thing worth carrying to card 212**

`sequential-storage` passes *the caller's buffer* straight to `NorFlash::write`
(`item.rs`, `Item::write_new`, line 307: `flash.write(data_address, data_block)`
where `data_block` is a prefix of the buffer). `esp-storage`'s ESP32 backend
requires a word-aligned buffer, and a plain `[0u8; 128]` on the stack is not
guaranteed to be one. So the crate ships `Scratch`, `#[repr(align(4))]`, and the
tests run `mock_flash` with `alignment_check: true`, which panics on an unaligned
write buffer - the check exists in the crate for exactly this reason.

Sizes: the longest item is 1 key byte + the 64-byte PSK = 65, rounded up to the
4-byte word = 68. `SCRATCH_MIN` = 72 (the floor every method checks before
touching flash), `SCRATCH_LEN` = 128 (what the firmware should supply, with room
for a ~120-byte key later). RAM: 128 bytes for the buffer plus `Store`, which is
one `MapStorage` with `Cache::new_uncached()` - no page or key tables, so its own
footprint is the flash handle and a `Range<u32>`. The buffer is the caller's
because research 006 section 2 measured 11,040 bytes of `.bss` appearing the
moment buffers were held across an `await` inside an embassy task.

**Tests: 42, all green** (`cargo test -p screeny-settings`, ~12 s, dominated by the
endurance test). 26 in `tests/store.rs` against `MockFlashBase<16, 4, 1024>` - 16
pages x 4 KB with 4-byte words, the real `screeny` partition's geometry - and 16 in
`tests/policy.rs` for the value types and the debounce. No `futures` dev-dependency:
the mock flash never yields, so a 20-line bounded poll loop with `Waker::noop()`
runs the futures and turns "this hung" into a failed test.

Numbers the card asked to be counted:

- **Card 063's acceptance**: 1800 `SET_BRIGHTNESS` changes over 60 s at 30/s
  become **1 commit, 2 flash write operations, 0 erases**. One commit rather than a
  handful because a continuous drag never goes quiet until it ends - which is the
  intent. A six-separate-drags variant gives exactly 6 writes.
- **Endurance**: 6000 differing brightness saves = **12,039 flash writes and 3 page
  erases**, and every key still round-trips afterwards, including a Wi-Fi credential
  written into an already-recycling store and migrated across a recycle.
- **Mid-write failure**: `mock_flash`'s `bytes_until_shutoff` (a one-shot countdown
  in byte-operations, set on the flash handle) swept from 1 to 219 for a name write
  and 1 to 259 for a Wi-Fi write. After every cut the store is reopened and loaded:
  the name is "before" or "after" and never anything else, an unrelated key is
  untouched, and the Wi-Fi is old, absent or new but never a mixed pair.
- **Garbage**: 120 rounds of a seeded-xorshift-random 64 KB partition. `load` never
  panics and never returns a `Settings` the types could not hold.

**`no_std` check**: `cargo check -p screeny-settings --target thumbv7em-none-eabi`,
which was already installed (`rustup target list --installed`). Clean. No Xtensa
toolchain was sourced and `firmware/` was not touched. This also confirms the
feature-unification worry was unfounded: `sequential-storage`'s `_test` feature
pulls in `std`, but with `resolver = "2"` a dev-dependency's features are not
unified into the non-test build, so the library still compiles as `no_std`.

**Research doc 006 section 7, checked against the 8.0.1 source.** Everything in it
held: async-only over `embedded_storage_async::nor_flash::NorFlash`,
`MapConfig::try_new` (and `new` really does `panic!` at `map.rs:35`),
`Cache::new_uncached()`, `u8` keys, `mock_flash` behind `_test`, `Error::LogicBug`
existing so the crate returns instead of panicking, `run_with_auto_repair!`. Four
things it does not say that a build card needs:

1. **`MapStorage` owns the flash.** 8.0.1's API is
   `MapStorage::new(flash, config, cache)` and then `storage.fetch_item(buf, &key)`;
   the flash, range and cache are no longer passed per call. `destroy()` gets the
   flash back.
2. **The caller's buffer is handed to `NorFlash::write`**, so it must be word
   aligned. Nothing in section 7 mentions alignment, and it is the one way this
   could have worked on the host and failed on the device.
3. **`remove_item` requires `MultiwriteNorFlash`**, so "erase a key" is not
   available on ESP32 internal flash. Hence the empty-SSID marker.
4. Section 7 says `Error::ItemTooBig` is unreachable, which is right, but the
   reachable buffer error is `Error::BufferTooSmall(needed)` from `item.rs:139`
   when a *stored* item is longer than the buffer - that is what a too-small
   scratch actually produces, and it is mapped to `StoreError::Scratch`.

Section 7's line numbers were also spot-checkable and correct (`map.rs:30` for the
panicking `new`, `lib.rs:634` for `run_with_auto_repair!`, `lib.rs:525-531` for
`LogicBug`).

**Proposed follow-up cards** (my range, 213-219; none of them done here):

- **213 - `firmware/src/display.rs` should take `DEFAULT_BRIGHTNESS` from
  `screeny-settings`.** The constant 96 is now in two places. One line in card 212's
  territory, which is why it is not done here.
- **214 - add `crates/settings` (and `crates/receiver`, `crates/encode`,
  `crates/panel`) to the layout table in the root `README.md`.** The table has
  drifted; out of this card's scope.
- **215 - consider one `WIFI_CREDENTIALS` item instead of keys 1 and 2.** A single
  CRC-protected item would make a credential change genuinely atomic instead of
  three-step. It needs a schema bump and a change to the key table research 006
  section 7 fixed, so it is a decision, not a tidy-up.
- **216 - a `GET_SETTINGS` / settings-page view that does not go near the PSK.**
  `LoadReport` and `Settings` are shaped for it, but nothing consumes them yet;
  worth a card so the HTTP work (201) has a defined source.

**Acceptance, all four**

- `cargo test -p screeny-settings`: 42 passed, 0 failed (16 policy + 26 store).
- `cargo test` at the root: exit 0, 56 suites, 0 failures, pacing tests included.
  No re-run was needed - it was green first time, twice.
- `cargo clippy -p screeny-settings --all-targets -- -D warnings`: clean.
- `cargo check -p screeny-settings --target thumbv7em-none-eabi`: clean.
- Also `cargo fmt -p screeny-settings` (the rest of the workspace is not
  rustfmt-clean, so nothing outside this crate was reformatted).

**Nothing outside scope was touched**: `crates/settings/**`, the card file, and the
root `Cargo.lock` (8 new locked packages, of which 4 are optional features of
`sequential-storage` that are never built). No `firmware/`, no `crates/proto`, no
`crates/receiver`, no spec, no `README.md`. No serial port, no flash, no camera, no
background processes left behind.
