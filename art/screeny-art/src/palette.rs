//! Designed palettes, and mapping a continuous-colour frame onto one.
//!
//! This is how continuous renders (3D shading, simulations) get "inside" what
//! the panel can carry: instead of handing the sender hundreds of colours to
//! quantise as it sees fit, the piece picks at most 32, in a perceptual space,
//! and the frame goes out indexed and exact (brief section 2.3).
//!
//! Map *after* any supersampled downsample. Averaging samples makes new
//! in-between colours, so a palette applied before it would not survive.

use crate::color::{oklch, to_oklab, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, MAX_PALETTE, N, W};

pub struct Palette {
    colours: Vec<Rgb>,
    lab: Vec<[f32; 3]>,
    /// Lightness distance between neighbouring entries: the dither amplitude.
    step: f32,
}

impl Palette {
    /// Any colours, up to 32. `step` is the typical OKLab lightness gap between
    /// neighbours, which sets how far dither may push a pixel.
    pub fn new(mut colours: Vec<Rgb>, step: f32) -> Self {
        colours.truncate(MAX_PALETTE);
        let lab = colours.iter().map(|c| to_oklab(*c)).collect();
        Palette { colours, lab, step }
    }

    /// True black plus, for each hue, `steps` lightnesses from `dark` to
    /// `light` (OKLCH). Keep `dark` around 0.4 or above: the panel has almost
    /// no levels below that. `hues.len() * steps` must be at most 31.
    pub fn ramps(hues: &[f32], steps: usize, (dark, light): (f32, f32), chroma: f32) -> Self {
        let steps = steps.max(1);
        let gap = if steps > 1 { (light - dark) / (steps - 1) as f32 } else { light - dark };
        let mut colours = vec![Rgb::BLACK];
        for &h in hues {
            for k in 0..steps {
                colours.push(oklch(dark + gap * k as f32, chroma, h));
            }
        }
        Palette::new(colours, gap.abs().max(0.02))
    }

    pub fn len(&self) -> usize {
        self.colours.len()
    }

    pub fn is_empty(&self) -> bool {
        self.colours.is_empty()
    }

    /// Nearest palette entry in OKLab for every pixel, through a fixed ordered
    /// dither on lightness. `strength` 0 gives hard bands, 1 a full step of
    /// dither. Pixels that are already black stay black.
    pub fn map(&self, frame: &Frame, dither: Dither, strength: f32) -> Frame {
        let black = self.colours.iter().position(|c| *c == Rgb::BLACK);
        let indices = (0..N)
            .map(|i| {
                let c = frame.pixel(i).clamp01();
                if let (Some(b), true) = (black, c.r + c.g + c.b < 1e-4) {
                    return b as u8;
                }
                let mut lab = to_oklab(c);
                lab[0] += dither.threshold(i % W, i / W) * self.step * strength;
                let dist = |p: &[f32; 3]| (0..3).map(|k| (p[k] - lab[k]).powi(2)).sum::<f32>();
                (0..self.lab.len())
                    .min_by(|a, b| dist(&self.lab[*a]).total_cmp(&dist(&self.lab[*b])))
                    .unwrap_or(0) as u8
            })
            .collect();
        Frame::Indexed { palette: self.colours.clone(), indices }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::linear_frame;

    #[test]
    fn a_gradient_lands_inside_the_palette() {
        let pal = Palette::ramps(&[20.0, 140.0, 260.0], 6, (0.42, 0.88), 0.2);
        assert_eq!(pal.len(), 19);
        let f = linear_frame(|x, y| Rgb::new(x as f32 / 63.0, y as f32 / 31.0, 0.3));
        let Frame::Indexed { palette, indices } = pal.map(&f, Dither::BlueNoise, 1.0) else { panic!("not indexed") };
        assert_eq!(palette.len(), 19);
        assert!(indices.iter().all(|i| (*i as usize) < 19));
    }

    #[test]
    fn black_stays_black() {
        let pal = Palette::ramps(&[0.0], 4, (0.42, 0.88), 0.2);
        let Frame::Indexed { indices, .. } = pal.map(&Frame::black(), Dither::BlueNoise, 1.0) else { panic!() };
        assert!(indices.iter().all(|i| *i == 0));
    }
}
