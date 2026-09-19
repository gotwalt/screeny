//! Ordered dither masks. Fixed patterns only: they stay put from frame to frame
//! and read as texture, where error diffusion would crawl (brief section 2.4).

use crate::frame::{H, N, W};
use crate::rng::Rng;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dither {
    None,
    Bayer4,
    Bayer8,
    #[default]
    BlueNoise,
}

impl Dither {
    /// Threshold for pixel (x, y), in -0.5..0.5: add it (times one quantisation
    /// step) before rounding.
    pub fn threshold(self, x: usize, y: usize) -> f32 {
        match self {
            Dither::None => 0.0,
            Dither::Bayer4 => (bayer(x, y, 2) as f32 + 0.5) / 16.0 - 0.5,
            Dither::Bayer8 => (bayer(x, y, 3) as f32 + 0.5) / 64.0 - 0.5,
            Dither::BlueNoise => blue_noise()[(y % H) * W + (x % W)],
        }
    }
}

/// Bayer matrix value of order 2^bits, by bit interleaving.
fn bayer(x: usize, y: usize, bits: u32) -> u32 {
    let (mut v, x, y) = (0, x as u32, y as u32);
    for i in 0..bits {
        let (xb, yb) = ((x >> i) & 1, (y >> i) & 1);
        v = (v << 2) | ((xb ^ yb) << 1) | yb;
    }
    v
}

/// A panel-sized, toroidal blue-noise mask, built once. Each pixel is ranked by
/// placing points one at a time in the largest remaining void (the second half
/// of void-and-cluster). Deterministic: the mask is the same on every run.
fn blue_noise() -> &'static [f32; N] {
    static MASK: OnceLock<[f32; N]> = OnceLock::new();
    MASK.get_or_init(|| {
        const R: i32 = 7;
        const SIGMA: f32 = 1.7;
        let mut kernel = [[0.0_f32; (2 * R + 1) as usize]; (2 * R + 1) as usize];
        for (j, row) in kernel.iter_mut().enumerate() {
            for (i, k) in row.iter_mut().enumerate() {
                let (dx, dy) = (i as f32 - R as f32, j as f32 - R as f32);
                *k = (-(dx * dx + dy * dy) / (2.0 * SIGMA * SIGMA)).exp();
            }
        }
        let mut rng = Rng::new(0x5c2e_e9f1);
        // Tiny random tilt so the first placements are not a regular lattice.
        let mut energy: Vec<f32> = (0..N).map(|_| rng.f32() * 1e-3).collect();
        let mut placed = [false; N];
        let mut mask = [0.0_f32; N];
        for rank in 0..N {
            let mut best = usize::MAX;
            for i in 0..N {
                if !placed[i] && (best == usize::MAX || energy[i] < energy[best]) {
                    best = i;
                }
            }
            placed[best] = true;
            mask[best] = (rank as f32 + 0.5) / N as f32 - 0.5;
            let (bx, by) = ((best % W) as i32, (best / W) as i32);
            for dy in -R..=R {
                for dx in -R..=R {
                    let x = (bx + dx).rem_euclid(W as i32) as usize;
                    let y = (by + dy).rem_euclid(H as i32) as usize;
                    energy[y * W + x] += kernel[(dy + R) as usize][(dx + R) as usize];
                }
            }
        }
        mask
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_are_balanced() {
        for d in [Dither::Bayer4, Dither::Bayer8, Dither::BlueNoise] {
            let mut sum = 0.0;
            for y in 0..H {
                for x in 0..W {
                    let t = d.threshold(x, y);
                    assert!((-0.5..0.5).contains(&t));
                    sum += t;
                }
            }
            assert!((sum / N as f32).abs() < 0.01, "{d:?} mean {}", sum / N as f32);
        }
    }

    #[test]
    fn bayer4_is_a_permutation() {
        let mut seen = [false; 16];
        for y in 0..4 {
            for x in 0..4 {
                seen[bayer(x, y, 2) as usize] = true;
            }
        }
        assert!(seen.iter().all(|s| *s));
    }
}
