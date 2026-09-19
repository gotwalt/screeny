//! What the LED panel actually emits.
//!
//! The firmware holds one table mapping sRGB8 -> BCM duty. An LED driven by
//! binary-coded modulation emits light in proportion to duty, so that table
//! *is* the gamma correction: `duty = round(eotf(v/255) * (2^n-1))`.
//!
//! **n is small.** Card 001 measured the Rust `esp-hub75` driver on this panel
//! at the Tidbyt's 10 MHz pixel clock: 6 bits/channel gives ~154 Hz refresh,
//! 7 bits gives ~76 Hz (visibly flickering), 8 bits is unusable. And because
//! that driver has no OE-duty brightness control, dimming costs bit depth:
//! at a Tidbyt-like 30/255 brightness only about 3 of the 6 bits survive.
//!
//! Two consequences drive everything in this lab:
//!
//! 1. With n bitplanes the panel has 2^n duty steps spread **linearly in
//!    light**, so the dark end is coarse in perceptual terms. At n=6 the
//!    bottom 8.6% of the sRGB code range has one output level. Precision a
//!    codec spends down there is wasted.
//! 2. Codec error must be judged *after* this transform. Error the panel
//!    cannot show is not error. Scoring against an 8-bit sRGB display would
//!    over-reward codecs that spend bytes on precision, so every perceptual
//!    metric in `metrics.rs` runs source and decode through `Panel::emit`
//!    first.
//!
//! `subframes` models **device-side temporal dithering**: at 154 Hz refresh
//! and a 30 fps network rate there are ~5 panel refreshes per received frame,
//! so the driver can alternate between adjacent duty values and land on
//! `2^n * subframes` effective levels in the time average.

use crate::color::{oklab, SRGB_TO_LIN};

#[derive(Clone, Copy, Debug)]
pub struct Panel {
    /// BCM bitplanes per channel actually clocked out.
    pub bits: u32,
    /// Panel refreshes per received frame that the driver may dither across.
    pub subframes: u32,
}

impl Panel {
    pub const fn new(bits: u32) -> Self {
        Self {
            bits,
            subframes: 1,
        }
    }
    pub const fn dithered(bits: u32, subframes: u32) -> Self {
        Self { bits, subframes }
    }

    fn steps(&self) -> f32 {
        (((1u32 << self.bits) - 1) * self.subframes) as f32
    }

    /// Emitted linear light (0..1) for one sRGB8 code value, time-averaged
    /// over `subframes` panel refreshes.
    pub fn emit1(&self, v: u8) -> f32 {
        let max = self.steps();
        (SRGB_TO_LIN[v as usize] * max).round() / max
    }

    pub fn emit(&self, c: [u8; 3]) -> [f32; 3] {
        [self.emit1(c[0]), self.emit1(c[1]), self.emit1(c[2])]
    }

    pub fn emit_oklab(&self, c: [u8; 3]) -> [f32; 3] {
        oklab(self.emit(c))
    }

    /// (distinct output levels reachable from the 256 sRGB codes,
    ///  how many sRGB codes land on the single lowest level)
    pub fn distinct_levels(&self) -> (usize, usize) {
        let max = self.steps();
        let mut seen = std::collections::BTreeSet::new();
        let mut crushed = 0usize;
        for v in 0..=255u8 {
            let d = (SRGB_TO_LIN[v as usize] * max).round() as u32;
            if d == 0 {
                crushed += 1;
            }
            seen.insert(d);
        }
        (seen.len(), crushed)
    }
}

/// What card 001 measured: 6 bitplanes at ~154 Hz, no temporal dither.
/// This is the panel every headline number in the report is scored against.
pub const NOMINAL: Panel = Panel::new(6);
/// Same panel with the driver dithering across the ~5 refreshes it gets per
/// received frame. Shows how much codec precision stops being wasted.
pub const TEMPORAL: Panel = Panel::dithered(6, 5);
/// Today's dimmed behaviour: brightness is taken out of bit depth.
pub const DIMMED: Panel = Panel::new(3);
/// Reference, and the ceiling if the driver ever gets OE-duty brightness and
/// a faster clock.
pub const DEEP: Panel = Panel::new(8);
