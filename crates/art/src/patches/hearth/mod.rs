//! Hearth: a fire burning just below the bottom edge of the panel.
//!
//! The owner, 2026-10-20: "something akin to a fireplace ... it looks like
//! just below the bottom of the screen is a fire, and we're seeing
//! flames/smokes/embers with meaningful randomness/generative behavior to it."
//! Card 372.
//!
//! **What is simulated.** A real Eulerian fluid fire ([`fluid`]): fuel from a
//! bed of a few seeded logs, burning to heat, heat lifting the gas,
//! vorticity confinement keeping the curl at the flame tips, a pressure
//! projection keeping it incompressible. Its colour is blackbody emission
//! ([`ramp`]: Planck against the CIE 1931 matching functions, worked out once
//! on the CPU). Embers ([`embers`]) ride the same velocity field. Smoke is the
//! burnt fuel the sim carries up, lit only by the fire's own glow.
//!
//! **Why 2D, on the CPU.** The Studio that plays this is a Raspberry Pi 4
//! with no GPU (owner, via the orchestrator, 2026-10-20), so the budget is
//! about one core at 30 fps. A 2D grid at three cells per LED (192 x 112, the
//! bottom 16 rows below the frame) is what the panel can resolve and then
//! some: each LED is the average of nine cells, in linear light. A thin 3D
//! grid would add the soft depth of a ray-marched volume, but at 64 x 32 that
//! depth is a fraction of one LED's blur while costing the depth count over
//! again in every pass; the money is better spent on resolution and pressure
//! sweeps in 2D. See the card's Log.
//!
//! **Framing.** The fuel bed is below the frame. The flames usually reach the
//! lower third, at a swell perhaps half; above that is black, with the odd
//! ember climbing and a faint warm wisp of smoke just over the flames.
//!
//! **Time.** Everything steps on a fixed [`STEP`] from `ctx.dt` - the fluid,
//! the eased `size`, the breathing, the dice for gusts, embers and settling
//! logs - so a snapshot is the same picture every time, and at any frame rate.
//! The first frame is already a burning fire: [`WARMUP`] seconds are run
//! invisibly before it.
//!
//! **Colour** is painted by palette index ([`Hearth::paint`]): black, a
//! 24-step blackbody ramp chosen at even perceptual lightness, and four smoke
//! tints. 29 colours, so every frame is exact whatever it looks like.
//!
//! Named settings to offer (the card asks for two or three):
//!
//! - **Roaring**: `size` 1, `gusts` 0.5, `embers` 0.7, `smoke` 0.4.
//! - **Evening** (the defaults): `size` 0.5, `gusts` 0.4, `embers` 0.4,
//!   `smoke` 0.3.
//! - **Embers**: `size` 0, `gusts` 0.2, `embers` 0.25, `smoke` 0.15.
//!
//! Credit: the method is Stam, "Stable Fluids" (SIGGRAPH 1999); Fedkiw, Stam
//! and Jensen, "Visual Simulation of Smoke" (SIGGRAPH 2001); and Nguyen,
//! Fedkiw and Jensen, "Physically Based Modeling and Animation of Fire"
//! (SIGGRAPH 2002). The pipeline follows `olliep24/firesim`
//! (<https://github.com/olliep24/firesim>), the reference the owner pointed
//! at, studied but not copied - it has no licence.

mod embers;
mod fluid;
mod noise;
mod ramp;
#[cfg(test)]
mod tests;

use crate::color::{oklch, smoothstep, to_oklab, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use embers::Embers;
use fluid::{Drive, Fluid, Tune};
use ramp::Ramp;

pub const DEF: PatchDef = PatchDef {
    id: "hearth",
    name: "Hearth",
    blurb: "A fire just below the frame: flames licking up from a bed of logs, the odd ember climbing through black, faint fire-lit smoke.",
    params: PARAMS,
    make,
    // The seed lays the logs (how many, where, how big), and every wandering
    // strength, gust and spark follows from it: another seed is another fire.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("size", "How big the fire is (eases over tens of seconds)", 0.0, 1.0, 0.01, 0.5),
    param("gusts", "How often a draught leans the flames", 0.0, 1.0, 0.01, 0.4),
    param("embers", "How many sparks fly up", 0.0, 1.0, 0.01, 0.4),
    param("smoke", "How much smoke shows", 0.0, 1.0, 0.01, 0.3),
    param("color", "How much colour (0 is pure grayscale)", 0.0, 1.0, 0.01, 1.0),
];

/// Grid cells per LED, each way.
const CELLS: usize = 3;
const GW: usize = W * CELLS;
/// Rows of grid below the bottom edge of the frame: where the logs are.
const BELOW: usize = 10;
const GH: usize = H * CELLS + BELOW;
/// The rows fuel comes in at: the logs themselves.
const BED_ROWS: usize = 5;

/// The fixed step, seconds.
pub(crate) const STEP: f64 = 1.0 / 60.0;
/// Seconds of fire run before the first frame.
const WARMUP: f64 = 8.0;
/// After a stall, at most this many steps are caught up: a paused Studio
/// resumes the fire, it does not fast-forward it.
const CATCHUP: i64 = 30;
/// How long `size` takes to follow its parameter: the time constant, seconds.
const SIZE_EASE: f32 = 18.0;

const FIRE_STEPS: usize = 24;
const SMOKE_STEPS: usize = 4;
/// Darkest fire entry's lightness, OKLab; below half of it, black.
const FIRE_L0: f32 = 0.2;

struct Log {
    /// Centre and half-width, in cells.
    x: f32,
    half: f32,
    salt: u32,
    /// Offset into its own strength noise.
    phase: f32,
}

struct Gust {
    start: f64,
    length: f64,
    /// Signed peak, cells per second.
    peak: f32,
}

struct Settle {
    start: f64,
    log: usize,
    sparks: u32,
    next_spark: f64,
}

pub(crate) struct Hearth {
    fluid: Fluid,
    embers: Embers,
    logs: Vec<Log>,
    salt: u32,
    dice: Rng,
    /// Sim seconds, a whole number of steps.
    t: f64,
    steps: i64,
    clock: f64,
    warm: bool,
    /// The eased fire size the sim is burning at.
    size: f32,
    gust: Option<Gust>,
    calm_until: f64,
    settle: Option<Settle>,
    inject: Vec<f32>,
    coals: Vec<f32>,
    wind: Vec<f32>,
    ramp: Ramp,
    /// The fire ramp's temperatures and lightnesses, and the palette built
    /// from them for the last `color`.
    fire_l: [f32; FIRE_STEPS],
    fire_theta: [f32; FIRE_STEPS],
    palette: Option<(f32, Vec<Rgb>)>,
    // Counters for tests and `playing`.
    pub(crate) gusts_seen: u32,
    pub(crate) settles_seen: u32,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Hearth::new(seed))
}

impl Hearth {
    pub(crate) fn new(seed: u64) -> Hearth {
        let mut rng = Rng::new(seed ^ 0x4ea7_7a11);
        let salt = rng.u64() as u32;
        // Three or four logs across the width, never evenly spaced.
        let count = 3 + (rng.u64() % 2) as usize;
        let mut logs = Vec::with_capacity(count);
        for k in 0..count {
            let slot = (k as f32 + 0.5) / count as f32;
            logs.push(Log {
                x: GW as f32 * (0.12 + 0.76 * slot) + rng.range(-10.0, 10.0),
                half: rng.range(20.0, 32.0),
                salt: rng.u64() as u32,
                phase: rng.range(0.0, 100.0),
            });
        }
        let ramp = Ramp::new();
        let (fire_theta, fire_l) = fire_steps(&ramp);
        Hearth {
            fluid: Fluid::new(GW, GH),
            embers: Embers::new(rng.u64()),
            logs,
            salt,
            dice: Rng::new(rng.u64()),
            t: 0.0,
            steps: 0,
            clock: 0.0,
            warm: false,
            size: 0.5,
            gust: None,
            calm_until: 20.0,
            settle: None,
            inject: vec![0.0; GW * BED_ROWS],
            coals: vec![0.0; GW * BED_ROWS],
            wind: vec![0.0; GH],
            ramp,
            fire_l,
            fire_theta,
            palette: None,
            gusts_seen: 0,
            settles_seen: 0,
        }
    }

    /// The physical constants at the current eased size.
    fn tune(&self) -> Tune {
        let s = self.size;
        Tune {
            buoyancy: 95.0,
            smoke_weight: 6.0,
            drag: 0.9,
            wind_grip: 1.4,
            confine: 14.0,
            // A big fire's fuel lasts longer as it climbs: taller tongues.
            burn: 2.0 - 0.6 * s,
            fuel_heat: 1.5,
            // Size is mostly how fast the gas cools: how far it rises still glowing.
            cool4: 2.0 - 1.2 * s,
            cool1: 2.4 - 1.8 * s,
            expand: 1.5,
            smoke_yield: 0.9,
            smoke_fade: 0.55,
        }
    }

    /// The fire breathing by itself: slow swells and lulls over minutes.
    fn breath(&self) -> f32 {
        let t = self.t as f32;
        1.0 + 0.22 * noise::fbm1(t / 75.0, self.salt ^ 0xb4ea) + 0.1 * noise::fbm1(t / 23.0, self.salt ^ 0x77)
    }

    /// How strongly log `k` is burning now, before size and breath.
    fn log_strength(&self, k: usize) -> f32 {
        let log = &self.logs[k];
        let t = self.t as f32;
        let mut s = 0.6 + 0.8 * noise::noise1(t / 17.0 + log.phase, log.salt);
        if let Some(settle) = &self.settle {
            if settle.log == k {
                let age = (self.t - settle.start) as f32;
                s += 1.3 * smoothstep(0.0, 0.35, age) * (-(age / 2.2)).exp();
            }
        }
        s
    }

    /// One fixed step of everything.
    fn step(&mut self, ctx: &Ctx) {
        let dt = STEP as f32;
        let target = ctx.get("size");
        self.size += (target - self.size) * (1.0 - (-dt / SIZE_EASE).exp());
        self.events(ctx);

        // Fuel from the logs: each log's own bump, broken into tongues by
        // noise along its length that slowly drifts, so the tongues merge and
        // part instead of standing still.
        let t = self.t as f32;
        let breath = self.breath();
        let strengths: Vec<f32> = (0..self.logs.len()).map(|k| self.log_strength(k)).collect();
        for x in 0..GW {
            let xf = x as f32;
            let mut bed = 0.0;
            for (log, s) in self.logs.iter().zip(&strengths) {
                let d = (xf - log.x) / log.half;
                bed += s * (-d * d).exp();
            }
            let tongues = (2.2 * noise::fbm2(xf / 7.0, t * 0.7, self.salt)).max(0.0).powf(1.3);
            let lo = 0.45 - 0.25 * self.size;
            let rate = 1.5 * smoothstep(lo, lo + 0.6, bed * (0.35 + tongues) * breath);
            // The coals glow whatever the flames are doing, brightest where
            // the logs are, shimmering slowly along their length.
            let glow = 0.62 * (bed / 1.4).min(1.0) * (0.85 + 0.3 * noise::noise2(xf / 5.0, t * 0.3, self.salt ^ 0xc0a1));
            for y in 0..BED_ROWS {
                // Most in the middle of the log, a little at its edges.
                let k = 1.0 - (y as f32 - 2.0).abs() / 3.0;
                self.inject[y * GW + x] = rate * k;
                self.coals[y * GW + x] = glow * k;
            }
        }

        // The air: a calm wandering drift, and a gust when one is blowing,
        // stronger higher up and weakest down among the logs.
        let drift = 3.0 * noise::fbm1(t / 21.0, self.salt ^ 0xd1f7);
        let gust = self.gust.as_ref().map_or(0.0, |g| {
            let a = ((self.t - g.start) / g.length).clamp(0.0, 1.0) as f32;
            let env = (std::f32::consts::PI * a).sin().powi(2);
            g.peak * env * (1.0 + 0.25 * noise::fbm1(t * 0.9, self.salt ^ 0x6057))
        });
        for (y, w) in self.wind.iter_mut().enumerate() {
            *w = (drift + gust) * (0.3 + 0.7 * smoothstep(0.0, GH as f32 * 0.55, y as f32));
        }

        let tune = self.tune();
        self.fluid.step(dt, &tune, &Drive { inject: &self.inject, coals: &self.coals, bed_rows: BED_ROWS, wind: &self.wind });
        self.embers.step(dt, &self.fluid, GH as f32 + 2.0);
        self.t += STEP;
        self.steps += 1;
    }

    /// The dice: gusts, log settles, and single sparks.
    fn events(&mut self, ctx: &Ctx) {
        let dt = STEP;
        let gusts = ctx.get("gusts");
        if self.gust.as_ref().is_some_and(|g| self.t > g.start + g.length) {
            self.gust = None;
        }
        // A mean wait of four minutes at 0, rising to one every half minute
        // at 1, and none at all when it is turned right off.
        if gusts > 0.0 && self.gust.is_none() && self.t > self.calm_until {
            let mean = 240.0 * (30.0f64 / 240.0).powf(f64::from(gusts));
            if f64::from(self.dice.f32()) < dt / mean {
                let peak = self.dice.range(14.0, 26.0) * (0.6 + 0.8 * gusts) * self.dice.sign();
                let length = f64::from(self.dice.range(4.5, 9.0));
                self.gust = Some(Gust { start: self.t, length, peak });
                self.calm_until = self.t + length + 15.0;
                self.gusts_seen += 1;
            }
        }

        // A log settles now and then - a short flare and a handful of sparks.
        if self.settle.as_ref().is_some_and(|s| self.t > s.start + 8.0) {
            self.settle = None;
        }
        if self.settle.is_none() && f64::from(self.dice.f32()) < dt / 200.0 && self.t > 4.0 {
            let log = (self.dice.u64() % self.logs.len() as u64) as usize;
            let sparks = 3 + (ctx.get("embers") * 6.0).round() as u32;
            self.settle = Some(Settle { start: self.t, log, sparks, next_spark: self.t + 0.25 });
            self.settles_seen += 1;
        }
        if let Some(s) = &mut self.settle {
            if s.sparks > 0 && self.t >= s.next_spark {
                s.sparks -= 1;
                s.next_spark = self.t + f64::from(self.dice.range(0.05, 0.25));
                let log = &self.logs[s.log];
                let x = log.x + self.dice.range(-0.5, 0.5) * log.half;
                self.embers.spawn(x, BED_ROWS as f32 + 3.0, 1.0);
            }
        }

        // Single sparks: on average one every six seconds or so at the
        // default, fewer from a small fire.
        let vigour = (0.25 + self.size).min(1.0) * self.breath();
        let rate = f64::from(ctx.get("embers") * 0.42 * vigour);
        if f64::from(self.dice.f32()) < dt * rate {
            let weights: Vec<f32> = (0..self.logs.len()).map(|k| self.log_strength(k)).collect();
            let total: f32 = weights.iter().sum();
            let mut pick = self.dice.f32() * total;
            let mut which = 0;
            for (k, w) in weights.iter().enumerate() {
                which = k;
                if pick < *w {
                    break;
                }
                pick -= w;
            }
            let log = &self.logs[which];
            let x = log.x + self.dice.range(-0.6, 0.6) * log.half;
            self.embers.spawn(x, BED_ROWS as f32 + 3.0, vigour.min(1.0));
        }
    }

    fn advance(&mut self, ctx: &Ctx) {
        if !self.warm {
            self.warm = true;
            self.size = ctx.get("size");
            let warm_steps = (WARMUP / STEP).round() as i64;
            for _ in 0..warm_steps {
                self.step(ctx);
            }
            // The embers and events of the warm-up are not seen; the clock
            // the frame reads restarts from here.
            self.steps = 0;
        }
        self.clock += ctx.dt.clamp(0.0, 0.25);
        let target = (self.clock / STEP + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.step(ctx);
        }
    }

    /// Fire light per LED, linear: nine cells averaged, embers added.
    fn light(&self) -> Vec<Rgb> {
        let mut px = vec![Rgb::BLACK; N];
        let norm = 1.0 / (CELLS * CELLS) as f32;
        for ly in 0..H {
            // Panel row 0 is the top; grid row 0 the bottom.
            let gy0 = BELOW + (H - 1 - ly) * CELLS;
            for lx in 0..W {
                let mut acc = Rgb::BLACK;
                for dy in 0..CELLS {
                    let row = (gy0 + dy) * GW + lx * CELLS;
                    for dx in 0..CELLS {
                        acc = acc.add(self.ramp.emit(self.fluid.temp[row + dx]));
                    }
                }
                px[ly * W + lx] = acc.scale(norm);
            }
        }
        for e in &self.embers.live {
            self.draw_ember(&mut px, e);
        }
        px
    }

    /// An ember as a soft, sub-pixel point, smeared along the last step's
    /// motion so it never twinkles as it crosses an LED boundary.
    fn draw_ember(&self, px: &mut [Rgb], e: &embers::Ember) {
        const SIGMA: f32 = 0.42;
        const TAPS: usize = 3;
        const SUB: usize = 3;
        // The peak LED reads about as bright as gas at the ember's heat.
        let colour = self.ramp.emit(e.heat).scale(2.1 / TAPS as f32);
        let to_led = |x: f32, y: f32| (x / CELLS as f32, H as f32 - (y - BELOW as f32) / CELLS as f32);
        for k in 0..TAPS {
            let f = (k as f32 + 0.5) / TAPS as f32;
            let (cx, cy) = to_led(e.px + (e.x - e.px) * f, e.py + (e.y - e.py) * f);
            let (x0, x1) = ((cx - 2.0).floor().max(0.0) as usize, ((cx + 2.0).ceil().max(0.0) as usize).min(W));
            let (y0, y1) = ((cy - 2.0).floor().max(0.0) as usize, ((cy + 2.0).ceil().max(0.0) as usize).min(H));
            for ly in y0..y1 {
                for lx in x0..x1 {
                    let mut cover = 0.0;
                    for sy in 0..SUB {
                        for sx in 0..SUB {
                            let dx = lx as f32 + (sx as f32 + 0.5) / SUB as f32 - cx;
                            let dy = ly as f32 + (sy as f32 + 0.5) / SUB as f32 - cy;
                            cover += (-(dx * dx + dy * dy) / (2.0 * SIGMA * SIGMA)).exp();
                        }
                    }
                    let c = &mut px[ly * W + lx];
                    *c = c.add(colour.scale(cover / (SUB * SUB) as f32));
                }
            }
        }
    }

    /// Smoke per LED, lit by the fire around it: the smoke the sim carries,
    /// times a wide blur of the fire's own light. Nothing far from the fire
    /// is lit, because nothing else is a light.
    fn smoke_light(&self, light: &[Rgb], amount: f32) -> Vec<f32> {
        let mut glow: Vec<f32> = light.iter().map(|c| c.luma()).collect();
        blur(&mut glow, 6);
        blur(&mut glow, 6);
        let norm = 1.0 / (CELLS * CELLS) as f32;
        let mut out = vec![0.0; N];
        for ly in 0..H {
            let gy0 = BELOW + (H - 1 - ly) * CELLS;
            for lx in 0..W {
                let mut s = 0.0;
                for dy in 0..CELLS {
                    let row = (gy0 + dy) * GW + lx * CELLS;
                    s += self.fluid.smoke[row..row + CELLS].iter().sum::<f32>();
                }
                let i = ly * W + lx;
                out[i] = amount * 2.2 * (s * norm).min(1.0) * (glow[i] * 9.0).min(1.0);
            }
        }
        out
    }

    fn palette(&mut self, color: f32) -> Vec<Rgb> {
        if let Some((c, p)) = &self.palette {
            if *c == color {
                return p.clone();
            }
        }
        let mut pal = vec![Rgb::BLACK];
        for theta in self.fire_theta {
            let lab = to_oklab(self.ramp.emit(theta));
            let (c, h) = ((lab[1] * lab[1] + lab[2] * lab[2]).sqrt(), lab[2].atan2(lab[1]).to_degrees());
            pal.push(oklch(lab[0], c * color, h));
        }
        for k in 0..SMOKE_STEPS {
            let l = 0.16 + 0.04 * k as f32;
            pal.push(oklch(l, 0.05 * color, 38.0));
        }
        self.palette = Some((color, pal.clone()));
        pal
    }

    /// The frame, by palette index: fire where there is fire, smoke where
    /// there is lit smoke and no fire, black everywhere else.
    fn paint(&mut self, ctx: &Ctx) -> Frame {
        let light = self.light();
        let smoke = self.smoke_light(&light, ctx.get("smoke"));
        let palette = self.palette(ctx.get("color"));
        let dither = Dither::BlueNoise;
        let indices = (0..N)
            .map(|i| {
                let thr = dither.threshold(i % W, i / W);
                let l = to_oklab(light[i])[0];
                let p = fire_position(&self.fire_l, l);
                if p >= 0.55 {
                    let k = (p + 0.5 + thr).floor().clamp(1.0, FIRE_STEPS as f32) as u8;
                    return k;
                }
                let q = smoke[i] * SMOKE_STEPS as f32;
                if q >= 0.6 {
                    let k = (q + 0.5 + thr).floor().clamp(1.0, SMOKE_STEPS as f32) as usize;
                    return (FIRE_STEPS + k) as u8;
                }
                0
            })
            .collect();
        Frame::Indexed { palette, indices }
    }

    /// For tests: how many embers are alive.
    #[cfg(test)]
    pub(crate) fn ember_count(&self) -> usize {
        self.embers.live.len()
    }

    #[cfg(test)]
    pub(crate) fn eased_size(&self) -> f32 {
        self.size
    }

    #[cfg(test)]
    pub(crate) fn settling(&self) -> bool {
        self.settle.is_some()
    }
}

/// Where lightness `l` falls along the fire ramp: 0 is black, 1 the first
/// entry, `FIRE_STEPS` the last, fractional in between.
fn fire_position(fire_l: &[f32; FIRE_STEPS], l: f32) -> f32 {
    if l <= fire_l[0] {
        return l / fire_l[0];
    }
    for k in 1..FIRE_STEPS {
        if l < fire_l[k] {
            return k as f32 + (l - fire_l[k - 1]) / (fire_l[k] - fire_l[k - 1]);
        }
    }
    FIRE_STEPS as f32
}

/// The fire ramp's entries: temperatures at even OKLab lightness from
/// [`FIRE_L0`] up to the hottest gas.
fn fire_steps(ramp: &Ramp) -> ([f32; FIRE_STEPS], [f32; FIRE_STEPS]) {
    let l_of = |theta: f32| to_oklab(ramp.emit(theta))[0];
    let top = l_of(1.0);
    let mut thetas = [0.0; FIRE_STEPS];
    let mut ls = [0.0; FIRE_STEPS];
    for k in 0..FIRE_STEPS {
        let want = FIRE_L0 + (top - FIRE_L0) * k as f32 / (FIRE_STEPS - 1) as f32;
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..30 {
            let mid = 0.5 * (lo + hi);
            if l_of(mid) < want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        thetas[k] = hi;
        ls[k] = l_of(hi);
    }
    (thetas, ls)
}

/// A box blur of radius `r`, both ways, edges averaged over what is there.
fn blur(v: &mut [f32], r: usize) {
    let mut tmp = vec![0.0; N];
    for y in 0..H {
        for x in 0..W {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(W));
            let s: f32 = v[y * W + a..y * W + b].iter().sum();
            tmp[y * W + x] = s / (b - a) as f32;
        }
    }
    for x in 0..W {
        for y in 0..H {
            let (a, b) = (y.saturating_sub(r), (y + r + 1).min(H));
            let s: f32 = (a..b).map(|j| tmp[j * W + x]).sum();
            v[y * W + x] = s / (b - a) as f32;
        }
    }
}

impl Patch for Hearth {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.advance(ctx);
        self.paint(ctx)
    }

    fn playing(&self) -> Option<Playing> {
        let detail = if self.settle.is_some() {
            "a log settles".to_string()
        } else if let Some(g) = &self.gust {
            format!("a draught from the {}", if g.peak > 0.0 { "left" } else { "right" })
        } else {
            "burning quietly".to_string()
        };
        Some(Playing {
            title: format!("a fire of {} logs", self.logs.len()),
            detail,
            actions: Vec::new(),
            notes: Vec::new(),
        })
    }
}
