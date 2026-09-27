//! The clip player: samples one clip at a time, anchors it wherever the
//! hand currently is (so root motion carries across a switch and across a
//! looping clip's own seam rather than teleporting), blends a switch by
//! **inertialization** rather than a two-clip cross-fade (the card's own
//! word for the technique - Bollo, GDC 2018), and pins any fingertip
//! flagged as planted so a walk does not skate.
//!
//! **Inertialization**, briefly: at the moment of a cut, take the outgoing
//! pose's *offset* from the incoming clip's own pose at the chosen entry
//! point, and its *velocity* offset, and decay both smoothly to zero over a
//! short window - added on top of the incoming clip, which is sampled
//! exactly as it would be alone. The decay curve here is a cubic Hermite
//! matched to the offset and velocity at `t=0` and to zero position *and*
//! zero velocity at `t=T` (`hermite_decay`), which is what makes the
//! blended pose continuous in both value and rate at the cut - no pop - and
//! the transition from *any* two moments in *any* two clips, not a pair of
//! pre-authored ones.

use super::clip::{self, Clip, PoseVel};
use super::contact_ik;
use super::geom::{v3, Frame, Quat, V3};
use super::hand_rig::{self, Pose};
use std::collections::BTreeMap;

/// How long a cut takes to decay away, seconds. Short enough to read as a
/// cut rather than a cross-fade, long enough that even a fast hand's swing
/// does not visibly kink.
pub const BLEND_TIME: f32 = 0.22;

/// Candidates per second scanned for the entry-point search (the card:
/// "cutting at frames where pose and velocity already match"). Clips are
/// short (a second or two), so a fine grid costs nothing.
const MATCH_RATE: f32 = 40.0;

/// A cubic Hermite decay from `(x0, v0)` at `u = 0` to `(0, 0)` at `u = 1`,
/// evaluated at `u` (`t / duration`, already clamped by the caller). Used
/// once per scalar channel (root position's three axes, the rotation
/// offset's one angle, all 23 joint-angle channels) - the same curve every
/// time, which is the point of keeping it generic (`Pose::to_array`'s own
/// doc makes the same argument for the lerp and the velocity finite
/// difference).
fn hermite_decay(x0: f32, v0: f32, u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let h00 = 2.0 * u * u * u - 3.0 * u * u + 1.0;
    let h10 = u * u * u - 2.0 * u * u + u;
    h00 * x0 + h10 * v0
}

/// The decaying offset added on top of the incoming clip while a blend is
/// in flight. `rot_axis`/`rot_angle0`/`rot_v0` treat the root's orientation
/// offset as a single fixed-axis rotation that shrinks to nothing (Bollo's
/// own simplification for a quaternion offset - `geom.rs`'s doc has the
/// fuller argument for why a fixed axis is fine over a short blend).
struct Blend {
    /// The absolute sim time of the cut - `elapsed` is computed from
    /// `sim_t - cut_sim_t` at read time, not accumulated frame by frame
    /// (an earlier version added a `dt` each call, which left the very
    /// first sample after a cut already a whole step into the decay
    /// rather than at `u = 0`; see the final report for the pop this
    /// caused before it was pinned by
    /// `switching_clips_is_continuous_in_pose_and_velocity_at_the_cut`).
    cut_sim_t: f64,
    duration: f32,
    pos0: V3,
    posv0: V3,
    rot_axis: V3,
    rot_angle0: f32,
    rot_v0: f32,
    pose0: [f32; 23],
    posev0: [f32; 23],
}

impl Blend {
    fn start(cut_sim_t: f64, duration: f32, from: PoseVel, to: PoseVel) -> Blend {
        let pos0 = from.pos.sub(to.pos);
        let posv0 = from.vel.sub(to.vel);
        let diff = from.rot.mul(to.rot.conjugate());
        let (rot_axis, rot_angle0) = diff.to_axis_angle();
        // The angular-velocity offset is a genuine 3-vector; projected onto
        // the diff's own axis because the decay below only has one scalar
        // to shrink (see this module's doc on why a fixed axis is enough
        // over a short blend). Off-axis components are left alone - a
        // small, stated approximation, not a hidden one.
        let rot_v0 = from.rot_vel.sub(to.rot_vel).dot(rot_axis);
        let (p0, pv0) = (from.pose.to_array(), from.pose_vel);
        let (t0, tv0) = (to.pose.to_array(), to.pose_vel);
        let mut pose0 = [0.0; 23];
        let mut posev0 = [0.0; 23];
        for i in 0..23 {
            pose0[i] = p0[i] - t0[i];
            posev0[i] = pv0[i] - tv0[i];
        }
        Blend { cut_sim_t, duration, pos0, posv0, rot_axis, rot_angle0, rot_v0, pose0, posev0 }
    }

    fn done(&self, sim_t: f64) -> bool {
        (sim_t - self.cut_sim_t) as f32 >= self.duration
    }

    /// The offset to add to the incoming clip's own sample at this moment.
    fn offset(&self, sim_t: f64) -> (V3, Quat, [f32; 23]) {
        let elapsed = (sim_t - self.cut_sim_t) as f32;
        let u = elapsed / self.duration.max(1e-4);
        // `v0` is pre-scaled by `duration` before it ever reaches
        // `hermite_decay` (`h10(u)` is dimensionless in `u`, and `v0` is
        // per real second, so the tangent needs one factor of `duration`
        // to land in the same units `h00(u) * x0` is already in) - the
        // same convention the rotation and pose terms below use, and an
        // earlier version of this scaled the *whole* result by `duration`
        // instead, which also (wrongly) scaled the `x0` term; harmless
        // today only because `x0` is always exactly zero by construction
        // (`Performance::new`'s own doc on why), so it never showed up in
        // a test.
        let pos = v3(
            hermite_decay(self.pos0.x, self.posv0.x * self.duration, u),
            hermite_decay(self.pos0.y, self.posv0.y * self.duration, u),
            hermite_decay(self.pos0.z, self.posv0.z * self.duration, u),
        );
        let angle = hermite_decay(self.rot_angle0, self.rot_v0 * self.duration, u);
        let rot = if self.rot_angle0.abs() > 1e-6 || self.rot_v0.abs() > 1e-6 { Quat::from_axis_angle(self.rot_axis, angle) } else { Quat::IDENTITY };
        let mut pose = [0.0; 23];
        for ((p, x0), v0) in pose.iter_mut().zip(self.pose0).zip(self.posev0) {
            *p = hermite_decay(x0, v0 * self.duration, u);
        }
        (pos, rot, pose)
    }
}

/// One performance: a clip, anchored to wherever the hand was placed when
/// it began, with root motion (including a translating loop's own seam)
/// carried forward rather than reset.
struct Performance {
    clip_name: String,
    start_sim_t: f64,
    placement: V3,
    clip_origin: V3,
    loop_delta: V3,
    duration: f32,
}

impl Performance {
    /// `anchor` is where the hand should actually be the instant this
    /// performance begins (usually wherever the outgoing one left it);
    /// `entry_pos` is where the clip's own data says it is at `entry_t` -
    /// the two differ whenever the motion-matching search does not land
    /// exactly on frame zero, so `placement` absorbs that difference once,
    /// up front, rather than leaving a residual for `Blend` to paper over
    /// for the whole transition.
    fn new(clip: &Clip, name: &str, sim_t: f64, anchor: V3, entry_t: f32, entry_pos: V3) -> Performance {
        let origin = clip.frames[0].root_pos_v3();
        let last = clip.frames[clip.frames.len() - 1].root_pos_v3();
        Performance {
            clip_name: name.to_string(),
            // `entry_t` seconds into the clip already happened "before"
            // `sim_t`, so the clip's own zero is that far in the past.
            start_sim_t: sim_t - f64::from(entry_t),
            placement: anchor.sub(entry_pos.sub(origin)),
            clip_origin: origin,
            loop_delta: last.sub(origin),
            duration: clip.duration().max(1e-3),
        }
    }

    fn local_t(&self, sim_t: f64) -> (f32, f32) {
        let elapsed = (sim_t - self.start_sim_t) as f32;
        let n = (elapsed / self.duration).floor().max(0.0);
        (elapsed - n * self.duration, n)
    }

    fn sample(&self, clip: &Clip, sim_t: f64) -> (V3, Quat, Pose, [bool; 5]) {
        let (local_t, n) = self.local_t(sim_t);
        let s = clip.sample(local_t);
        let mut pos = self.placement.add(s.root_pos.sub(self.clip_origin)).add(self.loop_delta.scale(n));
        // Depth (`z`, how far into the box) is the one axis `placement`
        // does *not* carry forward from the outgoing performance: every
        // clip is authored at its own considered distance from the camera
        // (near for a gesture at the glass, further for a walk across),
        // and re-anchoring `z` the way `x`/`y` are re-anchored (so a walk
        // picks up exactly where the hand already is) would instead freeze
        // the *whole run* at whatever the very first performance's `z`
        // happened to be - which is what shipped once, and is why the
        // hand rendered as a handful of barely-lit dots for the rest of
        // the run rather than ever reading as a hand (see the final
        // report's honest note on this). `Blend`'s decaying offset still
        // smooths the resulting jump if two consecutive clips are authored
        // at different distances.
        pos.z = s.root_pos.z;
        (pos, s.root_rot, s.pose, s.contacts)
    }
}

/// Where a planted fingertip is pinned in the box, recorded the frame its
/// flag turned on and held until it turns off.
type Planted = [Option<V3>; 4];

pub struct ClipPlayer {
    perf: Option<Performance>,
    blend: Option<Blend>,
    planted: Planted,
    /// The hand's own scale, metres - needed to plant a fingertip in the
    /// same units the clip's own root motion is in.
    hand_h: f32,
    /// Where the very first performance is anchored, before there is any
    /// "wherever the hand currently is" to anchor to - a small, seeded
    /// lateral offset (the card's "another one like this" - `skeletons`
    /// varies its actors' starting places by seed the same way) rather
    /// than a fixed spot every run starts from identically.
    start_anchor: V3,
}

/// What the player hands the renderer for one frame.
#[derive(Clone)]
pub struct Posed {
    pub root_pos: V3,
    pub root_rot: Quat,
    pub root_frame: Frame,
    pub pose: Pose,
    pub contacts: [bool; 5],
    pub clip_name: String,
}

impl ClipPlayer {
    #[must_use]
    pub fn new(hand_h: f32, seed: u64) -> ClipPlayer {
        let mut rng = crate::rng::Rng::new(seed ^ 0xb0b0_1234_5678_9abc);
        let start_anchor = v3(rng.range(-0.15, 0.15), 0.0, 0.0);
        ClipPlayer { perf: None, blend: None, planted: [None; 4], hand_h, start_anchor }
    }

    /// Cut to `name` at `sim_t`, anchored at wherever the hand currently is
    /// (or the box's centre, for the very first performance). Finds the
    /// entry point in the new clip whose pose and velocity are closest to
    /// the outgoing moment (the card's "motion-matching search over
    /// candidate entry frames"), then seeds an inertialization blend from
    /// the discontinuity that is left over.
    pub fn switch_to(&mut self, clips: &BTreeMap<String, Clip>, name: &str, sim_t: f64) {
        let Some(new_clip) = clips.get(name) else { return };
        let from = self.pose_vel_now(clips, sim_t);
        let anchor = from.map_or(self.start_anchor, |f| f.pos);
        let entry_t = best_entry(new_clip, from);
        let to = clip::pose_vel(new_clip, entry_t, 1.0 / 200.0);
        if let Some(from) = from {
            // `to.pos` is in the *new clip's own* local coordinates; `from`
            // is already in world/box coordinates (`pose_vel_now` placed
            // it there). `Performance::new`'s own `placement` is exactly
            // what cancels that gap for `x`/`y` on every sample *after*
            // this one, but `Blend::start` wants both sides in the same
            // frame *right now* - so `x`/`y` get moved into world
            // coordinates too (`anchor`, by construction: see
            // `Performance::new`'s doc). An earlier version did this for
            // `z` as well, which reintroduced exactly the pop
            // inertialization is supposed to remove whenever `entry_t` was
            // not frame zero (pinned by
            // `switching_clips_is_continuous_in_pose_and_velocity_at_the_cut`).
            // `z` is left as the new clip's own *raw* depth - `sample`'s
            // own doc explains why `z` is never re-anchored - so a real
            // difference in the two clips' own considered distances (a
            // walk further back, a gesture pushed right up to the glass)
            // is exactly the discontinuity the blend decays, rather than
            // one artificially erased before it ever saw it.
            let to_world = PoseVel { pos: v3(anchor.x, anchor.y, to.pos.z), ..to };
            self.blend = Some(Blend::start(sim_t, BLEND_TIME, from, to_world));
        }
        self.perf = Some(Performance::new(new_clip, name, sim_t, anchor, entry_t, to.pos));
        self.planted = [None; 4];
    }

    fn pose_vel_now(&self, clips: &BTreeMap<String, Clip>, sim_t: f64) -> Option<PoseVel> {
        let perf = self.perf.as_ref()?;
        let clip = clips.get(&perf.clip_name)?;
        let (local_t, n) = perf.local_t(sim_t);
        let mut pv = clip::pose_vel(clip, local_t, 1.0 / 200.0);
        let raw_z = pv.pos.z;
        pv.pos = perf.placement.add(pv.pos.sub(perf.clip_origin)).add(perf.loop_delta.scale(n));
        pv.pos.z = raw_z; // `z` is never re-anchored - see `Performance::sample`'s doc
        Some(pv)
    }

    /// Sample the current performance at `sim_t`, with any in-flight blend
    /// folded in and any planted fingertip pinned.
    /// `_dt` is unused now that the blend times itself off `sim_t` alone
    /// (see `Blend`'s own doc on why accumulating `dt` was a bug); kept in
    /// the signature so `choreo.rs`'s call site does not need to change
    /// shape along with the fix.
    pub fn sample(&mut self, clips: &BTreeMap<String, Clip>, sim_t: f64, _dt: f32) -> Option<Posed> {
        let perf = self.perf.as_ref()?;
        let clip = clips.get(&perf.clip_name)?;
        let clip_name = perf.clip_name.clone();
        let (mut root_pos, mut root_rot, mut pose, contacts) = perf.sample(clip, sim_t);

        if let Some(blend) = &self.blend {
            let (pos_off, rot_off, pose_off) = blend.offset(sim_t);
            root_pos = root_pos.add(pos_off);
            root_rot = rot_off.mul(root_rot);
            let mut arr = pose.to_array();
            for i in 0..23 {
                arr[i] += pose_off[i];
            }
            pose = Pose::from_array(arr).clamp();
            if blend.done(sim_t) {
                self.blend = None;
            }
        }

        let root_frame = Frame::IDENTITY.rotate_q(root_rot);
        self.pin_contacts(root_pos, root_frame, &mut pose, contacts);

        Some(Posed { root_pos, root_rot, root_frame, pose, contacts, clip_name })
    }

    /// Fingers 0-3 (index/middle/ring/pinky - `hand_rig::Joints::tips`'
    /// `contacts[1..=4]`) get real contact IK; the thumb (`contacts[0]`)
    /// does not yet - no placeholder or shot-list clip plants it, so there
    /// is nothing to prove this against, and it is left as an honest gap
    /// rather than an untested guess (see the final report).
    fn pin_contacts(&mut self, root: V3, root_frame: Frame, pose: &mut Pose, contacts: [bool; 5]) {
        for i in 0..4 {
            let flag = contacts[i + 1];
            if !flag {
                self.planted[i] = None;
                continue;
            }
            let fp = pose.fingers[i];
            // The *spread-free* reference frame (`finger_anchor_frame` with
            // `spread = 0`): `ref_frame.up` is the one axis spread never
            // moves (a pure yaw about it), so it is the fixed pole a
            // target's own azimuth is measured against below - see this
            // block's own doc for why solving `spread` as a third DOF (not
            // just `mcp`/`pip`) is what a moving root actually needs. The
            // anchor itself does not depend on spread either way
            // (`hand_rig.rs`'s own doc on the anchor being a fixed offset).
            let (anchor, ref_frame) = hand_rig::finger_anchor_frame(root, root_frame, pose.wrist, 0.0, i, self.hand_h);
            let l1 = hand_rig::PROX_LEN[i] * self.hand_h;
            let (mid, dist) = (hand_rig::MID_LEN[i] * self.hand_h, hand_rig::DIST_LEN[i] * self.hand_h);
            // The two-link IK below solves the `MCP`-`PIP` pair (plus
            // `spread`, below); `DIP` is left to the animation (one target
            // point cannot pin four independent joints), but its *fixed*
            // bend still moves the tip, so it cannot simply be dropped
            // either - folded into an *effective* second link instead: the
            // straight `MID` bone plus the `DIST` bone kinked by `dip`, as
            // one vector, with its own length (`eff_l2`) and its own angle
            // off the `MID` bone's own direction (`dip_off`). Solving with
            // `eff_l2` and then subtracting `dip_off` from the returned
            // angle is what makes the *tip* - not just the `PIP` joint -
            // land on `target`.
            let (dc, ds) = fp.dip.sin_cos();
            let (kx, ky) = (mid + dist * dc, dist * ds);
            let eff_l2 = kx.hypot(ky);
            let dip_off = ky.atan2(kx);
            let target = *self.planted[i].get_or_insert_with(|| {
                // First frame of this plant: the *real* (three-segment,
                // spread included) forward kinematics of the animated,
                // un-pinned pose - not the two-link approximation - so the
                // pin does not itself introduce a pop.
                let a1 = fp.mcp;
                let a2 = a1 + fp.pip;
                let a3 = a2 + fp.dip;
                let (x, y) = (l1 * a1.cos() + mid * a2.cos() + dist * a3.cos(), l1 * a1.sin() + mid * a2.sin() + dist * a3.sin());
                let horiz = ref_frame.right.scale(fp.spread.sin() * y).add(ref_frame.fwd.scale(fp.spread.cos() * y));
                anchor.add(ref_frame.up.scale(x)).add(horiz)
            });
            // Everything a moving root can drag `target` away from is a
            // fixed-`up`-axis reach problem: `spread` (a yaw about
            // `ref_frame.up`) can always be chosen so the finger's own
            // bend-plane contains `d`'s horizontal direction exactly
            // (`az`), at which point the ordinary flex-only two-link IK
            // reaches any point whose vertical component and horizontal
            // *distance* are within `l1 + eff_l2` - which is why an
            // earlier version of this, solving only `mcp`/`pip` at the
            // clip's own fixed `spread`, could not correct a root that
            // drifted sideways at all (pinned by
            // `a_planted_fingertip_does_not_slide_while_the_root_walks`,
            // which caught exactly that).
            let d = target.sub(anchor);
            let (right_c, fwd_c) = (d.dot(ref_frame.right), d.dot(ref_frame.fwd));
            let az = right_c.atan2(fwd_c);
            let (a, b) = (d.dot(ref_frame.up), right_c.hypot(fwd_c));
            let (mcp_flex, kink_flex) = contact_ik::solve_2link(a, b, l1, eff_l2);
            pose.fingers[i].spread = az;
            pose.fingers[i].mcp = mcp_flex;
            pose.fingers[i].pip = kink_flex - dip_off;
        }
        *pose = pose.clamp();
    }
}

/// The entry point in `clip` whose pose and velocity best match `from`
/// (`None` for the very first performance, which just starts at the front).
/// A plain grid search - clips are short, so this is cheap - scoring by a
/// weighted sum of squared differences: root velocity direction and speed
/// matter most (a walk cutting into a walk should not change gear), joint
/// angles matter, joint angular rates least (least visible at 64x32).
fn best_entry(clip: &Clip, from: Option<PoseVel>) -> f32 {
    let Some(from) = from else { return clip.frames[0].t };
    let dur = clip.duration();
    let n = ((dur * MATCH_RATE).round() as usize).max(1);
    let mut best_t = clip.frames[0].t;
    let mut best_cost = f32::INFINITY;
    for i in 0..=n {
        let t = clip.frames[0].t + dur * i as f32 / n as f32;
        let to = clip::pose_vel(clip, t, 1.0 / 200.0);
        let pos_cost = from.pos.sub(to.pos).len2();
        let vel_cost = from.vel.sub(to.vel).len2();
        let (fa, ta) = (from.pose.to_array(), to.pose.to_array());
        let mut pose_cost = 0.0;
        let mut vel_pose_cost = 0.0;
        for k in 0..23 {
            pose_cost += (fa[k] - ta[k]).powi(2);
            vel_pose_cost += (from.pose_vel[k] - to.pose_vel[k]).powi(2);
        }
        let cost = 3.0 * pos_cost + 2.0 * vel_cost + pose_cost + 0.3 * vel_pose_cost;
        if cost < best_cost {
            best_cost = cost;
            best_t = t;
        }
    }
    best_t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patches::thing::clip::placeholders;

    fn clips() -> BTreeMap<String, Clip> {
        placeholders::all()
    }

    #[test]
    fn a_first_switch_starts_at_the_clips_own_front() {
        let mut p = ClipPlayer::new(0.19, 7);
        p.switch_to(&clips(), "wave", 0.0);
        let f = p.sample(&clips(), 0.0, 1.0 / 30.0).expect("posed");
        assert_eq!(f.clip_name, "wave");
    }

    /// The acceptance test the card asks for by name: "the player's
    /// inertialized transition between two placeholder clips shows no pop",
    /// checked here as pose *and* velocity continuity at the switch
    /// instant, not just by eye.
    #[test]
    fn switching_clips_is_continuous_in_pose_and_velocity_at_the_cut() {
        let clips = clips();
        let mut p = ClipPlayer::new(0.19, 7);
        p.switch_to(&clips, "walk-scuttle", 0.0);
        // Run the outgoing clip a while so it has real velocity, not a
        // frame-zero standing start.
        let mut before = None;
        for i in 0..20 {
            let t = i as f64 * (1.0 / 30.0);
            before = p.sample(&clips, t, 1.0 / 30.0);
        }
        let before = before.expect("posed");
        let dt = 1.0 / 30.0;
        let t_cut = 20.0 * dt as f64;
        p.switch_to(&clips, "point", t_cut);
        let at_cut = p.sample(&clips, t_cut, dt).expect("posed");
        let just_after = p.sample(&clips, t_cut + f64::from(dt), dt).expect("posed");

        // Position: the cut itself must not jump.
        assert!(before.root_pos.sub(at_cut.root_pos).len() < 0.01, "position popped at the cut: {:?} vs {:?}", before.root_pos, at_cut.root_pos);

        // Velocity: estimate it either side of the cut by finite difference
        // and check it does not jump either - the actual claim
        // inertialization makes over a cross-fade.
        let v_before = at_cut.root_pos.sub(before.root_pos).scale(1.0 / dt);
        let v_after = just_after.root_pos.sub(at_cut.root_pos).scale(1.0 / dt);
        assert!(v_before.sub(v_after).len() < 3.0, "velocity should not jump sharply at the cut: {v_before:?} vs {v_after:?}");
    }

    #[test]
    fn a_planted_fingertip_does_not_slide_while_the_root_walks() {
        let clips = clips();
        let mut p = ClipPlayer::new(0.19, 7);
        p.switch_to(&clips, "walk-scuttle", 0.0);
        let mut prior_pin: Option<V3> = None;
        for i in 0..15 {
            let t = i as f64 * (1.0 / 60.0);
            let f = p.sample(&clips, t, 1.0 / 60.0).expect("posed");
            if f.contacts[1] {
                // index planted: recompute its world tip and check it barely
                // moves frame to frame while the flag holds.
                let j = f.pose.solve(f.root_pos, f.root_frame, 0.19);
                let tip = j.tips()[1]; // index fingertip - `tips()` is `[thumb, index, middle, ring, pinky]`
                if let Some(prev) = prior_pin {
                    assert!(tip.sub(prev).len() < 0.01, "planted tip slid: {tip:?} vs {prev:?}");
                }
                prior_pin = Some(tip);
            } else {
                prior_pin = None;
            }
        }
    }
}
