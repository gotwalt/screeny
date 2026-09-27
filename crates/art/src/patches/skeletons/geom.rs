//! Minimal 3D vector and frame maths for the box and the rig.
//!
//! `flock/sim.rs` has its own `V3`; this is a second, deliberately separate
//! copy rather than a shared dependency, so `skeletons` and `flock` stay in
//! their own directories with nothing between them to merge (card 328's
//! scope rule). Card 333 (`thing`) is expected to reuse this file wholesale.

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

    /// The frame a standing actor's pelvis starts from: facing world
    /// direction `heading` (0 = facing the camera, `-z`), level.
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
#[derive(Clone, Copy, Debug, Default)]
pub struct Angles {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

impl Angles {
    pub const fn new(yaw: f32, pitch: f32, roll: f32) -> Angles {
        Angles { yaw, pitch, roll }
    }

    #[must_use]
    pub fn add(self, o: Angles) -> Angles {
        Angles::new(self.yaw + o.yaw, self.pitch + o.pitch, self.roll + o.roll)
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
}
