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

    /// The sRGB8 code that displays what the panel emits for the code `v`:
    /// send `v`, see this. On [`DEVICE`] it is the identity for every code
    /// above 38, and below that a collapse of up to four codes onto one level
    /// - which is the whole of the dark end in one function.
    #[inline]
    #[must_use]
    pub fn code(&self, v: u8) -> u8 {
        lin_to_srgb8(self.emit1(v))
    }

    /// The sRGB8 code that displays what the panel would emit for `c`. This is
    /// the transform the preview applies before drawing dots.
    #[must_use]
    pub fn round_trip(&self, c: [u8; 3]) -> [u8; 3] {
        [self.code(c[0]), self.code(c[1]), self.code(c[2])]
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
///
/// This is the *within one frame* view: a colour that is only on screen for
/// one 30 fps frame gets about five of the firmware's sixteen dither phases,
/// so it resolves about 195 of the 256 sRGB codes. A colour that is **held**
/// gets the whole cycle and resolves 237 - that is [`DEVICE`]. Scoring a codec
/// is a per-frame question, so the encoder keeps this one.
pub const TEMPORAL: Panel = Panel::dithered(6, 5);

/// How many refreshes the firmware spends a sub-level remainder over:
/// `gamma::FRAC` in `firmware/src/gamma.rs`, which keeps four fractional bits
/// below one duty level and bumps the level when the remainder beats the
/// phase threshold.
pub const DITHER_PHASES: u32 = 16;

/// **What the device shows a colour it is holding.** 64 duty levels spread
/// over [`DITHER_PHASES`] refreshes: 1008 duty steps per channel, in the time
/// average, and the panel model everything on the art side quantises and
/// previews against.
///
/// The full phase cycle is 16 refreshes, about 104 ms at the measured 154 Hz,
/// so this is the picture from three 30 fps frames onwards. Numbers, all
/// checked in this module's tests against the firmware's own table:
///
/// | | [`NOMINAL`] | [`TEMPORAL`] | `DEVICE` |
/// |---|---|---|---|
/// | duty steps | 63 | 315 | 1008 |
/// | distinct levels out of 256 sRGB codes | 64 | 195 | 237 |
/// | codes that come out black | 22 | 6 | 2 |
/// | darkest lit code | 22 | 6 | 2 |
///
/// The brief's "the darkest visible level is about sRGB 6, it was 34 without
/// dithering" is the same fact seen by eye: sRGB 2 is one duty step in sixteen
/// refreshes and is not something you could call visible.
pub const DEVICE: Panel = Panel::dithered(6, DITHER_PHASES);
/// What dimming used to cost, before card 020: brightness scaled the pixel
/// values, so a 30/255 cap left a 6-bit channel using values 0-7. **The device
/// has not behaved like this since card 020** - it dims the output-enable
/// window instead (see [`oe_slots`]) and keeps all six bits at every
/// brightness. Kept as the name of the loss, for `lab`'s comparison tables and
/// for anyone reading the old research.
pub const DIMMED: Panel = Panel::new(3);
/// Half the levels, which the first version of the art brief called "a dim
/// room". **It is not one.** Dimming costs light and not depth (card 020, card
/// 066): a dim room is [`DEVICE`] times [`oe_light`], with every duty step
/// still there. Kept, like [`DIMMED`], as the name of a loss - a receiver that
/// had to scale pixel values would land here - and used by `lab`'s comparison
/// tables, not by anything that predicts this device.
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

    // -----------------------------------------------------------------
    // Card 102: the dark end, pinned to the firmware
    // -----------------------------------------------------------------

    /// `firmware/src/gamma.rs`'s `SRGB_TO_Q`, copied here as a **fixture**.
    ///
    /// Not a second implementation - nothing reads it but the test below.
    /// The device's whole colour behaviour is this table plus
    /// `display::quantise_dither`, and the one claim [`DEVICE`] makes is that
    /// it reproduces them. A copy of the numbers is the only way to check that
    /// from a host crate: `firmware/` is a separate cargo project on the `esp`
    /// toolchain and cannot be depended on.
    #[rustfmt::skip]
    const FIRMWARE_SRGB_TO_Q: [u16; 256] = [
        0, 0, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5,
        5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 12, 12, 13, 14,
        15, 15, 16, 17, 18, 19, 20, 20, 21, 22, 23, 24, 25, 26, 28, 29,
        30, 31, 32, 33, 35, 36, 37, 39, 40, 41, 43, 44, 46, 47, 49, 50,
        52, 53, 55, 57, 58, 60, 62, 64, 65, 67, 69, 71, 73, 75, 77, 79,
        81, 83, 85, 87, 89, 92, 94, 96, 98, 101, 103, 105, 108, 110, 113, 115,
        118, 120, 123, 126, 128, 131, 134, 137, 140, 142, 145, 148, 151, 154, 157, 160,
        163, 166, 170, 173, 176, 179, 183, 186, 189, 193, 196, 200, 203, 207, 210, 214,
        218, 221, 225, 229, 233, 236, 240, 244, 248, 252, 256, 260, 264, 268, 273, 277,
        281, 285, 290, 294, 299, 303, 307, 312, 317, 321, 326, 330, 335, 340, 345, 349,
        354, 359, 364, 369, 374, 379, 384, 390, 395, 400, 405, 410, 416, 421, 427, 432,
        438, 443, 449, 454, 460, 466, 472, 477, 483, 489, 495, 501, 507, 513, 519, 525,
        531, 538, 544, 550, 556, 563, 569, 576, 582, 589, 595, 602, 609, 615, 622, 629,
        636, 643, 650, 657, 664, 671, 678, 685, 692, 699, 707, 714, 721, 729, 736, 744,
        751, 759, 767, 774, 782, 790, 798, 805, 813, 821, 829, 837, 846, 854, 862, 870,
        878, 887, 895, 903, 912, 920, 929, 938, 946, 955, 964, 972, 981, 990, 999, 1008,
    ];

    /// **[`DEVICE`] is the firmware, to the last duty step.**
    ///
    /// The firmware keeps `FRAC_BITS = 4` fractional bits below a duty level
    /// (`SRGB_TO_Q[v] = round(63 * 16 * lin(v))`) and `quantise_dither` spends
    /// the remainder across the 16 phases, so over a full cycle a held colour
    /// averages exactly `SRGB_TO_Q[v] / 16` levels out of 63. That is
    /// `Panel::dithered(6, 16)`, and this is the proof rather than the claim.
    #[test]
    fn device_is_the_firmwares_gamma_table() {
        for v in 0..=255usize {
            let want = f64::from(FIRMWARE_SRGB_TO_Q[v]) / 1008.0;
            let got = f64::from(DEVICE.emit1(v as u8));
            assert!((want - got).abs() < 1e-6, "code {v}: firmware emits {want}, DEVICE says {got}");
        }
        assert_eq!(DEVICE.steps, 1008, "63 levels x 16 dither phases");
    }

    /// The dark end, as numbers rather than adjectives. The firmware's own doc
    /// comment says "only sRGB 0 and 1 emit nothing"; the brief says the
    /// darkest visible value moved from sRGB 34 to about 6.
    #[test]
    fn the_dark_end_is_what_the_brief_measured() {
        assert_eq!(NOMINAL.distinct_levels(), (64, 22), "bit planes alone");
        assert_eq!(TEMPORAL.distinct_levels(), (195, 6), "one frame's worth of phases");
        assert_eq!(DEVICE.distinct_levels(), (237, 2), "a held colour, all 16 phases");

        let first_lit = |p: &Panel| (0..=255u8).find(|v| p.emit1(*v) > 0.0).unwrap();
        assert_eq!(first_lit(&NOMINAL), 22);
        assert_eq!(first_lit(&TEMPORAL), 6, "the brief's 'about sRGB 6'");
        assert_eq!(first_lit(&DEVICE), 2, "the firmware's 'only sRGB 0 and 1 emit nothing'");
    }

    /// Where the panel is coarser than the 8-bit hand-over and where it is
    /// finer. Above code 38 the device shows back exactly the code it was
    /// sent; below it codes share a level - at most four of them - and that
    /// collapse is the only thing an art pipeline has to dither around.
    #[test]
    fn the_collapse_is_confined_to_the_bottom_forty_codes() {
        let collapsed: Vec<u8> = (0..=255u8).filter(|v| DEVICE.code(*v) != *v).collect();
        assert_eq!(collapsed.len(), 19);
        assert_eq!(*collapsed.last().unwrap(), 38, "nothing above 38 moves");

        let mut per_level = std::collections::BTreeMap::new();
        for v in 0..=255u8 {
            *per_level.entry(DEVICE.code(v)).or_insert(0u32) += 1;
        }
        assert_eq!(*per_level.values().max().unwrap(), 4, "the worst pile-up is four codes");
    }

    /// Brightness still costs light and not depth on the dithered panel, and
    /// the old "32 levels is a dim room" reading is wrong by 205 levels.
    #[test]
    fn a_dim_room_is_the_same_depth_at_less_light() {
        let full = Lut::new(DEVICE, 255);
        let dim = Lut::new(DEVICE, 96);
        assert!(dim.map(255) < full.map(255), "brightness 96 is dimmer");
        assert!(
            (oe_light(96) - 9.0 / 25.0).abs() < 1e-6,
            "the firmware's DEFAULT_BRIGHTNESS is 9 of 25 slots"
        );
        assert_eq!(DEVICE.distinct_levels().0, 237);
        assert_eq!(DIM.distinct_levels().0, 32, "the model card 066 retired");
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
