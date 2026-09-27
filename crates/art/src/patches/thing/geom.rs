//! Minimal 3D vector, frame and quaternion maths for the box, the hand rig
//! and the clip player.
//!
//! `V3`, `Frame`, `Angles` and `rodrigues` are a byte-for-byte copy of
//! `patches/skeletons/geom.rs` (itself a deliberate second copy of `flock`'s
//! own `V3` - card 328's scope rule, which this card inherits, and which its
//! own doc comment predicted: "card 333 is expected to reuse this file
//! wholesale"). `Quat` is new: the clip player needs a real spherical
//! interpolation for the hand's **root** orientation (it turns freely in
//! three dimensions as it walks and looks around, so a fixed rotation axis
//! cannot be assumed the way it can for a hinge), where `Frame::rotate`'s
//! sequential yaw/pitch/roll is the wrong tool - composing two different
//! Euler triples and interpolating each angle independently does not trace
//! the shortest path between two orientations and can twist through a pole.
//! Every other rotating joint in the hand (see `hand_rig.rs`) turns about one
//! or two *fixed* anatomical axes within a small limit range, where linear
//! interpolation of the scalar angle *is* the geodesic on that axis's circle
//! of rotations - literally a slerp, just one with a constant axis - so
//! those are interpolated as plain angles (see `clip.rs::lerp_angle`) and do
//! not need this.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl V3 {
    pub const ZERO: V3 = v3(0.0, 0.0, 0.0);
    pub const UP: V3 = v3(0.0, 1.0, 0.0);

    pub fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    pub fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }

    pub fn scale(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }

    pub fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: V3) -> V3 {
        v3(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }

    pub fn len2(self) -> f32 {
        self.dot(self)
    }

    pub fn len(self) -> f32 {
        self.len2().sqrt()
    }

    pub fn lerp(self, o: V3, t: f32) -> V3 {
        self.add(o.sub(self).scale(t))
    }

    #[must_use]
    pub fn normalize(self) -> V3 {
        let l = self.len();
        if l > 1e-8 {
            self.scale(1.0 / l)
        } else {
            V3::UP
        }
    }
}

/// A right-handed orthonormal basis: `right`, `up`, `fwd`. A bone's own frame
/// after its joint's rotation; the bone to its child runs along `up`.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub right: V3,
    pub up: V3,
    pub fwd: V3,
}

impl Frame {
    pub const IDENTITY: Frame = Frame { right: v3(1.0, 0.0, 0.0), up: V3::UP, fwd: v3(0.0, 0.0, 1.0) };

    /// An arbitrary yawed, level frame - only the tests need one (a real
    /// root frame always comes from a clip's own quaternion), which is why
    /// this is `#[cfg(test)]`: kept for the FK tests to draw a "some other
    /// direction" root from, without shipping unused production API.
    #[cfg(test)]
    pub fn heading(heading: f32) -> Frame {
        let (s, c) = heading.sin_cos();
        Frame { right: v3(c, 0.0, s), up: V3::UP, fwd: v3(s, 0.0, -c) }
    }

    /// This frame, rotated by `a` (yaw about `up`, then pitch about the new
    /// `right`, then roll about the newer `fwd`). What a joint's own local
    /// angles do to its parent's frame to produce its own.
    #[must_use]
    pub fn rotate(self, a: Angles) -> Frame {
        let (mut right, mut up, mut fwd) = (self.right, self.up, self.fwd);
        if a.yaw != 0.0 {
            right = rodrigues(right, up, a.yaw);
            fwd = rodrigues(fwd, up, a.yaw);
        }
        if a.pitch != 0.0 {
            up = rodrigues(up, right, a.pitch);
            fwd = rodrigues(fwd, right, a.pitch);
        }
        if a.roll != 0.0 {
            right = rodrigues(right, fwd, a.roll);
            up = rodrigues(up, fwd, a.roll);
        }
        Frame { right, up, fwd }
    }

    /// This frame, rotated by a quaternion (the root's own free 3D
    /// orientation - see [`Quat`]'s doc).
    #[must_use]
    pub fn rotate_q(self, q: Quat) -> Frame {
        Frame { right: q.rotate(self.right), up: q.rotate(self.up), fwd: q.rotate(self.fwd) }
    }

    /// The next joint out along this frame's own axis, `len` along `up`.
    #[must_use]
    pub fn extend(self, from: V3, len: f32) -> V3 {
        from.add(self.up.scale(len))
    }
}

/// Rotate unit vector `v` by `angle` radians around unit `axis` (Rodrigues).
pub fn rodrigues(v: V3, axis: V3, angle: f32) -> V3 {
    let (s, c) = angle.sin_cos();
    v.scale(c).add(axis.cross(v).scale(s)).add(axis.scale(axis.dot(v) * (1.0 - c)))
}

/// A joint's local rotation relative to its parent's frame: yaw about the
/// parent's `up`, pitch about the result's `right`, roll about the result's
/// `fwd`, applied in that order (see [`Frame::rotate`]). Radians.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Angles {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

impl Angles {
    pub const fn new(yaw: f32, pitch: f32, roll: f32) -> Angles {
        Angles { yaw, pitch, roll }
    }
}

/// A unit quaternion (`w + xi + yj + zk`), for the hand's **root**
/// orientation only (see this module's doc for why the rest of the rig does
/// not need one). Stored `(x, y, z, w)`, Hamilton convention, right-handed -
/// the same handedness [`rodrigues`] uses, so [`Quat::from_axis_angle`] and
/// `rodrigues` rotate a vector the same way for the same axis and angle
/// (proven by `quat_matches_rodrigues_for_a_single_axis`).
#[derive(Clone, Copy, Debug)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };

    #[must_use]
    pub fn from_axis_angle(axis: V3, angle: f32) -> Quat {
        let axis = axis.normalize();
        let (s, c) = (angle * 0.5).sin_cos();
        Quat { x: axis.x * s, y: axis.y * s, z: axis.z * s, w: c }
    }

    #[must_use]
    pub fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    #[must_use]
    pub fn dot(self, o: Quat) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }

    #[must_use]
    pub fn normalize(self) -> Quat {
        let l = (self.dot(self)).sqrt().max(1e-8);
        Quat { x: self.x / l, y: self.y / l, z: self.z / l, w: self.w / l }
    }

    #[must_use]
    pub fn conjugate(self) -> Quat {
        Quat { x: -self.x, y: -self.y, z: -self.z, w: self.w }
    }

    /// Rotate a vector by this quaternion.
    #[must_use]
    pub fn rotate(self, v: V3) -> V3 {
        let qv = v3(self.x, self.y, self.z);
        let t = qv.cross(v).scale(2.0);
        v.add(t.scale(self.w)).add(qv.cross(t))
    }

    /// This rotation as `(axis, angle)`, the short way round (`angle` in
    /// `0..=pi`) - what a decaying inertialization offset and an angular
    /// velocity estimate both want (`player.rs`, `clip.rs::pose_vel`).
    /// `angle` is `0.0` (axis arbitrary, `V3::UP`) for the identity or
    /// anything numerically indistinguishable from it.
    #[must_use]
    pub fn to_axis_angle(self) -> (V3, f32) {
        let q = self.normalize();
        // A negative `w` describes the long way round the same rotation;
        // negating the whole quaternion (which represents the same
        // rotation) keeps `angle` on the short arc.
        let q = if q.w < 0.0 { Quat { x: -q.x, y: -q.y, z: -q.z, w: -q.w } } else { q };
        let angle = 2.0 * q.w.clamp(-1.0, 1.0).acos();
        let s = (1.0 - q.w * q.w).max(0.0).sqrt();
        if s < 1e-5 {
            (V3::UP, 0.0)
        } else {
            (v3(q.x / s, q.y / s, q.z / s), angle)
        }
    }

    /// Spherical linear interpolation, shortest arc, `t` in `0..=1`.
    #[must_use]
    pub fn slerp(self, other: Quat, t: f32) -> Quat {
        let mut b = other;
        let mut d = self.dot(b);
        if d < 0.0 {
            b = Quat { x: -b.x, y: -b.y, z: -b.z, w: -b.w };
            d = -d;
        }
        if d > 0.9995 {
            // Nearly identical: linear interpolation is stable where the
            // slerp formula's `1/sin(theta)` is not.
            return Quat { x: self.x + (b.x - self.x) * t, y: self.y + (b.y - self.y) * t, z: self.z + (b.z - self.z) * t, w: self.w + (b.w - self.w) * t }
                .normalize();
        }
        let theta0 = d.clamp(-1.0, 1.0).acos();
        let theta = theta0 * t;
        let s0 = (theta0 - theta).sin();
        let s1 = theta.sin();
        let sin0 = theta0.sin().max(1e-6);
        let (ka, kb) = (s0 / sin0, s1 / sin0);
        Quat { x: self.x * ka + b.x * kb, y: self.y * ka + b.y * kb, z: self.z * ka + b.z * kb, w: self.w * ka + b.w * kb }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rodrigues_preserves_length_and_rotates_a_right_angle_correctly() {
        let v = v3(1.0, 0.0, 0.0);
        let r = rodrigues(v, V3::UP, std::f32::consts::FRAC_PI_2);
        assert!((r.len() - 1.0).abs() < 1e-5);
        // A quarter turn about +y takes +x to -z (right-handed).
        assert!((r.x - 0.0).abs() < 1e-4 && (r.z - (-1.0)).abs() < 1e-4, "{r:?}");
    }

    #[test]
    fn frame_rotate_keeps_orthonormal_basis() {
        let f = Frame::heading(0.7).rotate(Angles::new(0.3, -0.4, 0.2));
        let dot = |a: V3, b: V3| a.dot(b).abs();
        assert!(dot(f.right, f.up) < 1e-4);
        assert!(dot(f.up, f.fwd) < 1e-4);
        assert!(dot(f.right, f.fwd) < 1e-4);
        for v in [f.right, f.up, f.fwd] {
            assert!((v.len() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn quat_matches_rodrigues_for_a_single_axis() {
        let axis = v3(0.3, 0.9, -0.2).normalize();
        let angle = 1.1_f32;
        let v = v3(0.4, -0.6, 0.5);
        let a = rodrigues(v, axis, angle);
        let b = Quat::from_axis_angle(axis, angle).rotate(v);
        assert!(a.sub(b).len() < 1e-4, "{a:?} vs {b:?}");
    }

    #[test]
    fn slerp_at_zero_and_one_returns_the_endpoints() {
        let a = Quat::from_axis_angle(V3::UP, 0.2);
        let b = Quat::from_axis_angle(V3::UP, 1.4);
        let s0 = a.slerp(b, 0.0);
        let s1 = a.slerp(b, 1.0);
        assert!((s0.x - a.x).abs() < 1e-4 && (s0.w - a.w).abs() < 1e-4);
        assert!((s1.x - b.x).abs() < 1e-4 && (s1.w - b.w).abs() < 1e-4);
    }

    #[test]
    fn slerp_takes_the_shortest_arc_and_stays_unit_length() {
        let a = Quat::from_axis_angle(V3::UP, 0.1);
        let b = Quat::from_axis_angle(V3::UP, std::f32::consts::PI - 0.05);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let s = a.slerp(b, t);
            assert!((s.dot(s) - 1.0).abs() < 1e-3, "slerp should stay unit length at t={t}");
        }
    }

    #[test]
    fn to_axis_angle_round_trips_through_from_axis_angle() {
        let axis = v3(0.2, -0.5, 0.9).normalize();
        for angle in [0.0_f32, 0.3, 1.7, 3.0] {
            let q = Quat::from_axis_angle(axis, angle);
            let (a2, ang2) = q.to_axis_angle();
            if angle.abs() > 1e-4 {
                assert!((ang2 - angle).abs() < 1e-3, "{ang2} vs {angle}");
                assert!(a2.sub(axis).len() < 1e-3, "{a2:?} vs {axis:?}");
            } else {
                assert!(ang2 < 1e-3);
            }
        }
    }

    #[test]
    fn slerp_of_a_quaternion_with_itself_is_itself() {
        let a = Quat::from_axis_angle(v3(1.0, 1.0, 0.0), 0.77);
        let s = a.slerp(a, 0.5);
        assert!((s.x - a.x).abs() < 1e-4 && (s.w - a.w).abs() < 1e-4);
    }
}
