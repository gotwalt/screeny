//! Drawing a grid of two-handed dials, shared by the pieces built on them.

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

/// The darkest step of a hand's ramp. Anything dimmer is left black, because
/// the panel has almost no levels down there.
const DARK: f32 = 0.32;
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
    /// Half the hands' thickness.
    pub half: f32,
    /// Hour hand, minute hand.
    pub tints: [Tint; 2],
    /// Brightness of a ring round each dial, 0 for none.
    pub ring: f32,
}

impl Dials<'_> {
    /// Two ramps of fifteen steps plus black: 31 colours, so the frame is sent
    /// exactly. Enough steps that a slowly turning hand's edge is smooth with
    /// no dither on it. The hour hand lies over the minute hand.
    pub fn draw(&self) -> Frame {
        let inks = self.tints.map(|t| oklch(t.light, t.chroma, t.hue));
        let mut colours = vec![Rgb::BLACK];
        for t in self.tints {
            colours.extend((0..STEPS).map(|k| oklch(DARK + (t.light - DARK) * k as f32 / (STEPS - 1) as f32, t.chroma, t.hue)));
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
            for h in 0..2 {
                if hand_distance(px, py, self.angles[i][h], self.lens[h]) <= self.half {
                    return inks[h];
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

/// Distance from a point to the hand: a segment from the dial's centre along
/// `angle` (degrees clockwise from 12 o'clock; y grows downwards) for `len`.
fn hand_distance(px: f32, py: f32, angle: f32, len: f32) -> f32 {
    let (s, c) = angle.to_radians().sin_cos();
    let (dx, dy) = (s, -c);
    let along = (px * dx + py * dy).clamp(0.0, len);
    ((px - dx * along).powi(2) + (py - dy * along).powi(2)).sqrt()
}
