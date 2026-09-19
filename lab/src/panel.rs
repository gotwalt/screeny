//! What the LED panel actually emits.
//!
//! The firmware will hold one table mapping sRGB8 -> BCM duty cycle. An LED
//! driven by binary-coded modulation emits light in proportion to duty, so
//! that table *is* the gamma correction: `duty = round(eotf(v/255) * (2^n-1))`.
//!
//! Two consequences drive everything in this lab:
//!
//! 1. With n bitplanes the panel has 2^n duty steps spread **linearly in
//!    light**, so the dark end is coarse in perceptual terms. At n=8, sRGB
//!    codes 0..11 all collapse onto duty 0 or 1 -- roughly the bottom 5% of
//!    the code range has 2 distinct outputs. Precision a codec spends down
//!    there is wasted.
//! 2. Conversely, error *up* the curve is compressed: codec error should be
//!    judged after this transform, not on raw sRGB codes. Every perceptual
//!    metric in `metrics.rs` therefore runs source and decode through
//!    `Panel::emit` first.

use crate::color::{oklab, SRGB_TO_LIN};

#[derive(Clone, Copy, Debug)]
pub struct Panel {
    /// Number of BCM bitplanes per channel actually clocked out.
    pub bits: u32,
}

impl Panel {
    pub const fn new(bits: u32) -> Self {
        Self { bits }
    }

    /// Emitted linear light (0..1) for one sRGB8 code value.
    pub fn emit1(&self, v: u8) -> f32 {
        let max = ((1u32 << self.bits) - 1) as f32;
        let lin = SRGB_TO_LIN[v as usize];
        (lin * max).round() / max
    }

    pub fn emit(&self, c: [u8; 3]) -> [f32; 3] {
        [self.emit1(c[0]), self.emit1(c[1]), self.emit1(c[2])]
    }

    pub fn emit_oklab(&self, c: [u8; 3]) -> [f32; 3] {
        oklab(self.emit(c))
    }

    /// How many distinct output levels the panel can show, and how many sRGB
    /// codes collapse onto the lowest two of them. Reported in the writeup.
    pub fn distinct_levels(&self) -> (usize, usize) {
        let mut seen = std::collections::BTreeSet::new();
        let mut crushed = 0usize;
        for v in 0..=255u8 {
            let d = (SRGB_TO_LIN[v as usize] * (((1u32 << self.bits) - 1) as f32)).round() as u32;
            if d <= 1 {
                crushed += 1;
            }
            seen.insert(d);
        }
        (seen.len(), crushed)
    }
}

/// The panel we assume for scoring. 8 BCM bitplanes at 64x32 1/16-scan is
/// comfortably reachable on an ESP32 (see the research notes); we also report
/// a pessimistic 5-bit panel to show how much the answer depends on it.
pub const NOMINAL: Panel = Panel::new(8);
pub const PESSIMISTIC: Panel = Panel::new(5);
