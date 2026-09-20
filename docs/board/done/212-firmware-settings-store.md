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

### What was built

`firmware/src/store.rs` (829 lines) is the device half of `crates/settings`.
`crates/settings` itself is **unchanged** - it needed no fix and got none.

- **The partition table is read by a non-`async`, `#[inline(never)]` helper**
  (`find_partition`). That is the whole trick for keeping card 200's 11 KB
  accident from happening again: its 3072-byte buffer sits on a real stack frame
  that is gone before the caller's first `await`, so it cannot become part of a
  task future and therefore cannot become `.bss`. All that survives is the
  32-byte `PartitionEntry` - which is also the only way the crate lets you get
  back to a region later, since `FlashRegion`'s fields are `pub(crate)` and
  there is no "build me a region from this offset and length".
- **`Store` is built per operation, not held.** A `NorFlashRegion` borrows a
  `FlashRegion` which borrows the `FlashStorage`, so a long-lived `Store` would
  be a self-referential struct. Building one is a range check and a struct
  literal (the map's cache is `new_uncached`), so this costs nothing.
- **The "command channel" is one `AtomicU8` bitmask plus one `Signal`** - 1 byte
  and ~12 bytes of `.bss`. A `Channel<Cmd, 4>` would have cost ~4x the command
  size *and* its `try_send` would return `Full` in exactly the case where losing
  the notification is unacceptable, because `note_dirty` is called from inside
  the receiver's synchronous control handler and cannot block or fail. Two
  changes to the same field coalesce, which is what the debounce wants anyway.
- **Immediate vs debounced.** `SET_BRIGHTNESS` and `SET_IDLE` are debounced (3 s
  of quiet, `screeny_settings::Debounce`) and their replies cannot report
  `ERR_STORAGE`; a failed commit is logged and counted in `store::FAILURES`,
  where card 222's status page can read it. `SET_NAME` and `SET_WIFI` are
  written by `control_task` **before** it sends the reply, which is the only way
  `ERR_STORAGE` can be honest; on failure the reply is rewritten as
  `ERR_STORAGE` from the request's own opcode and `req_id`.
- **Lock order.** Both `store_task` and `control_task` take `CORE` first,
  release it, then take `STORE`; neither ever holds both. The store task reads
  the live value (the `BRIGHTNESS` atomic, the receiver's `Core`) rather than
  keeping a copy, so flash cannot disagree with the panel, and it drops the
  `CORE` guard before the flash call so a sector erase is not also a lock hold.
- **A counting `NorFlash` wrapper** sits under `BlockingAsync`, so "did this
  write erase a sector?" is answered by the erase call itself rather than
  guessed from a duration. It also gives card 063's acceptance ("a 60 s
  brightness sweep costs a handful of writes") a number to read.
- **`spike-ota` keeps its OTA half and loses its settings half** - the real
  store replaces it. It now borrows the store's `FlashStorage` because
  `FlashStorage::new` panics if called twice.
- `display::DEFAULT_BRIGHTNESS` is now an alias of
  `screeny_settings::DEFAULT_BRIGHTNESS`, as `crates/settings/README.md` asked.
  `FW_VERSION` -> `0.3.0`.

### `bench-wifi`, and proving that "off" means off

`build.rs` checks `CARGO_FEATURE_BENCH_WIFI` and returns before it looks at the
environment or either `wifi.env`, so with the feature off nothing is read. The
`SSID`/`PASSWORD` constants are `#[cfg(feature = "bench-wifi")]`, so in a default
build there is no name by which a credential could reach the binary.

Checked, not just intended. With the **dummy** pair `Example-Wifi1` /
`password9` exported in the environment:

| build | occurrences of the dummy SSID in the ELF |
|---|---|
| `cargo build --release` | **0** |
| `cargo build --release --features bench-wifi` | 1 |

### Flash 1 - `bench-wifi,store-selftest` (`captures/card212-seed-selftest.log`)

Booted, joined, 30 fps rx / 30 fps shown, zero drops. But **every store call
returned `Corrupted`**, including the seed:

```
store: 'screeny' partition at 0x410000, 65536 bytes (16 pages)
store: loaded schema Unreadable fallback 0b1111 error Some(Corrupted) | ...
store: seeding failed: Corrupted
selftest: counters before the reboot | commits 0 skips 0 failures 5 ...
```

Diagnosis: the `screeny` partition has **never been written**. 0x410000 is
inside the region the stock Tidbyt image used, so the bytes there are neither
`0xFF` nor a `sequential-storage` map, and the crate rightly refuses them. This
is precisely the case the card describes: *a wrong settings partition is fixed
in code with `erase_all`, not with `espflash erase-*`.*

Worth recording that the failure was clean: `load` fell back to defaults, the
device carried on, the panel stayed lit and the stream stayed at 30 fps with a
dead store. That is the "never fails, never erases by itself" contract working.

### The repair, and why it is narrow

`store::repair` fires **only** on `SchemaState::Unreadable` + `StoreError::Corrupted`
- the load could not read even the schema-version item, *after* `sequential-storage`
had already re-run the operation through its own `run_with_auto_repair!` pass. A
partly damaged map does not look like that; it comes back as defaults plus a
`fallback` bitset and is left alone. Once per boot, before the panel is up, and
nothing in the path reboots, so it cannot become a loop.

### Flash 2 - the same features, with the repair (`captures/card212-repair-selftest.log`)

```
store: the 'screeny' partition does not hold a settings map ... Erasing it once, now.
store: erase_all took 192917 us, 16 sectors
store: loaded schema Blank fallback 0b1111 error None | name "" brightness 96 idle 0 | wifi none
store: seeded the empty store from the build's credentials (Committed, 3452 us, 0 erases)
```

**Per-write timings, measured on the device, mid-stream at 30 fps:**

| write | result | duration | sector erases |
|---|---|---|---|
| `erase_all` (64 KB, one-off repair) | - | 192,917 us | 16 |
| seed credentials (SSID + PSK, 2 items) | Committed | 3,452 us | 0 |
| `save_brightness` | Committed | 1,772 us | 0 |
| `save_idle_mode` | Committed | 981 us | 0 |
| `save_name` | Committed | 1,306 us | 0 |
| `save_brightness`, same value again | **Skipped** | 647 us | 0 |
| `save_name` (restore, pass 2) | Committed | 6,575 us | 0 |
| `save_idle_mode` (restore, pass 2) | Committed | 1,250 us | 0 |

Two surprises, both in our favour. **A single-item write costs ~1-7 ms, not the
~8 ms research 006 estimated**, because `sequential-storage` appends a ~4-70
byte item into an already-erased page - it does not rewrite a 4 KB page. And a
**sector erase is ~12 ms, not ~50 ms**: 16 sectors in 193 ms. So the worst case
this card was told to worry about is about four times cheaper than budgeted. No
write in the whole run needed an erase at all; with 16 pages for a record of
under 200 bytes, page recycling is a long way off.

**What the panel and the stream did through the writes.** The telemetry line
immediately after the four-write burst:

```
telemetry: 30 fps rx, 30 fps shown, 155 swaps/s | drops stale 0 superseded 0
  decode 0 rejected 0 gaps 0 | ia 33680 us jit 1162 us | decode 614 us (max 2227)
  | render 3097 us (max 3207 window, 3962 boot) | state 1 codec 0x10 ...
```

`render` max over the whole burst was **3,243 us** against a ~3,100 us steady
state - a 150 us blip, invisible. The costlier pair (the two pass-2 restores,
6.6 ms + 1.3 ms back to back) did show up, and it is worth quoting because it is
the honest worst case:

```
telemetry: 30 fps rx, 30 fps shown, 154 swaps/s | drops stale 0 superseded 1
  decode 0 rejected 0 gaps 0 | ia 32947 us jit 9142 us | ... render 3086 us (max 7681 window ...)
```

One frame **superseded**, zero **decode** drops, still 30 fps shown. That is
"drain the socket, newest wins" doing its job: a frame arrived while core 0 was
inside the ROM write and a newer one replaced it. Core 1's first render after
the unpark took 7.7 ms instead of 3.1 ms - one late refresh, no corrupt frame.
Research 006's "do nothing special for a settings write" holds.

**Settings survived the reboot** (the self-test's own verdict):

```
store: loaded schema Current fallback 0b0000 error None | name "selftest-212" brightness 111 idle 2 | wifi stored
selftest: PASS 2, after the reboot - loaded name "selftest-212" brightness 111 idle 2; pass 1 wrote "selftest-212" 111 2
selftest: all three settings survived the reboot: YES
```

and the telemetry after that boot reported `bright 111`, so the stored value
reached the panel and not just the log. mDNS announced
`selftest-212._screeny._udp.local` from the stored name, and the Studio kept
streaming at 30 fps through the rename - so it is not discovering by instance
name.

### Flash 3 - the DEFAULT build (`captures/card212-default-from-store.log`)

The acceptance the owner asked for. This binary contains no credentials at all:

```
store: loaded schema Current fallback 0b0000 error None | name "" brightness 111 idle 0 | wifi stored
wifi: connected ...
mdns: screeny-4a00a4.local -> 192.168.7.221 as screeny-4a00a4._screeny._udp.local port 49374 (10 txt keys)
telemetry: 30 fps rx, 30 fps shown, 155 swaps/s | drops ... decode 0 ... | bright 111 | heap 45540/98304
```

It joined from flash alone, and `brightness 111` - written by the self-test two
flashes earlier - came back, so a setting survives a **reflash** as well as a
reboot (`fw-run.sh` never erases the `screeny` partition). The name and the idle
mode are back to their defaults because pass 2 restored them on purpose; the
brightness was deliberately left as the marker. **Set it back to 96 if you want
the default.**

### Sizes

| | before | after (default build) | delta |
|---|---|---|---|
| `.rwtext` | 11,812 | 15,124 | +3,312 |
| `.data` | 56,124 | 56,848 | +724 |
| `.bss` | 102,424 | 106,712 | +4,288 |
| `.stack` | 37,512 | 32,504 | **-5,008** |
| `.text` | 531,237 | 569,697 | +38,460 |
| `.rodata` | 73,176 | 78,816 | +5,640 |
| flash image | 768,176 | 816,320 | +48,144 |

The image grew by **48,144 bytes**, which is research 006's ~48 KB budget almost
exactly.

`.stack` fell by 5,008 bytes, well over the card's 2 KB tripwire, so I went and
found out why before going on. `.stack` is a remainder - what the linker has
left between `_bss_end` and 0x3ffe0000 - so it falls by exactly what `.data` and
`.bss` gained. `xtensa-esp32-elf-nm` on both ELFs accounts for every byte:

| symbol | delta | what it is |
|---|---|---|
| `__embassy_main::POOL` | +1,176 | `store::init` and the loaded `Settings` held across `main`'s awaits |
| `net::control_task::POOL` | +1,000 | the `Immediate` (a `Wifi` is ~100 bytes) and the immediate-write future |
| `wifi_task::POOL` | +936 | `active` / `builtin` / the new pair, ~100 bytes each |
| `store::store_task::POOL` | +800 | the new task |
| `store::STORE` | +184 (`.data`) | `FlashStorage` + the 32-byte entry + the 128-byte `Scratch` |
| `NEW_WIFI` | +108 | `Signal<Wifi>` |
| `CURRENT_SSID` | +36 | the 32-byte `GET_WIFI` buffer + its length |
| the six counters, `DIRTY`, `WAKE` | +33 | |

**No 3 KB partition-table buffer appears anywhere**, which is the thing the card
warned about, and the `Scratch` buffer is in a named static rather than inside a
future. It is more than the "few hundred bytes" the card budgeted, but it is
four task futures and one static, all itemised, none of it a stray buffer.

The measured number is the one that matters, and there is plenty of room:

```
before:  stack: core 0 main high-water  6000 of 37512 bytes, 30488 free (painted at boot)
after:   stack: core 0 main high-water 10688 of 32504 bytes, 20792 free (painted at boot)
```

The +4,688 bytes of *depth* is `find_partition`'s 3 KB buffer plus the
`esp-storage` / `sequential-storage` call chain under it - which is exactly
where that buffer was moved to on purpose. 20.8 KB of headroom remains.

### Flashes: three, and why each

1. `card212-seed-selftest` - `bench-wifi,store-selftest`, the first real build.
   Found the `Corrupted` partition.
2. `card212-repair-selftest` - the same features plus `store::repair`. The
   evidence run: repair, seed, four timed writes, reboot, reload, restore.
3. `card212-default-from-store` - the **default** build, no features. The
   acceptance: joins from the store alone with no credentials compiled in.

No build failed to boot, so the known-good image was never needed. No monitors,
servers or emulators were started beyond `fw-run.sh`'s own, each wrapped in
`timeout 400` with `secs` of 150, 150 and 120. `pgrep -fl espflash` is empty.

### Left for the orchestrator (needs the LAN, which a worker cannot reach)

`SET_NAME`, `SET_BRIGHTNESS`, `SET_IDLE`, `GET_WIFI`, `SET_WIFI` and the
`ERR_STORAGE` path have **never been exercised over the wire** - only the store
calls underneath them have. The acceptance list at the top of this card is
unchanged and is all still to do.

### Orchestrator, over the wire after the merge (2026-09-20)

- Default build from `main`: **0 occurrences** of the real SSID and of the real password
  in the ELF (counted with `grep -c`, values never printed).
- `SET_NAME bench-212`, `SET_IDLE 1`, `SET_BRIGHTNESS 77`, `REBOOT`: `GET_INFO` came
  back with `name=bench-212`, telemetry with brightness 77, and the boot log with
  `loaded ... name "bench-212" brightness 77 idle 1 | wifi stored`. `fw=0.3.0`.
- Brightness sweep: **4,098 `SET_BRIGHTNESS` in 60 s -> 1 flash commit** (2.3 ms, 0
  erases), stream at 30 fps with 0 drops throughout. Card 063's acceptance, met.
- `SET_WIFI` with the dummy pair, not persisted: 3 attempts (`NoAccessPointFound`),
  fallback to the stored network, `GET_WIFI` state 3 (`FAILED`) and sticky, stream back.
  The PSK appears 0 times in the serial log. **Found and fixed:** the reply never
  reached the sender ("no reply to op 0x0b") because `send_to` only queues the datagram
  and the rejoin started at once; the control task now waits 100 ms first, as `REBOOT`
  already did. Re-tested: `ok (Empty)`.
- `screeny-probe` gained `set-wifi SSID PSK [--persist]` for this (bench only, not part
  of `conformance`).
- Conformance: 60 passed, 0 failed, 4 skipped. (A first run read 25/35/4 because the
  Studio was still streaming: its `set_panel` switch no longer stops the stream, the
  per-device `player/set` does. Recorded in `docs/design/device-web.md`.)
- Name, idle mode and brightness restored to `screeny-4a00a4`, 0, 96.
- Follow-ups accepted from the worker's list: mDNS rebind after an address change, the
  failure screen naming the SSID (folds into card 223), store counters on the status
  page (card 222), a factory-reset path (the button's 15 s hold, card 231; an opcode
  needs a spec change and waits).
