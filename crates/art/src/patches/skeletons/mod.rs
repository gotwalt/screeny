//! Skeletons: a camera looking into a box where a couple of skeletons live.
//!
//! The picture is two things drawn by two very different routes, painted by
//! one shared, indexed palette (card 328): [`box_scene`], a box lit by one
//! bulb, ray-box-intersected and shaded per pixel; and the skeletons
//! themselves, a forward-kinematic rig ([`rig`]) posed by an action
//! vocabulary ([`actions`]) and choreographed over time ([`actor`]), drawn
//! as tapered capsules ([`capsule`]) sorted far to near like `flock`'s birds.
//! [`geom`] is the 3D maths underneath all of it.
//!
//! The box, the camera, the capsule renderer and the palette technique are
//! written to be reused wholesale: card 333 (`thing`, an Addams-Family hand
//! in the same box) is expected to take `box_scene.rs` and `capsule.rs` as
//! they stand and write its own rig.

mod actions;
mod actor;
mod box_scene;
mod capsule;
mod geom;
mod rig;

use crate::color::{oklch, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use actor::World;
use box_scene::{Camera, Lch};
use capsule::{Coverage, SUPERSAMPLE};
use geom::v3;

pub const DEF: PatchDef = PatchDef {
    id: "skeletons",
    name: "Skeletons",
    blurb: "A camera into a box where a couple of skeletons live: walking, dancing, waving, coming right up to the glass.",
    params: PARAMS,
    make,
    // The box, the light and the two skeletons all come from the seed (their
    // heights, their starting places, which motif each one reaches for
    // first), so "another one like this" is a real, different box.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("skeletons", "How many live in the box", 2.0, 3.0, 1.0, 2.0),
    // Defaults tuned calm, not lively (the owner, 2026-09-26: "this is a
    // form of visual poetry... busyness is a thing we are trying to
    // avoid") - `pace` and `sway` both sit below their own middle.
    param("pace", "How fast they move (lower is slower, dreamier)", 0.4, 2.0, 0.05, 0.75),
    param("light", "How bright the bulb is", 0.5, 1.8, 0.05, 1.0),
    param("sway", "How loose the idle motion is", 0.0, 1.6, 0.05, 0.6),
    param("color", "How much colour (0 = grayscale)", 0.0, 1.0, 0.05, 0.4),
];

/// The choreography's own fixed step (card 328's "seeded, stepped on a fixed
/// internal timestep"): fine enough for a walk cycle, coarse enough that a
/// couple of skeletons cost nothing.
const STEP: f64 = 1.0 / 30.0;
/// Longest catch-up after a stall, in steps: about ten seconds picked up
/// from where it left off, not fast-forwarded.
const CATCHUP: i64 = 300;
/// A bone's ink never falls under this, however far from the bulb it is -
/// the floor that keeps a skeleton at the back of the box a dim stick
/// figure rather than a smudge that has merged into the dark.
const LIGHT_FLOOR: f32 = 0.60;
/// Ink levels for the skeletons: coarse anti-aliasing at the low end,
/// `LIGHT_FLOOR..1.0` for how lit a bone is at the high end.
const BONE_LEVELS: usize = 10;
/// Fixed reference colour for every bone: `color` only ever adds a faint
/// warm cast, so a skeleton is bone-white however lit or shadowed a
/// particular part of it is - the lit/shadow axis is ink, not hue.
fn bone_lch(color: f32) -> Lch {
    (0.74, color * 0.05, 58.0)
}
/// How dark an eye socket cuts into the skull: not true black (that would
/// read as a hole clean through the head), a shadow.
const EYE_INK: f32 = 0.04;

/// Apparent full standing height, in LEDs, below which the ribcage and the
/// face stop being drawn at all - as `flock` decides a bird's level of
/// detail from its projected span, never from a size knob.
const AREA: (f32, f32) = (10.0, 26.0);

struct Skeletons {
    world: World,
    warped: f64,
    steps: i64,
    seed: u64,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Skeletons { world: World::new(seed), warped: 0.0, steps: 0, seed })
}

impl Skeletons {
    /// Step the choreography up to where this frame's clock has reached.
    /// Taken from total simulated time rather than drained from an
    /// accumulator, so any frame rate lands on the same integer step and a
    /// snapshot is the same PNG every time (as `flock::advance` is).
    fn advance(&mut self, ctx: &Ctx) {
        self.warped += ctx.dt.clamp(0.0, 0.25) * f64::from(ctx.get("pace"));
        let target = (self.warped / STEP + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        let count = (ctx.get("skeletons").round() as usize).clamp(2, 3);
        while self.steps < target {
            self.steps += 1;
            let t = self.steps as f64 * STEP;
            self.world.step(t, count, self.seed);
        }
    }
}

/// A bone endpoint's ink: real cylindrical shading (see
/// `box_scene::bone_shade`'s doc - the owner, 2026-09-26, lifted the
/// compute-cost limit and asked by name for "proper shading of the bones"),
/// floored so a skeleton is never less than a dim stick figure however far
/// from the bulb or however deep in its own shadow side it is.
fn ink_at(p: geom::V3, axis: geom::V3, eye: geom::V3, light: f32) -> f32 {
    (LIGHT_FLOOR + (1.0 - LIGHT_FLOOR) * box_scene::bone_shade(p, axis, eye) * light).clamp(0.0, 1.0)
}

/// One capsule ready for the coverage buffer, in screen space with depth
/// for sorting.
struct CDraw {
    depth: f32,
    a: (f32, f32),
    b: (f32, f32),
    ra: f32,
    rb: f32,
    ink_a: f32,
    ink_b: f32,
}

fn project_capsule(cam: &Camera, a: geom::V3, b: geom::V3, ra: f32, rb: f32, light: f32) -> Option<CDraw> {
    let (ax, ay, az) = cam.project(a, W, H)?;
    let (bx, by, bz) = cam.project(b, W, H)?;
    let axis = b.sub(a);
    let (ink_a, ink_b) = (ink_at(a, axis, cam.eye, light), ink_at(b, axis, cam.eye, light));
    Some(CDraw { depth: 0.5 * (az + bz), a: (ax, ay), b: (bx, by), ra: ra * cam.focal / az, rb: rb * cam.focal / bz, ink_a, ink_b })
}

/// A disc (the skull, an eye) has no axis of its own: shaded as if it
/// squarely faced the camera (see `box_scene::bone_shade`).
fn project_disc(cam: &Camera, centre: geom::V3, radius: f32, light: f32, ink_override: Option<f32>) -> Option<CDraw> {
    let (x, y, z) = cam.project(centre, W, H)?;
    let ink = ink_override.unwrap_or_else(|| ink_at(centre, geom::V3::ZERO, cam.eye, light));
    Some(CDraw { depth: z, a: (x, y), b: (x, y), ra: radius * cam.focal / z, rb: radius * cam.focal / z, ink_a: ink, ink_b: ink })
}

fn mix_hue(a: f32, b: f32, t: f32) -> f32 {
    a + ((b - a + 540.0).rem_euclid(360.0) - 180.0) * t
}

fn mix(a: Lch, b: Lch, t: f32) -> Lch {
    let (ha, hb) = (if a.0 < 0.01 { b.2 } else { a.2 }, if b.0 < 0.01 { a.2 } else { b.2 });
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, mix_hue(ha, hb, t))
}

/// Below this the panel has only a handful of levels and they carry colour
/// casts (brief 2.1): true black instead. Same rule `flock` uses.
const FLOOR_L: f32 = 0.02;

fn paint((l, c, h): Lch) -> Rgb {
    if l < FLOOR_L {
        Rgb::BLACK
    } else {
        oklch(l, c, h)
    }
}

fn build_palette(bg: &[Lch; box_scene::BANDS], bone: Lch, levels: usize) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(box_scene::BANDS * levels);
    for band in bg {
        for k in 0..levels {
            out.push(paint(mix(*band, bone, k as f32 / (levels - 1) as f32)));
        }
    }
    out
}

/// Every capsule for every actor at one instant `t`, far to near. Called
/// several times per frame at different `t` within the frame's own interval
/// for motion blur (see [`Skeletons::render`]'s `BLUR_SAMPLES`) - a pure
/// function of the actors' *continuous* pose functions, never advancing the
/// choreography itself (`Actor::pos_at`'s doc explains why that is sound).
fn gather_draws(actors: &[actor::Actor], t: f64, sway: f32, light: f32, cam: &Camera) -> Vec<CDraw> {
    let mut draws: Vec<CDraw> = Vec::new();
    for a in actors {
        let pose = a.pose(t, sway);
        let (px, pz) = a.pos_at(t);
        let ground = v3(px, 0.0, pz);
        let mut joints = pose.solve(ground, a.heading, a.height);
        let Some((_, _, z_ref)) = cam.project(joints.chest, W, H) else { continue };
        let span = a.height * cam.focal / z_ref;
        let detail = box_scene::smooth(AREA.0, AREA.1, span);

        let mut bones = joints.bones(detail);
        let head_off = a.head_off(t);
        if head_off > 0.04 {
            // Detached: drop the neck-skull bone rather than stretch it,
            // and carry the skull (and everything hung off it) up and
            // away along a small arc.
            bones.remove(3);
            let lift = joints.chest_frame.up.scale(0.55 * a.height * head_off);
            let bob = joints.chest_frame.fwd.scale(0.10 * a.height * head_off * (t as f32 * 2.6).sin());
            joints.skull = joints.skull.add(lift).add(bob);
        }
        for bone in &bones {
            if let Some(d) = project_capsule(cam, bone.a, bone.b, bone.ra * a.height, bone.rb * a.height, light) {
                draws.push(d);
            }
        }
        if let Some(d) = project_disc(cam, joints.skull, rig::SKULL_R * a.height, light, None) {
            draws.push(d);
        }
        if detail > 0.12 {
            let jaw_open = if head_off > 0.04 { 0.4 } else { pose.jaw };
            let jaw = joints.jaw_tip(jaw_open);
            if let Some(d) = project_capsule(cam, joints.skull, jaw, 0.020 * a.height, 0.013 * a.height, light) {
                draws.push(d);
            }
            for eye in joints.eyes() {
                if let Some(d) = project_disc(cam, eye, rig::EYE_R * a.height, light, Some(EYE_INK)) {
                    draws.push(d);
                }
            }
        }
    }
    draws.sort_by(|p, q| q.depth.total_cmp(&p.depth));
    draws
}

/// How many moments across each frame's interval are rendered and averaged
/// for motion blur. Compute is no longer the constraint here (the owner,
/// 2026-09-26: "spend it on image quality... motion blur"); four is enough
/// that a hand swinging through a wave or a foot lifting through a stride
/// leaves a real, soft trail rather than a stutter, without the cost
/// climbing towards where it would ever matter for a background patch.
const BLUR_SAMPLES: usize = 4;

impl Patch for Skeletons {
    fn playing(&self) -> Option<Playing> {
        let title = format!("{} in the box", self.world.actors.len());
        let detail = self.world.actors.iter().map(actor::Actor::playing_label).collect::<Vec<_>>().join(", ");
        Some(Playing { title, detail, actions: Vec::new(), notes: Vec::new() })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.advance(ctx);
        let color = ctx.get("color");
        let light = ctx.get("light");
        let sway = ctx.get("sway");
        let sim_t = self.steps as f64 * STEP;

        let cam = Camera::at(ctx.t, W);
        let shadows: Vec<(f32, f32)> = self.world.actors.iter().map(|a| a.pos).collect();

        let bg_bands = box_scene::bands(color);
        let bone = bone_lch(color);
        let palette = build_palette(&bg_bands, bone, BONE_LEVELS);

        // Motion blur: `BLUR_SAMPLES` moments spread over the last fixed
        // step, each its own fully sorted, fully anti-aliased render,
        // averaged - a box filter over time, the same idea `Frame::supersample`
        // already uses over space.
        let covers: Vec<Coverage> = (0..BLUR_SAMPLES)
            .map(|i| {
                let frac = (i as f64 + 0.5) / BLUR_SAMPLES as f64;
                let t = sim_t - STEP * (1.0 - frac);
                let draws = gather_draws(&self.world.actors, t, sway, light, &cam);
                let mut cov = Coverage::new(SUPERSAMPLE, W, H);
                for d in &draws {
                    cov.capsule(d.a, d.b, d.ra, d.rb, d.ink_a, d.ink_b);
                }
                cov
            })
            .collect();

        let bg_dither = Dither::Bayer4;
        let ink_dither = Dither::BlueNoise;
        let bands_n = box_scene::BANDS;
        let indices = (0..N)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let raw = (box_scene::brightness_at(&cam, x as f32 + 0.5, y as f32 + 0.5, W, H, &shadows) * light).clamp(0.0, 1.0);
                let band = (raw * (bands_n - 1) as f32 + bg_dither.threshold(x, y)).round().clamp(0.0, (bands_n - 1) as f32) as usize;
                let ink = covers.iter().map(|c| c.pixel(x, y)).sum::<f32>() / BLUR_SAMPLES as f32;
                let k = (ink * (BONE_LEVELS - 1) as f32 + ink_dither.threshold(x, y)).round().clamp(0.0, (BONE_LEVELS - 1) as f32) as usize;
                (band * BONE_LEVELS + k) as u8
            })
            .collect();

        Frame::Indexed { palette, indices }
    }
}

#[cfg(test)]
mod tests;
