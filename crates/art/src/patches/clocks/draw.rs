//! Drawing a grid of two-handed dials, shared by the patches built on them.

use super::Hands;
use crate::color::{oklch, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H};
use crate::palette::Palette;

/// How one hand is coloured: OKLCH hue and chroma, and the lightness of the
/// hand itself (its anti-aliased edge runs down from there towards black).
#[derive(Clone, Copy, Debug)]
pub struct Tint {
    pub hue: f32,
    pub chroma: f32,
    pub light: f32,
}

/// A dial's rest state: how much of its hands to draw, and - once they have
/// stopped moving - what colour to draw them.
#[derive(Clone, Copy, Debug)]
pub struct RestScale {
    /// Ink, `tint.scale(ink)`, while `shade` is `None` - dimming costs no
    /// palette entries: a hand's ink scaled in linear light is exactly what
    /// its anti-aliasing ramp already is, so a dimmed hand lands on a step of
    /// its own ramp. `1.0` is a hand at full strength.
    pub ink: f32,
    /// Hand length as a share of full, `1.0` for a hand at full reach.
    pub reach: f32,
    /// Card 188: once a dial has settled and is truly *held*, its ink is this
    /// exact colour instead of `tint.scale(ink)` - a level triple chosen off
    /// the panel (`screeny_art::panel::level_triple`), the same for both
    /// hands, rather than a computed value that happens to sit near one.
    /// `None` while the dial is still moving towards rest, so a fade stays
    /// the continuous, cheaply-dithered `tint.scale(ink)` the whole way.
    pub shade: Option<Rgb>,
}

impl RestScale {
    /// A dial drawn at full strength, not resting.
    pub const FULL: RestScale = RestScale { ink: 1.0, reach: 1.0, shade: None };
}

/// The darkest step of a hand's ramp. Anything dimmer is left black.
///
/// It was 0.32 while the panel was thought to have almost no levels down there;
/// card 102 measured that it has (only sRGB 0 and 1 come out black; card 248's
/// dead zone later moved that to 0-4, still a handful of codes). At 0.32 the
/// faintest edge a hand could have was a pixel it covers 4% of, and everything
/// from 2% up was rounded *up* to that - which is what made a tip look blunt
/// and a slow hand's edge arrive in a visible step. At 0.16 the faintest edge
/// is half a percent, so an edge fades in from nothing.
const DARK: f32 = 0.16;
const STEPS: usize = 15;

pub struct Dials<'a> {
    pub angles: &'a [Hands],
    pub cols: usize,
    pub rows: usize,
    /// LEDs per dial. The grid is centred vertically.
    pub cell: f32,
    /// Length of the hour and minute hands, from the centre to the middle of
    /// the rounded tip.
    pub lens: [f32; 2],
    /// Half the hands' thickness, at the centre of the dial.
    pub half: f32,
    /// Half the thickness at the tip, as a share of `half`: 1 is a hand with
    /// parallel sides and a blunt round end, less is a hand that tapers to a
    /// finer one. A tapered hand is drawn that much longer, so that its tip
    /// still ends where the blunt one did.
    pub tip: f32,
    /// Hour hand, minute hand.
    pub tints: [Tint; 2],
    /// Per-dial rest state, for dials drawn as being at rest rather than part
    /// of the picture. A dial with no entry here - an empty slice is the
    /// usual case - draws in full, [`RestScale::FULL`].
    pub rest: &'a [RestScale],
    /// Brightness of a ring round each dial, 0 for none.
    pub ring: f32,
    /// Brightness of a mark at 12 o'clock on each dial, in the hour hand's
    /// colour, 0 for none. One mark, not four: at the quarters, neighbouring
    /// dials' marks would meet across the cell edge and read as a lattice.
    pub mark: f32,
}

impl Dials<'_> {
    /// Two ramps of fifteen steps plus black: 31 colours, so the frame is
    /// sent exactly - plus, once any dial has settled onto a held, dark
    /// shade (card 188), that shade's own colour, added once and verbatim so
    /// it is an exact palette entry rather than getting nearest-matched to
    /// whichever ramp step happens to sit close. Enough ramp steps that a
    /// slowly turning hand's edge is smooth with no dither on it. The hour
    /// hand lies over the minute hand.
    pub fn draw(&self) -> Frame {
        let inks = self.tints.map(|t| oklch(t.light, t.chroma, t.hue));
        // A partly covered pixel is the ink dimmed in linear light, so that is
        // what the ramp is: the same chromaticity all the way down, in even
        // steps of lightness (lightness goes as the cube root of light).
        let mut colours = vec![Rgb::BLACK];
        for (t, ink) in self.tints.iter().zip(inks) {
            colours.extend((0..STEPS).map(|k| {
                let l = DARK + (t.light - DARK) * k as f32 / (STEPS - 1) as f32;
                ink.scale((l / t.light).powi(3))
            }));
        }
        for r in self.rest {
            if let Some(shade) = r.shade {
                if !colours.contains(&shade) {
                    colours.push(shade);
                }
            }
        }
        let palette = Palette::new(colours, 0.04);

        let top = (H as f32 - self.rows as f32 * self.cell) * 0.5;
        let frame = Frame::supersample(6, |x, y| {
            let (cx, cy) = (x / self.cell, (y - top) / self.cell);
            if cy < 0.0 || cy >= self.rows as f32 {
                return Rgb::BLACK;
            }
            let i = (cy as usize).min(self.rows - 1) * self.cols + (cx as usize).min(self.cols - 1);
            let (px, py) = ((cx.fract() - 0.5) * self.cell, (cy.fract() - 0.5) * self.cell);
            let r = self.rest.get(i).copied().unwrap_or(RestScale::FULL);
            let tip = self.tip.clamp(0.0, 1.0);
            for (h, tint) in inks.iter().enumerate() {
                let len = (self.lens[h] + self.half * (1.0 - tip)) * r.reach;
                let (distance, along) = hand_distance(px, py, self.angles[i][h], len);
                if distance <= self.half * (1.0 - (1.0 - tip) * along) {
                    return r.shade.unwrap_or_else(|| tint.scale(r.ink));
                }
            }
            if self.mark > 0.0 {
                let (mx, my) = (px, py + self.cell * 0.5 - 1.0);
                if mx * mx + my * my <= 0.75 * 0.75 {
                    return inks[0].scale(self.mark);
                }
            }
            let ring = ((px * px + py * py).sqrt() - (self.cell * 0.5 - 0.45)).abs();
            if self.ring > 0.0 && ring < 0.3 {
                inks[1].scale(0.1 * self.ring)
            } else {
                Rgb::BLACK
            }
        });
        palette.map(&frame, Dither::None, 0.0)
    }
}

/// Distance from a point to the hand - a segment from the dial's centre along
/// `angle` (degrees clockwise from 12 o'clock; y grows downwards) for `len` -
/// and how far along it the nearest point is, 0 at the centre to 1 at the tip.
fn hand_distance(px: f32, py: f32, angle: f32, len: f32) -> (f32, f32) {
    let (s, c) = angle.to_radians().sin_cos();
    let (dx, dy) = (s, -c);
    let along = (px * dx + py * dy).clamp(0.0, len);
    let distance = ((px - dx * along).powi(2) + (py - dy * along).powi(2)).sqrt();
    (distance, if len > 0.0 { along / len } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tapered hand is finer at the tip than at the centre, and reaches
    /// exactly as far as the blunt one it replaces.
    #[test]
    fn a_tapered_hand_is_finer_at_the_tip_and_no_shorter() {
        let tints = [Tint { hue: 80.0, chroma: 0.05, light: 0.93 }; 2];
        // One dial, both hands pointing right along the middle of the cell.
        let angles = vec![[90.0, 90.0]];
        let lit = |tip: f32| {
            let dials = Dials { angles: &angles, cols: 1, rows: 1, cell: 16.0, lens: [7.0; 2], half: 1.0, tip, tints, rest: &[], ring: 0.0, mark: 0.0 };
            let Frame::Indexed { indices, .. } = dials.draw() else { panic!("not indexed") };
            // How much hand is in each column of the dial: both hands share one
            // tint here, so a ramp index is a measure of ink.
            let step = |i: u8| if i == 0 { 0 } else { (usize::from(i) - 1) % STEPS + 1 };
            (0..16).map(|x| (0..H).map(|y| step(indices[y * 64 + x])).sum::<usize>()).collect::<Vec<_>>()
        };
        let (blunt, fine) = (lit(1.0), lit(0.4));
        let reach = |cols: &[usize]| cols.iter().rposition(|n| *n > 0).unwrap();
        assert_eq!(reach(&blunt), reach(&fine), "the tapered hand stops short of where the blunt one ended");
        assert_eq!(blunt[9], blunt[14], "a blunt hand has parallel sides: {blunt:?}");
        assert!(fine[14] < fine[9] && fine[14] < blunt[14], "the tip is no finer: {fine:?} against {blunt:?}");
    }

    /// A hand's soft edge must stay the hand's colour. Blue cannot hold much
    /// chroma when light, so its ink is pale, and a ramp built at constant
    /// chroma would be a different, more saturated colour than the ink dimmed:
    /// edge pixels then fell nearer the *other* hand's grey ramp.
    #[test]
    fn edges_keep_their_own_hands_colour() {
        for hue in [255.0, 20.0, 140.0, 320.0] {
            let tints = [Tint { hue, chroma: 0.15, light: 0.82 }, Tint { hue: 35.0, chroma: 0.01, light: 0.97 }];
            let angles = vec![[90.0, 270.0]; 8];
            let dials = Dials { angles: &angles, cols: 4, rows: 2, cell: 16.0, lens: [7.0; 2], half: 0.8, tip: 1.0, tints, rest: &[], ring: 0.0, mark: 1.0 };
            let Frame::Indexed { indices, .. } = dials.draw() else { panic!("not indexed") };
            // The hour hand points right and the mark is above it: everything in
            // the right half of each dial, and its top rows, is the hour ramp
            // (entries 1..=15) or black.
            for (i, &index) in indices.iter().enumerate() {
                let (x, y) = (i % 64, i / 64);
                if x % 16 >= 9 || y % 16 < 3 {
                    assert!(index <= STEPS as u8, "hue {hue}: pixel ({x},{y}) took the minute hand's colour");
                }
            }
            assert!(indices.iter().any(|i| (1..STEPS as u8).contains(i)), "no soft edges were drawn");
        }
    }
}
