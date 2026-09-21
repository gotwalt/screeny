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
//! successive panel refreshes, walked bit-reversed rather than in counting
//! order so the fast components of the dither move fast
//! (`firmware/src/display.rs`, card 030 then card 248). A colour that is held
//! therefore averages **1008 duty steps** per channel, not 64. Since card 248
//! a remainder of one sixteenth of a level snaps onto the level instead of
//! blinking at 9.6 Hz (`snap_dead_zone`), so only sRGB 0-4 come out black,
//! not just 0 and 1 - five codes' worth of light too small to read as light
//! anyway, traded for a dither that does not blink up close. The 64-level
//! hard quantisation this module used to do was the panel before card 030: it
//! crushed everything under sRGB 21 and banded every gradient into 64 steps
//! that the panel does not have.
//!
//! The 32- and 16-level options that used to sit beside it were worse than
//! obsolete - they were a bug. They modelled "dimmed by scaling", which the
//! device has not done since card 020: it dims by shortening the
//! output-enable window, so **every duty step survives at every brightness**
//! and only the light goes away (card 066, `screeny_panel::oe_light`). A dim
//! room is [`Panel::DEVICE`] times `oe_light(brightness)`, not fewer levels.

use crate::color::{linear_to_srgb8, srgb8_to_linear, Rgb};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub use screeny_panel::model::DITHER_PHASES;

/// Duty levels per channel from the bit planes alone: `firmware::gamma::PLANES`
/// is 6.
pub const NATIVE_LEVELS: u32 = 64;

/// Duty steps per channel the device resolves in the time average:
/// `(NATIVE_LEVELS - 1) * DITHER_PHASES`.
pub const DEVICE_STEPS: u32 = (NATIVE_LEVELS - 1) * DITHER_PHASES;

/// Which panel a frame is quantised to and previewed through.
///
/// Three, because that is what brief 2.1.1 asks for: [`Panel::Dithered`] is
/// what the device does, [`Panel::BitPlanes`] is the same panel without its
/// temporal dither ("because the dark end behaves like the coarser number",
/// section 5), and [`Panel::AlignedDark`] is the third choice card 188 adds -
/// `Dithered` everywhere but the levels the eye resolves as a blink held and
/// close up. Anything a person might have meant by 32 or 16 levels is a
/// brightness, not a panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    /// **The device.** 64 duty levels spread over [`DITHER_PHASES`] refreshes,
    /// bit-reversed and dead-zone-snapped since card 248: 1008 duty steps,
    /// 229 of the 256 sRGB codes distinguishable, and sRGB 0-4 black.
    #[default]
    Dithered,
    /// The bit planes alone: 64 duty levels, 21 codes crushed to black, the
    /// darkest visible value sRGB 21 (card 248: the firmware rounds this path
    /// instead of truncating it). What the panel did before card 030, and
    /// still the honest view of anything that does not hold still long enough
    /// for the phase cycle (about 104 ms, three frames at 30 fps) to average.
    BitPlanes,
    /// [`Panel::Dithered`] above [`DARK_ALIGN_LEVEL`]; below it, a channel is
    /// forced onto the nearest of [`screeny_panel::aligned_levels`]'s codes
    /// instead of dithered (card 188, brief 2.1.1). `Dithered`'s dark shades
    /// blink at 9.6 Hz held and close up; `BitPlanes` fixes that by throwing
    /// away the bright gradients the dither handles well. This keeps the
    /// 1008-step path everywhere but the handful of levels that need
    /// steadying.
    AlignedDark,
}

impl Panel {
    /// The device, by name, for code that is not reading a setting.
    pub const DEVICE: Panel = Panel::Dithered;

    fn model(self) -> screeny_panel::Panel {
        match self {
            // AlignedDark chooses codes differently below the threshold, but
            // it is still the device the codes are chosen for: emitted light,
            // the preview's collapse and distinct-level counts all read the
            // same physical panel as `Dithered`.
            Panel::Dithered | Panel::AlignedDark => screeny_panel::model::DEVICE,
            Panel::BitPlanes => screeny_panel::model::NOMINAL,
        }
    }

    /// Duty steps per channel: 1008 on the device, 63 without its dither.
    pub fn steps(self) -> u32 {
        self.model().steps
    }

    /// Distinct levels this panel can reach from the 256 sRGB codes, and how
    /// many of those codes come out black. `(229, 5)` and `(64, 21)`.
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
    ///
    /// [`Panel::AlignedDark`] only: below [`DARK_ALIGN_LEVEL`] this ignores
    /// `bias` and every other consideration - the whole point is that the
    /// code does not move from frame to frame.
    pub fn code(self, v: f32, bias: f32) -> u8 {
        if self == Panel::AlignedDark {
            if let Some(code) = dark_aligned_code(v) {
                return code;
            }
        }
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

// ---------------------------------------------------------------------------
// The dark-end snap (card 188, brief 2.1.1)
// ---------------------------------------------------------------------------

/// Below this level, a channel that would otherwise be dithered is forced
/// onto the nearest of [`screeny_panel::aligned_levels`]'s codes instead: a
/// level down here is at most a few sRGB codes wide, so a *held* colour
/// between two of them alternates them at 9.6 Hz and the eye resolves that up
/// close (brief 2.1, 2.1.1). Above it dithering keeps the full 1008-step
/// path, where a level is worth at most four codes and the alternation is a
/// few percent of the light, invisible even held.
///
/// A named constant, not a literal, because card 248 may bring it down: a
/// shorter, bit-reversed dither cycle needs fewer levels steadied this way.
pub const DARK_ALIGN_LEVEL: u32 = 16;

/// The sRGB8 code for `level`, from [`screeny_panel::aligned_levels`] (card
/// 188 deliverable 1), built once. Covers every level [`NATIVE_LEVELS`] has;
/// callers that only care about the dark end still go through this one
/// table, which is what [`level_triple`] and the dark-end snap both read, so
/// there is one implementation of "which code is level N" in this crate.
#[must_use]
pub fn level_code(level: u32) -> u8 {
    static LUT: OnceLock<[u8; NATIVE_LEVELS as usize]> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        let mut t = [0u8; NATIVE_LEVELS as usize];
        for row in screeny_panel::aligned_levels(NATIVE_LEVELS - 1) {
            t[row.level as usize] = row.code;
        }
        t
    });
    lut[level.min(NATIVE_LEVELS - 1) as usize]
}

/// A colour built directly from three levels (brief 2.1.1's "level triples"):
/// each channel is [`level_code`] for that level, decoded back to linear
/// light. For a dark ramp that is hand-picked rather than computed - a
/// patch's own held, dark shades (`crates/art/src/patches/clocks`) - not for
/// a value that merely ends up near one.
#[must_use]
pub fn level_triple(levels: [u32; 3]) -> Rgb {
    Rgb::new(
        srgb8_to_linear(level_code(levels[0])),
        srgb8_to_linear(level_code(levels[1])),
        srgb8_to_linear(level_code(levels[2])),
    )
}

/// [`Panel::AlignedDark`]'s snap: `v`'s nearest level (plain rounding, the way
/// a target is judged before any dithering), and [`level_code`] for it if
/// that level is below [`DARK_ALIGN_LEVEL`]. `None` above the threshold, so
/// the caller falls through to the ordinary dithered path.
fn dark_aligned_code(v: f32) -> Option<u8> {
    let level = (v.clamp(0.0, 1.0) * screeny_panel::NOMINAL.max()).round() as u32;
    (level < DARK_ALIGN_LEVEL).then(|| level_code(level))
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

    /// Card 102's headline, renumbered by card 248. The old model crushed
    /// everything under sRGB 22; the device puts out light from sRGB 5 (the
    /// dead zone's cost) and shows 229 distinct levels.
    #[test]
    fn the_device_has_a_dark_end_and_the_bit_planes_do_not() {
        assert_eq!(Panel::Dithered.steps(), DEVICE_STEPS);
        assert_eq!(Panel::Dithered.steps(), 1008);
        assert_eq!(Panel::BitPlanes.steps(), 63);
        assert_eq!(Panel::Dithered.distinct_levels(), (229, 5));
        assert_eq!(Panel::BitPlanes.distinct_levels(), (64, 21));

        let darkest = |p: Panel| (0..=255u8).find(|v| p.emit(*v) > 0.0).unwrap();
        assert_eq!(darkest(Panel::Dithered), 5);
        assert_eq!(darkest(Panel::BitPlanes), 21);
    }

    /// The whole dark ramp, code by code, against `screeny_panel` - which is
    /// pinned to the firmware's own gamma table in its own tests. The test
    /// card's dark ramp - sRGB 0 to 63 across the panel's 64 columns - keeps
    /// 39 distinct levels on the device and 4 without the dither. (Card 248:
    /// was 45 and 4; the dead zone collapses six of this particular ramp's 64
    /// codes onto a neighbour.)
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
        assert_eq!(distinct(Panel::Dithered), 39);
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

    /// Quantising costs at most a duty step, handing the result over as an
    /// 8-bit code costs at most four more, and - since card 248 - the dead
    /// zone can cost one further step: a chosen code whose own table entry
    /// happens to sit a sixteenth of a level off is shown snapped onto the
    /// level, exactly as the firmware shows it. Still under 0.6% of full
    /// light at the top of the range where the codes are coarser than the
    /// panel.
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
        assert!(worst <= 5.5, "worst hand-over error {worst} duty steps");
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

    /// Card 188. Below the threshold, `AlignedDark` ignores the dither bias
    /// entirely and always lands on the same code for the same target - the
    /// point of the whole exercise. Above it, it is `Dithered`, bias and all.
    #[test]
    fn aligned_dark_is_steady_below_the_threshold_and_dithered_above_it() {
        let p = Panel::AlignedDark;
        for v in 0..=255u8 {
            let target = crate::color::srgb8_to_linear(v);
            let level = (target * screeny_panel::NOMINAL.max()).round() as u32;
            let lo = p.code(target, -0.49);
            let hi = p.code(target, 0.49);
            if level < DARK_ALIGN_LEVEL {
                assert_eq!(lo, hi, "code {v} (level {level}): the bias moved an aligned dark code");
                assert_eq!(lo, level_code(level), "code {v} (level {level})");
            } else {
                assert_eq!(lo, Panel::Dithered.code(target, -0.49), "code {v} above the threshold");
                assert_eq!(hi, Panel::Dithered.code(target, 0.49), "code {v} above the threshold");
            }
        }
    }

    /// Every aligned code, whatever level it was chosen for, is itself within
    /// 2/16 of that level - `AlignedDark` cannot introduce a worse blink than
    /// the one it is fixing.
    #[test]
    fn every_aligned_dark_code_is_close_to_its_level() {
        for level in 0..DARK_ALIGN_LEVEL {
            let code = level_code(level);
            let (nearest, offset) = screeny_panel::nearest_level(screeny_panel::duty_16ths(code));
            assert_eq!(nearest, level, "level {level}: code {code}'s nearest level moved");
            assert!((0..=2).contains(&offset), "level {level}: code {code} is {offset} sixteenths off");
        }
    }

    /// `level_triple` round-trips through [`level_code`]: three levels in,
    /// the aligned codes' own linear light out, nothing in between.
    #[test]
    fn level_triple_is_three_aligned_codes() {
        let c = level_triple([1, 2, 3]);
        assert_eq!(c.r, crate::color::srgb8_to_linear(level_code(1)));
        assert_eq!(c.g, crate::color::srgb8_to_linear(level_code(2)));
        assert_eq!(c.b, crate::color::srgb8_to_linear(level_code(3)));
    }

    /// `Panel::DEVICE`, `distinct_levels` and the rest of the physical-panel
    /// queries read `AlignedDark` as the same device `Dithered` does - only
    /// code selection differs.
    #[test]
    fn aligned_dark_is_the_same_device_as_dithered() {
        assert_eq!(Panel::AlignedDark.steps(), Panel::Dithered.steps());
        assert_eq!(Panel::AlignedDark.distinct_levels(), Panel::Dithered.distinct_levels());
        for v in 0..=255u8 {
            assert_eq!(Panel::AlignedDark.emit(v), Panel::Dithered.emit(v), "code {v}");
            assert_eq!(Panel::AlignedDark.shown(v), Panel::Dithered.shown(v), "code {v}");
        }
    }
}
