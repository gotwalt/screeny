//! Aligned palettes: colours chosen as whole level triples (brief 2.1.1,
//! `docs/design/aligned-palette.md`).
//!
//! [`Panel::AlignedDark`](super::Panel::AlignedDark) steadies a dark pixel by
//! rounding each channel to its own nearest level, and three channels
//! rounding independently can each go the other way: a muted colour - the
//! kind that lives near black - changes hue (hearth's card 374 ramp is the
//! worked example). The fix is to choose the three levels together. This is
//! physarum's search, moved here so any patch can have a steady, hue-true
//! dark end without picking triples by hand.
//!
//! **Not for per pixel.** A search is about 4 µs; build a ramp once per
//! change of whatever shapes it, cache it, and index into it.

use super::{level_linear, level_triple, NATIVE_LEVELS};
use crate::color::{to_oklab, Rgb};

/// A colour the panel shows the same way on every refresh: each channel is
/// [`level_code`] of its level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aligned {
    /// [`level_triple`] of `levels`, linear light.
    pub rgb: Rgb,
    /// The three levels, for printing and pinning into a hand-picked table.
    pub levels: [u32; 3],
    /// OKLab lightness of `rgb`.
    pub l: f32,
    /// Nothing above the floor was reachable: this is white, and it does not
    /// rise. Only ever seen at the ceiling.
    pub clipped: bool,
}

/// Minimum OKLab L step between two entries of a ramp.
pub const ALIGNED_RISE: f32 = 0.004;

/// Weight of OKLab a and b (hue and chroma) against L in the search: a step
/// that is a shade off is fine, one that changes colour is not.
pub const ALIGNED_HUE_WEIGHT: f32 = 1.5;

/// The non-black level triple nearest `target` in OKLab (a and b weighted by
/// [`ALIGNED_HUE_WEIGHT`]) whose L is above `floor + ALIGNED_RISE`. `floor`
/// 0.0 is no floor.
///
/// The search is the levels either side of each channel's value and one more
/// each way, 64 triples, widened upward only if none of them clears the
/// floor. Black is never returned: a ramp's bottom step would vanish, so
/// black is the caller's to place. Below level-1 grey (sRGB 34, L 0.252) there
/// is no neutral, and a darker target comes back as the nearest level-1
/// primary or pair, whatever its hue.
#[must_use]
pub fn nearest_aligned(target: Rgb, floor: f32) -> Aligned {
    let top = NATIVE_LEVELS as i32 - 1;
    let at = |v: f32| (0..NATIVE_LEVELS).rposition(|l| level_linear(l) <= v).unwrap_or(0) as i32;
    let at = [at(target.r), at(target.g), at(target.b)];
    let want = to_oklab(target);
    let mut wide = 0;
    loop {
        let span = |c: usize| (at[c] - 1).max(0)..=(at[c] + 2 + wide).min(top);
        let mut best: Option<(f32, Aligned)> = None;
        // Ascending r, g, b with a strict `<`, so a tie goes to the first
        // found: the order physarum's palette was pinned with. Clamping the
        // window at 0 and 63 drops the duplicates physarum's search visited,
        // which could only ever tie, never win.
        for r in span(0) {
            for g in span(1) {
                for b in span(2) {
                    if r + g + b == 0 {
                        continue;
                    }
                    let levels = [r as u32, g as u32, b as u32];
                    let rgb = level_triple(levels);
                    let lab = to_oklab(rgb);
                    if lab[0] <= floor + ALIGNED_RISE {
                        continue;
                    }
                    let e = (lab[0] - want[0]).powi(2) + ALIGNED_HUE_WEIGHT * ((lab[1] - want[1]).powi(2) + (lab[2] - want[2]).powi(2));
                    if best.is_none_or(|(be, _)| e < be) {
                        best = Some((e, Aligned { rgb, levels, l: lab[0], clipped: false }));
                    }
                }
            }
        }
        if let Some((_, a)) = best {
            return a;
        }
        if at.iter().all(|a| a + 2 + wide >= top) {
            let levels = [top as u32; 3];
            let rgb = level_triple(levels);
            return Aligned { rgb, levels, l: to_oklab(rgb)[0], clipped: true };
        }
        wide += 1;
    }
}

/// One aligned entry per target, each strictly lighter than the last by more
/// than [`ALIGNED_RISE`] (unless `clipped`). Targets are usually
/// [`oklch`](crate::color::oklch) stops, darkest first.
#[must_use]
pub fn aligned_ramp(targets: impl IntoIterator<Item = Rgb>) -> Vec<Aligned> {
    let mut floor = 0.0;
    targets
        .into_iter()
        .map(|t| {
            let a = nearest_aligned(t, floor);
            floor = a.l;
            a
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::oklch;
    use crate::panel::{level_code, Panel};

    fn hue(c: Rgb) -> f32 {
        let lab = to_oklab(c);
        lab[2].atan2(lab[1]).to_degrees()
    }

    fn ramp(hue: f32, chroma: f32) -> Vec<Rgb> {
        (0..12).map(|i| oklch(0.2 + 0.7 * i as f32 / 11.0, chroma, hue)).collect()
    }

    /// Every result is three level codes, and `AlignedDark` leaves it exactly
    /// where it is whatever the dither bias: a fixed point of the backstop.
    #[test]
    fn every_entry_is_three_level_codes() {
        for i in 0..8 {
            for h in 0..12 {
                for c in [0.0, 0.05, 0.12] {
                    let a = nearest_aligned(oklch(0.2 + 0.1 * i as f32, c, 30.0 * h as f32), 0.0);
                    let codes = a.levels.map(level_code);
                    assert_eq!(a.rgb.to_srgb8(), codes, "{a:?}");
                    assert_eq!(Panel::AlignedDark.snap8(a.rgb, -0.49), codes, "{a:?}");
                    assert_eq!(Panel::AlignedDark.snap8(a.rgb, 0.49), codes, "{a:?}");
                    assert!(!a.clipped && a.levels != [0; 3]);
                }
            }
        }
    }

    #[test]
    fn ramps_rise_strictly() {
        for (h, c) in [(0.0, 0.0), (50.0, 0.05), (260.0, 0.15)] {
            let r = aligned_ramp(ramp(h, c));
            assert_eq!(r.len(), 12);
            for w in r.windows(2) {
                assert!(w[1].l > w[0].l + ALIGNED_RISE, "hue {h}: {:?} then {:?}", w[0], w[1]);
            }
            assert!(r.iter().all(|a| !a.clipped));
        }
    }

    #[test]
    fn the_ceiling_is_flagged_not_broken() {
        let a = nearest_aligned(Rgb::new(1.0, 1.0, 1.0), 0.999);
        assert!(a.clipped);
        assert_eq!(a.levels, [NATIVE_LEVELS - 1; 3]);
    }

    /// The reason this exists: a muted hue survives, down to the darkest
    /// neutral. Measured worst 22 degrees (one entry, at L 0.25); rounding
    /// each channel to its own nearest level gets 52 on the same grid, so
    /// swapping the search for that fails here. Above L 0.4 the two are
    /// close - the gain is at the dark end, which is where it is needed.
    #[test]
    fn a_muted_hue_holds() {
        for i in 0..7 {
            for h in 0..12 {
                let target = oklch(0.25 + 0.1 * i as f32, 0.05, 30.0 * h as f32);
                let a = nearest_aligned(target, 0.0);
                let d = (hue(a.rgb) - hue(target) + 540.0).rem_euclid(360.0) - 180.0;
                assert!(d.abs() <= 30.0, "{target:?}: {a:?} is {d:.1} degrees off");
            }
        }
    }
}
