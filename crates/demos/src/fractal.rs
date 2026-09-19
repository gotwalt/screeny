//! Endless Mandelbrot zoom.
//!
//! **Endlessness.** A single zoom has a precision wall: at 64-bit the image
//! turns to blocks somewhere past 1e-14. So the piece is a *tour*. It zooms
//! toward one curated boundary point at a constant exponential rate for
//! `leg_secs`, from scale 1.6 down to 2e-12 (still ~100x clear of the f64
//! wall, which is where the sample spacing, `scale / (64 * ss)`, meets the
//! ulp of the coordinate), and then cross-dissolves into the next leg, which
//! is already zooming at the same rate when the dissolve starts. Motion
//! continuity carries the cut. The tour is a fixed list traversed from a
//! seeded offset, so it runs for hours (one lap is `legs * leg_secs`) and
//! never degenerates: every target is checked by a test for interior fraction
//! and colour count at full depth.
//!
//! **Look.** Continuous (smooth) escape time, mapped through a designed cyclic
//! OKLCH ramp that never goes below L 0.42 (the panel's dark end is missing -
//! brief section 2.1), snapped to panel levels, rotating slowly. Interior is
//! true black, used as a shape. Every sample is coloured and then averaged in
//! **linear light** over an ss x ss grid, which is what stops the bands
//! shimmering as they flow.

use crate::color::{lin_to_srgb8_3, oklch_to_lin};
use crate::dither::bayer;
use crate::frame::{Frame, Indexed, Piece, H, NPIX, W};
use crate::panel::{Panel, NOMINAL};
use std::time::Duration;

/// A curated point on the boundary of the set, with the palette it is toured
/// with. Coordinates are the centre of the deepest view.
#[derive(Clone, Copy)]
pub struct Target {
    pub name: &'static str,
    pub cx: f64,
    pub cy: f64,
    pub palette: usize,
}

pub const TARGETS: &[Target] = &[
    Target {
        name: "seahorse",
        cx: -0.743_643_887_037_151,
        cy: 0.131_825_904_205_330,
        palette: 0,
    },
    Target {
        name: "needle",
        cx: -1.749_705_768_080_503,
        cy: 0.000_029_831_647_555,
        palette: 1,
    },
    Target {
        name: "julia-island",
        cx: -0.774_680_610_626_903_9,
        cy: -0.137_416_885_603_786_7,
        palette: 2,
    },
    Target {
        name: "north-spiral",
        cx: -0.101_096_363_845_62,
        cy: 0.956_286_510_809_14,
        palette: 3,
    },
    Target {
        name: "elephant",
        cx: 0.274_546_022_101_212_9,
        cy: 0.005_819_581_324_461_8,
        palette: 4,
    },
    Target {
        name: "west-tendril",
        cx: -1.256_885_663_319_430_4,
        cy: 0.378_945_843_823_574_2,
        palette: 5,
    },
];

const BAILOUT: f64 = 256.0;
const START_SCALE: f64 = 1.6;
const END_SCALE: f64 = 2.0e-12;

/// Palette keyframes in OKLCH (L, chroma, hue in turns), interpolated
/// cyclically. Designed so the whole ramp lives in the mid-to-bright range and
/// each one is two hue families rather than a rainbow.
const PALETTES: &[&[(f32, f32, f32)]] = &[
    // 0 ember: deep red -> orange -> pale gold -> magenta
    &[
        (0.46, 0.15, 0.055),
        (0.74, 0.17, 0.105),
        (0.96, 0.07, 0.175),
        (0.62, 0.14, 0.925),
    ],
    // 1 ice: blue -> cyan -> white -> violet
    &[
        (0.48, 0.14, 0.700),
        (0.78, 0.12, 0.560),
        (0.97, 0.03, 0.560),
        (0.60, 0.15, 0.790),
    ],
    // 2 acid: green -> chartreuse -> cream -> teal
    &[
        (0.52, 0.17, 0.390),
        (0.80, 0.18, 0.320),
        (0.96, 0.06, 0.270),
        (0.62, 0.13, 0.480),
    ],
    // 3 sunset: magenta -> rose -> peach -> indigo
    &[
        (0.50, 0.16, 0.940),
        (0.74, 0.16, 0.030),
        (0.95, 0.07, 0.130),
        (0.58, 0.16, 0.820),
    ],
    // 4 copper: brown-gold -> amber -> white-gold -> olive
    &[
        (0.50, 0.12, 0.140),
        (0.76, 0.15, 0.170),
        (0.97, 0.05, 0.220),
        (0.63, 0.11, 0.300),
    ],
    // 5 lagoon: teal -> aqua -> pale -> blue
    &[
        (0.50, 0.13, 0.520),
        (0.79, 0.13, 0.480),
        (0.96, 0.04, 0.430),
        (0.60, 0.14, 0.650),
    ],
];

fn lerp_hue(a: f32, b: f32, t: f32) -> f32 {
    let mut d = b - a;
    while d > 0.5 {
        d -= 1.0;
    }
    while d < -0.5 {
        d += 1.0;
    }
    a + d * t
}

/// Sample a cyclic OKLCH keyframe ramp into `n` linear-light colours, each
/// snapped to something the panel can actually emit.
pub fn build_ramp(keys: &[(f32, f32, f32)], n: usize, panel: &Panel) -> Vec<[f32; 3]> {
    let k = keys.len();
    (0..n)
        .map(|i| {
            let u = i as f32 / n as f32 * k as f32;
            let i0 = u.floor() as usize % k;
            let i1 = (i0 + 1) % k;
            let t = u - u.floor();
            // Smoothstep between keyframes: a linear ramp through few keys has
            // visible creases where the slope changes.
            let t = t * t * (3.0 - 2.0 * t);
            let (l0, c0, h0) = keys[i0];
            let (l1, c1, h1) = keys[i1];
            let lin = oklch_to_lin(
                l0 + (l1 - l0) * t,
                c0 + (c1 - c0) * t,
                lerp_hue(h0, h1, t),
            );
            // Snap through the panel model so the ramp has no steps the panel
            // cannot show and no per-channel rounding casts.
            let snapped = panel.emit(lin_to_srgb8_3(lin));
            snapped
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Leg {
    target: Target,
    /// Seconds since this leg started.
    age: f64,
}

impl Leg {
    fn scale(&self, rate: f64) -> f64 {
        START_SCALE * (-self.age * rate).exp()
    }
}

pub struct FractalZoom {
    pub seed: u64,
    /// Supersampling factor per axis.
    pub ss: usize,
    pub leg_secs: f64,
    pub fade_secs: f64,
    /// Palette cycles per unit of the mapped escape-time coordinate.
    pub band_period: f32,
    /// Palette rotations per second.
    pub rot_rate: f32,
    pub panel: Panel,
    ramps: Vec<Vec<[f32; 3]>>,
    ramps_idx: Vec<Vec<[u8; 3]>>,
    threads: usize,
    /// Scratch: one linear colour per panel pixel, per live leg.
    buf_a: Vec<[f32; 3]>,
    buf_b: Vec<[f32; 3]>,
}

impl Default for FractalZoom {
    fn default() -> Self {
        Self::new(1)
    }
}

/// Palette entries for the full-colour path. More entries than the wire's
/// 256-colour ceiling is pointless; 192 keeps the ramp smooth after the
/// supersample average without pushing the frame's distinct-colour count up.
const RAMP_N: usize = 192;
/// The indexed path owns 31 ramp entries plus black.
const RAMP_IDX_N: usize = 31;

impl FractalZoom {
    pub fn new(seed: u64) -> Self {
        let panel = NOMINAL;
        let ramps = PALETTES
            .iter()
            .map(|k| build_ramp(k, RAMP_N, &panel))
            .collect();
        let ramps_idx = PALETTES
            .iter()
            .map(|k| {
                build_ramp(k, RAMP_IDX_N, &panel)
                    .into_iter()
                    .map(lin_to_srgb8_3)
                    .collect()
            })
            .collect();
        FractalZoom {
            seed,
            ss: 4,
            leg_secs: 105.0,
            fade_secs: 1.6,
            band_period: 26.0,
            rot_rate: 0.035,
            panel,
            ramps,
            ramps_idx,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            buf_a: vec![[0.0; 3]; NPIX],
            buf_b: vec![[0.0; 3]; NPIX],
        }
    }

    fn rate(&self) -> f64 {
        (START_SCALE / END_SCALE).ln() / self.leg_secs
    }

    fn target(&self, leg_no: i64) -> Target {
        let n = TARGETS.len() as i64;
        let i = (leg_no + (self.seed as i64 % n) + n * 4) % n;
        TARGETS[i as usize]
    }

    /// The legs alive at time `t` and the weight of the newer one.
    fn legs(&self, t: f64) -> (Leg, Option<(Leg, f32)>) {
        let n = (t / self.leg_secs).floor() as i64;
        let age = t - n as f64 * self.leg_secs;
        let cur = Leg {
            target: self.target(n),
            age,
        };
        if n > 0 && age < self.fade_secs {
            let prev = Leg {
                target: self.target(n - 1),
                age: age + self.leg_secs,
            };
            let w = (age / self.fade_secs) as f32;
            let w = w * w * (3.0 - 2.0 * w);
            (prev, Some((cur, w)))
        } else {
            (cur, None)
        }
    }

    fn max_iter(&self, scale: f64) -> u32 {
        // Enough to keep the boundary resolved as the view shrinks, with a
        // floor that keeps the shallow end from looking blobby.
        let depth = (START_SCALE / scale).log2().max(0.0);
        (220.0 + 62.0 * depth) as u32
    }

    /// Render one leg into 64x32 linear-light colours.
    fn render_leg(&self, leg: &Leg, t: f64, out: &mut [[f32; 3]]) {
        let scale = leg.scale(self.rate());
        let max_iter = self.max_iter(scale);
        let ramp = &self.ramps[leg.target.palette % self.ramps.len()];
        let ss = self.ss;
        let inv_ss2 = 1.0 / (ss * ss) as f32;
        // The view is 2:1, so `scale` is the half-width; half-height is half of it.
        let half_w = scale;
        let half_h = scale * H as f64 / W as f64;
        let x0 = leg.target.cx - half_w;
        let y0 = leg.target.cy - half_h;
        let dx = 2.0 * half_w / (W * ss) as f64;
        let dy = 2.0 * half_h / (H * ss) as f64;
        let rot = self.rot_rate as f64 * t;
        let inv_period = 1.0 / self.band_period as f64;
        let rows_per = H.div_ceil(self.threads.max(1));
        std::thread::scope(|s| {
            for (band, chunk) in out.chunks_mut(rows_per * W).enumerate() {
                let y_base = band * rows_per;
                s.spawn(move || {
                    for (row, px_row) in chunk.chunks_mut(W).enumerate() {
                        let y = y_base + row;
                        for (x, px) in px_row.iter_mut().enumerate() {
                            let mut acc = [0f32; 3];
                            for j in 0..ss {
                                let ci = y0 + ((y * ss + j) as f64 + 0.5) * dy;
                                for i in 0..ss {
                                    let cr = x0 + ((x * ss + i) as f64 + 0.5) * dx;
                                    if let Some(nu) = escape(cr, ci, max_iter) {
                                        let u = (nu * inv_period + rot).rem_euclid(1.0);
                                        let e = ramp[(u * ramp.len() as f64) as usize
                                            % ramp.len()];
                                        acc[0] += e[0];
                                        acc[1] += e[1];
                                        acc[2] += e[2];
                                    }
                                }
                            }
                            *px = [acc[0] * inv_ss2, acc[1] * inv_ss2, acc[2] * inv_ss2];
                        }
                    }
                });
            }
        });
    }

    /// The frame as linear-light colours, legs already mixed.
    fn render_lin(&mut self, t: Duration, out: &mut [[f32; 3]]) {
        let ts = t.as_secs_f64();
        let (a, b) = self.legs(ts);
        let mut buf_a = std::mem::take(&mut self.buf_a);
        self.render_leg(&a, ts, &mut buf_a);
        match b {
            None => out.copy_from_slice(&buf_a),
            Some((leg_b, w)) => {
                let mut buf_b = std::mem::take(&mut self.buf_b);
                self.render_leg(&leg_b, ts, &mut buf_b);
                for i in 0..NPIX {
                    for k in 0..3 {
                        out[i][k] = buf_a[i][k] * (1.0 - w) + buf_b[i][k] * w;
                    }
                }
                self.buf_b = buf_b;
            }
        }
        self.buf_a = buf_a;
    }

    /// Which palette is on screen at `t` (the newer leg's, during a dissolve).
    fn active_palette(&self, t: f64) -> usize {
        let (a, b) = self.legs(t);
        match b {
            Some((leg, w)) if w > 0.5 => leg.target.palette,
            _ => a.target.palette,
        }
    }

    pub fn leg_name(&self, t: f64) -> &'static str {
        let (a, b) = self.legs(t);
        match b {
            Some((leg, w)) if w > 0.5 => leg.target.name,
            _ => a.target.name,
        }
    }

    pub fn leg_scale(&self, t: f64) -> f64 {
        let (a, b) = self.legs(t);
        match b {
            Some((leg, w)) if w > 0.5 => leg.scale(self.rate()),
            _ => a.scale(self.rate()),
        }
    }
}

impl Piece for FractalZoom {
    fn name(&self) -> &'static str {
        "fractal"
    }

    fn render(&mut self, t: Duration, out: &mut Frame) {
        let mut lin = vec![[0f32; 3]; NPIX];
        self.render_lin(t, &mut lin);
        for i in 0..NPIX {
            let c = lin_to_srgb8_3(lin[i]);
            out.px[i * 3] = c[0];
            out.px[i * 3 + 1] = c[1];
            out.px[i * 3 + 2] = c[2];
        }
    }

    /// The exact-on-the-wire path: the frame's own 32-colour palette (31 ramp
    /// entries plus black), with the choice between the two nearest entries
    /// made by an ordered dither, so gradients keep their sub-step detail as a
    /// stable screen-door texture instead of banding.
    fn render_indexed(&mut self, t: Duration, out: &mut Indexed) -> bool {
        let mut lin = vec![[0f32; 3]; NPIX];
        self.render_lin(t, &mut lin);
        let pal = &self.ramps_idx[self.active_palette(t.as_secs_f64()) % self.ramps_idx.len()];
        out.palette.clear();
        out.palette.push([0, 0, 0]);
        out.palette.extend_from_slice(pal);
        let pal_lin: Vec<[f32; 3]> = out
            .palette
            .iter()
            .map(|c| self.panel.emit(*c))
            .collect();
        for y in 0..H {
            for x in 0..W {
                let c = lin[y * W + x];
                // Two nearest palette entries in linear light, then dither
                // along the line between them.
                let mut best = (f32::MAX, 0usize);
                let mut second = (f32::MAX, 0usize);
                for (i, p) in pal_lin.iter().enumerate() {
                    let d = (p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2);
                    if d < best.0 {
                        second = best;
                        best = (d, i);
                    } else if d < second.0 {
                        second = (d, i);
                    }
                }
                let idx = if second.0 == f32::MAX {
                    best.1
                } else {
                    let a = pal_lin[best.1];
                    let b = pal_lin[second.1];
                    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
                    let tproj = if len2 > 1e-9 {
                        ((c[0] - a[0]) * ab[0] + (c[1] - a[1]) * ab[1] + (c[2] - a[2]) * ab[2])
                            / len2
                    } else {
                        0.0
                    };
                    if tproj > 0.5 + bayer(x, y) {
                        second.1
                    } else {
                        best.1
                    }
                };
                out.indices[y * W + x] = idx as u8;
            }
        }
        true
    }
}

/// Escape time with a continuous (fractional) iteration count, or `None` for
/// a point that never escaped.
///
/// Two early-outs matter at depth: the main cardioid / period-2 bulb test
/// (free, kills the shallow views' black mass) and periodicity checking - an
/// orbit that returns to a previously seen point is trapped, which is what
/// makes the mini-brots inside a deep view cost ~40 iterations instead of
/// `max_iter`.
#[inline]
pub fn escape(cr: f64, ci: f64, max_iter: u32) -> Option<f64> {
    let q = (cr - 0.25) * (cr - 0.25) + ci * ci;
    if q * (q + (cr - 0.25)) <= 0.25 * ci * ci {
        return None;
    }
    if (cr + 1.0) * (cr + 1.0) + ci * ci <= 0.0625 {
        return None;
    }
    let (mut zr, mut zi) = (0f64, 0f64);
    let (mut or, mut oi) = (0f64, 0f64);
    let mut check = 8u32;
    let mut n = 0u32;
    while n < max_iter {
        let zr2 = zr * zr;
        let zi2 = zi * zi;
        let m = zr2 + zi2;
        if m > BAILOUT * BAILOUT {
            // nu = n + 1 - log2(log|z|); constant offsets do not matter, only
            // smoothness does.
            let log_zn = 0.5 * m.ln();
            let nu = n as f64 + 1.0 - (log_zn.ln() / std::f64::consts::LN_2);
            return Some(nu);
        }
        zi = 2.0 * zr * zi + ci;
        zr = zr2 - zi2 + cr;
        n += 1;
        // Periodicity check: compare against a reference point refreshed on a
        // geometric schedule (Brent's algorithm).
        let dr = zr - or;
        let di = zi - oi;
        if dr * dr + di * di < 1e-24 {
            return None;
        }
        if n == check {
            or = zr;
            oi = zi;
            check *= 2;
        }
    }
    None
}

/// Fraction of samples that fell inside the set, for the curation test.
pub fn interior_fraction(z: &FractalZoom, leg_age: f64, target: Target) -> f32 {
    let leg = Leg {
        target,
        age: leg_age,
    };
    let mut out = vec![[0f32; 3]; NPIX];
    z.render_leg(&leg, 0.0, &mut out);
    let black = out
        .iter()
        .filter(|p| p[0] + p[1] + p[2] < 1e-6)
        .count();
    black as f32 / NPIX as f32
}

/// Seconds of leg age at which the view reaches full depth.
pub fn full_depth_age(z: &FractalZoom) -> f64 {
    z.leg_secs
}

/// Convenience for tests and the preview's target sheet.
pub fn render_target(z: &mut FractalZoom, target: Target, age: f64, out: &mut Frame) {
    let leg = Leg { target, age };
    let mut lin = vec![[0f32; 3]; NPIX];
    z.render_leg(&leg, age, &mut lin);
    for i in 0..NPIX {
        let c = lin_to_srgb8_3(lin[i]);
        out.px[i * 3] = c[0];
        out.px[i * 3 + 1] = c[1];
        out.px[i * 3 + 2] = c[2];
    }
}
