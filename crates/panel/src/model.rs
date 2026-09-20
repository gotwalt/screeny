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
/// What dimming used to cost, before card 020: brightness scaled the pixel
/// values, so a 30/255 cap left a 6-bit channel using values 0-7. **The device
/// has not behaved like this since card 020** - it dims the output-enable
/// window instead (see [`oe_slots`]) and keeps all six bits at every
/// brightness. Kept as the name of the loss, for `lab`'s comparison tables and
/// for anyone reading the old research.
pub const DIMMED: Panel = Panel::new(3);
/// A dim room in the art brief's terms: half the levels (brief section 2.1).
/// Every piece is checked at 32 levels as well as at 64.
pub const DIM: Panel = Panel::levels(32);
/// Reference: 8 bitplanes. More depth than this panel has at any brightness.
pub const DEEP: Panel = Panel::new(8);

// ---------------------------------------------------------------------------
// Output-enable dimming: how the device gets darker (card 020)
// ---------------------------------------------------------------------------

/// Hard ceiling on output-enable duty, in pixel-clock slots per 64-slot scan
/// row, mirroring `firmware/src/display.rs`'s `MAX_OE_SLOTS`. 25/64 is 39%
/// duty, Tidbyt's own documented maximum, and it is the USB power budget
/// rather than a tuning knob.
pub const MAX_OE_SLOTS: u32 = 25;

/// Runtime brightness 0..=255 -> lit output-enable slots, exactly
/// `firmware::display::slots_for`.
///
/// The real resolution of the brightness control is [`MAX_OE_SLOTS`] steps,
/// not 256: the panel is lit for a whole number of pixel clocks or not at all,
/// so e.g. 129 and 130 are the same picture. Modelling that is the difference
/// between a preview that predicts the device and one that is merely close.
#[must_use]
pub const fn oe_slots(brightness: u8) -> u32 {
    (brightness as u32 * MAX_OE_SLOTS + 127) / 255
}

/// The fraction of full light the panel emits at `brightness`.
///
/// Linear in duty and therefore linear in light, and applied *after*
/// quantisation: dimming costs light, not bit depth.
#[must_use]
pub fn oe_light(brightness: u8) -> f32 {
    oe_slots(brightness) as f32 / MAX_OE_SLOTS as f32
}

// ---------------------------------------------------------------------------
// Brightness
// ---------------------------------------------------------------------------

/// The 256-entry lookup that turns a sent sRGB8 code into the sRGB8 code a
/// preview should draw: the panel model of
/// `docs/design/generative-art-brief.md` section 5, at one brightness.
///
/// One per brightness value, rebuilt whenever brightness or the panel changes,
/// which on a real device is only when someone sends `SET_BRIGHTNESS`.
///
/// **Order matters, and this is the whole of card 066.** The device quantises
/// at full depth - `firmware/src/gamma.rs`'s table is the sRGB EOTF and
/// nothing else, with brightness deliberately kept out of it - and then dims
/// by shortening the output-enable window ([`oe_slots`], card 020). So
/// brightness costs light and not bit depth: [`Lut::new`] quantises first and
/// scales the *emitted* light afterwards, and [`Lut::distinct_levels`] does
/// not fall as the panel gets dimmer. [`Lut::value_scaled`] is the other
/// order, which is what the device did before card 020.
#[derive(Debug, Clone)]
pub struct Lut {
    table: [u8; 256],
    panel: Panel,
    brightness: u8,
}

impl Lut {
    /// Build the lookup for one brightness, the way the device behaves:
    /// quantise to a duty step, then emit that step for a shorter window.
    #[must_use]
    pub fn new(panel: Panel, brightness: u8) -> Self {
        let light = oe_light(brightness);
        Self::build(panel, brightness, |v| {
            // The panel has a fixed number of duty steps per channel and
            // nothing in between; round to the nearest one. The output-enable
            // window then scales every step by the same factor, which moves no
            // code onto another code's step.
            panel.quant_lin(SRGB_TO_LIN[v]) * light
        })
    }

    /// The pre-card-020 model: scale the value, *then* quantise, so dimming
    /// spends bit depth. The device did this while it dimmed by scaling pixel
    /// values into the framebuffer, and a receiver built on a driver without
    /// output-enable control would still have to.
    ///
    /// Nothing renders through it today - the simulator uses [`Lut::new`] -
    /// but it is what [`DIMMED`] describes, and keeping it is what lets a test
    /// show how much banding the old model invented.
    #[must_use]
    pub fn value_scaled(panel: Panel, brightness: u8) -> Self {
        let scale = brightness as f32 / 255.0;
        Self::build(panel, brightness, |v| panel.quant_lin(SRGB_TO_LIN[v] * scale))
    }

    fn build(panel: Panel, brightness: u8, emit: impl Fn(usize) -> f32) -> Self {
        let mut table = [0u8; 256];
        for (v, slot) in table.iter_mut().enumerate() {
            *slot = lin_to_srgb8(emit(v));
        }
        Lut {
            table,
            panel,
            brightness,
        }
    }

    /// `(distinct codes this lookup can output, how many of the 256 inputs
    /// land on black)` - [`Panel::distinct_levels`] with brightness applied.
    ///
    /// Counted in the 8-bit codes a preview actually draws, so at a low enough
    /// brightness two duty steps can round onto one screen colour. That is the
    /// preview window running out of resolution, not the panel banding.
    #[must_use]
    pub fn distinct_levels(&self) -> (usize, usize) {
        let seen: std::collections::BTreeSet<u8> = self.table.iter().copied().collect();
        let crushed = self.table.iter().filter(|&&v| v == 0).count();
        (seen.len(), crushed)
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

    /// Card 066. The device dims by shortening the output-enable window, so
    /// every code keeps its own duty step and only the light goes away. The
    /// simulator used to scale before quantising and so predicted banding the
    /// panel does not have - at the bench cap of 160, exactly where it shows.
    #[test]
    fn dimming_costs_light_not_bit_depth() {
        let full = Lut::new(NOMINAL, 255);
        let (levels, _) = full.distinct_levels();
        assert_eq!(levels, 64, "64 duty steps at full brightness");
        for b in [160u8, 128] {
            let dim = Lut::new(NOMINAL, b);
            assert_eq!(
                dim.distinct_levels(),
                full.distinct_levels(),
                "brightness {b} keeps every duty step"
            );
            assert!(dim.map(255) < full.map(255), "brightness {b} is dimmer");
        }
    }

    /// And the model it replaced did not: this is the difference card 066 is
    /// about, kept as a number rather than a claim.
    #[test]
    fn the_pre_card_020_model_spent_depth_on_brightness() {
        let (new, _) = Lut::new(NOMINAL, 128).distinct_levels();
        let (old, _) = Lut::value_scaled(NOMINAL, 128).distinct_levels();
        assert_eq!(new, 64);
        assert_eq!(old, 33, "scaling before quantising halves the steps");
    }

    /// `oe_slots` is `firmware::display::slots_for` and has to stay that way;
    /// the comments quoted here are the firmware's own.
    #[test]
    fn oe_slots_is_the_firmwares_slots_for() {
        assert_eq!(oe_slots(0), 0);
        assert_eq!(oe_slots(255), MAX_OE_SLOTS);
        assert_eq!(oe_slots(96), 9, "the firmware's DEFAULT_BRIGHTNESS is 9 slots");
        assert_eq!(oe_light(255), 1.0);
        // The control has 25 steps, not 256: neighbouring brightnesses are
        // often the same picture, on the device and now in the preview.
        assert_eq!(oe_slots(129), oe_slots(130));
        let a = Lut::new(NOMINAL, 129);
        let b = Lut::new(NOMINAL, 130);
        for v in 0..=255u8 {
            assert_eq!(a.map(v), b.map(v), "same OE window, same picture at {v}");
        }
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
