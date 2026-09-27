//! A clip: per-joint local rotations over time, plus root motion and
//! per-fingertip contact flags, at whatever rate it was captured at (the
//! card: "per-joint local rotations over time (plus root/wrist motion and
//! per-finger contact flags) at the capture rate"). Stored as data files
//! (`crates/art/assets/thing/*.clip.json`), embedded at compile time
//! (`include_str!`) and parsed once behind an [`std::sync::OnceLock`] -
//! `dither.rs` and `color.rs` do the same thing for their own baked tables.
//!
//! **Where clips come from, today and once the footage lands.**
//! [`placeholders`] builds a handful of clips in Rust code, by hand, so the
//! patch runs end to end before any footage exists (the card: "a small set
//! of hand-keyed placeholder clips so the patch runs end to end... Label
//! them placeholders in `playing()`"). `tools/thing-capture/convert.py` is
//! the other source: it writes real `.clip.json` files with this exact
//! schema, and [`ALL_CLIPS`] is the one place - mirroring
//! `patches/mod.rs`'s own `ALL` list - a new file gets added, one line, after
//! which [`embedded_clips`] carries it and [`all_clips`] prefers it over any
//! placeholder of the same name (so `wave` stops being a placeholder the
//! moment a real `wave.clip.json` is embedded, with no other code to touch).

use super::geom::{v3, Quat, V3};
use super::hand_rig::Pose;
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClipFrame {
    /// Seconds from the clip's own start.
    pub t: f32,
    /// The wrist's position in the box, metres, in the choreographer's own
    /// frame (see `choreo.rs`: the box floor is `y = 0`, so a walking clip's
    /// `root_pos.y` is the wrist's height off the floor, not zero).
    pub root_pos: [f32; 3],
    /// The whole hand's orientation, `(x, y, z, w)` - see `geom::Quat`'s doc
    /// for why the root gets a real quaternion and the joints below it do
    /// not.
    pub root_rot: [f32; 4],
    pub pose: Pose,
    /// `[thumb, index, middle, ring, pinky]`, matching `hand_rig::Joints::tips`.
    pub contacts: [bool; 5],
}

impl ClipFrame {
    #[must_use]
    pub fn root_rot_quat(&self) -> Quat {
        let [x, y, z, w] = self.root_rot;
        Quat { x, y, z, w }
    }

    #[must_use]
    pub fn root_pos_v3(&self) -> V3 {
        v3(self.root_pos[0], self.root_pos[1], self.root_pos[2])
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub name: String,
    /// Whether this clip is meant to be looped by the choreographer (a walk
    /// cycle) rather than played once and left at its last frame (a
    /// one-shot gesture). Advisory only - `Clip::sample` clamps either way,
    /// so a looping caller wraps `t` itself.
    pub loopable: bool,
    /// Ascending `t`, at least two frames (checked by
    /// [`Clip::first_frame_regression`]'s companion test at load time).
    pub frames: Vec<ClipFrame>,
}

/// One clip sampled at a moment: everything the renderer or the player's
/// blending needs, already interpolated.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub root_pos: V3,
    pub root_rot: Quat,
    pub pose: Pose,
    pub contacts: [bool; 5],
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Lerp every scalar joint angle between two poses - `mod.rs`'s motion blur
/// uses this too, to interpolate between two already-simulated steps
/// rather than repeating this file's own field list a third time.
#[must_use]
pub fn lerp_pose(a: &Pose, b: &Pose, t: f32) -> Pose {
    let (aa, ba) = (a.to_array(), b.to_array());
    let mut out = [0.0; 23];
    for i in 0..23 {
        out[i] = lerp(aa[i], ba[i], t);
    }
    Pose::from_array(out).clamp()
}

impl Clip {
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.frames.last().map_or(0.0, |f| f.t) - self.frames.first().map_or(0.0, |f| f.t)
    }

    /// Sample at `t` seconds from the clip's own start, slerping the root
    /// orientation and lerping everything else between the two keyframes
    /// bracketing `t` (see `geom.rs`'s doc on why that is a real slerp for
    /// the root and an equivalent one, per fixed axis, for every joint
    /// below it). `t` outside the clip's own span clamps to the nearest end
    /// - a caller that wants to loop wraps `t` into `0..=duration()` first.
    #[must_use]
    pub fn sample(&self, t: f32) -> Sample {
        let frames = &self.frames;
        debug_assert!(frames.len() >= 2, "a clip needs at least two frames");
        let local = t;
        if local <= frames[0].t {
            return Self::exact(&frames[0]);
        }
        if local >= frames[frames.len() - 1].t {
            return Self::exact(&frames[frames.len() - 1]);
        }
        let i = frames.partition_point(|f| f.t <= local).max(1).min(frames.len() - 1);
        let (a, b) = (&frames[i - 1], &frames[i]);
        let span = (b.t - a.t).max(1e-6);
        let u = ((local - a.t) / span).clamp(0.0, 1.0);
        let contacts = if u < 0.5 { a.contacts } else { b.contacts };
        Sample { root_pos: a.root_pos_v3().lerp(b.root_pos_v3(), u), root_rot: a.root_rot_quat().slerp(b.root_rot_quat(), u), pose: lerp_pose(&a.pose, &b.pose, u), contacts }
    }

    fn exact(f: &ClipFrame) -> Sample {
        Sample { root_pos: f.root_pos_v3(), root_rot: f.root_rot_quat(), pose: f.pose, contacts: f.contacts }
    }
}

/// The whole-hand pose and velocity at a moment, for motion matching (the
/// card: "cutting at frames where pose and velocity already match") and for
/// inertialization's own offset math (`player.rs`).
#[derive(Clone, Copy, Debug)]
pub struct PoseVel {
    pub pos: V3,
    pub vel: V3,
    pub rot: Quat,
    /// Angular velocity, as a rotation vector (axis scaled by radians per
    /// second) - by central difference of `root_rot`, the same convention
    /// `player.rs`'s inertialization decays.
    pub rot_vel: V3,
    pub pose: Pose,
    /// Per-second rate of every scalar joint angle (see `Pose::to_array`) -
    /// a velocity, not a pose, so it is the flat array rather than
    /// [`Pose`] itself: nothing here should ever be clamped to a joint
    /// limit, which is a fate `Pose`'s own methods are the one place that
    /// applies.
    pub pose_vel: [f32; 23],
}

/// A clip's pose and velocity at `t`, velocity by central difference over
/// `dt` - used by the player to seed inertialization and by the
/// motion-matching search over candidate entry frames.
#[must_use]
pub fn pose_vel(clip: &Clip, t: f32, dt: f32) -> PoseVel {
    let dt = dt.max(1e-4);
    let s0 = clip.sample(t - dt);
    let s1 = clip.sample(t + dt);
    let st = clip.sample(t);
    let vel = s1.root_pos.sub(s0.root_pos).scale(1.0 / (2.0 * dt));
    let (axis, angle) = s1.root_rot.mul(s0.root_rot.conjugate()).to_axis_angle();
    let rot_vel = axis.scale(angle / (2.0 * dt));
    let (a0, a1) = (s0.pose.to_array(), s1.pose.to_array());
    let mut pose_vel = [0.0; 23];
    for i in 0..23 {
        pose_vel[i] = (a1[i] - a0[i]) / (2.0 * dt);
    }
    PoseVel { pos: st.root_pos, vel, rot: st.root_rot, rot_vel, pose: st.pose, pose_vel }
}

pub mod placeholders;

/// Every real clip file shipped with the crate: one line per file, exactly
/// as `patches/mod.rs`'s `ALL` is one line per patch. Names match the shot
/// list (`.claude/process/board/doing/333-thing-shot-list.md`) so a file
/// dropped in here needs no other code changed to reach the choreographer.
const ALL_CLIPS: &[(&str, &str)] = &[
    // A converter-produced clip, run against a synthetic stand-in (see
    // `tools/thing-capture/README.md`) rather than the owner's own footage,
    // which does not exist yet - proof the file format and the loading path
    // work end to end, not a performance.
    ("converted-demo", include_str!("../../../assets/thing/converted-demo.clip.json")),
];

fn embedded_clips() -> &'static BTreeMap<String, Clip> {
    static CLIPS: OnceLock<BTreeMap<String, Clip>> = OnceLock::new();
    CLIPS.get_or_init(|| {
        ALL_CLIPS
            .iter()
            .map(|(name, json)| {
                let clip: Clip = serde_json::from_str(json).unwrap_or_else(|e| panic!("assets/thing/{name}.clip.json: {e}"));
                assert!(clip.frames.len() >= 2, "assets/thing/{name}.clip.json: a clip needs at least two frames");
                (name.to_string(), clip)
            })
            .collect()
    })
}

/// Every clip the choreographer can reach: the shipped files, then the
/// placeholders filling in any name (or any gap) the files do not cover yet.
#[must_use]
pub fn all_clips() -> BTreeMap<String, Clip> {
    let mut out = placeholders::all();
    for (name, clip) in embedded_clips() {
        out.insert(name.clone(), clip.clone());
    }
    out
}

/// True for a clip this build had to make up (no footage-derived file of
/// that name exists) - `playing()` reads this to say so honestly.
#[must_use]
pub fn is_placeholder(name: &str) -> bool {
    !embedded_clips().contains_key(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::geom::Angles;
    use super::super::hand_rig::{FingerPose, ThumbPose};

    fn frame(t: f32, x: f32, yaw: f32) -> ClipFrame {
        let q = Quat::from_axis_angle(V3::UP, yaw);
        ClipFrame {
            t,
            root_pos: [x, 0.0, 1.0],
            root_rot: [q.x, q.y, q.z, q.w],
            pose: Pose { wrist: Angles::new(0.0, 0.0, 0.0), thumb: ThumbPose::default(), fingers: [FingerPose { mcp: yaw.abs(), ..Default::default() }; 4] },
            contacts: [false, false, false, false, false],
        }
    }

    #[test]
    fn sampling_between_two_frames_interpolates_position_and_rotation() {
        let clip = Clip { name: "t".into(), loopable: false, frames: vec![frame(0.0, 0.0, 0.0), frame(1.0, 2.0, 1.0)] };
        let mid = clip.sample(0.5);
        assert!((mid.root_pos.x - 1.0).abs() < 1e-4, "{mid:?}");
        // A slerp halfway between yaw 0 and yaw 1 rad about +y should itself
        // be a rotation of very nearly 0.5 rad about +y.
        let back = mid.root_rot.rotate(v3(1.0, 0.0, 0.0));
        let want = super::super::geom::rodrigues(v3(1.0, 0.0, 0.0), V3::UP, 0.5);
        assert!(back.sub(want).len() < 1e-3, "{back:?} vs {want:?}");
    }

    #[test]
    fn sampling_outside_the_clip_clamps_to_the_nearest_end() {
        let clip = Clip { name: "t".into(), loopable: false, frames: vec![frame(0.0, 0.0, 0.0), frame(1.0, 2.0, 1.0)] };
        assert_eq!(clip.sample(-5.0).root_pos.x, 0.0);
        assert_eq!(clip.sample(50.0).root_pos.x, 2.0);
    }

    #[test]
    fn contacts_are_not_interpolated_they_switch_at_the_midpoint() {
        let mut a = frame(0.0, 0.0, 0.0);
        a.contacts[1] = true;
        let b = frame(1.0, 0.0, 0.0);
        let clip = Clip { name: "t".into(), loopable: false, frames: vec![a, b] };
        assert!(clip.sample(0.1).contacts[1], "still planted just after the first frame");
        assert!(!clip.sample(0.9).contacts[1], "released well before the second frame");
    }

    #[test]
    fn every_shipped_clip_file_parses_and_has_at_least_two_frames() {
        for (name, clip) in embedded_clips() {
            assert!(clip.frames.len() >= 2, "{name}");
            for w in clip.frames.windows(2) {
                assert!(w[1].t > w[0].t, "{name}: frames must be in ascending time order");
            }
        }
    }

    #[test]
    fn placeholders_are_reported_as_placeholders_and_shipped_files_are_not() {
        assert!(is_placeholder("wave"), "there is no wave.clip.json yet");
        assert!(!is_placeholder("converted-demo"), "this one is a real (if synthetic) file");
    }
}
