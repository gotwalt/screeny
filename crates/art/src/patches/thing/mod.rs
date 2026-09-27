//! `thing`: a right hand, cut off at the wrist, living in card 328's box -
//! the Addams Family "Thing" the owner asked for, walking on its fingertips
//! and gesturing at the glass. This card (333) builds everything the
//! performance needs except the performance itself: the hand
//! ([`hand_rig`]), a clip format and a player that blends between clips by
//! inertialization rather than a cross-fade ([`clip`], [`player`]), contact
//! IK so a planted fingertip does not skate ([`contact_ik`]), a
//! choreographer that picks walks and gestures at random with rests
//! between ([`choreo`]), and the converter that will turn the owner's own
//! phone footage into clips (`tools/thing-capture/`). Until that footage
//! lands, [`clip::placeholders`] is hand-keyed scaffolding, labelled as such
//! in [`Patch::playing`] - "the real motion arrives when the owner's
//! footage does."
//!
//! The render reuses card 328's box and camera wholesale
//! ([`box_scene`], a byte-for-byte copy so `skeletons` stays untouched) and
//! `skeletons`' own capsule technique ([`capsule`]), painted by one shared,
//! indexed palette exactly as `skeletons` and `flock` are.

mod box_scene;
mod capsule;
mod choreo;
mod clip;
mod contact_ik;
mod geom;
mod hand_rig;
mod player;

use crate::color::{oklch, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use box_scene::{Camera, Lch};
use capsule::{Coverage, SUPERSAMPLE};
use choreo::Choreographer;
use geom::{v3, V3};
use hand_rig::HAND_H;
use player::Posed;

pub const DEF: PatchDef = PatchDef {
    id: "thing",
    name: "Thing",
    blurb: "A hand, cut off at the wrist, living in a box: it walks on its fingertips and gestures at the glass.",
    params: PARAMS,
    make,
    // The seed drives the choreographer's own rng (which gesture comes
    // next, how long each rest lasts), so "another one like this" is a
    // real, differently-paced performance.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    // Defaults calm, matching the autumn set's own ethos ("visual poetry...
    // busyness is a thing we are trying to avoid") rather than
    // `skeletons`' own numbers, which is why these are not identical.
    param("pace", "How fast it moves (lower is slower, dreamier)", 0.4, 2.0, 0.05, 0.8),
    param("light", "How bright the bulb is", 0.5, 1.8, 0.05, 1.0),
    param("color", "How much colour (0 = grayscale)", 0.0, 1.0, 0.05, 0.35),
];

/// The choreography's own fixed step, matching `skeletons`' `STEP` (card
/// 328's own reasoning applies unchanged: fine enough for a walk cycle,
/// coarse enough that one hand costs nothing).
const STEP: f64 = 1.0 / 30.0;
const CATCHUP: i64 = 300;

const LIGHT_FLOOR: f32 = 0.55;
const INK_LEVELS: usize = 10;
fn hand_lch(color: f32) -> Lch {
    // Pale, well below full white (the card): a cool, slightly desaturated
    // bone tone, `color` only ever adding a faint warm cast exactly as
    // `skeletons`' own `bone_lch` does.
    (0.78, color * 0.05, 62.0)
}
const NAIL_INK: f32 = 0.98;

struct Thing {
    choreo: Choreographer,
    warped: f64,
    steps: i64,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Thing { choreo: Choreographer::new(seed), warped: 0.0, steps: 0 })
}

impl Thing {
    fn advance(&mut self, ctx: &Ctx) {
        self.warped += ctx.dt.clamp(0.0, 0.25) * f64::from(ctx.get("pace"));
        let target = (self.warped / STEP + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.steps += 1;
            let t = self.steps as f64 * STEP;
            self.choreo.advance(t, STEP as f32);
        }
    }
}

fn ink_at(p: V3, axis: V3, eye: V3, light: f32) -> f32 {
    (LIGHT_FLOOR + (1.0 - LIGHT_FLOOR) * box_scene::bone_shade(p, axis, eye) * light).clamp(0.0, 1.0)
}

struct CDraw {
    depth: f32,
    a: (f32, f32),
    b: (f32, f32),
    ra: f32,
    rb: f32,
    ink_a: f32,
    ink_b: f32,
}

fn project_capsule(cam: &Camera, a: V3, b: V3, ra: f32, rb: f32, light: f32) -> Option<CDraw> {
    let (ax, ay, az) = cam.project(a, W, H)?;
    let (bx, by, bz) = cam.project(b, W, H)?;
    let axis = b.sub(a);
    let (ink_a, ink_b) = (ink_at(a, axis, cam.eye, light), ink_at(b, axis, cam.eye, light));
    Some(CDraw { depth: 0.5 * (az + bz), a: (ax, ay), b: (bx, by), ra: ra * cam.focal / az, rb: rb * cam.focal / bz, ink_a, ink_b })
}

fn project_disc(cam: &Camera, centre: V3, radius: f32, light: f32, ink_override: Option<f32>) -> Option<CDraw> {
    let (x, y, z) = cam.project(centre, W, H)?;
    let ink = ink_override.unwrap_or_else(|| ink_at(centre, V3::ZERO, cam.eye, light));
    Some(CDraw { depth: z, a: (x, y), b: (x, y), ra: radius * cam.focal / z, rb: radius * cam.focal / z, ink_a: ink, ink_b: ink })
}

/// Apparent hand height in LEDs above which knuckle bumps and nails start
/// showing - the same "decide the level of detail by projected size" rule
/// `flock` and `skeletons` both use, rather than a size knob.
const DETAIL_AREA: (f32, f32) = (7.0, 16.0);

/// Keep the hand's own root inside the box's walkable floor, whatever a
/// clip's own authored coordinates say - a render-time safety net (a
/// placeholder or a converter fit is not guaranteed to stay inside the
/// walls the way `skeletons`' own choreography clamps its actors to), using
/// `box_scene`'s own bounds rather than a second copy of them.
fn keep_in_box(p: V3) -> V3 {
    v3(
        p.x.clamp(-box_scene::WALK_HALF_W, box_scene::WALK_HALF_W),
        p.y,
        p.z.clamp(box_scene::WALK_NEAR_Z, box_scene::WALK_FAR_Z),
    )
}

fn gather_draws(posed: &Posed, light: f32, cam: &Camera) -> Vec<CDraw> {
    let joints = posed.pose.solve(keep_in_box(posed.root_pos), posed.root_frame, HAND_H);
    let mut draws = Vec::new();

    let Some((_, _, z_ref)) = cam.project(joints.wrist, W, H) else { return draws };
    let span = HAND_H * cam.focal / z_ref;
    let detail = box_scene::smooth(DETAIL_AREA.0, DETAIL_AREA.1, span);

    for bone in joints.bones(HAND_H) {
        if let Some(d) = project_capsule(cam, bone.a, bone.b, bone.ra, bone.rb, light) {
            draws.push(d);
        }
    }

    // A palm pad: fan capsules between neighbouring metacarpal anchors so
    // the palm reads as one rounded surface rather than four separate
    // struts meeting at the wrist (the card: "the surface is a real hand,
    // not capsules" - this is the cheap, honest way this render gets
    // closer to that within the coverage-buffer technique `skeletons`
    // already proved out; see the final report for where this still falls
    // short of a true skinned mesh).
    for i in 0..3 {
        let a = joints.fingers[i][0];
        let b = joints.fingers[i + 1][0];
        if let Some(d) = project_capsule(cam, a, b, hand_rig::PALM_R * HAND_H * 0.85, hand_rig::PALM_R * HAND_H * 0.85, light) {
            draws.push(d);
        }
    }

    if detail > 0.08 {
        // Knuckle bumps: a small disc at every PIP/DIP so a curled finger
        // reads as jointed rather than a smooth tapered cone.
        for i in 0..4 {
            for &p in &[joints.fingers[i][1], joints.fingers[i][2]] {
                if let Some(d) = project_disc(cam, p, 0.028 * HAND_H, light, None) {
                    draws.push(d);
                }
            }
        }
    }
    if detail > 0.25 {
        // Nails: a small, brighter disc on the back of each fingertip.
        for i in 0..4 {
            let nail_pos = joints.fingers[i][3].add(joints.tip_frame[i].fwd.scale(-0.012 * HAND_H));
            if let Some(mut d) = project_disc(cam, nail_pos, 0.020 * HAND_H, light, Some(NAIL_INK)) {
                d.depth -= 0.001; // in front of the fingertip it sits on
                draws.push(d);
            }
        }
        let nail_pos = joints.thumb[3].add(joints.thumb_tip_frame.fwd.scale(-0.012 * HAND_H));
        if let Some(mut d) = project_disc(cam, nail_pos, 0.022 * HAND_H, light, Some(NAIL_INK)) {
            d.depth -= 0.001;
            draws.push(d);
        }
    }

    draws.sort_by(|p, q| q.depth.total_cmp(&p.depth));
    draws
}

/// Motion-blur sub-samples across the frame's own interval, interpolated
/// between the choreographer's two most recent fixed-step snapshots (see
/// `choreo.rs`'s doc for why this is an interpolation between two already-
/// simulated moments rather than a re-evaluation at an arbitrary time the
/// way `skeletons`' purely-functional `Actor::pos_at` allows: the player's
/// contact pinning is real, once-a-step state).
const BLUR_SAMPLES: usize = 4;

/// A tiny, ever-present idle tremor on the thumb, a direct function of real
/// engine time (`ctx.t`) rather than the choreographed sim time - so the
/// picture keeps changing (however slightly) between any two different
/// moments even when the choreography itself is mid-hold or, worse,
/// happens to land on the exact same phase of a looping clip twice (an
/// exact coincidence `crates/art/tests/rate.rs`'s own "the picture moved"
/// precondition caught once: `pace`'s default and `wave`'s period landed
/// two of the test's own comparison instants on the identical loop phase,
/// giving a byte-identical frame that had nothing wrong with it and
/// everything to do with unlucky arithmetic). The frequency is
/// deliberately not a round number, the same reason `box_scene::Camera::at`
/// isn't, so it cannot resonate with a clip's own (much rounder) duration.
///
/// This has to land on the **thumb**, not any part of the wrist or the
/// four fingers: `player.rs::pin_contacts` plants those against a fixed
/// world target whenever a clip flags them, and a fixed target is exactly
/// what active contact IK is *for* - it would counter-steer a wrist tremor
/// right back out again (found by testing against `rate.rs` directly, not
/// by inspection: an earlier version of this tremored the wrist and
/// `rest`'s own four-fingers-planted pose cancelled every bit of it). The
/// thumb is never pinned by any shipped clip (`pin_contacts`'s own doc
/// on that honest gap), so nothing ever fights this.
const IDLE_HZ: f64 = 0.083;
const IDLE_AMPLITUDE: f32 = 0.15;

fn interp(prev: &Posed, curr: &Posed, u: f32, t: f64) -> Posed {
    let mut pose = clip::lerp_pose(&prev.pose, &curr.pose, u);
    pose.thumb.cmc_flex += IDLE_AMPLITUDE * ((t * IDLE_HZ * std::f64::consts::TAU) as f32).sin();
    let pose = pose.clamp();
    Posed {
        root_pos: prev.root_pos.lerp(curr.root_pos, u),
        root_rot: prev.root_rot.slerp(curr.root_rot, u),
        root_frame: geom::Frame::IDENTITY.rotate_q(prev.root_rot.slerp(curr.root_rot, u)),
        pose,
        contacts: curr.contacts,
        clip_name: curr.clip_name.clone(),
    }
}

impl Patch for Thing {
    fn playing(&self) -> Option<Playing> {
        Some(self.choreo.playing())
    }

    fn act(&mut self, action: &str) {
        self.choreo.act(action);
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.advance(ctx);
        let color = ctx.get("color");
        let light = ctx.get("light");

        let cam = Camera::at(ctx.t, W);
        let (prev, curr) = self.choreo.snapshot();
        let Some(curr) = curr else {
            // Nothing simulated yet (should not happen after `advance`,
            // which always samples at least once) - an honestly black
            // frame rather than a panic on a first-frame edge case.
            let palette = vec![Rgb::BLACK];
            return Frame::Indexed { palette, indices: vec![0; N] };
        };
        let prev = prev.unwrap_or(curr);
        let shadow_pos = (curr.root_pos.x, curr.root_pos.z);

        let bg_bands = box_scene::bands(color);
        let hand = hand_lch(color);
        let mut palette = Vec::with_capacity(box_scene::BANDS * INK_LEVELS);
        for band in &bg_bands {
            for k in 0..INK_LEVELS {
                let t = k as f32 / (INK_LEVELS - 1) as f32;
                palette.push(paint(mix(*band, hand, t)));
            }
        }

        // Nails paint within this same ramp: `NAIL_INK` is near its top end
        // (`ink_at`'s own floor and range mean an ordinary lit knuckle never
        // quite reaches it), so a nail reads as a small, real highlight
        // rather than needing a second palette entry.
        let covers: Vec<Coverage> = (0..BLUR_SAMPLES)
            .map(|i| {
                let u = (i as f32 + 0.5) / BLUR_SAMPLES as f32;
                let posed = interp(prev, curr, u, ctx.t);
                let draws = gather_draws(&posed, light, &cam);
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
                let raw = (box_scene::brightness_at(&cam, x as f32 + 0.5, y as f32 + 0.5, W, H, std::slice::from_ref(&shadow_pos)) * light).clamp(0.0, 1.0);
                let band = (raw * (bands_n - 1) as f32 + bg_dither.threshold(x, y)).round().clamp(0.0, (bands_n - 1) as f32) as usize;
                let ink = covers.iter().map(|c| c.pixel(x, y)).sum::<f32>() / BLUR_SAMPLES as f32;
                let k = (ink * (INK_LEVELS - 1) as f32 + ink_dither.threshold(x, y)).round().clamp(0.0, (INK_LEVELS - 1) as f32) as usize;
                (band * INK_LEVELS + k) as u8
            })
            .collect::<Vec<u8>>();

        Frame::Indexed { palette, indices }
    }
}

fn mix_hue(a: f32, b: f32, t: f32) -> f32 {
    a + ((b - a + 540.0).rem_euclid(360.0) - 180.0) * t
}

fn mix(a: Lch, b: Lch, t: f32) -> Lch {
    let (ha, hb) = (if a.0 < 0.01 { b.2 } else { a.2 }, if b.0 < 0.01 { a.2 } else { b.2 });
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, mix_hue(ha, hb, t))
}

const FLOOR_L: f32 = 0.02;

fn paint((l, c, h): Lch) -> Rgb {
    if l < FLOOR_L {
        Rgb::BLACK
    } else {
        oklch(l, c, h)
    }
}

#[cfg(test)]
mod tests;
