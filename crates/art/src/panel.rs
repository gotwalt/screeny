//! Panel model: what the LEDs can actually show.
//!
//! **One implementation, and it is [`screeny_panel`]** (card 102). This module
//! is the art side's reading of it: the frame is handed over as 8-bit sRGB, so
//! everything here is about choosing a code and knowing what the panel will do
//! with it.
//!
//! What the device does, in one paragraph. The firmware's gamma table is the
//! sRGB EOTF scaled to `63 * 16`, so it knows each code's wanted duty to a
//! sixteenth of a level; the display loop spends that remainder across sixteen
//! successive panel refreshes (`firmware/src/display.rs`, card 030). A colour
//! that is held therefore averages **1008 duty steps** per channel, not 64,
//! and only sRGB 0 and 1 come out black. The 64-level hard quantisation this
//! module used to do was the panel before card 030: it crushed everything
//! under sRGB 22 and banded every gradient into 64 steps that the panel does
//! not have.
//!
//! The 32- and 16-level options that used to sit beside it were worse than
//! obsolete - they were a bug. They modelled "dimmed by scaling", which the
//! device has not done since card 020: it dims by shortening the
//! output-enable window, so **every duty step survives at every brightness**
//! and only the light goes away (card 066, `screeny_panel::oe_light`). A dim
//! room is [`Panel::DEVICE`] times `oe_light(brightness)`, not fewer levels.

use crate::color::{linear_to_srgb8, srgb8_to_linear, Rgb};
use serde::{Deserialize, Serialize};

pub use screeny_panel::model::DITHER_PHASES;

/// Duty levels per channel from the bit planes alone: `firmware::gamma::PLANES`
/// is 6.
pub const NATIVE_LEVELS: u32 = 64;

/// Duty steps per channel the device resolves in the time average:
/// `(NATIVE_LEVELS - 1) * DITHER_PHASES`.
pub const DEVICE_STEPS: u32 = (NATIVE_LEVELS - 1) * DITHER_PHASES;

/// Which panel a frame is quantised to and previewed through.
///
/// Two, because they are the two the brief asks to be looked at: what the
/// device does, and the same panel without its temporal dither, "because the
/// dark end behaves like the coarser number" (brief section 5). Anything a
/// person might have meant by 32 or 16 levels is a brightness, not a panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    /// **The device.** 64 duty levels spread over [`DITHER_PHASES`] refreshes:
    /// 1008 duty steps, 237 of the 256 sRGB codes distinguishable, and only
    /// sRGB 0 and 1 black.
    #[default]
    Dithered,
    /// The bit planes alone: 64 duty levels, 22 codes crushed to black, the
    /// darkest visible value sRGB 34. What the panel did before card 030, and
    /// still the honest view of anything that does not hold still long enough
    /// for the phase cycle (about 104 ms, three frames at 30 fps) to average.
    BitPlanes,
}

impl Panel {
    /// The device, by name, for code that is not reading a setting.
    pub const DEVICE: Panel = Panel::Dithered;

    fn model(self) -> screeny_panel::Panel {
        match self {
            Panel::Dithered => screeny_panel::model::DEVICE,
            Panel::BitPlanes => screeny_panel::model::NOMINAL,
        }
    }

    /// Duty steps per channel: 1008 on the device, 63 without its dither.
    pub fn steps(self) -> u32 {
        self.model().steps
    }

    /// Distinct levels this panel can reach from the 256 sRGB codes, and how
    /// many of those codes come out black. `(237, 2)` and `(64, 22)`.
    pub fn distinct_levels(self) -> (usize, usize) {
        self.model().distinct_levels()
    }

    /// Nearest displayable value for one linear channel, as light. `bias` is an
    /// ordered-dither offset in units of one **duty step**, in -0.5..0.5.
    ///
    /// The unit matters. A duty step is 1/1008 of full light on the device, so
    /// above the dark end it is a tenth of an 8-bit sRGB code and the dither
    /// does nothing at all: the hand-over rounds it away, no texture appears,
    /// and nothing is spent on the wire making noise the panel cannot show.
    /// Down where the panel is coarser than the codes - below sRGB 39, where
    /// up to four codes share a level - a duty step is much bigger than a code
    /// and the dither does the whole job of mixing the two levels either side.
    /// So one rule gives dither exactly where the panel needs it. Quantising
    /// to 64 levels, as this used to, put the dither everywhere instead.
    pub fn quantise(self, v: f32, bias: f32) -> f32 {
        let steps = self.steps() as f32;
        ((v.clamp(0.0, 1.0) * steps + bias).round().clamp(0.0, steps)) / steps
    }

    /// The sRGB8 code to hand over for one linear channel.
    pub fn code(self, v: f32, bias: f32) -> u8 {
        linear_to_srgb8(self.quantise(v, bias))
    }

    /// [`Panel::code`] on all three channels: a colour ready for the wire.
    pub fn snap8(self, c: Rgb, bias: f32) -> [u8; 3] {
        [self.code(c.r, bias), self.code(c.g, bias), self.code(c.b, bias)]
    }

    /// The light the panel emits for a handed-over sRGB8 code, time-averaged.
    pub fn emit(self, code: u8) -> f32 {
        self.model().emit1(code)
    }

    /// What a handed-over code looks like: the sRGB8 code a preview should
    /// draw. The identity above sRGB 38 on the device; a collapse below it.
    pub fn shown(self, code: u8) -> u8 {
        self.model().code(code)
    }

    /// [`Panel::shown`] over a whole `N * 3` sRGB frame, in place. This is the
    /// dark end arriving in the preview: near-black codes move onto the level
    /// the panel really has, and everything else is left exactly alone.
    pub fn show(self, rgb: &mut [u8]) {
        // 256 entries beats three branches per channel per pixel, and makes
        // `BitPlanes` cost the same as `Dithered`.
        let lut: [u8; 256] = std::array::from_fn(|v| self.shown(v as u8));
        for b in rgb.iter_mut() {
            *b = lut[*b as usize];
        }
    }

    /// Round a colour to one the panel shows exactly, staying in linear light.
    /// Palette design wants this: an entry that is already on a level does not
    /// pick up a cast from three channels rounding differently later on.
    pub fn snap(self, c: Rgb, bias: f32) -> Rgb {
        let s = self.snap8(c, bias);
        Rgb::new(srgb8_to_linear(s[0]), srgb8_to_linear(s[1]), srgb8_to_linear(s[2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::linear_to_srgb8;

    /// The first rows of the level table in brief section 2.1 - the bit planes
    /// on their own, which is what that table is.
    #[test]
    fn levels_match_the_brief() {
        let want = [0u8, 34, 50, 62, 71, 80, 87, 94, 100, 106];
        for (k, w) in want.iter().enumerate() {
            assert_eq!(linear_to_srgb8(k as f32 / 63.0), *w, "level {k}");
        }
        assert_eq!(linear_to_srgb8(62.0 / 63.0), 253);
    }

    /// Card 102's headline. The old model crushed everything under sRGB 22;
    /// the device puts out light from sRGB 2 and shows 237 distinct levels.
    #[test]
    fn the_device_has_a_dark_end_and_the_bit_planes_do_not() {
        assert_eq!(Panel::Dithered.steps(), DEVICE_STEPS);
        assert_eq!(Panel::Dithered.steps(), 1008);
        assert_eq!(Panel::BitPlanes.steps(), 63);
        assert_eq!(Panel::Dithered.distinct_levels(), (237, 2));
        assert_eq!(Panel::BitPlanes.distinct_levels(), (64, 22));

        let darkest = |p: Panel| (0..=255u8).find(|v| p.emit(*v) > 0.0).unwrap();
        assert_eq!(darkest(Panel::Dithered), 2);
        assert_eq!(darkest(Panel::BitPlanes), 22);
    }

    /// The whole dark ramp, code by code, against `screeny_panel` - which is
    /// pinned to the firmware's own gamma table in its own tests. The test
    /// card's dark ramp - sRGB 0 to 63 across the panel's 64 columns - keeps
    /// 45 distinct levels on the device and 4 without the dither.
    #[test]
    fn the_dark_ramp_agrees_with_the_panel_crate() {
        for p in [Panel::Dithered, Panel::BitPlanes] {
            let model = p.model();
            for v in 0..=255u8 {
                assert_eq!(p.emit(v), model.emit1(v), "{p:?} code {v}");
                assert_eq!(p.shown(v), model.code(v), "{p:?} code {v}");
            }
        }
        let distinct = |p: Panel| {
            (0..64u8).map(|v| p.shown(v)).collect::<std::collections::BTreeSet<_>>().len()
        };
        assert_eq!(distinct(Panel::Dithered), 45);
        assert_eq!(distinct(Panel::BitPlanes), 4);
    }

    /// The old `dark_srgb_is_off` test, kept and inverted: it asserted that
    /// sRGB 21 was black, and on this device it is not.
    #[test]
    fn dark_srgb_is_no_longer_off() {
        let p = Panel::Dithered;
        assert!(p.quantise(crate::color::srgb8_to_linear(21), 0.0) > 0.0);
        assert!(p.quantise(crate::color::srgb8_to_linear(2), 0.0) > 0.0);
        assert_eq!(p.quantise(crate::color::srgb8_to_linear(1), 0.0), 0.0);
        // And the bit planes alone still do what the brief's table says.
        assert_eq!(Panel::BitPlanes.quantise(crate::color::srgb8_to_linear(21), 0.0), 0.0);
    }

    /// Quantising costs at most a duty step, and handing the result over as an
    /// 8-bit code costs at most four more, which is 0.4% of full light at the
    /// top of the range where the codes are coarser than the panel.
    #[test]
    fn the_hand_over_is_faithful_to_within_a_few_duty_steps() {
        let p = Panel::Dithered;
        let steps = p.steps() as f32;
        let mut worst = 0.0f32;
        for i in 0..=10_000 {
            let v = i as f32 / 10_000.0;
            let err = (p.emit(p.code(v, 0.0)) - v).abs() * steps;
            worst = worst.max(err);
        }
        assert!(worst <= 4.5, "worst hand-over error {worst} duty steps");
    }

    /// Dither where the panel is coarse, nowhere else - the reason the bias is
    /// in duty steps. At sRGB 200 a full-amplitude ordered dither cannot move
    /// the code at all; at sRGB 10 it moves it between the two levels the
    /// panel actually has.
    #[test]
    fn dither_lands_only_where_the_panel_is_coarser_than_the_codes() {
        let p = Panel::Dithered;
        let spread = |code: u8| {
            let v = crate::color::srgb8_to_linear(code);
            let lo = p.emit(p.code(v, -0.49));
            let hi = p.emit(p.code(v, 0.49));
            (hi - lo) * p.steps() as f32
        };
        assert_eq!(spread(200), 0.0, "a duty step is a tenth of a code up here");
        assert!(spread(10) >= 1.0, "down here a duty step is bigger than a code");
    }
}
