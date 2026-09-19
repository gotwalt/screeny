//! What the LED panel actually emits.
//!
//! Lifted from `lab/src/panel.rs` (card 002); the reasoning there is the
//! reasoning here. In one paragraph: the firmware maps sRGB8 to a BCM duty
//! with `bits` bitplanes, so the panel has `2^bits` duty steps spread
//! *linearly in light* and the dark end is coarse in perceptual terms. Codec
//! error must therefore be judged after that transform - error the panel
//! cannot show is not error - which is what [`Panel::emit`] is for.
//!
//! Temporal dithering (card 030, shipped) is why a panel is not only its
//! bitplane count: at ~154 Hz refresh and 30 fps there are ~5 panel refreshes
//! per received frame, and the driver alternates between adjacent duty values
//! to land on `(2^bits - 1) * subframes` effective steps in the time average.
//! So the model is "how many duty increments are there", and
//! [`Panel::new`] (bitplanes), [`Panel::dithered`] and [`Panel::levels`] are
//! three ways of saying it. Card 016 folded `crates/demos`' level-counting
//! version and `crates/sim`'s brightness [`Lut`] into this one.

use screeny_proto::{Rgb888Frame, NBYTES};

use crate::color::{lin_to_srgb8, SRGB_TO_LIN};

/// A model of the panel's sRGB8 -> emitted-light transfer function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panel {
    /// Duty increments per channel, counting temporal dithering: one less
    /// than the number of distinct levels, because one of them is off.
    pub steps: u32,
}

impl Panel {
    /// `bits` bitplanes, no temporal dithering.
    #[must_use]
    pub const fn new(bits: u32) -> Self {
        Panel {
            steps: (1u32 << bits) - 1,
        }
    }

    /// `bits` bitplanes dithered across `subframes` refreshes.
    #[must_use]
    pub const fn dithered(bits: u32, subframes: u32) -> Self {
        Panel {
            steps: ((1u32 << bits) - 1) * subframes,
        }
    }

    /// A panel counted in *levels* rather than bitplanes: 64 levels is the
    /// same thing as 6 bitplanes.
    #[must_use]
    pub const fn levels(levels: u32) -> Self {
        Panel {
            steps: if levels > 1 { levels - 1 } else { 1 },
        }
    }

    /// Distinct duty steps, counting temporal dithering.
    #[inline]
    #[must_use]
    pub fn max(&self) -> f32 {
        self.steps as f32
    }

    /// Emitted linear light (0..1) for one sRGB8 code value, time-averaged
    /// over the panel's subframes.
    #[inline]
    #[must_use]
    pub fn emit1(&self, v: u8) -> f32 {
        let max = self.max();
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
        crate::color::oklab(self.emit(c))
    }

    /// Quantise a linear value to the nearest emittable level.
    #[inline]
    #[must_use]
    pub fn quant_lin(&self, v: f32) -> f32 {
        let max = self.max();
        (v.clamp(0.0, 1.0) * max).round() / max
    }

    /// The sRGB8 code that displays what the panel would emit for `c`. This is
    /// the transform the preview applies before drawing dots.
    #[must_use]
    pub fn round_trip(&self, c: [u8; 3]) -> [u8; 3] {
        let e = self.emit(c);
        [lin_to_srgb8(e[0]), lin_to_srgb8(e[1]), lin_to_srgb8(e[2])]
    }

    /// Snap an sRGB8 colour to one the panel can show exactly. Palette design
    /// runs every entry through this, so a designed ramp has no wasted steps
    /// and no surprise colour casts from three channels rounding differently.
    #[must_use]
    pub fn snap(&self, c: [u8; 3]) -> [u8; 3] {
        self.round_trip(c)
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
        let max = self.max();
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

/// What card 001 measured: 6 bitplanes at ~154 Hz, no temporal dither. Also
/// the art side's "64 levels".
pub const NOMINAL: Panel = Panel::new(6);
/// The same panel with the driver dithering across the ~5 refreshes it gets
/// per received frame. **Codec selection scores against this** (card 002,
/// `enc/hybrid.rs`), and since card 030 it is also what the device does.
pub const TEMPORAL: Panel = Panel::dithered(6, 5);
/// Today's dimmed behaviour: brightness is taken out of bit depth.
pub const DIMMED: Panel = Panel::new(3);
/// A dim room in the art brief's terms: half the levels (brief section 2.1).
/// Every piece is checked at 32 levels as well as at 64.
pub const DIM: Panel = Panel::levels(32);
/// Reference, and the ceiling if the driver ever gets OE-duty brightness.
pub const DEEP: Panel = Panel::new(8);

// ---------------------------------------------------------------------------
// Brightness
// ---------------------------------------------------------------------------

/// The 256-entry lookup a receiver would burn into flash: the panel model of
/// `docs/design/generative-art-brief.md` section 5, with brightness applied in
/// linear light because that is where a duty cycle lives.
///
/// One per brightness value, rebuilt whenever brightness or the panel changes
/// - which, on a real device, is only when someone sends `SET_BRIGHTNESS`.
/// The firmware does the same thing with an integer gamma/brightness LUT, and
/// does it with the same loss: scaling before quantising is what card 020
/// changed on the device, so this models the panel as `crates/sim` has always
/// shown it.
#[derive(Debug, Clone)]
pub struct Lut {
    table: [u8; 256],
    panel: Panel,
    brightness: u8,
}

impl Lut {
    /// Build the lookup for one brightness.
    #[must_use]
    pub fn new(panel: Panel, brightness: u8) -> Self {
        let scale = brightness as f32 / 255.0;
        let mut table = [0u8; 256];
        for (v, slot) in table.iter_mut().enumerate() {
            // The panel has a fixed number of duty steps per channel and
            // nothing in between; round to the nearest one.
            *slot = lin_to_srgb8(panel.quant_lin(SRGB_TO_LIN[v] * scale));
        }
        Lut {
            table,
            panel,
            brightness,
        }
    }

    /// True if this lookup is still the right one.
    #[must_use]
    pub fn matches(&self, panel: Panel, brightness: u8) -> bool {
        self.panel == panel && self.brightness == brightness
    }

    /// Map one channel.
    #[must_use]
    pub fn map(&self, v: u8) -> u8 {
        self.table[v as usize]
    }

    /// Map a whole frame.
    pub fn apply(&self, src: &Rgb888Frame, dst: &mut Rgb888Frame) {
        for i in 0..NBYTES {
            dst[i] = self.table[src[i] as usize];
        }
    }
}

/// Run one frame through the panel model. Builds the lookup each time, which
/// is fine at the once-per-displayed-frame rate this is called at.
pub fn apply(panel: Panel, brightness: u8, src: &Rgb888Frame, dst: &mut Rgb888Frame) {
    Lut::new(panel, brightness).apply(src, dst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{lin_to_srgb8, srgb_to_lin_f};

    /// The simulator's own round-trip test (card 006), which is why the two
    /// halves of the EOTF have to agree to the last bit.
    #[test]
    fn srgb_round_trips_through_linear() {
        for v in 0..=255u8 {
            assert_eq!(lin_to_srgb8(srgb_to_lin_f(v as f32 / 255.0)), v, "channel {v}");
        }
    }

    #[test]
    fn full_brightness_black_and_white_are_exact() {
        let lut = Lut::new(Panel::levels(64), 255);
        assert_eq!(lut.map(0), 0);
        assert_eq!(lut.map(255), 255);
    }

    #[test]
    fn quantisation_is_visible_and_monotonic() {
        let lut = Lut::new(Panel::levels(64), 255);
        let distinct: std::collections::BTreeSet<u8> = (0..=255u8).map(|v| lut.map(v)).collect();
        assert_eq!(distinct.len(), 64, "one output per panel duty step");
        let mut prev = 0u8;
        for v in 0..=255u8 {
            let m = lut.map(v);
            assert!(m >= prev, "LUT must be monotonic at {v}");
            prev = m;
        }
    }

    #[test]
    fn thirty_two_levels_is_coarser_than_sixty_four() {
        let a = Lut::new(Panel::levels(32), 255);
        let b = Lut::new(Panel::levels(64), 255);
        let n = |l: &Lut| {
            (0..=255u8)
                .map(|v| l.map(v))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        };
        assert_eq!(n(&a), 32);
        assert_eq!(n(&b), 64);
    }

    #[test]
    fn brightness_darkens_everything_and_zero_is_black() {
        let full = Lut::new(NOMINAL, 255);
        let half = Lut::new(NOMINAL, 128);
        let off = Lut::new(NOMINAL, 0);
        for v in 0..=255u8 {
            assert!(half.map(v) <= full.map(v), "half is never brighter at {v}");
            assert_eq!(off.map(v), 0, "brightness 0 is black at {v}");
        }
        assert!(half.map(255) < 255);
    }

    /// Six bitplanes, 64 levels and the art brief's `Panel { levels: 64 }` are
    /// three names for one transfer function. Card 016 merged them; this is
    /// what stops them drifting apart again.
    #[test]
    fn bitplanes_and_levels_are_the_same_panel() {
        assert_eq!(Panel::new(6), Panel::levels(64));
        assert_eq!(Panel::new(5), Panel::levels(32));
        assert_eq!(Panel::dithered(6, 1), Panel::new(6));
        for v in 0..=255u8 {
            assert_eq!(NOMINAL.emit1(v), Panel::levels(64).emit1(v));
        }
    }
}
