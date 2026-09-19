//! Colour spaces for the encoders, the scorer, the art and the simulator.
//!
//! Host side only: `f32` and tables are fine here, none of this ships to the
//! ESP32. Lifted from `lab/src/color.rs` (card 002) with two additions that
//! card 031 needed - [`cbrt_fast`] and a packed-`u32` colour key - and, since
//! card 016, this is the *only* copy: `crates/screeny`, `crates/demos` and
//! `crates/sim` had three, which is three chances for the sender to score a
//! frame in a colour space the preview does not draw it in.
//!
//! Everything blends, filters and dithers in linear light; sRGB8 exists only
//! at the edges (frames out, preview PNGs).

use std::sync::LazyLock;

/// sRGB EOTF: 8-bit code value -> linear light in 0..1.
pub static SRGB_TO_LIN: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut t = [0f32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = srgb_to_lin_f(i as f32 / 255.0);
    }
    t
});

/// Pack an sRGB888 colour into a `u32` key. `0xFF_FFFF` is the largest value,
/// so `u32::MAX` is free to use as an "empty" sentinel in caches.
#[inline(always)]
#[must_use]
pub const fn pack(c: [u8; 3]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

/// Inverse of [`pack`].
#[inline(always)]
#[must_use]
pub const fn unpack(v: u32) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// sRGB EOTF on a float code value in `0..=1`. [`SRGB_TO_LIN`] is this
/// sampled at the 256 8-bit code values, and a lookup there is exact.
#[must_use]
pub fn srgb_to_lin_f(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light -> sRGB, float in and out.
#[must_use]
pub fn lin_to_srgb_f(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Linear light -> 8-bit sRGB code value.
#[must_use]
pub fn lin_to_srgb8(v: f32) -> u8 {
    (lin_to_srgb_f(v) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Cube root, ~11 significant bits, no `libm` call.
///
/// The Oklab transform needs three cube roots per colour and the chooser does
/// tens of thousands of them per frame (card 031). This is the classic
/// exponent-divide seed plus two Newton steps; over the argument range Oklab
/// actually uses (linear light in `0..=1`, so `l`,`m`,`s` in `0..=1`) the
/// relative error stays below 2e-6 - about fifteen ulp, and four orders of
/// magnitude smaller than the differences the chooser ranks. `tests/color.rs`
/// pins that.
#[inline(always)]
#[must_use]
pub fn cbrt_fast(x: f32) -> f32 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let neg = x < 0.0;
    let a = if neg { -x } else { x };
    // Seed: divide the biased exponent by three.
    let mut y = f32::from_bits(a.to_bits() / 3 + 0x2a51_2cc2);
    // Two Newton steps on y^3 = a.
    y -= (y - a / (y * y)) * (1.0 / 3.0);
    y -= (y - a / (y * y)) * (1.0 / 3.0);
    if neg {
        -y
    } else {
        y
    }
}

/// Linear sRGB -> Oklab (Bjorn Ottosson's matrices).
#[inline]
#[must_use]
pub fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let l_ = cbrt_fast(l);
    let m_ = cbrt_fast(m);
    let s_ = cbrt_fast(s);
    [
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    ]
}

/// [`oklab`] with the standard library's cube root. The reference the fast
/// path is checked against; not used in any hot loop.
#[must_use]
pub fn oklab_exact(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Oklab -> linear sRGB (turns cluster centroids back into colours).
#[must_use]
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

/// OKLCH (L in 0..1, chroma, hue in turns) -> linear sRGB, gamut-mapped by
/// reducing chroma until the result fits. Desaturating rather than clipping
/// keeps the hue: clipping a channel is what turns a designed ramp muddy.
#[must_use]
pub fn oklch_to_lin(l: f32, c: f32, h_turns: f32) -> [f32; 3] {
    let h = h_turns * std::f32::consts::TAU;
    let (sh, ch) = (h.sin(), h.cos());
    let mut chroma = c;
    for _ in 0..24 {
        let rgb = oklab_inv([l, chroma * ch, chroma * sh]);
        if rgb.iter().all(|v| *v >= -0.001 && *v <= 1.001) {
            return [
                rgb[0].clamp(0.0, 1.0),
                rgb[1].clamp(0.0, 1.0),
                rgb[2].clamp(0.0, 1.0),
            ];
        }
        chroma *= 0.88;
    }
    let rgb = oklab_inv([l, 0.0, 0.0]);
    [
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
    ]
}

/// [`oklch_to_lin`] encoded back to an sRGB888 colour.
#[must_use]
pub fn oklch_to_srgb8(l: f32, c: f32, h_turns: f32) -> [u8; 3] {
    lin_to_srgb8_3(oklch_to_lin(l, c, h_turns))
}

/// [`lin_to_srgb8`] on all three channels.
#[must_use]
pub fn lin_to_srgb8_3(c: [f32; 3]) -> [u8; 3] {
    [lin_to_srgb8(c[0]), lin_to_srgb8(c[1]), lin_to_srgb8(c[2])]
}

/// Relative luminance of a linear-light colour (Rec.709 weights).
#[must_use]
pub fn luma_lin(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// The name the art side calls [`lin`] by.
pub use self::lin as srgb8_to_lin;

/// Emitted linear light of an sRGB888 pixel, before the panel model.
#[inline(always)]
#[must_use]
pub fn lin(c: [u8; 3]) -> [f32; 3] {
    let t = &*SRGB_TO_LIN;
    [
        t[c[0] as usize],
        t[c[1] as usize],
        t[c[2] as usize],
    ]
}

/// sRGB888 -> Oklab, via the sRGB EOTF.
#[inline]
#[must_use]
pub fn oklab_srgb8(c: [u8; 3]) -> [f32; 3] {
    oklab(lin(c))
}

/// Squared Oklab distance; the inner-loop cost function.
#[inline(always)]
#[must_use]
pub fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    x * x + y * y + z * z
}

/// Rec.709 luma of an sRGB888 pixel, in gamma space (for SSIM).
#[must_use]
pub fn luma_gamma(c: [u8; 3]) -> f32 {
    0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32
}

/// A direct-mapped colour -> Oklab memo.
///
/// Candidate frames decode to few distinct colours (32 for `PAL5`, <=256 for
/// `PAL8_LZ`, <=1024 for `BC1_DUAL`) and palettes barely move between frames,
/// so a fixed table with no eviction policy hits almost always and costs one
/// masked compare per pixel. A collision just recomputes: there is no
/// correctness question here, only speed.
pub struct LabCache {
    keys: Box<[u32]>,
    vals: Box<[[f32; 3]]>,
    mask: usize,
}

impl LabCache {
    /// A cache with `2^bits` slots.
    #[must_use]
    pub fn new(bits: u32) -> Self {
        let n = 1usize << bits;
        LabCache {
            keys: vec![u32::MAX; n].into_boxed_slice(),
            vals: vec![[0f32; 3]; n].into_boxed_slice(),
            mask: n - 1,
        }
    }

    /// Look up `f(c)`, computing and storing it on a miss.
    #[inline(always)]
    pub fn get(&mut self, c: [u8; 3], f: impl FnOnce([u8; 3]) -> [f32; 3]) -> [f32; 3] {
        let k = pack(c);
        // Fibonacci hashing: the low bits of a colour are the noisy ones, and
        // this spreads all 24 over the index.
        let i = ((k.wrapping_mul(2_654_435_761) >> 11) as usize) & self.mask;
        if self.keys[i] == k {
            return self.vals[i];
        }
        let v = f(c);
        self.keys[i] = k;
        self.vals[i] = v;
        v
    }

    /// Forget everything. Only needed when the mapping function changes.
    pub fn clear(&mut self) {
        self.keys.fill(u32::MAX);
    }
}
