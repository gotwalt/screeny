# Device web: status, settings, firmware update, captive portal, the button

**Status: decisions recorded, design pending research (cards 200, 201, 202).** This
file is the source of truth for the device-web track (cards 200-249, coordinated by
the `firmware` Claude session). The sections marked *pending* are filled in from
`docs/research/006`, `007` and `008` when those cards land.

## What the owner asked for (2026-09-20)

1. A web server on the device: diagnostics and status, network settings, and a
   **safe** firmware update.
2. A use for the Tidbyt's button, specifically resetting WiFi.
3. A captive-portal WiFi network when the device is not joined to one, with the
   panel showing the network name and instructions, and a QR code if the panel can
   carry one.

## Decisions

| # | Decision | Who, when |
|---|---|---|
| 1 | **The portal screen carries a WiFi QR code.** Measured on the real panel: version 2-L (25x25 modules), `WIFI:T:nopass;S:screeny-4a00a4;;` (exactly the 32 bytes 2-L holds), one LED per module, standard polarity (lit white background, dark modules off), 3-pixel lit quiet zone, default brightness. The owner's phone scanned it "easily". | owner's phone, 2026-09-20 |
| 2 | **The setup AP is an open network** named `screeny-<id>`. The home PSK crosses it in clear during setup; the owner does not treat that PSK as a secret (spec 8.4). | owner, 2026-09-20 |
| 3 | **HTTP is unauthenticated on the LAN**, settings and firmware upload included - the same posture as `SET_WIFI` and `REBOOT` (spec 8.4). The API is shaped so a PIN can be added (parked card 041). An upload is still validated as a `screeny-fw` image before the boot slot changes. | owner, 2026-09-20 |
| 4 | **The button is real and reachable** (on the back; the owner can press it). Its GPIO is unknown: card 202 finds it statically and ships a probe firmware; the orchestrator runs the probe with the owner pressing. | owner, 2026-09-20 |
| 5 | **The serial console of spec 8.1 is superseded** by the portal and the settings page (CLAUDE.md: "do not build other schemes"). `SET_WIFI` (8.2) stays and writes the same store. The spec is edited when the store lands, with notice to the software session. | orchestrator, 2026-09-20 |
| 6 | **Compile-time credentials become optional**: when present they seed an empty store (bench convenience); a build without them boots straight to the portal. That is what a public repo needs. | orchestrator, 2026-09-20 |
| 7 | **The frame path is the product.** No HTTP request, flash write or portal activity may cost a frame at 30 fps, except a firmware update, which is allowed to take the panel over with an "updating" screen. | standing |

## Working agreement with the software session

`firmware/`, the serial port and flashing belong to the firmware session; cards
200-249. `crates/proto`, `crates/receiver` and `docs/design/protocol-v1.md` are
shared: either session tells the other before changing them. After any flash the
regression check is `cargo run --release -p screeny-probe -- --addr 192.168.7.221
conformance --slow` (firmware 0.2.0: 60 pass, 0 fail, 4 skip). Once the Studio runs
on workbench it holds the source lock around the clock; release it with
`POST http://workbench.local:8787/api/v1/set_panel {"on":false}` before bench work
and give it back with `{"on":true,"to":"screeny-4a00a4"}`.

## What the research settled

### Flash, store, OTA (card 200, `docs/research/006-flash-store-ota.md`)

- Partition table: `nvs` 0x9000+0x5000, `otadata` 0xE000+0x2000, `ota_0`
  0x10000+2 MB, `ota_1` 0x210000+2 MB, `screeny` (settings) 0x410000+64 KB. No
  `factory`, no coredump. Today's image is 743 KB, 35% of a slot.
- **`espflash` never touches `otadata`.** With no `factory` partition a serial flash
  writes `ota_0` while a stale `otadata` may still select `ota_1`. `tools/fw-run.sh`
  must always pass `--partition-table firmware/partitions.csv --erase-data-parts ota`.
- **Every flash write must park core 1**: `esp-storage`'s default returns
  `OtherCoreRunning` while the display owns core 1. Use `multicore_auto_park()` and
  the `critical-section` feature; never `multicore_ignore()`; only core 0 touches
  flash. The park is per 4 KB sector (~50 ms): the circular DMA keeps the panel lit,
  only the dither phase freezes. Small config writes need no special handling; an OTA
  shows a static "updating" screen with dither off.
- **Open risk, bench only**: core 0 has interrupts masked for the same ~50 ms per
  sector erase. Whether esp-radio's WiFi survives that during an upload is the first
  thing to measure on hardware.
- **Rollback**: espflash's bundled bootloader has `APP_ROLLBACK_ENABLE` off. App-side
  revert (mark the running slot `Invalid`, reboot) covers every image that boots but
  never becomes healthy. Only a rebuilt ESP-IDF v6.1 bootloader covers an image that
  crashes before the confirm code runs. **Owner decision pending**: build that
  bootloader (ESP-IDF is not installed here; a docker image would do) and commit the
  26 KB blob.
- Health criterion (proposed): WiFi + DHCP, and one HTTP request or 120 s uptime, and
  at least one display swap; never before 60 s; revert at 180 s.
- Buffers held across an `await` in a task land in `.bss` and come out of core 0's
  stack (the spike's 11 KB did). Scratch buffers are heap-allocated for the duration
  of the operation, never in a future.
- Settings: `sequential-storage 8.0.1` map over the `screeny` partition through
  `BlockingAsync`; record logic in `crates/settings` (card 211).

### The button (card 202, `docs/research/008-button.md`)

- **GPIO15, active low, internal pull-up**, read out of the stock image's
  `gpio_config_t` and confirmed by a stock boot log on Tidbyt's forum. Not `EN`, not
  GPIO0: holding it at boot cannot strand the device in the ROM bootloader. GPIO15 is
  also the MTDO strap (a held button silences the ROM boot log - harmless) and one of
  the two board-ID ADC straps.
- `BUTTON_GPIO` stays `None` until the bench confirms it: `cargo build --release
  --features gpio-probe --bin gpio_probe` in `firmware/`, flash, the owner presses.
  Phase B of the probe (pull-down) says whether anything external holds the pin up.
- Gestures (proposed): short press = identify/status screen for 10 s; held past 1 s
  starts an on-panel countdown, release cancels; 5 s wipes WiFi and reboots into the
  portal; 15 s factory-resets all settings. Held at boot runs the same ladder. One
  embassy task on core 0 in `wait_for_any_edge`, 30 ms debounce.
- Fallback (three quick power cycles) is parked unless the probe says the button is
  unusable.

### HTTP, soft-AP, portal (card 201) - pending

## Build order

| card | what | hardware |
|---|---|---|
| 210 | partition table + `tools/fw-run.sh` flags; first flash of the new layout | yes (orchestrator) |
| 211 | `crates/settings`, host-tested against the real map (in flight) | no |
| 212 | firmware: the store on the `screeny` partition, settings loaded at boot, debounce task, `ERR_STORAGE`, `SET_WIFI` wired, compile-time credentials optional (delivers 063) | yes |
| 203 | bench: confirm GPIO15 with the probe, owner pressing; set `BUTTON_GPIO` | yes (orchestrator + owner) |
| 22x | HTTP status + settings, soft-AP + portal + QR screen, sim support (081) - from card 201 | mixed |
| 23x | button task, hold ladder + countdown, wipe -> portal | yes |
| 24x | OTA: stage + validate, activate/confirm/revert, "updating" screen, the interrupt-window measurement, rollback bootloader | yes |
