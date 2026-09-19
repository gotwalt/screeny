//! Palette design and palette mapping.
//!
//! Median cut in Oklab for the initial split, then weighted Lloyd (k-means)
//! refinement. Oklab rather than sRGB because the whole point of the palette
//! is to put its few entries where the eye will miss them least.
//!
//! `quantette` (MIT/Apache, Wu + k-means, also Oklab) would do this job too;
//! it is hand-rolled here only so the temporal-seeding experiment below is
//! possible and so the lab has no quantiser dependency.

use crate::color::{oklab_inv, oklab_srgb8, lin_to_srgb8};
use crate::frame::{Frame, NPIX};
use std::collections::HashMap;

pub struct Hist {
    /// Distinct colours: (sRGB888, Oklab, count).
    pub bins: Vec<([u8; 3], [f32; 3], f32)>,
}

pub fn histogram(f: &Frame) -> Hist {
    let mut m: HashMap<[u8; 3], f32> = HashMap::new();
    for p in 0..NPIX {
        *m.entry(f.at(p)).or_insert(0.0) += 1.0;
    }
    let mut bins: Vec<_> = m
        .into_iter()
        .map(|(c, n)| (c, oklab_srgb8(c), n))
        .collect();
    // Deterministic order so runs are reproducible.
    bins.sort_by(|a, b| a.0.cmp(&b.0));
    Hist { bins }
}

fn centroid(bins: &[([u8; 3], [f32; 3], f32)]) -> [f32; 3] {
    let mut acc = [0f64; 3];
    let mut w = 0f64;
    for (_, lab, n) in bins {
        for k in 0..3 {
            acc[k] += lab[k] as f64 * *n as f64;
        }
        w += *n as f64;
    }
    if w == 0.0 {
        return [0.0; 3];
    }
    [
        (acc[0] / w) as f32,
        (acc[1] / w) as f32,
        (acc[2] / w) as f32,
    ]
}

/// Median cut over Oklab boxes, splitting the box with the largest
/// count-weighted spread along its longest axis.
fn median_cut(hist: &Hist, k: usize) -> Vec<[f32; 3]> {
    let mut boxes: Vec<Vec<([u8; 3], [f32; 3], f32)>> = vec![hist.bins.clone()];
    while boxes.len() < k {
        // Pick the box worth splitting.
        let mut best = None;
        let mut best_score = 0f32;
        for (i, b) in boxes.iter().enumerate() {
            if b.len() < 2 {
                continue;
            }
            let (axis, span) = longest_axis(b);
            let n: f32 = b.iter().map(|x| x.2).sum();
            let score = span * n.sqrt();
            if score > best_score {
                best_score = score;
                best = Some((i, axis));
            }
        }
        let Some((i, axis)) = best else { break };
        let mut b = boxes.swap_remove(i);
        b.sort_by(|p, q| p.1[axis].partial_cmp(&q.1[axis]).unwrap());
        // Split at the weighted median.
        let total: f32 = b.iter().map(|x| x.2).sum();
        let mut acc = 0.0;
        let mut cut = 1;
        for (j, x) in b.iter().enumerate() {
            acc += x.2;
            if acc >= total / 2.0 {
                cut = (j + 1).min(b.len() - 1).max(1);
                break;
            }
        }
        let right = b.split_off(cut);
        boxes.push(b);
        boxes.push(right);
    }
    boxes.iter().map(|b| centroid(b)).collect()
}

fn longest_axis(b: &[([u8; 3], [f32; 3], f32)]) -> (usize, f32) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for (_, lab, _) in b {
        for k in 0..3 {
            lo[k] = lo[k].min(lab[k]);
            hi[k] = hi[k].max(lab[k]);
        }
    }
    // Oklab a/b have a smaller natural range than L; weight them up so chroma
    // detail is not always sacrificed first.
    let w = [1.0f32, 1.0, 1.0];
    let mut best = 0;
    let mut span = -1.0;
    for k in 0..3 {
        let s = (hi[k] - lo[k]) * w[k];
        if s > span {
            span = s;
            best = k;
        }
    }
    (best, span)
}

fn lloyd(hist: &Hist, mut cents: Vec<[f32; 3]>, iters: usize) -> Vec<[f32; 3]> {
    let k = cents.len();
    for _ in 0..iters {
        let mut acc = vec![[0f64; 3]; k];
        let mut wsum = vec![0f64; k];
        for (_, lab, n) in &hist.bins {
            let mut bi = 0;
            let mut bd = f32::MAX;
            for (i, c) in cents.iter().enumerate() {
                let d = crate::color::d2(*lab, *c);
                if d < bd {
                    bd = d;
                    bi = i;
                }
            }
            for j in 0..3 {
                acc[bi][j] += lab[j] as f64 * *n as f64;
            }
            wsum[bi] += *n as f64;
        }
        for i in 0..k {
            if wsum[i] > 0.0 {
                cents[i] = [
                    (acc[i][0] / wsum[i]) as f32,
                    (acc[i][1] / wsum[i]) as f32,
                    (acc[i][2] / wsum[i]) as f32,
                ];
            }
        }
    }
    cents
}

/// Total weighted squared Oklab error of a palette on this histogram.
pub fn palette_cost(hist: &Hist, pal: &[[f32; 3]]) -> f64 {
    let mut e = 0f64;
    for (_, lab, n) in &hist.bins {
        let mut bd = f32::MAX;
        for c in pal {
            bd = bd.min(crate::color::d2(*lab, *c));
        }
        e += bd as f64 * *n as f64;
    }
    e
}

fn lab_to_srgb8(lab: [f32; 3]) -> [u8; 3] {
    let l = oklab_inv(lab);
    [
        lin_to_srgb8(l[0]),
        lin_to_srgb8(l[1]),
        lin_to_srgb8(l[2]),
    ]
}

pub struct Palette {
    pub srgb: Vec<[u8; 3]>,
    pub lab: Vec<[f32; 3]>,
    pub lin: Vec<[f32; 3]>,
}

impl Palette {
    pub fn from_srgb(srgb: Vec<[u8; 3]>) -> Self {
        let lab = srgb.iter().map(|c| oklab_srgb8(*c)).collect();
        let lin = srgb.iter().map(|c| super::lin(*c)).collect();
        Palette { srgb, lab, lin }
    }

    #[inline]
    pub fn nearest(&self, lab: [f32; 3]) -> usize {
        let mut bi = 0;
        let mut bd = f32::MAX;
        for (i, c) in self.lab.iter().enumerate() {
            let d = crate::color::d2(lab, *c);
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        bi
    }
}

/// Build a `k`-entry palette for `f`.
///
/// If `seed` is supplied it is used as the starting point for Lloyd instead of
/// a fresh median cut, which keeps palettes similar frame to frame (less
/// flicker). The fresh palette is computed anyway and wins if the seeded one
/// is more than 15% worse, so a scene cut cannot leave a stale palette latched.
pub fn build(f: &Frame, k: usize, seed: Option<&[[u8; 3]]>) -> Palette {
    let hist = histogram(f);
    if hist.bins.len() <= k {
        let mut srgb: Vec<[u8; 3]> = hist.bins.iter().map(|b| b.0).collect();
        while srgb.len() < k {
            srgb.push([0, 0, 0]);
        }
        return Palette::from_srgb(srgb);
    }
    // Lloyd is O(bins * k); large palettes are already close after median cut,
    // so spend the iterations where they change the answer.
    let iters = if k <= 32 {
        10
    } else if k <= 64 {
        6
    } else {
        3
    };
    let fresh = lloyd(&hist, median_cut(&hist, k), iters);
    let chosen = match seed {
        Some(s) if s.len() == k => {
            let seeded = lloyd(&hist, s.iter().map(|c| oklab_srgb8(*c)).collect(), iters);
            let (cs, cf) = (palette_cost(&hist, &seeded), palette_cost(&hist, &fresh));
            if cs <= cf * 1.15 {
                seeded
            } else {
                fresh
            }
        }
        _ => fresh,
    };
    Palette::from_srgb(chosen.iter().map(|c| lab_to_srgb8(*c)).collect())
}

/// A fixed palette: a lattice in sRGB space plus a grey ramp. Deliberately
/// content-independent -- it is the "no per-frame palette" baseline.
pub fn fixed(k: usize) -> Palette {
    let srgb: Vec<[u8; 3]> = match k {
        16 => vec![
            [0, 0, 0],
            [85, 85, 85],
            [170, 170, 170],
            [255, 255, 255],
            [170, 0, 0],
            [255, 85, 85],
            [0, 170, 0],
            [85, 255, 85],
            [0, 0, 170],
            [85, 85, 255],
            [170, 170, 0],
            [255, 255, 85],
            [0, 170, 170],
            [85, 255, 255],
            [170, 0, 170],
            [255, 85, 255],
        ],
        32 => {
            // 3x3x3 sRGB lattice (27) + 5 extra greys to fill the ramp.
            let lv = [0u8, 128, 255];
            let mut v = Vec::new();
            for &r in &lv {
                for &g in &lv {
                    for &b in &lv {
                        v.push([r, g, b]);
                    }
                }
            }
            for &g in &[32u8, 64, 96, 176, 216] {
                v.push([g, g, g]);
            }
            v
        }
        _ => panic!("no fixed palette of size {k}"),
    };
    assert_eq!(srgb.len(), k);
    Palette::from_srgb(srgb)
}
