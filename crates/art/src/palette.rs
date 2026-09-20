//! Designed palettes, and mapping a continuous-colour frame onto one.
//!
//! This is how continuous renders (3D shading, simulations) get "inside" what
//! the panel can carry: instead of handing the sender hundreds of colours to
//! quantise as it sees fit, the patch chooses them itself, in a perceptual
//! space, and the frame goes out indexed and exact (brief section 2.3).
//!
//! **How many is "itself"?** Up to [`MAX_PALETTE`] - 256 - not 32 (card 102).
//! 32 is the size that is exact *whatever the index plane looks like*, because
//! the fixed-rate `PAL5` rung cannot overflow the budget
//! ([`GUARANTEED_PALETTE`]). Above it exactness is a compression question, and
//! the answer for flat-shaded, terraced and palette-cycled work is usually
//! yes: `PAL8_LZ` carries palette plus LZ-compressed indices, and in the lab a
//! plasma and a Mandelbrot zoom went out exactly on 98-100% of frames. What
//! costs bytes is spatial incoherence, not colour count - a 200-colour smooth
//! gradient fits where a 40-colour field of confetti does not. Nobody models
//! this: [`crate::Meter`] asks the real encoder and `Measured::exact` is the
//! answer for the frame in hand.
//!
//! Map *after* any supersampled downsample. Averaging samples makes new
//! in-between colours, so a palette applied before it would not survive.

use crate::color::{oklch, to_oklab, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, GUARANTEED_PALETTE, MAX_PALETTE, N, W};

pub struct Palette {
    colours: Vec<Rgb>,
    lab: Vec<[f32; 3]>,
    /// Lightness distance between neighbouring entries: the dither amplitude.
    step: f32,
}

impl Palette {
    /// Any colours, up to [`MAX_PALETTE`]. `step` is the typical OKLab
    /// lightness gap between neighbours, which sets how far dither may push a
    /// pixel.
    ///
    /// Beyond [`GUARANTEED_PALETTE`] the frame is exact when it compresses,
    /// which is a property of the picture; [`Palette::always_exact`] is the
    /// question "regardless of the picture". A palette for a **GPU scene** has
    /// a second, smaller ceiling: `gpu::fragment::SCENE_PALETTE`, the length of
    /// the shader's uniform array.
    pub fn new(mut colours: Vec<Rgb>, step: f32) -> Self {
        colours.truncate(MAX_PALETTE);
        let lab = colours.iter().map(|c| to_oklab(*c)).collect();
        Palette { colours, lab, step }
    }

    /// True black plus, for each hue, `steps` lightnesses from `dark` to
    /// `light` (OKLCH).
    ///
    /// `dark` used to carry the advice "keep it around 0.4 or above, the panel
    /// has almost no levels below that". That was the panel before its
    /// temporal dither: 22 of the 256 sRGB codes came out black and the darker
    /// half of sRGB was 14 levels. The device now resolves 237 levels and only
    /// sRGB 0 and 1 are black (card 102), so a ramp may reach much further
    /// down. What is still true is that the *bottom* of it is the panel's
    /// weakest range - few refreshes to average over, so a large area of it
    /// sparkles faintly, and the three channels step at different sRGB values
    /// so low greys pick up colour casts. Dark ramps are a choice now, not a
    /// thing to avoid.
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

    pub fn colours(&self) -> &[Rgb] {
        &self.colours
    }

    pub fn len(&self) -> usize {
        self.colours.len()
    }

    pub fn is_empty(&self) -> bool {
        self.colours.is_empty()
    }

    /// True when this palette goes on the wire exactly **whatever the indices
    /// do**, including pure index noise: at most [`GUARANTEED_PALETTE`]
    /// colours, which the fixed-rate rung carries in 1376 bytes.
    ///
    /// False is not "lossy". It means the answer depends on the picture, and
    /// the picture is what [`crate::Meter`] measures.
    pub fn always_exact(&self) -> bool {
        self.colours.len() <= GUARANTEED_PALETTE
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

    /// Card 102: a palette past the guaranteed 32 is not a lossy frame, it is
    /// a compression question - and a smooth, terraced picture wins it. The
    /// real encoder says so; nothing here models it.
    #[test]
    fn a_large_palette_still_goes_out_exactly() {
        let pal = Palette::ramps(&[20.0, 140.0, 260.0, 40.0], 16, (0.20, 0.92), 0.12);
        assert_eq!(pal.len(), 65, "four 16-step ramps and black");
        assert!(!pal.always_exact(), "past the fixed-rate rung's guarantee");

        let f = linear_frame(|x, y| {
            let u = x as f32 / 63.0;
            crate::color::oklch(0.2 + 0.7 * u, 0.12, 20.0 + 80.0 * (y as f32 / 31.0).floor())
        });
        let mut output = crate::pipeline::Output::default();
        output.limiter.enabled = false;
        let out = crate::Pipeline::new(output).process(pal.map(&f, Dither::None, 0.0), 1.0 / 30.0);
        assert!(out.stats.exact, "{} colours went lossy", out.stats.distinct_colours);
        assert_eq!(out.stats.codec, screeny_proto::dec::codec::PAL8_LZ);
        assert!(out.stats.encoded_bytes <= crate::meter::PAYLOAD_BYTES);
    }

    #[test]
    fn black_stays_black() {
        let pal = Palette::ramps(&[0.0], 4, (0.42, 0.88), 0.2);
        let Frame::Indexed { indices, .. } = pal.map(&Frame::black(), Dither::BlueNoise, 1.0) else { panic!() };
        assert!(indices.iter().all(|i| *i == 0));
    }
}
