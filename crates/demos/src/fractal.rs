//! Endless Mandelbrot zoom.
//!
//! **Endlessness.** The piece is a *tour*, not one zoom. It zooms toward a
//! boundary point at a constant exponential rate for `leg_secs` - ten octaves
//! in ninety seconds, about nine seconds per doubling, which is the ambient
//! pace this panel wants - and then cross-dissolves into the next leg, which
//! is already zooming at the same rate when the dissolve starts, so motion
//! continuity carries the cut. Six targets makes a nine-minute lap, and the
//! lap repeats forever without ever hitting a precision wall or a flat patch.
//!
//! **Why only ten octaves.** Not precision - f64 is good to about forty. It
//! is iterations. Escape counts near the boundary grow roughly geometrically
//! with depth (measured: `preview probe`; at four octaves the 90th percentile
//! pixel needs ~50 iterations, at twelve it needs ~5000, at twenty ~50000).
//! Ten octaves is the deepest a 30 fps budget reaches at 16 samples per pixel,
//! and it costs nothing artistically: everything a deeper view would add is
//! finer than one of these 2048 pixels. Pixels that do exhaust the budget fade
//! into the black interior instead of clipping, so the ceiling reads as a dark
//! halo around the set rather than as speckle.
//!
//! **Look.** Continuous (smooth) escape time in a `sqrt` coordinate (so band
//! spacing stays put as the view shrinks), mapped through a designed cyclic
//! OKLCH ramp that never goes below L 0.40 (the panel's dark end is missing -
//! brief section 2.1) and has a narrow bright highlight rather than a broad
//! pale one. Interior is true black, used as a shape. Every sample is coloured
//! and then averaged in **linear light** over an ss x ss grid, which is what
//! stops the bands shimmering as they flow, and the result is snapped to codes
//! the panel can emit, which typically halves the frame's colour count.
//!
//! Deterministic: the seed picks where in the tour it starts, and nothing else
//! in the piece is random.

use crate::color::{lin_to_srgb8_3, oklch_to_lin};
use crate::dither::bayer;
use crate::frame::{Frame, Indexed, Piece, H, NPIX, W};
use crate::panel::{Panel, NOMINAL};
use std::time::Duration;

/// A curated *direction*, not a point: a segment from somewhere known to be
/// inside the set to somewhere known to be outside it.
///
/// Hand-written deep-zoom coordinates are the obvious approach and it is a
/// trap - a digit out of place puts the centre in a smooth patch of exterior
/// and the zoom fades to one flat colour, which is exactly what the first
/// version of this file did on three of six targets. Bisecting a segment
/// whose ends straddle the boundary cannot do that: the limit point is on the
/// boundary by construction, so every depth has black interior on one side
/// and bands on the other. The endpoints only need three decimal places,
/// which are the digits a human can actually be sure of.
#[derive(Clone, Copy)]
pub struct Region {
    pub name: &'static str,
    /// A point inside the set (a bulb or cardioid centre).
    pub inside: (f64, f64),
    /// A point outside it; the segment between them crosses the boundary.
    pub outside: (f64, f64),
    pub palette: usize,
}

/// A resolved target: `Region` bisected to the last bit of f64.
#[derive(Clone, Copy)]
pub struct Target {
    pub name: &'static str,
    pub cx: f64,
    pub cy: f64,
    pub palette: usize,
}

pub const REGIONS: &[Region] = &[
    // Seahorse valley, between the cardioid and the period-2 bulb.
    Region {
        name: "seahorse",
        inside: (-0.1, 0.0),
        outside: (-0.748, 0.123),
        palette: 0,
    },
    // The western antenna: aim from the period-2 bulb along the needle.
    Region {
        name: "needle",
        inside: (-1.0, 0.0),
        outside: (-1.768, 0.002),
        palette: 1,
    },
    // Elephant valley on the east flank of the cardioid.
    Region {
        name: "elephant",
        inside: (0.0, 0.0),
        outside: (0.2825, 0.0105),
        palette: 2,
    },
    // The northern bulb's shoulder.
    Region {
        name: "north",
        inside: (-0.125, 0.744),
        outside: (-0.1, 0.9),
        palette: 3,
    },
    // Triple spiral valley, north-west of the cardioid.
    Region {
        name: "triple-spiral",
        inside: (-0.125, 0.744),
        outside: (-0.088, 0.654),
        palette: 4,
    },
    // South-west, toward the period-3 bulb's antenna.
    Region {
        name: "south-tendril",
        inside: (-1.0, 0.0),
        outside: (-1.257, -0.379),
        palette: 5,
    },
];

/// Is `c` in the set, as far as a long orbit can tell? Used only for the
/// one-off bisection, so it can afford a big iteration count.
fn inside(cr: f64, ci: f64) -> bool {
    escape(cr, ci, 20_000).is_none()
}

/// Bisect a region's segment down to the last representable step. The result
/// is a point every neighbourhood of which contains both interior and
/// exterior: a boundary point, to f64.
pub fn resolve(r: &Region) -> Target {
    let (mut ax, mut ay) = r.inside;
    let (mut bx, mut by) = r.outside;
    debug_assert!(inside(ax, ay), "{}: inside end is not inside", r.name);
    for _ in 0..90 {
        let mx = 0.5 * (ax + bx);
        let my = 0.5 * (ay + by);
        if (mx == ax && my == ay) || (mx == bx && my == by) {
            break;
        }
        if inside(mx, my) {
            ax = mx;
            ay = my;
        } else {
            bx = mx;
            by = my;
        }
    }
    Target {
        name: r.name,
        cx: bx,
        cy: by,
        palette: r.palette,
    }
}

const BAILOUT: f64 = 256.0;
const START_SCALE: f64 = 0.55;
const END_SCALE: f64 = 5.4e-4;

/// Palette keyframes in OKLCH (L, chroma, hue in turns), interpolated
/// cyclically.
///
/// Each ramp is one hue family swept through a second, with a *narrow*
/// highlight rather than a broad pale one: on LED primaries, saturated colour
/// is where this display is spectacular and large near-white areas read as
/// washed out and dirty (brief section 2.2). Nothing goes below L 0.40, which
/// is where the panel still has levels to spare.
const PALETTES: &[&[(f32, f32, f32)]] = &[
    // 0 ember: deep red -> orange -> gold -> white-hot -> magenta
    &[
        (0.40, 0.16, 0.040),
        (0.53, 0.18, 0.070),
        (0.68, 0.19, 0.105),
        (0.85, 0.15, 0.155),
        (0.60, 0.17, 0.955),
        (0.45, 0.16, 0.005),
    ],
    // 1 ice: deep blue -> blue -> cyan -> pale cyan -> violet
    &[
        (0.40, 0.15, 0.725),
        (0.55, 0.16, 0.685),
        (0.72, 0.14, 0.555),
        (0.86, 0.11, 0.545),
        (0.58, 0.16, 0.800),
        (0.45, 0.15, 0.760),
    ],
    // 2 acid: deep green -> green -> chartreuse -> cream -> teal
    &[
        (0.42, 0.17, 0.425),
        (0.58, 0.19, 0.380),
        (0.75, 0.19, 0.325),
        (0.86, 0.13, 0.260),
        (0.57, 0.14, 0.495),
        (0.46, 0.16, 0.455),
    ],
    // 3 sunset: magenta -> rose -> peach -> indigo
    &[
        (0.42, 0.16, 0.900),
        (0.55, 0.18, 0.965),
        (0.70, 0.17, 0.025),
        (0.85, 0.13, 0.095),
        (0.52, 0.17, 0.805),
        (0.45, 0.16, 0.855),
    ],
    // 4 copper: brown -> amber -> gold -> olive
    &[
        (0.40, 0.11, 0.120),
        (0.55, 0.14, 0.155),
        (0.72, 0.16, 0.195),
        (0.85, 0.13, 0.225),
        (0.60, 0.12, 0.300),
        (0.46, 0.11, 0.085),
    ],
    // 5 lagoon: teal -> aqua -> pale aqua -> blue
    &[
        (0.42, 0.13, 0.545),
        (0.56, 0.15, 0.500),
        (0.72, 0.14, 0.460),
        (0.85, 0.11, 0.425),
        (0.55, 0.16, 0.660),
        (0.46, 0.14, 0.600),
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
    /// Escape-time band spacing, in units of sqrt(iterations) per palette
    /// cycle. See `band_coord`.
    pub band_period: f32,
    pub targets: Vec<Target>,
    /// Palette rotations per second.
    pub rot_rate: f32,
    /// Iteration budget: `base + per_octave * zoom octaves`.
    pub iter_base: f64,
    pub iter_per_octave: f64,
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
            leg_secs: 90.0,
            fade_secs: 1.1,
            band_period: 11.0,
            rot_rate: 0.035,
            iter_base: 350.0,
            iter_per_octave: 120.0,
            targets: REGIONS.iter().map(resolve).collect(),
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
        let n = self.targets.len() as i64;
        let i = (leg_no + (self.seed as i64 % n) + n * 4) % n;
        self.targets[i as usize]
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
        let depth = (START_SCALE / scale).log2().max(0.0);
        (self.iter_base + self.iter_per_octave * depth) as u32
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
        let fade_from = 0.55 * max_iter as f64;
        let fade_span = max_iter as f64 - fade_from;
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
                                        let u = (band_coord(nu) * inv_period + rot)
                                            .rem_euclid(1.0);
                                        let e = ramp[(u * ramp.len() as f64) as usize
                                            % ramp.len()];
                                        // Fade the last stretch of the budget
                                        // into the black interior, so the
                                        // iteration ceiling reads as a dark
                                        // halo round the set rather than as a
                                        // hard edge and a rash of black
                                        // speckles.
                                        let g = if nu > fade_from {
                                            let f = ((nu - fade_from) / (fade_span)).min(1.0);
                                            let f = f * f * (3.0 - 2.0 * f);
                                            (1.0 - f) as f32
                                        } else {
                                            1.0
                                        };
                                        acc[0] += e[0] * g;
                                        acc[1] += e[1] * g;
                                        acc[2] += e[2] * g;
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
            // Snap to codes the panel can actually emit. It costs nothing in
            // quality - the panel would round to these anyway - and it folds
            // hundreds of near-identical averages onto the same code, which
            // is straight profit in the sender's palette budget.
            let c = self.panel.snap(lin_to_srgb8_3(lin[i]));
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

/// The coordinate the palette cycles in.
///
/// Escape counts pile up geometrically as you approach the set, so cycling
/// the palette linearly in iterations gives wide bands far out and, near the
/// boundary, bands finer than a pixel - which is the mush the first version of
/// this piece showed at depth: 1800 colours of it, averaging to grey-pink.
/// `sqrt` spreads the same range so that about three cycles cross the panel at
/// any depth, shallow or deep, without any per-frame normalisation (which
/// would flicker).
#[inline]
pub fn band_coord(nu: f64) -> f64 {
    nu.max(0.0).sqrt()
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
        let c = z.panel.snap(lin_to_srgb8_3(lin[i]));
        out.px[i * 3] = c[0];
        out.px[i * 3 + 1] = c[1];
        out.px[i * 3 + 2] = c[2];
    }
}
