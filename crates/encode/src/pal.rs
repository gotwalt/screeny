//! Palette mapping, dithering, and the `PAL5` wire payload.
//!
//! Lifted from `lab/src/enc/pal.rs` (card 002). The lab's uncompressed 4 bpp
//! mode is not a v1 wire codec, so what remains here is `PAL5` (spec 4.1) and
//! the index planes `PAL8_LZ` and `PAL4_LZ` compress.
//!
//! ## Dithering in linear light
//!
//! Spatial dithering works because the eye integrates *emitted light* over
//! neighbouring LEDs, so the mixing ratio between two palette entries has to
//! be solved in linear light or the result is systematically too dark.
//! Choosing *which* two entries still happens in Oklab.
//!
//! ## Why this is fast now
//!
//! Every per-pixel decision above - nearest entry, the entry on the far side,
//! the mix ratio - depends only on the pixel's **colour**, never on its
//! position. Only the final threshold compare is positional. So all of it is
//! computed once per distinct colour ([`DitherPlan`]) and the per-pixel loop
//! becomes an array read and a float compare.

use screeny_proto::{H, NPIX, W};

use screeny_panel::color::oklab;

use super::hist::Hist;
use super::nn::NnIndex;
use super::quant::{nearest_per_bin, Palette};

/// Spatial dither strategy.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dither {
    /// Nearest palette entry, no dithering.
    #[default]
    None,
    /// 8x8 Bayer, identical every frame: spatially noisy but temporally still.
    Ordered,
    /// 8x8 Bayer with a per-frame golden-ratio phase offset, so the *time*
    /// average over a few frames lands closer to the true colour.
    OrderedTemporal,
}

/// 8x8 Bayer matrix normalised to `[0, 1)`.
pub const BAYER8: [[f32; 8]; 8] = {
    let raw: [[u8; 8]; 8] = [
        [0, 32, 8, 40, 2, 34, 10, 42],
        [48, 16, 56, 24, 50, 18, 58, 26],
        [12, 44, 4, 36, 14, 46, 6, 38],
        [60, 28, 52, 20, 62, 30, 54, 22],
        [3, 35, 11, 43, 1, 33, 9, 41],
        [51, 19, 59, 27, 49, 17, 57, 25],
        [15, 47, 7, 39, 13, 45, 5, 37],
        [63, 31, 55, 23, 61, 29, 53, 21],
    ];
    let mut out = [[0f32; 8]; 8];
    let mut y = 0;
    while y < 8 {
        let mut x = 0;
        while x < 8 {
            out[y][x] = (raw[y][x] as f32 + 0.5) / 64.0;
            x += 1;
        }
        y += 1;
    }
    out
};

/// Dither threshold for pixel `(x, y)` of frame `t`.
#[inline(always)]
#[must_use]
pub fn threshold(d: Dither, x: usize, y: usize, t: usize) -> f32 {
    let base = BAYER8[y & 7][x & 7];
    match d {
        Dither::OrderedTemporal => {
            let v = base + t as f32 * 0.618_034;
            v - v.floor()
        }
        _ => base,
    }
}

/// Per-colour dither decision: the two bracketing palette entries and the
/// fraction of `hi` the true colour sits at.
#[derive(Clone, Copy)]
struct Bracket {
    lo: u8,
    hi: u8,
    alpha: f32,
}

/// Everything about a dithered mapping that does not depend on pixel position.
pub struct DitherPlan {
    per_bin: Vec<Bracket>,
}

impl DitherPlan {
    /// Solve the bracket for every distinct colour in the frame.
    ///
    /// Requires [`Hist::ensure_lab`].
    #[must_use]
    pub fn build(hist: &Hist, pal: &Palette) -> Self {
        let index = NnIndex::build(&pal.lab);
        let mut per_bin = Vec::with_capacity(hist.len());
        for (i, b) in hist.bins.iter().enumerate() {
            let cl = screeny_panel::color::lin(b.srgb);
            let i0 = index.nearest(hist.lab()[i]).0;
            // Overshoot past the nearest entry: whatever lies on the far side
            // brackets the true colour together with `i0`.
            let p0 = pal.lin[i0];
            let over = [
                (2.0 * cl[0] - p0[0]).clamp(0.0, 1.0),
                (2.0 * cl[1] - p0[1]).clamp(0.0, 1.0),
                (2.0 * cl[2] - p0[2]).clamp(0.0, 1.0),
            ];
            let i1 = index.nearest(oklab(over)).0;
            if i1 == i0 {
                per_bin.push(Bracket {
                    lo: i0 as u8,
                    hi: i0 as u8,
                    alpha: 0.0,
                });
                continue;
            }
            // Least-squares mix ratio along p0->p1, in linear light.
            let p1 = pal.lin[i1];
            let mut num = 0.0;
            let mut den = 0.0;
            for k in 0..3 {
                let dv = p1[k] - p0[k];
                num += (cl[k] - p0[k]) * dv;
                den += dv * dv;
            }
            let alpha = if den > 1e-9 {
                (num / den).clamp(0.0, 1.0)
            } else {
                0.0
            };
            per_bin.push(Bracket {
                lo: i0 as u8,
                hi: i1 as u8,
                alpha,
            });
        }
        DitherPlan { per_bin }
    }

    /// Expand to one palette index per pixel.
    pub fn apply(&self, hist: &Hist, d: Dither, t: usize, out: &mut [u8; NPIX]) {
        let bin_of = hist.bin_of();
        for y in 0..H {
            for x in 0..W {
                let p = y * W + x;
                let br = self.per_bin[bin_of[p] as usize];
                out[p] = if br.lo != br.hi && threshold(d, x, y, t) < br.alpha {
                    br.hi
                } else {
                    br.lo
                };
            }
        }
    }
}

/// Map a frame onto a palette, one index per pixel.
///
/// Requires [`Hist::ensure_lab`].
pub fn map(hist: &Hist, pal: &Palette, dither: Dither, t: usize, out: &mut [u8; NPIX]) {
    match dither {
        Dither::None => {
            let per_bin = nearest_per_bin(hist, pal);
            for (p, slot) in out.iter_mut().enumerate() {
                *slot = per_bin[hist.bin_of()[p] as usize];
            }
        }
        d => DitherPlan::build(hist, pal).apply(hist, d, t, out),
    }
}

/// Pack indices as nibbles, two per byte, high nibble first (spec 4.1, 4.3).
pub fn pack_nibbles(idx: &[u8; NPIX], out: &mut [u8]) {
    debug_assert_eq!(out.len(), NPIX / 2);
    for (p, &i) in idx.iter().enumerate() {
        if p & 1 == 0 {
            out[p >> 1] = (i & 0xf) << 4;
        } else {
            out[p >> 1] |= i & 0xf;
        }
    }
}

/// Pack bit `b` of each index as a bitplane, MSB first within each byte.
pub fn pack_plane(idx: &[u8; NPIX], b: u32, out: &mut [u8]) {
    debug_assert_eq!(out.len(), NPIX / 8);
    out.fill(0);
    for (p, &i) in idx.iter().enumerate() {
        if (i >> b) & 1 == 1 {
            out[p >> 3] |= 0x80 >> (p & 7);
        }
    }
}

/// Build a `PAL5` payload: 32 x RGB888, 1024 nibble bytes, 256 bit-4 bytes
/// (spec 4.1). Always exactly [`screeny_proto::dec::PAL5_LEN`] bytes.
#[must_use]
pub fn emit_pal5(pal: &Palette, idx: &[u8; NPIX]) -> Vec<u8> {
    let mut out = vec![0u8; screeny_proto::dec::PAL5_LEN];
    for (i, c) in pal.srgb.iter().take(32).enumerate() {
        out[i * 3..i * 3 + 3].copy_from_slice(c);
    }
    let (head, tail) = out.split_at_mut(96);
    let _ = head;
    let (nib, plane) = tail.split_at_mut(NPIX / 2);
    pack_nibbles(idx, nib);
    pack_plane(idx, 4, plane);
    out
}
