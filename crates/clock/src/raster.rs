//! Drawing the dials on the device.
//!
//! The Studio draws them with six-by-six supersampling and then maps the frame
//! onto 31 colours so that it compresses. Neither is any use here. Instead each
//! pixel works out **how much of it the hand covers**, exactly, from the
//! distance to the hand's centre line: one sample per pixel, and a smoother edge
//! than the supersampler's 36 levels. Nothing is palette-mapped, because no frame
//! goes near a network.
//!
//! Light adds linearly, so the two hands are composed in linear light and only
//! then encoded to sRGB, through a table - this runs per lit pixel.
//!
//! That table also does what the Studio's `AlignedDark` output does to a frame
//! before sending it: a dark channel lands on a whole panel level rather than
//! between two. Between two, the panel's temporal dither alternates them and
//! the eye can see that up close (brief 2.1.1). Higher up it cannot, and the
//! dither is what makes the ramp smooth, so it is left alone there.
//!
//! Only the 8 x 8 cell of each dial is visited, and a hand's body needs no
//! square root at all (the perpendicular distance *is* the distance); only the
//! pixels around its two round ends pay for one.

use alloc::boxed::Box;
use alloc::vec;

use crate::face::{dark_levels, Hands, Rest, CELL, CLOCKS, COLS, ROWS};
#[allow(unused_imports)]
use crate::math::F32Ext;

/// Panel width in pixels.
pub const W: usize = 64;
/// Panel height in pixels.
pub const H: usize = 32;
/// One frame of sRGB888, row-major.
pub const FRAME_BYTES: usize = W * H * 3;

/// A colour in OKLCH; `light` is the hand itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub hue: f32,
    pub chroma: f32,
    pub light: f32,
}

impl Tint {
    /// The ink, in linear sRGB.
    pub fn ink(self) -> [f32; 3] {
        oklch(self.light, self.chroma, self.hue)
    }
}

/// The look of the hands, as opposed to how they move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Half the hands' thickness at the centre of the dial, LEDs.
    pub half: f32,
    /// Half the thickness at the tip while dancing, as a share of `half`.
    pub tip: f32,
    /// Hour hand, minute hand.
    pub tints: [Tint; 2],
    /// Brightness of a ring round each dial, 0 for none.
    pub ring: f32,
    /// A resting dial's held ink: `0` is its own colour scaled, anything else a
    /// [`crate::face::DARK_CHOICES`] ramp.
    pub dark: usize,
}

impl Default for Look {
    /// The owner's "Gold and Blue" (2026-10-28): a brass hour hand and a slate
    /// minute hand, muted. The same numbers as the Studio's patch defaults.
    fn default() -> Self {
        let light = 0.67;
        Look {
            half: 1.55 * 0.5,
            tip: 0.2,
            tints: [
                Tint { hue: 74.0, chroma: 0.065, light },
                Tint { hue: 240.0, chroma: 0.065, light },
            ],
            ring: 0.0,
            dark: 0,
        }
    }
}

/// The panel's duty levels, as a share of full scale.
const LEVELS: f32 = 63.0;
/// Below this level a channel is steadied onto a whole level: the Studio's
/// `DARK_ALIGN_LEVEL`, and `crates/art` checks the two agree.
pub const DARK_ALIGN_LEVEL: usize = 16;
/// The sRGB code that lands exactly on each level below [`DARK_ALIGN_LEVEL`],
/// so the panel's dither has nothing to do. `crates/art` checks this against
/// `screeny_panel::aligned_levels`, which is host-only.
pub const LEVEL_CODES: [u8; DARK_ALIGN_LEVEL] = [0, 34, 50, 62, 71, 80, 87, 94, 100, 106, 111, 116, 121, 126, 130, 134];

/// Entries in the linear -> sRGB table. 1024 steps of full scale: a hand's ink
/// sits around a third of that, so the faintest resting dial still has dozens.
const LUT: usize = 1024;

/// A [`Look`] made ready to draw: the inks worked out and the table built.
pub struct Raster {
    look: Look,
    inks: [[f32; 3]; 2],
    lut: Box<[u8]>,
}

impl Raster {
    pub fn new(look: Look) -> Self {
        let mut lut = vec![0u8; LUT].into_boxed_slice();
        for (i, v) in lut.iter_mut().enumerate() {
            *v = encode(i as f32 / (LUT - 1) as f32);
        }
        Raster { look, inks: look.tints.map(Tint::ink), lut }
    }

    pub fn look(&self) -> Look {
        self.look
    }

    /// Change the look without touching the table.
    pub fn set_look(&mut self, look: Look) {
        self.look = look;
        self.inks = look.tints.map(Tint::ink);
    }

    /// Draw the grid into `out`, which is cleared first. `idle` marks the dials
    /// that are not part of a digit, drawn back towards `rest` as the picture
    /// `settled` onto the time.
    pub fn draw(&self, out: &mut [u8], angles: &[Hands; CLOCKS], idle: &[bool; CLOCKS], rest: Rest, settled: f32) {
        out[..FRAME_BYTES].fill(0);
        let settled = settled.clamp(0.0, 1.0);
        let scale = rest.scale(settled);
        let half = self.look.half;
        // A digit is hands meeting end to end, and a stroke that pinched at
        // every joint would not read as one, so the taper goes as the picture
        // settles and the hands are blunt again by the time they are a numeral.
        let tip = (self.look.tip + (1.0 - self.look.tip) * settled).clamp(0.0, 1.0);
        let top = (H - ROWS * CELL as usize) / 2;
        let reach_full = CELL * 0.5 - half + half * (1.0 - tip);
        // Only once a resting dial has truly landed: a fade is moving content,
        // and stays on the ordinary ramp the whole way.
        let shade = (settled >= 1.0 && rest.ink < 1.0)
            .then(|| dark_levels(self.look.dark, rest.ink))
            .flatten()
            .map(|levels| levels.map(|l| srgb_to_linear(LEVEL_CODES[(l as usize).min(DARK_ALIGN_LEVEL - 1)])));
        for row in 0..ROWS {
            for col in 0..COLS {
                let i = row * COLS + col;
                let (ink, reach) = if idle[i] { (scale[0], scale[1]) } else { (1.0, 1.0) };
                let len = reach_full * reach;
                let hands = [hand(angles[i][0], len, half, tip), hand(angles[i][1], len, half, tip)];
                let shade = if idle[i] { shade } else { None };
                self.dial(out, col * CELL as usize, top + row * CELL as usize, &hands, ink, shade);
            }
        }
    }

    fn dial(&self, out: &mut [u8], x0: usize, y0: usize, hands: &[Hand; 2], ink: f32, shade: Option<[f32; 3]>) {
        let cell = CELL as usize;
        let ring = self.look.ring;
        let (inks, ink) = match &shade {
            Some(s) => ([*s, *s], 1.0),
            None => (self.inks, ink),
        };
        for y in 0..cell {
            let py = y as f32 + 0.5 - CELL * 0.5;
            for x in 0..cell {
                let px = x as f32 + 0.5 - CELL * 0.5;
                let over = hands[0].coverage(px, py);
                // Both hands leave one centre, so their coverage is nested, not
                // independent: the minute hand adds only what shows past the
                // hour hand. (Multiplying by `1 - over` would double the edge of
                // two hands lying together, and tint it.)
                let under = (hands[1].coverage(px, py) - over).max(0.0);
                let (mut a, mut b) = (over * ink, under * ink);
                if ring > 0.0 {
                    let edge = CELL * 0.5 - 0.45;
                    let d = ((px * px + py * py).sqrt() - edge).abs();
                    let left = 1.0 - over - under;
                    b += span(d, 0.3) * left * 0.1 * ring;
                }
                if a <= 0.0 && b <= 0.0 {
                    continue;
                }
                a = a.min(1.0);
                b = b.min(1.0);
                let at = ((y0 + y) * W + x0 + x) * 3;
                for c in 0..3 {
                    let v = inks[0][c] * a + inks[1][c] * b;
                    out[at + c] = self.lut[((v * (LUT - 1) as f32 + 0.5) as usize).min(LUT - 1)];
                }
            }
        }
    }
}

/// One hand, ready to be asked about pixels.
struct Hand {
    /// Unit vector along the hand; y grows downwards.
    dx: f32,
    dy: f32,
    len: f32,
    half: f32,
    /// How much of `half` is gone per LED along.
    taper: f32,
}

fn hand(angle: f32, len: f32, half: f32, tip: f32) -> Hand {
    let (s, c) = angle.to_radians().sin_cos();
    Hand { dx: s, dy: -c, len, half, taper: if len > 0.0 { half * (1.0 - tip) / len } else { 0.0 } }
}

impl Hand {
    /// How much of the pixel centred at (`px`, `py`) this hand covers, 0..1.
    #[inline(always)]
    fn coverage(&self, px: f32, py: f32) -> f32 {
        let run = px * self.dx + py * self.dy;
        let across = (px * self.dy - py * self.dx).abs();
        let along = run.clamp(0.0, self.len);
        let radius = self.half - self.taper * along;
        let beyond = run - along;
        // A pixel reaches half a diagonal; anything further than that past the
        // edge cannot be touched, and most of the cell leaves here.
        if across > radius + 0.71 || beyond.abs() > radius + 0.71 {
            return 0.0;
        }
        let distance = if beyond == 0.0 { across } else { (across * across + beyond * beyond).sqrt() };
        span(distance, radius)
    }
}

/// How much of a one-LED pixel `distance` from a stroke's centre a stroke of
/// half-width `radius` covers: the overlap of two intervals. Right for a stroke
/// thinner than a pixel too, where "distance to the edge" would overestimate.
#[inline(always)]
fn span(distance: f32, radius: f32) -> f32 {
    (radius.min(distance + 0.5) - (-radius).max(distance - 0.5)).clamp(0.0, 1.0)
}

/// OKLCH -> linear sRGB, with chroma reduced until the colour is in gamut.
/// `l` 0..1, `c` roughly 0..0.35, `h` in degrees.
pub fn oklch(l: f32, c: f32, h_deg: f32) -> [f32; 3] {
    let (sin, cos) = h_deg.to_radians().sin_cos();
    let in_gamut = |c: f32| {
        let rgb = oklab_to_linear(l, c * cos, c * sin);
        rgb.iter().all(|v| (-0.0005..=1.0005).contains(v)).then_some(rgb)
    };
    let clamp = |rgb: [f32; 3]| rgb.map(|v| v.clamp(0.0, 1.0));
    if let Some(rgb) = in_gamut(c) {
        return clamp(rgb);
    }
    let (mut lo, mut hi) = (0.0_f32, c);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if in_gamut(mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    clamp(oklab_to_linear(l, lo * cos, lo * sin))
}

fn oklab_to_linear(l: f32, a: f32, b: f32) -> [f32; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_93 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_4 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
}

/// A linear value as the code that goes to the panel.
fn encode(v: f32) -> u8 {
    let level = (v * LEVELS + 0.5) as usize;
    if level < DARK_ALIGN_LEVEL {
        LEVEL_CODES[level]
    } else {
        (linear_to_srgb(v) * 255.0 + 0.5) as u8
    }
}

fn srgb_to_linear(code: u8) -> f32 {
    let c = code as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face::{pose, resting, DEFAULT_REST, RESTS};
    use alloc::vec::Vec;

    fn frame(angles: &[Hands; CLOCKS], idle: &[bool; CLOCKS], settled: f32) -> Vec<u8> {
        let mut out = vec![0xaa; FRAME_BYTES];
        Raster::new(Look::default()).draw(&mut out, angles, idle, RESTS[DEFAULT_REST], settled);
        out
    }

    #[test]
    fn the_margins_stay_black() {
        let out = frame(&pose(23, 58, RESTS[DEFAULT_REST]), &[false; CLOCKS], 1.0);
        for y in (0..4).chain(28..32) {
            assert!(out[y * W * 3..(y + 1) * W * 3].iter().all(|b| *b == 0), "row {y}");
        }
    }

    #[test]
    fn a_digit_stroke_is_solid_across_a_cell_edge() {
        // 1111: the second column of each digit is one bar. Its top dial is all
        // hour hand, and the top half of the next is too (`DD`, then `UD`), so
        // one ink runs from the first dial's centre across the join to the second's.
        let out = frame(&pose(11, 11, RESTS[DEFAULT_REST]), &resting(11, 11), 1.0);
        let x = 8 + 3;
        let lit: Vec<u8> = (8..16).map(|y| out[(y * W + x) * 3]).collect();
        let (lo, hi) = (lit.iter().min().unwrap(), lit.iter().max().unwrap());
        assert!(*lo as f32 > *hi as f32 * 0.8, "the bar pinches: {lit:?}");
    }

    #[test]
    fn a_turning_hand_keeps_its_ink() {
        // The total light in a hand must not pulse as it turns: that is what
        // aliasing looks like on a slow hand.
        let ink = |deg: f32| {
            let mut a = [[0.0; 2]; CLOCKS];
            a.iter_mut().for_each(|h| *h = [deg, deg]);
            let out = frame(&a, &[false; CLOCKS], 0.0);
            out[..3 * W * 12].iter().map(|b| (*b as f32 / 255.0).powf(2.2)).sum::<f32>()
        };
        let all: Vec<f32> = (0..360).step_by(3).map(|d| ink(d as f32 + 0.37)).collect();
        let (lo, hi) = all.iter().fold((f32::MAX, 0.0f32), |(l, h), v| (l.min(*v), h.max(*v)));
        assert!((hi - lo) / hi < 0.18, "ink swings {lo}..{hi}");
    }

    #[test]
    fn a_hand_overlaps_without_adding_up() {
        // Both hands at the same angle: the hour hand lies over the minute hand.
        let mut a = [[0.0; 2]; CLOCKS];
        a.iter_mut().for_each(|h| *h = [90.0, 90.0]);
        let out = frame(&a, &[false; CLOCKS], 1.0);
        // The stroke is 1.55 LEDs centred on a pixel boundary: 77.5% of the two
        // rows either side of it, in the hour hand's colour and no other.
        let ink = Tint::ink(Look::default().tints[0]);
        for c in 0..3 {
            let want = encode(ink[c] * 0.775) as i32;
            let got = out[(7 * W + 6) * 3 + c] as i32;
            assert!((got - want).abs() <= 2, "channel {c}: {got} against {want}");
        }
    }
}
