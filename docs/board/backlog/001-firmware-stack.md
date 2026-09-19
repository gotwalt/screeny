---
id: 001
title: Prove out the embassy firmware stack for Tidbyt Gen 1
type: research
hardware: no
depends: []
owner:
branch:
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
