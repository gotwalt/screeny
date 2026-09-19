//! Panel-aware candidate scoring: mean Oklab dE between the source frame and
//! what the device will actually light up.
//!
//! Both sides go through [`Panel::emit`] before Oklab, because error the panel
//! cannot show is not error (card 002, `lab/src/metrics.rs`). Selection scores
//! against [`crate::panel::TEMPORAL`] - the panel we are going to have, not
//! the one we have today - so that a codec cannot win by hiding behind the
//! current driver's coarseness and then look worse when card 030 lands.
//!
//! Card 031's three changes, in order of how much they bought:
//!
//! 1. The **source** side is computed once per frame, not once per candidate.
//! 2. The **candidate** side goes through a direct-mapped colour memo: a
//!    decoded candidate has at most 1024 distinct colours and usually 32, and
//!    palettes barely move between frames.
//! 3. An optional **subsample stride**. `1` scores every pixel; the fast
//!    profile scores every fourth, with a per-row offset so the sample lattice
//!    does not line up with the 8x8 Bayer grid the dithered candidate uses.

use screeny_proto::{Rgb888Frame, H, NPIX, W};

use crate::color::{d2, oklab, LabCache};
use crate::panel::Panel;

/// Scores decoded candidates against a source frame.
pub struct Scorer {
    panel: Panel,
    lut: [f32; 256],
    src: Vec<[f32; 3]>,
    cache: LabCache,
    stride: usize,
    samples: Vec<u32>,
}

impl Scorer {
    /// A scorer for `panel`, sampling every `stride`-th pixel.
    #[must_use]
    pub fn new(panel: Panel, stride: usize) -> Self {
        let stride = stride.max(1);
        let mut s = Scorer {
            panel,
            lut: panel.emit_lut(),
            src: vec![[0f32; 3]; NPIX],
            cache: LabCache::new(13),
            stride,
            samples: Vec::new(),
        };
        s.build_samples();
        s
    }

    fn build_samples(&mut self) {
        self.samples.clear();
        if self.stride == 1 {
            return;
        }
        for y in 0..H {
            // Offset each row so the lattice is dispersed rather than a set
            // of vertical stripes through the Bayer matrix.
            let off = (y * 5) % self.stride;
            let mut x = off;
            while x < W {
                self.samples.push((y * W + x) as u32);
                x += self.stride;
            }
        }
    }

    /// The panel model being scored against.
    #[must_use]
    pub fn panel(&self) -> Panel {
        self.panel
    }

    /// How many pixels each score looks at.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        if self.stride == 1 {
            NPIX
        } else {
            self.samples.len()
        }
    }

    #[inline(always)]
    fn emit_lab(lut: &[f32; 256], c: [u8; 3]) -> [f32; 3] {
        oklab([
            lut[c[0] as usize],
            lut[c[1] as usize],
            lut[c[2] as usize],
        ])
    }

    /// Cache the source frame's emitted Oklab. Call once per frame, before
    /// scoring any candidate against it.
    pub fn prepare(&mut self, src: &Rgb888Frame) {
        let lut = &self.lut;
        let cache = &mut self.cache;
        if self.stride == 1 {
            for p in 0..NPIX {
                let c = [src[p * 3], src[p * 3 + 1], src[p * 3 + 2]];
                self.src[p] = cache.get(c, |c| Self::emit_lab(lut, c));
            }
        } else {
            for &p in &self.samples {
                let p = p as usize;
                let c = [src[p * 3], src[p * 3 + 1], src[p * 3 + 2]];
                self.src[p] = cache.get(c, |c| Self::emit_lab(lut, c));
            }
        }
    }

    /// Mean Oklab dE of a decoded candidate against the prepared source.
    pub fn score(&mut self, cand: &Rgb888Frame) -> f64 {
        let lut = &self.lut;
        let cache = &mut self.cache;
        let src = &self.src;
        let mut acc = 0f64;
        if self.stride == 1 {
            for p in 0..NPIX {
                let c = [cand[p * 3], cand[p * 3 + 1], cand[p * 3 + 2]];
                let d = cache.get(c, |c| Self::emit_lab(lut, c));
                acc += d2(src[p], d).sqrt() as f64;
            }
            acc / NPIX as f64
        } else {
            for &p in &self.samples {
                let p = p as usize;
                let c = [cand[p * 3], cand[p * 3 + 1], cand[p * 3 + 2]];
                let d = cache.get(c, |c| Self::emit_lab(lut, c));
                acc += d2(src[p], d).sqrt() as f64;
            }
            acc / self.samples.len() as f64
        }
    }
}

/// Mean panel-aware Oklab dE between two frames, computed straightforwardly.
///
/// This is `lab/src/metrics.rs::mean_de` with nothing cached and no
/// subsampling: the reference [`Scorer`] is checked against, and what the
/// `encode-stats` subcommand reports.
#[must_use]
pub fn mean_de(panel: &Panel, a: &Rgb888Frame, b: &Rgb888Frame) -> f64 {
    let mut s = 0f64;
    for p in 0..NPIX {
        let ca = [a[p * 3], a[p * 3 + 1], a[p * 3 + 2]];
        let cb = [b[p * 3], b[p * 3 + 1], b[p * 3 + 2]];
        s += d2(panel.emit_oklab(ca), panel.emit_oklab(cb)).sqrt() as f64;
    }
    s / NPIX as f64
}
