//! Hearth: a fire burning just below the bottom edge of the panel.
//!
//! The owner, 2026-10-20: "something akin to a fireplace ... it looks like
//! just below the bottom of the screen is a fire, and we're seeing
//! flames/smokes/embers with meaningful randomness/generative behavior to it."
//! And, on how: "i want maximum fire realism". Card 372.
//!
//! **What is simulated.** A real 3D Eulerian fluid fire, stepped in wgpu
//! compute shaders ([`gpu`], `hearth.wgsl`): fuel from a bed of a few seeded
//! logs, burning to heat and an expansion at the flame (Nguyen, Fedkiw and
//! Jensen), heat lifting the gas, vorticity confinement keeping the curl at
//! the flame tips, MacCormack advection keeping them sharp, a pressure
//! projection keeping it incompressible. It is seen by ray-marching the
//! volume, many rays per LED: emission is blackbody ([`ramp`]: Planck against
//! the CIE 1931 matching functions, worked out once on the CPU into a table),
//! and the smoke absorbs it and is lit only by the fire's own light, gathered
//! onto a coarse grid and spread. Embers are GPU particles in the same
//! velocity field.
//!
//! **The grid is shaped to the shot**: 192 x 128 x 48 cells, three across per
//! LED, the logs in the bottom rows below the frame, and a camera a little
//! low and a long way back, looking a touch up.
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
//! and Jensen, "Visual Simulation of Smoke" (SIGGRAPH 2001); Nguyen, Fedkiw
//! and Jensen, "Physically Based Modeling and Animation of Fire" (SIGGRAPH
//! 2002); and Selle et al., "An Unconditionally Stable MacCormack Method"
//! (2008). The pipeline follows `olliep24/firesim`
//! (<https://github.com/olliep24/firesim>), the reference the owner pointed
//! at, studied but not copied - it has no licence.

mod gpu;
mod noise;
mod ramp;
#[cfg(test)]
mod tests;

use crate::color::{oklch, smoothstep, to_oklab, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::gpu::Gpu;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use gpu::{Params, Sim, EMBERS};
use ramp::Ramp;

pub const DEF: PatchDef = PatchDef {
    id: "hearth",
    name: "Hearth",
    blurb: "GPU. A 3D fluid fire just below the frame: flames licking up from a bed of logs, the odd ember climbing through black, faint fire-lit smoke.",
    params: PARAMS,
    make,
    // The seed lays the logs (how many, where, how big), and every wandering
    // strength, gust and spark follows from it: another seed is another fire.
    seeded: true,
};

// Card 373: the owner liked card 372's fire best with these sliders all the
// way up, so card 372's maximum is now about [`OLD_MAX`] of each range, the
// default sits there, and the rest of the range is headroom above it.
const PARAMS: &[ParamSpec] = &[
    param("size", "How big the fire is (eases over tens of seconds)", 0.0, 1.0, 0.01, 0.6),
    param("gusts", "How often the draught surges", 0.0, 1.0, 0.01, 0.6),
    param("embers", "How many sparks fly up", 0.0, 1.0, 0.01, 0.6),
    param("smoke", "How much smoke shows", 0.0, 1.0, 0.01, 0.3),
    // Card 373: the panel as the inside of a wood stove's firebox, its top
    // edge the ceiling, a chimney flue in the middle of it.
    param("draft", "How hard the chimney draws (0: an open fire, no stove)", 0.0, 1.0, 0.01, 0.7),
    param("flue", "How wide the flue is", 0.0, 1.0, 0.01, 0.4),
    param("color", "How much colour (0 is pure grayscale)", 0.0, 1.0, 0.01, 1.0),
];

/// Where card 372's maximum sits on the rescaled `size`, `gusts` and
/// `embers` (card 373): a slider at this value behaves as card 372's did at
/// 1, and the range runs on to 1 / `OLD_MAX` of it.
const OLD_MAX: f32 = 0.6;

/// A slider in card 372's units: 1 is its old maximum.
fn intensity(ctx: &Ctx, id: &str) -> f32 {
    ctx.get(id) / OLD_MAX
}

/// Grid cells per LED across the panel.
const CELLS: usize = 3;
const GW: usize = W * CELLS;
const GH: usize = 128;
const GD: usize = 48;
/// The grid row the bottom edge of the frame sits on (at the middle depth):
/// the logs are below it.
const BELOW: usize = 9;
/// The firebox ceiling (card 373): the grid row at the frame's top edge, at
/// the middle depth. Only with a draft.
const CEILING: usize = BELOW + 3 * H;
/// The rows fuel comes in at: the logs themselves.
const BED_ROWS: usize = 5;
/// Supersamples per LED, each way.
const SAMPLES: u32 = 5;
/// Ray-march step, cells.
const MARCH_STEP: f32 = 0.5;
/// How far back the camera stands from the middle of the hearth, cells.
const DISTANCE: f32 = 340.0;
/// How far below the frame's middle the camera's eye is: a little low,
/// looking a touch up.
const EYE_DROP: f32 = 22.0;
/// An ember slot at or below this heat is empty.
const EMBER_OUT: f32 = 0.30;

/// The fixed step, seconds.
pub(crate) const STEP: f64 = 1.0 / 30.0;
/// Seconds of fire run before the first frame.
const WARMUP: f64 = 5.0;
/// After a stall, at most this many steps are caught up: a paused Studio
/// resumes the fire, it does not fast-forward it.
const CATCHUP: i64 = 15;
/// How long `size` takes to follow its parameter: the time constant, seconds.
const SIZE_EASE: f32 = 18.0;

const FIRE_STEPS: usize = 24;
const SMOKE_STEPS: usize = 4;
/// Darkest fire entry's lightness, OKLab; below half of it, black.
const FIRE_L0: f32 = 0.2;
/// Soot that has taken more than this share of the fire behind it paints
/// that LED black.
const SOOT_BLACK: f32 = 0.6;
/// The soot greys, OKLab lightness, darkest first. Neutral: no chroma.
const SMOKE_L: [f32; SMOKE_STEPS] = [0.2, 0.25, 0.3, 0.35];
/// Lit smoke, as the march returns it, to a step on the smoke ramp at
/// `smoke` 1.
const SMOKE_GAIN: f32 = 20.0;

struct Log {
    /// Centre and half-width across, in cells, and the same into the hearth.
    x: f32,
    half: f32,
    z: f32,
    zhalf: f32,
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

/// The camera, worked out once: where it is, its unit axes, and the
/// tangents of half the frame each way.
struct Camera {
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
    tan_w: f32,
    tan_h: f32,
}

impl Camera {
    fn new() -> Camera {
        let centre = [GW as f32 * 0.5, BELOW as f32 + 1.5 * H as f32, GD as f32 * 0.5];
        let eye = [centre[0], centre[1] - EYE_DROP, centre[2] - DISTANCE];
        let d = [centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let fwd = d.map(|v| v / len);
        // right = world up x fwd, normalised; up = fwd x right.
        let r = [fwd[2], 0.0, -fwd[0]];
        let rl = (r[0] * r[0] + r[2] * r[2]).sqrt();
        let right = r.map(|v| v / rl);
        let up = [
            fwd[1] * right[2] - fwd[2] * right[1],
            fwd[2] * right[0] - fwd[0] * right[2],
            fwd[0] * right[1] - fwd[1] * right[0],
        ];
        Camera { eye, right, up, fwd, tan_w: 0.5 * GW as f32 / len, tan_h: 1.5 * H as f32 / len }
    }

    /// A grid position on the panel, in LED coordinates.
    fn project(&self, p: [f32; 3]) -> Option<(f32, f32)> {
        let d = [p[0] - self.eye[0], p[1] - self.eye[1], p[2] - self.eye[2]];
        let dot = |a: [f32; 3]| a[0] * d[0] + a[1] * d[1] + a[2] * d[2];
        let z = dot(self.fwd);
        if z < 1.0 {
            return None;
        }
        let u = dot(self.right) / (z * self.tan_w);
        let v = dot(self.up) / (z * self.tan_h);
        Some(((u + 1.0) * 0.5 * W as f32, (1.0 - v) * 0.5 * H as f32))
    }
}

pub(crate) struct Hearth {
    /// `None` until first render; `Some(None)` if there is no GPU.
    sim: Option<Option<Sim>>,
    camera: Camera,
    /// The ember slots as last read back (three vec4s each), the slots
    /// filled since, and the sparks waiting to go in with the next step.
    sparks: Vec<[f32; 4]>,
    pending: Vec<usize>,
    spawns: Vec<(usize, [[f32; 4]; 3])>,
    spark_dice: Rng,
    logs: Vec<Log>,
    salt: u32,
    dice: Rng,
    /// Sim seconds, a whole number of steps.
    t: f64,
    steps: i64,
    /// Steps since the patch began, for the shader's per-step dice.
    step_no: u64,
    clock: f64,
    warm: bool,
    /// The eased fire size the sim is burning at.
    size: f32,
    /// Soot absorption per cell per unit of smoke.
    soot: f32,
    /// The chimney's pull this step, surges included (0: an open fire), and
    /// the flue's radius in cells.
    draft: f32,
    flue: f32,
    gust: Option<Gust>,
    calm_until: f64,
    settle: Option<Settle>,
    bed: Vec<[f32; 2]>,
    wind: [[f32; 4]; 40],
    ramp: Ramp,
    /// The fire ramp's temperatures and lightnesses, and the palette built
    /// from them for the last `color`.
    fire_l: [f32; FIRE_STEPS],
    fire_theta: [f32; FIRE_STEPS],
    palette: Option<(f32, Vec<Rgb>)>,
    // Counters for tests and `playing`.
    pub(crate) gusts_seen: u32,
    pub(crate) settles_seen: u32,
    pub(crate) sparks_thrown: u32,
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
                z: GD as f32 * 0.5 + rng.range(-7.0, 7.0),
                zhalf: rng.range(4.0, 7.0),
                salt: rng.u64() as u32,
                phase: rng.range(0.0, 100.0),
            });
        }
        let ramp = Ramp::new();
        let (fire_theta, fire_l) = fire_steps(&ramp);
        Hearth {
            sim: None,
            camera: Camera::new(),
            sparks: vec![[0.0; 4]; EMBERS * 3],
            pending: Vec::new(),
            spawns: Vec::new(),
            spark_dice: Rng::new(rng.u64()),
            logs,
            salt,
            dice: Rng::new(rng.u64()),
            t: 0.0,
            steps: 0,
            step_no: 0,
            clock: 0.0,
            warm: false,
            size: 0.5,
            soot: 0.28,
            draft: 0.0,
            flue: 13.0,
            gust: None,
            calm_until: 20.0,
            settle: None,
            bed: vec![[0.0; 2]; GW * GD],
            wind: [[0.0; 4]; 40],
            ramp,
            fire_l,
            fire_theta,
            palette: None,
            gusts_seen: 0,
            settles_seen: 0,
            sparks_thrown: 0,
        }
    }

    /// The uniform block for a step at the current eased size: the physical
    /// constants (cells and seconds, tuned by eye), the camera, the wind.
    fn params(&self) -> Params {
        let s = self.size / OLD_MAX;
        // Card 372's cooling curve up to its old maximum, and on down past
        // it for the headroom: a hotter, taller fire.
        let (cool4, cool1) = if s <= 1.0 {
            (0.9 + 1.5 * (1.0 - s) * (1.0 - s), 0.7 + 2.3 * (1.0 - s) * (1.0 - s))
        } else {
            (0.9 - 0.3 * (s - 1.0), 0.7 - 0.25 * (s - 1.0))
        };
        let c = &self.camera;
        let scale = |v: [f32; 3], k: f32| [v[0] * k, v[1] * k, v[2] * k, 0.0];
        Params {
            dims: [GW as u32, GH as u32, GD as u32, self.step_no as u32],
            coarse: [(GW / 4) as u32, (GH / 4) as u32, (GD / 4) as u32, EMBERS as u32],
            // dt, buoyancy, smoke weight, drag
            a: [STEP as f32, 95.0, 1.0, 0.9],
            // wind grip, confinement, burn, fuel heat
            b: [1.4, 5.0, 2.0 - 0.6 * s, 1.5],
            // Size is mostly how fast the gas cools: how far it rises still
            // glowing. cool4, cool1, expansion, smoke yield.
            c: [cool4, cool1, 1.5, 0.5],
            // smoke fade, smoke absorption, smoke lighting, emission
            d: [0.55, self.soot, 1.0, 0.4],
            eye: [c.eye[0], c.eye[1], c.eye[2], SAMPLES as f32],
            right: scale(c.right, c.tan_w),
            up: scale(c.up, c.tan_h),
            fwd: [c.fwd[0], c.fwd[1], c.fwd[2], MARCH_STEP],
            stove: [self.draft, CEILING as f32, self.flue, 0.0],
            wind: self.wind,
        }
    }

    /// The fire breathing by itself: slow swells and lulls over minutes.
    fn breath(&self) -> f32 {
        let t = self.t as f32;
        1.0 + 0.12 * noise::fbm1(t / 75.0, self.salt ^ 0xb4ea) + 0.06 * noise::fbm1(t / 23.0, self.salt ^ 0x77)
    }

    /// How strongly log `k` is burning now, before size and breath.
    fn log_strength(&self, k: usize) -> f32 {
        let log = &self.logs[k];
        let t = self.t as f32;
        let mut s = 0.85 + 0.3 * noise::noise1(t / 17.0 + log.phase, log.salt);
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
        // How strongly soot absorbs the flame behind it: the smoke slider.
        self.soot = 0.08 + 0.25 * ctx.get("smoke");
        let target = ctx.get("size");
        self.size += (target - self.size) * (1.0 - (-dt / SIZE_EASE).exp());
        self.events(ctx);

        // Fuel from the logs: each log's own bump, broken into tongues by
        // noise over the bed that slowly drifts, so the tongues merge and
        // part instead of standing still. The bed is held at a fuel *level*
        // where it burns (see the card's Log: a fuel rate is far too
        // touchy), and the coals glow whatever the flames are doing.
        let t = self.t as f32;
        let breath = self.breath();
        let strengths: Vec<f32> = (0..self.logs.len()).map(|k| self.log_strength(k)).collect();
        let lo = 0.55 - 0.45 * self.size / OLD_MAX;
        for z in 0..GD {
            let zf = z as f32;
            for x in 0..GW {
                let xf = x as f32;
                let mut bed = 0.0;
                for (log, s) in self.logs.iter().zip(&strengths) {
                    let (dx, dz) = ((xf - log.x) / log.half, (zf - log.z) / log.zhalf);
                    bed += s * (-dx * dx - dz * dz).exp();
                }
                if bed < 0.02 {
                    self.bed[z * GW + x] = [0.0, 0.0];
                    continue;
                }
                let tongues = (0.2 + 1.8 * noise::fbm3(xf / 7.0, zf / 7.0, t * 0.7, self.salt)).max(0.0).powf(1.2);
                let level = 1.5 * smoothstep(lo, lo + 0.6, bed * (0.35 + tongues) * breath);
                let shimmer = 0.85 + 0.3 * noise::noise3(xf / 5.0, zf / 5.0, t * 0.3, self.salt ^ 0xc0a1);
                self.bed[z * GW + x] = [level, 0.75 * (bed / 1.4).min(1.0) * shimmer];
            }
        }

        // The air: a calm wandering drift, and a gust when one is blowing,
        // stronger higher up and weakest down among the logs.
        // In a stove the walls stop any wind: a gust is the chimney drawing
        // harder for a few seconds (the stove breathing), and the air only
        // wanders a little.
        let draft = ctx.get("draft");
        let stove = draft > 0.0;
        let drift = 3.0 * noise::fbm1(t / 21.0, self.salt ^ 0xd1f7) * if stove { 0.3 } else { 1.0 };
        let (gust, surge) = self.gust.as_ref().map_or((0.0, 0.0), |g| {
            let a = ((self.t - g.start) / g.length).clamp(0.0, 1.0) as f32;
            let env = (std::f32::consts::PI * a).sin().powi(2) * (1.0 + 0.25 * noise::fbm1(t * 0.9, self.salt ^ 0x6057));
            (g.peak * env, env * g.peak.abs() / 18.0)
        });
        let gust = if stove { 0.0 } else { gust };
        for y in 0..GH {
            self.wind[y / 4][y % 4] = (drift + gust) * (0.1 + 0.9 * smoothstep(BELOW as f32, GH as f32 * 0.6, y as f32));
        }
        self.draft = if stove { draft * (1.0 + 0.9 * surge) } else { 0.0 };
        self.flue = 6.0 + 18.0 * ctx.get("flue");

        let params = self.params();
        if let Some(Some(sim)) = &self.sim {
            for (slot, spark) in self.spawns.drain(..) {
                sim.spawn(slot, spark);
            }
            sim.step(&params, &self.bed);
        }
        self.t += STEP;
        self.steps += 1;
        self.step_no += 1;
    }

    /// Throw a spark up from `(x, z)` on the bed into a free slot, if there
    /// is one. `vigour` in `0..1` is how hard its fire is burning.
    fn spawn(&mut self, x: f32, z: f32, vigour: f32) {
        self.sparks_thrown += 1;
        let free = (0..EMBERS).find(|k| self.sparks[3 * k][3] <= EMBER_OUT && !self.pending.contains(k));
        let Some(slot) = free else { return };
        let r = &mut self.spark_dice;
        let pos = [x + r.range(-3.0, 3.0), BED_ROWS as f32 + 3.0, z + r.range(-2.0, 2.0)];
        let heat = r.range(0.88, 1.0);
        let v = [r.range(-12.0, 12.0), r.range(30.0, 55.0) * (0.7 + 0.5 * vigour), r.range(-6.0, 6.0)];
        let cooling = r.range(0.16, 0.30);
        self.pending.push(slot);
        // Until the next read-back says otherwise, the slot is this spark.
        self.sparks[3 * slot] = [pos[0], pos[1], pos[2], heat];
        self.sparks[3 * slot + 2] = [pos[0], pos[1], pos[2], 0.0];
        self.spawns.push((slot, [[pos[0], pos[1], pos[2], heat], [v[0], v[1], v[2], cooling], [pos[0], pos[1], pos[2], 0.0]]));
    }

    /// The dice: gusts, log settles, and single sparks.
    fn events(&mut self, ctx: &Ctx) {
        let dt = STEP;
        let gusts = intensity(ctx, "gusts");
        if self.gust.as_ref().is_some_and(|g| self.t > g.start + g.length) {
            self.gust = None;
        }
        // A mean wait of four minutes near 0, one every half minute at card
        // 372's maximum (now the default), and more often above it; none at
        // all when it is turned right off.
        if gusts > 0.0 && self.gust.is_none() && self.t > self.calm_until {
            let mean = 240.0 * (30.0f64 / 240.0).powf(f64::from(gusts));
            if f64::from(self.dice.f32()) < dt / mean {
                let peak = self.dice.range(11.0, 18.0) * (0.6 + 0.8 * gusts.min(1.4)) * self.dice.sign();
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
        // A mean wait of 150 s at the default, about 100 s at the top.
        let embers = intensity(ctx, "embers");
        let settle_mean = 150.0 / (0.3 + 0.7 * f64::from(embers));
        if self.settle.is_none() && f64::from(self.dice.f32()) < dt / settle_mean && self.t > 4.0 {
            let log = (self.dice.u64() % self.logs.len() as u64) as usize;
            let sparks = 3 + (embers * 4.0).round() as u32;
            self.settle = Some(Settle { start: self.t, log, sparks, next_spark: self.t + 0.25 });
            self.settles_seen += 1;
        }
        let mut burst = None;
        if let Some(s) = &mut self.settle {
            if s.sparks > 0 && self.t >= s.next_spark {
                s.sparks -= 1;
                s.next_spark = self.t + f64::from(self.dice.range(0.05, 0.25));
                let log = &self.logs[s.log];
                burst = Some((log.x + self.dice.range(-0.5, 0.5) * log.half, log.z));
            }
        }
        if let Some((x, z)) = burst {
            self.spawn(x, z, 1.0);
        }

        // Single sparks: on average one every six seconds or so at the
        // default, fewer from a small fire.
        let vigour = (0.25 + self.size / OLD_MAX).min(1.6) * self.breath();
        let rate = f64::from(embers * 0.42 * vigour);
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
            let (x, z) = (log.x + self.dice.range(-0.6, 0.6) * log.half, log.z);
            self.spawn(x, z, vigour.min(1.0));
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
            // The clock the frame reads restarts from here.
            self.steps = 0;
        }
        self.clock += ctx.dt.clamp(0.0, 0.25);
        let target = (self.clock / STEP + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.step(ctx);
        }
    }

    /// An ember as a soft, sub-pixel point, smeared along the last step's
    /// motion so it never twinkles as it crosses an LED boundary. `spark` is
    /// its three slot vec4s.
    fn draw_ember(&self, px: &mut [Rgb], spark: &[[f32; 4]]) {
        const SIGMA: f32 = 0.42;
        const TAPS: usize = 3;
        const SUB: usize = 3;
        let (now, prev) = (spark[0], spark[2]);
        // The peak LED reads about as bright as gas at the ember's heat.
        let colour = self.ramp.emit(now[3]).scale(2.6 / TAPS as f32);
        for k in 0..TAPS {
            let f = (k as f32 + 0.5) / TAPS as f32;
            let at = [0, 1, 2].map(|c| prev[c] + (now[c] - prev[c]) * f);
            let Some((cx, cy)) = self.camera.project(at) else { continue };
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
        // Soot: neutral greys, its own entries, never the flame's hue
        // (card 373: fire-tinted smoke read as a muddy dark red).
        for k in 0..SMOKE_STEPS {
            pal.push(oklch(SMOKE_L[k], 0.0, 0.0));
        }
        self.palette = Some((color, pal.clone()));
        pal
    }

    /// The frame, by palette index: fire where there is fire, smoke where
    /// there is lit smoke and no fire, black everywhere else. `leds` is the
    /// march's (fire light rgb, lit smoke) per LED.
    fn paint(&mut self, ctx: &Ctx, leds: &[[f32; 4]]) -> Frame {
        let (leds, bare) = leds.split_at(N);
        let mut light: Vec<Rgb> = leds.iter().map(|l| Rgb::new(l[0], l[1], l[2])).collect();
        // How much of each LED's fire the soot in front of it has taken.
        let soot: Vec<f32> =
            light.iter().zip(bare).map(|(c, b)| if b[0] > 1e-4 { (1.0 - c.luma() / b[0]).clamp(0.0, 1.0) } else { 0.0 }).collect();
        for k in 0..EMBERS {
            if self.sparks[3 * k][3] > EMBER_OUT {
                let spark = self.sparks[3 * k..3 * k + 3].to_vec();
                self.draw_ember(&mut light, &spark);
            }
        }
        let amount = ctx.get("smoke");
        let palette = self.palette(ctx.get("color"));
        let dither = Dither::BlueNoise;
        let indices = (0..N)
            .map(|i| {
                let thr = dither.threshold(i % W, i / W);
                let l = to_oklab(light[i])[0];
                let p = fire_position(&self.fire_l, l);
                // Flame seen through thick soot goes to black, not to the
                // ramp's dark reds: smoke on this panel is a silhouette.
                if soot[i] > SOOT_BLACK && p < 0.5 * FIRE_STEPS as f32 {
                    return 0;
                }
                if p >= 0.55 {
                    return (p + 0.5 + thr).floor().clamp(1.0, FIRE_STEPS as f32) as u8;
                }
                let q = amount * SMOKE_GAIN * leds[i][3] * SMOKE_STEPS as f32;
                if q >= 0.6 && p < 0.25 {
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
        (0..EMBERS).filter(|k| self.sparks[3 * k][3] > EMBER_OUT).count()
    }

    #[cfg(test)]
    pub(crate) fn eased_size(&self) -> f32 {
        self.size
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

impl Patch for Hearth {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        if self.sim.is_none() {
            self.sim = Some(match Gpu::shared() {
                Ok(gpu) => Some(Sim::new(gpu, [GW as u32, GH as u32, GD as u32], &self.ramp.table())),
                Err(e) => {
                    eprintln!("screeny-art: hearth: {e}; rendering black");
                    None
                }
            });
        }
        if !matches!(self.sim, Some(Some(_))) {
            return Frame::black();
        }
        self.advance(ctx);
        let params = self.params();
        let Some(Some(sim)) = &self.sim else { return Frame::black() };
        let Some((leds, sparks)) = sim.picture(&params) else { return Frame::black() };
        self.sparks = sparks;
        self.pending.clear();
        self.paint(ctx, &leds)
    }

    fn playing(&self) -> Option<Playing> {
        let detail = if self.settle.is_some() {
            "a log settles".to_string()
        } else if let Some(g) = &self.gust {
            if self.draft > 0.0 {
                "the chimney draws hard".to_string()
            } else {
                format!("a draught from the {}", if g.peak > 0.0 { "left" } else { "right" })
            }
        } else {
            "burning quietly".to_string()
        };
        Some(Playing { title: format!("a fire of {} logs", self.logs.len()), detail, actions: Vec::new(), notes: Vec::new() })
    }
}
