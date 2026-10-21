//! The colour of fire: blackbody emission, worked out once on the CPU.
//!
//! Planck's law gives the spectrum a body at temperature `K` radiates;
//! integrating it against the CIE 1931 colour matching functions gives its
//! colour as XYZ, and the standard matrix takes that to linear sRGB. Only the
//! **chromaticity** is kept from that - real radiance spans orders of
//! magnitude across a flame, which no panel shows - and the brightness is a
//! curve of our own ([`luminance`]), steep, so only the hottest gas is bright.
//!
//! The CIE functions are the analytic multi-lobe Gaussian fit of Wyman, Sloan
//! and Shirley, "Simple Analytic Approximations to the CIE XYZ Color Matching
//! Functions" (JCGT 2013): accurate to a percent or so, which is far finer than
//! anything this panel can show, and twenty lines instead of a table.
//!
//! The top of the scale is held at [`HOTTEST_K`], a pale gold: the fire never
//! reaches white (owner's rule).

use crate::color::Rgb;

/// Kelvin at the coolest temperature that still glows, and at the hottest.
pub const COOLEST_K: f32 = 600.0;
pub const HOTTEST_K: f32 = 2900.0;

/// The panel-light of fully hot gas, in linear light (Y). Well under 1: even
/// the hottest pixels are a pale gold, never full brightness.
const MAX_Y: f32 = 0.42;

fn lobe(l: f32, mu: f32, s1: f32, s2: f32) -> f32 {
    let s = if l < mu { s1 } else { s2 };
    let t = (l - mu) / s;
    (-0.5 * t * t).exp()
}

fn cie(l: f32) -> [f32; 3] {
    let x = 1.056 * lobe(l, 599.8, 37.9, 31.0) + 0.362 * lobe(l, 442.0, 16.0, 26.7) - 0.065 * lobe(l, 501.1, 20.4, 26.2);
    let y = 0.821 * lobe(l, 568.8, 46.9, 40.5) + 0.286 * lobe(l, 530.9, 16.3, 31.1);
    let z = 1.217 * lobe(l, 437.0, 11.8, 36.0) + 0.681 * lobe(l, 459.0, 26.0, 13.8);
    [x, y, z]
}

/// The colour of a blackbody at `kelvin`, linear sRGB, scaled so its
/// luminance is 1 and clipped into gamut (low temperatures fall outside sRGB's
/// red corner; the clip keeps them a deep, saturated red).
pub fn chromaticity(kelvin: f32) -> Rgb {
    // Planck: B(l, T) = 2hc^2 / l^5 / (exp(hc / (l k T)) - 1). Constants
    // fold into c2 = hc/k; the leading factor cancels in the normalisation.
    const C2: f64 = 1.438_776_9e-2; // m K
    let mut xyz = [0.0f64; 3];
    let mut nm = 380.0f32;
    while nm <= 780.0 {
        let l = f64::from(nm) * 1e-9;
        let b = 1.0 / (l.powi(5) * ((C2 / (l * f64::from(kelvin))).exp() - 1.0));
        let m = cie(nm);
        for k in 0..3 {
            xyz[k] += b * f64::from(m[k]);
        }
        nm += 5.0;
    }
    let (x, y, z) = (xyz[0] / xyz[1], 1.0, xyz[2] / xyz[1]);
    let r = 3.240_6 * x - 1.537_2 * y - 0.498_6 * z;
    let g = -0.968_9 * x + 1.875_8 * y + 0.041_5 * z;
    let b = 0.055_7 * x - 0.204_0 * y + 1.057_0 * z;
    let (r, g, b) = (r.max(0.0) as f32, g.max(0.0) as f32, b.max(0.0) as f32);
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Rgb::new(r / luma, g / luma, b / luma)
}

/// The sim's temperature (`0..1`, 1 the burn temperature) in kelvin.
pub fn kelvin(theta: f32) -> f32 {
    // Curved, so most of the visible range is red and orange and only the
    // hottest gas goes gold.
    COOLEST_K + (HOTTEST_K - COOLEST_K) * theta.clamp(0.0, 1.0).powf(1.3)
}

/// How bright gas at `theta` glows, in linear light. Nothing below a
/// threshold (gas that has cooled stops glowing, and the eye sees only the
/// smoke it leaves), then a steep rise: most of a flame is deep red and
/// orange, its core a little gold.
pub fn luminance(theta: f32) -> f32 {
    let t = ((theta - 0.24) / 0.76).clamp(0.0, 1.0);
    MAX_Y * t * t.sqrt()
}

/// The emission of gas at `theta`, by table: [`LUT`] entries over `0..1`.
pub struct Ramp {
    lut: Vec<Rgb>,
}

pub const LUT: usize = 512;

impl Ramp {
    pub fn new() -> Ramp {
        let lut = (0..LUT)
            .map(|i| {
                let theta = i as f32 / (LUT - 1) as f32;
                let y = luminance(theta);
                if y <= 0.0 {
                    Rgb::BLACK
                } else {
                    chromaticity(kelvin(theta)).scale(y)
                }
            })
            .collect();
        Ramp { lut }
    }

    /// The table as the shader takes it: linear RGB, one vec4 an entry.
    pub fn table(&self) -> [[f32; 4]; LUT] {
        std::array::from_fn(|i| [self.lut[i].r, self.lut[i].g, self.lut[i].b, 0.0])
    }

    #[inline]
    pub fn emit(&self, theta: f32) -> Rgb {
        let i = (theta.clamp(0.0, 1.0) * (LUT - 1) as f32 + 0.5) as usize;
        self.lut[i.min(LUT - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Low is red, high is gold, and nothing is white: blue stays well under
    /// red at the top of the scale.
    #[test]
    fn the_ramp_runs_red_to_gold_and_never_white() {
        let low = chromaticity(1000.0);
        assert!(low.r > 3.0 * low.g && low.b < 0.01, "{low:?}");
        let top = chromaticity(HOTTEST_K);
        assert!(top.r > top.g && top.g > top.b && top.b < 0.5 * top.r, "{top:?}");
        let r = Ramp::new();
        let hot = r.emit(1.0);
        assert!(hot.r <= 1.0 && hot.g < 0.6 && hot.b < 0.3, "{hot:?}");
        assert_eq!(r.emit(0.1), Rgb::BLACK);
    }
}
