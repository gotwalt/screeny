//! Minimal 3D vector and quaternion maths for this patch's flight and its
//! mesh.
//!
//! **Copied, not shared.** `flock::sim::V3` already exists and `bats::sim`
//! copied it rather than importing it (that module's doc explains why: card
//! 313's rule that flock must stay byte-identical, and every patch here is
//! its own thing rather than a shared dependency on another patch's private
//! module). This patch needs the same generic vector algebra plus a
//! quaternion, which nothing else in the tree has yet - card 328
//! (`skeletons`) is building its own CPU 3D renderer in parallel and may want
//! one too; if it does, lifting this into a shared `crate::geom3` (or
//! similar) once both patches exist and agree what they need is worth doing
//! later. Nothing here is leaves-specific.

/// A world-space vector, or a leaf-local one before it is rotated into world
/// space - the type does not know which, same as `flock::sim::V3`.
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
    /// World up. Gravity pulls the other way, exactly as `flock::sim::UP`.
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

    pub fn len(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn unit_or(self, fallback: V3) -> V3 {
        let n = self.len();
        if n > 1e-6 {
            self.scale(1.0 / n)
        } else {
            fallback
        }
    }

}

/// A unit quaternion, `w + xi + yj + zk`, rotating leaf-local space into
/// world space: `q.rotate(LOCAL_NORMAL)` is which way the leaf's top face
/// currently points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quat {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat { w: 1.0, x: 0.0, y: 0.0, z: 0.0 };

    pub fn from_axis_angle(axis: V3, angle: f32) -> Quat {
        let a = axis.unit_or(V3::UP);
        let (s, c) = (angle * 0.5).sin_cos();
        Quat { w: c, x: a.x * s, y: a.y * s, z: a.z * s }
    }

    fn scale(self, k: f32) -> Quat {
        Quat { w: self.w * k, x: self.x * k, y: self.y * k, z: self.z * k }
    }

    fn add(self, o: Quat) -> Quat {
        Quat { w: self.w + o.w, x: self.x + o.x, y: self.y + o.y, z: self.z + o.z }
    }

    pub fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    pub fn len(self) -> f32 {
        (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    #[must_use]
    pub fn normalised(self) -> Quat {
        let n = self.len();
        if n > 1e-9 {
            self.scale(1.0 / n)
        } else {
            Quat::IDENTITY
        }
    }

    /// The inverse rotation of a unit quaternion: negating the vector part
    /// undoes it (`q.conjugate().mul(q) == IDENTITY` for a unit `q`). Card
    /// 322's echo reprojection uses this to take a world-space point back
    /// into a leaf's own local, attachment-relative frame.
    #[must_use]
    pub fn conjugate(self) -> Quat {
        Quat { w: self.w, x: -self.x, y: -self.y, z: -self.z }
    }

    /// Rotate `v` by this quaternion (assumed unit): the standard
    /// `v + 2w(q_v x v) + 2(q_v x (q_v x v))` form, which is cheaper than
    /// building a matrix for a single vector.
    #[must_use]
    pub fn rotate(self, v: V3) -> V3 {
        let q = v3(self.x, self.y, self.z);
        let t = q.cross(v).scale(2.0);
        v.add(t.scale(self.w)).add(q.cross(t))
    }

    /// Advance this orientation one fixed step under a world-frame angular
    /// velocity `omega` (radians/second): the standard `dq/dt = 1/2 * wq * q`
    /// integration, renormalised every step.
    ///
    /// Renormalising every step (not occasionally) is what keeps a leaf's
    /// orientation a true rotation over a run that may go for months: a
    /// first-order integrator drifts `|q|` away from 1 a little every step,
    /// and an art system that is never restarted has no "every so often" to
    /// hide behind.
    #[must_use]
    pub fn integrate(self, omega: V3, dt: f32) -> Quat {
        let wq = Quat { w: 0.0, x: omega.x, y: omega.y, z: omega.z };
        let dq = wq.mul(self).scale(0.5 * dt);
        self.add(dq).normalised()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{PI, TAU};

    #[test]
    fn rotating_by_identity_changes_nothing() {
        let v = v3(1.0, 2.0, 3.0);
        assert_eq!(Quat::IDENTITY.rotate(v), v);
    }

    #[test]
    fn a_quarter_turn_about_up_swaps_the_horizontal_axes() {
        let q = Quat::from_axis_angle(V3::UP, PI / 2.0);
        let r = q.rotate(v3(1.0, 0.0, 0.0));
        assert!((r.x).abs() < 1e-5, "x {r:?}");
        assert!((r.z - (-1.0)).abs() < 1e-4, "z {r:?}");
    }

    #[test]
    fn integrating_a_spin_stays_unit_length_over_many_steps() {
        let mut q = Quat::IDENTITY;
        let omega = v3(1.3, -2.1, 0.7);
        for _ in 0..10_000 {
            q = q.integrate(omega, 1.0 / 90.0);
        }
        assert!((q.len() - 1.0).abs() < 1e-5, "|q| drifted to {}", q.len());
    }

    #[test]
    fn conjugate_undoes_a_rotation() {
        let q = Quat::from_axis_angle(v3(0.3, 1.0, -0.4), 1.7);
        let v = v3(1.3, -0.6, 2.1);
        let back = q.conjugate().rotate(q.rotate(v));
        assert!(back.sub(v).len() < 1e-5, "{back:?} vs {v:?}");
    }

    #[test]
    fn a_full_turn_returns_to_where_it_started() {
        let axis = v3(0.3, 1.0, -0.4);
        let mut q = Quat::IDENTITY;
        let steps = 3600;
        for _ in 0..steps {
            q = q.integrate(axis.unit_or(V3::UP).scale(TAU / (steps as f32 / 90.0)), 1.0 / 90.0);
        }
        let back = q.rotate(v3(0.0, 0.0, 1.0));
        assert!((back.x).abs() < 0.02 && (back.y).abs() < 0.02 && (back.z - 1.0).abs() < 0.02, "{back:?}");
    }
}
