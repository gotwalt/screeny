//! Block encoders for the BC1 and colour-cell families.
//!
//! Two shapes of block:
//!
//! * **ramp** (BC1-like): two endpoints and 2^n evenly spaced interpolants.
//!   Endpoints are fitted along the block's principal colour axis, then
//!   refined twice by least squares against the current index assignment.
//! * **free** (colour-cell): k independent colours from a k-means over the
//!   block. Better where a block's colours are not collinear -- glyph edges,
//!   UI chrome -- and worse on smooth ramps, which is exactly the split we
//!   want to measure.
//!
//! Fitting happens in sRGB space because that is where the decoder
//! interpolates. The *final* index assignment is done in Oklab, which is free
//! (the levels are already fixed) and strictly better.

use super::{Codec, EncCtx};
use crate::color::{d2, oklab_srgb8};
use crate::dec::{lerp8, mode, W4, W8};
use crate::frame::{Frame, H, W};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ends {
    Rgb565,
    Rgb888,
    Rgb444,
}

pub struct BlockCodec {
    mode: u8,
    bw: usize,
    bh: usize,
    ends: Ends,
    levels: usize,
    free: bool,
    bpb: usize,
    name: &'static str,
    note: &'static str,
}

impl BlockCodec {
    pub fn bc1() -> Self {
        Self { mode: mode::BC1, bw: 4, bh: 4, ends: Ends::Rgb565, levels: 4, free: false, bpb: 8,
               name: "bc1", note: "4x4, 2xRGB565 endpoints, 2-bit indices, 1025 B" }
    }
    pub fn bc1_i3() -> Self {
        Self { mode: mode::BC1_I3, bw: 4, bh: 4, ends: Ends::Rgb565, levels: 8, free: false, bpb: 10,
               name: "bc1-i3", note: "4x4, 2xRGB565 endpoints, 3-bit indices, 1281 B" }
    }
    pub fn bc1_e888() -> Self {
        Self { mode: mode::BC1_E888, bw: 4, bh: 4, ends: Ends::Rgb888, levels: 4, free: false, bpb: 10,
               name: "bc1-e888", note: "4x4, 2xRGB888 endpoints, 2-bit indices, 1281 B" }
    }
    pub fn blk42() -> Self {
        Self { mode: mode::BLK42, bw: 4, bh: 2, ends: Ends::Rgb444, levels: 4, free: false, bpb: 5,
               name: "blk42", note: "4x2, 2xRGB444 endpoints, 2-bit indices, 1281 B" }
    }
    pub fn blk84_i3() -> Self {
        Self { mode: mode::BLK84_I3, bw: 8, bh: 4, ends: Ends::Rgb565, levels: 8, free: false, bpb: 16,
               name: "blk84-i3", note: "8x4, 2xRGB565 endpoints, 3-bit indices, 1025 B" }
    }
    pub fn cc4() -> Self {
        Self { mode: mode::CC4, bw: 4, bh: 4, ends: Ends::Rgb444, levels: 4, free: true, bpb: 10,
               name: "cc4", note: "4x4, 4 free RGB444 colours, 2-bit indices, 1281 B" }
    }
    pub fn cc2_42() -> Self {
        Self { mode: mode::CC2_42, bw: 4, bh: 2, ends: Ends::Rgb565, levels: 2, free: true, bpb: 5,
               name: "cc2-42", note: "4x2, 2 free RGB565 colours, 1-bit indices, 1281 B" }
    }
    pub fn cc2_44() -> Self {
        Self { mode: mode::CC2_44, bw: 4, bh: 4, ends: Ends::Rgb888, levels: 2, free: true, bpb: 8,
               name: "cc2-44", note: "4x4, 2 free RGB888 colours, 1-bit indices, 1025 B" }
    }

    pub fn payload_len(&self) -> usize {
        1 + (W / self.bw) * (H / self.bh) * self.bpb
    }
}

/// Per-block choice between (RGB565 endpoints, 8 levels) and (RGB888
/// endpoints, 4 levels). Both cost exactly 10 bytes; a 16-byte flag plane says
/// which. 1 + 16 + 1280 = **1297 B**.
pub struct BlockDual;

fn block_err(px: &[[f32; 3]], tab: &[[u8; 3]], idx: &[u8]) -> f64 {
    let mut e = 0f64;
    for (j, p) in px.iter().enumerate() {
        let a = oklab_srgb8(q888(*p));
        let b = oklab_srgb8(tab[idx[j] as usize]);
        e += d2(a, b).sqrt() as f64;
    }
    e
}

impl Codec for BlockDual {
    fn name(&self) -> &'static str {
        "bc1-dual"
    }
    fn note(&self) -> &'static str {
        "4x4, per-block choice of RGB565+3-bit or RGB888+2-bit, 1297 B"
    }
    fn encode(&self, f: &Frame, _budget: usize, _ctx: &mut EncCtx) -> Vec<u8> {
        const NB: usize = (W / 4) * (H / 4);
        let mut flags = vec![0u8; NB / 8];
        let mut body = Vec::with_capacity(NB * 10);
        let mut px = vec![[0f32; 3]; 16];
        for by in 0..H / 4 {
            for bx in 0..W / 4 {
                let n = by * (W / 4) + bx;
                for j in 0..16 {
                    let c = f.get(bx * 4 + (j & 3), by * 4 + (j >> 2));
                    px[j] = [c[0] as f32, c[1] as f32, c[2] as f32];
                }
                let (a5, b5, i5) = fit_ramp(&px, Ends::Rgb565, 8);
                let (a8, b8, i8) = fit_ramp(&px, Ends::Rgb888, 4);
                let e5 = block_err(&px, &ramp(a5, b5, 8), &i5);
                let e8 = block_err(&px, &ramp(a8, b8, 4), &i8);
                if e8 < e5 {
                    flags[n >> 3] |= 0x80 >> (n & 7);
                    body.extend_from_slice(&a8);
                    body.extend_from_slice(&b8);
                    push_idx2(&mut body, &i8);
                } else {
                    body.extend_from_slice(&pack565(a5));
                    body.extend_from_slice(&pack565(b5));
                    push_idx3_planes(&mut body, &i5);
                }
            }
        }
        let mut out = Vec::with_capacity(1 + NB / 8 + NB * 10);
        out.push(mode::BC1_DUAL);
        out.extend_from_slice(&flags);
        out.extend_from_slice(&body);
        out
    }
}

fn weights(levels: usize) -> &'static [u32] {
    match levels {
        2 => &[0, 256],
        4 => &W4,
        8 => &W8,
        _ => unreachable!(),
    }
}

// --- endpoint quantisation -------------------------------------------------

fn q565(c: [f32; 3]) -> [u8; 3] {
    let r = (c[0] / 255.0 * 31.0).round().clamp(0.0, 31.0) as u32;
    let g = (c[1] / 255.0 * 63.0).round().clamp(0.0, 63.0) as u32;
    let b = (c[2] / 255.0 * 31.0).round().clamp(0.0, 31.0) as u32;
    [
        crate::dec::x5(r),
        crate::dec::x6(g),
        crate::dec::x5(b),
    ]
}
fn q444(c: [f32; 3]) -> [u8; 3] {
    let f = |v: f32| crate::dec::x4((v / 255.0 * 15.0).round().clamp(0.0, 15.0) as u32);
    [f(c[0]), f(c[1]), f(c[2])]
}
fn q888(c: [f32; 3]) -> [u8; 3] {
    [
        c[0].round().clamp(0.0, 255.0) as u8,
        c[1].round().clamp(0.0, 255.0) as u8,
        c[2].round().clamp(0.0, 255.0) as u8,
    ]
}

fn quantise(ends: Ends, c: [f32; 3]) -> [u8; 3] {
    match ends {
        Ends::Rgb565 => q565(c),
        Ends::Rgb444 => q444(c),
        Ends::Rgb888 => q888(c),
    }
}

fn pack565(c: [u8; 3]) -> [u8; 2] {
    let v = (((c[0] as u32) >> 3) << 11) | (((c[1] as u32) >> 2) << 5) | ((c[2] as u32) >> 3);
    [(v & 0xff) as u8, (v >> 8) as u8]
}
fn pack444_pair(a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    let n = |v: u8| (v >> 4) as u8;
    [
        (n(a[0]) << 4) | n(a[1]),
        (n(a[2]) << 4) | n(b[0]),
        (n(b[1]) << 4) | n(b[2]),
    ]
}

// --- block fitting ---------------------------------------------------------

fn ramp(a: [u8; 3], b: [u8; 3], levels: usize) -> Vec<[u8; 3]> {
    let w = weights(levels);
    w.iter()
        .map(|&wk| {
            [
                lerp8(a[0], b[0], wk),
                lerp8(a[1], b[1], wk),
                lerp8(a[2], b[2], wk),
            ]
        })
        .collect()
}

/// Dominant colour axis of the block, by power iteration on the covariance.
fn principal_axis(px: &[[f32; 3]], mean: [f32; 3]) -> [f32; 3] {
    let mut cov = [[0f32; 3]; 3];
    for p in px {
        let d = [p[0] - mean[0], p[1] - mean[1], p[2] - mean[2]];
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let mut v = [1f32, 1.0, 1.0];
    for _ in 0..12 {
        let mut n = [0f32; 3];
        for i in 0..3 {
            for j in 0..3 {
                n[i] += cov[i][j] * v[j];
            }
        }
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-12 {
            return [1.0, 1.0, 1.0];
        }
        v = [n[0] / len, n[1] / len, n[2] / len];
    }
    v
}

/// Fit a two-endpoint ramp to a block. Returns the quantised endpoints and the
/// per-pixel index assignment.
fn fit_ramp(px: &[[f32; 3]], ends: Ends, levels: usize) -> ([u8; 3], [u8; 3], Vec<u8>) {
    let n = px.len() as f32;
    let mut mean = [0f32; 3];
    for p in px {
        for k in 0..3 {
            mean[k] += p[k] / n;
        }
    }
    let axis = principal_axis(px, mean);
    let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
    for p in px {
        let t = (0..3).map(|k| (p[k] - mean[k]) * axis[k]).sum::<f32>();
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    let mut a = [0f32; 3];
    let mut b = [0f32; 3];
    for k in 0..3 {
        a[k] = (mean[k] + axis[k] * tmin).clamp(0.0, 255.0);
        b[k] = (mean[k] + axis[k] * tmax).clamp(0.0, 255.0);
    }

    let w = weights(levels);
    let mut qa = quantise(ends, a);
    let mut qb = quantise(ends, b);
    let mut idx = vec![0u8; px.len()];

    for round in 0..3 {
        // Assign against the reconstruction the decoder will actually build.
        let tab = ramp(qa, qb, levels);
        for (j, p) in px.iter().enumerate() {
            let mut bi = 0;
            let mut bd = f32::MAX;
            for (i, c) in tab.iter().enumerate() {
                let d = (0..3)
                    .map(|k| {
                        let e = p[k] - c[k] as f32;
                        e * e
                    })
                    .sum::<f32>();
                if d < bd {
                    bd = d;
                    bi = i;
                }
            }
            idx[j] = bi as u8;
        }
        if round == 2 {
            break;
        }
        // Least-squares refit of (a,b) given the index assignment.
        let (mut saa, mut sab, mut sbb) = (0f32, 0f32, 0f32);
        let mut sap = [0f32; 3];
        let mut sbp = [0f32; 3];
        for (j, p) in px.iter().enumerate() {
            let t = w[idx[j] as usize] as f32 / 256.0;
            let s = 1.0 - t;
            saa += s * s;
            sab += s * t;
            sbb += t * t;
            for k in 0..3 {
                sap[k] += s * p[k];
                sbp[k] += t * p[k];
            }
        }
        let det = saa * sbb - sab * sab;
        if det.abs() > 1e-6 {
            for k in 0..3 {
                a[k] = ((sbb * sap[k] - sab * sbp[k]) / det).clamp(0.0, 255.0);
                b[k] = ((saa * sbp[k] - sab * sap[k]) / det).clamp(0.0, 255.0);
            }
            qa = quantise(ends, a);
            qb = quantise(ends, b);
        }
    }

    // Final assignment in Oklab.
    let tab = ramp(qa, qb, levels);
    let tabl: Vec<[f32; 3]> = tab.iter().map(|c| oklab_srgb8(*c)).collect();
    for (j, p) in px.iter().enumerate() {
        let pl = oklab_srgb8(q888(*p));
        let mut bi = 0;
        let mut bd = f32::MAX;
        for (i, c) in tabl.iter().enumerate() {
            let d = d2(pl, *c);
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        idx[j] = bi as u8;
    }
    (qa, qb, idx)
}

/// Fit `k` free colours to a block with a small k-means in Oklab.
fn fit_free(px: &[[f32; 3]], ends: Ends, k: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    let labs: Vec<[f32; 3]> = px.iter().map(|p| oklab_srgb8(q888(*p))).collect();
    // Seed along the principal axis so the initialisation is deterministic and
    // already close.
    let n = px.len() as f32;
    let mut mean = [0f32; 3];
    for p in px {
        for c in 0..3 {
            mean[c] += p[c] / n;
        }
    }
    let axis = principal_axis(px, mean);
    let mut order: Vec<usize> = (0..px.len()).collect();
    let proj: Vec<f32> = px
        .iter()
        .map(|p| (0..3).map(|c| (p[c] - mean[c]) * axis[c]).sum::<f32>())
        .collect();
    order.sort_by(|&i, &j| proj[i].partial_cmp(&proj[j]).unwrap());
    let mut cents: Vec<[f32; 3]> = (0..k)
        .map(|i| labs[order[(i * (px.len() - 1)) / (k - 1).max(1)]])
        .collect();

    let mut idx = vec![0u8; px.len()];
    for _ in 0..10 {
        for (j, l) in labs.iter().enumerate() {
            let mut bi = 0;
            let mut bd = f32::MAX;
            for (i, c) in cents.iter().enumerate() {
                let d = d2(*l, *c);
                if d < bd {
                    bd = d;
                    bi = i;
                }
            }
            idx[j] = bi as u8;
        }
        // Recompute centroids in *sRGB* -- the colours we will actually store
        // are sRGB, and averaging there matches what quantisation expects.
        let mut acc = vec![[0f32; 3]; k];
        let mut cnt = vec![0f32; k];
        for (j, p) in px.iter().enumerate() {
            let i = idx[j] as usize;
            for c in 0..3 {
                acc[i][c] += p[c];
            }
            cnt[i] += 1.0;
        }
        for i in 0..k {
            if cnt[i] > 0.0 {
                let m = [acc[i][0] / cnt[i], acc[i][1] / cnt[i], acc[i][2] / cnt[i]];
                cents[i] = oklab_srgb8(q888(m));
            }
        }
    }
    // Materialise and quantise the k colours, then re-assign against them.
    let mut acc = vec![[0f32; 3]; k];
    let mut cnt = vec![0f32; k];
    for (j, p) in px.iter().enumerate() {
        let i = idx[j] as usize;
        for c in 0..3 {
            acc[i][c] += p[c];
        }
        cnt[i] += 1.0;
    }
    let cols: Vec<[u8; 3]> = (0..k)
        .map(|i| {
            if cnt[i] > 0.0 {
                quantise(ends, [acc[i][0] / cnt[i], acc[i][1] / cnt[i], acc[i][2] / cnt[i]])
            } else {
                quantise(ends, mean)
            }
        })
        .collect();
    let cl: Vec<[f32; 3]> = cols.iter().map(|c| oklab_srgb8(*c)).collect();
    for (j, l) in labs.iter().enumerate() {
        let mut bi = 0;
        let mut bd = f32::MAX;
        for (i, c) in cl.iter().enumerate() {
            let d = d2(*l, *c);
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        idx[j] = bi as u8;
    }
    (cols, idx)
}

// --- index packing (must mirror `crate::dec::block`) -----------------------

fn push_idx2(out: &mut Vec<u8>, idx: &[u8]) {
    for chunk in idx.chunks(4) {
        let mut b = 0u8;
        for (j, &v) in chunk.iter().enumerate() {
            b |= (v & 3) << (6 - 2 * j);
        }
        out.push(b);
    }
}

fn push_idx3_planes(out: &mut Vec<u8>, idx: &[u8]) {
    let stride = idx.len() / 8;
    for bit in 0..3 {
        for byte in 0..stride {
            let mut b = 0u8;
            for k in 0..8 {
                if (idx[byte * 8 + k] >> bit) & 1 == 1 {
                    b |= 0x80 >> k;
                }
            }
            out.push(b);
        }
    }
}

fn push_idx1(out: &mut Vec<u8>, idx: &[u8]) {
    for chunk in idx.chunks(8) {
        let mut b = 0u8;
        for (j, &v) in chunk.iter().enumerate() {
            if v & 1 == 1 {
                b |= 0x80 >> j;
            }
        }
        out.push(b);
    }
}

impl Codec for BlockCodec {
    fn name(&self) -> &'static str {
        self.name
    }
    fn note(&self) -> &'static str {
        self.note
    }
    fn encode(&self, f: &Frame, _budget: usize, _ctx: &mut EncCtx) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.payload_len());
        out.push(self.mode);
        let npx = self.bw * self.bh;
        let mut px = vec![[0f32; 3]; npx];
        for by in 0..H / self.bh {
            for bx in 0..W / self.bw {
                for j in 0..npx {
                    let c = f.get(bx * self.bw + j % self.bw, by * self.bh + j / self.bw);
                    px[j] = [c[0] as f32, c[1] as f32, c[2] as f32];
                }
                if self.free {
                    let (cols, idx) = fit_free(&px, self.ends, self.levels);
                    match (self.ends, self.levels) {
                        (Ends::Rgb565, 2) => {
                            out.extend_from_slice(&pack565(cols[0]));
                            out.extend_from_slice(&pack565(cols[1]));
                            push_idx1(&mut out, &idx);
                        }
                        (Ends::Rgb888, 2) => {
                            out.extend_from_slice(&cols[0]);
                            out.extend_from_slice(&cols[1]);
                            push_idx1(&mut out, &idx);
                        }
                        (Ends::Rgb444, 4) => {
                            out.extend_from_slice(&pack444_pair(cols[0], cols[1]));
                            out.extend_from_slice(&pack444_pair(cols[2], cols[3]));
                            push_idx2(&mut out, &idx);
                        }
                        _ => unreachable!(),
                    }
                } else {
                    let (a, b, idx) = fit_ramp(&px, self.ends, self.levels);
                    match self.ends {
                        Ends::Rgb565 => {
                            out.extend_from_slice(&pack565(a));
                            out.extend_from_slice(&pack565(b));
                        }
                        Ends::Rgb888 => {
                            out.extend_from_slice(&a);
                            out.extend_from_slice(&b);
                        }
                        Ends::Rgb444 => out.extend_from_slice(&pack444_pair(a, b)),
                    }
                    if self.levels == 8 {
                        push_idx3_planes(&mut out, &idx);
                    } else {
                        push_idx2(&mut out, &idx);
                    }
                }
            }
        }
        debug_assert_eq!(out.len(), self.payload_len());
        out
    }
}
