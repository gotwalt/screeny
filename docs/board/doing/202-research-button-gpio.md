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

### 2026-09-19 — static analysis of the stock image: it is GPIO15

Worked in the session scratch dir, never in the tree. Steps:

- `otadata` at 0x e000: slot0 `ota_seq=11` (CRC valid), slot1 `ota_seq=10` (CRC valid),
  both `ota_state=VALID`. Boot slot = `(11-1) % 2 = 0` → **app0 at 0x10000 is the
  running app**. `esptool image-info`: project `tidbyt`, version 33426, built
  Feb 20 2024, ESP-IDF 5.1.2. app1 is version 35369, Aug 6 2024 — newer build,
  older ota_seq. Analysed both.
- Surprise worth recording: the flash MMU maps the DROM/IROM segments 8 bytes
  lower than the load addresses in the segment headers. Empirically (scored 5410
  candidate pointers against string starts) `DROM VA = 0x3f400000 + app_file_offset`
  and `IROM VA = 0x400d0000 + app_file_offset - 0x60000`. My first pass used the
  header addresses, found zero references to any string, and looked like a dead end.
- `tidbyt/button` is at DROM 0x3f402a7c (app0). One literal referencing it:
  IROM 0x400d0538. Xtensa `.literal` sections are all collected at the head of
  `.flash.text`, so the whole module's pool sits at 0x400d0538..0x400d0548.
- Decoded every `l32r` in IROM/IRAM by hand to find the users of that pool, then
  disassembled with `xtensa-esp32-elf-objdump -D -b binary -m xtensa`.

**The decisive artefact** is a 24-byte `gpio_config_t` template in DROM at
0x3f402ac0 (app0) / 0x3f402ce8 (app1) — byte-for-byte identical in both builds:

    00 80 00 00 00 00 00 00 | 01 00 00 00 | 01 00 00 00 | 00 00 00 00 | 03 00 00 00
    pin_bit_mask = 0x8000 (GPIO15), mode = GPIO_MODE_INPUT,
    pull_up_en = 1, pull_down_en = 0, intr_type = GPIO_INTR_ANYEDGE

`tidbyt_button_init` (app0 0x400d57a0) `memcpy`s those 24 bytes onto its stack and
passes them to `gpio_config`, and the press test (0x400d57c0) is
`gpio_get_level(15) == 0` — verified by disassembling the callee at 0x4012182c and
seeing it index `GPIO_IN_REG` at GPIO base + 0x3c. Scanning all of DROM for
plausible input-mode `gpio_config_t` structs turns up exactly one isolated hit:
GPIO15.

Stock flow, from the `tidbyt/boot` function at 0x400d3838: timers → button init →
if pressed at boot, WARN "Reset button is being held. Keep holding for 5 seconds"
(`%d` is a literal `movi.n a15, 5`), poll until either release (WARN "Reset button
released. Reset sequence aborted.") or the uptime double reaches 5 000 000 µs
(literal 0x415312d0_00000000 = 5e6 as an IEEE-754 double), then WARN "Erasing NVS."
and `nvs_flash_erase`. Runtime presses go through `gpio_isr_handler_add` (a per-pin
callback table at 0x400d0850) into a handler that delays 100 ms, re-reads the pin,
and only then logs "reset button event pressed" — a 100 ms software debounce.

Web research (a parallel search) landed on the same pin independently, from a stock
boot log a user posted on Tidbyt's forum: `GPIO[15]| InputEn: 1| OutputEn: 0|
Pullup: 1| Pulldown: 0| Intr:3` at the exact millisecond as `Reset button is being
held`. Two independent lines of evidence, same answer. URLs are in the write-up.

Conflict to flag for the orchestrator: `firmware/src/tidbyt.rs` calls GPIO15
`BOARD_ID_ADC_B`. That is also true — but only in app1 (Aug 2024), which has
`Couldn't adc read IO13` **and** `Couldn't adc read IO15`. app0, the running image,
has neither string. So GPIO15 is dual-purpose on this board. Detail in the doc.

### 2026-09-19 — the probe binary

`firmware/src/bin/gpio_probe.rs`, plus a `gpio-probe` feature and two explicit
`[[bin]]` sections in `firmware/Cargo.toml`. The probe bin carries
`required-features = ["gpio-probe"]`, which is what keeps it out of the normal
build. Verified, in this order: built the probe (ELF at
`firmware/target/xtensa-esp32-none-elf/release/gpio_probe`), then ran a plain
`cargo build --release` — it compiled `screeny-fw` only and left the probe
artifact's timestamp untouched. No warnings from either build.
`espflash save-image` turns the probe ELF into a 93,360-byte application image,
so it is a valid flashable binary. **Not flashed:** this card is `hardware: no`.

API checked against the real `esp-hal =1.2.2` sources rather than memory:
`Input::new(pin, InputConfig)`, `Input::apply_config(&InputConfig)`,
`Input::level()`, `InputConfig::with_pull`, `AnyPin: InputPin`, `Pin::number()`,
`Instant::now().duration_since_epoch().as_millis()`. `Input` has no
`pin_number()`, so the pin-table assertion runs on the `AnyPin` array before the
inputs are constructed. The async `wait_for_any_edge` / `wait_for_rising_edge`
the gesture design leans on are real too (`gpio/asynch.rs`).

GPIO12 is probed but gets **no internal pull in either phase** — the card's own
rule is "reading it is fine, never drive it", and the benefit of pulling the
flash-voltage strap was not worth arguing about. It is called out in the legend
the probe prints, so the transcript explains itself.

### 2026-09-19 — write-up, and a small honest edit to `tidbyt.rs`

`docs/research/008-button.md`: conclusions, the static analysis with addresses
and short disassembly excerpts, the independent web confirmation with URLs, the
GPIO15-is-also-a-strap loose end, the probe and exactly what to ask the owner to
do, the gesture design (thresholds table + the one-paragraph version the card
asked for), the power-cycle fallback with its failure modes, and five proposed
build cards. No card files written, as instructed.

`firmware/src/tidbyt.rs`: `BUTTON_GPIO` stays `None`, because nothing has been
measured on this unit — but the comment now says what the binary says, points at
the research doc and the probe, and says which card is allowed to set it to
`Some(15)`. `pins::BOARD_ID_ADC_B` gained a cross-reference. Comments only; the
release build was re-run afterwards and is clean.

Temp extracts (the two app partitions, a disassembly chunk and the probe image)
lived only in the session scratch directory and were deleted. No process left
running, no serial port opened, no camera.
