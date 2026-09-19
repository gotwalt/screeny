//! Palette encoders: fixed and adaptive, 4 and 5 bits per pixel.
//!
//! Rate accounting (payload including the mode byte):
//! * 4 bpp: 1 + 16*3 + 1024 = **1073 B**
//! * 5 bpp: 1 + 32*3 + 1024 + 256 = **1377 B**
//!
//! 5 bpp is the ceiling for a flat indexed frame: 6 bpp would need 1536 B of
//! indices alone.
//!
//! ## Dithering in linear light
//!
//! Spatial dithering works because the eye integrates *emitted light* over
//! neighbouring LEDs. So the mixing ratio between two palette entries has to
//! be solved in linear light, not in sRGB, or the dithered result is
//! systematically too dark. Selection of *which* two entries still happens in
//! Oklab, where "nearest" means what we want it to mean.

use super::{lin, threshold, Codec, Dither, EncCtx};
use crate::color::oklab;
use crate::dec::mode;
use crate::frame::{Frame, H, NPIX, W};

use super::quant::{self, Palette};

pub struct PalCodec {
    k: usize,
    dither: Dither,
    adaptive: bool,
    temporal_pal: bool,
    name: &'static str,
    note: &'static str,
}

impl PalCodec {
    pub fn fixed16() -> Self {
        PalCodec {
            k: 16,
            dither: Dither::Ordered,
            adaptive: false,
            temporal_pal: false,
            name: "pal4-fixed",
            note: "fixed 16-colour lattice, ordered dither, 1073 B",
        }
    }
    pub fn fixed32() -> Self {
        PalCodec {
            k: 32,
            dither: Dither::Ordered,
            adaptive: false,
            temporal_pal: false,
            name: "pal5-fixed",
            note: "fixed 32-colour lattice, ordered dither, 1377 B",
        }
    }
    pub fn adaptive(k: usize, dither: Dither, temporal_pal: bool) -> Self {
        let name = match (k, dither, temporal_pal) {
            (16, Dither::None, _) => "pal4-adapt",
            (16, Dither::Ordered, _) => "pal4-adapt-ord",
            (32, Dither::None, _) => "pal5-adapt",
            (32, Dither::Ordered, false) => "pal5-adapt-ord",
            (32, Dither::Ordered, true) => "pal5-adapt-ord-tp",
            (32, Dither::ErrorDiffusion, _) => "pal5-adapt-fs",
            (32, Dither::OrderedTemporal, _) => "pal5-adapt-tdith",
            _ => "pal-adapt",
        };
        let note = match (k, dither, temporal_pal) {
            (16, Dither::None, _) => "per-frame 16-colour palette, no dither, 1073 B",
            (16, Dither::Ordered, _) => "per-frame 16-colour palette, Bayer 8x8, 1073 B",
            (32, Dither::None, _) => "per-frame 32-colour palette, no dither, 1377 B",
            (32, Dither::Ordered, false) => "per-frame 32-colour palette, Bayer 8x8, 1377 B",
            (32, Dither::Ordered, true) => "as pal5-adapt-ord but palette seeded from previous frame",
            (32, Dither::ErrorDiffusion, _) => "per-frame 32-colour palette, Floyd-Steinberg, 1377 B",
            (32, Dither::OrderedTemporal, _) => "32-colour palette, Bayer with per-frame phase (temporal dither)",
            _ => "",
        };
        PalCodec {
            k,
            dither,
            adaptive: true,
            temporal_pal,
            name,
            note,
        }
    }
}

/// Map a frame onto a palette, returning one index per pixel.
pub fn map(pal: &Palette, f: &Frame, dither: Dither, t: usize) -> Vec<u8> {
    match dither {
        Dither::ErrorDiffusion => map_fs(pal, f),
        Dither::None => (0..NPIX)
            .map(|p| pal.nearest(crate::color::oklab_srgb8(f.at(p))) as u8)
            .collect(),
        d => map_ordered(pal, f, d, t),
    }
}

fn map_ordered(pal: &Palette, f: &Frame, d: Dither, t: usize) -> Vec<u8> {
    let mut out = vec![0u8; NPIX];
    for y in 0..H {
        for x in 0..W {
            let p = y * W + x;
            let c = f.at(p);
            let cl = lin(c);
            let i0 = pal.nearest(oklab(cl));
            // Overshoot past the nearest entry and find what lies on the far
            // side; those two bracket the true colour.
            let p0 = pal.lin[i0];
            let over = [
                (2.0 * cl[0] - p0[0]).clamp(0.0, 1.0),
                (2.0 * cl[1] - p0[1]).clamp(0.0, 1.0),
                (2.0 * cl[2] - p0[2]).clamp(0.0, 1.0),
            ];
            let i1 = pal.nearest(oklab(over));
            if i1 == i0 {
                out[p] = i0 as u8;
                continue;
            }
            let p1 = pal.lin[i1];
            // Least-squares mix ratio along p0->p1, in linear light.
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
            out[p] = if threshold(d, x, y, t) < alpha {
                i1 as u8
            } else {
                i0 as u8
            };
        }
    }
    out
}

fn map_fs(pal: &Palette, f: &Frame) -> Vec<u8> {
    let mut err = vec![[0f32; 3]; NPIX];
    let mut out = vec![0u8; NPIX];
    for y in 0..H {
        let serp = y % 2 == 1;
        for xi in 0..W {
            let x = if serp { W - 1 - xi } else { xi };
            let p = y * W + x;
            let c = lin(f.at(p));
            let want = [
                (c[0] + err[p][0]).clamp(0.0, 1.0),
                (c[1] + err[p][1]).clamp(0.0, 1.0),
                (c[2] + err[p][2]).clamp(0.0, 1.0),
            ];
            let i = pal.nearest(oklab(want));
            out[p] = i as u8;
            let got = pal.lin[i];
            let e = [want[0] - got[0], want[1] - got[1], want[2] - got[2]];
            let dir: i32 = if serp { -1 } else { 1 };
            let mut spread = |dx: i32, dy: i32, w: f32| {
                let nx = x as i32 + dx * dir;
                let ny = y as i32 + dy;
                if nx >= 0 && nx < W as i32 && ny >= 0 && ny < H as i32 {
                    let q = ny as usize * W + nx as usize;
                    for k in 0..3 {
                        err[q][k] += e[k] * w;
                    }
                }
            };
            spread(1, 0, 7.0 / 16.0);
            spread(-1, 1, 3.0 / 16.0);
            spread(0, 1, 5.0 / 16.0);
            spread(1, 1, 1.0 / 16.0);
        }
    }
    out
}

/// Pack indices as nibbles (2 px/byte), high nibble first.
pub fn pack_nibbles(idx: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; idx.len().div_ceil(2)];
    for (p, &i) in idx.iter().enumerate() {
        if p & 1 == 0 {
            v[p >> 1] |= (i & 0xf) << 4;
        } else {
            v[p >> 1] |= i & 0xf;
        }
    }
    let _ = NPIX;
    v
}

/// Pack bit `b` of each index as a bitplane, MSB-first within each byte.
pub fn pack_plane(idx: &[u8], b: u32) -> Vec<u8> {
    let mut v = vec![0u8; idx.len().div_ceil(8)];
    for (p, &i) in idx.iter().enumerate() {
        if (i >> b) & 1 == 1 {
            v[p >> 3] |= 0x80 >> (p & 7);
        }
    }
    v
}

pub fn emit_pal4(pal: &Palette, idx: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1073);
    out.push(mode::PAL4);
    for c in pal.srgb.iter().take(16) {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&pack_nibbles(idx));
    out
}

pub fn emit_pal5(pal: &Palette, idx: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1377);
    out.push(mode::PAL5);
    for c in pal.srgb.iter().take(32) {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&pack_nibbles(idx));
    out.extend_from_slice(&pack_plane(idx, 4));
    out
}

impl Codec for PalCodec {
    fn name(&self) -> &'static str {
        self.name
    }
    fn note(&self) -> &'static str {
        self.note
    }
    fn encode(&self, f: &Frame, _budget: usize, ctx: &mut EncCtx) -> Vec<u8> {
        let pal = if self.adaptive {
            let seed = if self.temporal_pal {
                ctx.prev_pal.as_deref()
            } else {
                None
            };
            quant::build(f, self.k, seed)
        } else {
            quant::fixed(self.k)
        };
        let idx = map(&pal, f, self.dither, ctx.frame_idx);
        if self.temporal_pal {
            ctx.prev_pal = Some(pal.srgb.clone());
        }
        if self.k == 16 {
            emit_pal4(&pal, &idx)
        } else {
            emit_pal5(&pal, &idx)
        }
    }
}
