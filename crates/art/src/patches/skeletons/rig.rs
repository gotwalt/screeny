//! The skeleton rig: forward kinematics from a [`Pose`] to world joint
//! positions, and the bones to draw from them.
//!
//! Seventeen joints, the ones the card asks for: pelvis, spine, chest (where
//! the brief says "shoulders"), neck, skull, two shoulders, two elbows, two
//! wrists, two hips, two knees, two ankles. The jaw and the eye sockets hang
//! off the skull as small, cheap decorations rather than joints of their
//! own - a jaw is one hinge with nothing beneath it, and giving it a full FK
//! entry bought nothing but two more numbers to keep in step. A follow-up can
//! promote it if the owner wants the jaw independently posable mid-sequence.
//!
//! A bone's direction is the `up` axis of its *own* rotated frame (see
//! [`super::geom::Frame::rotate`]): a joint's local angles bend the bone
//! distal to it, exactly as a shoulder's rotation swings the upper arm and a
//! knee's rotation bends the shin. Bind angles (an upper arm hangs down at
//! rest, not out to the side) are baked in here so [`Pose`]'s numbers are all
//! "how far from a relaxed stand", not "the raw angle in space".

use super::geom::{v3, Angles, Frame, V3};
use std::f32::consts::PI;

/// A joint's local motion, everything at `0.0` being a relaxed, standing
/// pose facing however the actor is headed. Shared by every action: an
/// action is a recipe for these fifteen numbers (plus the jaw and foot
/// lift) over time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pose {
    /// Pelvis: overall lean and twist of the whole torso.
    pub torso: Angles,
    pub neck: Angles,
    /// Skull relative to the neck: turning the head, tipping it back.
    pub skull: Angles,
    /// `[left, right]`.
    pub shoulder: [Angles; 2],
    pub elbow: [Angles; 2],
    pub hip: [Angles; 2],
    pub knee: [Angles; 2],
    /// How far open, `0..1`.
    pub jaw: f32,
    /// How far each foot is lifted off the floor, a fraction of leg length.
    /// `[left, right]`.
    pub foot_lift: [f32; 2],
    /// Pelvis height above its resting stand, a fraction of leg length:
    /// negative to crouch or sit, small positive for a bounce.
    pub rise: f32,
}

/// Where every joint landed, in world space, plus the two frames the
/// decorations (jaw, eye sockets, ribs) are hung from.
pub struct Joints {
    pub pelvis: V3,
    pub spine: V3,
    pub chest: V3,
    pub neck: V3,
    pub skull: V3,
    pub shoulder: [V3; 2],
    pub elbow: [V3; 2],
    pub wrist: [V3; 2],
    pub hip: [V3; 2],
    pub knee: [V3; 2],
    pub ankle: [V3; 2],
    pub toe: [V3; 2],
    pub skull_frame: Frame,
    pub chest_frame: Frame,
}

/// Bone lengths and offsets, as fractions of the actor's standing height.
/// Legs sum to [`HIP_H`] so a relaxed stand plants both feet on the floor.
pub const HIP_H: f32 = 0.50;
const SPINE_LEN: f32 = 0.16;
const CHEST_LEN: f32 = 0.14;
const NECK_LEN: f32 = 0.06;
const SKULL_LEN: f32 = 0.09;
pub const SKULL_R: f32 = 0.075;
const SHOULDER_OUT: f32 = 0.11;
const SHOULDER_UP: f32 = 0.04;
const UPPER_ARM_LEN: f32 = 0.17;
const FOREARM_LEN: f32 = 0.15;
const HIP_OUT: f32 = 0.09;
const THIGH_LEN: f32 = 0.26;
const SHIN_LEN: f32 = 0.24;
const TOE_LEN: f32 = 0.10;

/// Jaw and eye geometry, off the skull frame.
pub const JAW_FWD: f32 = 0.045;
pub const JAW_DROP: (f32, f32) = (0.05, 0.11);
pub const EYE_OUT: f32 = 0.035;
pub const EYE_FWD: f32 = 0.05;
pub const EYE_UP: f32 = -0.01;
pub const EYE_R: f32 = 0.018;

/// A limb hangs straight down at rest: flip the parent's `up` to point down
/// (pitch by pi about `right`, which is untouched), with a small constant
/// outward lean so arms do not appear to clip the torso. Sign is `-1.0` for
/// the left side, `1.0` for the right, matching [`Joints`]'s `[left, right]`
/// arrays.
fn hang(sgn: f32) -> Angles {
    Angles::new(0.0, PI, sgn * 0.16)
}

/// A slight natural curve to the upper torso, constant and not part of
/// [`Pose`]: without it a straight two-segment spine reads as a ruler.
const CHEST_CURVE: Angles = Angles::new(0.0, 0.06, 0.0);

impl Pose {
    /// Forward kinematics: every joint's world position at standing height
    /// `h` (any world unit; the box and the actors both work in metres),
    /// planted at `ground` and facing `heading` (0 = facing the camera).
    #[must_use]
    pub fn solve(&self, ground: V3, heading: f32, h: f32) -> Joints {
        let base = Frame::heading(heading);
        let torso = base.rotate(self.torso);
        let pelvis = ground.add(V3::UP.scale((HIP_H + self.rise) * h));
        let spine = torso.extend(pelvis, SPINE_LEN * h);
        let chest_frame = torso.rotate(CHEST_CURVE);
        let chest = chest_frame.extend(spine, CHEST_LEN * h);
        let neck_frame = chest_frame.rotate(self.neck);
        let neck = neck_frame.extend(chest, NECK_LEN * h);
        let skull_frame = neck_frame.rotate(self.skull);
        let skull = skull_frame.extend(neck, SKULL_LEN * h);

        let mut shoulder = [V3::ZERO; 2];
        let mut elbow = [V3::ZERO; 2];
        let mut wrist = [V3::ZERO; 2];
        let mut hip = [V3::ZERO; 2];
        let mut knee = [V3::ZERO; 2];
        let mut ankle = [V3::ZERO; 2];
        let mut toe = [V3::ZERO; 2];
        let fwd_flat = v3(heading.sin(), 0.0, -heading.cos());
        for (side, sgn) in [(0usize, -1.0_f32), (1, 1.0)] {
            let anchor = chest.add(chest_frame.right.scale(sgn * SHOULDER_OUT * h)).add(chest_frame.up.scale(SHOULDER_UP * h));
            shoulder[side] = anchor;
            let upper = chest_frame.rotate(hang(sgn).add(self.shoulder[side]));
            elbow[side] = upper.extend(anchor, UPPER_ARM_LEN * h);
            let fore = upper.rotate(self.elbow[side]);
            wrist[side] = fore.extend(elbow[side], FOREARM_LEN * h);

            let hip_anchor = pelvis.add(torso.right.scale(sgn * HIP_OUT * h));
            hip[side] = hip_anchor;
            let thigh = torso.rotate(hang(sgn).add(self.hip[side]));
            knee[side] = thigh.extend(hip_anchor, THIGH_LEN * h);
            let shin = thigh.rotate(self.knee[side]);
            ankle[side] = shin.extend(knee[side], SHIN_LEN * h);
            toe[side] = ankle[side].add(fwd_flat.scale(TOE_LEN * h)).add(V3::UP.scale(self.foot_lift[side] * h));
        }

        Joints { pelvis, spine, chest, neck, skull, shoulder, elbow, wrist, hip, knee, ankle, toe, skull_frame, chest_frame }
    }
}

/// One capsule to draw: from `a` (radius `ra`) to `b` (radius `rb`), as
/// fractions of the actor's height; the caller scales by drawn size.
#[derive(Clone, Copy)]
pub struct Bone {
    pub a: V3,
    pub b: V3,
    pub ra: f32,
    pub rb: f32,
}

impl Joints {
    /// Every bone to draw. `detail` (0..1, the level of detail by projected
    /// size - see [`super::AREA`]) fades the ribcage in only once a
    /// skeleton is big enough for it to read as anything but noise.
    #[must_use]
    pub fn bones(&self, detail: f32) -> Vec<Bone> {
        let bone = |a, b, ra, rb| Bone { a, b, ra, rb };
        let mut out = vec![
            bone(self.pelvis, self.spine, 0.052, 0.046),
            bone(self.spine, self.chest, 0.046, 0.050),
            bone(self.chest, self.neck, 0.026, 0.020),
            bone(self.neck, self.skull, 0.020, 0.018),
        ];
        for side in 0..2 {
            out.push(bone(self.chest, self.shoulder[side], 0.030, 0.022));
            out.push(bone(self.shoulder[side], self.elbow[side], 0.022, 0.016));
            out.push(bone(self.elbow[side], self.wrist[side], 0.016, 0.012));
            out.push(bone(self.pelvis, self.hip[side], 0.040, 0.030));
            out.push(bone(self.hip[side], self.knee[side], 0.030, 0.022));
            out.push(bone(self.knee[side], self.ankle[side], 0.022, 0.014));
            out.push(bone(self.ankle[side], self.toe[side], 0.014, 0.010));
        }
        if detail > 0.01 {
            let bow = self.chest_frame.right.scale(0.09 * detail);
            let fwd = self.chest_frame.fwd.scale(0.032 * detail);
            for i in 0..3 {
                let t = 0.20 + 0.24 * i as f32;
                let centre = self.spine.lerp(self.chest, t);
                let mid = centre.add(fwd);
                out.push(bone(centre.sub(bow), mid, 0.010 * detail, 0.014 * detail));
                out.push(bone(mid, centre.add(bow), 0.014 * detail, 0.010 * detail));
            }
        }
        out
    }

    /// Where the jaw's free end is, `open` 0..1.
    #[must_use]
    pub fn jaw_tip(&self, open: f32) -> V3 {
        self.skull
            .add(self.skull_frame.fwd.scale(JAW_FWD))
            .sub(self.skull_frame.up.scale(JAW_DROP.0 + (JAW_DROP.1 - JAW_DROP.0) * open))
    }

    /// The two eye sockets, left then right.
    #[must_use]
    pub fn eyes(&self) -> [V3; 2] {
        [-1.0_f32, 1.0].map(|sgn| {
            self.skull
                .add(self.skull_frame.right.scale(sgn * EYE_OUT))
                .add(self.skull_frame.fwd.scale(EYE_FWD))
                .add(self.skull_frame.up.scale(EYE_UP))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FK sanity: every bone from `Joints::bones` is exactly the length the
    /// pose asked for, whatever the joint angles are. A bug that let a
    /// rotation scale a vector (a missing normalisation, say) would show up
    /// here as a bone that stretches or shrinks as it turns.
    #[test]
    fn limbs_keep_their_lengths_whatever_the_pose() {
        let mut rng = crate::rng::Rng::new(7);
        for _ in 0..200 {
            let pose = Pose {
                torso: Angles::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)),
                neck: Angles::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)),
                skull: Angles::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)),
                shoulder: [0, 1].map(|_| Angles::new(rng.range(-2.0, 2.0), rng.range(-2.0, 2.0), rng.range(-2.0, 2.0))),
                elbow: [0, 1].map(|_| Angles::new(0.0, rng.range(-2.0, 0.0), 0.0)),
                hip: [0, 1].map(|_| Angles::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-0.5, 0.5))),
                knee: [0, 1].map(|_| Angles::new(0.0, rng.range(-2.0, 0.0), 0.0)),
                jaw: rng.f32(),
                foot_lift: [rng.f32() * 0.2, rng.f32() * 0.2],
                rise: rng.range(-0.3, 0.1),
            };
            let h = 1.6;
            let j = pose.solve(v3(rng.range(-1.0, 1.0), 0.0, rng.range(0.0, 3.0)), rng.range(-3.0, 3.0), h);
            let want: &[(&str, V3, V3, f32)] = &[
                ("pelvis-spine", j.pelvis, j.spine, SPINE_LEN),
                ("spine-chest", j.spine, j.chest, CHEST_LEN),
                ("chest-neck", j.chest, j.neck, NECK_LEN),
                ("neck-skull", j.neck, j.skull, SKULL_LEN),
                ("chest-shoulderL", j.chest, j.shoulder[0], SHOULDER_OUT.hypot(SHOULDER_UP)),
                ("shoulderL-elbowL", j.shoulder[0], j.elbow[0], UPPER_ARM_LEN),
                ("elbowL-wristL", j.elbow[0], j.wrist[0], FOREARM_LEN),
                ("pelvis-hipL", j.pelvis, j.hip[0], HIP_OUT),
                ("hipL-kneeL", j.hip[0], j.knee[0], THIGH_LEN),
                ("kneeL-ankleL", j.knee[0], j.ankle[0], SHIN_LEN),
            ];
            for (name, a, b, frac) in want {
                let got = a.sub(*b).len();
                assert!((got - frac * h).abs() < 1e-3, "{name}: {got} != {}", frac * h);
            }
        }
    }

    #[test]
    fn a_relaxed_stand_plants_both_feet_on_the_floor() {
        let pose = Pose::default();
        let j = pose.solve(V3::ZERO, 0.0, 1.6);
        assert!(j.ankle[0].y.abs() < 0.02, "left ankle at {}", j.ankle[0].y);
        assert!(j.ankle[1].y.abs() < 0.02, "right ankle at {}", j.ankle[1].y);
        assert!(j.skull.y > j.chest.y && j.chest.y > j.pelvis.y);
    }

    /// `sit` (`actions::sit`) is meant to put the whole body down: a much
    /// lower skull than standing, and the feet (nearly) on the floor with
    /// the legs reaching towards the camera rather than floating at knee
    /// height - the pose shipped once with the hip flexed the wrong way
    /// round, which floated the feet a third of a metre up; this is that
    /// bug, pinned.
    #[test]
    fn sitting_puts_the_feet_near_the_floor_and_the_head_down() {
        let idle = super::super::actions::idle(3.0, 1.0).solve(v3(0.0, 0.0, 1.0), 0.0, 1.6);
        let sit = super::super::actions::sit(1.0).solve(v3(0.0, 0.0, 1.0), 0.0, 1.6);
        assert!(sit.skull.y < idle.skull.y * 0.75, "a seated head should be well below a standing one: {} vs {}", sit.skull.y, idle.skull.y);
        assert!(sit.ankle[0].y < 0.12, "a seated foot should rest near the floor, not float: {}", sit.ankle[0].y);
        assert!(sit.ankle[0].z < 1.0, "a seated leg should reach towards the camera, not away from it");
    }
}
