//! The moon: a subject with a real surface, not a disc.
//!
//! Card 319's picture, in short: maria and craters from a procedural
//! height/albedo field, lit by the sun at a phase (full, gibbous, crescent),
//! limb darkening, and a faint halo. Everything here answers one question -
//! [`Moon::value`], "how bright is the ray in this direction" - which
//! [`super::Bats::render`] quantises into the palette ramp exactly the way
//! the first pass quantised its sky-elevation ramp; the difference is that
//! this ramp is a real analytic sphere, ray-traced per sample, rather than a
//! 1-D function of elevation.
//!
//! The moon does not rotate - like the real one, tidally locked - so its
//! craters are fixed points on the unit sphere in the sphere's own body
//! frame, generated once per seed. The *light* sweeping across them as
//! `phase` changes is a second, independent direction, exactly as it is for
//! the real moon: the far side of this module's honesty is that a phase
//! slider changes only the sun, never the rock.

use super::sim::{v3, V3};
use crate::color::smoothstep;
use crate::rng::Rng;
use std::f32::consts::TAU;

/// How many craters and maria a moon gets. Few, and each one large - brief
/// section 3's "favour big shapes, bold contrast, strong silhouettes"
/// applies as much to the moon's own texture as to anything else on this
/// panel. An early version used a dozen smaller ones (craters down to 3
/// degrees of arc); rendered and judged, they were real (confirmed by
/// sampling actual pixel values, not just by eye) but too fine a texture to
/// read as more than a slightly grainy sphere - a handful of large,
/// unmistakable marks reads as "a real surface" at this resolution where a
/// naturalistic crater field does not.
const CRATERS: usize = 6;
const MARIA: usize = 3;

/// How far the disc's own brightness falls off towards the limb - a stand-in
/// for the real Moon's limb darkening (its regolith scatters light less
/// efficiently at a grazing angle). Not driven to zero: the real limb is
/// dim, not black.
const LIMB_K: f32 = 0.55;

/// The halo's own falloff and peak strength. Faint by design (the card's
/// "faint halo", and brief 4's "aim for an average picture level under
/// ~40%" - a full moon is already most of this patch's lit area).
const HALO_K: f32 = 60.0;
const HALO_AMP: f32 = 0.12;

/// A faint hint of the unlit disc - real earthshine, however dim - asked for
/// by the orchestrator's review round 1: without it a crescent or gibbous
/// moon's own dark side is indistinguishable from the black sky right next
/// to it, and the card wants the moon to "read as a moon with surface and
/// phase, not a disc" - a bald lit sliver with no sense of the sphere behind
/// it reads more like the second than the first. Small enough that it never
/// competes with the lit crescent for attention.
const EARTHSHINE_AMP: f32 = 0.025;

// Bold on purpose (brief section 3: "favour big shapes, bold contrast,
// strong silhouettes" - a panel this coarse has no room for a subtle 10%
// modulation). The first version of these numbers (0.30/0.24/0.26) was
// almost invisible once the limb-darkening gradient was in the same
// picture: real, per the debug ASCII heatmap in `mod.rs`'s Log, but too
// faint against that gradient for a person glancing at the panel to notice.
const CRATER_FLOOR: f32 = 0.62;
const CRATER_RIM: f32 = 0.48;
const MARE_DARK: f32 = 0.52;

pub struct Moon {
    /// Unit direction from the camera to the moon's centre. Fixed for the
    /// life of the patch, like the bats' camera.
    pub dir: V3,
    /// A reference axis in the sky-plane perpendicular to `dir`, for
    /// building the phase's light direction. Has nothing to do with the
    /// craters, which live in their own body-fixed frame.
    right: V3,
    craters: Vec<(V3, f32)>,
    maria: Vec<(V3, f32)>,
}

/// A uniformly random point on the unit sphere - the craters' and maria's
/// own body-fixed positions. Only ever cosmetic crater placement, not
/// physics, so the classic two-angle construction (not perfectly uniform at
/// the poles, immaterial for a dozen points) is more than enough.
fn random_unit(rng: &mut Rng) -> V3 {
    let z = rng.range(-1.0, 1.0);
    let a = rng.range(0.0, TAU);
    let r = (1.0 - z * z).max(0.0).sqrt();
    v3(r * a.cos(), r * a.sin(), z)
}

impl Moon {
    pub fn new(rng: &mut Rng, dir: V3) -> Moon {
        let world_up = v3(0.0, 1.0, 0.0);
        let right = world_up.cross(dir).unit_or(v3(1.0, 0.0, 0.0));
        // A few big craters, more small ones - `u * u` biases the draw
        // towards the low end, the way real crater-size distributions do.
        let craters: Vec<(V3, f32)> = (0..CRATERS)
            .map(|_| {
                let u = rng.f32();
                (random_unit(rng), 0.12 + 0.34 * u * u)
            })
            .collect();
        let maria: Vec<(V3, f32)> = (0..MARIA).map(|_| (random_unit(rng), rng.range(0.30, 0.50))).collect();
        Moon { dir, right, craters, maria }
    }

    /// The sun's direction for this `phase` (0 and 1 are new moon, 0.5 is
    /// full): a rotation of "towards the camera" by the phase angle about
    /// `right`, so the terminator sweeps left-right across the disc as a
    /// real phase does, independent of the fixed crater field underneath it.
    #[must_use]
    pub fn light_for(&self, phase: f32) -> V3 {
        let angle = (phase - 0.5) * TAU;
        let (s, c) = angle.sin_cos();
        self.dir.scale(-c).add(self.right.scale(s))
    }

    fn albedo(&self, n: V3) -> f32 {
        let mut a = 1.0_f32;
        for &(c, r) in &self.maria {
            let t = n.dot(c).clamp(-1.0, 1.0).acos() / r;
            if t < 1.0 {
                a -= MARE_DARK * (1.0 - smoothstep(0.5, 1.0, t));
            }
        }
        for &(c, r) in &self.craters {
            let t = n.dot(c).clamp(-1.0, 1.0).acos() / r;
            if t < 1.0 {
                a -= CRATER_FLOOR * (1.0 - smoothstep(0.0, 0.75, t)).max(0.0);
                let rim = smoothstep(0.72, 0.88, t) * (1.0 - smoothstep(0.88, 1.0, t));
                a += CRATER_RIM * rim;
            }
        }
        a.clamp(0.12, 1.3)
    }

    /// This ray's brightness, roughly `0..1.2` before the caller quantises
    /// it into the palette ramp (a rim highlight may peek a little over 1,
    /// which the ramp's own cap then absorbs): the halo outside the disc of
    /// angular radius `ang_r`, or the lit, textured, limb-darkened surface
    /// inside it.
    ///
    /// `ray` need not be unit length - `View::ray` does not return one - and
    /// every angle below is only meaningful for a unit vector, so this
    /// normalises it first. Skipping that once, in an early version, was a
    /// real bug (see `mod.rs`'s Log): the dot product with a ray that grew
    /// longer away from screen centre stopped being a true cosine at all,
    /// and the halo it fed came out roughly constant across the whole panel
    /// instead of decaying with distance from the moon.
    #[must_use]
    pub fn value(&self, ray: V3, ang_r: f32, light: V3) -> f32 {
        let ray = ray.unit_or(self.dir);
        let cos_v = ray.dot(self.dir);
        let r = ang_r.sin();
        let sin2 = (1.0 - cos_v * cos_v).max(0.0);
        // How much of the whole disc is lit right now, `0..1` - the halo is
        // fainter around a crescent than a full moon, the way a dim source
        // casts a dim glow. `light` points *towards* the sun from the moon,
        // so at full moon (sun behind the camera) `light` is close to
        // `-dir`, not `dir` - the sign the first version of this line got
        // backwards, making a full moon's halo the dimmest instead of the
        // brightest (found the same way as the ray-normalisation bug above:
        // by rendering it and looking).
        let lit_frac = (0.5 - 0.5 * light.dot(self.dir)).clamp(0.05, 1.0);
        if sin2 > r * r {
            let ang = cos_v.clamp(-1.0, 1.0).acos() - ang_r;
            return HALO_AMP * lit_frac * (-HALO_K * ang.max(0.0)).exp();
        }
        let t = cos_v - (r * r - sin2).max(0.0).sqrt();
        let hit = ray.scale(t);
        let n = hit.sub(self.dir).scale(1.0 / r.max(1e-5));
        let view = ray.scale(-1.0);
        let lambert = n.dot(light).max(0.0);
        let limb = (1.0 - LIMB_K) + LIMB_K * n.dot(view).max(0.0);
        let lit = (lambert * limb * self.albedo(n)).clamp(0.0, 1.2);
        // Earthshine only ever raises the dark side's own floor - never adds
        // to the lit side, which already carries the picture.
        let disc_v = lit.max(EARTHSHINE_AMP * limb);
        // Blended smoothly across the last few percent of the disc, into the
        // halo's own value right at the limb, rather than a hard ring: an
        // anti-aliased limb on top of the supersampling that already does
        // the rest of the edge.
        let ang = cos_v.clamp(-1.0, 1.0).acos();
        let edge = smoothstep(ang_r * 0.90, ang_r, ang);
        let halo_here = HALO_AMP * lit_frac;
        disc_v * (1.0 - edge) + halo_here * edge
    }
}
