//! What the LED panel actually emits.
//!
//! Lifted from `lab/src/panel.rs` (card 002); the reasoning there is the
//! reasoning here. In one paragraph: the firmware maps sRGB8 to a BCM duty
//! with `bits` bitplanes, so the panel has `2^bits` duty steps spread
//! *linearly in light* and the dark end is coarse in perceptual terms. Codec
//! error must therefore be judged after that transform - error the panel
//! cannot show is not error - which is what [`Panel::emit`] is for.
//!
//! `subframes` models device-side temporal dithering (card 030): at ~154 Hz
//! refresh and 30 fps there are ~5 panel refreshes per received frame, so the
//! driver can alternate between adjacent duty values and land on
//! `2^bits * subframes` effective levels in the time average.

use crate::color::{oklab, SRGB_TO_LIN};

/// A model of the panel's sRGB8 -> emitted-light transfer function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panel {
    /// BCM bitplanes per channel actually clocked out.
    pub bits: u32,
    /// Panel refreshes per received frame that the driver may dither across.
    pub subframes: u32,
}

impl Panel {
    /// `bits` bitplanes, no temporal dithering.
    #[must_use]
    pub const fn new(bits: u32) -> Self {
        Self {
            bits,
            subframes: 1,
        }
    }

    /// `bits` bitplanes dithered across `subframes` refreshes.
    #[must_use]
    pub const fn dithered(bits: u32, subframes: u32) -> Self {
        Self { bits, subframes }
    }

    /// Distinct duty steps, counting temporal dithering.
    #[inline]
    #[must_use]
    pub fn steps(&self) -> f32 {
        (((1u32 << self.bits) - 1) * self.subframes) as f32
    }

    /// Emitted linear light (0..1) for one sRGB8 code value, time-averaged
    /// over `subframes` panel refreshes.
    #[inline]
    #[must_use]
    pub fn emit1(&self, v: u8) -> f32 {
        let max = self.steps();
        (SRGB_TO_LIN[v as usize] * max).round() / max
    }

    /// [`Panel::emit1`] on all three channels.
    #[inline]
    #[must_use]
    pub fn emit(&self, c: [u8; 3]) -> [f32; 3] {
        [self.emit1(c[0]), self.emit1(c[1]), self.emit1(c[2])]
    }

    /// Emitted light of an sRGB8 pixel, in Oklab. The chooser's unit of error.
    #[inline]
    #[must_use]
    pub fn emit_oklab(&self, c: [u8; 3]) -> [f32; 3] {
        oklab(self.emit(c))
    }

    /// A 256-entry table of [`Panel::emit1`], so the hot paths do not repeat
    /// the round-trip through the EOTF.
    #[must_use]
    pub fn emit_lut(&self) -> [f32; 256] {
        let mut t = [0f32; 256];
        for (v, slot) in t.iter_mut().enumerate() {
            *slot = self.emit1(v as u8);
        }
        t
    }

    /// `(distinct output levels reachable from the 256 sRGB codes, how many
    /// sRGB codes land on the single lowest level)`.
    #[must_use]
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
pub const NOMINAL: Panel = Panel::new(6);
/// The same panel with the driver dithering across the ~5 refreshes it gets
/// per received frame. **Codec selection scores against this**, not against
/// the panel we have today, so that the picture cannot get worse when the
/// firmware gets better (card 002, `enc/hybrid.rs`).
pub const TEMPORAL: Panel = Panel::dithered(6, 5);
/// Today's dimmed behaviour: brightness is taken out of bit depth.
pub const DIMMED: Panel = Panel::new(3);
/// Reference, and the ceiling if the driver ever gets OE-duty brightness.
pub const DEEP: Panel = Panel::new(8);
