//! Physarum: a slime-mould colony of a million and more agents, seen through
//! the panel as through a window onto a much larger world.
//!
//! The owner, 2026-10-27: a piece "inspired by Sage Jenson's 36 Points", to
//! "make the most of the vast compute environment relative to the tiny
//! display size to create a visual experience that feels like magic even
//! despite the tiny resolution". An experiment (`exp/physarum`).
//!
//! **What is simulated.** Jones's agent model (2010): each agent senses the
//! trail ahead and to either side, turns toward the most, steps and
//! deposits; the trail spreads and fades. Jenson's extension makes every
//! agent parameter a law of the trail it stands in, `base + amp * s^exp`,
//! which is what lets one colony hold structure at several scales. A
//! [`Point`] is one set of those laws: one organism.
//!
//! **Why so many agents for 2048 LEDs.** The world is 1024 x 512 cells and
//! the camera sees a window of it, a few to sixteen cells per LED; each LED
//! is the average of the trail over its footprint, so hundreds of agents
//! stand behind every one. That is what keeps the picture calm: no
//! sparkle, smooth gradients that go out exact. And each frame is exposed
//! over several substeps, so filaments sliding under an LED move its value
//! continuously instead of in steps.
//!
//! **Colour** is painted by palette index: black, then a ramp in one hue.
//!
//! Credit: Jeff Jones, "Characteristics of pattern formation and evolution
//! in approximations of Physarum transport networks" (Artificial Life,
//! 2010); Sage Jenson, "36 Points" (2022); Antoine Bleuje's
//! interactive-physarum, studied, not copied.

mod gpu;

use crate::color::{oklch, smoothstep, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, W};
use crate::gpu::Gpu;
use crate::patch::{choice, param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use gpu::{Params, Sim};

pub const DEF: PatchDef = PatchDef {
    id: "physarum",
    name: "Physarum",
    blurb: "GPU. A slime-mould colony of a million agents, seen through a window onto a far larger world: veins, cells and labyrinths that grow, hold and dissolve.",
    params: PARAMS,
    make,
    // The seed scatters the agents and starts the camera.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    choice("point", "Organism", POINT_NAMES, 0.0),
    param("cycle", "Minutes on each organism before growing into the next (0: stay)", 0.0, 60.0, 0.5, 0.0),
    param("zoom", "Closer (above 1) or further than each organism's own scale", 0.25, 4.0, 0.05, 1.0),
    param("breathe", "How far the camera breathes in and out, octaves", 0.0, 2.0, 0.05, 0.0),
    param("drift", "How fast the camera drifts, LEDs per second", 0.0, 4.0, 0.05, 0.4),
    param("hue", "Hue of the faintest veins, degrees", 0.0, 360.0, 1.0, 195.0),
    param("spread", "How far the hue turns from the faintest veins to the densest, degrees", -180.0, 180.0, 1.0, 25.0),
    param("saturation", "How much colour (0: white light only)", 0.0, 2.0, 0.01, 1.0),
    param("white", "How far the densest cores burn toward white (0: they keep their colour)", 0.0, 1.0, 0.01, 1.0),
    param("exposure", "How bright, against the world's mean trail", 0.05, 2.0, 0.01, 0.7),
    param("contrast", "How steeply the veins fall off into black", 0.5, 4.0, 0.05, 2.0),
    param("cap", "How much trail a vein can hold, times the mean (stops a few highways taking everything)", 1.5, 40.0, 0.5, 6.0),
    param("flow", "How hard a slow current bends every path", 0.0, 0.3, 0.005, 0.04),
    param("renewal", "Agents scattered afresh, thousandths per substep", 0.0, 5.0, 0.05, 0.5),
    param("agents", "Agents, millions (rebuilds the colony)", 0.25, 4.0, 0.25, 1.5),
    param("steps", "Substeps per frame: exposure, and speed", 1.0, 8.0, 1.0, 3.0),
];

/// The world, cells. Sixteen per LED across at the widest zoom.
const WORLD_W: u32 = 1024;
const WORLD_H: u32 = 512;

/// One substep's law for an agent parameter: `base + amp * s^exp`, `s` the
/// trail under the agent relative to the world's mean.
#[derive(Clone, Copy, Debug)]
struct Law(f32, f32, f32);

impl Law {
    const fn k(v: f32) -> Law {
        Law(v, 0.0, 1.0)
    }
    fn lerp(self, o: Law, t: f32) -> Law {
        Law(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t, self.2 + (o.2 - self.2) * t)
    }
}

/// One organism: the laws for sensor distance (cells), sensor angle and
/// rotation angle (radians) and move distance (cells per substep), and the
/// trail's fade and spread per substep.
#[derive(Clone, Copy, Debug)]
struct Point {
    name: &'static str,
    sd: Law,
    sa: Law,
    ra: Law,
    md: Law,
    decay: f32,
    diffuse: f32,
    /// A small random turn per substep, radians, full width.
    wobble: f32,
    /// World cells per LED at which its structure reads best on the panel.
    zoom: f32,
}

impl Point {
    fn lerp(&self, o: &Point, t: f32) -> Point {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Point {
            name: if t < 0.5 { self.name } else { o.name },
            sd: self.sd.lerp(o.sd, t),
            sa: self.sa.lerp(o.sa, t),
            ra: self.ra.lerp(o.ra, t),
            md: self.md.lerp(o.md, t),
            decay: m(self.decay, o.decay),
            diffuse: m(self.diffuse, o.diffuse),
            wobble: m(self.wobble, o.wobble),
            // In octaves, so a change of scale moves at an even pace.
            zoom: (self.zoom.ln() + (o.zoom.ln() - self.zoom.ln()) * t).exp(),
        }
    }
}

const POINT_NAMES: &[&str] = &["veins", "honeycomb", "labyrinth", "multiscale", "worms", "lace"];

const POINTS: [Point; 6] = [
    // Jones's classic network: narrow sensors, a wider turn.
    Point {
        name: "veins",
        sd: Law::k(9.0),
        sa: Law::k(0.39),
        ra: Law::k(0.78),
        md: Law::k(1.0),
        decay: 0.06,
        diffuse: 0.6,
        wobble: 0.05,
        zoom: 10.0,
    },
    // Far sensors and long steps: big, clean cells.
    Point {
        name: "honeycomb",
        sd: Law::k(26.0),
        sa: Law::k(0.6),
        ra: Law::k(0.45),
        md: Law::k(1.6),
        decay: 0.04,
        diffuse: 0.5,
        wobble: 0.05,
        zoom: 12.0,
    },
    // Wide sensors, a narrow turn: walls that wander and fold.
    Point {
        name: "labyrinth",
        sd: Law::k(7.0),
        sa: Law::k(1.4),
        ra: Law::k(0.35),
        md: Law::k(0.9),
        decay: 0.07,
        diffuse: 0.7,
        wobble: 0.08,
        zoom: 4.0,
    },
    // Jenson's idea: in thick trail, sense further and turn harder.
    Point {
        name: "multiscale",
        sd: Law(3.0, 18.0, 0.6),
        sa: Law(0.3, 0.35, 0.8),
        ra: Law(0.25, 0.6, 1.2),
        md: Law::k(1.1),
        decay: 0.05,
        diffuse: 0.6,
        wobble: 0.05,
        zoom: 14.0,
    },
    // Fast where it is dense, slow where it is not: travelling worms.
    Point {
        name: "worms",
        sd: Law::k(11.0),
        sa: Law::k(0.5),
        ra: Law::k(0.22),
        md: Law(0.3, 1.8, 1.3),
        decay: 0.05,
        diffuse: 0.5,
        wobble: 0.1,
        zoom: 6.0,
    },
    // Short sensors, short steps: a fine, even mesh.
    Point { name: "lace", sd: Law::k(5.0), sa: Law::k(0.7), ra: Law::k(0.35), md: Law::k(0.7), decay: 0.08, diffuse: 0.8, wobble: 0.05, zoom: 3.0 },
];

/// The ramp: index 0 is black, 1..=STEPS one hue rising to near white.
const STEPS: usize = 24;
/// Below this much of the first step, black: faint trail does not glow.
const FLOOR: f32 = 0.6;
/// The fixed step, seconds, and how many a stalled run may catch up.
const STEP: f64 = 1.0 / 30.0;
const CATCHUP: u32 = 3;
/// How fast the exposure follows the picture, seconds.
const GAIN_TAU: f32 = 2.0;

struct Physarum {
    seed: u64,
    /// `None` until first render; `Some(None)` if there is no GPU.
    sim: Option<Option<Sim>>,
    t: f64,
    owed: f64,
    substep: u32,
    /// The camera's middle, world cells, and its heading, radians.
    cam: [f64; 2],
    heading: f64,
    gain: Option<f32>,
    point: Point,
    last: Option<Frame>,
}

fn make(seed: u64) -> Box<dyn Patch> {
    let mut rng = Rng::new(seed ^ 0x5157);
    let cam = [f64::from(rng.f32()) * f64::from(WORLD_W), f64::from(rng.f32()) * f64::from(WORLD_H)];
    let heading = f64::from(rng.f32()) * std::f64::consts::TAU;
    Box::new(Physarum { seed, sim: None, t: 0.0, owed: 0.0, substep: 0, cam, heading, gain: None, point: POINTS[0], last: None })
}

fn agents_for(millions: f32, seed: u64) -> Vec<[f32; 4]> {
    let n = ((millions * 1e6) as usize).clamp(1 << 16, 8_000_000 - 1);
    let mut rng = Rng::new(seed);
    (0..n).map(|_| [rng.f32() * WORLD_W as f32, rng.f32() * WORLD_H as f32, rng.f32() * std::f32::consts::TAU, 0.0]).collect()
}

impl Physarum {
    /// The organism now: the chosen one, or with `cycle`, each in turn for
    /// that many minutes, the last part of each growing into the next.
    fn point(&self, ctx: &Ctx) -> Point {
        let chosen = (ctx.get("point") as usize).min(POINTS.len() - 1);
        let minutes = f64::from(ctx.get("cycle"));
        if minutes <= 0.0 {
            return POINTS[chosen];
        }
        let phase = self.t / (minutes * 60.0);
        let i = (chosen + phase.floor() as usize) % POINTS.len();
        let blend = smoothstep(0.7, 1.0, phase.fract() as f32);
        POINTS[i].lerp(&POINTS[(i + 1) % POINTS.len()], blend)
    }

    fn ramp(ctx: &Ctx) -> Vec<Rgb> {
        let (hue, spread, saturation, white) = (ctx.get("hue"), ctx.get("spread"), ctx.get("saturation"), ctx.get("white"));
        let mut palette = vec![Rgb::BLACK];
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            // Toward white, the top runs lighter; keeping its colour, it
            // stops short of the lightness no saturated colour reaches.
            let l = 0.32 + (0.48 + 0.15 * white) * t.powf(0.9);
            // With `white`, chroma swells through the middle and lets go at
            // the top, so the densest veins burn toward white; without, it
            // rises and holds.
            let burn = (std::f32::consts::PI * (t * 0.85 + 0.1)).sin().max(0.0);
            let hold = smoothstep(0.0, 0.35, t) * 0.8 + 0.2;
            let c = 0.17 * saturation * (hold + (burn - hold) * white);
            palette.push(oklch(l, c, hue + spread * t));
        }
        palette
    }

    fn paint(&mut self, ctx: &Ctx, leds: &[f32]) -> Frame {
        // A fixed exposure against the world's mean, eased only so a change
        // of the slider does not jump.
        let want = ctx.get("exposure");
        let gain = match self.gain {
            Some(g) => g + (want - g) * (1.0 - (-ctx.dt as f32 / GAIN_TAU).exp()),
            None => want,
        };
        self.gain = Some(gain);
        let contrast = ctx.get("contrast");
        let dither = Dither::BlueNoise;
        let indices = leds
            .iter()
            .enumerate()
            .map(|(i, v)| {
                // A soft shoulder: dense veins approach white, never clip flat.
                let pos = (1.0 - (-(v * gain).max(0.0).powf(contrast)).exp()) * STEPS as f32;
                if pos < FLOOR {
                    return 0;
                }
                let thr = dither.threshold(i % W, i / W);
                (pos + 0.5 + thr).floor().clamp(1.0, STEPS as f32) as u8
            })
            .collect();
        Frame::Indexed { palette: Self::ramp(ctx), indices }
    }
}

impl Patch for Physarum {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let millions = ctx.get("agents");
        let wanted = agents_for_count(millions);
        if matches!(&self.sim, Some(Some(s)) if s.agents() != wanted) {
            self.sim = None;
        }
        if self.sim.is_none() {
            self.sim = Some(match Gpu::shared() {
                Ok(gpu) => Some(Sim::new(gpu, WORLD_W, WORLD_H, &agents_for(millions, self.seed))),
                Err(e) => {
                    eprintln!("screeny-art: physarum: {e}; rendering black");
                    None
                }
            });
        }
        self.owed += ctx.dt;
        let mut steps = 0;
        while self.owed >= STEP * 0.5 && steps < CATCHUP {
            self.owed -= STEP;
            steps += 1;
        }
        self.owed = self.owed.max(-STEP);
        if steps == 0 {
            if let Some(last) = &self.last {
                return last.clone();
            }
            steps = 1;
        }
        self.t += STEP * f64::from(steps);

        let point = self.point(ctx);
        self.point = point;
        let base = point.zoom / ctx.get("zoom");
        let scale = base * 2f32.powf(ctx.get("breathe") * (self.t * std::f64::consts::TAU / 150.0).sin() as f32);
        // The camera wanders: a heading that turns slowly back and forth.
        self.heading += STEP * f64::from(steps) * (0.02 + 0.05 * (self.t / 53.0).sin());
        let speed = f64::from(ctx.get("drift") * scale) * STEP * f64::from(steps);
        self.cam[0] = (self.cam[0] + speed * self.heading.cos()).rem_euclid(f64::from(WORLD_W));
        self.cam[1] = (self.cam[1] + speed * self.heading.sin()).rem_euclid(f64::from(WORLD_H));

        let Some(Some(sim)) = &mut self.sim else {
            return Frame::black();
        };
        let substeps = (ctx.get("steps") as u32).max(1) * steps;
        let density = sim.agents() as f32 / (WORLD_W * WORLD_H) as f32;
        let laws = [point.sd, point.sa, point.ra, point.md].map(|l| [l.0, l.1, l.2]).concat();
        let params: Vec<Params> = (0..substeps)
            .map(|k| {
                self.substep = self.substep.wrapping_add(1);
                let _ = k;
                Params {
                    w: WORLD_W,
                    h: WORLD_H,
                    n: sim.agents(),
                    seed: self.substep.wrapping_mul(0x9e37_79b9),
                    laws: laws.clone().try_into().unwrap_or([0.0; 12]),
                    deposit: 1.0,
                    decay: point.decay,
                    diffuse: point.diffuse,
                    inv_ref: point.decay / density,
                    cx: self.cam[0] as f32,
                    cy: self.cam[1] as f32,
                    scale,
                    first: 0,
                    wobble: point.wobble,
                    respawn: ctx.get("renewal") * 1e-3,
                    cap: ctx.get("cap"),
                    flow: ctx.get("flow"),
                    time: self.t as f32,
                    pad: [0.0; 2],
                }
            })
            .collect();
        let Some(mut leds) = sim.frame(&params) else {
            return Frame::black();
        };
        let inv = 1.0 / substeps as f32;
        leds.iter_mut().for_each(|v| *v *= inv);
        let frame = self.paint(ctx, &leds);
        self.last = Some(frame.clone());
        frame
    }

    fn playing(&self) -> Option<Playing> {
        Some(Playing { title: self.point.name.to_string(), detail: String::new(), actions: Vec::new(), notes: Vec::new() })
    }
}

fn agents_for_count(millions: f32) -> u32 {
    ((millions * 1e6) as usize).clamp(1 << 16, 8_000_000 - 1) as u32
}
