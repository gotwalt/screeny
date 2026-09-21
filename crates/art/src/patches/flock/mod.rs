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
//! **Drawn big it is more than that** ([`bird`], card 123): a wing with a
//! wrist in it, carrying chord, over a tapered body with a tail that fans. How
//! much of that a bird gets is decided by its *projected span* and never by
//! `size` - see [`AREA`] - so the far side of a default flock is still the two
//! LEDs above, and only the birds that are big enough for it to read are given
//! a surface.
//!
//! **The frame is indexed and exact.** Its palette is two-dimensional: a sky
//! band (where the view ray points, plus the sun's glow) crossed with an ink
//! level (how much bird is over it). Both are quantised through the ordered
//! dither, so the sky's bands and the birds' anti-aliasing both survive into a
//! frame of at most [`BANDS`] x [`INK`] colours. Nothing here is a continuous
//! gradient the codec has to guess at.

pub(crate) mod bird;
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
    param("birds", "How many birds", 3.0, 150.0, 1.0, 55.0),
    param("pace", "How fast the flight moves (lower is slower, dreamier)", 0.15, 2.0, 0.05, 0.7),
    param("calm", "How wide and slow the turns are", 0.0, 1.0, 0.01, 0.90),
    param("wild", "How often it changes its mind", 0.0, 1.0, 0.01, 0.65),
    param("lift", "How much of the motion is vertical", 0.0, 1.0, 0.01, 0.50),
    param("near", "How close the camera rides (m)", 2.0, 16.0, 0.5, 6.0),
    param("size", "How big the birds are drawn (a longer lens, not a closer camera)", 0.5, 3.0, 0.1, 1.0),
    param("lean", "How far the birds lean into a turn", 0.0, 2.0, 0.05, 1.0),
    param("bank", "How far the view leans into a turn", 0.0, 1.5, 0.05, 0.8),
    choice("backdrop", "What is behind the birds", BACKDROPS, 0.0),
    choice("scheme", "Light birds or dark silhouettes", SCHEMES, 0.0),
    param("hue", "Sky colour", 0.0, 360.0, 1.0, 250.0),
    param("spread", "How much the colour shifts, horizon to zenith", -150.0, 150.0, 1.0, 40.0),
    param("wheel", "How fast the colour rotates (degrees a minute)", 0.0, 120.0, 1.0, 5.0),
    param("sky", "How bright the sky is", 0.3, 1.3, 0.01, 1.0),
    param("terrain", "How much unseen scenery makes them swerve", 0.0, 1.0, 0.01, 0.55),
    param("beat", "How fast the wings beat (Hz)", 0.5, 6.0, 0.1, 2.4),
];

/// What is behind the birds (the owner, 2026-09-20): the whole sky; nothing
/// at all - white birds on true black; or black with only a faint line where
/// the horizon is, which is the least that still shows the camera banking and
/// climbing.
const BACKDROPS: &[&str] = &["sky", "horizon line", "black"];

/// The two pictures the card asks for, in the patch's own words.
const SCHEMES: &[&str] = &["light on dark", "dusk silhouettes"];

/// Horizontal field of view. Wide enough that the flock is around you rather
/// than in front of you, narrow enough that a bird ten metres off is a bird.
const FOV: f32 = 76.0;

/// Coverage samples per panel pixel per axis, used to anti-alias the birds
/// (card 168). This used to be a parameter ("Samples per axis") - the owner,
/// 2026-09-21: "super confusing." Nothing here is a cost the owner asked
/// about, and three was always the answer: 0.3 ms a frame at 55 birds, well
/// inside the 33 ms budget (card 177's Log), so there was nothing to trade
/// and nothing for a person to usefully turn. An old saved value for
/// `samples` is simply ignored - `Params::set` refuses an id the current
/// spec does not have (`crates/art/src/patch.rs`), which is exactly the
/// behaviour a removed parameter needs.
const SUPERSAMPLE: usize = 3;

/// Sky bands, from the darkest end of the ramp to the horizon.
pub const SKY: usize = 12;
/// Sky bands plus the sun's halo and its disc.
pub const BANDS: usize = SKY + 2;
/// Levels of bird over sky, `0` being no bird at all.
///
/// With a sky behind them the palette is [`BANDS`] x [`INK`] and six is what
/// there is room for. With the sky **off** every band is the same black (or,
/// for the horizon line, the line's own ramp), so the bands cost nothing and
/// the ink may have as many levels as the index byte allows - which matters,
/// because on black these six levels are carrying both the anti-aliasing and
/// the whole of the depth cue, and the steps show as birds come and go.
/// Sixteen x fourteen bands is 224 entries, inside the 256 an index byte has.
pub const INK: usize = 6;
pub const INK_DARK: usize = 12;

/// How many ink levels this backdrop gets.
pub fn ink_levels(backdrop: usize) -> usize {
    if backdrop == 0 {
        INK
    } else {
        INK_DARK
    }
}

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
            // A luminous dusk: the whole sky is lit and the birds are cut
            // out of it. The range has to be wide - a sky that is all one
            // bright lavender has nowhere for a silhouette to sit, and it
            // lights every LED.
            (0.20 + 0.60 * u.powf(0.9), 0.05 + 0.13 * u)
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
    // A silhouette is the sky *darkened*, not a different colour. Given its
    // own hue - the complement was the first try - every half-covered pixel
    // between a pale sky and a dark bird lands on a muddy in-between, which is
    // precisely the near-grey pastel the brief says this panel cannot show.
    // Same hue, much lower lightness, and the blends run cleanly down it.
    let bird = if dusk { (0.05, 0.04, hue) } else { (0.93, 0.05, hue + 40.0) };
    (bands, bird)
}

/// `BANDS` x `ink` colours: sky band `b` with `k`/`ink-1` of a bird over it.
fn palette(bands: &[Lch; BANDS], bird: Lch, ink: usize) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(BANDS * ink);
    for band in bands {
        for k in 0..ink {
            out.push(paint(mix(*band, bird, k as f32 / (ink - 1) as f32)));
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

    /// An anti-aliased filled triangle, laid over what is already there.
    ///
    /// A wing's surface, which a stroke cannot be: a stroke is one width all
    /// the way along, and a wing is broad at the root and nothing at all at
    /// the tip. Coverage comes from the signed distance to the nearest edge,
    /// so the edges anti-alias exactly as the strokes' do and a wing that the
    /// view has turned edge-on thins away instead of flickering.
    fn triangle(&mut self, a: (f32, f32), b: (f32, f32), c: (f32, f32), ink: f32) {
        let ss = self.ss as f32;
        let aa = 0.5 / ss;
        // Twice the signed area: which way round the corners are given, and
        // whether there is a triangle here at all. A wing seen exactly
        // edge-on, or one whose chord the level of detail has taken to zero,
        // is a line and has nothing to fill.
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1e-6 {
            return;
        }
        let wind = area.signum();
        let (min_x, max_x) = (a.0.min(b.0).min(c.0), a.0.max(b.0).max(c.0));
        let (min_y, max_y) = (a.1.min(b.1).min(c.1), a.1.max(b.1).max(c.1));
        let lo_x = (((min_x - aa) * ss).floor().max(0.0)) as usize;
        let hi_x = (((max_x + aa) * ss).ceil().max(0.0) as usize).min(self.w);
        let lo_y = (((min_y - aa) * ss).floor().max(0.0)) as usize;
        let hi_y = (((max_y + aa) * ss).ceil().max(0.0) as usize).min(self.h);
        if lo_x >= hi_x || lo_y >= hi_y {
            return;
        }
        let edges = [(a, b), (b, c), (c, a)];
        for j in lo_y..hi_y {
            let py = (j as f32 + 0.5) / ss;
            for i in lo_x..hi_x {
                let px = (i as f32 + 0.5) / ss;
                let mut d = f32::INFINITY;
                for (p, q) in edges {
                    let (ex, ey) = (q.0 - p.0, q.1 - p.1);
                    let len = (ex * ex + ey * ey).sqrt().max(1e-6);
                    d = d.min(wind * (ex * (py - p.1) - ey * (px - p.0)) / len);
                }
                let cover = smoothstep(-aa, aa, d);
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

/// How much a bird at the far side of the flock still stands out from the sky,
/// per scheme. A dim *light* bird on a near-black sky reads at a fifth of full
/// contrast; a silhouette on a lit sky needs three times that or it is a
/// smudge, because it is only a little darker than what is behind it.
const HAZE_LIGHT: f32 = 0.20;
const HAZE_DUSK: f32 = 0.62;

impl Flock {
    fn tuning(ctx: &Ctx) -> Tuning {
        Tuning {
            blobs: (ctx.get("terrain") * sim::BLOBS as f32).round() as usize,
            wild: ctx.get("wild"),
            lift: ctx.get("lift"),
            lean: ctx.get("lean"),
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
        // The owner's options (2026-09-20). Without the sky the birds are
        // white on true black whatever `scheme` says - a silhouette needs
        // something to be cut out of - and the sun goes too, so nothing but
        // the flock (and, if asked for, one faint line) is ever lit.
        let backdrop = ctx.get("backdrop") as usize;
        let which = if backdrop == 0 { ctx.get("scheme") as usize } else { 0 };
        let (mut bands, mut bird) = scheme(which, ctx.get("hue") + wheel, ctx.get("spread"), ctx.get("sky"));
        if backdrop != 0 {
            let horizon = bands[SKY - 1];
            bands = [(0.0, 0.0, 0.0); BANDS];
            if backdrop == 1 {
                // The sky's bands become the line at rising strength (for its
                // anti-aliased edge): dim, and in the horizon's own hue.
                let line = (0.42, horizon.1 * 0.5, horizon.2);
                for (b, band) in bands.iter_mut().enumerate().take(SKY) {
                    *band = mix((0.0, 0.0, 0.0), line, b as f32 / (SKY - 1) as f32);
                }
            }
            bird = (0.97, 0.0, 0.0);
        }
        let levels = ink_levels(backdrop);
        let colours = palette(&bands, bird, levels);

        // No sun without a sky: a zero vector has no direction to glow in.
        let view = View::of(&self.sim, if backdrop == 0 { self.sun } else { v3(0.0, 0.0, 0.0) });
        let ss = SUPERSAMPLE;
        let mut cover = Coverage::new(ss);
        let dusk = which == 1;
        let size = ctx.get("size");
        self.seen = draw_birds(&self.sim, &view, &mut cover, dusk, size);

        // Sky and ink are quantised separately, each through its own mask,
        // because the two axes are not the same problem. The sky
        // is a smooth gradient across the whole frame, so its dither is most
        // of the index plane and what it costs on the wire matters: an
        // ordered pattern tiles and compresses where a random one does not,
        // and Bayer 4x4 came out 200 bytes cheaper than blue noise for a
        // picture I could not tell apart (card 168's Log). The ink is a
        // handful of levels over small, moving shapes, where blue noise's
        // lack of structure is worth having and there is hardly any of it.
        let ink_dither = Dither::BlueNoise;
        let sky_dither = Dither::Bayer4;
        let indices = (0..N)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let bias = ink_dither.threshold(x, y);
                let sky_bias = sky_dither.threshold(x, y);
                let b = if backdrop == 0 {
                    let mut band = 0.0;
                    for j in 0..ss {
                        for k in 0..ss {
                            let fx = x as f32 + (k as f32 + 0.5) / ss as f32;
                            let fy = y as f32 + (j as f32 + 0.5) / ss as f32;
                            band += view.band_at(view.ray(fx, fy));
                        }
                    }
                    band /= (ss * ss) as f32;
                    (band + sky_bias).round().clamp(0.0, (BANDS - 1) as f32) as usize
                } else if backdrop == 1 {
                    // The horizon as a line and nothing else: one LED thick at
                    // any bank, never dithered, so it is a steady hint and not
                    // a sparkle.
                    // Anti-aliased down the sky ramp's own bands, which in
                    // this mode are the line's colour at rising strength, so a
                    // banked horizon is a smooth slope and not a staircase.
                    let (up, down) = (view.ray(x as f32 + 0.5, y as f32).y, view.ray(x as f32 + 0.5, y as f32 + 1.0).y);
                    let fall = up - down;
                    if fall.abs() < 1e-6 {
                        0
                    } else {
                        // Where the horizon crosses this column, in LEDs from
                        // this LED's top edge; one LED thick around that.
                        let cover = (1.0 - (0.5 - up / fall).abs()).clamp(0.0, 1.0);
                        (cover * (SKY - 1) as f32).round() as usize
                    }
                } else {
                    0
                };
                let ink = cover.pixel(x, y) * (levels - 1) as f32;
                let k = (ink + bias).round().clamp(0.0, (levels - 1) as f32) as usize;
                (b * levels + k) as u8
            })
            .collect();

        Frame::Indexed { palette: colours, indices }
    }
}

/// How wide a bird has to be drawn, in LEDs, before it is given a wing's
/// surface rather than a wing's line, and how wide before it has all of it.
///
/// **The level of detail is decided by projected span, never by `size`.** At
/// the default 55 birds the far side of the flock is two or three pixels
/// across and the nearest is seven or eight: below [`AREA.0`] the model is
/// exactly card 168's skeleton, so the picture the owner said "this is great"
/// about is the picture that still comes out. The surface grows in over the
/// next few LEDs rather than switching on, so a bird coming towards the camera
/// does not pop.
const AREA: (f32, f32) = (5.0, 9.0);

/// How much brighter a wing's underside is than its top, as a fraction of the
/// bird's ink. Small on purpose: it is there so a bird rolling through a turn
/// flashes, not so the two sides read as different colours. Inverted for the
/// dusk scheme, where more ink is *darker* and a lit underside means less of
/// it.
const UNDERSIDE: f32 = 0.16;

/// Every bird, far to near: a tapered body, a tail that fans, and two wings
/// with a wrist. Returns how many landed on the panel.
///
/// `size` scales the wingspan the bird is *drawn* at - a longer lens, not a
/// closer camera (card 122): it makes the same flight bigger on the panel
/// without moving the camera into the flock, where card 169 found that
/// scatters it. It touches nothing in `sim`, so the flight, the seat and the
/// framing are exactly what they are at `size` 1.
fn draw_birds(sim: &Sim, view: &View, cover: &mut Coverage, dusk: bool, size: f32) -> usize {
    let floor = if dusk { HAZE_DUSK } else { HAZE_LIGHT };
    let mut order: Vec<(f32, usize)> = Vec::with_capacity(sim.flock().len());
    for (i, b) in sim.flock().iter().enumerate() {
        if let Some((_, _, z)) = view.project(b.pos) {
            order.push((z, i));
        }
    }
    order.sort_by(|a, b| b.0.total_cmp(&a.0));

    let span_m = sim::SPAN * size;
    let mut seen = 0;
    for (z, i) in order {
        let bird = sim.flock()[i];

        // How far the bird spans on the panel sets its stroke weights, how
        // much of a wing's surface it is given, and - with the haze - how much
        // it stands out from the sky.
        let span = span_m * view.focal / z;
        let pose = bird::pose(bird, span_m, smoothstep(AREA.0, AREA.1, span));
        // The body tapers away from the chest, which is the fattest part and
        // sits just behind the shoulder: a thinner neck forward of it, a
        // thinner head and beak again, and a thin boom back to the tail. All
        // four floor at about the same sub-pixel width, so a distant bird is
        // the single even dash it has been since card 168.
        let body = (span * 0.17).clamp(0.42, 1.30);
        let neck = (0.62 * body).max(0.40);
        let beak = (0.38 * body).max(0.36);
        let boom = (0.42 * body).max(0.38);
        let wing = (span * 0.13).clamp(0.38, 1.05);
        // The spar is the leading edge, and a wing's leading edge is thicker
        // at the shoulder than at the tip.
        let hand = (0.70 * wing).max(0.36);
        // Depth reads as contrast: far birds are washed into the sky, and one
        // that comes closer than the camera's own personal space fades out
        // rather than filling the panel.
        // Depth is carried by contrast far more than by size at this scale:
        // sixteen metres of air already halves how much a bird stands out
        // from the sky, which is what stops fifty of them reading as fog.
        //
        // How far that is allowed to go is not the same in the two schemes. A
        // dim light bird on a near-black sky still reads; a hazed *silhouette*
        // on a bright sky is a smudge, because it is only a little darker than
        // what is behind it. So the far end of the haze is held much higher
        // for the dusk picture.
        let haze = floor + (1.0 - floor) * (-(z / 16.0).powf(1.6)).exp();
        let ink = haze * smoothstep(0.7, 1.8, z);
        if ink < 0.02 {
            continue;
        }

        // On the panel, or near enough to it to be worth drawing. Measured at
        // the chest, which is the one landmark a wing cannot swing away from.
        let Some(chest) = view.project(pose.spine[2]) else { continue };
        let margin = span + 3.0;
        if chest.0 < -margin
            || chest.0 > W as f32 + margin
            || chest.1 < -margin
            || chest.1 > H as f32 + margin
        {
            continue;
        }

        // A bird that has put any one of its corners behind the lens is one
        // the camera is inside; it has already faded to nothing by then.
        let flat = |p: V3| view.project(p).map(|(x, y, _)| (x, y));
        let Some(spine) = pose.spine.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let Some(tail) = pose.tail.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else {
            continue;
        };
        seen += 1;

        // The surfaces first and the bones over them, so a wing's own
        // leading edge is never dimmed by the membrane behind it.
        cover.triangle(spine[3], tail[0], tail[1], ink);
        let to_bird = pose.spine[2].sub(view.eye);
        let away = 1.0 / to_bird.len().max(1e-6);
        for w in &pose.wings {
            let Some(spar) = w.spar.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else {
                continue;
            };
            let Some(trail) = w.trail.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else {
                continue;
            };
            // Which face of this wing the camera is on. Positive is the
            // underside: the wing's own up points away from the eye.
            let facing = (to_bird.dot(w.normal) * away).clamp(-1.0, 1.0);
            let lit = (ink * (1.0 + if dusk { -UNDERSIDE } else { UNDERSIDE } * facing)).min(1.0);
            cover.triangle(spar[0], spar[1], trail[1], lit);
            cover.triangle(spar[0], trail[1], trail[0], lit);
            cover.triangle(spar[1], spar[2], trail[1], lit);
            cover.stroke(spar[0], spar[1], wing, lit);
            cover.stroke(spar[1], spar[2], hand, lit);
        }
        cover.stroke(spine[4], spine[3], boom, ink);
        cover.stroke(spine[3], spine[2], body, ink);
        cover.stroke(spine[2], spine[1], neck, ink);
        cover.stroke(spine[1], spine[0], beak, ink);
    }
    seen
}

#[cfg(test)]
mod tests;
