//! The box: a floor, a back wall, two side walls, and one bulb. The camera
//! sits just outside the front of it, which the panel is the window cut
//! into. Card 328 (`skeletons`) asked for this file to be reused wholesale
//! by card 333 (`thing`); this is a byte-for-byte copy of
//! `patches/skeletons/box_scene.rs`, kept as its own file (not a shared
//! module) so the two patches have nothing to merge and `skeletons` stays
//! provably untouched by this branch. `thing`'s hand lives in the same box,
//! at the same scale (a hand-sized figure standing where a skeleton's feet
//! would be), so nothing here needed to change for the new occupant.
//!
//! The owner, 2026-09-26, on this and the other autumn patches: "we'll
//! prefer foreground animations against a black backdrop. It suits the
//! panel the best." So the box itself is not a lit set: the walls, the
//! ceiling and the back wall are true black always (`brightness_at` returns
//! `0` for every plane but the floor) - geometry that is never drawn is not
//! a backdrop. The only light in the picture is a pool on the floor under
//! the bulb, the hand's own bone-pale ink, and the contact shadow it casts.

use super::geom::{v3, Angles, Frame, V3};
use crate::color::smoothstep;

/// Box interior, metres: side walls at `x = ±HALF_W`, back wall at
/// `z = DEPTH`, the window (where the panel is) at `z = 0`, floor at `y = 0`.
pub const HALF_W: f32 = 1.55;
pub const DEPTH: f32 = 3.7;
pub const CEIL_Y: f32 = 2.25;
pub const FLOOR_Y: f32 = 0.0;

/// Where a skeleton may stand, clear of the walls. `WALK_NEAR_Z` is also
/// chosen so a full standing height fits in the panel there without the
/// head clipping the top (see `FOV`'s Log entry): closer than this and only
/// a deliberate close-up (`peer`) should go.
pub const WALK_HALF_W: f32 = HALF_W - 0.22;
pub const WALK_NEAR_Z: f32 = 0.30;
pub const WALK_FAR_Z: f32 = DEPTH - 0.22;

/// One bulb, hung low over the front third of the box - nearer the window
/// than the back, which is what makes the back read as darker than the
/// front (the brief's "the back is darker than the front").
const LIGHT: V3 = v3(0.05, CEIL_Y - 0.30, DEPTH * 0.30);

/// How fast the light falls off with distance. Chosen by eye: bright enough
/// close to the bulb to read as a source, faded enough at the back wall
/// that the box reads as deep and quiet. Tight on purpose (card 328's Log):
/// a slow, wide glow put most of the floor into a dim mid-grey the panel
/// dithers visibly (brief 2.1.1's weak dark range); a small, quick-fading
/// pool of light under the bulb reads better and matches "dark, quiet, low
/// detail" besides.
const ATT_K: f32 = 1.1;

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

/// A bone's shading at one of its ends, `0..1`, combining distance to the
/// bulb with a real surface normal - the "shaded in a few value steps by
/// the light (lit side, shadow side)" the card asks for (2026-09-26: the
/// owner then lifted the compute-cost limit and asked by name for "proper
/// shading of the bones", so this replaced a distance-only version).
///
/// A bone has no single normal - it is a cylinder - so the normal used is
/// the one that actually matters for a capsule drawn as a silhouette: the
/// component of the direction to the camera that is perpendicular to the
/// bone's own axis, i.e. the outward normal of the cylinder at the point
/// the camera is actually looking at. `axis` may be the zero vector (a
/// disc: the skull, an eye), in which case the point is shaded as if it
/// faced the camera squarely, which is the right answer for a small sphere.
#[must_use]
pub fn bone_shade(p: V3, axis: V3, eye: V3) -> f32 {
    let view = eye.sub(p);
    let view_len = view.len();
    let view_u = if view_len > 1e-5 { view.scale(1.0 / view_len) } else { v3(0.0, 0.0, -1.0) };
    let axis_len = axis.len();
    let normal = if axis_len > 1e-5 {
        let axis_u = axis.scale(1.0 / axis_len);
        let perp = view_u.sub(axis_u.scale(axis_u.dot(view_u)));
        let perp_len = perp.len();
        if perp_len > 1e-5 {
            perp.scale(1.0 / perp_len)
        } else {
            view_u
        }
    } else {
        view_u
    };
    // A soft ambient floor within the shading itself (a bone's shadow side
    // is dim, not lightless - nothing here is meant to vanish), on top of
    // the ordinary distance falloff.
    let lit = lambert(p, normal);
    atten(p) * (0.35 + 0.65 * lit)
}

/// How occluded a floor point is by the walls either side of it, `0..1`
/// (`1` = fully open). Not a real ambient-occlusion trace - a cheap,
/// honest "distance to the nearest wall" stand-in - but it is what puts a
/// soft dark seam along the floor's edge that a single point light alone
/// does not (the owner, 2026-09-26: "soft shadows and ambient occlusion in
/// the box"). The walls themselves are never drawn (`brightness_at` is
/// black off the floor), so this only ever shades the floor.
fn corner_ao(hit: V3) -> f32 {
    let nearest = (hit.x - (-HALF_W)).min(HALF_W - hit.x).min(DEPTH - hit.z);
    0.55 + 0.45 * smoothstep(0.0, 0.42, nearest)
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

/// Horizontal field of view: wide, on purpose (card 328's Log). A narrower
/// lens could not get a skeleton at the front of the box to "fill the
/// height" (the card's own words) without a standing figure's head going
/// off the top of the panel at the near end of the walkable floor - the
/// maths is in the Log. Wide also means the side walls rush past in a
/// strong, converging tunnel, which is the depth cue the card asks for.
const FOV: f32 = 98.0;

impl Camera {
    #[must_use]
    pub fn at(t: f64, w: usize) -> Camera {
        let t = t as f32;
        // A slow, small figure-of-eight: enough that the box does not feel
        // like a photograph, nowhere near enough to be a pan.
        let yaw = 0.018 * (t * 0.070).sin();
        let pitch = 0.11 + 0.010 * (t * 0.051).cos();
        let eye = v3(0.04 * (t * 0.033).sin(), 1.02 + 0.02 * (t * 0.047).sin(), -1.55);
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
pub const BANDS: usize = 14;

/// OKLCH lightness is roughly a cube root of linear light (for a neutral
/// colour, `linear == l.powi(3)` exactly - `color::oklab_to_linear`'s rows
/// each sum to 1). A ramp meant to look linear in *brightness* - which is
/// what `brightness_at`'s `raw` is - therefore wants an *expanding* curve
/// (`l ~ u.cbrt()`), or the mid tones vanish into black long before the
/// palette says they should (this shipped inverted once: card 328's own log
/// has the fix). `MAX_L` is chosen so the brightest a raw ray realistically
/// gets (close beside the bulb) lands at a lit, not blinding, linear ~0.35.
///
/// Below [`DARK_CUT`], `raw` is held to true black rather than ramped: the
/// panel's dark range is its weakest (brief 2.1.1), and a slow grey gradient
/// across most of the floor dithered visibly here (also card 328's Log) -
/// most of a quiet, dark box is better off truly black, with the gradient's
/// resolution spent on the pool of light under the bulb instead.
const MAX_L: f32 = 0.705;
const DARK_CUT: f32 = 0.16;

#[must_use]
pub fn bands(color: f32) -> [Lch; BANDS] {
    let mut out = [(0.0, 0.0, 0.0); BANDS];
    for (i, band) in out.iter_mut().enumerate() {
        let u = i as f32 / (BANDS - 1) as f32;
        let lit = smoothstep(DARK_CUT, 1.0, u);
        let l = MAX_L * lit.cbrt();
        let c = color * 0.05 * lit;
        *band = (l, c, 58.0);
    }
    out
}

/// This ray's brightness, `0..1` before quantising into [`bands`]: black
/// backdrop, one pool of light on the floor (2026-09-26's Log entry - the
/// walls, ceiling and back wall are never anything but `0`), darkened under
/// each skeleton's feet so their place in the box reads as a contact
/// shadow rather than a cutout.
#[must_use]
pub fn brightness_at(cam: &Camera, x: f32, y: f32, w: usize, h: usize, shadows: &[(f32, f32)]) -> f32 {
    let Some((hit, plane)) = intersect(cam.eye, cam.ray(x, y, w, h)) else {
        return 0.0;
    };
    if plane != Plane::Floor {
        return 0.0;
    }
    let mut b = lambert(hit, normal_of(plane)) * atten(hit) * corner_ao(hit);
    for &(sx, sz) in shadows {
        let d2 = (hit.x - sx) * (hit.x - sx) + (hit.z - sz) * (hit.z - sz);
        b *= 1.0 - 0.62 * (-d2 / 0.16).exp();
    }
    b.clamp(0.0, 1.0)
}

#[must_use]
pub fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    smoothstep(e0, e1, x)
}
