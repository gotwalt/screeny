//! Tidbyt Gen 1 board facts.
//!
//! Every number here is read out of Tidbyt's own firmware, `tidbyt/hdk`
//! (there is no `tidbyt/firmware-esp32`, and no schematic has ever been
//! published). `docs/research/001-firmware-stack.md` carries the citations
//! and says which of these are cross-checked and which are inferred.
//!
//! None of it has been confirmed against real hardware — this crate has never
//! been flashed.
//!
//! Some of these constants exist to be read rather than called: they record
//! which GPIOs are already spoken for, so that a later card does not
//! rediscover it the hard way.
#![allow(dead_code)]

/// Panel geometry. 1/16 scan, so the panel shifts two rows at a time and the
/// row address only needs four bits.
pub const PANEL_COLS: usize = 64;
pub const PANEL_ROWS: usize = 32;

/// HUB75 pin map, Gen 1.
///
/// `hdk/src/display.cpp:23-40`, the `#else` (non-`TIDBYT_GEN2`) branch.
/// Gen 2 is a different board with a different map; these are Gen 1 only.
///
/// Caveat that matters for bring-up: the stock firmware picks its RGB pin
/// assignment at run time from a board revision it reads off two ADC straps
/// (its log format string is `Display for rev %d, RGB=%d,%d,%d`), and the
/// tronbyt firmware ships a `tidbyt-gen1_swap` target that rotates
/// R -> B -> G -> R. So a given Gen 1 unit may have the colour channels
/// permuted relative to what is below. If the test pattern comes up with the
/// wrong colours, that is the first thing to try.
pub mod pins {
    pub const R1: u8 = 21;
    pub const G1: u8 = 2;
    pub const B1: u8 = 22;
    pub const R2: u8 = 23;
    pub const G2: u8 = 4;
    pub const B2: u8 = 27;

    pub const A: u8 = 26;
    pub const B: u8 = 5;
    pub const C: u8 = 25;
    pub const D: u8 = 18;

    /// There is no E line on Gen 1: `hdk/src/display.cpp:35` defines
    /// `CH_E` as `-1`, with the comment "assign to pin 14 if using more than
    /// two panels". `esp-hub75` insists on five address pins whatever the
    /// panel does, so we hand it GPIO14 — free on this board, and the pin
    /// Tidbyt themselves earmarked for the job. Row addresses never exceed
    /// 15, so it is never driven high.
    pub const E_UNUSED: u8 = 14;

    pub const LAT: u8 = 19;
    pub const OE: u8 = 32;
    pub const CLK: u8 = 33;

    /// GPIO16 and GPIO17 are the PSRAM chip select and clock
    /// (`hdk/sdkconfig:1188-1189`). Do not use them for anything.
    pub const PSRAM_CS: u8 = 16;
    pub const PSRAM_CLK: u8 = 17;

    /// GPIO13 and GPIO15 are analogue board-identification straps: the stock
    /// firmware ADC-reads both to decide generation and revision (ADC unit 2,
    /// channels 4 and 3, 12-bit, 12 dB atten, four samples averaged).
    ///
    /// GPIO15 is *also* the reset button — see [`super::BUTTON_GPIO`]. Anything
    /// that reads the strap has to do it once, before the button is configured.
    pub const BOARD_ID_ADC_A: u8 = 13;
    pub const BOARD_ID_ADC_B: u8 = 15;
}

/// The reset button. Card 202 read the pin out of Tidbyt's own flash image:
/// the stock firmware's `gpio_config_t` is `pin_bit_mask = 1 << 15`, input,
/// internal pull-up, any-edge interrupt, and its press test is
/// `gpio_get_level(15) == 0` — so **GPIO15, active low**. A stock boot log on
/// Tidbyt's forum says the same thing independently. `docs/research/008-button.md`
/// has the evidence, the gesture design and the build cards.
///
/// Still `None`, deliberately: nothing here has been confirmed on this unit yet.
/// `firmware/src/bin/gpio_probe.rs` settles it in one minute with the owner
/// pressing the button; the card that runs it sets this to `Some(15)`. Do not
/// set it from the reading alone.
///
/// Note that GPIO15 is also [`pins::BOARD_ID_ADC_B`]: the newer stock build
/// ADC-reads it (ADC2 channel 3) for the hardware revision *and* uses it as the
/// button. Both facts are true; see the research doc before using either.
pub const BUTTON_GPIO: Option<u8> = None;

/// Stock firmware brightness, as an 8-bit value handed to the HUB75 library's
/// `setBrightness8()`, which turns it into output-enable duty across the row.
///
/// `hdk/src/display.h:6-8`: min 1, default 30, max 100 — i.e. Tidbyt's own
/// "full brightness" is 100/255, about 39% duty, and the shipping default is
/// 30/255, about 12%.
///
/// Card 001 concluded that `esp-hub75` could not reproduce this and that we
/// would have to scale pixel values, at a cost of about three of our six
/// bits. **That turned out to be wrong**: output-enable is a bit in every
/// framebuffer entry and the lit window can be narrowed at run time. Card 007
/// does exactly what these numbers describe, in
/// `DmaFrameBuffer::set_oe_slots` — see `vendor/README.md` and
/// `display::MAX_OE_SLOTS`. Colour depth is untouched.
pub const STOCK_BRIGHTNESS_DEFAULT: u8 = 30;
pub const STOCK_BRIGHTNESS_MAX: u8 = 100;

/// Pixel clock. Tidbyt runs the panel at 10 MHz
/// (`hdk/src/display.cpp:61`, `HUB75_I2S_CFG::HZ_10M`). That is the only
/// clock this panel is known to work at, so it is where bring-up starts;
/// the ESP32's I2S tops out near 19 MHz and the refresh-rate table in the
/// research doc says what faster clocks buy.
pub const PIXEL_CLOCK_MHZ: u32 = 10;
