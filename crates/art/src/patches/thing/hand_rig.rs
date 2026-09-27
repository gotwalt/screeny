//! The hand rig: a right hand cut off at the wrist, matched to MediaPipe's
//! 21-landmark layout so a clip captured through the Hand Landmarker
//! retargets directly (card 333's own requirement) - landmark 0 is the
//! wrist; 1-4 are the thumb (CMC, MCP, IP, TIP); 5-8, 9-12, 13-16, 17-20 are
//! the index, middle, ring and pinky (MCP, PIP, DIP, TIP each).
//!
//! Forward kinematics follows `patches/skeletons/rig.rs`'s own pattern
//! exactly (a fixed anchor off the parent frame, a joint's local [`Angles`]
//! rotate the frame the next bone extends along) because it is the house
//! technique for this - a hinge (PIP, DIP) is one non-zero angle; a two-axis
//! joint (MCP) is two; the thumb's CMC is the saddle the card asks for
//! (flexion *and* abduction, the two axes that let the thumb oppose the
//! fingers). [`Pose::clamp`] enforces the joint limits at the one place
//! every pose passes through - built by hand, sampled from a clip, or
//! nudged by the player's inertialization offset.
//!
//! Bone lengths are fractions of `h`, the hand's own scale (wrist to a
//! relaxed middle fingertip) - the same convention `rig.rs` uses for a
//! skeleton's height, so `thing`'s render can size the hand the way
//! `skeletons` sizes a figure: by how many LEDs it projects to.

use super::geom::{Angles, Frame, V3};
#[cfg(test)]
use super::geom::v3;

/// The hand's own scale, metres, wrist to a relaxed middle fingertip - the
/// one place this number lives; `player.rs`, `choreo.rs` and `mod.rs` all
/// read it from here rather than each keeping their own copy.
///
/// **Not** a real hand's ~0.19 m: `box_scene.rs`'s camera (`Camera::at`,
/// shared byte-for-byte with `skeletons`) sits at `eye.z = -1.55`, *behind*
/// the box's own front window (`z = 0`), because it was tuned to frame a
/// ~1.6-1.8 m standing figure across the box's walkable floor
/// (`WALK_NEAR_Z` to `WALK_FAR_Z`). A real-scale hand anywhere on that same
/// floor sits at an actual camera distance of at least `1.55 m` before the
/// box's own depth is even added, and projects to a couple of LEDs at
/// best - which is exactly what shipped once (a handful of barely-lit
/// dots, not a hand; see the card's Log for 2026-09-26) before this was
/// measured rather than eyeballed. `HAND_H` is instead the size that reads
/// as a "luxurious" hand on this same camera - enough LEDs across a bone's
/// own width, up close, that it reads as a filled limb rather than a
/// scatter of sub-pixel strokes - chosen the same way `flock`/`skeletons`
/// choose a subject's *apparent* size in LEDs rather than its real-world
/// one, not a claim that Thing is over a metre long.
pub const HAND_H: f32 = 1.05;

/// One finger's two joints below the MCP anchor, plus the MCP's own local
/// rotation. `spread` is abduction (fingers apart, +away from the thumb);
/// `mcp`/`pip`/`dip` are flexion, all hinge-like except `mcp`'s spread axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FingerPose {
    pub spread: f32,
    pub mcp: f32,
    pub pip: f32,
    pub dip: f32,
}

/// The thumb: CMC is the saddle (two free axes), MCP and IP are hinges.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThumbPose {
    pub cmc_flex: f32,
    pub cmc_abduct: f32,
    pub mcp: f32,
    pub ip: f32,
}

/// Every joint's local motion, `[index, middle, ring, pinky]` for the plain
/// fingers - fifteen numbers, all `0.0` at a relaxed flat-on-the-table rest
/// (matching the shot list's rest pose). A clip is this sampled over time,
/// plus root motion and contact flags (see `clip.rs`).
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pose {
    pub wrist: Angles,
    pub thumb: ThumbPose,
    pub fingers: [FingerPose; 4],
}

/// Anatomical joint limits, radians, picked by eye against a real hand held
/// up beside the screen - not a citation, a first pass to argue with. Every
/// [`Pose`] that reaches the renderer has been through [`Pose::clamp`], so a
/// bad keyframe, a bad fit from the converter, or an inertialization offset
/// that overshoots cannot bend a joint past what a hand actually does.
pub mod limits {
    pub const WRIST_YAW: (f32, f32) = (-0.35, 0.35);
    pub const WRIST_PITCH: (f32, f32) = (-1.2, 0.9);
    pub const WRIST_ROLL: (f32, f32) = (-0.5, 0.5);
    pub const SPREAD: (f32, f32) = (-0.35, 0.35);
    pub const MCP: (f32, f32) = (-0.15, 1.6);
    pub const PIP: (f32, f32) = (0.0, 1.7);
    pub const DIP: (f32, f32) = (0.0, 1.5);
    pub const CMC_FLEX: (f32, f32) = (-0.3, 1.0);
    pub const CMC_ABDUCT: (f32, f32) = (-0.2, 1.2);
    pub const THUMB_MCP: (f32, f32) = (0.0, 1.2);
    pub const THUMB_IP: (f32, f32) = (-0.2, 1.4);
}

fn c(v: f32, r: (f32, f32)) -> f32 {
    v.clamp(r.0, r.1)
}

impl FingerPose {
    #[must_use]
    pub fn clamp(self) -> FingerPose {
        FingerPose { spread: c(self.spread, limits::SPREAD), mcp: c(self.mcp, limits::MCP), pip: c(self.pip, limits::PIP), dip: c(self.dip, limits::DIP) }
    }
}

impl ThumbPose {
    #[must_use]
    pub fn clamp(self) -> ThumbPose {
        ThumbPose {
            cmc_flex: c(self.cmc_flex, limits::CMC_FLEX),
            cmc_abduct: c(self.cmc_abduct, limits::CMC_ABDUCT),
            mcp: c(self.mcp, limits::THUMB_MCP),
            ip: c(self.ip, limits::THUMB_IP),
        }
    }
}

impl Pose {
    #[must_use]
    pub fn clamp(self) -> Pose {
        Pose {
            wrist: Angles::new(c(self.wrist.yaw, limits::WRIST_YAW), c(self.wrist.pitch, limits::WRIST_PITCH), c(self.wrist.roll, limits::WRIST_ROLL)),
            thumb: self.thumb.clamp(),
            fingers: self.fingers.map(FingerPose::clamp),
        }
    }

    /// Every scalar joint angle, flattened - the one representation the
    /// clip sampler's lerp, the velocity finite-difference and the player's
    /// inertialization decay all share, so there is exactly one place that
    /// knows the field order (`clip.rs` and `player.rs` both build on this
    /// rather than repeating the field list three times, which is how an
    /// earlier draft of this card's code had it).
    #[must_use]
    pub fn to_array(self) -> [f32; 23] {
        let w = &self.wrist;
        let t = &self.thumb;
        let mut out = [w.yaw, w.pitch, w.roll, t.cmc_flex, t.cmc_abduct, t.mcp, t.ip, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        for i in 0..4 {
            let f = &self.fingers[i];
            out[7 + i * 4] = f.spread;
            out[7 + i * 4 + 1] = f.mcp;
            out[7 + i * 4 + 2] = f.pip;
            out[7 + i * 4 + 3] = f.dip;
        }
        out
    }

    #[must_use]
    pub fn from_array(a: [f32; 23]) -> Pose {
        Pose {
            wrist: Angles::new(a[0], a[1], a[2]),
            thumb: ThumbPose { cmc_flex: a[3], cmc_abduct: a[4], mcp: a[5], ip: a[6] },
            fingers: [0, 1, 2, 3].map(|i| FingerPose { spread: a[7 + i * 4], mcp: a[7 + i * 4 + 1], pip: a[7 + i * 4 + 2], dip: a[7 + i * 4 + 3] }),
        }
    }
}

/// Metacarpal (wrist -> MCP anchor) length and spread, per finger, fractions
/// of `h`. Index first (closer to the thumb) through pinky (`SPREAD_X`
/// increasing away from the thumb side); `PALM_LEN` shortest for the pinky,
/// longest for the middle finger, as a real hand's is.
pub const SPREAD_X: [f32; 4] = [-0.15, -0.04, 0.07, 0.17];
pub const PALM_LEN: [f32; 4] = [0.42, 0.47, 0.45, 0.40];
pub const PALM_FWD: [f32; 4] = [0.03, 0.02, 0.00, -0.02];

/// Phalanx lengths, fractions of `h`: `[index, middle, ring, pinky]`. `pub`
/// (with the anchor fractions above) so `player.rs`'s contact IK can solve
/// in the same units as this file's own FK, rather than a second copy of
/// these numbers drifting from these - see [`finger_anchor_frame`].
pub const PROX_LEN: [f32; 4] = [0.22, 0.25, 0.23, 0.17];
pub const MID_LEN: [f32; 4] = [0.13, 0.15, 0.14, 0.10];
pub const DIST_LEN: [f32; 4] = [0.09, 0.10, 0.09, 0.08];

/// Where the thumb's CMC joint sits, fixed relative to the palm frame.
const THUMB_ANCHOR: (f32, f32, f32) = (-0.22, 0.06, 0.10);
/// The thumb's resting bind: rotated out from the fingers' plane so it can
/// oppose them, the way [`super::hand_rig`]'s doc frames `hang` in `rig.rs`.
const THUMB_BIND: Angles = Angles::new(-0.55, 0.35, 0.0);
const THUMB_META_LEN: f32 = 0.14;
const THUMB_PROX_LEN: f32 = 0.13;
const THUMB_DIST_LEN: f32 = 0.10;

/// Every joint's world position, plus the frames a render needs for shading
/// and decoration (a nail, a knuckle).
pub struct Joints {
    pub wrist: V3,
    pub palm_frame: Frame,
    /// `[CMC, MCP, IP, TIP]`.
    pub thumb: [V3; 4],
    /// `[index, middle, ring, pinky]`, each `[MCP, PIP, DIP, TIP]`.
    pub fingers: [[V3; 4]; 4],
    /// The frame each fingertip's distal phalanx ends in, for a nail's
    /// placement and orientation.
    pub tip_frame: [Frame; 4],
    pub thumb_tip_frame: Frame,
}

impl Pose {
    /// Forward kinematics at hand scale `h`, root position `root` and root
    /// orientation `root_frame` (the whole hand's placement and heading -
    /// where root motion and camera-facing come from; see `clip.rs`).
    #[must_use]
    pub fn solve(&self, root: V3, root_frame: Frame, h: f32) -> Joints {
        let palm_frame = root_frame.rotate(self.wrist);
        let wrist = root;

        let mut fingers = [[V3::ZERO; 4]; 4];
        let mut tip_frame = [Frame::IDENTITY; 4];
        for i in 0..4 {
            let anchor =
                wrist.add(palm_frame.right.scale(SPREAD_X[i] * h)).add(palm_frame.up.scale(PALM_LEN[i] * h)).add(palm_frame.fwd.scale(PALM_FWD[i] * h));
            let fp = self.fingers[i];
            let mcp_frame = palm_frame.rotate(Angles::new(fp.spread, fp.mcp, 0.0));
            let pip_pos = mcp_frame.extend(anchor, PROX_LEN[i] * h);
            let pip_frame = mcp_frame.rotate(Angles::new(0.0, fp.pip, 0.0));
            let dip_pos = pip_frame.extend(pip_pos, MID_LEN[i] * h);
            let dip_frame = pip_frame.rotate(Angles::new(0.0, fp.dip, 0.0));
            let tip_pos = dip_frame.extend(dip_pos, DIST_LEN[i] * h);
            fingers[i] = [anchor, pip_pos, dip_pos, tip_pos];
            tip_frame[i] = dip_frame;
        }

        let cmc_anchor = wrist
            .add(palm_frame.right.scale(THUMB_ANCHOR.0 * h))
            .add(palm_frame.up.scale(THUMB_ANCHOR.1 * h))
            .add(palm_frame.fwd.scale(THUMB_ANCHOR.2 * h));
        let cmc_frame = palm_frame.rotate(THUMB_BIND).rotate(Angles::new(self.thumb.cmc_abduct, self.thumb.cmc_flex, 0.0));
        let thumb_mcp = cmc_frame.extend(cmc_anchor, THUMB_META_LEN * h);
        let mcp_frame = cmc_frame.rotate(Angles::new(0.0, self.thumb.mcp, 0.0));
        let thumb_ip = mcp_frame.extend(thumb_mcp, THUMB_PROX_LEN * h);
        let ip_frame = mcp_frame.rotate(Angles::new(0.0, self.thumb.ip, 0.0));
        let thumb_tip = ip_frame.extend(thumb_ip, THUMB_DIST_LEN * h);

        Joints {
            wrist,
            palm_frame,
            thumb: [cmc_anchor, thumb_mcp, thumb_ip, thumb_tip],
            fingers,
            tip_frame,
            thumb_tip_frame: ip_frame,
        }
    }
}

/// The MCP anchor and the (spread-only, pre-flex) frame a finger's flex
/// bends from - the same two quantities `Pose::solve` computes inline for
/// every finger, pulled out so `player.rs`'s contact IK can plant a
/// fingertip using this file's own numbers instead of a second copy of them
/// (`hand_rig.rs`'s own doc comment on `PROX_LEN` et al.).
#[must_use]
pub fn finger_anchor_frame(root: V3, root_frame: Frame, wrist: Angles, spread: f32, i: usize, h: f32) -> (V3, Frame) {
    let palm_frame = root_frame.rotate(wrist);
    let anchor = root.add(palm_frame.right.scale(SPREAD_X[i] * h)).add(palm_frame.up.scale(PALM_LEN[i] * h)).add(palm_frame.fwd.scale(PALM_FWD[i] * h));
    (anchor, palm_frame.rotate(Angles::new(spread, 0.0, 0.0)))
}

/// One capsule to draw, as fractions of `h` (the caller scales by drawn
/// size), mirroring `rig::Bone`.
#[derive(Clone, Copy)]
pub struct Bone {
    pub a: V3,
    pub b: V3,
    pub ra: f32,
    pub rb: f32,
}

/// A stump length behind the wrist, purely cosmetic (the "short flat-ended
/// stump" the card asks for): a fixed point along `-palm_frame.up` from the
/// wrist, so the hand does not look like it ends in a point.
pub const STUMP_LEN: f32 = 0.16;
pub const STUMP_R: f32 = 0.145;
pub const PALM_R: f32 = 0.15;
pub const WRIST_R: f32 = 0.13;

impl Joints {
    #[must_use]
    pub fn stump(&self, h: f32) -> V3 {
        self.wrist.sub(self.palm_frame.up.scale(STUMP_LEN * h))
    }

    /// Every bone to draw: the phalanges, the metacarpals (drawn fat and
    /// overlapping so they read as a solid palm, not four thin struts - see
    /// `mod.rs`'s render for the palm pad proper), and the stump.
    #[must_use]
    pub fn bones(&self, h: f32) -> Vec<Bone> {
        let bone = |a, b, ra, rb| Bone { a, b, ra, rb };
        let mut out = Vec::with_capacity(24);
        out.push(bone(self.stump(h), self.wrist, STUMP_R * h, WRIST_R * h));
        for i in 0..4 {
            let [mcp, pip, dip, tip] = self.fingers[i];
            let base_r = [0.052, 0.058, 0.055, 0.045][i];
            out.push(bone(self.wrist, mcp, PALM_R * h, base_r * h * 1.35));
            out.push(bone(mcp, pip, base_r * h, base_r * 0.78 * h));
            out.push(bone(pip, dip, base_r * 0.72 * h, base_r * 0.6 * h));
            out.push(bone(dip, tip, base_r * 0.55 * h, base_r * 0.4 * h));
        }
        out.push(bone(self.wrist, self.thumb[0], PALM_R * 0.9 * h, 0.05 * h));
        out.push(bone(self.thumb[0], self.thumb[1], 0.05 * h, 0.044 * h));
        out.push(bone(self.thumb[1], self.thumb[2], 0.044 * h, 0.036 * h));
        out.push(bone(self.thumb[2], self.thumb[3], 0.036 * h, 0.026 * h));
        out
    }

    /// The four fingertips and the thumb tip, in MediaPipe order
    /// (`[thumb, index, middle, ring, pinky]`) - the same order
    /// `clip::ClipFrame::contacts` and `player.rs`'s contact pinning use
    /// for the five flags, kept here as a convenience for tests and for a
    /// future caller that wants every tip at once rather than indexing
    /// `fingers`/`thumb` by hand.
    #[cfg(test)]
    #[must_use]
    pub fn tips(&self) -> [V3; 5] {
        [self.thumb[3], self.fingers[0][3], self.fingers[1][3], self.fingers[2][3], self.fingers[3][3]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn rand_pose(rng: &mut Rng) -> Pose {
        let mut f = |lo: f32, hi: f32| rng.range(lo, hi);
        Pose {
            wrist: Angles::new(f(-1.0, 1.0), f(-1.0, 1.0), f(-1.0, 1.0)),
            thumb: ThumbPose { cmc_flex: f(-1.0, 1.5), cmc_abduct: f(-1.0, 1.5), mcp: f(-1.0, 1.5), ip: f(-1.0, 1.5) },
            fingers: [0, 1, 2, 3].map(|_| FingerPose { spread: f(-1.0, 1.0), mcp: f(-1.0, 2.0), pip: f(-1.0, 2.0), dip: f(-1.0, 2.0) }),
        }
        .clamp()
    }

    /// FK sanity, `rig.rs`'s own test transplanted: every bone is exactly
    /// the length its constant says, whatever the (clamped) joint angles.
    #[test]
    fn bones_keep_their_lengths_whatever_the_pose() {
        let mut rng = Rng::new(11);
        for _ in 0..300 {
            let pose = rand_pose(&mut rng);
            let h = 0.19; // metres - about a real hand's length.
            let root = v3(rng.range(-1.0, 1.0), 0.0, rng.range(0.0, 3.0));
            let root_frame = Frame::heading(rng.range(-3.0, 3.0));
            let j = pose.solve(root, root_frame, h);

            let len = |a: V3, b: V3| a.sub(b).len();
            for i in 0..4 {
                let [mcp, pip, dip, tip] = j.fingers[i];
                assert!((len(j.wrist, mcp) - (PALM_LEN[i].hypot(SPREAD_X[i]).hypot(PALM_FWD[i])) * h).abs() < 2e-3, "finger {i} metacarpal");
                assert!((len(mcp, pip) - PROX_LEN[i] * h).abs() < 1e-3, "finger {i} proximal");
                assert!((len(pip, dip) - MID_LEN[i] * h).abs() < 1e-3, "finger {i} middle");
                assert!((len(dip, tip) - DIST_LEN[i] * h).abs() < 1e-3, "finger {i} distal");
            }
            assert!((len(j.thumb[1], j.thumb[2]) - THUMB_PROX_LEN * h).abs() < 1e-3, "thumb proximal");
            assert!((len(j.thumb[2], j.thumb[3]) - THUMB_DIST_LEN * h).abs() < 1e-3, "thumb distal");
        }
    }

    #[test]
    fn clamp_holds_every_joint_inside_its_anatomical_limit() {
        let mut rng = Rng::new(99);
        for _ in 0..500 {
            let pose = rand_pose(&mut rng); // already clamped by rand_pose
            assert!(pose.wrist.yaw >= limits::WRIST_YAW.0 - 1e-6 && pose.wrist.yaw <= limits::WRIST_YAW.1 + 1e-6);
            assert!(pose.wrist.pitch >= limits::WRIST_PITCH.0 - 1e-6 && pose.wrist.pitch <= limits::WRIST_PITCH.1 + 1e-6);
            for fp in pose.fingers {
                assert!(fp.spread >= limits::SPREAD.0 - 1e-6 && fp.spread <= limits::SPREAD.1 + 1e-6);
                assert!(fp.mcp >= limits::MCP.0 - 1e-6 && fp.mcp <= limits::MCP.1 + 1e-6);
                assert!(fp.pip >= limits::PIP.0 - 1e-6 && fp.pip <= limits::PIP.1 + 1e-6);
                assert!(fp.dip >= limits::DIP.0 - 1e-6 && fp.dip <= limits::DIP.1 + 1e-6);
            }
        }
    }

    #[test]
    fn finger_anchor_frame_agrees_with_pose_solve() {
        let mut rng = Rng::new(23);
        for _ in 0..50 {
            let pose = rand_pose(&mut rng);
            let root = v3(rng.range(-1.0, 1.0), 0.0, rng.range(0.0, 3.0));
            let root_frame = Frame::heading(rng.range(-3.0, 3.0));
            let h = 0.19;
            let j = pose.solve(root, root_frame, h);
            for i in 0..4 {
                let (anchor, _) = finger_anchor_frame(root, root_frame, pose.wrist, pose.fingers[i].spread, i, h);
                assert!(anchor.sub(j.fingers[i][0]).len() < 1e-4, "finger {i}");
            }
        }
    }

    #[test]
    fn pose_to_array_and_back_round_trips() {
        let mut rng = Rng::new(41);
        for _ in 0..50 {
            let pose = rand_pose(&mut rng);
            let back = Pose::from_array(pose.to_array());
            assert_eq!(pose, back);
        }
    }

    #[test]
    fn a_relaxed_rest_pose_is_flat_and_roughly_planar() {
        let pose = Pose::default();
        let j = pose.solve(V3::ZERO, Frame::heading(0.0), 0.19);
        // A flat, relaxed hand: every fingertip within a shallow band of the
        // palm's own plane (the `fwd` axis), not curled into a fist or
        // splayed vertically.
        for i in 0..4 {
            let tip = j.fingers[i][3];
            let out_of_plane = tip.sub(j.wrist).dot(j.palm_frame.fwd).abs();
            assert!(out_of_plane < 0.05, "finger {i} should stay near the palm's plane at rest: {out_of_plane}");
        }
    }
}
