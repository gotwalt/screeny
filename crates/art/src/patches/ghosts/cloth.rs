//! The cloth: a sheet of particles dropped over an invisible body and
//! simulated by Rapier's soft-body solver (card 327 - dimforge.com/blog/
//! 2026/09/25/advanced-soft-bodies-for-games-in-the-rapier-physics-engine/),
//! and the mesh it produces for the renderer.
//!
//! **Card 336 rebuilt the body underneath.** Cards 326/327 draped the sheet
//! over a bare head - "a dome on a bell", the owner's own words for why it
//! did not read as a bedsheet ghost. The owner's reference
//! (`.claude/process/board/refs/336-ghost-reference.png`, never committed) is
//! a real bedsheet thrown over a person with their arms held out: a small
//! head, then two arms out to the sides, and the sheet caught on three high
//! points - the crown and each wrist - sagging in deep catenary folds between
//! them and flaring into wide "wings" where extra fabric hangs past each
//! wrist. This module keeps 326/327's proven machinery (the poncho topology,
//! Rapier mass-spring cloth, per-triangle air, the kinematic head collar) and
//! adds: a full body ([`arm_points`]'s shoulder/elbow/wrist FK, driven by
//! [`super::act::Arms`]), kinematic collider capsules for the torso and each
//! arm segment (so the free skirt drapes *over* them, not through them), and
//! two small kinematic "cuffs" woven into the skirt's own topology at the
//! wrist azimuths (see "Wrist cuffs" below) so the sheet is actually pinned
//! at the crown and both wrists, exactly as the reference shows, rather than
//! merely resting near them.
//!
//! **Why Rapier, and which model** (unchanged from 327): mass-spring edges
//! (`SoftBodyBuilder::cloth`'s own family - structural/shear/bend distance
//! springs), not FEM - this release's FEM is volumetric tetrahedra, nothing a
//! one-particle-thick sheet can attach to without inventing a fake thickness.
//!
//! **Topology.** Unchanged in kind from 326/327: the sheet wraps all the way
//! around the body, like a poncho - one pole vertex at the crown,
//! [`HEAD_RINGS`] rings glued rigidly to the head and neck, then
//! [`SKIRT_RINGS`] free rings hanging below, [`COLS`] columns around the
//! azimuth. See [`build_template`].
//!
//! **Wrist cuffs.** At a specific skirt ring ([`WRIST_RING`]) and the two
//! azimuth columns nearest each arm ([`RIGHT_COL`]/[`LEFT_COL`]), a small
//! span of vertices ([`WRIST_SPAN`] either side) is *also* pinned - kinematic
//! like the head collar, but driven by the live wrist position
//! ([`arm_points`]) rather than a fixed head-local offset, spread a little
//! across the wrist's own width so the fabric gathers there like a cuff
//! rather than a single sharp point. Between the neck collar and a cuff, the
//! free skirt sags as a real catenary (card 336: "deep catenary drapes
//! sagging between each wrist and the head"); past a cuff, the rest of that
//! column's rings hang on as the free-falling "wing".
//!
//! **Collision.** A kinematic head sphere (unchanged from 327, sized to the
//! neck's own narrower radius per 326's own recorded bug), plus new kinematic
//! capsules for the torso and all four arm segments (upper/forearm x2),
//! moved every step from [`arm_points`]'s own FK so the parts of the skirt
//! that are *not* pinned still drape realistically over the body rather than
//! clipping through it.
//!
//! **Air** (unchanged from 327): a per-triangle aerodynamic force each
//! physics step - the standard flat-plate/pressure cloth-wind force (Baraff &
//! Witkin, "Large Steps in Cloth Simulation", 1998, section 4.1).
//!
//! **Determinism** (unchanged from 327): `enhanced-determinism`, single
//! threaded, fixed physics step, the same total-simulated-time accumulator
//! every sim in this codebase uses.

use super::act::{ArmPose, Arms, Shape};
use rapier3d::prelude::*;
use std::f32::consts::PI;

pub(crate) const COLS: usize = 20;
/// Four rings round the dome, plus a fifth: the neck (see [`build_template`]).
pub(crate) const HEAD_RINGS: usize = 5;
pub(crate) const SKIRT_RINGS: usize = 12;
pub(crate) const RINGS: usize = HEAD_RINGS + SKIRT_RINGS;
/// Pole + every ring.
pub(crate) const VERTS: usize = 1 + RINGS * COLS;
/// The first ring that is simulated rather than glued to the head: rings
/// `0..HEAD_RINGS` are kinematic, so `HEAD_RINGS` itself is the first free
/// one - the ring the skirt actually starts hanging from.
const FIRST_FREE_RING: usize = HEAD_RINGS;

/// The azimuth columns nearest each arm: `col == 0` is the seam at the back
/// (see [`azimuth`]), so a quarter-turn either way from the front column
/// (`COLS/2`) lands on the sides - `COLS` is a multiple of 4 so these are
/// exact. `az(RIGHT_COL) == 0` (world `+X`, screen right); `az(LEFT_COL) ==
/// PI` (world `-X`, screen left) - checked by a test below.
const RIGHT_COL: usize = COLS / 4;
const LEFT_COL: usize = 3 * COLS / 4;
/// Which skirt ring hosts the wrist cuff - chosen so a real catenary run of
/// rings separates it from the neck collar, and enough rings remain past it
/// to hang on as the wing (see the module doc's "Wrist cuffs").
const WRIST_RING: usize = HEAD_RINGS + 4;
/// Columns either side of [`RIGHT_COL`]/[`LEFT_COL`] that are part of the
/// cuff, spread a little across the wrist's own width rather than collapsed
/// to one point.
const WRIST_SPAN: i64 = 1;

const THETA_TOP: f32 = 0.22; // ~13 degrees off the pole: the crown is not a point on screen either.
/// Where the round part of the head stops - short of the equator, so the
/// dome reads as a ball, not a shape that is still widening when the cloth
/// takes over (card 326 review: "a head sphere that dominates the top").
const THETA_DOME: f32 = 1.15; // ~66 degrees.
/// The neck ring's own height, a little below the dome's widest point.
const THETA_NECK: f32 = 1.35; // ~77 degrees.
/// The neck's radius, as a fraction of the dome's own widest point - the
/// pinch that makes "head, then shoulders" read as two things rather than
/// one continuously widening cone (326's review's main complaint).
const NECK_FACTOR: f32 = 0.56;
/// The skirt's own baseline flare, away from the two arms - modest, so a
/// ghost reads as a tall column that flares hard only where an arm actually
/// holds the fabric out, not a bell at every azimuth (card 336: "tall
/// silhouette").
const BASE_FLARE: f32 = 1.05;
/// Angular half-width (radians) of the raised-cosine bump that pulls the
/// skirt's own template out toward each arm - just the initial guess the
/// physics settle refines, not the final shape.
const ARM_BUMP_WIDTH: f32 = 0.55;

/// The dome's own widest radius and the neck's radius, both a plain function
/// of `head_r`.
fn dome_and_neck_radius(head_r: f32) -> (f32, f32) {
    let dome = head_r * THETA_DOME.sin();
    (dome, dome * NECK_FACTOR)
}

fn neck_y_of(head_r: f32) -> f32 {
    head_r * THETA_NECK.cos()
}

/// The baseline (non-arm) skirt's own hem drop below the neck - the same
/// formula [`build_template`]'s skirt loop uses, so [`drop_below_centre`]
/// can't drift from the geometry it is meant to describe.
fn baseline_hem_len(shape: Shape) -> f32 {
    let neck_y = neck_y_of(shape.head_r);
    (shape.height - shape.head_r - neck_y).max(shape.height * 0.3)
}

/// Fast early widening, easing off - the shoulders are most of the way to
/// full width within the first skirt ring or two, not a slow taper all the
/// way to the hem.
fn flare_profile(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Shortest signed angular distance from `az` to `target`, in `(-PI, PI]`.
fn az_delta(az: f32, target: f32) -> f32 {
    let d = (az - target + PI).rem_euclid(std::f32::consts::TAU) - PI;
    d.abs()
}

/// How strongly azimuth `az` sits near an arm's own azimuth (`0` = right,
/// `PI` = left) - `1.0` right at the arm, easing to `0.0` by
/// [`ARM_BUMP_WIDTH`] radians away. Only used to seed the template's initial
/// guess (see the module doc); the physics settle is what actually decides
/// the shape.
fn arm_bump(az: f32) -> f32 {
    let d = az_delta(az, 0.0).min(az_delta(az, PI));
    if d > ARM_BUMP_WIDTH {
        0.0
    } else {
        0.5 * (1.0 + (PI * d / ARM_BUMP_WIDTH).cos())
    }
}

/// How far this ghost's cloth can reach from its own centreline, at its
/// widest - the wrist's own horizontal reach at the baseline "held out"
/// gesture, almost always, since that is far past the baseline hem's own
/// modest flare. What an entrance, an exit or a peek has to clear before
/// nothing of the *sheet*, not just the head, is on screen.
pub(crate) fn extent(shape: Shape) -> f32 {
    let (_, elbow, wrist) = arm_points(shape, ArmPose { pitch: shape.arm_pitch0, elbow: 0.16 }, 1.0);
    let wing = wrist.x.max(elbow.x) + shape.hem_amp * 0.3;
    let (dome, _) = dome_and_neck_radius(shape.head_r);
    dome.max(wing)
}

/// How far the crown sits above the head's own centre.
pub(crate) fn rise_above_centre(shape: Shape) -> f32 {
    shape.head_r
}

/// How far the lowest hem point ever reaches below the head's own centre -
/// the baseline hem drop plus the corner lobes' own worst-case droop, minus
/// the neck's own (small) rise above centre, matching [`build_template`]'s
/// actual geometry rather than a guess at it.
pub(crate) fn drop_below_centre(shape: Shape) -> f32 {
    let neck_y = neck_y_of(shape.head_r);
    let hem_len = baseline_hem_len(shape);
    (hem_len + shape.hem_amp - neck_y).max(hem_len * 0.5)
}

/// Crown to the lowest a hem point ever reaches - what a vertical placement
/// has to clear at both ends, matching [`build_template`]'s own geometry
/// rather than a guess at it.
pub(crate) fn total_height(shape: Shape) -> f32 {
    rise_above_centre(shape) + drop_below_centre(shape)
}

/// The shoulder, elbow and wrist positions (head-local, head centred at the
/// origin, no yaw) for one arm at gesture `arm`. `side` is `+1.0` (right,
/// `+X`) or `-1.0` (left, `-X`) - see [`RIGHT_COL`]/[`LEFT_COL`]'s own
/// azimuth convention, which this matches by construction (both are "world
/// `+X` is the ghost's own right, screen right"). `pitch` is radians above
/// horizontal; `elbow` is the forearm's own further bend past the upper
/// arm's direction (so `elbow == 0.0` is a dead-straight arm).
pub(crate) fn arm_points(shape: Shape, arm: ArmPose, side: f32) -> (V3, V3, V3) {
    let shoulder = V3::new(side * shape.shoulder_x, shape.shoulder_y, 0.0);
    let (s1, c1) = arm.pitch.sin_cos();
    let elbow = shoulder.add(V3::new(side * c1, s1, 0.0).scale(shape.upper_arm));
    let pitch2 = arm.pitch + arm.elbow;
    let (s2, c2) = pitch2.sin_cos();
    let wrist = elbow.add(V3::new(side * c2, s2, 0.0).scale(shape.forearm));
    (shoulder, elbow, wrist)
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl V3 {
    pub const ZERO: V3 = V3 { x: 0.0, y: 0.0, z: 0.0 };

    pub fn new(x: f32, y: f32, z: f32) -> V3 {
        V3 { x, y, z }
    }
    pub fn add(self, o: V3) -> V3 {
        V3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    pub fn sub(self, o: V3) -> V3 {
        V3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    pub fn scale(self, k: f32) -> V3 {
        V3::new(self.x * k, self.y * k, self.z * k)
    }
    pub fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: V3) -> V3 {
        V3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    /// The zero vector normalises to itself rather than NaN - the one place
    /// degenerate geometry (two coincident particles, or an arm held dead
    /// vertical when finding a perpendicular) must not poison a step.
    pub fn normalize(self) -> V3 {
        let l = self.length();
        if l > 1e-6 {
            self.scale(1.0 / l)
        } else {
            V3::ZERO
        }
    }
    /// Rotate around world Y by `yaw` radians - the only rotation the whole
    /// body ever does here (card 326's "turn"): shoulders and arms turn with
    /// the head, since a real body turns as one.
    pub fn rot_y(self, yaw: f32) -> V3 {
        let (s, c) = yaw.sin_cos();
        V3::new(c * self.x + s * self.z, self.y, -s * self.x + c * self.z)
    }
}

/// To/from Rapier's own vector type (`glam::Vec3` under `f32`/`dim3`) at the
/// one boundary that needs it - every other line of geometry in this module
/// stays in [`V3`].
fn to_rapier(v: V3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}
fn from_rapier(v: Vector) -> V3 {
    V3::new(v.x, v.y, v.z)
}

/// A robust "across" direction for a limb pointing along `fwd`: the
/// perpendicular used to spread the wrist cuff a little across its own
/// width. Falls back to world `+Z` if `fwd` is (near enough) parallel to
/// world up, the one case `+Y x fwd` degenerates.
fn across(fwd: V3) -> V3 {
    let up = V3::new(0.0, 1.0, 0.0);
    let r = up.cross(fwd);
    if r.length() > 1e-3 {
        r.normalize()
    } else {
        V3::new(0.0, 0.0, 1.0).cross(fwd).normalize()
    }
}

/// The body's rigid placement in world space at one instant: where it is,
/// which way it is turned, and this instant's arm gesture (card 336: the
/// arms are part of the pose, not the static shape).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BodyPose {
    pub pos: V3,
    pub yaw: f32,
    pub arms: Arms,
}

impl BodyPose {
    fn to_world(self, local: V3) -> V3 {
        local.rot_y(self.yaw).add(self.pos)
    }
    /// This instant as a Rapier `Pose`, for the kinematic head collider.
    fn to_pose(self) -> Pose {
        Pose::from_parts(to_rapier(self.pos), Rot3::from_scaled_axis(Vector::new(0.0, self.yaw, 0.0)))
    }
    /// World shoulder/elbow/wrist for both arms at this instant.
    fn arms_world(self, shape: Shape) -> ArmsWorld {
        let (ls, le, lw) = arm_points(shape, self.arms.left, -1.0);
        let (rs, re, rw) = arm_points(shape, self.arms.right, 1.0);
        ArmsWorld {
            left: (self.to_world(ls), self.to_world(le), self.to_world(lw)),
            right: (self.to_world(rs), self.to_world(re), self.to_world(rw)),
        }
    }
}

/// One instant's arm joints, already in world space - computed once per step
/// and shared by the wrist cuffs and the arm colliders, so they can never
/// read a different gesture than each other.
struct ArmsWorld {
    left: (V3, V3, V3),
    right: (V3, V3, V3),
}

/// A capsule collider's kinematic pose from its two world-space endpoints:
/// the midpoint, and the rotation that carries local `+Y` (`capsule_y`'s own
/// axis) onto the segment's own direction.
fn capsule_pose(a: V3, b: V3) -> Pose {
    let mid = a.add(b).scale(0.5);
    let dir = b.sub(a).normalize();
    let rot = if dir.length() > 0.5 { Rot3::from_rotation_arc(Vector::Y, to_rapier(dir)) } else { Rot3::IDENTITY };
    Pose::from_parts(to_rapier(mid), rot)
}

/// One vertex, ready for the GPU: world position, world normal, UV (`u`
/// around the azimuth - front at 0.5 - `v` from crown to hem), and a fold/AO
/// scalar (card 326: "soft shadows in the folds").
#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderVertex {
    pub pos: V3,
    pub normal: V3,
    pub uv: [f32; 2],
    pub fold: f32,
}

/// `az(col)`: azimuth in radians, with `col == COLS/2` (`u == 0.5`) facing
/// world `+Z` - a head at `z < 0` is in front of a camera at the origin
/// looking down `-Z`, so the side facing that camera is the `+Z` side, not
/// `-Z`. (Getting this backwards was a real bug: it put the eyes on the
/// skull's far side, never once visible, however the head turned.) At
/// yaw 0 this is straight at the camera; the seam (`col == 0`) lands at the
/// back, where a viewer never lingers.
fn azimuth(col: usize) -> f32 {
    std::f32::consts::TAU * col as f32 / COLS as f32 - std::f32::consts::FRAC_PI_2
}

fn uv_of(col: usize) -> f32 {
    col as f32 / COLS as f32
}

/// The rest template: every vertex's position in head-local space (head
/// centred at the origin, no rotation), and which ones are kinematic. Also
/// the two per-column jitters (radius and height) that give the hem its own
/// modest waviness before physics does anything at all.
struct Template {
    local: Vec<V3>,
    /// `true` for the pole, every head ring, and the two wrist cuffs: driven
    /// kinematically every step, never by physics.
    kinematic: Vec<bool>,
    ring_of: Vec<usize>,
    col_of: Vec<usize>,
}

fn ring_col_index(ring: usize, col: usize) -> usize {
    1 + ring * COLS + col % COLS
}

/// `true` for a column within [`WRIST_SPAN`] of `centre` (wrapping around
/// the azimuth), and the signed column offset from it if so.
fn wrist_span_of(col: usize, centre: usize) -> Option<i64> {
    (-WRIST_SPAN..=WRIST_SPAN).find(|&d| (centre as i64 + d).rem_euclid(COLS as i64) as usize == col)
}

fn build_template(shape: Shape) -> Template {
    let head_r = shape.head_r;
    let hem_len = baseline_hem_len(shape);
    let humps = shape.humps;

    let mut local = vec![V3::ZERO; VERTS];
    let mut kinematic = vec![false; VERTS];
    let mut ring_of = vec![0usize; VERTS];
    let mut col_of = vec![0usize; VERTS];

    local[0] = V3::new(0.0, head_r, 0.0);
    kinematic[0] = true;

    // Head rings: the first `HEAD_RINGS - 1` are rigid points on the dome
    // itself, theta running from just off the crown to short of the equator
    // (`THETA_DOME`) - a ball, not a shape still widening when the cloth
    // takes over. The last one is the neck: same rigid, kinematic ring, but
    // pulled in to `NECK_FACTOR` of the dome's own widest radius, which is
    // the pinch that separates "head" from "shoulders" at a glance.
    for ring in 0..HEAD_RINGS - 1 {
        let theta = THETA_TOP + (THETA_DOME - THETA_TOP) * ring as f32 / (HEAD_RINGS - 2) as f32;
        let (radius, y) = (head_r * theta.sin(), head_r * theta.cos());
        for col in 0..COLS {
            let i = ring_col_index(ring, col);
            let az = azimuth(col);
            local[i] = V3::new(radius * az.cos(), y, radius * az.sin());
            kinematic[i] = true;
            ring_of[i] = ring;
            col_of[i] = col;
        }
    }
    let neck_ring = HEAD_RINGS - 1;
    let (_, neck_radius) = dome_and_neck_radius(head_r);
    let neck_y = neck_y_of(head_r);
    for col in 0..COLS {
        let i = ring_col_index(neck_ring, col);
        let az = azimuth(col);
        local[i] = V3::new(neck_radius * az.cos(), neck_y, neck_radius * az.sin());
        kinematic[i] = true;
        ring_of[i] = neck_ring;
        col_of[i] = col;
    }

    // Skirt rings: a modest baseline flare away from the arms (`BASE_FLARE`
    // - "tall silhouette", card 336), pulled wider toward each arm's own
    // azimuth by `arm_bump` (an initial guess only; the physics settle,
    // below, is what actually drapes it), with a per-column wave (`humps`)
    // that only ever pulls the hem *down* and *out* at a few spots, never up
    // - "uneven, pointed hem with corners hanging low".
    let wrist_j = WRIST_RING - HEAD_RINGS + 1;
    let t_wrist = wrist_j as f32 / SKIRT_RINGS as f32;
    // The wrist's own horizontal reach at the baseline "held out" gesture -
    // what `arm_bump`'s columns actually flare out toward (see `radius`
    // below), not just an unused weight on the corner lobes.
    let (_, wrist_elbow, wrist_pt) = arm_points(shape, ArmPose { pitch: shape.arm_pitch0, elbow: 0.16 }, 1.0);
    let wrist_reach = wrist_pt.x.max(wrist_elbow.x);
    for j in 1..=SKIRT_RINGS {
        let ring = HEAD_RINGS + j - 1;
        let t = j as f32 / SKIRT_RINGS as f32;
        let ramp = (t / t_wrist).min(1.0);
        let base_radius = neck_radius * (1.0 + BASE_FLARE * flare_profile(t));
        let base_y = neck_y - hem_len * t;
        for col in 0..COLS {
            let i = ring_col_index(ring, col);
            let az = azimuth(col);
            let bump = arm_bump(az) * ramp;
            let lobe = 0.5 + 0.5 * (humps * az + shape.phase0).cos();
            let drip = shape.hem_amp * 0.6 * t * t * lobe * (1.0 - bump);
            // Blend the baseline (torso-hugging) radius toward the wrist's
            // own reach at the arm azimuths - this, not just the wrist
            // cuff's own pinned ring, is what gives a whole wide *wing* of
            // fabric hanging from each wrist rather than a single pulled
            // thread (card 336: "wide fabric 'wings' at the wrists").
            let radius = lerp(base_radius, wrist_reach, bump) + drip * 0.2;
            let y = base_y - drip;
            local[i] = V3::new(radius * az.cos(), y, radius * az.sin());
            ring_of[i] = ring;
            col_of[i] = col;

            // The wrist cuff: at the chosen ring, the columns nearest each
            // arm are pinned instead of free, spread a little across the
            // wrist's own width. Seed a reasonable initial guess here (the
            // baseline "held out" gesture); `Cloth::step` overrides the
            // *live* target every step from the actual gesture.
            if ring == WRIST_RING {
                let side_and_span = wrist_span_of(col, RIGHT_COL)
                    .map(|d| (1.0_f32, d))
                    .or_else(|| wrist_span_of(col, LEFT_COL).map(|d| (-1.0_f32, d)));
                if let Some((side, d)) = side_and_span {
                    let (_, elbow, wrist) = arm_points(shape, ArmPose { pitch: shape.arm_pitch0, elbow: 0.16 }, side);
                    let spread = across(wrist.sub(elbow)).scale(d as f32 * head_r * 0.35);
                    local[i] = wrist.add(spread);
                    kinematic[i] = true;
                }
            }
        }
    }

    Template { local, kinematic, ring_of, col_of }
}

/// The mesh's triangles (pole fan + ring quads), in the same vertex indexing
/// [`ring_col_index`] uses. One implementation shared by the physics body's
/// own collision surface (below) and `mod.rs`'s GPU index buffer, rather
/// than two triangulations that could quietly drift apart.
pub(crate) fn triangles() -> Vec<[u32; 3]> {
    let idx = |ring: usize, col: usize| -> u32 { ring_col_index(ring, col) as u32 };
    let mut out = Vec::new();
    for col in 0..COLS {
        out.push([0, idx(0, col), idx(0, col + 1)]);
    }
    for ring in 0..RINGS - 1 {
        for col in 0..COLS {
            let (a, b, c, d) = (idx(ring, col), idx(ring + 1, col), idx(ring + 1, col + 1), idx(ring, col + 1));
            out.push([a, b, d]);
            out.push([b, c, d]);
        }
    }
    out
}

/// Structural (ring-to-ring, round-the-ring, shear diagonals) and bending
/// (skip-one) edges, split the way `SoftBodyBuilder` wants them
/// (`edges`/`bend_edges`) - only where at least one endpoint is free, same
/// as 326's `build_edges`: two rigid points never need a constraint between
/// them (they are both driven exactly, every step, regardless).
fn build_edges() -> (Vec<[u32; 2]>, Vec<[u32; 2]>) {
    let mut edges = Vec::new();
    let mut bend = Vec::new();

    for ring in 0..RINGS {
        if ring >= HEAD_RINGS {
            for col in 0..COLS {
                edges.push([ring_col_index(ring, col) as u32, ring_col_index(ring, col + 1) as u32]);
            }
        }
        if ring + 1 < RINGS && ring + 1 >= FIRST_FREE_RING {
            for col in 0..COLS {
                edges.push([ring_col_index(ring, col) as u32, ring_col_index(ring + 1, col) as u32]);
                edges.push([ring_col_index(ring, col) as u32, ring_col_index(ring + 1, col + 1) as u32]);
                edges.push([ring_col_index(ring, col + 1) as u32, ring_col_index(ring + 1, col) as u32]);
            }
        }
        if ring >= HEAD_RINGS && ring + 2 < RINGS {
            for col in 0..COLS {
                bend.push([ring_col_index(ring, col) as u32, ring_col_index(ring + 2, col) as u32]);
                bend.push([ring_col_index(ring, col) as u32, ring_col_index(ring, col + 2) as u32]);
            }
        }
    }
    (edges, bend)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// `sway` (0 = tight, 1 = floppy) as the structural family's own natural
/// frequency and damping ratio: a tighter sheet is stiffer (higher
/// frequency) and snaps back without overshoot (damping ratio near
/// critical); a floppier one is both softer and a little underdamped, so it
/// billows and settles rather than snapping straight.
fn edge_softness(sway: f32) -> SpringCoefficients<Real> {
    let s = sway.clamp(0.0, 1.0);
    SpringCoefficients::new(lerp(46.0, 12.0, s), lerp(0.95, 0.55, s))
}
/// Bending resists far less than stretching, at every `sway`, but kept stiff
/// and damped enough that a lateral gust folds the skirt rather than
/// swinging the whole thing as one rigid flap.
fn bend_softness(sway: f32) -> SpringCoefficients<Real> {
    let s = sway.clamp(0.0, 1.0);
    SpringCoefficients::new(lerp(30.0, 10.0, s), lerp(0.92, 0.6, s))
}

/// The physics step, seconds - independent of the panel's frame rate.
pub(crate) const PHYS_DT: f32 = 1.0 / 180.0;
const GRAVITY: f32 = 34.0;
/// Every particle's mass. Absolute value does not matter for gravity (it
/// falls at the same rate regardless), only for how hard the aerodynamic
/// force (below) can push it and how the spring frequencies (normalized by
/// effective mass already, per `SoftBodyMaterial`'s own doc) come out.
const PARTICLE_MASS: f32 = 1.0;
/// The soft body's own contact-skin thickness: small next to the sheet
/// (rings are tens of units apart), just enough that the boundary collision
/// mesh has a little padding against the body colliders.
const PARTICLE_RADIUS: f32 = 0.4;
/// The body colliders' own skin, so the cloth's contact margin has a little
/// clearance from the collider surfaces it drapes over, on top of the
/// solver's own contact prediction distance.
const BODY_SKIN: f32 = 0.15;
/// Standard flat-plate aerodynamic drag (see the module doc): force along a
/// triangle's own normal, scaled by its area and the velocity component
/// along that normal.
const AIR_DRAG: f32 = 0.006;

/// The sheet's live state: Rapier's own soft-body world (one per ghost - a
/// ghost's cloth is simulated continuously across frames, `mod.rs` keys one
/// by `Pose::key`), the fixed template and triangle list, the shape (needed
/// every step to re-evaluate the live arm FK), and the body's own kinematic
/// colliders.
pub(crate) struct Cloth {
    template: Template,
    faces: Vec<[u32; 3]>,
    /// Pole, head rings and the neck ring - fixed head-local offset, driven
    /// by [`BodyPose::to_world`] alone.
    head_kinematic: Vec<usize>,
    /// `[right, left]` wrist cuffs: `(vertex index, signed spread along
    /// `across(wrist - elbow)`, in local units)`.
    wrist_kinematic: [Vec<(usize, f32)>; 2],
    shape: Shape,
    world: PhysicsWorld,
    body: SoftBodyHandle,
    head_body: RigidBodyHandle,
    torso_body: RigidBodyHandle,
    /// `[left upper, left fore, right upper, right fore]`.
    arm_bodies: [RigidBodyHandle; 4],
    /// Simulated seconds already applied - the fixed-step accumulator's own
    /// clock, so `advance` never depends on how finely it is called (the
    /// house rule every simulation in this codebase follows).
    warped: f64,
}

/// The torso capsule's own two endpoints, head-local: from just below the
/// shoulder line down toward the hip, standing in for "a body under the
/// sheet" between the arms (card 336) so the front/back skirt drapes with a
/// little real volume rather than collapsing onto the centreline.
fn torso_points(shape: Shape) -> (V3, V3) {
    let top = V3::new(0.0, shape.shoulder_y * 0.7, 0.0);
    let bottom = V3::new(0.0, shape.shoulder_y - shape.height * 0.3, 0.0);
    (top, bottom)
}

impl Cloth {
    /// Cast a fresh sheet over a body held still at `body0`, and settle it
    /// under gravity before anyone can see it (card 326: "warms up
    /// invisibly ... the first frame is a settled sheet, not a falling
    /// one"). `sway` is applied at settle time too, so a very floppy sheet
    /// still starts from a sheet-shaped rest, not a mid-fall one.
    pub(crate) fn spawn(shape: Shape, body0: BodyPose, sway: f32) -> Cloth {
        let template = build_template(shape);
        let faces = triangles();
        let wrist_kinematic = wrist_kinematic_of();
        // Every kinematic vertex gets a live target each step from exactly
        // one of two sources: the wrist cuffs (their own indices, listed
        // above) or the fixed head-local offset (everything else pinned).
        // Excluding "wrist-azimuth columns" from the head list here once
        // excluded *head-ring* vertices at those same columns too (a real
        // bug: those vertices were then pinned by the soft-body builder but
        // never given a target at all, which Rapier defaults to the origin -
        // exactly the "a vertex miles from the body" blowup this module's
        // own safety net does not catch, because the position is finite,
        // just wrong). Checked against the wrist cuffs' own index list
        // instead, not a column-proximity guess.
        let wrist_idx: std::collections::HashSet<usize> = wrist_kinematic.iter().flatten().map(|&(i, _)| i).collect();
        let head_kinematic: Vec<usize> = (0..VERTS).filter(|&i| template.kinematic[i] && !wrist_idx.contains(&i)).collect();
        let all_kinematic: Vec<usize> = (0..VERTS).filter(|&i| template.kinematic[i]).collect();
        let (edges, bend_edges) = build_edges();
        let positions: Vec<Vector> = template.local.iter().map(|&l| to_rapier(body0.to_world(l))).collect();

        let material = SoftBodyMaterial {
            edge_softness: edge_softness(sway),
            bend_softness: bend_softness(sway),
            ..Default::default()
        };
        let builder = SoftBodyBuilder::new(positions)
            .pinned_particles(all_kinematic.iter().map(|&i| i as u32))
            .edges(edges)
            .bend_edges(bend_edges)
            .surface(faces.clone())
            .material(material)
            .particle_mass(PARTICLE_MASS)
            .particle_radius(PARTICLE_RADIUS)
            .self_contacts(false);

        let mut world = PhysicsWorld::new();
        world.gravity = Vector::new(0.0, -GRAVITY, 0.0);
        world.integration_parameters.dt = PHYS_DT;
        let body = world.insert_soft_body(builder);

        // The neck's own (narrower) radius, not the head's widest point -
        // 326's log records the reverse as a real bug: the free skirt
        // starts at the neck, so testing against the head's own radius
        // shoved the whole skirt out to the head's equator on first settle.
        let (_, neck_r) = dome_and_neck_radius(shape.head_r);
        let (head_body, _) =
            world.insert(RigidBodyBuilder::kinematic_position_based().pose(body0.to_pose()), ColliderBuilder::ball(neck_r).contact_skin(BODY_SKIN));

        let (torso_top, torso_bot) = torso_points(shape);
        let torso_len = torso_bot.sub(torso_top).length();
        let (torso_body, _) = world.insert(
            RigidBodyBuilder::kinematic_position_based().pose(capsule_pose(body0.to_world(torso_top), body0.to_world(torso_bot))),
            ColliderBuilder::capsule_y(torso_len * 0.5, shape.head_r * 0.85).contact_skin(BODY_SKIN),
        );

        let arms0 = body0.arms_world(shape);
        let arm_r = shape.head_r * 0.42;
        let mk_capsule = |world: &mut PhysicsWorld, a: V3, b: V3| {
            world
                .insert(
                    RigidBodyBuilder::kinematic_position_based().pose(capsule_pose(a, b)),
                    ColliderBuilder::capsule_y(b.sub(a).length() * 0.5, arm_r).contact_skin(BODY_SKIN),
                )
                .0
        };
        let arm_bodies = [
            mk_capsule(&mut world, arms0.left.0, arms0.left.1),
            mk_capsule(&mut world, arms0.left.1, arms0.left.2),
            mk_capsule(&mut world, arms0.right.0, arms0.right.1),
            mk_capsule(&mut world, arms0.right.1, arms0.right.2),
        ];

        let mut cloth =
            Cloth { template, faces, head_kinematic, wrist_kinematic, shape, world, body, head_body, torso_body, arm_bodies, warped: 0.0 };
        const SETTLE_STEPS: usize = 450;
        for _ in 0..SETTLE_STEPS {
            cloth.step(body0, PHYS_DT, sway);
        }
        cloth
    }

    /// One fixed-size physics step: move every kinematic collider and pinned
    /// particle to `body`'s new pose and gesture, refresh the live `sway`
    /// softness, add this step's aerodynamic force, then let Rapier step.
    fn step(&mut self, body: BodyPose, dt: f32, sway: f32) {
        self.world.integration_parameters.dt = dt;
        let arms = body.arms_world(self.shape);

        self.world.bodies[self.head_body].set_next_kinematic_position(body.to_pose());
        let (torso_top, torso_bot) = torso_points(self.shape);
        self.world.bodies[self.torso_body].set_next_kinematic_position(capsule_pose(body.to_world(torso_top), body.to_world(torso_bot)));
        self.world.bodies[self.arm_bodies[0]].set_next_kinematic_position(capsule_pose(arms.left.0, arms.left.1));
        self.world.bodies[self.arm_bodies[1]].set_next_kinematic_position(capsule_pose(arms.left.1, arms.left.2));
        self.world.bodies[self.arm_bodies[2]].set_next_kinematic_position(capsule_pose(arms.right.0, arms.right.1));
        self.world.bodies[self.arm_bodies[3]].set_next_kinematic_position(capsule_pose(arms.right.1, arms.right.2));

        {
            let sb = &mut self.world.soft_bodies[self.body];
            for &i in &self.head_kinematic {
                sb.set_particle_kinematic_target(i, to_rapier(body.to_world(self.template.local[i])));
            }
            for &(i, spread) in &self.wrist_kinematic[0] {
                sb.set_particle_kinematic_target(i, to_rapier(wrist_target(arms.right, spread)));
            }
            for &(i, spread) in &self.wrist_kinematic[1] {
                sb.set_particle_kinematic_target(i, to_rapier(wrist_target(arms.left, spread)));
            }
            let mat = sb.material_mut();
            mat.edge_softness = edge_softness(sway);
            mat.bend_softness = bend_softness(sway);
        }

        self.apply_air();
        self.world.step();

        // A last safety net: anything that went non-finite (a pathological
        // parameter combination, not one this design should reach) is
        // snapped back to its own kinematic target if it has one, or the
        // body's own position otherwise - "no exploding cloth at any param
        // setting" has to hold even if a future edit gets a constant wrong.
        let sb = &mut self.world.soft_bodies[self.body];
        for i in 0..VERTS {
            let p = sb.particle_position(i);
            if p.x.is_finite() && p.y.is_finite() && p.z.is_finite() {
                continue;
            }
            let target = if self.head_kinematic.contains(&i) {
                body.to_world(self.template.local[i])
            } else if let Some(&(_, spread)) = self.wrist_kinematic[0].iter().find(|&&(j, _)| j == i) {
                wrist_target(arms.right, spread)
            } else if let Some(&(_, spread)) = self.wrist_kinematic[1].iter().find(|&&(j, _)| j == i) {
                wrist_target(arms.left, spread)
            } else {
                body.pos
            };
            sb.set_particle_position(i, to_rapier(target));
            sb.set_particle_velocity(i, Vector::ZERO);
        }
    }

    /// The standard flat-plate/pressure aerodynamic force (see the module
    /// doc), one triangle at a time: still air, so the force opposes
    /// whatever the fabric's own motion projects onto each face's normal.
    fn apply_air(&mut self) {
        let sb = &mut self.world.soft_bodies[self.body];
        sb.reset_forces(false);
        for &[a, b, c] in &self.faces {
            let (a, b, c) = (a as usize, b as usize, c as usize);
            let (pa, pb, pc) = (sb.particle_position(a), sb.particle_position(b), sb.particle_position(c));
            let cross = (pb - pa).cross(pc - pa);
            let area2 = cross.length();
            if area2 < 1e-6 {
                continue;
            }
            let n = cross / area2;
            let v = (sb.particle_velocity(a) + sb.particle_velocity(b) + sb.particle_velocity(c)) / 3.0;
            let vn = v.dot(n);
            let force = n * (-AIR_DRAG * area2 * vn);
            let share = force / 3.0;
            sb.add_particle_force(a, share, false);
            sb.add_particle_force(b, share, false);
            sb.add_particle_force(c, share, false);
        }
    }

    /// Advance from wherever this sheet's own clock is to `target` seconds,
    /// at the fixed [`PHYS_DT`], asking `body_at` for the body's rigid pose
    /// at each step's own absolute time. The step count comes from total
    /// simulated time, not a counter, so two callers stepping by different
    /// amounts still agree (card 326's rule, and `flock::Flock::advance`'s).
    pub(crate) fn advance(&mut self, target: f64, sway: f32, mut body_at: impl FnMut(f64) -> BodyPose) {
        const CATCHUP: i64 = 240; // at most 1.3s of steps in one call, however far `target` jumped.
        let dt = f64::from(PHYS_DT);
        let already = (self.warped / dt).round() as i64;
        let want = (target / dt + 1e-6).floor() as i64;
        let from = already.max(want - CATCHUP);
        for s in from..want {
            let t = (s + 1) as f64 * dt;
            self.step(body_at(t), PHYS_DT, sway);
        }
        self.warped = want.max(already) as f64 * dt;
    }

    fn pos(&self, i: usize) -> V3 {
        from_rapier(self.world.soft_bodies[self.body].particle_position(i))
    }

    /// Every particle's current world position - used by the tests below;
    /// `render_vertices` reads [`Self::pos`] directly instead so it never
    /// allocates a full copy it does not need.
    #[cfg(test)]
    fn all_positions(&self) -> Vec<V3> {
        (0..VERTS).map(|i| self.pos(i)).collect()
    }

    /// The mesh for the renderer: every vertex's world position, a normal
    /// from its *actual* current neighbours (so a fold really does change
    /// how it catches the light), UV, and the fold/AO term.
    pub(crate) fn render_vertices(&mut self) -> Vec<RenderVertex> {
        let fold = self.compute_fold();
        (0..VERTS)
            .map(|i| {
                let ring = if i == 0 { 0 } else { self.template.ring_of[i] + 1 };
                let col = if i == 0 { 0 } else { self.template.col_of[i] };
                let normal = self.normal_at(i);
                RenderVertex { pos: self.pos(i), normal, uv: [uv_of(col), ring as f32 / RINGS as f32], fold: fold[i] }
            })
            .collect()
    }

    fn neighbour(&self, i: usize, ring: i64, col_delta: i64) -> Option<usize> {
        if i == 0 {
            return None;
        }
        let r = self.template.ring_of[i] as i64 + ring;
        if !(0..RINGS as i64).contains(&r) {
            return None;
        }
        let c = (self.template.col_of[i] as i64 + col_delta).rem_euclid(COLS as i64) as usize;
        Some(ring_col_index(r as usize, c))
    }

    fn normal_at(&self, i: usize) -> V3 {
        let p = self.pos(i);
        let right = self.neighbour(i, 0, 1).map(|j| self.pos(j).sub(p)).unwrap_or(V3::new(1.0, 0.0, 0.0));
        let left = self.neighbour(i, 0, -1).map(|j| self.pos(j).sub(p)).unwrap_or(right.scale(-1.0));
        let down = self.neighbour(i, 1, 0).map(|j| self.pos(j).sub(p));
        let up = self.neighbour(i, -1, 0).map(|j| self.pos(j).sub(p));
        let vertical = match (up, down) {
            (Some(u), Some(d)) => d.sub(u),
            (Some(u), None) => u.scale(-1.0),
            (None, Some(d)) => d,
            (None, None) => V3::new(0.0, -1.0, 0.0),
        };
        let horizontal = right.sub(left);
        // `horizontal x vertical`, not the other way round: `vertical x
        // horizontal` pointed inward, toward the head's own centre, on every
        // vertex checked by hand (326's own bug - it meant every fold's
        // light and dark side was reversed) - verified against the fallback
        // below, which is unambiguously outward.
        let n = horizontal.cross(vertical).normalize();
        // The apex's own outward normal (up the crown) is what the fan
        // above it should shade like, not whatever the first ring's tangent
        // frame happens to compute.
        if i == 0 {
            V3::new(0.0, 1.0, 0.0)
        } else if n.length() > 0.5 {
            n
        } else {
            self.pos(i).sub(V3::ZERO).normalize()
        }
    }

    /// A cheap curvature proxy: how far a vertex sits behind the plane its
    /// neighbours describe, along its own normal. Negative (a valley) darkens;
    /// convex points are left alone.
    fn compute_fold(&self) -> Vec<f32> {
        let mut fold = vec![0.0; VERTS];
        for (i, slot) in fold.iter_mut().enumerate().skip(1) {
            let mut acc = V3::ZERO;
            let mut n = 0.0;
            for (dr, dc) in [(0, 1), (0, -1), (1, 0), (-1, 0)] {
                if let Some(j) = self.neighbour(i, dr, dc) {
                    acc = acc.add(self.pos(j));
                    n += 1.0;
                }
            }
            if n < 2.0 {
                continue;
            }
            let avg = acc.scale(1.0 / n);
            let laplacian = self.pos(i).sub(avg);
            let normal = self.normal_at(i);
            *slot = laplacian.dot(normal);
        }
        fold
    }
}

/// The two wrist cuffs' own `(vertex index, signed spread)` lists, `[right,
/// left]` - shared by `spawn` (to build the kinematic target function) and
/// nothing else, so the span/side logic lives in exactly one place.
fn wrist_kinematic_of() -> [Vec<(usize, f32)>; 2] {
    let mut right = Vec::new();
    let mut left = Vec::new();
    for col in 0..COLS {
        let i = ring_col_index(WRIST_RING, col);
        if let Some(d) = wrist_span_of(col, RIGHT_COL) {
            right.push((i, d as f32));
        } else if let Some(d) = wrist_span_of(col, LEFT_COL) {
            left.push((i, d as f32));
        }
    }
    [right, left]
}

/// The live world target for one wrist cuff vertex: the wrist itself, spread
/// `spread` local units across the forearm's own width via [`across`].
fn wrist_target((_, elbow, wrist): (V3, V3, V3), spread: f32) -> V3 {
    if spread == 0.0 {
        return wrist;
    }
    wrist.add(across(wrist.sub(elbow)).scale(spread))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patches::ghosts::act::Shape;
    use crate::rng::Rng;

    fn shape(rng: &mut Rng) -> Shape {
        Shape::new(rng, 28.0)
    }

    fn body(pos: V3, yaw: f32) -> BodyPose {
        BodyPose { pos, yaw, arms: Arms { left: ArmPose { pitch: -0.15, elbow: 0.16 }, right: ArmPose { pitch: -0.15, elbow: 0.16 } } }
    }

    /// `RIGHT_COL`/`LEFT_COL` really do land on the world `+X`/`-X` sides -
    /// [`arm_points`]'s own `side` sign only makes sense if they do.
    #[test]
    fn wrist_columns_match_the_arm_sides() {
        assert!(azimuth(RIGHT_COL).abs() < 1e-4, "RIGHT_COL azimuth {}", azimuth(RIGHT_COL));
        assert!((azimuth(LEFT_COL).abs() - PI).abs() < 1e-4, "LEFT_COL azimuth {}", azimuth(LEFT_COL));
    }

    /// The mesh's own normals point outward, away from the head's centre -
    /// checked at a ring far enough from the pole that "outward" is
    /// unambiguous - not inward (the bug this test is named for: a swapped
    /// cross-product order once made every fold's light and dark side
    /// backwards).
    #[test]
    fn normals_point_outward_not_inward() {
        let mut rng = Rng::new(11);
        let s = shape(&mut rng);
        let mut cloth = Cloth::spawn(s, body(V3::ZERO, 0.0), 0.45);
        let verts = cloth.render_vertices();
        for col in 0..COLS {
            let v = &verts[ring_col_index(2, col)];
            let outward = v.pos.sub(V3::new(0.0, v.pos.y, 0.0)).normalize();
            assert!(v.normal.dot(outward) > 0.5, "ring 2 col {col}: normal {:?} vs outward {outward:?}", v.normal);
        }
    }

    /// A freshly settled sheet, over a body that never moves, has every
    /// vertex finite and within a sane distance of the body - the first
    /// "does not explode" check, before anything is asked to move at all.
    #[test]
    fn settling_never_explodes() {
        let mut rng = Rng::new(11);
        for _ in 0..6 {
            let s = shape(&mut rng);
            let cloth = Cloth::spawn(s, body(V3::ZERO, 0.0), 0.5);
            for p in cloth.all_positions() {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                assert!(p.sub(V3::ZERO).length() < s.total_height() * 4.0 + 20.0, "{p:?} flew away for height={}", s.height);
            }
        }
    }

    /// The full `sway` range, and a wide range of shapes and gestures, all
    /// settle to something finite and bounded - not just the default.
    #[test]
    fn every_sway_setting_is_stable() {
        let mut rng = Rng::new(5);
        for sway in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let s = shape(&mut rng);
            let b = BodyPose {
                pos: V3::new(3.0, -2.0, 1.0),
                yaw: 0.4,
                arms: Arms { left: ArmPose { pitch: 1.0, elbow: 0.3 }, right: ArmPose { pitch: -1.0, elbow: -0.2 } },
            };
            let cloth = Cloth::spawn(s, b, sway);
            for p in cloth.all_positions() {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite(), "sway {sway}");
            }
        }
    }

    /// A moving, turning body with moving arms over several seconds of real
    /// motion never blows the sheet up either - the case the settle-only
    /// test above does not reach.
    #[test]
    fn moving_body_stays_stable() {
        let mut rng = Rng::new(21);
        let s = shape(&mut rng);
        let mut cloth = Cloth::spawn(s, body(V3::ZERO, 0.0), 0.9);
        let mut t = 0.0_f64;
        while t < 8.0 {
            t += 1.0 / 30.0;
            let tt = t;
            cloth.advance(t, 0.9, |sub| BodyPose {
                pos: V3::new(20.0 * (tt as f32 * 0.5).sin(), 2.0 * (tt as f32).cos(), 0.0),
                yaw: (sub as f32 * 0.3).sin(),
                arms: super::super::act::arm_gesture(s, sub as f32),
            });
        }
        for p in cloth.all_positions() {
            assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
            assert!(p.length() < 200.0, "{p:?} escaped");
        }
    }

    /// Stepping to the same target time by different call granularities
    /// gives the same sheet - the determinism every fixed-step sim here
    /// promises.
    #[test]
    fn advance_does_not_depend_on_call_granularity() {
        let mut rng = Rng::new(3);
        let s = shape(&mut rng);
        let body_at = |t: f64| body(V3::new(t as f32, 0.0, 0.0), 0.0);

        // Unlike `act`'s own step-size test - which can compare `poses_at`
        // at one fixed instant regardless of how far a loop overshot it,
        // because a `Pose` is a stateless function of `t` - `Cloth::advance`
        // is stateful and cannot be asked about a moment it has already
        // stepped past. So both loops are driven by step sizes that divide
        // the target evenly (60 steps of 1/30s, 90 of 1/45s), landing on
        // exactly 2.0 with no overshoot for either.
        let mut coarse = Cloth::spawn(s, body_at(0.0), 0.4);
        for i in 1..=60 {
            let t = i as f64 / 30.0;
            coarse.advance(t, 0.4, &body_at);
        }

        let mut fine = Cloth::spawn(s, body_at(0.0), 0.4);
        for i in 1..=90 {
            let t = i as f64 / 45.0;
            fine.advance(t, 0.4, &body_at);
        }

        for (a, b) in coarse.all_positions().iter().zip(fine.all_positions().iter()) {
            assert!(a.sub(*b).length() < 1e-4, "{a:?} vs {b:?}");
        }
    }

    /// Rapier's soft-body solver is deterministic given the same input
    /// sequence: two identically-built sheets, stepped the same way, must
    /// land on exactly the same bytes - the promise `enhanced-determinism`
    /// and single-threading exist for, checked directly rather than assumed.
    #[test]
    fn same_seed_and_steps_gives_the_same_sheet_twice() {
        let mut rng = Rng::new(17);
        let s = shape(&mut rng);
        let body_at = |t: f64| body(V3::new((t as f32 * 0.7).sin() * 10.0, 0.0, 0.0), (t as f32 * 0.3).sin());
        let run = || {
            let mut cloth = Cloth::spawn(s, body_at(0.0), 0.6);
            for i in 1..=150 {
                let t = i as f64 / 30.0;
                cloth.advance(t, 0.6, &body_at);
            }
            cloth.all_positions()
        };
        let a = run();
        let b = run();
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert_eq!((pa.x, pa.y, pa.z), (pb.x, pb.y, pb.z), "non-deterministic step");
        }
    }

    /// The free skirt never sinks into the kinematic head collider - "no
    /// interpenetration" checked directly, not just assumed from adding a
    /// collider. A sharp yaw whip is exactly the motion that would drive the
    /// skirt across the head if the collision were missing or the wrong
    /// radius (326's log names getting this radius wrong as a real bug).
    #[test]
    fn skirt_never_penetrates_the_head_collider() {
        let mut rng = Rng::new(41);
        let s = shape(&mut rng);
        let (_, neck_r) = dome_and_neck_radius(s.head_r);
        let body_at = |t: f64| body(V3::ZERO, (t as f32 * 4.0).sin() * 2.5); // a fast yaw whip in place.
        let mut cloth = Cloth::spawn(s, body_at(0.0), 0.8);
        let mut worst = f32::MAX;
        let mut t = 0.0_f64;
        while t < 4.0 {
            t += 1.0 / 60.0;
            cloth.advance(t, 0.8, body_at);
            for i in 0..VERTS {
                if cloth.template.kinematic[i] {
                    continue;
                }
                let d = cloth.pos(i).sub(V3::ZERO).length();
                worst = worst.min(d);
            }
        }
        // A little slack for the solver's own contact skin/prediction margin,
        // not a loosened test: comfortably inside the neck radius would mean
        // the collider is not doing anything.
        assert!(worst > neck_r * 0.6, "a free vertex reached {worst} inside a neck radius of {neck_r}");
    }

    /// The free skirt never sinks into either arm's own capsule colliders -
    /// the same "no interpenetration" promise as the head, now checked
    /// against the new body (card 336): swing one arm from drooped to raised
    /// and back, the motion most likely to sweep the skirt across it if the
    /// arm colliders were missing or mis-sized.
    #[test]
    fn skirt_never_penetrates_an_arm_collider() {
        let mut rng = Rng::new(53);
        let s = shape(&mut rng);
        let arm_r = s.head_r * 0.32;
        let body_at = |t: f64| BodyPose {
            pos: V3::ZERO,
            yaw: 0.0,
            arms: Arms {
                left: ArmPose { pitch: (t as f32 * 1.3).sin(), elbow: 0.16 },
                right: ArmPose { pitch: -0.15, elbow: 0.16 },
            },
        };
        let mut cloth = Cloth::spawn(s, body_at(0.0), 0.7);
        let mut worst = f32::MAX;
        let mut t = 0.0_f64;
        while t < 5.0 {
            t += 1.0 / 60.0;
            cloth.advance(t, 0.7, body_at);
            let b = body_at(t);
            let arms = b.arms_world(s);
            for i in 0..VERTS {
                if cloth.template.kinematic[i] {
                    continue;
                }
                let p = cloth.pos(i);
                let seg_dist = |a: V3, b: V3| -> f32 {
                    let ab = b.sub(a);
                    let len2 = ab.dot(ab).max(1e-6);
                    let t = ((p.sub(a).dot(ab)) / len2).clamp(0.0, 1.0);
                    p.sub(a.add(ab.scale(t))).length()
                };
                worst = worst.min(seg_dist(arms.left.0, arms.left.1));
                worst = worst.min(seg_dist(arms.left.1, arms.left.2));
            }
        }
        assert!(worst > arm_r * 0.6, "a free vertex reached {worst} inside an arm radius of {arm_r}");
    }

    /// The two wrist cuffs really do move with the arm gesture: a raised arm
    /// pulls its cuff well above a drooped one's - "the sheet hangs from
    /// those points" only means something if the points themselves move
    /// with the arm (card 336).
    #[test]
    fn wrist_cuffs_follow_the_arm_gesture() {
        let mut rng = Rng::new(9);
        let s = shape(&mut rng);
        let raised = BodyPose { pos: V3::ZERO, yaw: 0.0, arms: Arms { left: ArmPose { pitch: -0.15, elbow: 0.16 }, right: ArmPose { pitch: 1.0, elbow: 0.0 } } };
        let drooped = BodyPose { pos: V3::ZERO, yaw: 0.0, arms: Arms { left: ArmPose { pitch: -0.15, elbow: 0.16 }, right: ArmPose { pitch: -1.0, elbow: 0.0 } } };
        let mut a = Cloth::spawn(s, raised, 0.3);
        let mut b = Cloth::spawn(s, drooped, 0.3);
        // A few real steps at the held pose so the cuff has actually reached
        // its kinematic target (spawn's own settle already does this, but a
        // couple more steps at a fixed pose leaves no doubt).
        for _ in 0..30 {
            a.step(raised, PHYS_DT, 0.3);
            b.step(drooped, PHYS_DT, 0.3);
        }
        let (right_idx, _) = a.wrist_kinematic[0][0];
        let ya = a.pos(right_idx).y;
        let yb = b.pos(right_idx).y;
        assert!(ya > yb + 1.0, "raised cuff y={ya} not clearly above drooped cuff y={yb}");
    }
}
