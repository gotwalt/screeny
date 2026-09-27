//! Hand-keyed placeholder clips: scaffolding, not a performance (the card's
//! own words - "Hand-keyed motion is scaffolding only... not the result").
//! Each one exists so the player, the choreographer and the render have
//! something to run against before the owner's footage lands. Every clip
//! here starts and ends at (or very near) [`rest`]'s own pose, which is what
//! makes an inertialized cut between any two of them safe (the card: "a
//! shared rest pose every take passes through gives the choreographer a
//! safe hand-off").

use super::super::geom::{v3, Angles, Quat, V3};
use super::super::hand_rig::{FingerPose, Pose, ThumbPose};
use super::{Clip, ClipFrame};
use std::collections::BTreeMap;
use std::f32::consts::{FRAC_PI_2, PI};

/// Card 334's own finding, made while getting the GPU raymarch to read as a
/// hand at all rather than fixing the framing: `hand_rig::Pose::solve`
/// extends a relaxed finger along the palm frame's own `up` axis (the
/// generic "a limb extends along its frame's `up`" convention `rig.rs`
/// established for a *standing* skeleton, where identity really does mean
/// "upright"). Every placeholder clip's `root_rot` was `quat_yaw` alone - a
/// rotation about world `+y` only - which leaves `up` exactly where it
/// started (`+y` is the yaw axis) whatever the yaw. So every placeholder
/// clip shipped with card 333 had its fingers pointing at the ceiling, not
/// lying on the table or planted underfoot - invisible in the old
/// supersampled-capsule "grey blob" (the orchestrator's own words) and
/// obvious the moment the SDF made the silhouette legible.
///
/// This is a base *tilt*, applied before the yaw (so yaw keeps meaning
/// "which way it faces" - see the `mul` order below), not a change to the
/// clip format, the player, or contact IK: those all read `root_rot`
/// exactly as before, and both the FK ([`Pose::solve`]) and contact IK
/// (`finger_anchor_frame`, `player.rs::pin_contacts`) derive the palm frame
/// from it the same way, so a tilt baked in here keeps everything - planted
/// fingertips included - self-consistent. Three tilts, one per way a hand
/// actually sits in this box:
/// - [`Tilt::Presented`]: resting, palm mostly down, fingers pointing
///   further into the box *and* a little up - `rest`, `wave`. Not a full
///   quarter turn (`up -> +z` exactly): this camera looks almost straight
///   down the box's own depth axis, so a hand tilted *perfectly* flat
///   points its fingers straight at the lens's own view direction and
///   reads foreshortened almost to a dot - measured by projecting a
///   fingertip through `box_scene::Camera`, not assumed. `PRESENT_PITCH`
///   leaves `up` with a real `+y` component too, so the wrist-to-fingertip
///   length actually shows on screen; `right` (where `SPREAD_X` separates
///   the fingers) is untouched by any pitch, so the fingers' own lateral
///   spread was never the foreshortened axis and needed no yaw trick.
/// - [`Tilt::FlatToward`]: lying flat, palm down, fingers pointing at the
///   glass (`up -> -z`) - `point`, where foreshortening *is* the point: a
///   finger pointing straight at the viewer is supposed to look like that.
/// - [`Tilt::Standing`]: upside down relative to a flat hand (`up -> -y`),
///   fingers reaching straight down to the floor as legs, wrist held up -
///   `walk_scuttle`, where `up -> -y` is already a screen-vertical axis on
///   this camera and needs no adjustment.
#[derive(Clone, Copy)]
enum Tilt {
    Presented,
    FlatToward,
    Standing,
}

/// Degrees off "perfectly flat" `Tilt::Presented` leaves the fingers
/// pointing, towards the camera's own up direction - see `Tilt`'s own doc.
const PRESENT_PITCH: f32 = 1.05; // radians, ~60 degrees

impl Tilt {
    fn base(self) -> Quat {
        let pitch = match self {
            Tilt::Presented => PRESENT_PITCH,
            Tilt::FlatToward => -FRAC_PI_2,
            Tilt::Standing => PI,
        };
        Quat::from_axis_angle(v3(1.0, 0.0, 0.0), pitch)
    }
}

/// `yaw` (which way it faces, about world `+y`) composed *outside* the base
/// tilt (`yaw.mul(tilt)`, so `q.rotate(v) = yaw.rotate(tilt.rotate(v))`):
/// the tilt reorients the identity hand into "flat" or "standing" first,
/// then the yaw spins that already-oriented hand around the true vertical -
/// a lazy-Susan turn regardless of which tilt it is turning.
fn quat_tilted(tilt: Tilt, yaw: f32) -> [f32; 4] {
    let q = Quat::from_axis_angle(V3::UP, yaw).mul(tilt.base());
    [q.x, q.y, q.z, q.w]
}

/// The rest pose every placeholder starts and ends at: flat, palm down,
/// fingers relaxed and very slightly apart (the shot list's own words).
fn rest_pose() -> Pose {
    Pose { wrist: Angles::default(), thumb: ThumbPose { cmc_abduct: 0.15, ..Default::default() }, fingers: [FingerPose { spread: 0.05, ..Default::default() }; 4] }
}

fn fist_pose() -> Pose {
    Pose {
        wrist: Angles::default(),
        thumb: ThumbPose { cmc_flex: 0.5, cmc_abduct: 0.3, mcp: 0.9, ip: 0.8 },
        fingers: [FingerPose { spread: 0.0, mcp: 1.4, pip: 1.5, dip: 1.3 }; 4],
    }
}

/// Index extended, the rest curled, thumb folded across - a point.
fn point_pose() -> Pose {
    Pose {
        wrist: Angles::new(0.0, -0.1, 0.0),
        thumb: ThumbPose { cmc_flex: 0.6, cmc_abduct: 0.2, mcp: 0.8, ip: 0.6 },
        fingers: [
            FingerPose::default(),
            FingerPose { spread: 0.0, mcp: 1.3, pip: 1.5, dip: 1.2 },
            FingerPose { spread: -0.05, mcp: 1.35, pip: 1.55, dip: 1.25 },
            FingerPose { spread: -0.1, mcp: 1.3, pip: 1.5, dip: 1.2 },
        ],
    }
}

/// Fingers open and a little spread, as if greeting someone - the pose
/// `wave`'s root motion rocks from side to side.
fn open_pose() -> Pose {
    // Card 334: pushed close to `hand_rig::limits::SPREAD`'s own anatomical
    // cap (`+/-0.35`) - at this panel's resolution even "open, spread"
    // fingers need most of a real hand's own spread range to leave a
    // clearly separate LED (or more) of true black between neighbours;
    // the original, gentler spread read as a single rounded blob (measured
    // by rendering, not assumed - see the card's Log).
    Pose {
        wrist: Angles::default(),
        thumb: ThumbPose { cmc_abduct: 0.4, cmc_flex: 0.1, ..Default::default() },
        fingers: [
            FingerPose { spread: -0.35, mcp: 0.1, pip: 0.05, dip: 0.0 },
            FingerPose { spread: -0.15, mcp: 0.05, pip: 0.0, dip: 0.0 },
            FingerPose { spread: 0.15, mcp: 0.05, pip: 0.0, dip: 0.0 },
            FingerPose { spread: 0.35, mcp: 0.1, pip: 0.05, dip: 0.0 },
        ],
    }
}

fn frame(t: f32, pos: V3, tilt: Tilt, yaw: f32, pose: Pose, contacts: [bool; 5]) -> ClipFrame {
    ClipFrame { t, root_pos: [pos.x, pos.y, pos.z], root_rot: quat_tilted(tilt, yaw), pose, contacts }
}

/// Standing height (wrist off the floor) for a gesture performed on the
/// fingertips - a rough stand-in for "stand on the table on the wrist or
/// fingertips, as feels natural" (the shot list). Card 334 rescaled this
/// (and every `z`/stride/`FLAT_Y` below) in proportion to
/// `hand_rig::HAND_H`'s own rescale (`1.05` -> `0.30`) and to the new,
/// hand-scaled box (`box_scene::WALK_NEAR_Z`/`WALK_FAR_Z`, `0.10..0.56` m
/// deep rather than `0.30..3.48`) - card 333's own numbers were tuned to
/// the old, human-scaled box and would place every placeholder against the
/// back wall or beyond it in the new one.
const STAND_Y: f32 = 0.034;
const NONE: [bool; 5] = [false, false, false, false, false];
const ALL: [bool; 5] = [true, true, true, true, true];

/// A hand lying flat sits *on* the floor, not centred *at* it: `root_pos.y`
/// is the wrist's own centre, and the wrist capsule has real radius
/// (`hand_rig::WRIST_R * HAND_H`, about `0.039` m), so `y = 0.0` buries
/// roughly its bottom half in the floor plane - invisible in card 333's
/// capsule render (which never drew the floor and the hand in the same
/// pass) and a real bug the GPU raymarch exposed at once: the box's own
/// floor plane and the hand's own SDF surface are both real geometry now,
/// competing for the nearest hit, and a hand embedded in the floor mostly
/// loses. A small clearance above the thickest part of the hand keeps the
/// whole flat pose - wrist, palm, tapering fingers - clear of the floor
/// plane along its entire length.
const FLAT_Y: f32 = 0.048;

/// Hand resting, palm mostly down - the shared hand-off pose. Two frames,
/// identical: a hold, sampled anywhere gives the same pose (checked by
/// `pose_is_stable_wherever_it_is_sampled`).
fn rest() -> Clip {
    let z = 0.30;
    let pose = rest_pose();
    Clip {
        name: "rest".into(),
        loopable: true,
        frames: vec![
            frame(0.0, v3(0.0, FLAT_Y, z), Tilt::Presented, 0.0, pose, ALL),
            frame(1.0, v3(0.0, FLAT_Y, z), Tilt::Presented, 0.0, pose, ALL),
        ],
    }
}

/// A small side-to-side wave, rocking on the wrist, fingers open - loops
/// cleanly because its last frame is its first.
fn wave() -> Clip {
    let z = 0.15;
    let p = open_pose();
    Clip {
        name: "wave".into(),
        loopable: true,
        frames: vec![
            frame(0.0, v3(0.0, STAND_Y, z), Tilt::Presented, 0.0, p, NONE),
            frame(0.3, v3(0.0, STAND_Y, z), Tilt::Presented, 0.5, p, NONE),
            frame(0.6, v3(0.0, STAND_Y, z), Tilt::Presented, -0.5, p, NONE),
            frame(0.9, v3(0.0, STAND_Y, z), Tilt::Presented, 0.5, p, NONE),
            frame(1.2, v3(0.0, STAND_Y, z), Tilt::Presented, 0.0, p, NONE),
        ],
    }
}

/// Rest -> point at the camera -> hold -> rest. A one-shot gesture. The
/// bookend `rest_pose()` frames share `rest()`'s own `Tilt::Presented` (so
/// the inertialized hand-off between the two clips has nothing to blend
/// away); the pointing frames themselves are `Tilt::FlatToward` at
/// `yaw = 0.0`, so the index finger's own straight, uncurled default
/// extends towards the glass, foreshortened on purpose - "pointing at the
/// camera" reads as exactly that shape, the one case broadside would be
/// the wrong choice.
fn point() -> Clip {
    let z = 0.16;
    let r = rest_pose();
    let p = point_pose();
    Clip {
        name: "point".into(),
        loopable: false,
        frames: vec![
            frame(0.0, v3(0.0, FLAT_Y, z), Tilt::Presented, 0.0, r, ALL),
            frame(0.35, v3(0.0, STAND_Y, z), Tilt::FlatToward, 0.0, p, NONE),
            frame(1.0, v3(0.0, STAND_Y, z), Tilt::FlatToward, 0.0, p, NONE),
            frame(1.35, v3(0.0, FLAT_Y, z), Tilt::Presented, 0.0, r, ALL),
        ],
    }
}

/// A quick scuttle: fingertips as legs, the wrist raised, walking a short
/// distance left to right. Contacts alternate index/ring with middle/pinky,
/// which is what keeps this from reading as the hand sliding. The step
/// length (`X`) is deliberately small relative to a finger's own reach -
/// `l1 + eff_l2`, on the order of half `HAND_H` - because a planted
/// fingertip is held by contact IK (`player.rs::pin_contacts`), which is a
/// two-and-a-bit-joint chain with real anatomical limits: a stride longer
/// than its reach cannot be planted honestly, only clamped and dragged
/// (this shipped once at ten times this length, and
/// `a_planted_fingertip_does_not_slide_while_the_root_walks` caught it
/// exactly that way).
fn walk_scuttle() -> Clip {
    let y = 0.027;
    let base_z = 0.40;
    let p = fist_pose();
    let curled = Pose { fingers: [FingerPose { mcp: 0.7, pip: 0.9, dip: 0.6, spread: 0.1 }; 4], ..p };
        // `lift_a` alternates which pair of "legs" is planted, a coarse
        // stand-in for a real gait (see the honest-verdict note on this in
        // the final report).
    let step = |lift_a: bool| if lift_a { [false, true, false, true, false] } else { [true, false, true, false, true] };
    // Card 334 rescaled this with `HAND_H`/`STAND_Y` (see that const's own
    // doc): the reach a planted fingertip's contact IK can honestly cover
    // changed in proportion, so the stride has to as well or it is dragged
    // rather than planted again (`a_planted_fingertip_does_not_slide_while_the_root_walks`).
    const X: f32 = 0.018;
    Clip {
        name: "walk-scuttle".into(),
        loopable: true,
        frames: vec![
            frame(0.0, v3(-2.0 * X, y, base_z), Tilt::Standing, 0.0, curled, step(true)),
            frame(0.25, v3(-X, y * 1.4, base_z), Tilt::Standing, 0.0, curled, step(false)),
            frame(0.5, v3(0.0, y, base_z), Tilt::Standing, 0.0, curled, step(true)),
            frame(0.75, v3(1.0 * X, y * 1.4, base_z), Tilt::Standing, 0.0, curled, step(false)),
            frame(1.0, v3(2.0 * X, y, base_z), Tilt::Standing, 0.0, curled, step(true)),
        ],
    }
}

/// Every placeholder, by name.
#[must_use]
pub fn all() -> BTreeMap<String, Clip> {
    [rest(), wave(), point(), walk_scuttle()].into_iter().map(|c| (c.name.clone(), c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placeholder_has_at_least_two_frames_in_ascending_time() {
        for (name, clip) in all() {
            assert!(clip.frames.len() >= 2, "{name}");
            for w in clip.frames.windows(2) {
                assert!(w[1].t > w[0].t, "{name}: frames must be ascending");
            }
        }
    }

    #[test]
    fn rest_is_stable_wherever_it_is_sampled() {
        let rest = rest();
        let a = rest.sample(0.0);
        let b = rest.sample(0.7);
        assert_eq!(a.root_pos, b.root_pos);
        assert_eq!(a.pose, b.pose);
    }

    /// A gesture loop (`wave`) that does not travel should return to the
    /// same pose *and* the same spot.
    #[test]
    fn waves_loop_ends_exactly_where_it_began() {
        let clip = &all()["wave"];
        let first = &clip.frames[0];
        let last = &clip.frames[clip.frames.len() - 1];
        assert_eq!(first.root_pos, last.root_pos);
        assert_eq!(first.pose, last.pose);
    }

    /// A translating gait (`walk-scuttle`) is a different kind of loop: the
    /// *cycle* repeats (same pose, same height/bob) while the wrist keeps
    /// travelling - the choreographer (`choreo.rs`) is the one that must
    /// carry root motion forward across the seam rather than snapping back
    /// to the first frame's `x`, which this only checks the data supports.
    #[test]
    fn walk_scuttles_gait_cycle_repeats_even_though_it_travels() {
        let clip = &all()["walk-scuttle"];
        let first = &clip.frames[0];
        let last = &clip.frames[clip.frames.len() - 1];
        assert_eq!(first.pose, last.pose, "the gait phase should repeat");
        assert!((first.root_pos[1] - last.root_pos[1]).abs() < 1e-6, "the bob height should repeat");
        assert_ne!(first.root_pos[0], last.root_pos[0], "but the walk should actually have travelled");
    }

    #[test]
    fn point_starts_and_ends_at_the_shared_rest_pose() {
        let clip = &all()["point"];
        assert_eq!(clip.frames[0].pose, rest_pose());
        assert_eq!(clip.frames[clip.frames.len() - 1].pose, rest_pose());
    }
}
