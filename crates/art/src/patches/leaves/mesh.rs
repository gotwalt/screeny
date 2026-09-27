//! A leaf as a 3D surface: a leaf-shaped outline, triangulated, with a
//! midrib crease and a slight cup or curl so it is not a flat card - the
//! owner's own words for what card 314's flat blob was missing.
//!
//! **Three silhouettes** (maple, oak, birch-ish - [`SHAPES`]), picked per leaf
//! from the seed, are one width function each: a taper from a narrow stem
//! end to a peak near the base and back down to a point at the tip, with a
//! wave riding on it for maple's pointed lobes and oak's rounder ones, none
//! at all for birch's plain oval. All three are built the same way: a strip
//! of rings across the leaf's length, three points per ring - left edge,
//! midrib, right edge - lofted into a surface. The midrib sits a little above
//! (or below) the edges, which is the crease and cup in one number
//! ([`Build::cup`]), and the tip lifts on top of that ([`Build::curl`]).
//!
//! Everything here is in **leaf-local space**: `x` runs `-1` (stem end) to
//! `1` (tip), `y` is across the width, `z` is the small out-of-plane relief.
//! [`super::leaf::Leaf3D::LOCAL_NORMAL`] - `(0, 0, 1)` - is this frame's "top"
//! (adaxial) face at the midrib; the physics and the mesh agree on that by
//! construction; a flat, uncupped, uncurled leaf's un-rotated normal is
//! exactly that vector everywhere on its surface.
//!
//! Normals are computed from the actual lofted geometry (a central
//! difference along the ring direction and across it, not an approximation),
//! so the cup and the curl really do shade as a curved surface and not a flat
//! one with a note saying it is curved.

use super::geom::{v3, V3};

pub const SHAPES: usize = 3;

/// One vertex, ready to hand to the renderer once it has been rotated,
/// scaled and translated into world space.
#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub pos: V3,
    pub normal: V3,
}

/// Rings across the leaf blade, stem to tip. More than card 168's flock
/// needed for a bird's wing because a leaf is the *whole* subject here, close
/// enough sometimes to show its outline, where a bird's wing was one part of
/// a bigger silhouette.
const LEAF_RINGS: usize = 11;
/// Rings along the stem, tip to where it meets the blade.
const STEM_RINGS: usize = 2;
const RINGS: usize = STEM_RINGS + LEAF_RINGS;

const STEM_LEN: f32 = 0.22;
const STEM_W: f32 = 0.02;

/// Where the blade is widest, `x` from the stem end (`-1`) to the tip (`1`).
/// Real maple and oak leaves are widest a little below their middle; this is
/// that, not the geometric centre.
const PEAK: f32 = -0.05;
/// How sharply the width rises from the base to the peak, and falls from the
/// peak to the tip. Higher tip power is a sharper point.
const BASE_POW: f32 = 0.65;
const TIP_POW: (f32, f32, f32) = (1.5, 1.15, 1.9); // maple, oak, birch

/// Half-width at the peak, as a fraction of the half-length (which is `1`).
const WMAX: f32 = 0.40;

/// The lobe wave riding on the width taper: `(frequency, amplitude, sharpen)`.
/// Maple's is high-frequency, high-amplitude and sharpened into points;
/// oak's is lower and rounder; birch's is none at all.
const LOBES: [(f32, f32, f32); SHAPES] = [(3.4, 0.26, 1.6), (5.2, 0.13, 0.0), (0.0, 0.0, 0.0)];

fn width_at(shape: u8, x: f32) -> f32 {
    let tip_pow = [TIP_POW.0, TIP_POW.1, TIP_POW.2][shape as usize % SHAPES];
    let base = if x <= PEAK {
        let t = (x + 1.0) / (PEAK + 1.0);
        t.max(0.0).powf(BASE_POW)
    } else {
        let t = (x - PEAK) / (1.0 - PEAK);
        (1.0 - t).max(0.0).powf(tip_pow)
    };
    let mut w = WMAX * base;
    let (freq, amp, sharpen) = LOBES[shape as usize % SHAPES];
    if amp > 0.0 {
        let wave = (freq * (x - PEAK) * std::f32::consts::PI).cos();
        let wave = if sharpen > 0.0 { wave.max(0.0).powf(sharpen) } else { wave };
        w *= 1.0 + amp * wave;
    }
    w.max(0.006)
}

/// The three lateral samples of one ring: left edge, midrib, right edge.
fn ring(shape: u8, x: f32, cup: f32, curl: f32) -> [V3; 3] {
    if x < -1.0 {
        // The stem: a thin, flat sliver, no cup or curl - a stem does not
        // catch the light the way the blade does.
        let stem_t = ((x + 1.0) / -STEM_LEN).clamp(0.0, 1.0); // 0 at the blade, 1 at the tip
        let w = STEM_W * (1.0 - stem_t) + 0.003;
        return [v3(x, -w, 0.0), v3(x, 0.0, 0.0), v3(x, w, 0.0)];
    }
    let w = width_at(shape, x);
    let curl_z = curl * x.max(0.0).powi(2);
    let z_at = |yfrac: f32| cup * w * yfrac * yfrac + curl_z;
    [v3(x, -w, z_at(1.0)), v3(x, 0.0, z_at(0.0)), v3(x, w, z_at(1.0))]
}

/// Every ring's three points, stem tip to leaf tip.
fn rings(shape: u8, cup: f32, curl: f32) -> [[V3; 3]; RINGS] {
    let mut out = [[V3::ZERO; 3]; RINGS];
    for (i, slot) in out.iter_mut().enumerate() {
        let x = if i < STEM_RINGS {
            // Tip of the stem to just short of where it meets the blade -
            // the blade's own first ring (`j == 0` below) supplies `x = -1`
            // exactly, so this must not repeat it (a zero-length band would
            // still render fine, but there is no reason to waste one).
            -1.0 - STEM_LEN + STEM_LEN * i as f32 / STEM_RINGS as f32
        } else {
            let j = i - STEM_RINGS;
            -1.0 + 2.0 * j as f32 / (LEAF_RINGS - 1) as f32
        };
        *slot = ring(shape, x, cup, curl);
    }
    out
}

/// Central-difference normal at ring `i`, lateral slot `j` (`0` left, `1`
/// midrib, `2` right): the actual surface's tangents, not an approximation
/// from the flat width function - so the cup and the curl really shade as
/// the curved surface they are.
fn normal_at(rings: &[[V3; 3]; RINGS], i: usize, j: usize) -> V3 {
    let dx = if i == 0 {
        rings[1][j].sub(rings[0][j])
    } else if i == RINGS - 1 {
        rings[RINGS - 1][j].sub(rings[RINGS - 2][j])
    } else {
        rings[i + 1][j].sub(rings[i - 1][j])
    };
    let dy = match j {
        0 => rings[i][1].sub(rings[i][0]),
        2 => rings[i][2].sub(rings[i][1]),
        _ => rings[i][2].sub(rings[i][0]),
    };
    dx.cross(dy).unit_or(FALLBACK_NORMAL)
}

/// The fallback for a degenerate ring (should not happen outside a
/// zero-length mesh, but a fallback beats a NaN normal reaching the
/// renderer): the physics's own idea of "up" for a flat leaf.
const FALLBACK_NORMAL: V3 = v3(0.0, 0.0, 1.0);

/// The whole leaf as a triangle soup, in leaf-local space, ready to be
/// rotated, scaled by [`super::leaf::Build::size`] and translated to a
/// world-space vertex buffer once a frame. No culling is asked of the
/// renderer (the depth buffer sorts occlusion out, as `flock`'s and `knot`'s
/// doc comments both note for their own meshes), so winding order here does
/// not matter - only the normals, which are computed from the geometry
/// itself and do not depend on it either.
#[must_use]
pub fn build(shape: u8, cup: f32, curl: f32) -> Vec<Vertex> {
    let shape = shape % SHAPES as u8;
    let r = rings(shape, cup, curl);
    let mut out = Vec::with_capacity((RINGS - 1) * 4 * 3);
    let vtx = |i: usize, j: usize| Vertex { pos: r[i][j], normal: normal_at(&r, i, j) };
    for i in 0..RINGS - 1 {
        let (l0, c0, ri0) = (vtx(i, 0), vtx(i, 1), vtx(i, 2));
        let (l1, c1, ri1) = (vtx(i + 1, 0), vtx(i + 1, 1), vtx(i + 1, 2));
        out.extend_from_slice(&[l0, l1, c0, c0, l1, c1]);
        out.extend_from_slice(&[c0, c1, ri0, ri0, c1, ri1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_builds_a_nonempty_mesh_with_unit_normals() {
        for shape in 0..SHAPES as u8 {
            let verts = build(shape, 0.15, 0.1);
            assert!(!verts.is_empty());
            assert!(verts.len().is_multiple_of(3), "a triangle soup is a multiple of 3 vertices");
            for v in &verts {
                assert!(v.pos.x.is_finite() && v.pos.y.is_finite() && v.pos.z.is_finite());
                let n = v.normal.len();
                assert!((n - 1.0).abs() < 1e-3, "normal not unit length: {n}");
            }
        }
    }

    /// A flat, uncupped, uncurled leaf's normal should everywhere agree with
    /// [`super::super::leaf::Leaf3D::LOCAL_NORMAL`] - the physics and the
    /// mesh's shared idea of "which way is up" - to within the wobble the
    /// lobe wave puts into the width (which tilts the edge normals a little
    /// even at `cup = curl = 0`; the midrib itself, `y = 0`, is untouched by
    /// any lobe and should be exact).
    #[test]
    fn a_flat_leafs_midrib_normal_matches_the_physics_local_normal() {
        for shape in 0..SHAPES as u8 {
            let verts = build(shape, 0.0, 0.0);
            // Every second triangle's second vertex is a midrib point in this
            // module's own triangulation (`c0`/`c1` above); rather than depend
            // on that layout, just check every vertex whose y is ~0.
            for v in &verts {
                if v.pos.y.abs() < 1e-5 {
                    let dot = v.normal.dot(v3(0.0, 0.0, 1.0));
                    assert!(dot > 0.999, "shape {shape}: midrib normal {:?} not (0,0,1)-ish", v.normal);
                }
            }
        }
    }

    /// Cupping raises the edges relative to the midrib (or lowers them, for
    /// negative cup) - checked directly on the geometry, not just trusted
    /// from the formula.
    #[test]
    fn cup_lifts_the_edges_above_the_midrib() {
        let flat = ring(1, 0.3, 0.0, 0.0);
        let cupped = ring(1, 0.3, 0.25, 0.0);
        assert!((flat[0].z - 0.0).abs() < 1e-6);
        assert!(cupped[0].z > flat[0].z + 0.01, "left edge should have lifted: {:?} vs {:?}", cupped[0], flat[0]);
        assert!(cupped[2].z > flat[2].z + 0.01, "right edge should have lifted");
        assert!((cupped[1].z - flat[1].z).abs() < 1e-6, "the midrib itself does not move with cup");
    }

    /// Curl only touches the tip half (`x > 0`); the stem end is unaffected.
    #[test]
    fn curl_only_lifts_the_tip_half() {
        let base = ring(0, -0.9, 0.0, 0.0);
        let curled_base = ring(0, -0.9, 0.0, 0.3);
        assert!((base[1].z - curled_base[1].z).abs() < 1e-6, "curl should not touch the base");
        let tip = ring(0, 0.9, 0.0, 0.0);
        let curled_tip = ring(0, 0.9, 0.0, 0.3);
        assert!(curled_tip[1].z > tip[1].z + 0.01, "curl should lift the tip");
    }
}
