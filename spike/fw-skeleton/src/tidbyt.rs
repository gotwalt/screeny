//! Tidbyt Gen 1 board facts.
//!
//! XXX-PLACEHOLDER: pin numbers below are not yet confirmed against the Tidbyt
//! sources. Do not flash until this marker is gone.
//!
//! Everything here comes from Tidbyt's own open-source firmware and HDK; see
//! `docs/research/001-firmware-stack.md` for the citation behind each number.
//! Nothing in this file has been checked against real hardware by this crate's
//! author — see the "unverified" section of that document.

/// Panel geometry.
pub const PANEL_COLS: usize = 64;
pub const PANEL_ROWS: usize = 32;

/// HUB75 pin map, Gen 1.
///
/// From `tidbyt/firmware-esp32`, `src/main.cpp` (the `HUB75_I2S_CFG::i2s_pins`
/// initialiser). Gen 2 is a different board; these numbers are Gen 1 only.
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
    /// The Gen 1 panel is 1/16 scan, so there is no E address line. `esp-hub75`
    /// requires five address pins regardless, so we hand it a GPIO that is not
    /// connected to anything on this board. Row addresses only ever reach 15,
    /// so this pin is never driven high.
    pub const E_UNUSED: u8 = 32;

    pub const LAT: u8 = 19;
    pub const OE: u8 = 32;
    pub const CLK: u8 = 14;

    /// The single front button.
    pub const BUTTON: u8 = 0;
}

/// Brightness cap. The panel is powered from laptop USB on this bench, so the
/// firmware must never run the panel at full scale.
pub const BRIGHTNESS_CAP: u8 = 40;
