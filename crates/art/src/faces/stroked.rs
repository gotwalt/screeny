//! `Vesta`: the house face, drawn as stroked paths rather than as a bitmap.
//!
//! A path stroked with one round pen is a distance field, so the same glyph
//! resampled at 0.93 of its height is simply a slightly shorter glyph with its
//! own anti-aliased edge - which is what a split-flap module does to it six
//! frames out of every flip. [`distance`] is that field; a patch thresholds it
//! at half the stroke width inside `Frame::supersample`, and `weight` moves
//! that threshold, so this is the one face in the list a person can fatten.
//!
//! **The box.** A glyph is drawn in LEDs, centred, y downwards: half-width 5,
//! half-height 10, so bowls are true circles of radius 5 centred at `(0, -5)`
//! and `(0, 5)` and two of them stack exactly. Stroked at the default weight
//! that is 12 x 22 LEDs of ink in vesta's 14 x 30 module - a condensed,
//! slightly rounded grotesque, about as large as `HH:MM` can be on a 64 x 32
//! panel. It is not crisp and cannot be: every edge of it is a ramp.

/// Half-width and half-height of the stroked glyph box, in LEDs.
pub const A: f32 = 5.0;
pub const B: f32 = 10.0;

/// One piece of a numeral's skeleton.
///
/// Angles are degrees in panel convention - 0 is `+x` (right), 90 is `+y`
/// (down), so increasing an angle sweeps right, down, left, up - and an arc
/// runs from the first to the second, increasing.
#[derive(Clone, Copy, Debug)]
pub enum Stroke {
    /// `x0, y0, x1, y1`. Round caps.
    Seg(f32, f32, f32, f32),
    /// `cx, cy, r, from, to`. Round caps.
    Arc(f32, f32, f32, f32, f32),
}

use Stroke::{Arc, Seg};

/// The ten numerals.
///
/// Drawn for this panel rather than borrowed from a typeface: the bowls are
/// the module's own circles, `1` is given a foot so it does not stand alone in
/// a 14-LED module, `4` is open-topped and `7` has no crossbar to confuse with
/// a `1`. `0`, `5`, `6` and `9` share one construction - radius-5 arcs joined
/// by flat sides - so their bowls are tall and the face is a family.
pub const DIGITS: [&[Stroke]; 10] = [
    // 0: a stadium. Flat sides keep it apart from 8's pinched waist.
    &[Arc(0.0, -5.0, A, 180.0, 360.0), Seg(-A, -5.0, -A, 5.0), Seg(A, -5.0, A, 5.0), Arc(0.0, 5.0, A, 0.0, 180.0)],
    // 1: stem, flag, foot.
    &[Seg(0.0, -B, 0.0, B), Seg(-3.4, -6.8, 0.0, -B), Seg(-4.2, B, 4.2, B)],
    // 2: top bowl open below, diagonal, base.
    &[Arc(0.0, -5.0, A, 160.0, 380.0), Seg(4.70, -3.29, -4.40, 9.75), Seg(-4.9, B, 5.0, B)],
    // 3: two bowls meeting at the waist, both open to the left.
    &[Arc(0.0, -5.0, A, 170.0, 450.0), Arc(0.0, 5.0, A, 270.0, 550.0)],
    // 4: open top - apex, diagonal, crossbar, stem.
    &[Seg(2.6, -B, 2.6, B), Seg(2.6, -B, -5.0, 3.4), Seg(-5.0, 3.4, 5.0, 3.4)],
    // 5: bar, a short stem, and a tall bowl. The bowl is the 0's own stadium
    // cut open at the left - the same two arcs and a flat right side - so it
    // is two thirds of the numeral's height and as wide as the bar above it;
    // the stem is the remaining third and meets the bowl's shoulder, not its
    // side. (The first 5 hung a small round bowl off the *side* of a stem 13
    // LEDs long, and the author saw it at once: lopsided, all neck.)
    &[Seg(-4.4, -B, 4.6, -B), Seg(-4.4, -B, -4.4, -1.88), Arc(0.0, 0.5, A, 208.4, 360.0), Seg(A, 0.5, A, 5.0), Arc(0.0, 5.0, A, 0.0, 165.0)],
    // 6: the 0 with a bowl closed inside its lower two thirds and its top
    // right left open - one construction with the 0, the 5 and the 9, so the
    // face reads as a family. Told from the 8 by its flat left side and open
    // shoulder, from the 0 by the bar across its middle.
    &[Arc(0.0, -5.0, A, 180.0, 318.0), Seg(-A, -5.0, -A, 5.0), Arc(0.0, 5.0, A, 0.0, 180.0), Seg(A, 1.0, A, 5.0), Arc(0.0, 1.0, A, 180.0, 360.0)],
    // 7: bar and diagonal, no crossbar.
    &[Seg(-4.9, -B, 5.0, -B), Seg(5.0, -B, -1.8, B)],
    // 8: two circles, pinched waist.
    &[Arc(0.0, -5.0, A, 0.0, 360.0), Arc(0.0, 5.0, A, 0.0, 360.0)],
    // 9: the 6 turned through half a turn, exactly.
    &[Arc(0.0, 5.0, A, 0.0, 138.0), Seg(A, -5.0, A, 5.0), Arc(0.0, -5.0, A, 180.0, 360.0), Seg(-A, -5.0, -A, -1.0), Arc(0.0, -1.0, A, 0.0, 180.0)],
];

/// Distance in LEDs from `(x, y)` to digit `d`'s skeleton, `(x, y)` measured
/// from the centre of the glyph box with y downwards.
///
/// Ink is `distance <= weight * 0.5`; the supersampler does the rest.
#[must_use]
pub fn distance(d: usize, x: f32, y: f32) -> f32 {
    DIGITS[d % 10].iter().fold(f32::MAX, |best, s| best.min(to_stroke(*s, x, y)))
}

fn to_stroke(s: Stroke, px: f32, py: f32) -> f32 {
    match s {
        Seg(x0, y0, x1, y1) => {
            let (dx, dy) = (x1 - x0, y1 - y0);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            ((px - x0 - dx * t).powi(2) + (py - y0 - dy * t).powi(2)).sqrt()
        }
        Arc(cx, cy, r, from, to) => {
            let (dx, dy) = (px - cx, py - cy);
            let radial = (dx * dx + dy * dy).sqrt();
            // Where this point sits on the circle, brought into `from..from+360`
            // so a comparison against `to` does not depend on how the arc was
            // written down (a 3's bowl runs from 170 to 450).
            let mut a = dy.atan2(dx).to_degrees();
            while a < from {
                a += 360.0;
            }
            if a <= to {
                (radial - r).abs()
            } else {
                let end = |deg: f32| {
                    let (s, c) = deg.to_radians().sin_cos();
                    ((px - cx - r * c).powi(2) + (py - cy - r * s).powi(2)).sqrt()
                };
                end(from).min(end(to))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every numeral stays inside the box it was drawn for, so a module's
    /// black margin is really black and two neighbours never touch.
    #[test]
    fn every_numeral_fits_its_box() {
        for d in 0..10 {
            let mut ink = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for iy in -140..=140 {
                for ix in -80..=80 {
                    let (x, y) = (ix as f32 * 0.1, iy as f32 * 0.1);
                    if distance(d, x, y) <= 1.0 {
                        ink = (ink.0.min(x), ink.1.max(x), ink.2.min(y), ink.3.max(y));
                    }
                }
            }
            assert!(ink.0 >= -(A + 1.1) && ink.1 <= A + 1.1, "{d} is {ink:?} wide");
            assert!(ink.2 >= -(B + 1.1) && ink.3 <= B + 1.1, "{d} is {ink:?} tall");
            // And it uses the box: a numeral that only filled half of it would
            // read as a different size from its neighbours.
            assert!(ink.1 - ink.0 > 6.0 && ink.3 - ink.2 > 18.0, "{d} is only {ink:?}");
        }
    }

    /// The ten are told apart at the size they are drawn at. Rasterise each
    /// glyph at one sample per LED over its box and count the LEDs where two
    /// digits disagree: the closest pair on a clock face is the one to worry
    /// about, and even that must differ over a good fraction of the ink.
    #[test]
    fn no_two_numerals_are_near_each_other() {
        let bitmap = |d: usize| -> Vec<bool> {
            (0..22 * 14)
                .map(|i| {
                    let (x, y) = ((i % 14) as f32 - 6.5, (i / 14) as f32 - 10.5);
                    distance(d, x, y) <= 1.0
                })
                .collect()
        };
        let maps: Vec<Vec<bool>> = (0..10).map(bitmap).collect();
        let mut worst = (usize::MAX, 0, 0);
        for a in 0..10 {
            for b in a + 1..10 {
                let diff = maps[a].iter().zip(&maps[b]).filter(|(x, y)| x != y).count();
                if diff < worst.0 {
                    worst = (diff, a, b);
                }
            }
        }
        assert!(worst.0 >= 12, "{} and {} differ in only {} LEDs", worst.1, worst.2, worst.0);
    }

    /// An arc's distance is the circle's where the point is on it and the
    /// nearer end cap's where it is not, with no seam between the two.
    #[test]
    fn an_arc_is_continuous_at_its_ends() {
        let a = Arc(0.0, 0.0, 5.0, 0.0, 180.0);
        assert!((to_stroke(a, 5.0, 0.0)).abs() < 1e-5, "on the arc at its start");
        assert!((to_stroke(a, 0.0, 5.0)).abs() < 1e-5, "on the arc in the middle");
        assert!((to_stroke(a, -5.0, 0.0)).abs() < 1e-5, "on the arc at its end");
        // Just past the end, the distance grows from zero rather than jumping.
        let just_past = to_stroke(a, -5.0 * (0.01_f32).cos(), -5.0 * (0.01_f32).sin());
        assert!(just_past < 0.06, "a step at the end cap: {just_past}");
    }
}
