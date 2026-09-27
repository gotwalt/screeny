//! The box, rescaled for card 334's GPU raymarch.
//!
//! Card 328 (`skeletons`) asked for a byte-for-byte copy of its own
//! `box_scene.rs`, kept as `thing`'s own file precisely so the two patches
//! would have *nothing* to merge and `skeletons` would stay provably
//! untouched no matter what this file did next (`skeletons` still has its
//! own, still-original copy of this file). Card 333 took that literally.
//! Card 334 does not: the box and the camera in the version card 333 shipped
//! were `skeletons`' own numbers, tuned to frame a standing ~1.6-1.8 m
//! figure across a walkable floor several metres deep - a hand living in
//! that box sits at least `1.55 m` from the camera before the box's own
//! depth is even added, which is what made `HAND_H` need to lie about the
//! hand's size in the first place (see `hand_rig::HAND_H`'s old doc, and the
//! card's own words: "fix the framing properly rather than the `HAND_H`
//! apparent-size cheat"). This is that fix: a box and a camera actually
//! scaled for a hand, so `HAND_H` can go back to something close to a real
//! hand's own length and the render still fills a third to two-thirds of
//! the panel's height.
//!
//! What is unchanged from `skeletons`' idea of the box, only rescaled: a
//! floor, a back wall, two side walls, one bulb hung low over the front
//! third, true black everywhere the bulb's own pool of light does not
//! reach ("we'll prefer foreground animations against a black backdrop" -
//! the owner, 2026-09-26). The render itself (raymarching both the box's
//! planes and the hand's own SDF together, in `thing.wgsl`) replaced the
//! CPU-side `intersect`/`brightness_at`/`bone_shade`/`corner_ao` this file
//! used to carry, so only the numbers survive here: box geometry, the
//! light, and the camera (`Camera::at`, still read on the CPU once a frame
//! to build the GPU uniform, and by `mod.rs::keep_in_box` to keep a clip's
//! root motion inside the walkable floor).

use super::geom::{v3, Angles, Frame, V3};

/// Box interior, metres: side walls at `x = ±HALF_W`, back wall at
/// `z = DEPTH`, the window (where the panel is) at `z = 0`, floor at `y = 0`.
/// A "shoebox" scaled for a hand, not a standing figure: about 70 cm wide,
/// 65 cm deep, 42 cm to the ceiling - big enough that a walk has somewhere
/// to go, small enough that the camera below can sit close.
pub const HALF_W: f32 = 0.34;
pub const DEPTH: f32 = 0.64;
pub const CEIL_Y: f32 = 0.42;
pub const FLOOR_Y: f32 = 0.0;

/// Where the hand may stand, clear of the walls. `WALK_NEAR_Z` is close
/// enough to the glass that only a deliberate close-up gesture should go
/// nearer; `WALK_FAR_Z` is chosen so the far end still projects at least a
/// third of the panel's height (`hand_rig::HAND_H`'s own doc has the
/// worked numbers).
pub const WALK_HALF_W: f32 = HALF_W - 0.07;
pub const WALK_NEAR_Z: f32 = 0.10;
pub const WALK_FAR_Z: f32 = DEPTH - 0.08;

/// One bulb, hung low over the front third of the box - nearer the window
/// than the back, which is what makes the back read as darker than the
/// front.
pub const LIGHT: V3 = v3(0.03, CEIL_Y - 0.07, DEPTH * 0.30);

/// How fast the light falls off with distance, rescaled for the box's own
/// (much smaller) metre scale - see `skeletons::box_scene::ATT_K`'s doc for
/// why a tight, quick falloff reads better on this panel than a slow, wide
/// glow does.
pub const ATT_K: f32 = 11.0;

/// The camera: fixed just outside the window, drifting very slightly (the
/// autumn set's own house rule for this box - it is the occupant that
/// moves).
pub struct Camera {
    pub eye: V3,
    pub right: V3,
    pub up: V3,
    pub fwd: V3,
    pub focal: f32,
}

/// Horizontal field of view. Narrower than `skeletons`' own 98 degrees - a
/// small box this close to the lens does not need as wide a lens to fit its
/// occupant, and a narrower one keeps the converging-wall depth cue without
/// the extreme fisheye a hand-scale box would otherwise show up close.
const FOV: f32 = 86.0;

impl Camera {
    #[must_use]
    pub fn at(t: f64, w: usize) -> Camera {
        let t = t as f32;
        // A slow, small figure-of-eight: enough that the box does not feel
        // like a photograph, nowhere near enough to be a pan.
        let yaw = 0.014 * (t * 0.070).sin();
        let pitch = 0.14 + 0.008 * (t * 0.051).cos();
        let eye = v3(0.012 * (t * 0.033).sin(), 0.185 + 0.006 * (t * 0.047).sin(), -0.145);
        let f = Frame::IDENTITY.rotate(Angles::new(yaw, pitch, 0.0));
        Camera { eye, right: f.right, up: f.up, fwd: f.fwd, focal: 0.5 * w as f32 / (0.5 * FOV.to_radians()).tan() }
    }
}
