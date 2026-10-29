//! LED Art Panel board facts (`--features board-art-panel`).
//!
//! The LED Art Panel is a workshop PCB: a classic ESP32 devkit (ESP32-WROOM,
//! 4 MB flash, no PSRAM, CP2102) on a carrier that drives one 32x32 HUB75
//! panel (Adafruit 607: 4 mm pitch, 1/16 scan) and takes its own USB-C supply.
//! Every pin here is read out of the board's Arduino sketch,
//! `DCElfving/LED_Art_Panel`, `LED_Art_Panel.ino:33-52`, which drives it with
//! `ESP32-HUB75-MatrixPanel-I2S-DMA` at that library's defaults.
//!
//! The same names as [`crate::tidbyt`], so `crate::board` can be either.
#![allow(dead_code)]

/// Panel geometry. 1/16 scan like the Tidbyt's, so the row address is four
/// bits and the driver is the same; only the width differs.
pub const PANEL_COLS: usize = 32;
pub const PANEL_ROWS: usize = 32;

/// HUB75 pin map. The sketch's comment says B1 and B2 sit on non-standard
/// header pins on this PCB (4 and 8); that is the PCB's business - these are
/// the GPIOs.
pub mod pins {
    pub const R1: u8 = 18;
    pub const G1: u8 = 21;
    pub const B1: u8 = 4;
    pub const R2: u8 = 32;
    pub const G2: u8 = 19;
    pub const B2: u8 = 33;

    pub const A: u8 = 25;
    pub const B: u8 = 5;
    pub const C: u8 = 26;
    pub const D: u8 = 17;

    /// No E line on a 1/16-scan panel (the sketch passes `-1`). `esp-hub75`
    /// insists on five address pins, so it gets GPIO22, which the board does
    /// not connect. Row addresses never exceed 15, so it is never driven high.
    pub const E_UNUSED: u8 = 22;

    pub const LAT: u8 = 16;
    pub const OE: u8 = 14;
    pub const CLK: u8 = 27;
}

/// The devkit's BOOT button: GPIO0, active low, pulled up on the board. Held
/// while the chip comes out of reset it selects the ROM downloader instead of
/// this firmware, so the gestures (all made while running) are unaffected.
/// EN is the reset line and needs no firmware.
pub const BUTTON_GPIO: Option<u8> = Some(0);

/// The Tidbyt's pixel clock. The sketch runs the Arduino library's default
/// (also 10 MHz on a classic ESP32), so there is nothing to suggest otherwise.
pub const PIXEL_CLOCK_MHZ: u32 = 10;

/// Where `ota_1` starts in `firmware/partitions-4mb.csv`.
pub const OTA1_OFFSET: u32 = 0x1F0000;
