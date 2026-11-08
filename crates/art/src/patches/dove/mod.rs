//! Dove: a white dove flying behind the Art Frame's frosted glass, after the
//! dove of the Holy Spirit in church art - upright in the glass, belly to us,
//! head up, wings beating. It is simply there, as a bird would be: it holds
//! the way it is turned (three settings turn it) and only its wings and the
//! lift they give it move.
//!
//! It is seen as veil's hand is (`super::glass`): by its own light, sharp
//! only where it is nearest the glass and soft and faint further back, so the
//! wings come into focus and go out of it as they beat. It sits so that its
//! nearest wingtip, over a whole beat, just reaches the glass.

mod bird;

use super::glass::{self, Glass};
use crate::frame::Frame;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef};
use crate::rng::Rng;
use bird::{Mesh, Wings};
use std::f32::consts::TAU;

pub const DEF: PatchDef = PatchDef {
    id: "dove",
    name: "Dove",
    blurb: "A white dove behind frosted glass, after the dove of the Holy Spirit: a 3D bird beating its wings, sharp only where it nears the glass. Made for the 32x32 Art Frame.",
    params: PARAMS,
    make,
    // Nothing in it is chosen by the seed that a person would call another
    // picture: it is the same bird, turned as the settings say.
    seeded: false,
};

const PARAMS: &[ParamSpec] = &[
    param("turn", "Turned about the up-down axis (°)", -180.0, 180.0, 1.0, 0.0),
    param("tip", "Tipped head towards us (°)", -180.0, 180.0, 1.0, 0.0),
    param("spin", "Spun in the picture (°)", -180.0, 180.0, 1.0, 0.0),
    param("size", "Wingspan, of the frame's width", 0.3, 1.6, 0.01, 0.9),
    param("near", "How far behind the glass it glides", 0.0, 1.5, 0.01, 0.0),
    param("beat", "Wingbeats a second", 0.1, 4.0, 0.05, 0.7),
    param("stroke", "How deep the wings beat (°)", 0.0, 80.0, 1.0, 50.0),
    param("glide", "Share of the time spent gliding", 0.0, 1.0, 0.01, 0.4),
    param("depth", "How far behind the glass the dove stays clear", 0.1, 4.0, 0.01, 1.0),
    param("glow", "The dove's light through the glass", 0.0, 2.0, 0.01, 1.0),
    param("hue", "Hue", 0.0, 360.0, 1.0, 60.0),
    param("tint", "Tint (0: white)", 0.0, 0.12, 0.001, 0.0),
    param("steps", "Palette steps", 2.0, 200.0, 1.0, 120.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
];

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Dove::new(seed))
}

/// The Art Frame's width in sheet units (32 LEDs).
const FRAME: f32 = 2.16;

/// The wings held out in a glide: raised a little.
const GLIDE_LIFT: f32 = 0.12;

/// How far the tips trail the stroke, and how far the wings draw in on the
/// upstroke, at a full beat.
const BEND: f32 = 0.5;
const FOLD: f32 = 0.35;

/// How far the body rises and falls with each beat (bird units).
const BOB: f32 = 0.025;

/// How quickly the wings take up or leave off beating (s).
const EASE: f32 = 0.5;

/// The longest glide, whatever `glide` says, unless it says always.
const LONGEST_GLIDE: f32 = 30.0;

struct Dove {
    rng: Rng,
    glass: Glass,
    mesh: Mesh,
    /// The mesh's points as placed behind the glass this frame.
    placed: Vec<[f32; 3]>,
    /// Where in a beat the wings are (0..1), how much they beat (0 gliding,
    /// 1 a full beat), whether they are beating, and until when.
    phase: f32,
    amp: f32,
    beating: bool,
    until: f32,
    last: f64,
}

impl Dove {
    fn new(seed: u64) -> Self {
        Dove { rng: Rng::new(seed ^ 0xd0e), glass: Glass::new(), mesh: Mesh::default(), placed: Vec::new(), phase: 0.0, amp: 1.0, beating: true, until: 0.0, last: 0.0 }
    }
}

/// The wings at `phase` of a beat `amp` deep.
fn wings(phase: f32, amp: f32, stroke: f32) -> Wings {
    let (s, c) = (TAU * phase).sin_cos();
    Wings { lift: GLIDE_LIFT + amp * stroke * c, bend: amp * BEND * s, fold: amp * FOLD * (-s).max(0.0) }
}

/// A rotation as its three columns' images: the bird's x, y and z in the
/// sheet's axes (x right, y up, z towards us).
type Turn = [[f32; 3]; 3];

fn apply(r: &Turn, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|k| r[0][k] * p[0] + r[1][k] * p[1] + r[2][k] * p[2])
}

/// The bird's axes in the sheet. Unturned it is the dove of the Holy Spirit:
/// head up, belly to us, so its right wing is on our left. Then, in the
/// sheet's own axes: spun in the picture, tipped head towards us, and turned
/// about the up-down axis.
fn orientation(turn: f32, tip: f32, spin: f32) -> Turn {
    let base: Turn = [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]];
    let (ss, cs) = spin.sin_cos();
    let (st, ct) = tip.sin_cos();
    let (sy, cy) = turn.sin_cos();
    let rz = |p: [f32; 3]| [cs * p[0] - ss * p[1], ss * p[0] + cs * p[1], p[2]];
    let rx = |p: [f32; 3]| [p[0], ct * p[1] - st * p[2], st * p[1] + ct * p[2]];
    let ry = |p: [f32; 3]| [cy * p[0] + sy * p[2], p[1], -sy * p[0] + cy * p[2]];
    base.map(|axis| ry(rx(rz(axis))))
}

impl Patch for Dove {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let t = ctx.t as f32;
        if ctx.t < self.last {
            // Time went backwards (a restart): begin again.
            *self = Dove { rng: Rng::new(self.rng.u64()), ..Dove::new(0) };
        }
        self.last = ctx.t;
        let dt = (ctx.dt as f32).clamp(0.0, 0.1);
        let (beat, glide) = (ctx.get("beat"), ctx.get("glide"));
        let stroke = ctx.get("stroke").to_radians();

        // Beating for a few beats, then gliding a while, and again.
        if glide <= 0.0 {
            self.beating = true;
        } else if glide >= 1.0 {
            self.beating = false;
        } else if t >= self.until {
            self.beating = !self.beating;
            let run = self.rng.range(2.0, 6.0) / beat;
            self.until = t + if self.beating { run } else { (4.0 / beat * glide / (1.0 - glide) * self.rng.range(0.6, 1.4)).min(LONGEST_GLIDE) };
        }
        self.phase = (self.phase + beat * dt).fract();
        let goal = if self.beating { 1.0 } else { 0.0 };
        self.amp += (goal - self.amp) * (1.0 - (-dt / EASE).exp());

        // Placed so that, gliding, its nearest point is `near` behind the
        // glass, and it stays there however its wings go: a downstroke
        // brings them up to the glass, sharp, and the upstroke takes them
        // back into the blur. (Nothing goes through: what would is held at
        // the glass, as sharp as it gets.)
        let turn = orientation(ctx.get("turn").to_radians(), ctx.get("tip").to_radians(), ctx.get("spin").to_radians());
        let scale = ctx.get("size") * FRAME / bird::SPAN;
        bird::build(wings(0.0, 0.0, stroke), &mut self.mesh);
        let front = self.mesh.points.iter().map(|&p| apply(&turn, p)[2]).fold(f32::MIN, f32::max);
        let back = -ctx.get("near") - front * scale;

        // Each downstroke lifts it a little towards its back.
        let lift = BOB * self.amp * (TAU * self.phase).sin();
        bird::build(wings(self.phase, self.amp, stroke), &mut self.mesh);
        self.glass.clear();
        self.placed.clear();
        self.placed.extend(self.mesh.points.iter().map(|&p| {
            let q = apply(&turn, [p[0], p[1], p[2] + lift]);
            [q[0] * scale, q[1] * scale, q[2] * scale + back]
        }));
        for tri in &self.mesh.triangles {
            let [a, b, c] = tri.map(|i| self.placed[i as usize]);
            self.glass.press(a, b, c, [1.0; 3]);
        }
        self.glass.shine(0.0, ctx.get("depth"));
        self.glass.picture(&glass::Look::from(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Params;

    fn run(params: &Params, seconds: f32, mut each: impl FnMut(&Dove, &Frame, f32)) {
        let mut dove = Dove::new(7);
        for i in 1..(seconds * 30.0) as usize {
            let t = i as f64 / 30.0;
            let frame = dove.render(&Ctx { t, dt: 1.0 / 30.0, now: 0.0, params });
            each(&dove, &frame, t as f32);
        }
    }

    fn quick() -> Params {
        let mut p = Params::defaults(PARAMS);
        p.set(PARAMS, "samples", 1.0);
        p
    }

    fn lit(frame: &Frame) -> usize {
        // The Art Frame's centre square.
        (0..frame::N).filter(|&i| (16..48).contains(&(i % frame::W))).filter(|&i| frame.pixel(i).r > 0.02).count()
    }

    use crate::frame;

    #[test]
    fn it_shows_a_dove_and_its_wings_move() {
        let params = quick();
        let (mut least, mut most, mut changed) = (usize::MAX, 0, false);
        let mut first: Option<Vec<f32>> = None;
        run(&params, 6.0, |_, f, _| {
            let n = lit(f);
            (least, most) = (least.min(n), most.max(n));
            let px: Vec<f32> = (0..frame::N).map(|i| f.pixel(i).r).collect();
            match &first {
                None => first = Some(px),
                Some(a) => changed |= a.iter().zip(&px).any(|(x, y)| (x - y).abs() > 0.05),
            }
        });
        assert!(least > 60, "at times only {least} of 1024 LEDs lit");
        assert!(most < 1000, "{most} LEDs lit: it fills the frame");
        assert!(changed, "nothing moved");
    }

    /// Gliding, however it is turned, its nearest point is just at the glass.
    #[test]
    fn gliding_it_comes_up_to_the_glass() {
        for (turn, tip, spin, size) in [(0.0, 0.0, 0.0, 0.9), (90.0, 0.0, 0.0, 0.9), (30.0, 60.0, -20.0, 1.6), (-150.0, -80.0, 120.0, 0.3), (0.0, 180.0, 0.0, 1.2)] {
            let mut params = quick();
            for (id, v) in [("turn", turn), ("tip", tip), ("spin", spin), ("size", size), ("glide", 1.0)] {
                params.set(PARAMS, id, v);
            }
            let mut dove = Dove::new(3);
            let mut nearest = f32::MIN;
            for i in 1..300 {
                dove.render(&Ctx { t: i as f64 / 30.0, dt: 1.0 / 30.0, now: 0.0, params: &params });
                // Once the wings have settled out of the beat they start in.
                if i < 150 {
                    continue;
                }
                nearest = nearest.max(dove.placed.iter().map(|p| p[2]).fold(f32::MIN, f32::max));
            }
            let what = format!("turn {turn} tip {tip} spin {spin} size {size}");
            assert!(nearest.abs() < 0.02, "{what}: gliding, its nearest point is {nearest} from the glass");
        }
    }

    #[test]
    fn turning_it_changes_the_picture() {
        let picture = |id: &str, v: f32| {
            let mut params = quick();
            params.set(PARAMS, "glide", 1.0);
            params.set(PARAMS, id, v);
            let mut dove = Dove::new(1);
            let mut last = None;
            for i in 1..30 {
                last = Some(dove.render(&Ctx { t: i as f64 / 30.0, dt: 1.0 / 30.0, now: 0.0, params: &params }));
            }
            let f = last.unwrap();
            (0..frame::N).map(|i| f.pixel(i).r).collect::<Vec<f32>>()
        };
        let still = picture("turn", 0.0);
        for (id, v) in [("turn", 70.0), ("tip", 60.0), ("spin", 45.0)] {
            let other = picture(id, v);
            assert!(still.iter().zip(&other).any(|(a, b)| (a - b).abs() > 0.1), "{id} {v} changed nothing");
        }
    }
}
