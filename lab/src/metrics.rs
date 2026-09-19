//! Quality metrics.
//!
//! Three families, because none of them alone answers the question:
//!
//! * **PSNR** on raw sRGB codes: comparable with the literature, and blind to
//!   everything that matters here.
//! * **Oklab dE, panel-aware**: both source and decode are pushed through
//!   `Panel::emit` (gamma LUT, then quantise to the panel's BCM depth) before
//!   being converted to Oklab. Error the panel cannot show does not count, and
//!   error in the dark end counts as much as the eye says it should. This is
//!   the number the recommendation is based on. Reported x1000.
//! * **SSIM** on gamma-space luma: structure, which dE misses -- a dither
//!   pattern and a blur can have the same dE and look nothing alike.
//!
//! Plus two temporal metrics, because the codec runs at 30 fps and flicker is
//! far more objectionable than a static error of the same magnitude:
//!
//! * **flicker**: excess frame-to-frame Oklab-L change introduced by the codec.
//! * **dE(t-avg 4)**: error of the 4-frame *average* emitted light, which is
//!   roughly what the eye integrates. Temporal dithering trades the first
//!   against the second.

use crate::frame::{Frame, H, NPIX, W};
use crate::panel::Panel;

#[derive(Default, Clone, Copy)]
pub struct Acc {
    pub n: f64,
    pub bytes_sum: f64,
    pub bytes_max: usize,
    pub over: usize,
    pub mse: f64,
    pub de_sum: f64,
    pub de_all: f64,
    pub ssim_sum: f64,
    pub exact_sum: f64,
    pub blur_sum: f64,
    pub flicker_sum: f64,
    pub flicker_n: f64,
    pub tavg_sum: f64,
    pub tavg_n: f64,
    pub lossless: usize,
}

pub struct Summary {
    pub bytes_mean: f64,
    pub bytes_max: usize,
    pub over_pct: f64,
    pub psnr: f64,
    pub de_mean: f64,
    pub de_p95: f64,
    pub ssim: f64,
    pub exact_pct: f64,
    pub de_blur: f64,
    pub flicker: f64,
    pub tavg: f64,
    pub lossless_pct: f64,
}

/// Collects per-frame numbers plus the p95 tail, which needs all samples.
pub struct Collector {
    pub acc: Acc,
    de_samples: Vec<f32>,
    panel: Panel,
}

impl Collector {
    pub fn new(panel: Panel) -> Self {
        Collector {
            acc: Acc::default(),
            de_samples: Vec::new(),
            panel,
        }
    }

    pub fn add_frame(&mut self, src: &Frame, dec: &Frame, bytes: usize, budget: usize) {
        let a = &mut self.acc;
        a.n += 1.0;
        a.bytes_sum += bytes as f64;
        a.bytes_max = a.bytes_max.max(bytes);
        if bytes > budget {
            a.over += 1;
        }

        let mut mse = 0f64;
        let mut exact = 0usize;
        for i in 0..NPIX * 3 {
            let d = src.px[i] as f64 - dec.px[i] as f64;
            mse += d * d;
        }
        for p in 0..NPIX {
            if src.at(p) == dec.at(p) {
                exact += 1;
            }
        }
        a.mse += mse / (NPIX * 3) as f64;
        a.exact_sum += exact as f64 / NPIX as f64;
        if exact == NPIX {
            a.lossless += 1;
        }

        let mut de = 0f64;
        for p in 0..NPIX {
            let s = self.panel.emit_oklab(src.at(p));
            let d = self.panel.emit_oklab(dec.at(p));
            let e = crate::color::d2(s, d).sqrt();
            de += e as f64;
            self.de_samples.push(e);
        }
        a.de_sum += de / NPIX as f64;
        a.ssim_sum += ssim_luma(src, dec) as f64;
        a.blur_sum += blur_de(&self.panel, src, dec);
    }

    /// Excess temporal variation: how much more the decoded sequence flickers
    /// than the source does.
    pub fn add_temporal(&mut self, src: &[Frame], dec: &[Frame]) {
        let a = &mut self.acc;
        // Precompute emitted Oklab L per frame; the naive version recomputes
        // four cube roots per pixel per frame pair and dominates the run.
        let lofs = |fs: &[Frame]| -> Vec<Vec<f32>> {
            fs.iter()
                .map(|f| (0..NPIX).map(|p| self.panel.emit_oklab(f.at(p))[0]).collect())
                .collect()
        };
        let ls = lofs(src);
        let ld = lofs(dec);
        for t in 1..src.len() {
            let mut e = 0f64;
            for p in 0..NPIX {
                let ds = (ls[t][p] - ls[t - 1][p]).abs();
                let dd = (ld[t][p] - ld[t - 1][p]).abs();
                e += (dd - ds) as f64;
            }
            a.flicker_sum += e / NPIX as f64;
            a.flicker_n += 1.0;
        }

        // 4-frame average of emitted linear light, then dE.
        const N: usize = 4;
        for t0 in (0..src.len().saturating_sub(N - 1)).step_by(N) {
            let mut e = 0f64;
            for p in 0..NPIX {
                let mut ms = [0f32; 3];
                let mut md = [0f32; 3];
                for t in t0..t0 + N {
                    let s = self.panel.emit(src[t].at(p));
                    let d = self.panel.emit(dec[t].at(p));
                    for k in 0..3 {
                        ms[k] += s[k] / N as f32;
                        md[k] += d[k] / N as f32;
                    }
                }
                e += crate::color::d2(crate::color::oklab(ms), crate::color::oklab(md)).sqrt()
                    as f64;
            }
            a.tavg_sum += e / NPIX as f64;
            a.tavg_n += 1.0;
        }
    }

    pub fn finish(mut self) -> Summary {
        let a = self.acc;
        self.de_samples
            .sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        let p95 = if self.de_samples.is_empty() {
            0.0
        } else {
            self.de_samples[(self.de_samples.len() as f64 * 0.95) as usize
                % self.de_samples.len()] as f64
        };
        let mse = a.mse / a.n;
        Summary {
            bytes_mean: a.bytes_sum / a.n,
            bytes_max: a.bytes_max,
            over_pct: a.over as f64 / a.n * 100.0,
            psnr: if mse <= 1e-12 {
                99.0
            } else {
                10.0 * (255.0f64 * 255.0 / mse).log10()
            },
            de_mean: a.de_sum / a.n * 1000.0,
            de_p95: p95 * 1000.0,
            ssim: a.ssim_sum / a.n,
            exact_pct: a.exact_sum / a.n * 100.0,
            de_blur: a.blur_sum / a.n * 1000.0,
            flicker: if a.flicker_n > 0.0 {
                a.flicker_sum / a.flicker_n * 1000.0
            } else {
                0.0
            },
            tavg: if a.tavg_n > 0.0 {
                a.tavg_sum / a.tavg_n * 1000.0
            } else {
                0.0
            },
            lossless_pct: a.lossless as f64 / a.n * 100.0,
        }
    }
}

/// Mean panel-aware Oklab dE over a whole sequence, under an arbitrary panel
/// model. Used to show how the ranking moves with panel depth.
pub fn seq_de(panel: &Panel, src: &[Frame], dec: &[Frame]) -> f64 {
    let mut s = 0f64;
    for (a, b) in src.iter().zip(dec) {
        s += mean_de(panel, a, b);
    }
    s / src.len() as f64 * 1000.0
}

/// Mean panel-aware Oklab dE between two frames. Used by the hybrid encoder to
/// choose a mode, so it must be cheap enough to call a handful of times per
/// frame -- it is, at 2048 pixels.
pub fn mean_de(panel: &Panel, a: &Frame, b: &Frame) -> f64 {
    let mut s = 0f64;
    for p in 0..NPIX {
        s += crate::color::d2(panel.emit_oklab(a.at(p)), panel.emit_oklab(b.at(p))).sqrt() as f64;
    }
    s / NPIX as f64
}

/// dE after a mild spatial low-pass of the *emitted light*.
///
/// This is the "viewed from across the room" metric. Adjacent LEDs bleed into
/// each other optically and the eye's contrast sensitivity falls off at the
/// panel's pixel pitch, so a codec whose error is high-frequency (dither) is
/// punished less here than one whose error is low-frequency (banding, block
/// flatness). Separable [1 2 1]/4 -- deliberately conservative: at a normal
/// Tidbyt viewing distance a 1.9 mm pixel still subtends ~6 arcmin, which the
/// eye resolves easily, so a stronger blur would be flattering nonsense.
pub fn blur_de(panel: &Panel, a: &Frame, b: &Frame) -> f64 {
    let lp = |f: &Frame| -> Vec<[f32; 3]> {
        let e: Vec<[f32; 3]> = (0..NPIX).map(|p| panel.emit(f.at(p))).collect();
        let mut h = vec![[0f32; 3]; NPIX];
        for y in 0..H {
            for x in 0..W {
                let l = e[y * W + x.saturating_sub(1)];
                let c = e[y * W + x];
                let r = e[y * W + (x + 1).min(W - 1)];
                for k in 0..3 {
                    h[y * W + x][k] = 0.25 * l[k] + 0.5 * c[k] + 0.25 * r[k];
                }
            }
        }
        let mut v = vec![[0f32; 3]; NPIX];
        for y in 0..H {
            for x in 0..W {
                let u = h[y.saturating_sub(1) * W + x];
                let c = h[y * W + x];
                let d = h[(y + 1).min(H - 1) * W + x];
                for k in 0..3 {
                    v[y * W + x][k] = 0.25 * u[k] + 0.5 * c[k] + 0.25 * d[k];
                }
            }
        }
        v
    };
    let (fa, fb) = (lp(a), lp(b));
    let mut s = 0f64;
    for p in 0..NPIX {
        s += crate::color::d2(crate::color::oklab(fa[p]), crate::color::oklab(fb[p])).sqrt() as f64;
    }
    s / NPIX as f64
}

/// SSIM over 7x7 uniform windows on Rec.709 luma in gamma space.
pub fn ssim_luma(a: &Frame, b: &Frame) -> f32 {
    const WIN: usize = 7;
    let c1 = (0.01 * 255.0f32).powi(2);
    let c2 = (0.03 * 255.0f32).powi(2);
    let la: Vec<f32> = (0..NPIX).map(|p| crate::color::luma_gamma(a.at(p))).collect();
    let lb: Vec<f32> = (0..NPIX).map(|p| crate::color::luma_gamma(b.at(p))).collect();
    let n = (WIN * WIN) as f32;
    let mut sum = 0f32;
    let mut cnt = 0f32;
    for y in 0..=(H - WIN) {
        for x in 0..=(W - WIN) {
            let (mut ma, mut mb) = (0f32, 0f32);
            for j in 0..WIN {
                for i in 0..WIN {
                    let p = (y + j) * W + x + i;
                    ma += la[p];
                    mb += lb[p];
                }
            }
            ma /= n;
            mb /= n;
            let (mut va, mut vb, mut cov) = (0f32, 0f32, 0f32);
            for j in 0..WIN {
                for i in 0..WIN {
                    let p = (y + j) * W + x + i;
                    let da = la[p] - ma;
                    let db = lb[p] - mb;
                    va += da * da;
                    vb += db * db;
                    cov += da * db;
                }
            }
            va /= n - 1.0;
            vb /= n - 1.0;
            cov /= n - 1.0;
            let s = ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            sum += s;
            cnt += 1.0;
        }
    }
    sum / cnt
}
