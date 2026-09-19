//! YCoCg-R encoders with chroma subsampling.
//!
//! Luma is ordered-dithered to its target depth (floor(v + bayer) is unbiased,
//! so the dithered plane has the same mean as the original). Chroma is box
//! averaged over the subsample block *in YCoCg space* and then quantised.

use super::pal::{pack_nibbles, pack_plane};
use super::{Codec, EncCtx, BAYER8};
use crate::dec::mode;
use crate::frame::{Frame, H, NPIX, W};

pub struct YCoCgCodec {
    mode: u8,
    ybits: u32,
    cbits: u32,
    sub: usize,
    name: &'static str,
    note: &'static str,
}

impl YCoCgCodec {
    pub fn c420() -> Self {
        Self {
            mode: mode::YCOCG_420,
            ybits: 4,
            cbits: 3,
            sub: 2,
            name: "ycocg-420",
            note: "Y 4bpp full-res, Co/Cg 3bpp at 2x2, 1409 B",
        }
    }
    pub fn c410() -> Self {
        Self {
            mode: mode::YCOCG_410,
            ybits: 5,
            cbits: 5,
            sub: 4,
            name: "ycocg-410",
            note: "Y 5bpp full-res, Co/Cg 5bpp at 4x4, 1441 B",
        }
    }
}

#[inline]
pub fn forward(c: [u8; 3]) -> (i32, i32, i32) {
    let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
    let co = r - b;
    let t = b + (co >> 1);
    let cg = g - t;
    let y = t + (cg >> 1);
    (y, co, cg)
}

impl Codec for YCoCgCodec {
    fn name(&self) -> &'static str {
        self.name
    }
    fn note(&self) -> &'static str {
        self.note
    }
    fn encode(&self, f: &Frame, _budget: usize, _ctx: &mut EncCtx) -> Vec<u8> {
        let mut yy = vec![0i32; NPIX];
        let mut co = vec![0i32; NPIX];
        let mut cg = vec![0i32; NPIX];
        for p in 0..NPIX {
            let (a, b, c) = forward(f.at(p));
            yy[p] = a;
            co[p] = b;
            cg[p] = c;
        }

        // --- luma, ordered-dithered to ybits ---
        let ymax = ((1u32 << self.ybits) - 1) as f32;
        let mut yq = vec![0u8; NPIX];
        for y in 0..H {
            for x in 0..W {
                let p = y * W + x;
                let v = yy[p].clamp(0, 255) as f32 / 255.0 * ymax + BAYER8[y & 7][x & 7];
                yq[p] = v.floor().clamp(0.0, ymax) as u8;
            }
        }

        // --- chroma, box-averaged then quantised ---
        let cw = W / self.sub;
        let ch = H / self.sub;
        let shift = 9 - self.cbits;
        let step = (1i32) << shift;
        let half = 1i32 << (self.cbits - 1);
        let cmax = (1i32 << self.cbits) - 1;
        let mut coq = vec![0u8; cw * ch];
        let mut cgq = vec![0u8; cw * ch];
        for by in 0..ch {
            for bx in 0..cw {
                let (mut so, mut sg) = (0i32, 0i32);
                for dy in 0..self.sub {
                    for dx in 0..self.sub {
                        let p = (by * self.sub + dy) * W + bx * self.sub + dx;
                        so += co[p];
                        sg += cg[p];
                    }
                }
                let n = (self.sub * self.sub) as f32;
                let q = |v: f32| {
                    (((v / step as f32).round() as i32) + half).clamp(0, cmax) as u8
                };
                coq[by * cw + bx] = q(so as f32 / n);
                cgq[by * cw + bx] = q(sg as f32 / n);
            }
        }

        let mut out = Vec::new();
        out.push(self.mode);
        out.extend_from_slice(&pack_nibbles(&yq));
        if self.ybits == 5 {
            out.extend_from_slice(&pack_plane(&yq, 4));
        }
        for k in 0..self.cbits {
            out.extend_from_slice(&pack_plane(&coq, k));
        }
        for k in 0..self.cbits {
            out.extend_from_slice(&pack_plane(&cgq, k));
        }
        out
    }
}
