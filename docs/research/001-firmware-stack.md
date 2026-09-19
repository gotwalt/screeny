# 001 - Firmware stack for the Tidbyt Gen 1

Status: research complete, nothing run on hardware.
Spike: `spike/fw-skeleton/` (compiles, never flashed). **Card 016 deleted that
directory**: everything in it that was right now lives in `firmware/`, and git
history keeps the rest. Read the findings below as the reasoning that produced
`firmware/`, not as a description of code you can open.

## Conclusion

**Yes, `no_std` embassy / esp-hal is viable for 30 fps UDP plus HUB75 on this
chip — with caveats.** All five pieces (embassy executor, esp-radio WiFi,
embassy-net UDP, esp-hub75 I2S-parallel DMA, and our own frame buffer) build
into one binary for `xtensa-esp32-none-elf` today, and the RAM they want fits
inside the ESP32's internal SRAM with about 112 KB left over for the WiFi
heap. We do not need the `esp-idf-hal` / `std` fallback.

The caveats, in the order they are likely to bite:

1. **The display driver has no driver-chip init and no brightness control.**
   `esp-hub75` assumes a plain shift-register panel. Tidbyt's panel is
   declared FM6126A and needs a two-register init sequence before it will
   light. The spike ports that sequence (`spike/fw-skeleton/src/panel_init.rs`);
   it has never been run. Separately, `esp-hub75` cannot dim the panel the way
   Tidbyt does (output-enable duty), so our only lever is scaling pixel values,
   which costs colour depth exactly where we have least to spare. See card 020.
2. **Colour depth and refresh rate trade against each other, hard.** At
   Tidbyt's own 10 MHz pixel clock, 6 bits per channel gives 154 Hz refresh
   and 7 bits gives 76 Hz. There is no configuration that gives us 8 bits at a
   flicker-free rate on this chip. Card 002 should design for **5-6 bits per
   channel, before the brightness cap eats more**.
3. **WiFi and the display contend, and the mitigations are known but
   untested.** The driver's own documentation names the failure (WiFi's
   low-priority interrupt handlers delaying the refresh ISR by hundreds of
   microseconds) and the two fixes (`iram` feature, refresh ISR at
   `Priority3`). Both are enabled in the spike. Whether that is sufficient on
   this board is the single biggest open question.
4. **The exact crate versions are a narrow window.** `esp-radio` 1.0.0-beta.1
   is the only published WiFi release that shares a dependency tree with
   `esp-hub75` 0.17. beta.2, released the next day, already breaks it. Pin
   everything; expect to re-solve this puzzle on every bump.

---

## 1. Recommended dependency set

Exact versions, all verified to resolve together and compile (this is
`spike/fw-skeleton/Cargo.toml`):

```toml
esp-hal                = { version = "=1.2.2",        features = ["esp32", "unstable", "log-04"] }
esp-rtos               = { version = "=0.4.0",        features = ["esp32", "embassy", "esp-radio", "log-04"] }
esp-alloc              = { version = "=0.11.0",       features = ["esp32"] }
esp-bootloader-esp-idf = { version = "=0.6.0",        features = ["esp32"] }
esp-backtrace          = { version = "=0.20.0",       features = ["esp32", "panic-handler", "println"] }
esp-println            = { version = "=0.18.0",       features = ["esp32", "log-04", "auto"] }
esp-radio              = { version = "=1.0.0-beta.1", features = ["esp32", "wifi", "unstable", "log-04"] }

embassy-executor = "0.10.0"
embassy-time     = "0.5.1"
embassy-sync     = "0.8.0"
embassy-net      = { version = "0.9.1", features = ["dhcpv4", "medium-ethernet", "udp", "proto-ipv4", "multicast"] }

esp-hub75         = { version = "=0.17.0", default-features = false, features = [
  "esp32", "log", "iram", "skip-black-pixels", "circular-dma",
  "tail-closes-latch", "trail-blank-8", "invert-blank", "invert-oe",
] }
embedded-graphics = "0.8.2"

# discovery
edge-mdns        = { version = "=0.8.0", default-features = false, features = ["io"] }
edge-nal         = "=0.7.0"
edge-nal-embassy = "=0.9.0"
rand_core        = { version = "0.10", default-features = false }

# esp-radio's blobs misbehave below opt-level 2.
[profile.dev.package.esp-radio]
opt-level = 3
```

Plus `rust-toolchain.toml` with `channel = "esp"` and a `.cargo/config.toml`
carrying `target = "xtensa-esp32-none-elf"`,
`rustflags = ["-C", "link-arg=-Tlinkall.x", "-C", "link-arg=-nostartfiles"]`
and `[unstable] build-std = ["alloc", "core"]`.

### What changed in the ecosystem since the card was written

- `esp-wifi` **was** renamed: the maintained crate is now `esp-radio`
  (`esp-wifi` stops at 0.15.1). `esp-radio` 0.18.0 is the latest 0.x, and
  1.0.0-beta.1 / beta.2 are the current pre-1.0 line.
- `esp-hal-embassy` **is superseded by `esp-rtos`**. `esp-rtos` is a real
  scheduler (threads, queues) that both embassy and esp-radio sit on top of;
  `esp-hal-embassy` 0.9.1 still exists but the esp-hal repo's own WiFi
  examples and `esp-hub75`'s embassy example both use `esp-rtos` now. Entry
  points are `#[esp_rtos::main]` and
  `esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0)`.
- Do **not** enable any `arch-*` feature on `embassy-executor`; `esp-rtos`
  provides the executor.

### The version window, precisely

The constraint that decides everything is `esp-metadata-generated`:

| crate | `esp-hal` | `esp-metadata-generated` | `esp-sync` | `esp-alloc` |
|---|---|---|---|---|
| `esp-hub75` 0.17.0 | `~1.2.0` | `^0.5.0` | `^0.3.0` | - |
| `esp-radio` 1.0.0-beta.1 | `~1.2.0` | `^0.5.1` | `^0.3.0` | `^0.11.0` |
| `esp-radio` 1.0.0-beta.2 | `~1.2.0` | **`^0.6.0`** | `^0.3.0` | `^0.11.0` |
| `esp-radio` 0.18.0 | **`~1.1.0-rc.0`** | `^0.4.0` | `^0.2.1` | `^0.10.0` |
| `esp-rtos` 0.4.0 | `~1.2.0` | - | `^0.3.0` | `^0.11.0` |
| `esp-storage` 0.10.0 | `~1.2.0-rc.0` | `^0.5.0` | `^0.3.0` | - |

So `esp-radio` 0.18.0 is too old (wants esp-hal 1.1) and beta.2 is too new
(wants `esp-metadata-generated` 0.6, which `esp-hub75` 0.17 does not accept).
**beta.1 is the only version that works** — and **beta.2 has since been
yanked from crates.io**, one day after release, which is consistent with that
conflict though the maintainers did not say so. When bumping, bump `esp-hub75`
and `esp-radio` together and check this column first.

Two further traps in this area:

- **The esp-hal repository's own examples do not compile against the
  published crates.** The in-tree `esp-rtos` is ahead of the released 0.4.0:
  the examples call `esp_rtos::start(timg0.timer0)`, while published 0.4.0 is
  `esp_rtos::start(timer, int0: FROM_CPU_INTR0<'static>)`, and
  `start_second_core` likewise gained a `FROM_CPU_INTR1` argument. Follow
  `esp-hub75`'s embassy example instead: it builds against published crates
  and is therefore correct.
- **`embassy-net` 0.9 has no default features.** `udp`, `dhcpv4`,
  `multicast`, `proto-ipv4` and `medium-ethernet` must all be named
  explicitly. `medium-ethernet` in particular is not optional: esp-radio's
  `Interface` reports `HardwareAddress::Ethernet`.

Toolchain actually used: rustup toolchain `esp`, `rustc 1.97.0-nightly
(8ea53bcd7 2026-07-08)`, LLVM 21.1.3, via `. ~/export-esp.sh`.

---

## 2. Tidbyt Gen 1 hardware facts

Two corrections to the card's premise:

- **`github.com/tidbyt/firmware-esp32` does not exist** (404). The firmware
  *is* `tidbyt/hdk`. There is no published schematic or BOM anywhere.
- **The Gen 1 does have PSRAM: 8 MB.** See below.

### Pin map

From `tidbyt/hdk`, `src/display.cpp:23-40` — the `#else` branch of
`#ifdef TIDBYT_GEN2`. Gen 1 is the default PlatformIO env (`platformio.ini:24-26`
defines `[env:tidbyt]` with no build flags; `[env:tidbyt-gen2]` adds
`-DTIDBYT_GEN2`), and the same block appears unconditionally in the repo's
initial commit `839ec3e`, from before Gen 2 existed.

| Signal | Gen 1 GPIO | Gen 2 GPIO (for contrast) |
|---|---|---|
| R1 | 21 | 5 |
| G1 | 2 | 23 |
| B1 | 22 | 4 |
| R2 | 23 | 2 |
| G2 | 4 | 22 |
| B2 | 27 | 32 |
| A | 26 | 25 |
| B | 5 | 21 |
| C | 25 | 26 |
| D | 18 | 19 |
| E | **-1, absent** | -1 |
| LAT | 19 | 18 |
| OE | 32 | 27 |
| CLK | 33 | 15 |

Cross-checked against an independent ESPHome-on-Tidbyt writeup
(<https://community.home-assistant.io/t/esphome-on-tidbyt-gen-2/830367>),
which lists the identical Gen 1 map.

1/16 scan, so A-D are enough and there is no E line. `hdk/src/display.cpp:35`
comments "assign to pin 14 if using more than two panels", which is why the
spike hands GPIO14 to `esp-hub75` as its mandatory fifth address pin: it is
free, and Tidbyt already earmarked it. Row addresses never exceed 15, so it
is never driven.

**The RGB channel order is not stable across Gen 1 board revisions.** The
stock firmware picks its RGB assignment at run time from a detected board
revision — its log format string is
`Display for rev %d, RGB=%d,%d,%d` (tag `tidbyt/display`, recovered from the
factory image). The tronbyt firmware ships two Gen 1 targets,
`tidbyt-gen1` and `tidbyt-gen1_swap`, the latter rotating R -> B -> G -> R
(`tronbyt-fw/main/display.cpp:205-209`). **Budget for a colour-order flag in
the firmware**, and expect the first bring-up to possibly show the wrong
colours.

### Other GPIOs

| GPIO | Use | Confidence |
|---|---|---|
| 16, 17 | PSRAM CS / CLK (`hdk/sdkconfig:1188-1189`) | certain, do not touch |
| 13, 15 | analogue board-ID straps: the firmware ADC-reads both to get generation + revision (`Couldn't adc read IO13`, `Hardware generation: gen%d, %dmV, %dmV`) | certain |
| ? | I2C to an ATECC608 crypto element (log tag `tidbyt/atca`, cryptoauthlib in the image) | present, **GPIOs unverified** |
| ? | the reset button (log tag `tidbyt/button`, hold-to-reset flow) | present, **GPIO unverified** |
| 14 | free | inferred from the CH_E comment |

No speaker, no microphone, no touch pad on Gen 1: `hdk/src/audio.c` and
`hdk/src/touch.c` are entirely `#ifdef TIDBYT_GEN2` with Gen 1 no-op stubs,
and Tidbyt's FAQ says the same. No evidence of an ambient light sensor —
no ALS part string in the factory image, and the only ADC use is the board-ID
strap.

### PSRAM: yes, 8 MB

`hdk/sdkconfig:1152` `CONFIG_SPIRAM=y`, `:1162` 40 MHz, `:1181-1182`
bank-switching enabled with 25 reserved banks — and this is already true in
the Gen 1-only initial commit, so it is not a Gen 2 artefact. A real Gen 1
boot log (tronbyt issue #123) confirms:

> `quad_psram: This chip is ESP32-D0WD` / `esp_psram: Found 8MB PSRAM device` /
> `Virtual address not enough for PSRAM, map as much as we can. 4MB is mapped` /
> `Free PSRAM: 4087312` / `Free internal RAM: 103423`

**This does not help the display.** ESP32 DMA cannot read PSRAM, so the HUB75
framebuffers must live in internal SRAM regardless. `esp-hal` 1.2 does support
ESP32 PSRAM (a `Psram` driver object plus `esp_alloc::psram_allocator!`, up to
4 MB mapped; the bank-switched remainder needs the himem API, which esp-hal
does not provide). Worth having for assets or a frame history later; not
needed now, and the spike leaves it off.

### Flash and security

8 MB, DIO, 40 MHz (read out of the factory image's bootloader header).
Partition table, byte-identical to `hdk/boards/default_8mb.csv`:

```
nvs      data nvs   0x9000   0x5000
otadata  data ota   0xe000   0x2000
app0     app  ota_0 0x10000  0x3f0000
app1     app  ota_1 0x400000 0x3f0000
```

**No secure boot, no flash encryption** (`hdk/sdkconfig:315-316`, `:2144`), and
the factory image is plain text. The ATECC608 is for cloud auth, not boot
security. So we are free to flash, and the stock image is recoverable — the
published stock binaries live at
`https://storage.googleapis.com/tidbyt-public-firmware/<ver>/{bootloader,partitions,firmware}.bin`,
Gen 1 being `v10`. That is a second safety net behind our own
`backup/tidbyt-stock-*.bin`.

### Panel and brightness

`hdk/src/display.cpp:44-66` is the whole configuration:

```c
HUB75_I2S_CFG mxconfig(64, 32, 1, pins,
                       HUB75_I2S_CFG::FM6126A,  // driver chip
                       true,                    // double buffering
                       HUB75_I2S_CFG::HZ_10M,   // 10 MHz pixel clock
                       1,                       // latch blanking
                       true);                   // clkphase (Gen 1; Gen 2 is false)
```

- **Driver chip declared FM6126A**, which in the HUB75 library routes to
  `fm6124init()` — the register-11/12 init sequence. The tronbyt maintainer
  reports the actual silicon is an ICN2037, which takes the same code path and
  does not need the init; "FM6126A is just what Tidbyt used in their published
  firmware"
  (<https://github.com/mrcodetastic/ESP32-HUB75-MatrixPanel-DMA/discussions/770>).
  Either way, running the sequence is safe and skipping it is not.
- **Pixel clock 10 MHz.** Tidbyt's own library fork has a branch adding
  `HZ_13M = 13333333` with the message "Handy for being able to hit a refresh
  rate of exactly 200 Hz", so they were pushing higher internally; what the
  shipping firmware actually uses is unverified.
- **Latch blanking 1**, clkphase the library default on Gen 1 (Gen 2 is the
  one that deviates).
- **Brightness: default 30/255, maximum 100/255** (`hdk/src/display.h:6-8`),
  fed to `setBrightness8()`, which is output-enable duty across the 64
  columns. So Tidbyt's own "100%" is about **39% duty** and their shipping
  default is about **12%**. tronbyt reverse-engineered the same numbers
  independently and documents 100% -> 100/255 = 39% as the genuine-Tidbyt
  convention.

Power is 5 V over USB-C. **No measured current draw at full white exists in
any source I could find** — do not quote a number. Whether the 39% cap is
specifically a USB power budget is not stated anywhere; it has that effect
regardless.

---

## 3. HUB75 from Rust: `esp-hub75` is the right driver

`esp-hub75` 0.17.0 (liebman), released 2026-09-14, explicitly supports the
plain ESP32 via **I2S in parallel mode with DMA**, which is the same mechanism
the C library uses. It is actively developed (0.12 through 0.17 all shipped in
the last two months), has hardware-in-the-loop tests, and lists a generic
64x32 1/16-scan panel among its tested hardware. There is no serious
alternative in Rust; the fallback would be dropping to `esp-idf-hal` and the
C library, which we do not need.

### How it works, and what that costs

Two framebuffer families, from the companion crate `hub75-framebuffer` 0.12:
*standard*, which pre-renders a full copy per BCM bit-weight, and *bitplane*,
which stores one bit per pixel per plane and lets DMA descriptors assemble the
BCM output on the fly. Same picture, much less RAM. Use bitplane.

With `circular-dma` the DMA engine starts once and loops forever, and a buffer
swap is a pointer-delta update rather than a stop/restart — **no interrupts at
all in steady state**, one ISR per swap to apply it. That is exactly the shape
we want for a 30 fps stream, and it is supported on the ESP32.

Update model is a full framebuffer swap: you draw into the back buffer, call
`swap()`, and await. Tearing is avoided because the swap is applied at a frame
boundary. Two buffers are mandatory, so budget both.

### Refresh rate against colour depth

Measured out of the driver's own compile-time `refresh_hz::<FB>()` helper for
our 64x32 panel (these are exact, extracted from the build):

| BCM planes (bits/channel) | buffer size, each | refresh @ 10 MHz | refresh @ 19 MHz |
|---|---|---|---|
| 4 | 8,208 B | ~617 Hz | 1234 Hz |
| 5 | 10,260 B | ~299 Hz | 597 Hz |
| **6** | **12,312 B** | **154 Hz** | **293 Hz** |
| 7 | 14,364 B | ~76 Hz | 145 Hz |
| 8 | 16,416 B | ~38 Hz | 72 Hz |

(10 MHz figures for 6 planes are measured at 154 Hz; the others scale as
`1/(2^planes - 1)` and are given to one significant figure.)

**There is no 8-bit configuration that is usable.** 72 Hz at the ESP32's
maximum clock is below what the eye tolerates on a panel this bright, and 38 Hz
at Tidbyt's clock is unusable. Tidbyt themselves run 8-bit
`PIXEL_COLOR_DEPTH_BITS` in the C library, but that library trades depth for
rate dynamically via `lsbMsbTransitionBit` — it does not actually show 8 clean
bits either.

**Recommendation for card 002: design the codec for 5 or 6 bits per channel.**
Six planes at Tidbyt's own 10 MHz clock gives 154 Hz, which is comfortable, and
the spike is configured that way. Going to 19 MHz would buy 293 Hz or a seventh
plane, but 10 MHz is the only clock this panel is known to survive, so that is
a second-session experiment, not a bring-up assumption.

### What `esp-hub75` does not do

- **No driver-chip init.** No FM6126A/FM6124/ICN2038S register sequence, no
  concept of a "driver" setting. The spike ports `fm6124init()` into
  `src/panel_init.rs` and bit-bangs it over the GPIOs before handing them to
  I2S, which is where the C library does it too.
- **No brightness control.** There is no output-enable duty setting, which is
  the mechanism Tidbyt uses. The nearest levers are the `trail-blank-N` and
  `inter-row-blank-N` features, which blank a few clocks per row — enough to
  kill ghosting, nowhere near enough to reach 12% duty. That leaves scaling
  pixel values, which at a 30/255 cap throws away roughly 3 of our 6 bits.
  **This is the sharpest constraint on picture quality and it deserves its own
  card — see card 020.**
- **No tiling-free 4-address-line mode.** `Hub75Pins16` demands five address
  pins whether or not the panel has an E line; hence GPIO14.

### Panel tuning features to try on first bring-up

The README's known-good ESP32 set is
`circular-dma, iram, tail-closes-latch, trail-blank-8, invert-blank, invert-oe`,
which is what the spike uses. `invert-blank` + `invert-oe` are a pair and are
specific to the I2S and LCD_CAM backends: those peripherals drive all pins low
when a transfer ends, which un-blanks the panel between transfers, so the blank
pin is inverted in hardware and the OE bit inverted in the framebuffer to
compensate. If the panel ghosts, raise `trail-blank-N` or add
`inter-row-blank-N`; if it is too dim, lower them. If the image is shifted by a
pixel or garbled, try `invert-clock` — Tidbyt passes clkphase `true` on Gen 1,
which is the C library's default, and mapping that onto `esp-hub75`'s
convention is a guess until someone looks at a panel.

---

## 4. WiFi, embassy-net and coexistence with the display

### API shape (esp-radio 1.0.0-beta.1, read from the crate source)

There is no longer a public `esp_radio::init()`; the entry point is
`WifiController::new(peripherals.WIFI, ControllerConfig)`, and the
`embassy-net` driver comes from `esp_radio::wifi::Interface::station()`. The
async methods are `connect_async()`, `disconnect_async()`,
`wait_for_disconnect_async()` and `scan_async()`.

**Power save is already off**, contrary to what the ESP-IDF default would lead
you to expect. `WifiController::new` contains, verbatim:

```rust
// Set a sane default power saving mode. The blob default is not the best for bandwidth.
controller.set_power_saving(PowerSaveMode::default())?;
```

and `PowerSaveMode::None` carries `#[default]`. So we get no modem sleep out
of the box. **Set it explicitly anyway**: `set_power_saving` is
`#[instability::unstable]` and the behaviour is guaranteed by a code comment,
not by a stability promise. (`power_save` was removed from `ControllerConfig`,
so the setter is the only route.) The spike sets it. Enabling `unstable` on
`esp-radio` is required to reach it.

### Three settings whose defaults are wrong for us

1. **`ESP_RADIO_CONFIG_WIFI_MTU` defaults to 1492**, not 1500
   (`esp-radio/esp_config.yml`). That is what esp-radio advertises to
   embassy-net, and it caps a UDP payload at **1464 bytes, not 1472** —
   eight bytes smaller than the budget card 003 is designing against. WiFi
   carries a 2304-byte MSDU, so the spike sets the option to 1500 in
   `.cargo/config.toml` and keeps the 1472 figure. Card 003 should either
   rely on that setting or shrink the budget; it must not assume 1472 by
   default.
2. **`ESP_RTOS_CONFIG_TICK_RATE_HZ` defaults to 100**, a 10 ms scheduling
   quantum against a 33 ms frame period. An earlier esp-hal issue measured
   idle ping at 12.1-12.6 ms at 100 Hz versus 3.0-4.4 ms at 1000 Hz. The
   spike raises it to 1000.
3. **`ControllerConfig::country_info` defaults to `"CN"`**, which is the
   wrong channel set here. The spike sets `"US"`.

Other `ControllerConfig` defaults, because they are where the WiFi heap goes:
10 static RX buffers at ~1.6 KB each, 32 dynamic RX, 0 static TX, 32 dynamic
TX, AMPDU RX and TX on, RX block-ack window 6, RX queue 5 frames. The spike
drops `rx_queue_size` to 3 — we drain to the newest frame and discard the
rest, so a deep driver queue buys only stale frames and latency. That is a
starting point, not a measured optimum. Note `validate()` warns if
`rx_ba_win >= dynamic_rx_buf_num` or `>= 2 * static_rx_buf_num`.

Finally: **esp-radio's blobs require `opt-level` 2 or 3 to work**, per its own
documentation. The release profile is fine, but a debug build needs
`[profile.dev.package.esp-radio] opt-level = 3` or it misbehaves silently.
The spike has it.

### Coexistence: the known failure and the known fixes

This is documented by the display driver itself, which is the best possible
source. `Hub75Config::interrupt_priority` says, verbatim, that on the ESP32
"Wi-Fi and other long-running interrupt handlers run at low priority and can
delay the refresh ISR by hundreds of microseconds, causing visible flicker",
and prescribes `Priority::Priority3` plus the `iram` feature. The `iram`
feature description names the mechanism: it places the refresh ISR, the DMA
start/finish/wait path and the framebuffer pointer swap in instruction RAM "to
avoid flash-cache stalls (for example during Wi-Fi, PSRAM, or SPI-flash
activity) that can cause visible flicker", at a cost of 1-2 KiB of IRAM.

Reading esp-radio's source explains *why* that works. In
`esp-radio/src/wifi/os_adapter/esp32.rs` the WiFi MAC interrupt is registered
at `Priority::Priority1` and pinned with `intr_matrix_set(0, ...)` under the
comment "Force to bind WiFi interrupt to CPU0". The ESP32 has only three
priority levels, so a display ISR at Priority3 preempts the WiFi MAC handler
outright. The esp-rtos scheduler also sits at priority 1.

Both mitigations are enabled in the spike. Two things reduce the exposure
further:

- `circular-dma` means there is **no periodic refresh interrupt at all** — the
  DMA engine loops on its own and the ISR only fires to apply a swap. A
  delayed ISR therefore delays a frame update rather than corrupting the
  refresh, which degrades to a dropped frame instead of a visible glitch.
- The driver notes the obvious cost: a higher ISR priority increases the
  latency of everything it preempts, including WiFi bookkeeping. Use the
  lowest priority that removes flicker, do not just pin it at 3 forever.

**The C-world evidence is messier, and it is worth knowing how thin it is.**
Two claims get repeated constantly and neither is well founded:

- *"Running I2S at 10 or 20 MHz breaks WiFi."* This traces to
  `espressif/arduino-esp32#5834` (2021), which is about camera XCLK, not
  HUB75: 5, 10 and 20 MHz degrade throughput, **8 MHz does not**, and it
  affects modules but not the bare chip. Espressif closed it expired with no
  root cause. It has been reconfirmed twice since (ping 2-3 s dropping to
  6-10 ms purely from moving XCLK to 8 MHz) but never explained. Harmonics is
  the leading hypothesis and nobody has published a spectrum.
- *"WiFi MAC DMA starves I2S DMA on the shared bus."* No ESP32 erratum covers
  I2S/DMA/WiFi coexistence, and I could find no primary measurement behind
  this. The one controlled experiment in the HUB75 library's tracker produced
  **contradictory results from two competent testers on the same board**.

If the 8 MHz folklore turns out to matter, we are better placed than the C
world to act on it: the C library's plain-ESP32 divider only produces 10 or
20 MHz, whereas `esp-hub75`'s `with_frequency(Rate)` takes an arbitrary rate,
so 8 MHz is reachable from Rust. That is a cheap experiment if WiFi throughput
collapses when the panel runs.

One ESP32-specific bug worth recognising by its signature: `esp-hal` #4399,
where ping climbed 15 ms -> 210 ms -> 1963 ms -> unreachable while the station
still reported "Connected". A maintainer reproduced it easily on the original
ESP32 and on no other chip. It was fixed in esp-rtos 0.2.0, so 0.4.0 has the
fix — but it was exactly our chip, so if something like that appears, check
versions before debugging.

**Core pinning is available if we need it, but the evidence for it as a
flicker fix is negative** — the reporter of the esp-hub75 flicker issue tried
display and network on the same core and on opposite cores and called them
"both equally bad". Prefer `circular-dma`, `iram` and ISR priority. If it
comes to it, `esp-rtos` 0.4 can start its scheduler on the second core
(`esp_rtos::start_second_core(CPU_CTRL, FROM_CPU_INTR1, stack, || {})`) or on
the second core only, and exposes `esp_rtos::embassy::Executor` and
`InterruptExecutor<SWI>` so we can run an executor per core and a
high-priority interrupt executor besides. The spike is single-core on purpose:
it is one fewer variable for the first bring-up, and the escape hatch is
documented rather than spent.

### Throughput

Our load is small: 30 datagrams per second of at most 1472 bytes is about
350 kbit/s. Espressif's own figures for the ESP32 are 30 Mbit/s UDP receive
in a lab and 85 Mbit/s in a shield box, so we need roughly one percent of the
pessimistic number. **Bandwidth is not the risk.**

The Rust stack is slower than the C one — reported figures are around
300 KB/s from esp-wifi against 5 MB/s from the C iperf example on an S3, with
instruction-cache configuration, not buffer counts, as the dominant factor —
but that is still two orders of magnitude above what we need.

The interesting number is **jitter and burst loss**, and there is no published
esp-radio UDP latency or jitter measurement anywhere; the only public data is
ICMP round-trip times and TCP throughput. Card 003 owns pacing and playout;
card 013 measures it on real hardware. Treat any number quoted before then as
a guess, including the ones in this paragraph.

---

## 5. mDNS in `no_std`

**`edge-mdns` works, and there is now a responder in the skeleton**
(`spike/fw-skeleton/src/mdns.rs`) advertising `screeny.local` and
`_screeny._udp` on port 49374, with TXT records for protocol version, panel
size and the control port. The orchestrator asked for this as a compile test;
it went in cleanly, so it stayed.

The working set is `edge-mdns` 0.8.0 (features `io`, no defaults) +
`edge-nal` 0.7.0 + `edge-nal-embassy` 0.9.0 + `embassy-net` 0.9.1. Version
coupling is tight: `edge-nal-embassy` 0.9.0 is the first release that wants
`embassy-net ^0.9`; 0.8.1 wants ^0.8 and will not resolve.

Things worth knowing before card 008 owns this:

- **`edge-mdns` ships no `no_std` example.** Every example in the upstream
  repo is `std`. The pieces that needed working out: `VecBufAccess` is
  `heapless`-backed and fine as-is; `&UdpSocket` implements `UdpReceive`,
  `UdpSend` and `Readable`, so it can be handed to `Mdns::new` twice rather
  than split; and the RNG must implement `rand_core::TryRng` with
  `Error = Infallible`, after which `Rng` comes free from a blanket impl.
- **Bind the IPv4 socket explicitly.** `edge_mdns::io::DEFAULT_SOCKET` is an
  alias for the IPv6 one, and its own documentation says not to use it in
  production. Use `IPV4_DEFAULT_SOCKET`.
- **The repository moved** to `github.com/sysgrok/edge-net`.
- **It is not cheap in flash**: +124 KB of `.text`/`.rodata` (mostly the
  `domain` crate) and about 10 KB of DRAM. Both are affordable here.

Multicast: `embassy-net` 0.9 has a `multicast` feature (enabled in the spike)
and `Stack::join_multicast_group`, which is **not async in 0.9** — it was
until 0.4 and any tutorial that awaits it is stale. smoltcp's default group
limit is 4, which is plenty. `esp-radio` has no multicast MAC filter code of
its own; frames go straight to smoltcp.

**Whether multicast actually gets delivered through esp-radio on this board is
unverified**, and I could find no positive report of anyone doing mDNS on
esp-radio. The responder will fail loudly if the group join fails — it logs
and exits rather than sitting silent. Test this early; if it does not work,
discovery needs a different design, and the usual culprits are network-side
(AP client isolation, guest VLANs).

**Do not stream frames over multicast.** 802.11 multicast is unacknowledged,
never retransmitted, sent at the lowest basic rate, and buffered at the AP for
DTIM. Multicast for discovery, unicast for the 30 fps payload.

---

## 6. Persistent config storage

`esp-storage` 0.10.0 (`esp32`, `embedded-storage` features) plus
`sequential-storage` 8.0.1. Both compile in our graph; verified the same way.
`sequential-storage` speaks `embedded-storage-async` while `esp-storage`
implements the blocking traits, so bridge them with
`embassy_embedded_hal::adapter::BlockingAsync` (`embassy-embedded-hal` 0.6.0) —
`esp-storage`'s own documentation says to do exactly this.

Two things to know before card 014 uses it:

- **Stack cost.** `FlashStorage::read`/`write` always allocate a
  sector-sized (4 KiB) buffer on the stack; `read_nor`/`write_nor` only do so
  when the caller's slice is not word-aligned. Embassy task stacks are small.
  Use the `_nor` variants with aligned buffers.
- **Flash writes stall the cache.** Erasing or writing flash disables the
  instruction cache, so any code not in IRAM stops. This is precisely the
  hazard the `iram` feature exists for; the display should survive a config
  write, but WiFi may not. Write config rarely, and not while streaming.

The stock partition table already has a 20 KB `nvs` partition at 0x9000 that
we can reuse, and `esp-bootloader-esp-idf` can find partitions by name rather
than us hard-coding an offset.

---

## 7. Memory budget

Measured from the linked spike binary (`xtensa-esp32-elf-size -A`), not
estimated:

| region | bytes |
|---|---|
| `.data` + `.data.wifi` | 12,560 |
| `.bss` | 117,520 |
| `.stack` | 66,528 |
| `.dram2_uninit` (reclaimed ROM DRAM, given to the heap) | 65,536 |
| **total internal DRAM** | **262,144** |
| `.rwtext` (IRAM code) | 11,808 |
| `.rwtext.wifi` (IRAM code) | 51,416 |
| `.text` + `.rodata` + `.rodata.wifi` (flash) | ~608 KB of a 4,032 KB app slot |

Inside `.bss`, the blocks that matter:

| what | bytes |
|---|---|
| esp-alloc heap region 1 (reclaimed ROM DRAM) | 65,536 |
| esp-alloc heap region 2 | 49,152 |
| HUB75 framebuffer x2 (64x32, 6 planes, bitplane) | 2 x 12,316 |
| our RGB888 frame | 6,160 |
| mDNS socket and message buffers | ~10,000 |
| esp-radio / PHY / WPA statics | ~15,000 |

The WiFi heap gets **112 KB** across the two regions, against esp-radio's own
documented figure of **47-57 KB for a station**. Comfortable, with room to
grow: a second decoded frame for a playout buffer costs 6 KB, and going from
6 to 7 BCM planes costs 4 KB across the pair.

**One thing to watch.** The linker gives `.stack` whatever DRAM is left over,
so it is a residual, not a budget: adding the mDNS responder moved 10 KB out
of `.stack` and into `.bss` without anything warning about it. At 66 KB there
is still plenty, but a stack overflow from adding a static is a confusing way
to find that out. `esp-storage`'s 4 KiB on-stack sector buffers are the most
likely thing to collide with it.

Note also that enabling BLE would cost 64 KB of `dram_seg` outright. We have
no use for BLE, and this is a reason to keep it that way.

For reference, the stock firmware reports `Free internal RAM: 103423` after
boot — with PSRAM enabled and an entire ESP-IDF underneath. We are in the same
ballpark with a lot more headroom.

---

## 8. Task and core architecture

What the spike does, and what I would keep:

```
core 0, embassy thread-mode executor (esp_rtos::main)
  display_task   owns Hub75 + back buffer; wakes on a frame counter,
                 redraws, swap().await
  wifi_task      connect_async / wait_for_disconnect_async, forever
  net_task       embassy_net Runner
  frame_task     UdpSocket::recv_from -> decode -> shared frame, newest wins
  telemetry_task periodic counters over the serial console

esp-radio's own driver threads, scheduled by esp-rtos
HUB75 refresh: no task at all — circular DMA, ISR only on swap, Priority3, IRAM
```

The shared frame is a `Mutex<CriticalSectionRawMutex, RgbFrame>` plus an
`AtomicU32` sequence counter, and `frame_task` uses `try_lock`: if the display
task is mid-redraw when a datagram lands, the datagram is **dropped, not
queued**. Newest-wins is the whole point of the protocol, and a queue would
just add latency to a frame that is already stale. The drop is counted.

If flicker or jitter turns out to be a problem, the escape hatch is to move
the display work to the second core with `esp_rtos::start_second_core` and
leave WiFi on core 0 — but measure first.

---

## 9. Risks for first hardware bring-up

Roughly in order of likelihood, with what to look for:

1. **Panel stays dark.** Most likely the FM6126A init: either our port of
   `fm6124init()` is wrong, or the timing is (the C version relies on Arduino
   `digitalWrite` being slow; ours inserts a short spin instead). Second
   candidate is a wrong GPIO. Check the boot log's pin assertions first, then
   try commenting the init out — an ICN2037 panel does not need it.
2. **Wrong colours.** The Gen 1 RGB order varies by board revision. Swap
   R/G/B in the `Hub75Pins16` initialiser; the test pattern draws red, green
   and blue bands top to bottom specifically so this is one glance.
3. **Garbled or shifted image.** Clock phase. Add the `invert-clock` feature.
4. **Ghosting.** Raise `trail-blank-N`, then add `inter-row-blank-N`.
5. **Too bright / too much current.** The spike caps at Tidbyt's shipping
   30/255 by scaling pixel values, which is *not* the same mechanism as
   Tidbyt's OE duty and may not draw the same current. Watch the current draw
   before leaving it running. Card 020.
6. **Flicker when WiFi associates.** The two documented mitigations are
   already on. If it still flickers, the next moves are core pinning and
   lowering the pixel clock, in that order.
7. **Multicast never arrives**, which would sink the mDNS design. Test it
   before card 008 depends on it.
8. **`assert_pin` panics at boot.** That means someone edited one of the two
   pin maps and not the other; the panic message names the pin.

## 10. What I could not verify

- **Anything that needs hardware.** Nothing here has been flashed. The
  skeleton compiles; that is the entire claim.
- The reset button's GPIO, and the I2C pins for the ATECC608.
- The exact module part number (WROOM vs WROVER variant). PSRAM presence is
  proven; the part number is not stated anywhere.
- The stock firmware's actual refresh rate and pixel clock. The HDK says
  10 MHz and an 85 Hz floor; Tidbyt's library fork points at 200 Hz / 13.33 MHz.
- Measured current draw at full white, and whether the brightness cap is
  specifically a USB power budget. No source states either.
- How `esp-hub75`'s clock-phase convention maps onto the C library's
  `clkphase` flag. This is a coin flip until someone looks at the panel.
- Real UDP jitter, burst loss and end-to-end latency on this board. Cards 003
  and 013.
- Whether multicast works at all through esp-radio here.

## Sources

- `tidbyt/hdk` @ `83f884a` — `src/display.cpp`, `src/display.h`, `src/audio.c`,
  `src/touch.c`, `sdkconfig`, `platformio.ini`, `boards/default_8mb.csv`,
  and commit `839ec3e` (initial, Gen 1 only).
- `tidbyt/ESP32-HUB75-MatrixPanel-I2S-DMA` (Tidbyt's fork) —
  `ESP32-HUB75-MatrixPanel-leddrivers.cpp:24-101` (`shiftDriver`, `fm6124init`),
  `ESP32-HUB75-MatrixPanel-I2S-DMA.h`.
- `tronbyt/firmware-esp32` — `main/display.cpp`, `docs/BRIGHTNESS.md`,
  `Makefile`, and the stock Gen 1 factory image in `reset/`.
- <https://github.com/mrcodetastic/ESP32-HUB75-MatrixPanel-DMA/discussions/770>
  (actual driver silicon is ICN2037).
- <https://github.com/tronbyt/firmware-esp32/issues/123> (real Gen 1 boot log,
  8 MB PSRAM).
- <https://community.home-assistant.io/t/esphome-on-tidbyt-gen-2/830367>
  (independent Gen 1 pin map).
- `liebman/esp-hub75` 0.17.0 — `README.md`, `src/lib.rs` (`Hub75Config`,
  `Hub75Pins16`), `examples/gradient-embassy/`.
- `liebman/hub75-framebuffer` 0.12.0 — `src/lib.rs`, `src/bitplane/plain/`.
- `esp-rs/esp-hal` @ `a4e589c` — `examples/wifi/embassy_dhcp/`.
- `esp-radio` 1.0.0-beta.1 crate source — `src/wifi/mod.rs`.
- `esp-rtos` 0.4.0 crate source — `src/lib.rs`, `src/embassy/mod.rs`.
- `esp-storage` 0.10.0 crate source — `src/lib.rs`.
- crates.io API for every version and dependency requirement in section 1.
