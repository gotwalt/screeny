//! Flock: birds in slow motion, seen by a camera that is one of them.
//!
//! The flight is [`sim`]: Reynolds' boids in 3D on a fixed 1/60 s timestep,
//! steering round invisible geometry, with the camera in the flock as
//! `birds[0]`. This file is everything you can see - the colours, the sky and
//! the birds themselves.
//!
//! **A bird is two LEDs and a wingbeat.** At the far side of the flock that is
//! all it is; near the camera it spans eight or ten. What makes it a bird
//! rather than a dot is that its wings beat: a shallow V up, a shallow inverted
//! V down, a dash level, each bird on its own phase, gliding when it comes
//! down. Body and wings are drawn as anti-aliased strokes into a supersampled
//! coverage buffer, so a distant bird fades in coverage instead of popping
//! between LEDs, and nothing is tested against every sample.
//!
//! **The frame is indexed and exact.** Its palette is two-dimensional: a sky
//! band (where the view ray points, plus the sun's glow) crossed with an ink
//! level (how much bird is over it). Both are quantised through the ordered
//! dither, so the sky's bands and the birds' anti-aliasing both survive into a
//! frame of at most [`BANDS`] x [`INK`] colours. Nothing here is a continuous
//! gradient the codec has to guess at.

pub(crate) mod sim;

use crate::color::{oklch, smoothstep, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{choice, param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use sim::{v3, Sim, Tuning, V3, STEP};
use std::f32::consts::TAU;

pub const DEF: PatchDef = PatchDef {
    id: "flock",
    name: "Flock",
    blurb: "Birds in slow motion, seen by a camera that is one of them: boids in 3D round invisible geometry, an indexed sky and a hue that drifts.",
    params: PARAMS,
    make,
    // The world and the flock both come from the seed: the blobs' places,
    // sizes and drift periods, where the sun is, and the birds themselves. So
    // "another one like this" is a real thing to ask for, and the studio's
    // Another button means it (card 151).
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("birds", "Birds", 30.0, 150.0, 1.0, 55.0),
    param("pace", "Pace", 0.15, 2.0, 0.05, 0.7),
    param("calm", "Calm (wider, slower turns)", 0.0, 1.0, 0.01, 0.85),
    param("near", "How close the camera rides (m)", 2.0, 16.0, 0.5, 6.0),
    param("bank", "How far the view leans", 0.0, 1.5, 0.05, 0.8),
    choice("scheme", "Tones", SCHEMES, 0.0),
    param("hue", "Hue", 0.0, 360.0, 1.0, 250.0),
    param("spread", "Hue spread, horizon to zenith", -150.0, 150.0, 1.0, 40.0),
    param("wheel", "Hue drift (deg/min)", 0.0, 120.0, 1.0, 5.0),
    param("sky", "Sky level", 0.3, 1.3, 0.01, 1.0),
    param("terrain", "Invisible geometry", 0.0, 1.0, 0.01, 0.55),
    param("beat", "Wingbeat (Hz)", 0.5, 6.0, 0.1, 2.4),
    param("samples", "Samples per axis", 1.0, 6.0, 1.0, 3.0),
];

/// The two pictures the card asks for, in the patch's own words.
const SCHEMES: &[&str] = &["light on dark", "dusk silhouettes"];

/// Horizontal field of view. Wide enough that the flock is around you rather
/// than in front of you, narrow enough that a bird ten metres off is a bird.
const FOV: f32 = 76.0;

/// Sky bands, from the darkest end of the ramp to the horizon.
pub const SKY: usize = 12;
/// Sky bands plus the sun's halo and its disc.
pub const BANDS: usize = SKY + 2;
/// Levels of bird over sky, `0` being no bird at all.
pub const INK: usize = 6;

/// A colour as (lightness, chroma, hue in degrees), the way this file thinks
/// about one. Mixing happens here, not in RGB, so a fade through half a bird
/// keeps its hue instead of going muddy.
type Lch = (f32, f32, f32);

fn mix_hue(a: f32, b: f32, t: f32) -> f32 {
    a + ((b - a + 540.0).rem_euclid(360.0) - 180.0) * t
}

fn mix(a: Lch, b: Lch, t: f32) -> Lch {
    // A black end has no hue of its own; borrow the other's so a blend does
    // not swing through colours neither of them is.
    let (ha, hb) = (if a.0 < 0.01 { b.2 } else { a.2 }, if b.0 < 0.01 { a.2 } else { b.2 });
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, mix_hue(ha, hb, t))
}

/// Below this the panel has a handful of levels, they carry colour casts, and
/// a large area of them sparkles. True black instead: the deep end of this
/// sky is a shape, not the bottom of a fade.
const FLOOR_L: f32 = 0.085;

fn paint((l, c, h): Lch) -> Rgb {
    if l < FLOOR_L {
        Rgb::BLACK
    } else {
        oklch(l, c, h)
    }
}

/// The sky ramp and the bird, for one scheme.
///
/// `u` runs 0 at the dark end of the sky - straight up, and further down than
/// the horizon - to 1 at the horizon itself, which is the brightest part of
/// both pictures. One ramp does both ends, which is what a hazy sky really
/// looks like and what makes the horizon a line the eye can hold on to.
fn scheme(which: usize, hue: f32, spread: f32, level: f32) -> ([Lch; BANDS], Lch) {
    let dusk = which == 1;
    let mut bands = [(0.0, 0.0, 0.0); BANDS];
    for (b, band) in bands.iter_mut().enumerate().take(SKY) {
        let u = b as f32 / (SKY - 1) as f32;
        let (l, c) = if dusk {
            // A luminous dusk: the whole sky is lit and the birds are cut out
            // of it.
            (0.34 + 0.50 * u.powf(0.85), 0.045 + 0.105 * u)
        } else {
            // A deep sky the panel can hold: the top of the frame goes to true
            // black, the horizon is the one bright thing, and the birds are
            // brighter than any of it.
            (0.44 * u.powf(1.15), 0.05 + 0.085 * u)
        };
        *band = (l * level, c, mix_hue(hue + spread, hue, u));
    }
    let horizon = bands[SKY - 1];
    // The sun (or moon) near the horizon, as a bearing mark. It is the top of
    // the same ramp, so the ring of sky between it and the horizon is horizon
    // colour and there is no halo of unrelated bands around it.
    bands[SKY] = (horizon.0 + if dusk { 0.07 } else { 0.11 }, horizon.1 * 0.62, horizon.2 + 8.0);
    bands[SKY + 1] = (horizon.0 + if dusk { 0.15 } else { 0.28 }, horizon.1 * 0.30, horizon.2 + 14.0);
    let bird = if dusk { (0.085, 0.02, hue + 200.0) } else { (0.93, 0.05, hue + 40.0) };
    (bands, bird)
}

/// `BANDS` x `INK` colours: sky band `b` with `k`/`INK-1` of a bird over it.
fn palette(bands: &[Lch; BANDS], bird: Lch) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(BANDS * INK);
    for band in bands {
        for k in 0..INK {
            out.push(paint(mix(*band, bird, k as f32 / (INK - 1) as f32)));
        }
    }
    out
}

// --------------------------------------------------------------------------

/// The camera as the renderer needs it: an eye, three axes and a focal length.
pub(crate) struct View {
    eye: V3,
    right: V3,
    up: V3,
    fwd: V3,
    focal: f32,
    sun: V3,
}

impl View {
    pub(crate) fn of(sim: &Sim, sun: V3) -> View {
        let (right, up, fwd) = sim.view();
        View {
            eye: sim.camera().pos,
            right,
            up,
            fwd,
            focal: 0.5 * W as f32 / (0.5 * FOV.to_radians()).tan(),
            sun,
        }
    }

    /// Panel coordinates of a world point, and how far in front it is.
    /// `None` when it is behind the lens.
    pub(crate) fn project(&self, p: V3) -> Option<(f32, f32, f32)> {
        let v = p.sub(self.eye);
        let z = v.dot(self.fwd);
        if z < 0.35 {
            return None;
        }
        let k = self.focal / z;
        Some((
            0.5 * W as f32 + v.dot(self.right) * k,
            0.5 * H as f32 - v.dot(self.up) * k,
            z,
        ))
    }

    /// The direction the panel sees at these coordinates (not normalised in
    /// `y` alone; `sky` takes care of that).
    fn ray(&self, x: f32, y: f32) -> V3 {
        let px = (x - 0.5 * W as f32) / self.focal;
        let py = (0.5 * H as f32 - y) / self.focal;
        self.fwd.add(self.right.scale(px)).add(self.up.scale(py))
    }

    /// Where on the sky ramp this direction falls, 0..1, plus the sun's glow
    /// carried in the top two bands. Returned as a band coordinate in
    /// `0..BANDS-1` so the caller has one number to dither and round.
    fn band_at(&self, dir: V3) -> f32 {
        let inv = 1.0 / dir.len().max(1e-6);
        let sine = dir.y * inv;
        // x^0.75 by two square roots: this runs nine times a pixel.
        let warp = |a: f32| (a * a * a).sqrt().sqrt();
        let a = sine.abs().min(1.0);
        let v = if sine >= 0.0 {
            1.0 - warp(a)
        } else {
            // A step at the horizon, so it is a line and not just the top of a
            // gradient, and a faster fall below it.
            ((1.0 - 1.22 * warp(a)) * 0.88 - 0.05).max(0.0)
        };
        let sky = v * (SKY - 1) as f32;

        let cos = dir.dot(self.sun) * inv;
        // Angles as cosines: no acos in the inner loop. 3.2 deg disc, about a
        // 12 deg halo.
        let disc = smoothstep(0.998_2, 0.998_8, cos);
        let halo = 0.55 * (-50.0 * (1.0 - cos).max(0.0)).exp();
        let glow = (disc + halo * (1.0 - disc)).clamp(0.0, 1.0);
        sky + glow * ((BANDS - 1) as f32 - sky)
    }
}

// --------------------------------------------------------------------------

/// Coverage of bird over sky, at `ss` samples per panel pixel per axis.
struct Coverage {
    ss: usize,
    w: usize,
    h: usize,
    buf: Vec<f32>,
}

impl Coverage {
    fn new(ss: usize) -> Coverage {
        let (w, h) = (W * ss, H * ss);
        Coverage { ss, w, h, buf: vec![0.0; w * h] }
    }

    /// An anti-aliased stroke from `a` to `b`, `half` wide, laid over what is
    /// already there. Only the cells the stroke can reach are visited.
    fn stroke(&mut self, a: (f32, f32), b: (f32, f32), half: f32, ink: f32) {
        let ss = self.ss as f32;
        // Half a sub-cell of softness: enough to anti-alias, not enough to
        // smear a bird into a smudge.
        let aa = 0.5 / ss;
        let pad = half + aa;
        let lo_x = (((a.0.min(b.0) - pad) * ss).floor().max(0.0)) as usize;
        let hi_x = (((a.0.max(b.0) + pad) * ss).ceil().max(0.0) as usize).min(self.w);
        let lo_y = (((a.1.min(b.1) - pad) * ss).floor().max(0.0)) as usize;
        let hi_y = (((a.1.max(b.1) + pad) * ss).ceil().max(0.0) as usize).min(self.h);
        if lo_x >= hi_x || lo_y >= hi_y {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        for j in lo_y..hi_y {
            let py = (j as f32 + 0.5) / ss;
            for i in lo_x..hi_x {
                let px = (i as f32 + 0.5) / ss;
                let t = (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0);
                let (ex, ey) = (px - a.0 - dx * t, py - a.1 - dy * t);
                let d = (ex * ex + ey * ey).sqrt();
                let cover = smoothstep(pad, half - aa, d);
                if cover > 0.0 {
                    let cell = &mut self.buf[j * self.w + i];
                    *cell += (ink - *cell) * cover;
                }
            }
        }
    }

    /// Mean coverage over one panel pixel.
    fn pixel(&self, x: usize, y: usize) -> f32 {
        let mut sum = 0.0;
        for j in 0..self.ss {
            let row = (y * self.ss + j) * self.w + x * self.ss;
            sum += self.buf[row..row + self.ss].iter().sum::<f32>();
        }
        sum / (self.ss * self.ss) as f32
    }
}

// --------------------------------------------------------------------------

struct Flock {
    sim: Sim,
    /// Simulated seconds asked for so far, and steps actually taken. Kept
    /// apart so a render at any rate takes the same steps at the same moments
    /// (see [`Flock::advance`]).
    warped: f64,
    steps: i64,
    /// Which way the sun is, from the seed. It never moves: it is the one
    /// thing in the picture that says which way the flock has turned.
    sun: V3,
    /// For the studio's "now playing".
    seen: usize,
}

fn make(seed: u64) -> Box<dyn Patch> {
    let mut rng = crate::rng::Rng::new(seed ^ 0x73_756e);
    let bearing = rng.range(0.0, TAU);
    let lift = rng.range(0.10, 0.26);
    let flat = (1.0 - lift * lift).sqrt();
    Box::new(Flock {
        sim: Sim::new(seed),
        warped: 0.0,
        steps: 0,
        sun: v3(bearing.sin() * flat, lift, bearing.cos() * flat),
        seen: 0,
    })
}

/// Longest catch-up after a stall, in fixed steps. A studio that was paused
/// for a minute resumes flying, it does not fast-forward for a second.
const CATCHUP: i64 = 240;

impl Flock {
    fn tuning(ctx: &Ctx) -> Tuning {
        Tuning {
            blobs: (ctx.get("terrain") * sim::BLOBS as f32).round() as usize,
            ..Tuning::of(ctx.get("calm"), ctx.get("near"), ctx.get("bank"), ctx.get("beat"))
        }
    }

    /// Step the flight up to where this frame's clock has reached.
    ///
    /// The step count is taken from the total simulated time rather than by
    /// draining an accumulator, and it is floored with a small bias, so 30 fps
    /// and 60 fps agree on the integer even when their float sums differ in
    /// the last bits. That is what makes a snapshot the same PNG and a run the
    /// same flight at any rate.
    fn advance(&mut self, ctx: &Ctx, tune: &Tuning) {
        self.warped += ctx.dt.clamp(0.0, 0.25) * f64::from(ctx.get("pace"));
        let target = (self.warped / f64::from(STEP) + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.sim.step(tune, STEP);
            self.steps += 1;
        }
    }
}

impl Patch for Flock {
    fn playing(&self) -> Option<Playing> {
        Some(Playing {
            title: format!("{} birds", self.sim.flock().len()),
            detail: format!("{} in frame, nearest {:.1} m", self.seen, self.sim.nearest()),
            actions: Vec::new(),
            notes: Vec::new(),
        })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        let tune = Flock::tuning(ctx);
        self.sim.resize(ctx.get("birds") as usize);
        self.advance(ctx, &tune);

        let wheel = ctx.get("wheel") * (ctx.t / 60.0) as f32;
        let (bands, bird) = scheme(
            ctx.get("scheme") as usize,
            ctx.get("hue") + wheel,
            ctx.get("spread"),
            ctx.get("sky"),
        );
        let colours = palette(&bands, bird);

        let view = View::of(&self.sim, self.sun);
        let ss = (ctx.get("samples") as usize).clamp(1, 6);
        let mut cover = Coverage::new(ss);
        self.seen = draw_birds(&self.sim, &view, &mut cover);

        // Sky and birds are quantised separately through the same blue-noise
        // mask: the band is smooth and needs the dither to not stair-step, the
        // ink is a few levels and needs it to keep a wing's edge.
        let dither = Dither::BlueNoise;
        let indices = (0..N)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let bias = dither.threshold(x, y);
                let mut band = 0.0;
                for j in 0..ss {
                    for k in 0..ss {
                        let fx = x as f32 + (k as f32 + 0.5) / ss as f32;
                        let fy = y as f32 + (j as f32 + 0.5) / ss as f32;
                        band += view.band_at(view.ray(fx, fy));
                    }
                }
                band /= (ss * ss) as f32;
                let b = (band + bias).round().clamp(0.0, (BANDS - 1) as f32) as usize;
                let ink = cover.pixel(x, y) * (INK - 1) as f32;
                let k = (ink + bias).round().clamp(0.0, (INK - 1) as f32) as usize;
                (b * INK + k) as u8
            })
            .collect();

        Frame::Indexed { palette: colours, indices }
    }
}

/// Every bird, far to near, as a body dash and two wing strokes. Returns how
/// many landed on the panel.
fn draw_birds(sim: &Sim, view: &View, cover: &mut Coverage) -> usize {
    let mut order: Vec<(f32, usize)> = Vec::with_capacity(sim.flock().len());
    for (i, b) in sim.flock().iter().enumerate() {
        if let Some((_, _, z)) = view.project(b.pos) {
            order.push((z, i));
        }
    }
    order.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut seen = 0;
    for (z, i) in order {
        let bird = sim.flock()[i];
        let (right, up, fwd) = bird.frame();
        let half = 0.5 * sim::SPAN;

        // The wingbeat. Down is quicker than up, which is what tells the eye
        // this is a wing and not an oscillation; a glide holds them level and
        // very slightly raised.
        let s = (bird.phase + 0.35 * bird.phase.sin()).sin();
        let dihedral = 0.62 * (1.0 - 0.75 * bird.glide) * s + 0.10 * bird.glide;
        let (sin_d, cos_d) = dihedral.sin_cos();
        // Tips swept back: the only thing standing in for a wing's shape.
        let sweep = fwd.scale(-0.24 * sim::SPAN);
        let lift = up.scale(half * sin_d);
        let out = right.scale(half * cos_d);
        let shoulder = bird.pos.add(fwd.scale(0.04 * sim::SPAN));
        let nose = bird.pos.add(fwd.scale(0.30 * sim::SPAN));
        let tail = bird.pos.sub(fwd.scale(0.40 * sim::SPAN));

        let Some(p_shoulder) = view.project(shoulder) else { continue };
        let Some(p_nose) = view.project(nose) else { continue };
        let Some(p_tail) = view.project(tail) else { continue };
        let Some(p_left) = view.project(shoulder.sub(out).add(lift).add(sweep)) else { continue };
        let Some(p_right) = view.project(shoulder.add(out).add(lift).add(sweep)) else { continue };

        // How far the bird spans on the panel sets both its stroke weight and,
        // with the haze, how much it stands out from the sky.
        let span = sim::SPAN * view.focal / z;
        let wing = (span * 0.13).clamp(0.38, 1.05);
        let body = (span * 0.17).clamp(0.42, 1.30);
        // Depth reads as contrast: far birds are washed into the sky, and one
        // that comes closer than the camera's own personal space fades out
        // rather than filling the panel.
        // Depth is carried by contrast far more than by size at this scale:
        // sixteen metres of air already halves how much a bird stands out
        // from the sky, which is what stops seventy of them reading as fog.
        let haze = 0.20 + 0.80 * (-(z / 16.0).powf(1.6)).exp();
        let ink = haze * smoothstep(0.7, 1.8, z);
        if ink < 0.02 {
            continue;
        }

        let margin = span + 3.0;
        if p_shoulder.0 < -margin
            || p_shoulder.0 > W as f32 + margin
            || p_shoulder.1 < -margin
            || p_shoulder.1 > H as f32 + margin
        {
            continue;
        }
        seen += 1;

        let flat = |p: (f32, f32, f32)| (p.0, p.1);
        cover.stroke(flat(p_tail), flat(p_nose), body, ink);
        cover.stroke(flat(p_shoulder), flat(p_left), wing, ink);
        cover.stroke(flat(p_shoulder), flat(p_right), wing, ink);
    }
    seen
}

#[cfg(test)]
mod tests;
