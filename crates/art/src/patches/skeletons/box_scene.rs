//! The box: a floor, a back wall, two side walls, and one bulb. The camera
//! sits just outside the front of it, which the panel is the window cut
//! into. Reusable as it stands for card 333 (`thing`), which the card asks
//! to share this box and this camera.

use super::geom::{v3, Angles, Frame, V3};
use crate::color::smoothstep;

/// Box interior, metres: side walls at `x = ±HALF_W`, back wall at
/// `z = DEPTH`, the window (where the panel is) at `z = 0`, floor at `y = 0`.
pub const HALF_W: f32 = 1.55;
pub const DEPTH: f32 = 3.7;
pub const CEIL_Y: f32 = 2.25;
pub const FLOOR_Y: f32 = 0.0;

/// Where a skeleton may stand, clear of the walls.
pub const WALK_HALF_W: f32 = HALF_W - 0.22;
pub const WALK_NEAR_Z: f32 = 0.14;
pub const WALK_FAR_Z: f32 = DEPTH - 0.22;

/// One bulb, hung low over the front third of the box - nearer the window
/// than the back, which is what makes the back read as darker than the
/// front (the brief's "the back is darker than the front").
const LIGHT: V3 = v3(0.05, CEIL_Y - 0.30, DEPTH * 0.30);

/// How fast the light falls off with distance. Chosen by eye: bright enough
/// close to the bulb to read as a source, faded enough at the back wall
/// that the box reads as deep and quiet.
const ATT_K: f32 = 0.55;

/// A point's light exposure, `0..1`, before anything else is done with it:
/// how a surface with a normal shades it, and the floor a bone's own ink is
/// never allowed to fall under, both build on this same number.
fn atten(p: V3) -> f32 {
    let d2 = LIGHT.sub(p).len2();
    1.0 / (1.0 + ATT_K * d2)
}

fn lambert(p: V3, normal: V3) -> f32 {
    let to_light = LIGHT.sub(p);
    let d = to_light.len().max(1e-4);
    normal.dot(to_light.scale(1.0 / d)).max(0.0)
}

/// A bone's own light exposure, `0..1`: distance-attenuation only, no
/// surface normal (a bone has no one clean normal). The caller floors this
/// before using it as ink, so a skeleton never reads as less than a dim
/// stick figure even at the back of the box.
#[must_use]
pub fn bone_light(p: V3) -> f32 {
    atten(p)
}

/// The camera: fixed just outside the window, drifting very slightly (the
/// card: "fixed or drifts very slightly; it is the skeletons that move").
pub struct Camera {
    pub eye: V3,
    pub right: V3,
    pub up: V3,
    pub fwd: V3,
    pub focal: f32,
}

/// Horizontal field of view: wide enough that the side walls' converging
/// lines are visible and read as depth, narrow enough that a skeleton near
/// the window still fills a real fraction of the panel.
const FOV: f32 = 58.0;

impl Camera {
    #[must_use]
    pub fn at(t: f64, w: usize) -> Camera {
        let t = t as f32;
        // A slow, small figure-of-eight: enough that the box does not feel
        // like a photograph, nowhere near enough to be a pan.
        let yaw = 0.018 * (t * 0.070).sin();
        let pitch = 0.11 + 0.010 * (t * 0.051).cos();
        let eye = v3(0.04 * (t * 0.033).sin(), 1.02 + 0.02 * (t * 0.047).sin(), -2.05);
        let f = Frame::IDENTITY.rotate(Angles::new(yaw, pitch, 0.0));
        Camera { eye, right: f.right, up: f.up, fwd: f.fwd, focal: 0.5 * w as f32 / (0.5 * FOV.to_radians()).tan() }
    }

    /// Panel coordinates of a world point, and how far in front it is (for
    /// depth sorting). `None` when it is behind the lens.
    #[must_use]
    pub fn project(&self, p: V3, w: usize, h: usize) -> Option<(f32, f32, f32)> {
        let v = p.sub(self.eye);
        let z = v.dot(self.fwd);
        if z < 0.15 {
            return None;
        }
        let k = self.focal / z;
        Some((0.5 * w as f32 + v.dot(self.right) * k, 0.5 * h as f32 - v.dot(self.up) * k, z))
    }

    fn ray(&self, x: f32, y: f32, w: usize, h: usize) -> V3 {
        let px = (x - 0.5 * w as f32) / self.focal;
        let py = (0.5 * h as f32 - y) / self.focal;
        self.fwd.add(self.right.scale(px)).add(self.up.scale(py))
    }
}

/// Which surface a ray hit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Plane {
    Floor,
    Ceiling,
    Left,
    Right,
    Back,
}

fn normal_of(p: Plane) -> V3 {
    match p {
        Plane::Floor => V3::UP,
        Plane::Ceiling => v3(0.0, -1.0, 0.0),
        Plane::Left => v3(1.0, 0.0, 0.0),
        Plane::Right => v3(-1.0, 0.0, 0.0),
        Plane::Back => v3(0.0, 0.0, -1.0),
    }
}

/// The nearest box surface a ray from `eye` in direction `dir` reaches,
/// axis-aligned and simple because the box is.
fn intersect(eye: V3, dir: V3) -> Option<(V3, Plane)> {
    let mut best: Option<(f32, V3, Plane)> = None;
    let mut try_plane = |t: f32, plane: Plane, in_bounds: bool| {
        if t > 1e-4 && in_bounds && best.is_none_or(|(bt, ..)| t < bt) {
            best = Some((t, eye.add(dir.scale(t)), plane));
        }
    };
    if dir.y.abs() > 1e-6 {
        let t = (FLOOR_Y - eye.y) / dir.y;
        let h = eye.add(dir.scale(t));
        try_plane(t, Plane::Floor, h.x.abs() <= HALF_W && (0.0..=DEPTH).contains(&h.z));
        let t = (CEIL_Y - eye.y) / dir.y;
        let h = eye.add(dir.scale(t));
        try_plane(t, Plane::Ceiling, h.x.abs() <= HALF_W && (0.0..=DEPTH).contains(&h.z));
    }
    if dir.x.abs() > 1e-6 {
        let t = (-HALF_W - eye.x) / dir.x;
        let h = eye.add(dir.scale(t));
        try_plane(t, Plane::Left, (FLOOR_Y..=CEIL_Y).contains(&h.y) && (0.0..=DEPTH).contains(&h.z));
        let t = (HALF_W - eye.x) / dir.x;
        let h = eye.add(dir.scale(t));
        try_plane(t, Plane::Right, (FLOOR_Y..=CEIL_Y).contains(&h.y) && (0.0..=DEPTH).contains(&h.z));
    }
    if dir.z > 1e-6 {
        let t = (DEPTH - eye.z) / dir.z;
        let h = eye.add(dir.scale(t));
        try_plane(t, Plane::Back, h.x.abs() <= HALF_W && (FLOOR_Y..=CEIL_Y).contains(&h.y));
    }
    best.map(|(_, h, p)| (h, p))
}

/// A colour as (lightness, chroma, hue in degrees): mixed here, painted
/// with [`crate::color::oklch`] once, exactly as `flock` does it.
pub type Lch = (f32, f32, f32);

/// Background brightness bands, darkest to brightest, before the bulb's own
/// glow: a plain ramp, because the real shape of the box comes from
/// `brightness_at`, not from the palette.
pub const BANDS: usize = 12;

#[must_use]
pub fn bands(color: f32) -> [Lch; BANDS] {
    let mut out = [(0.0, 0.0, 0.0); BANDS];
    for (i, band) in out.iter_mut().enumerate() {
        let u = i as f32 / (BANDS - 1) as f32;
        let l = 0.30 * u.powf(1.22);
        let c = color * 0.045 * u;
        *band = (l, c, 58.0);
    }
    out
}

/// This ray's brightness, `0..1` before quantising into [`bands`]: how the
/// box's own lighting sees it, darkened under each skeleton's feet so their
/// place in the box reads as depth rather than a cutout.
#[must_use]
pub fn brightness_at(cam: &Camera, x: f32, y: f32, w: usize, h: usize, shadows: &[(f32, f32)]) -> f32 {
    let Some((hit, plane)) = intersect(cam.eye, cam.ray(x, y, w, h)) else {
        return 0.0;
    };
    let mut b = lambert(hit, normal_of(plane)) * atten(hit);
    if plane == Plane::Floor {
        for &(sx, sz) in shadows {
            let d2 = (hit.x - sx) * (hit.x - sx) + (hit.z - sz) * (hit.z - sz);
            b *= 1.0 - 0.5 * (-d2 / 0.10).exp();
        }
    }
    b.clamp(0.0, 1.0)
}

#[must_use]
pub fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    smoothstep(e0, e1, x)
}
