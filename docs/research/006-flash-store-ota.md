# 006 — Flash layout, a settings store, and an OTA that cannot brick the device

Card 200. Research only; nothing here touched the hardware. Every crate claim cites a
file and line in `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` at the version
`firmware/Cargo.lock` pins, or in `espflash 4.6.0`'s own sources
(`static.crates.io/crates/espflash/espflash-4.6.0.crate`), or in ESP-IDF `release/v6.1`.

---

## Conclusions

1. **A partition table is required and is the easy part.** Five entries, no `factory`, no
   coredump: `nvs`, `otadata`, `ota_0`, `ota_1`, `screeny`. Two 2 MB app slots on an 8 MB
   chip; today's image is 743,408 bytes, 35% of a slot. The CSV is in §3.

2. **A serial flash does *not* automatically beat a stale OTA slot, and that is a trap.**
   `espflash` writes exactly three things — bootloader, partition table, app — and picks
   the app partition as `find("factory")` else the first `App` entry. It never touches
   `otadata`. With no `factory` in the table, `espflash flash` overwrites `ota_0` while
   `otadata` may still select `ota_1`. `tools/fw-run.sh` must pass
   `--erase-data-parts ota` **on every flash**. Then a serial flash always wins.

3. **`esp-bootloader-esp-idf 0.6.0` gives us everything on the app side**: read the
   table, read/write `otadata` with the full IDF state machine (`New`, `PendingVerify`,
   `Valid`, `Invalid`, `Aborted`), get a `FlashRegion` for the other slot, and verify an
   image's appended SHA-256 (`PartitionEntry::sha256`). It does **not** validate an image
   before flipping `otadata`; we must, and §5 says exactly how.

4. **Real bootloader rollback is achievable but not with the bootloader we ship today.**
   espflash's bundled `esp32-bootloader.bin` is ESP-IDF v6.1 at stock defaults, and
   `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` defaults to *No*. So the shipped bootloader
   selects OTA slots and falls back from a *structurally broken* image, but it will never
   turn `New` into `PendingVerify` nor `PendingVerify` into `Aborted`. Getting real
   rollback = build one ESP-IDF v6.1 bootloader with that option on, commit the 26 KB
   `.bin`, and pass `espflash --bootloader`. **ESP-IDF is not installed on this bench.**
   Without it the residual risk is precisely: *an image that is structurally perfect and
   crashes or hangs before our own confirm code runs bootloops until someone plugs in
   USB.* §6 gives the app-side rollback that covers everything short of that, and the
   cheapest mitigation if the owner does not want an ESP-IDF install.

5. **Core 1 must be stalled during every flash write, and that is survivable.**
   `esp-storage 0.10.0`'s default multi-core strategy is `Error`: with the display owning
   core 1 forever, *every* erase and write returns `OtherCoreRunning`. We must call
   `FlashStorage::new(..).multicore_auto_park()`, which RTC-stalls core 1 (a hardware
   clock stall, not a software park) for the duration of each ROM call and unparks it
   after. Critically, the granularity is **one 4 KB sector or one 64 KB block at a time**,
   not one OTA. The HUB75 DMA is circular and needs no CPU, so the panel keeps scanning
   the buffer it has: a sector erase (~50 ms) freezes the *dither phase*, not the picture.
   The unknown that only the bench can settle is the other half of the same window: core 0
   has interrupts masked for that same ~50 ms, and esp-radio's WiFi does not like that.

6. **RAM cost is small; the trap is holding buffers across `await`.** The spike added
   **0 bytes** of library `.bss`. All of its +11,040 `.bss` bytes were its own temporaries
   living inside the `main` task's future (`___embassy_main4POOL`: 96 → 11,136 bytes).
   Core 0's stack paid for all of it: `.stack` fell 37,536 → 25,808, i.e. −11,728, which
   is the `.bss` growth plus the `.data` growth plus alignment — `.bss`, `.data` and the
   stack come out of one region, exactly as the card warned. Flash cost:
   **+47,984 bytes** of app image, of which 3,704 bytes are IRAM (`.rwtext`, the `#[ram]`
   ROM-flash wrappers).

7. **`sequential-storage 8.0.1` is async-only.** It wants
   `embedded_storage_async::nor_flash::NorFlash`; `esp-bootloader-esp-idf` hands us a
   blocking `NorFlashRegion`. `embassy_embedded_hal::adapter::BlockingAsync` (0.6.0, same
   `embedded-storage-async 0.4.1`) bridges them, and that is what the spike uses.

---

## 1. What is on the device right now

espflash's built-in default table (`espflash-4.6.0/src/image_format/idf.rs` line 745):

| name | type | subtype | offset | size |
|---|---|---|---|---|
| nvs | data | nvs | 0x9000 | 0x5000 |
| phy_init | data | phy | 0xf000 | 0x1000 |
| factory | app | factory | 0x10000 | 0x3f0000 |

The stock Tidbyt table (`backup/README.md`) was nvs 0x9000+0x5000, otadata 0xe000+0x2000,
app0 0x10000+0x3f0000, app1 0x400000+0x3f0000. Our new table keeps `nvs` and `otadata` at
the stock offsets, so the low 64 KB of flash has the same shape it has always had here.

## 2. Sizes, measured

`cargo build --release` in a clean worktree, `xtensa-esp32-elf-size -A`, and
`espflash save-image --chip esp32`:

| section | baseline | with `--features spike-ota` | delta |
|---|---|---|---|
| `.text` | 531,205 | 571,233 | +40,028 |
| `.rodata` | 73,064 | 76,632 | +3,568 |
| `.rwtext` (IRAM) | 11,812 | 15,516 | +3,704 |
| `.data` | 31,492 | 32,176 | +684 |
| `.bss` | 127,040 | 138,080 | +11,040 |
| `.stack` (core 0) | 37,536 | 25,808 | −11,728 |
| app image | **743,408** | **791,392** | +47,984 |

`xtensa-esp32-elf-nm -S --size-sort`: `_..._10screeny_fw6___main14___embassy_main4POOL`
is **96** bytes in the baseline and **11,136** bytes in the spike. That single symbol is
the whole `.bss` delta. The spike holds two `PARTITION_TABLE_MAX_LEN` (0xC00) buffers and
a 4096-byte staging chunk across `await` points inside `main`, and an embassy task's
future is a static. The build cards must not repeat that.

Budget for the real thing: the OTA + store code is ~48 KB of flash and, done right, a
few hundred bytes of `.bss` plus one heap-allocated 4 KB staging buffer that exists only
while an update is in flight. Card 201's HTTP server is on top of that. A 2 MB slot has
1.3 MB of headroom over today's image; that is not the constraint.

## 3. The proposed partition table

To be written to `firmware/partitions.csv` by the build card, **not** by this card.

```csv
# screeny partition table — 8 MB flash, two OTA slots, one settings partition.
# Flashed by tools/fw-run.sh via `espflash --partition-table`.
# App partitions must be 64 KB aligned; data partitions 4 KB aligned.
# Name,    Type, SubType,   Offset,   Size,     Flags
nvs,       data, nvs,       0x9000,   0x5000,
otadata,   data, ota,       0xE000,   0x2000,
ota_0,     app,  ota_0,     0x10000,  0x200000,
ota_1,     app,  ota_1,     0x210000, 0x200000,
screeny,   data, undefined, 0x410000, 0x10000,
```

Verified: `espflash partition-table --to-binary` accepts it and emits 3072 bytes with the
MD5 entry that `esp-bootloader-esp-idf`'s default `validation` feature demands
(`partitions.rs` line 301-320). `espflash save-image --partition-table <this>` reports
`App/part. size: 743,408/2,097,152 bytes, 35.45%`, confirming it targets `ota_0`.

Why each line:

- **`nvs` 20 KB at 0x9000.** Nothing in our `no_std` stack reads it. It is kept at the
  stock offset so the first partition still starts at 0x9000 and the table therefore still
  belongs at 0x8000, which is both espflash's inferred default and
  `esp-bootloader-esp-idf`'s compiled-in `partition-table-offset` (`esp_config.yml`,
  default 32768). Costs nothing; removing it would move every other offset.
- **`otadata` 8 KB at 0xE000.** `Ota::new` (`ota.rs` line 208) *rejects* any size but
  `0x2000`. Two 32-byte entries live at +0x0000 and +0x1000, i.e. two different 4 KB
  sectors, which is what makes the flip atomic (§5).
- **`ota_0` / `ota_1`, 2 MB each.** App partitions must be 64 KB aligned on ESP32
  (the MMU page size). 2 MB is 2.8× today's image. Bigger slots buy nothing and make
  `--erase-parts` and any full-slot SHA slower; smaller ones would need re-planning
  the first time the HTTP server and the portal land.
- **`screeny` 64 KB at 0x410000, data/`undefined` (0x06).** `undefined` is the only
  "custom data" subtype `DataPartitionSubType` knows (`partitions.rs` line 565), and
  `PartitionEntry::partition_type()` (line 129) `unwrap!`s the conversion, so an unknown
  subtype would **panic** on the device. Look it up by *label*, not by type: there is no
  `find_by_label`, so iterate `PartitionTable::iter()` and compare `label_as_str()`.
  64 KB = 16 `sequential-storage` pages, which is generous wear levelling for a record of
  well under 200 bytes.
- **No `phy_init`.** esp-radio (the Rust one) never reads it; it is an ESP-IDF artefact
  that espflash's default table carries for IDF users.
- **No `coredump`.** Three reasons. Nothing in the esp-rs stack writes one — coredump is
  an ESP-IDF panic-handler feature and we run `esp-backtrace`, which prints over serial.
  Writing flash *from a panic handler* is the one context where the park/critical-section
  discipline of §4 cannot be honoured (we would be parking a core from an arbitrary
  interrupt context, possibly while the other core holds a lock). And it would cost 64 KB
  for a feature nothing implements. The useful, cheap version of the same idea is a small
  persistent breadcrumb — the link map already reserves `.rtc_slow.persistent` at 0 bytes
  — recorded as a proposed card in §9.
- **0x420000..0x800000 (3.875 MB) left unallocated** on purpose, for a future asset or
  LittleFS partition; adding one later does not move anything above.

### What the first flash of the new table does to this device

`espflash flash` writes bootloader @0x1000, table @0x8000, app @ `ota_0` = 0x10000 — the
same offset the app already occupies. The new `otadata` at 0xE000..0x10000 lands on the
last sector of the current `nvs` plus the current `phy_init`, i.e. **on whatever bytes are
there now**, and espflash does not erase it. Both entries would then almost certainly fail
`bootloader_common_ota_select_valid`'s CRC check, the bootloader would log "No factory
image, trying OTA 0", boot `ota_0`, and `set_actual_ota_seq` would write a clean
`otadata[0]` — it self-heals. But "almost certainly" is a 1-in-2³² argument, and there is
no reason to make it: `--erase-data-parts ota` makes the first flash, and every flash,
deterministic.

### The exact change to `tools/fw-run.sh`

```diff
-espflash flash --chip esp32 --port "$PORT" --baud 230400 --non-interactive "$ELF" 2>&1 | tail -2
+# --partition-table: two OTA slots + the settings partition (docs/research/006).
+# --erase-data-parts ota: espflash never touches otadata, so without this a stale
+# OTA selection survives a serial flash and the device boots the *other* slot.
+espflash flash --chip esp32 --port "$PORT" --baud 230400 --non-interactive \
+  --partition-table firmware/partitions.csv --erase-data-parts ota \
+  "$ELF" 2>&1 | tail -2
```

`fw-run.sh` already `cd`s to `$ROOT`, so the relative path resolves. An `espflash.toml`
with an `[idf]` table would work too (`cli/config.rs` line 94 aliases `idf_format_args` to
`idf`, and `find_config_path` looks in the cwd and its parent), but explicit flags in the
one sanctioned flashing script beat a config file two directories away. `--flash-size 8mb`
is optional: espflash detects the size (`flasher/mod.rs` line 894).

## 4. Flash writes with the display on core 1

This is the part that decides whether any of the rest is safe.

### What `esp-storage` actually does

`esp-storage-0.10.0/src/common.rs` line 235 defines `MultiCoreStrategy`. On a multi-core
chip the default is `Error` (line 120), and `pre_write` (line 277) returns
`FlashStorageError::OtherCoreRunning` if any other core is running. Our display task owns
core 1 permanently, so **a default `FlashStorage` can never write on this device**. The
two escapes are `multicore_auto_park()` (line 255) and `unsafe multicore_ignore()`
(line 265).

`multicore_auto_park` calls `CpuControl::park_core`, which on ESP32
(`esp-hal-1.2.2/src/soc/esp32/cpu_control.rs` line 16) writes the RTC_CNTL
`SW_STALL_APPCPU_C0`/`C1` fields to the 0x86 stall pattern. That is a **hardware clock
stall**: core 1 freezes wherever it is and resumes there on unpark. Peripherals, DMA and
the interrupt controller are untouched.

Granularity is the good news. `nor_flash.rs::erase` (line 206) loops sector-by-sector to
the next 64 KB boundary, then block-by-block, then sector-by-sector again; `write_nor`
(line 159) chunks into 4 KB pieces. **Each** `internal_erase_sector` /
`internal_erase_block` / `internal_write` (`common.rs` lines 172-199) runs its own
`MultiCoreStrategy::with` (line 339), i.e. its own park → ROM call → unpark. Core 1 is
stalled per sector, not per update.

Inside that, `hardware.rs` wraps every ROM call in `maybe_with_critical_section`
(`lib.rs` line 66) and marks it `#[ram]`. The lock is an `esp_sync::RawMutex`, which
"disables interrupts on the current core while locked" and — explicitly — "does not take a
global critical section" (`esp-sync-0.3.0/src/lib.rs` line 285-292). Note that
`esp-bootloader-esp-idf` depends on `esp-storage` with `default-features = false`, so the
`critical-section` feature is **off** unless the firmware asks for it. It must be on: the
ESP32 cache cannot serve reads while SPI1 is erasing, so an interrupt handler whose code
lives in flash, firing mid-erase, is a crash and not a theoretical one.

### What the panel does

The HUB75 DMA is circular (`firmware/Cargo.toml`, `circular-dma`) and the refresh ISR is
in IRAM (`iram`). The DMA loops over the descriptors it has with no CPU at all. So while
core 1 is stalled:

- the panel keeps scanning the framebuffer it already holds, at full refresh and full
  brightness — **no blanking, no flicker, no dropped rows**;
- what stops is the per-refresh rewrite that spends the dither remainder, so the dither
  phase freezes. Visually that is a brief, very slight colour quantisation, not a glitch;
- the core-1 executor's pending timer and DMA interrupts coalesce and fire on unpark; one
  missed swap, not a corrupt frame.

Rough numbers for a typical 8 MB NOR part: 4 KB sector erase ~50 ms typical (spec max can
be 400 ms), 64 KB block erase ~300-500 ms, 4 KB program ~8 ms. So:

- **config write** (one `sequential-storage` item, usually no erase at all, occasionally
  one sector erase): ≤ ~60 ms of frozen dither. **Recommendation: do nothing.** It is
  invisible, and special-casing it adds a failure mode.
- **OTA** of a ~750 KB image, erase-as-you-go at sector granularity: 183 sectors ×
  (~50 ms erase + ~8 ms write) ≈ **11 s of flash-busy time**, in ~58 ms slices with the
  network in between. **Recommendation: put the panel on a static "updating" screen with
  dither *off* and the display task idle for the duration.** With dither off the display
  task sleeps and the DMA loops; stalls then cost literally nothing, and a static screen
  is honest about what the device is doing. Do **not** park core 1 wholesale for the whole
  update — `esp-storage` already parks it exactly when needed, and a long manual park
  would starve the core-1 executor's timer.

### Two rules the build cards must not break

1. **Only core 0 ever touches flash.** `pre_write` parks core 1 *before* core 0 takes
   `esp-storage`'s lock. If core 1 were ever stalled while holding that same lock, core 0
   would spin on it forever. ~~Because `RawMutex` is per-instance and not a global critical
   section, core 1 holding *any other* lock is harmless~~ — but flash from core 1 is a hard
   deadlock.
2. **Never `multicore_ignore()`.** Core 1 fetches instructions from flash constantly; the
   safety contract of that call is exactly the thing we cannot promise.

> **Correction, card 245 (2026-09-20). The struck-out clause in rule 1 is wrong, and it
> cost a day of bench time.** It is true of the lock core 0 takes *explicitly* and false
> of the ones it takes *implicitly, in an interrupt handler*. `MultiCoreStrategy::with`
> (`common.rs` 339-349) is park → `f()` → un-park, and the interrupt masking lives inside
> `f()`: the guard on `esp-storage`'s own lock is dropped when the `#[ram]` ROM wrapper
> returns (`hardware.rs` 17, `lib.rs` 74), so core 0 takes interrupts again **while core 1
> is still parked**, with ~40 ms of backlog queued. The first handler in is `esp-rtos`'s
> `timer_tick_handler` (`esp-rtos-0.4.0/src/timer/mod.rs` 232), which takes the cross-core
> scheduler lock (`scheduler.rs` 639) that core 1's executor holds on every wake. Frozen
> core 1 owns it; core 0 spins for ever; `post_write` is never reached, so core 1 is never
> un-parked. Both cores dead, no panic, no log — and only TIMG0's watchdog left. `esp-rtos`
> writes the same hazard down as a FIXME on its own light-sleep hook (`sleep.rs` 118-126).
>
> **Rule 3, therefore: every erase and every program goes through `store::guarded`**,
> which holds the one global `critical_section` — on this chip a single `esp_sync::RawMutex`
> shared by both cores (`esp-hal-1.2.2/src/sync.rs` 99) — across park → ROM call → un-park,
> so core 0 runs no interrupt handler while core 1 is frozen and can therefore wait on
> nothing. Reads are exempt: `internal_read` (`common.rs` 149) never parks core 1.
> Card 245's Log has the interleaving and the argument.

### The open risk

Core 0 has interrupts masked for the whole ROM erase (~50 ms, worst case several hundred
ms). esp-radio's WiFi driver expects to service its ISRs far more often than that. TCP
will retransmit and the association should survive, but "should" is doing work in that
sentence; a dropped association mid-upload, or an esp-radio internal timeout, is the most
likely way an OTA fails on this device. **This is the one thing that must be measured on
hardware**, and it is why the build card for staging should be able to report how long
each sector took.

## 5. OTA: the API, and validating before switching

### What the crate gives us

- `partitions::read_partition_table(&mut flash, &mut [u8; PARTITION_TABLE_MAX_LEN])`
  (line 625). `PARTITION_TABLE_MAX_LEN` is 0xC00 = 3072 bytes.
- `PartitionTable::booted_partition()` (line 389) — works on esp32 by reading MMU entry 0
  at `0x3FF10000`. This is how the app learns which slot it is *actually* running from,
  which can differ from the selected one after a bootloader fallback.
- `ota::Ota` (line 186): `current_app_partition`, `set_current_app_partition`,
  `current_ota_state`, `set_current_ota_state`. The full `OtaImageState` enum is there
  (line 75), `PendingVerify` and `Aborted` included.
- `ota_updater::OtaUpdater` (line 24): refuses to construct unless there is an `otadata`
  partition and ≥ 2 non-factory app partitions; `next_partition()` (line 141) returns a
  `FlashRegion` for the slot that is not booted; `activate_next_partition()` (line 134)
  flips `otadata`; `reset_data()` (line 156) wipes it back to factory/ota_0.
- `PartitionEntry::sha256(&mut flash)` (line 161) — for an app image with an appended
  hash it recomputes the SHA-256 over the image and **returns `Error::InvalidImage` if it
  does not match**. This is the single most valuable call in the crate for us.
- `FlashRegion::as_nor_flash()` (line 905) → a `NorFlashRegion` implementing
  `NorFlash` + `MultiwriteNorFlash` with `WRITE_SIZE` 4, `ERASE_SIZE` 4096.

### What it does not give us, and we must add

`esp-bootloader-esp-idf` never inspects an image before `activate_next_partition`. The
gate has to be ours, run against the staged slot, and it has five checks — in this order,
cheapest first (`spike_ota.rs::validate_staged` is the worked version):

1. `header[0] == 0xE9` — ESP image magic.
2. `u16::from_le(header[12..14]) == 0x0000` — chip id ESP32. Stops an ESP32-C3 build from
   ever reaching `otadata`.
3. `header[23] != 0` — "hash appended". Refuse an image without one; it is the only thing
   that makes check 5 possible.
4. `esp_app_desc` at offset 24 + 8 (after the image header and the first segment header):
   magic `0xABCD5432`, `project_name` (offset +48, 32 bytes) equal to **`screeny-fw`**
   (`main.rs` uses the no-argument `esp_app_desc!`, which takes `CARGO_PKG_NAME`;
   `lib.rs` line 370). Capture `version` (offset +16, 32 bytes) for the reply and for
   telemetry.
5. `PartitionEntry::sha256()` — recompute and compare the appended digest. A truncated or
   flipped-bit upload dies here, before `otadata` is touched.

The build card should also refuse an upload whose `Content-Length` exceeds the slot size,
before a single sector is erased.

### Streaming the upload

- **Chunk: 4096 bytes**, one sector. It is `esp-storage`'s own internal chunk for both
  erase and write, it is the largest unit that still gives core 1 a gap, and it keeps RAM
  at one buffer.
- **Erase as you go**, one sector immediately before writing it. `FlashRegion::erase(x,
  x+4096)` with `x` 64 KB-aligned takes the *sector* path, never the 64 KB block path
  (`nor_flash.rs` line 216-233), so the longest stall stays ~50 ms. Erasing the whole 2 MB
  slot up front would be ~25 s of stalls before a single byte arrives and buys nothing:
  trailing bytes after the new image are never read, because the bootloader takes the
  image length from the header and segment table (`get_image_metadata`, line 670).
- **Write through `NorFlashRegion::write`** with a word-aligned buffer. If the buffer is
  not 4-byte aligned, `write_nor` silently copies through *another* 4096-byte buffer on
  the stack (`esp-storage/src/lib.rs`, "Buffer alignment and stack usage"). Align the
  staging buffer and that never happens. Note that `FlashRegion::write` /
  `FlashStorage::write` (the non-`_nor` ones, which the `FlashAccess` impl uses for
  `otadata`) *always* put a 4096-byte sector buffer on the caller's stack and do a full
  read-modify-erase-write of the sector; that is fine for a 32-byte `otadata` entry, and
  wrong for bulk staging.
- **One buffer, one owner.** The staging buffer and the 3 KB partition-table buffer must
  not be live across an `await` (see §2). Put the per-chunk work in a synchronous
  `fn stage_chunk(...)` that the async HTTP handler calls, or allocate from the heap for
  the life of the update.

### Every way it can be interrupted, and why each state still boots

| when power / the connection dies | flash state | next boot |
|---|---|---|
| before the first byte | nothing changed | running slot |
| mid-stream | staged slot part-written, `otadata` untouched | running slot; the next update re-erases the staged slot |
| all bytes written, validation not yet run | as above | running slot |
| validation failed | as above; we never flip | running slot |
| **during the `otadata` write** | the entry being written has a bad CRC, or is 0xFF because the erase finished and the write did not | **the other entry wins.** `Ota::set_current_app_partition` writes `self.current_slot()?.next()` (`ota.rs` line 309) — it always writes the *inactive* entry, so the currently-active one is never in flight. `bootloader_common_ota_select_invalid` (v6.1 `bootloader_common_loader.c` line 78) rejects a bad CRC. This is the atomicity guarantee the whole design rests on. |
| `otadata` flipped, state `New`, power lost before reboot | committed | the new slot, exactly as intended |
| new image is structurally broken | committed | bootloader's `bootloader_utility_load_boot_image` walks backwards from the selected slot, fully verifying each candidate (v6.1 line 590-602), and lands on the old slot |
| new image boots and then misbehaves | committed | **this is the only bad case.** §6. |

## 6. Rollback

### The bootloader we ship today

`espflash-4.6.0/resources/bootloaders/manifest.yaml` says every bundled bootloader is
built from ESP-IDF `release/v6.1`, and the only sdkconfig fragment for esp32 is
`# CONFIG_BOOTLOADER_COMPILE_TIME_DATE is not set`. `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`
defaults to **No** (ESP-IDF kconfig reference, stable/esp32). In `bootloader_utility.c`
(v6.1) both halves of the state machine are inside `#ifdef
CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`: lines 392-402 turn any `PENDING_VERIFY` entry into
`ABORTED` on the next boot, and lines 442-448 turn the selected `NEW` entry into
`PENDING_VERIFY`. Neither is compiled in.

`strings` on the 26,112-byte `esp32-bootloader.bin` confirms the OTA *selection* half is
present ("No factory image, trying OTA 0", "ota data partition invalid, falling back to
factory", "Set actual ota_seq=%lu in otadata[0]"). The rollback half is `ESP_LOGD`, which
is compiled out at the default INFO level, so the binary cannot prove it either way — the
Kconfig default is the evidence.

When it *is* enabled, the mechanism is simple and good: `ABORTED` and `INVALID` entries
are not "valid" (`bootloader_common_loader.c` line 78-86), so
`bootloader_common_get_active_otadata` picks the other entry — the previous slot.

### What to do

**Recommended: build our own bootloader, once.** Install ESP-IDF v6.1, build a minimal
project's bootloader with `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` (espflash's own
`xtask build-bootloaders` does exactly this and is a working recipe), commit the ~26 KB
`.bin` as `firmware/bootloader/esp32-rollback-bootloader.bin`, and add
`--bootloader firmware/bootloader/...` to `tools/fw-run.sh`. It is written to 0x1000 on
ESP32 (`espflash/src/target/mod.rs` line 343), same as the bundled one.

Two things for the owner: ESP-IDF is **not installed here today** (`IDF_PATH` unset, no
`idf.py`, no `~/esp`; `~/export-esp.sh` only sets the Xtensa Rust toolchain), and this puts
a checked-in binary blob in a repo that is meant to go public. The blob is Espressif's own
Apache-2.0 output and espflash already ships an equivalent, so it is not a licensing
problem, but it is the owner's call.

### The app-side half, which we need either way

With a rollback bootloader, the sequence is: stage → validate → `activate_next_partition()`
→ `set_current_ota_state(New)` → reboot. The bootloader promotes `New` to `PendingVerify`.
Our new image must then confirm itself or be reverted on the next boot.

**Proposed health criterion** — confirm (`set_current_ota_state(Valid)`) when *all* of:

- WiFi associated and DHCP bound;
- the HTTP server has accepted one request **or** 120 s of uptime has passed;
- the display task has completed at least one swap since boot;

and no sooner than **60 s** after boot, so that a crash-after-30-seconds is still caught.

**If not met within 180 s**: `set_current_ota_state(Invalid)` on the active entry and
reboot. Because `INVALID` is rejected by `ota_select_valid`, the bootloader then uses the
other `otadata` entry and boots the old slot — **no bootloader support needed for this
path**. That covers every "boots but never becomes useful" failure: bad WiFi code, a
wedged HTTP server, a display that never swaps.

**Without a rollback bootloader**, that app-side check is all we get, and the residual risk
is exactly one shape: *an image that panics, hangs or watchdog-resets before the 60 s
confirm timer can run.* It bootloops forever; recovery is `tools/fw-run.sh` over USB, with
`backup/tidbyt-stock-*.bin` as the floor. Cheapest mitigations, in order:

1. Arm the confirm/revert logic as early as possible — the first thing after
   `esp_hal::init`, reading `otadata` before WiFi, the panel or anything else is brought
   up, so the surface that can crash "before the timer" is a few hundred instructions.
2. Have the *sender* keep the last-known-good image and re-push it: a bootloop is visible
   (the device never reappears on mDNS), and re-flashing over serial is one command.
3. Refuse OTA entirely when the device has no known-good other slot — e.g. the first
   update after a serial flash writes `ota_1` and keeps `ota_0` as the escape hatch,
   which the two-slot design already does.

None of these close the hole. Only the bootloader does.

## 7. The settings store

**Crate**: `sequential-storage 8.0.1`, `map` over the `screeny` partition, exactly as
`docs/design/protocol-v1.md` §8 already says. It is async-only
(`embedded-storage-async 0.4.1`), so wrap the blocking `NorFlashRegion` in
`embassy_embedded_hal::adapter::BlockingAsync` (0.6.0). Its no-cache type is
`cache::Cache::new_uncached()`.

**Range**: `MapConfig::try_new(0..len)` — offsets are partition-relative because
`NorFlashRegion` adds `region.offset` itself. Use `try_new`, not `new`: `new` **panics**
on a bad range (`map.rs` line 30). 64 KB / 4 KB = 16 pages, comfortably above the 2-page
minimum.

**Key set** (`u8` keys; `sequential-storage` implements `Key` for `u8`):

| key | value | notes |
|---|---|---|
| 0 `SchemaVersion` | `u8` | written first, read first; an unknown version means "ignore the rest and use defaults" |
| 1 `WifiSsid` | `&[u8]` ≤ 32 | §8 of the spec |
| 2 `WifiPsk` | `&[u8]` ≤ 64 | never logged, never in `GET_WIFI`, never on the panel |
| 3 `Name` | `&[u8]` ≤ 32 | card 063; also drives the mDNS instance name |
| 4 `Brightness` | `u8` | card 063 |
| 5 `IdleMode` | `u8` | card 063 |

Total well under 200 bytes; the largest item is far below the one-page limit, so
`Error::ItemTooBig` is unreachable.

**Debounce** (card 063's real concern): a `SET_BRIGHTNESS` slider sends ~60/s.
Keep the live value in the existing atomic, mark a dirty flag, and have one store task
commit after **3 s of quiet**, skipping the write when the value equals what was read at
boot or last committed. On a failed commit return `ERR_STORAGE` (spec §6.5), which
nothing returns today. A 60 s sweep at 30 changes/s should produce a handful of writes,
which is card 063's own acceptance test.

**Blank or corrupt partition**: a freshly erased partition reads as all-`0xFF`, every page
is "open", and `fetch_item` returns `Ok(None)` — defaults, no error. On damage,
`sequential-storage` returns `Error::Corrupted` and `MapStorage`'s `run_with_auto_repair!`
(`lib.rs` line 634) re-runs the operation after a repair pass; `Error::LogicBug` exists
precisely so the crate returns instead of panicking (`lib.rs` line 525-531). The firmware
rule: **any** error from the store at boot → log it, fall back to compiled-in defaults,
carry on. Never `unwrap`. Offer an explicit "erase settings" path (control opcode or the
button) rather than erasing automatically.

**Where the code lives.** The record encoding — key enum, value encode/decode, schema
version, migration — belongs in a new `no_std`, no-alloc crate `crates/settings`,
following `crates/receiver`'s precedent, so a plain `cargo test` covers it on the host.
The flash side (`FlashStorage`, `NorFlashRegion`, `BlockingAsync`, the debounce task) is
device-only and stays in `firmware/src/`. `sequential-storage` has a `mock_flash` module
behind its `_test` feature, so the host tests can exercise the real map machinery against
RAM rather than only the encoding.

## 8. The spike

`firmware/src/spike_ota.rs`, behind `--features spike-ota`, called from `main` so the
linker keeps it. It is **evidence, not the implementation**: the build cards should write
the real thing from this document, not copy the spike. It does compile cleanly on the
pinned stack with no version bumps, which was the question.

Two API surprises worth carrying forward: `sequential-storage 8`'s async-only trait
bound, and `Cache::new_uncached()` rather than a `NoCache` type.

## 9. Proposed build cards

Card numbers are the orchestrator's to assign; these are titles and scope.

**A. Partition table and the flashing path.** Add `firmware/partitions.csv` exactly as
§3. Change `tools/fw-run.sh` to pass `--partition-table` and `--erase-data-parts ota`, and
document in `backup/README.md` how the new layout relates to the stock one. `hardware:
yes` — the acceptance is one flash and one boot on the real device, with the serial log
showing the bootloader selecting `ota_0`, plus a `GET_INFO` that still works. This is the
foundation every other card here sits on and should land alone.

**B. A rollback-capable bootloader.** Install ESP-IDF v6.1, build a bootloader with
`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`, commit the binary under `firmware/bootloader/`
with a README recording the IDF ref and the exact sdkconfig fragment, and teach
`tools/fw-run.sh` to pass `--bootloader`. Needs an owner decision first (an IDF install
and a checked-in blob in a repo meant to go public). Without this card, card E's rollback
only covers images that boot.

**C. The settings store: `crates/settings` plus the device side.** The `no_std` record
crate with host tests, the `sequential-storage` map over the `screeny` partition through
`BlockingAsync`, load-at-boot-before-first-frame, and the "any error → defaults, never
panic" rule. Delivers card 063's brightness/idle/name persistence with the 3 s debounce
and `ERR_STORAGE`, and the WiFi credential slots that spec §8 needs. Retire card 063 into
this one or make it depend on it.

**D. Staging an image into the inactive slot.** The synchronous `stage_chunk` path:
4 KB chunks, erase-as-you-go at sector granularity, a word-aligned heap staging buffer,
`multicore_auto_park`, esp-storage with `critical-section` on, and per-sector timing
reported to telemetry. Plus the five-check validator of §5 and the `Content-Length`
pre-check. No HTTP yet — drive it from a control opcode or the serial console so it can be
tested before card 201's server exists. `hardware: yes`; the acceptance is a staged image
that validates, and a deliberately truncated one that is rejected without `otadata`
changing.

**E. Activate, confirm, revert.** `activate_next_partition` + `New`, the boot-time
confirm/revert state machine of §6 armed as early as possible in `main`, the health
criterion, and the 180 s revert. `hardware: yes`. Acceptance: a good image confirms and
survives a power cycle; an image deliberately built to never join WiFi reverts by itself
to the previous slot within 180 s, twice in a row.

**F. The display during an update.** A static "updating" screen with dither off and the
display task quiesced for the duration, and a measurement of what the panel actually does
during a 750 KB stage with dither on, so the choice is evidence rather than this
document's reasoning. Also the answer to "does esp-radio survive a ~50 ms interrupt-off
window", which is the open risk in §4 and may force smaller writes or a pause-and-resume
upload.

**G. A panic breadcrumb in RTC slow memory.** The cheap replacement for the coredump
partition we are not creating: a few dozen bytes of `.rtc_slow.persistent` holding the last
panic's PC, a reason code and a boot counter, surfaced in `GET_INFO` / `screeny stats`.
Survives a reset, costs no flash, and is what actually helps when an OTA'd image
misbehaves. Small, independent, and useful on its own.

## Sources

- Crate sources at the pinned versions:
  `esp-bootloader-esp-idf-0.6.0`, `esp-storage-0.10.0`, `esp-sync-0.3.0`,
  `esp-hal-1.2.2`, `sequential-storage-8.0.1`, `embassy-embedded-hal-0.6.0`, all under
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.
- `espflash 4.6.0` sources: <https://static.crates.io/crates/espflash/espflash-4.6.0.crate>
  (`src/image_format/idf.rs`, `src/cli/mod.rs`, `src/cli/config.rs`,
  `src/target/mod.rs`, `resources/bootloaders/manifest.yaml`, `resources/README.md`).
- ESP-IDF `release/v6.1`:
  <https://github.com/espressif/esp-idf/blob/release/v6.1/components/bootloader_support/src/bootloader_utility.c>,
  <https://github.com/espressif/esp-idf/blob/release/v6.1/components/bootloader_support/src/bootloader_common_loader.c>
- `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` default:
  <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/kconfig-reference.html>
- ESP-IDF OTA API guide:
  <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/ota.html>
- Partition tables:
  <https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-guides/partition-tables.html>
