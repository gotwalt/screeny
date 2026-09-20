---
id: 200
title: Research - flash layout, a settings store, and a firmware update that cannot brick the device
type: research
hardware: no
depends: [008]
owner: worker-200
branch: card/200-research-flash-store-ota
---

## Goal

The owner wants (2026-09-20) the device to take firmware updates over HTTP,
**safely**, and to keep its network settings in flash. Find out exactly how that is
done on *this* stack, prove the pieces link and fit, and recommend a design the
build cards can follow without rediscovering anything.

This is the device-web track: cards 200-249, coordinated by the `firmware` session.
Sibling research cards running in parallel: 201 (HTTP server, soft-AP, captive
portal) and 202 (the button's GPIO). Stay out of their questions.

## Context

Read first: `CLAUDE.md`, `firmware/src/main.rs` (the module doc and `main`),
`firmware/Cargo.toml`, `docs/research/001-firmware-stack.md`,
`docs/design/protocol-v1.md` section 8, `backup/README.md`,
`docs/board/parked/063-persist-settings.md`.

What is already known:

- ESP32-D0WD-V3, 8 MB flash, no usable PSRAM assumption. Firmware is `no_std`
  embassy on `esp-hal =1.2.2`, `esp-rtos =0.4.0`, `esp-radio =1.0.0-beta.1`,
  `esp-bootloader-esp-idf =0.6.0`. Versions are pinned for reasons in research 001;
  do not propose bumping them unless a finding forces it, and then say what breaks.
- We flash with `espflash flash` and **no partition table argument**, so the device
  currently carries espflash's default table (one `factory` app, no `otadata`). The
  stock Tidbyt table was nvs 0x9000+0x5000, otadata 0xe000+0x2000, app0
  0x10000+0x3f0000, app1 0x400000+0x3f0000. `tools/fw-run.sh` is the only flashing
  path and must stay the only one.
- **The display runs on core 1, permanently**: with dither on it rewrites a DMA
  buffer every refresh (154 Hz) from code that lives in flash. The HUB75 refresh ISR
  is in IRAM (`iram` feature) and the DMA is circular, so the panel keeps scanning
  the last buffer with no CPU at all. A flash erase/write disables the cache: what
  happens to core 1 while core 0 writes flash is the central risk of this card.
- Memory trap: `.bss` and core 0's main stack share one region; every static buffer
  shrinks the stack. Heap is 64 KB reclaimed + 32 KB. Report what your proposal costs.
- Spec section 8 already says the store is `esp-storage` + `sequential-storage` on a
  dedicated partition. Card 063 (parked) lists the settings that must persist
  (brightness, idle mode, name) and the write-amplification concern.
- The serial port and `backup/tidbyt-stock-*.bin` are always there as the last-resort
  recovery. "Safe" means the owner never needs them after a bad OTA.

## Questions to answer

1. **Partition table.** Propose `firmware/partitions.csv`: `nvs`, `otadata`, two app
   slots, a `screeny` config partition, (coredump? no - say why or why not). Sizes,
   offsets, and how the current image size compares to the slot. How `espflash`
   takes it (`--partition-table`, or `espflash.toml`), and the exact change to
   `tools/fw-run.sh`. What the first flash of the new table does to a device that
   has the old one. Does espflash write to `ota_0` and reset `otadata` on a serial
   flash, so that a serial flash always wins over a stale OTA slot?
2. **OTA API.** What `esp-bootloader-esp-idf 0.6.0` gives us (`ota`, `OtaUpdater` or
   whatever it is called in this version - read the source in `~/.cargo/registry`),
   what it needs from `esp-storage`, and what is missing. Image validation before
   switching slots: ESP image header magic, chip id, segment checksum, the appended
   SHA-256, and the `esp_app_desc` (project name `screeny-fw`, version) so that a
   wrong file is refused *before* `otadata` changes.
3. **Rollback.** Does the second-stage bootloader espflash ships honour
   `ESP_OTA_IMG_PENDING_VERIFY` and roll back an image that never confirms itself?
   If not: what it takes to build one that does (ESP-IDF needed? is it installed
   here?), how espflash is told to use it, and what the app-side "I am healthy,
   confirm me" call looks like. Propose the health criterion (e.g. joined WiFi or
   reached the portal, and served one HTTP request or N seconds alive) and the
   behaviour when it is not met. If real rollback is out of reach, say plainly what
   the residual risk is and what the cheapest mitigation is.
4. **Flash writes with the display on core 1.** Read `esp-storage` (the version that
   matches `esp-hal 1.2.2`): does it stall or park the other core during an
   erase/write, and is that sound with our core-1 executor and the Priority3 DMA
   interrupt? What does the panel do during a 4 KB erase (tens of ms) and during a
   ~1 MB OTA write (tens of seconds in total)? Recommend the display behaviour for
   small config writes (invisible? one dropped dither phase?) and for OTA (a static
   "updating" frame with dither off, display task quiesced, or core 1 parked).
5. **The settings store.** `sequential-storage` map over the config partition: key
   set (wifi ssid/psk, name, brightness, idle mode, a schema version), record
   format, wear, the debounce card 063 asks for, and behaviour on a corrupt or blank
   partition (must boot to defaults, never panic). Where the code should live so the
   record encoding is host-testable (a `no_std` module in `crates/`?).
6. **Streaming the upload.** An image is ~1 MB and RAM is ~100 KB: the HTTP body must
   stream into flash in chunks. Chunk size, erase strategy (erase-as-you-go vs
   up-front), what a dropped connection or power loss at each stage leaves behind,
   and why each of those states still boots.
7. **Proof it links.** A compile-only spike in your worktree: add the OTA + storage
   dependencies to `firmware/`, call the APIs from a function that is reachable, build
   release (`. ~/export-esp.sh && cd firmware && cargo build --release`), and report
   the image size and `.bss`/`.data` growth (`xtensa-esp32-elf-size`). Do not flash.
   Keep the spike on your branch under `lab/` or behind a cargo feature; it is
   evidence, not the implementation.

## Deliverables

- `docs/research/006-flash-store-ota.md`: conclusions first, then the evidence, with
  file and line references into the crate sources you read. End with a recommended
  design and a list of proposed build cards (titles + one paragraph each; do not
  write the card files).
- The proposed `firmware/partitions.csv` as a file in the research doc (fenced), not
  yet in `firmware/`.
- The compile-only spike, on your branch.

## Acceptance

The orchestrator can write the build cards from the research doc alone, and every
claim about a crate's behaviour cites the source line that shows it.

## Log

### 2026-09-19 — crate sources read (esp-bootloader-esp-idf 0.6.0, esp-storage 0.10.0)

Working through questions 2 and 4 first, because they decide whether the rest is
buildable at all. Notes so far, all from `~/.cargo/registry/src/index.crates.io-.../`:

- `esp-bootloader-esp-idf-0.6.0/src/ota.rs` — `Ota` manipulates only the 0x2000-byte
  `otadata` partition (two 32-byte `OtaSelectEntry` at +0x0000 and +0x1000, `ota_seq` +
  CRC32 of the seq + `ota_state`). `Ota::new` (line 200) *requires* `capacity() == 0x2000`
  and `PartitionType::Data(Ota)`. `OtaImageState` (line 75) is the full IDF enum including
  `PendingVerify`/`Aborted`.
- `src/ota_updater.rs` — `OtaUpdater::new` (line 24) refuses unless there is an otadata
  partition *and* >= 2 non-factory app partitions. `next_partition()` (line 141) hands back
  a `FlashRegion` for the slot that is not booted; `activate_next_partition()` (line 134)
  flips otadata. Nothing in this crate validates the image before the flip.
- `src/partitions.rs` — `PartitionEntry::sha256` (line 161) parses the ESP image header,
  walks segments, and *verifies the appended SHA-256* (`Error::InvalidImage` on mismatch).
  `get_image_metadata` (line 670) is the header/segment walker. `booted_partition()`
  (line 389) works on esp32 by reading MMU entry 0 at 0x3FF10000. `PARTITION_TABLE_MAX_LEN`
  = 0xC00 (3 KB buffer needed to read the table).
- The crate's `validation` feature (default on) MD5-checks the partition table itself
  (line 301).
- **The central finding for question 4**: `esp-storage-0.10.0/src/common.rs` line 235,
  `MultiCoreStrategy`. On a multi-core chip the *default* is `Error`: every erase/write
  returns `FlashStorageError::OtherCoreRunning` if the other core is running (line 280).
  Our display owns core 1 forever, so a naive `FlashStorage::new(...)` can never write.
  The two escapes are `multicore_auto_park()` (line 255) and `unsafe multicore_ignore()`
  (line 265).
- `park_core` on esp32 is `esp-hal-1.2.2/src/soc/esp32/cpu_control.rs` line 16: it writes
  the RTC_CNTL `SW_STALL_APPCPU_C0/C1` fields (0x02/0x21 -> the 0x86 stall pattern). That
  is a *hardware clock stall* of core 1, not a software park: core 1 freezes mid-instruction
  and resumes exactly where it was. Peripherals and DMA are untouched.
- Granularity is good news: `esp-storage/src/nor_flash.rs` `erase()` (line 206) loops
  sector-by-sector then block-by-block, and **each** `internal_erase_sector` /
  `internal_erase_block` / `internal_write` does its own park + unpark
  (`common.rs` line 172-199 -> `MultiCoreStrategy::with`, line 339). So core 1 is stalled
  per 4 KB sector / 64 KB block, not for the whole OTA.
- `esp-storage/src/hardware.rs` — the ROM calls are `#[ram]` and wrapped in
  `maybe_with_critical_section` (`lib.rs` line 66), which is an `esp_sync::RawMutex` and is
  only present when the `critical-section` feature is on. `esp-bootloader-esp-idf` pulls
  `esp-storage` with `default-features = false`, so we must enable it ourselves.
- Stack cost warning (`esp-storage/src/lib.rs` "Buffer alignment and stack usage"):
  `FlashStorage::read`/`write` *always* put a 4096-byte sector buffer on the caller's stack;
  `read_nor`/`write_nor` only do it when the caller's slice is not word-aligned. Everything
  `esp-bootloader-esp-idf`'s `FlashRegion::read/write` does goes through the 4 KB-stack
  versions (`flash/flash_access.rs`).
