# fw-skeleton

Compile-only spike for card 001. It has **never been flashed** — the card that
produced it had no hardware access, and compiling cleanly was the bar.

It exists to prove that one ESP32 binary can hold the embassy executor,
esp-radio WiFi, embassy-net UDP, an mDNS responder and esp-hub75 driving the
panel at once, and that the RAM they want together fits. The findings are in
`docs/research/001-firmware-stack.md`; read that before flashing this.

## Build

```sh
. ~/export-esp.sh
cd spike/fw-skeleton
cargo build --release
```

`rust-toolchain.toml` selects the `esp` toolchain and `.cargo/config.toml`
sets the target, the linker args and the esp-config environment overrides, so
there is nothing else to pass. Binary lands at
`target/xtensa-esp32-none-elf/release/fw-skeleton`.

## Before flashing

Do not flash without a verified stock backup in `backup/` — see `CLAUDE.md`.
Stock images are also downloadable (Gen 1 is `v10`), which is a second net,
not a substitute for the first.

## What to look for on first bring-up

Ranked by likelihood, with the fix. The long version is section 9 of the
research doc.

| Symptom | First thing to try |
|---|---|
| Panel stays dark | The FM6124 init in `src/panel_init.rs` — timing, or comment it out; an ICN2037 panel does not need it |
| Wrong colours | Gen 1 RGB order varies by board revision; permute R/G/B in `Hub75Pins16`. Card 021 |
| Image shifted or garbled | Clock phase: add the `invert-clock` feature |
| Ghosting | Raise `trail-blank-N`, then add `inter-row-blank-N` |
| Flicker once WiFi associates | `iram` and `Priority3` are already on; next is core pinning, then a lower pixel clock |
| `assert_pin` panics at boot | Someone edited one pin map and not the other; the message names the pin |

The startup test pattern is red, green and blue bands top to bottom, each
ramping left to right — so a channel swap is one glance and a wrong BCM depth
shows as banding in the ramp.
