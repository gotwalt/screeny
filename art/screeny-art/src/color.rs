//! Colour maths. Everything inside the pipeline is linear light; sRGB only
//! appears at the edges (authoring helpers and the 8-bit hand-over).

use std::sync::OnceLock;

/// Linear-light RGB, nominally 0..1 per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0.0, g: 0.0, b: 0.0 };

    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Rgb { r, g, b }
    }

    pub const fn splat(v: f32) -> Self {
        Rgb { r: v, g: v, b: v }
    }

    /// From 8-bit sRGB, the way colours are usually written down.
    pub fn from_srgb8(c: [u8; 3]) -> Self {
        Rgb::new(srgb8_to_linear(c[0]), srgb8_to_linear(c[1]), srgb8_to_linear(c[2]))
    }

    pub fn to_srgb8(self) -> [u8; 3] {
        [linear_to_srgb8(self.r), linear_to_srgb8(self.g), linear_to_srgb8(self.b)]
    }

    pub fn scale(self, k: f32) -> Self {
        Rgb::new(self.r * k, self.g * k, self.b * k)
    }

    pub fn add(self, o: Rgb) -> Self {
        Rgb::new(self.r + o.r, self.g + o.g, self.b + o.b)
    }

    pub fn lerp(self, o: Rgb, t: f32) -> Self {
        Rgb::new(
            self.r + (o.r - self.r) * t,
            self.g + (o.g - self.g) * t,
            self.b + (o.b - self.b) * t,
        )
    }

    pub fn clamp01(self) -> Self {
        Rgb::new(self.r.clamp(0.0, 1.0), self.g.clamp(0.0, 1.0), self.b.clamp(0.0, 1.0))
    }

    /// Relative luminance. Rec.709 weights stand in for the panel's real
    /// primaries, which have not been measured with a colorimeter.
    pub fn luma(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// Mean channel duty: a proxy for LED current, which is what the panel's
    /// power budget cares about.
    pub fn duty(self) -> f32 {
        (self.r + self.g + self.b) / 3.0
    }
}

/// sRGB transfer function, encoded 0..1 -> linear 0..1.
///
/// PROVISIONAL: the brief's level table matches the standard sRGB curve, but the
/// firmware's final gamma is not settled. This pair of functions is the only
/// place the curve is defined.
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub fn srgb8_to_linear(v: u8) -> f32 {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))[v as usize]
}

pub fn linear_to_srgb8(v: f32) -> u8 {
    (linear_to_srgb(v) * 255.0 + 0.5) as u8
}

/// OKLCH -> linear sRGB, with chroma reduced until the colour is in gamut.
/// `l` 0..1, `c` roughly 0..0.35, `h` in degrees.
///
/// Design palettes here rather than in RGB/HSV: equal `l` is roughly equal
/// perceived brightness across hues, which RGB "value" is very much not on
/// this panel.
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Rgb {
    let h = h_deg.to_radians();
    let (sin, cos) = h.sin_cos();
    let in_gamut = |c: f32| {
        let rgb = oklab_to_linear(l, c * cos, c * sin);
        let ok = |v: f32| (-0.0005..=1.0005).contains(&v);
        (ok(rgb.r) && ok(rgb.g) && ok(rgb.b)).then_some(rgb)
    };
    if let Some(rgb) = in_gamut(c) {
        return rgb.clamp01();
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
    oklab_to_linear(l, lo * cos, lo * sin).clamp01()
}

fn oklab_to_linear(l: f32, a: f32, b: f32) -> Rgb {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    Rgb::new(
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_93 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_4 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    )
}

pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trips() {
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb8(srgb8_to_linear(v)), v);
        }
    }

    #[test]
    fn oklch_stays_in_gamut() {
        for h in (0..360).step_by(15) {
            let c = oklch(0.7, 0.4, h as f32);
            for v in [c.r, c.g, c.b] {
                assert!((0.0..=1.0).contains(&v));
            }
        }
    }

    #[test]
    fn oklch_white_is_neutral() {
        let w = oklch(1.0, 0.0, 0.0);
        assert!((w.r - 1.0).abs() < 0.01 && (w.g - 1.0).abs() < 0.01 && (w.b - 1.0).abs() < 0.01);
    }
}
