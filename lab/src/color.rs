//! Colour spaces used by the *encoders and metrics* (host side, f32 is fine
//! here -- none of this ships to the ESP32).

use std::sync::LazyLock;

/// sRGB EOTF: 8-bit code value -> linear light in 0..1.
pub static SRGB_TO_LIN: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut t = [0f32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *v = if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
    }
    t
});

pub fn lin_to_srgb_f(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub fn lin_to_srgb8(v: f32) -> u8 {
    (lin_to_srgb_f(v) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Linear sRGB -> Oklab (Bjorn Ottosson's matrices).
pub fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    [
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    ]
}

/// Oklab -> linear sRGB (used to turn cluster centroids back into colours).
pub fn oklab_inv(lab: [f32; 3]) -> [f32; 3] {
    let (ll, a, b) = (lab[0], lab[1], lab[2]);
    let l_ = ll + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = ll - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = ll - 0.089_484_18 * a - 1.291_485_5 * b;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// sRGB888 -> Oklab, via the sRGB EOTF. Cached: there are only 2^24 possible
/// inputs but frames reuse colours heavily, so a per-channel table plus the
/// matrix is the right trade.
pub fn oklab_srgb8(c: [u8; 3]) -> [f32; 3] {
    let t = &*SRGB_TO_LIN;
    oklab([t[c[0] as usize], t[c[1] as usize], t[c[2] as usize]])
}

/// Squared Oklab distance; the encoders' inner-loop cost function.
pub fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    x * x + y * y + z * z
}

/// Rec.709 luma of an sRGB888 pixel, in gamma space (for SSIM).
pub fn luma_gamma(c: [u8; 3]) -> f32 {
    0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32
}
