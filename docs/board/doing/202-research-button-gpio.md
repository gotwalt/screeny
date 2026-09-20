---
id: 202
title: Research - find the Tidbyt's button GPIO, and design the WiFi-reset gesture
type: research
hardware: no
depends: [001]
owner: worker-202
branch: card/202-research-button-gpio
---

## Goal

The owner wants (2026-09-20) to use the Tidbyt Gen 1's button, specifically to reset
WiFi (wipe stored credentials and drop into the captive portal). Nobody knows which
GPIO it is on: `firmware/src/tidbyt.rs` has `BUTTON_GPIO: Option<u8> = None` and the
instruction "do not guess it, measure it". Find it as far as that can be done
without the device, and hand the orchestrator a probe firmware that settles it in
one minute with the owner pressing the button.

This is the device-web track: cards 200-249, coordinated by the `firmware` session.
Sibling research cards running in parallel: 200 (flash/store/OTA) and 201 (HTTP,
soft-AP, portal). Stay out of their questions.

## Context

Read first: `CLAUDE.md`, `firmware/src/tidbyt.rs`, `docs/research/001-firmware-stack.md`
(the pin tables and "Other GPIOs"), `docs/research/000-bench-notes.md`,
`backup/README.md`.

- The stock firmware dump is `/Users/aaron/src/screeny/backup/tidbyt-stock-b48a0a4a00a4.bin`
  (8388608 bytes, git-ignored, so it is **not in your worktree**: read it by that
  absolute path, read-only, and never copy it or any large extract of it into git -
  it is Tidbyt's copyrighted binary and this repo is going public). Partition table
  at 0x8000: nvs 0x9000, otadata 0xe000, app0 0x10000+0x3f0000, app1
  0x400000+0x3f0000. It contains the log tag `tidbyt/button` and a hold-to-reset flow.
- Tools on this Mac: `esptool`, `espflash`, and the Xtensa binutils under
  `~/.rustup/toolchains/esp/xtensa-esp-elf/*/xtensa-esp-elf/bin/`
  (`xtensa-esp32-elf-objdump` etc.). Work in the session scratch/temp directory for
  extracts; clean up after yourself.
- Known pin use (do not probe as inputs): the 14 HUB75 lines in `tidbyt::pins`,
  GPIO16/17 (PSRAM), GPIO1/3 (UART0), GPIO6-11 (flash). GPIO13/15 are ADC board-ID
  straps. Candidates therefore include 0, 12, 14?, 34, 35, 36, 39 and whatever the
  static analysis says. GPIO34-39 are input-only with no internal pulls. GPIO0 is
  the boot strap and is also wired to the CP2102N's auto-reset circuit. GPIO12 is
  the flash-voltage strap (MTDI): reading it is fine, never drive it.
- The owner calls it the "reset pin". It may be a pinhole button. It is possible
  (check!) that it is wired to `EN`/`CHIP_PU` and is a hard reset rather than a GPIO
  - the stock firmware's hold-to-reset *flow* argues against that, but settle it
  from the binary if you can: a GPIO button implies `gpio_config`/`gpio_get_level`
  or an `iot_button`/`button_gpio` component with a pin number in a config struct.
- The community firmware `tronbyt` leaves `BUTTON_PIN` at -1 for every board; the
  public `tidbyt/hdk` repo has no button code. Look again anyway if you have web
  access: hdk, tronbyt/firmware-esp32, ESPHome and WLED forum threads, Tidbyt Gen 1
  teardowns. Cite URLs.

## Deliverables

1. **Static analysis** of the stock app image: extract the active app partition
   (check `otadata` for which), `esptool image-info`, map the segments, find the
   `tidbyt/button` string and the code that references it, and identify the GPIO
   number passed to the GPIO driver. Xtensa literal pools make this tractable: find
   the literal that holds the string's address, then the function(s) loading it, then
   the nearby `gpio_config_t`/pin mask constants. Report the pin with your confidence
   and the evidence (addresses, disassembly excerpts of a few lines - not dumps).
   Also report the hold duration(s) and what the stock flow does, if visible.
2. **A probe firmware**, compile-only, that the orchestrator will flash with the
   owner present: a separate binary in `firmware/` (`src/bin/gpio_probe.rs`, or a
   cargo feature - your call, but `cargo build --release` of the normal firmware
   must be unaffected) that does **not** start WiFi or the panel, configures every
   candidate pin as an input (try pull-up, then pull-down, in two phases announced
   on serial; input-only pins have no pulls), and logs every level change with the
   pin number and a timestamp at 115200 baud, rate-limited so a floating pin cannot
   flood the log (cap per-pin events per second and say so when capping). It prints
   a legend at boot and a one-line summary every 5 s. Build it with
   `. ~/export-esp.sh && cd firmware && cargo build --release --bin gpio_probe`
   (or your equivalent; give the exact command). Do not flash; you have no hardware.
3. **The gesture design**, for the firmware that will use the button: debounce,
   short press (suggest: show the status/identify screen with IP address for 10 s),
   long hold (suggest: >= 5 s wipes WiFi credentials and reboots into the portal,
   with an on-panel countdown so it cannot happen by accident, and release-to-cancel),
   very long hold (factory reset of all settings?). How it is sampled without
   costing the frame path anything (a GPIO interrupt + embassy `Input::wait_for_*`
   on core 0). What happens if the button is held at boot (GPIO0 would enter the
   ROM bootloader - if the pin is 0, say what that means for the design).
4. **The fallback**, in case the button turns out to be `EN` or unfindable: a
   power-cycle gesture (e.g. three power cycles each within 10 s of boot wipes
   WiFi), which needs a boot counter in flash (card 200 owns the store; just specify
   the behaviour and its failure modes, such as a brown-out loop).

All of it written up in `docs/research/008-button.md`, conclusions first, ending with
proposed build cards (titles + one paragraph each; do not write the card files).

## Acceptance

Either the pin is identified from the binary with evidence the orchestrator can
check, or the doc says exactly why it cannot be and the probe binary builds and is
ready to settle it. The normal firmware build is untouched.

## Log
