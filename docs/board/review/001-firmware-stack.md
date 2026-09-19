---
id: 001
title: Prove out the embassy firmware stack for Tidbyt Gen 1
type: research
hardware: no
depends: []
owner: worker-a (Claude)
branch: card/001-firmware-stack
---

## Goal

Determine whether a `no_std` embassy / esp-hal firmware can drive the Tidbyt's HUB75
panel while receiving UDP over WiFi at 30 fps, and pin down the exact crate set and
hardware facts we need to build it. If it cannot, say so clearly and name the
fallback (esp-idf-hal/std).

## Context

- Device: Tidbyt Gen 1. ESP32-D0WD-V3 rev 3.0, dual core 240 MHz, 8 MB flash, 64x32
  HUB75 panel. PSRAM presence unknown; find out.
- Tidbyt's stock firmware is open source (`tidbyt/firmware-esp32`, PlatformIO, uses
  ESP32-HUB75-MatrixPanel-I2S-DMA). `tidbyt/hdk` has hardware details. These give the
  pin map, panel driver-chip quirks and the brightness cap Tidbyt ships with.
- Toolchain on this machine: rustup toolchain `esp` (espup 0.17.1), `espflash`,
  `esptool`. Run `. ~/export-esp.sh` before building.
- Today is 2026-09. The esp-rs ecosystem moves fast (esp-hal 1.x, `esp-wifi` was
  renamed `esp-radio`, etc.). Check crates.io / GitHub for what is current; do not
  trust memory for version numbers or APIs.

## Questions to answer

1. HUB75 on plain ESP32 from Rust: is `esp-hub75` (liebman) the right driver? Does it
   support ESP32 I2S-parallel DMA, embassy async, 64x32? What colour depth / refresh
   rate does it achieve, how much RAM does its DMA buffer need, and how does frame
   update work (full framebuffer swap? tearing?). Alternatives if it falls short.
2. Exact Tidbyt Gen 1 pin map (R1 G1 B1 R2 G2 B2 A B C D E LAT OE CLK), the button
   GPIO, any other peripherals, and panel quirks (e.g. FM6126A init sequence, swapped
   colour channels).
3. WiFi + network stack: esp-radio + embassy-net (smoltcp) UDP receive. Known
   throughput/latency numbers, power-save settings, RAM cost, and how WiFi coexists
   with I2S DMA display refresh (core pinning, interrupt priorities, flicker reports).
4. mDNS / DNS-SD responder options in `no_std` (e.g. `edge-mdns`), and whether
   multicast works with embassy-net on ESP32.
5. Persistent config storage in `no_std` (for runtime-changeable WiFi credentials
   later): `esp-storage` + `sequential-storage` or equivalent.
6. Memory budget: internal SRAM split between WiFi, net buffers, DMA framebuffer,
   and our own frame buffers.
7. Brightness / current limiting approach.

## Deliverables

- `docs/research/001-firmware-stack.md`: conclusions first, then evidence with links.
  Include a recommended `Cargo.toml` dependency block with exact versions and
  features, the pin map table, a task/core architecture sketch, and a risk list.
- `spike/fw-skeleton/`: a minimal crate that **compiles** for `xtensa-esp32-none-elf`
  with the recommended dependency set, initialises embassy, WiFi (SSID
  `example-wifi1`, password `password9`), a UDP socket, and the HUB75 driver with the
  Tidbyt pin map, and draws a test pattern. It will not be flashed by you (no hardware
  access); compiling cleanly is the bar. Note anything you could not verify without
  hardware.

## Acceptance

The orchestrator can flash `spike/fw-skeleton` as the first hardware bring-up attempt
and knows from the report what to look for if it fails.

## Log

### 2026-09-19, worker-a, branch `card/001-firmware-stack`

**Answer: yes, embassy/`no_std` is viable, with caveats.** Full write-up in
`docs/research/001-firmware-stack.md`; spike in `spike/fw-skeleton/`.

`cargo build --release` succeeds from clean in 26 s with zero warnings, after
`. ~/export-esp.sh`. The binary holds the embassy executor, esp-radio WiFi
with DHCP, a UDP socket on the frame port, an `edge-mdns` DNS-SD responder and
`esp-hub75` driving the panel over I2S0 parallel DMA, all at once. Nothing has
been flashed.

What I measured, rather than assumed:

- **Memory**, from the linked binary: 262,144 bytes of internal DRAM total,
  of which 112 KB goes to the WiFi heap against esp-radio's documented
  47-57 KB for a station. Comfortable. Flash is ~608 KB of a 4 MB app slot.
- **Refresh rate against colour depth**, extracted from the driver's
  compile-time helper for our exact panel: at Tidbyt's 10 MHz clock, 6 bits
  per channel gives 154 Hz and 7 gives ~76 Hz. **There is no usable 8-bit
  configuration on this chip.** Card 002 should design for 5-6 bits.

Two premises in the card were wrong and are worth correcting on the board:

- **`tidbyt/firmware-esp32` does not exist.** The firmware is `tidbyt/hdk`.
  There is no published schematic or BOM for any Tidbyt.
- **Gen 1 does have PSRAM: 8 MB**, confirmed by the HDK's sdkconfig, by the
  same setting in the repo's Gen 1-only initial commit, and by a real Gen 1
  boot log. It does not help the display — ESP32 DMA cannot read PSRAM — but
  it is there if we want it for assets.

Three things that will bite whoever writes the real firmware:

1. **`esp-hal-embassy` is superseded by `esp-rtos`**, and `esp-wifi` by
   `esp-radio`. The card's version guesses are all stale.
2. **The version window is one release wide.** `esp-radio` 1.0.0-beta.1 is
   the only published WiFi release whose dependency tree unifies with
   `esp-hub75` 0.17 — 0.18.0 wants esp-hal 1.1, and beta.2 wants
   `esp-metadata-generated` 0.6. beta.2 has since been yanked. Everything is
   pinned with `=`.
3. **The esp-hal repository's own examples do not compile against published
   crates** — its in-tree `esp-rtos` is ahead of the release. Follow
   `esp-hub75`'s embassy example instead.

Surprises worth flagging:

- **`esp-hub75` does no driver-chip init and has no brightness control.**
  Tidbyt declares the panel FM6126A, which needs a two-register sequence
  before it lights. I ported `fm6124init()` into `src/panel_init.rs`; it is
  bit-banged before I2S takes the GPIOs, and it is untested. The missing
  brightness control is worse, because the only substitute is scaling pixel
  values, which costs about three of our six bits. **New card 020.**
- **The Gen 1 RGB channel order is not fixed across board revisions.** The
  stock firmware picks it at runtime from two ADC straps; the community ships
  a `_swap` build for the other case. **New card 021.** The startup test
  pattern is red/green/blue bands specifically so this is one glance.
- **esp-radio advertises an MTU of 1492, not 1500**, which caps the UDP
  payload at 1464 rather than 1472. The spike raises
  `ESP_RADIO_CONFIG_WIFI_MTU` to 1500 so the intended budget holds; card 003
  needs to know either way.
- Two more defaults that are wrong for us and are now set explicitly: the
  esp-rtos tick is 100 Hz (10 ms) against a 33 ms frame period, and
  `country_info` defaults to `"CN"`.

On the orchestrator's questions: `embassy-net` 0.9 has **no** default
features and all of `udp`/`dhcpv4`/`multicast`/`proto-ipv4`/`medium-ethernet`
are named explicitly; `PowerSaveMode` does default to `None` — verified in
`WifiController::new` — and is set explicitly anyway because the setter is
`unstable`; and the mDNS responder went in cleanly and is in the skeleton, on
port 49374 with the control port advertised in TXT.

What I could not verify: everything that needs hardware. Also the reset
button's GPIO, the ATECC608's I2C pins, the module part number, the stock
firmware's real refresh rate, any current measurement, how `esp-hub75`'s
clock-phase convention maps onto the C library's, and — the one I would test
first — whether multicast is delivered through esp-radio at all.
