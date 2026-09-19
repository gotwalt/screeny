//! Ordered dither. The pattern is fixed in screen space, so it reads as a
//! stable screen-door texture instead of the crawling mess error diffusion
//! makes of animation (brief section 2.4). It also compresses: a regular
//! pattern is cheap for the LZ, random noise is the most expensive thing you
//! can draw.

use std::sync::LazyLock;

/// 8x8 Bayer matrix, normalised to -0.5..0.5.
pub static BAYER8: LazyLock<[f32; 64]> = LazyLock::new(|| {
    let mut m = [0f32; 64];
    for y in 0..8usize {
        for x in 0..8usize {
            // Standard recursive construction: interleave the bits of x^y and x.
            let mut v = 0u32;
            let (mut xx, mut yy) = (x as u32, (x ^ y) as u32);
            for b in 0..3 {
                v |= ((yy & 1) << (2 * b)) | ((xx & 1) << (2 * b + 1));
                xx >>= 1;
                yy >>= 1;
            }
            m[y * 8 + x] = (v as f32 + 0.5) / 64.0 - 0.5;
        }
    }
    m
});

#[inline]
pub fn bayer(x: usize, y: usize) -> f32 {
    BAYER8[(y % 8) * 8 + (x % 8)]
}
