//! `BC1_DUAL` (spec 4.5): 4x4 blocks, 10 bytes each, with a per-block flag
//! choosing between RGB565 endpoints with 8 levels and RGB888 endpoints with
//! 4. Lifted from `lab/src/enc/block.rs` (card 002), keeping only the codec
//! that made it onto the wire.
//!
//! Endpoints are fitted along the block's principal colour axis in sRGB -
//! which is where the decoder interpolates - then refined twice by least
//! squares against the current index assignment. The *final* index assignment
//! is done in Oklab, which is free (the levels are already fixed) and strictly
//! better.
//!
//! Card 031's change: the source pixels' Oklab comes from the frame histogram
//! instead of being recomputed. The lab did 4096 `oklab_srgb8` calls per frame
//! here - three cube roots each - for colours it had already converted.

use screeny_proto::dec::{lerp8, x5, x6, BC1_DUAL_LEN, W4, W8};
use screeny_proto::{Rgb888Frame, H, W};

use crate::color::d2;

use super::hist::Hist;

const NB: usize = (W / 4) * (H / 4); // 128 blocks

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ends {
    Rgb565,
    Rgb888,
}

fn q565(c: [f32; 3]) -> [u8; 3] {
    let r = (c[0] / 255.0 * 31.0).round().clamp(0.0, 31.0) as u32;
    let g = (c[1] / 255.0 * 63.0).round().clamp(0.0, 63.0) as u32;
    let b = (c[2] / 255.0 * 31.0).round().clamp(0.0, 31.0) as u32;
    [x5(r), x6(g), x5(b)]
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
        Ends::Rgb888 => q888(c),
    }
}

fn pack565(c: [u8; 3]) -> [u8; 2] {
    let v = (((c[0] as u32) >> 3) << 11) | (((c[1] as u32) >> 2) << 5) | ((c[2] as u32) >> 3);
    [(v & 0xff) as u8, (v >> 8) as u8]
}

fn weights(levels: usize) -> &'static [u32] {
    match levels {
        4 => &W4,
        _ => &W8,
    }
}

/// The reconstruction levels the decoder will build for these endpoints.
fn ramp(a: [u8; 3], b: [u8; 3], levels: usize, out: &mut [[u8; 3]; 8]) {
    let w = weights(levels);
    for k in 0..levels {
        out[k] = [
            lerp8(a[0], b[0], w[k]),
            lerp8(a[1], b[1], w[k]),
            lerp8(a[2], b[2], w[k]),
        ];
    }
}

/// Dominant colour axis of the block, by power iteration on the covariance.
fn principal_axis(px: &[[f32; 3]; 16], mean: [f32; 3]) -> [f32; 3] {
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

/// Fit a two-endpoint ramp to a block. Returns the quantised endpoints, the
/// per-pixel index assignment, and the resulting Oklab error.
fn fit_ramp(
    px: &[[f32; 3]; 16],
    lab: &[[f32; 3]; 16],
    ends: Ends,
    levels: usize,
) -> ([u8; 3], [u8; 3], [u8; 16], f64) {
    let mut mean = [0f32; 3];
    for p in px {
        for k in 0..3 {
            mean[k] += p[k] / 16.0;
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
    let mut idx = [0u8; 16];
    let mut tab = [[0u8; 3]; 8];

    for round in 0..3 {
        // Assign against the reconstruction the decoder will actually build.
        ramp(qa, qb, levels, &mut tab);
        for (j, p) in px.iter().enumerate() {
            let mut bi = 0;
            let mut bd = f32::MAX;
            for (i, c) in tab[..levels].iter().enumerate() {
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
        // Least-squares refit of (a, b) given the index assignment.
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

    // Final assignment in Oklab, and the error that comes with it.
    ramp(qa, qb, levels, &mut tab);
    let mut tabl = [[0f32; 3]; 8];
    for k in 0..levels {
        tabl[k] = crate::color::oklab_srgb8(tab[k]);
    }
    let mut err = 0f64;
    for (j, pl) in lab.iter().enumerate() {
        let mut bi = 0;
        let mut bd = f32::MAX;
        for (i, c) in tabl[..levels].iter().enumerate() {
            let d = d2(*pl, *c);
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        idx[j] = bi as u8;
        err += bd.sqrt() as f64;
    }
    (qa, qb, idx, err)
}

fn push_idx2(out: &mut Vec<u8>, idx: &[u8; 16]) {
    for chunk in idx.chunks(4) {
        let mut b = 0u8;
        for (j, &v) in chunk.iter().enumerate() {
            b |= (v & 3) << (6 - 2 * j);
        }
        out.push(b);
    }
}

fn push_idx3_planes(out: &mut Vec<u8>, idx: &[u8; 16]) {
    for bit in 0..3 {
        for byte in 0..2 {
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

/// Encode a frame as `BC1_DUAL`. Always exactly
/// [`screeny_proto::dec::BC1_DUAL_LEN`] bytes, whatever the content, which is
/// what makes it one of the two codecs a sender can always fall back to.
///
/// Requires [`Hist::ensure_lab`].
#[must_use]
pub fn encode(f: &Rgb888Frame, hist: &Hist) -> Vec<u8> {
    let mut flags = [0u8; NB / 8];
    let mut body = Vec::with_capacity(NB * 10);
    let bin_of = hist.bin_of();
    let labs = hist.lab();
    let mut px = [[0f32; 3]; 16];
    let mut lab = [[0f32; 3]; 16];

    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let n = by * (W / 4) + bx;
            for j in 0..16 {
                let p = (by * 4 + (j >> 2)) * W + bx * 4 + (j & 3);
                px[j] = [
                    f[p * 3] as f32,
                    f[p * 3 + 1] as f32,
                    f[p * 3 + 2] as f32,
                ];
                lab[j] = labs[bin_of[p] as usize];
            }
            let (a5, b5, i5, e5) = fit_ramp(&px, &lab, Ends::Rgb565, 8);
            let (a8, b8, i8, e8) = fit_ramp(&px, &lab, Ends::Rgb888, 4);
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

    let mut out = Vec::with_capacity(BC1_DUAL_LEN);
    out.extend_from_slice(&flags);
    out.extend_from_slice(&body);
    debug_assert_eq!(out.len(), BC1_DUAL_LEN);
    out
}
