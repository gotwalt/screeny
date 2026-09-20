---
id: 210
title: Partition table with two OTA slots and a settings partition; the flashing path uses it
type: build
hardware: yes
depends: [200]
owner: firmware orchestrator
branch: main
---

## Goal

Put the flash layout of `docs/research/006-flash-store-ota.md` section 3 on the device
and make `tools/fw-run.sh`, the only flashing path, always use it.

## Deliverables

- `firmware/partitions.csv`: `nvs` 0x9000+0x5000, `otadata` 0xE000+0x2000, `ota_0`
  0x10000+2 MB, `ota_1` 0x210000+2 MB, `screeny` 0x410000+64 KB.
- `tools/fw-run.sh`: `--flash-size 8mb --partition-table firmware/partitions.csv
  --erase-data-parts ota` on every flash, so a serial flash always beats a stale OTA
  selection (espflash never touches `otadata` by itself).

## Log

2026-09-20, orchestrator, on the bench:

- Host: `espflash partition-table --to-binary` accepts the CSV (3072 bytes).
  `espflash save-image` needs `--flash-size 8mb` or it refuses the table ("does not
  fit into the flash (4MB)"): without a device attached it assumes 4 MB. `flash`
  detects the size, but the script now says 8mb rather than rely on that.
- Flashed the unchanged 0.2.0 firmware from `main` with the new table. Boot log
  (`captures/card210-first-flash.log`, git-ignored): the bootloader (ESP-IDF
  v6.1-beta1) prints the five partitions, "No factory image, trying OTA 0", "Loaded app
  from partition at offset 0x10000", "Set actual ota_seq=1 in otadata[0]" - exactly
  the self-initialisation research 006 predicted. 154-156 swaps/s, no panics.
- The Studio on workbench re-acquired the panel by itself within ~15 s of boot.
- `screeny-probe conformance --slow` against the device: **60 passed, 0 failed, 4
  skipped**, identical to before the change. (The Studio holds the source lock around
  the clock now: switch it off with `set_panel {"on":false}` for the run and back on
  after, or every frame rule sees BUSY.)
- `fw-run.sh` still tries the camera and prints "cam-daemon is not running"; harmless
  (`|| true`), left alone while the camera is disconnected.
