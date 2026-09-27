//! The cloth: a sheet of particles draped over an invisible head, simulated by
//! Rapier's soft-body solver (card 327 - dimforge.com/blog/2026/09/25/
//! advanced-soft-bodies-for-games-in-the-rapier-physics-engine/), and the mesh
//! it produces for the renderer.
//!
//! **Why Rapier, and which model.** Card 326's hand-rolled Verlet + Gauss-
//! Seidel distance constraints worked but is exactly the kind of code a real
//! solver replaces: this rework keeps 326's geometry (the poncho template
//! below is untouched) and puts the *dynamics* on `rapier3d` 0.36's new
//! soft-body support. Two constraint models are on offer: mass-spring edges
//! (`SoftBodyBuilder::cloth`'s own family - structural/shear/bend distance
//! springs) or FEM over volumetric tetrahedral cells (`SoftBodyCellModel`,
//! `SoftBodySolver`). FEM in this release is a *volumetric* element model
//! (a cell is a tetrahedron, `[u32; 4]` in 3D); there is no thin-shell FEM
//! element, so it has nothing to attach to a one-particle-thick sheet without
//! inventing a fake thickness. Mass-spring edges are what a sheet actually
//! is, are exactly what Rapier's own `cloth`/`cloth_tube` generators use, and
//! is what this module builds by hand (our topology is a tapered tube of
//! rings, not their rectangular grid).
//!
//! **Topology.** Unchanged from 326: the sheet wraps all the way around the
//! head, like a poncho - a small fan of rings: one pole vertex at the crown,
//! [`HEAD_RINGS`] rings glued rigidly to the head (the skull and shoulders),
//! then [`SKIRT_RINGS`] free rings hanging below. [`COLS`] columns run around
//! the azimuth and wrap. See [`build_template`].
//!
//! **Kinematic vs free.** The pole and the head rings are pinned particles
//! (`SoftBody::set_particle_pinned`, done once via `SoftBodyBuilder::
//! pinned_particles`) whose target Rapier itself moves toward each step
//! (`SoftBody::set_particle_kinematic_target`) - "the head is what moves" is
//! now Rapier's own kinematic-particle feature rather than a hand-written
//! snap. The skirt rings are free, connected to the collar by real edges, so
//! momentum and lag come from Rapier's solver rather than ours.
//!
//! **Collision.** A kinematic `RigidBody` sphere, sized to the neck's own
//! (narrower) radius and moved every step to the head's pose, stands in for
//! "draped over a kinematic head collider" - the cloth's own default
//! boundary collision mesh (`SoftBodyBuilder::collider_template`, on by
//! default) meets it through Rapier's real contact solver, not a hand
//! sphere-push-out. Sized to the *neck's* radius, not the head's, on
//! purpose: 326's own log records the neck-vs-head-radius mixup as a real
//! bug once (the free skirt starts at the neck's narrower radius, not the
//! head's own widest point), and a wrong radius here would silently
//! reproduce it.
//!
//! **Air.** Rapier has no wind model of its own (checked: nothing in its
//! docs or source mentions one): [`Cloth::step`] adds a per-triangle
//! aerodynamic force each physics step (`SoftBody::add_particle_force`,
//! reset every step via `reset_forces`) - the standard flat-plate/pressure
//! cloth-wind force (Baraff & Witkin, "Large Steps in Cloth Simulation",
//! 1998, section 4.1): force along the face's own normal, proportional to
//! face area and the velocity component along that normal. A fold that
//! turns to face the direction of travel catches a lot of air; one edge-on
//! to it catches almost none - the drag/lift coupling the card asks for
//! falls out of that single term, no separate lift model needed.
//!
//! **Determinism.** `enhanced-determinism` is enabled in `Cargo.toml`
//! (forces libm over the platform's math intrinsics and an order-preserving
//! contact map); the crate's default is single-threaded already (the
//! `parallel` feature, which would pull in rayon, is never enabled); the
//! physics step is fixed-size ([`PHYS_DT`]) and `Cloth::advance` is the same
//! total-simulated-time accumulator as 326's (and `flock::Flock::advance`'s),
//! so two callers stepping by different call granularities land on the same
//! bytes, tested below.

use super::act::Shape;
use rapier3d::prelude::*;

pub(crate) const COLS: usize = 20;
/// Four rings round the dome, plus a fifth: the neck (see [`build_template`]).
pub(crate) const HEAD_RINGS: usize = 5;
pub(crate) const SKIRT_RINGS: usize = 9;
pub(crate) const RINGS: usize = HEAD_RINGS + SKIRT_RINGS;
/// Pole + every ring.
pub(crate) const VERTS: usize = 1 + RINGS * COLS;
/// The first ring that is simulated rather than glued to the head: rings
/// `0..HEAD_RINGS` are kinematic, so `HEAD_RINGS` itself is the first free
/// one - the ring the skirt actually starts hanging from.
const FIRST_FREE_RING: usize = HEAD_RINGS;

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
/// How much wider the hem flares than the neck. Past 1.0 the hem is wider
/// than the head itself, which is what makes it read as a sheet and not a
/// collar - most of this is spent early (see `flare_profile`), so the
/// "shoulders" are already wide just below the neck.
const FLARE: f32 = 1.55;

/// The dome's own widest radius and the neck's radius, both a plain function
/// of `r`: the two numbers `act::Shape` needs (for `margin`/`extent`) without
/// duplicating the rest of the template.
fn dome_and_neck_radius(r: f32) -> (f32, f32) {
    let dome = r * THETA_DOME.sin();
    (dome, dome * NECK_FACTOR)
}

/// The hem's own eventual radius: how far the flare reaches by `t = 1`.
fn hem_radius(r: f32) -> f32 {
    let (_, neck) = dome_and_neck_radius(r);
    neck * (1.0 + FLARE)
}

/// Fast early widening, easing off - the shoulders are most of the way to
/// full width within the first skirt ring or two, not a slow taper all the
/// way to the hem.
fn flare_profile(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// How far this ghost's cloth can reach from its own centreline, at its
/// widest - the hem's flare, almost always, since [`FLARE`] makes it wider
/// than the dome. What an entrance, an exit or a peek has to clear before
/// nothing of the *sheet*, not just the head, is on screen.
pub(crate) fn extent(shape: Shape) -> f32 {
    let r = shape.r.max(0.8);
    let (dome, _) = dome_and_neck_radius(r);
    let hem = hem_radius(r) + shape.hem_amp * 0.74; // the lobes' own bulge, at their biggest.
    dome.max(hem)
}

/// How far the crown sits above the head's own centre - always exactly `r`,
/// whatever the neck pinch does below it.
pub(crate) fn rise_above_centre(shape: Shape) -> f32 {
    shape.r.max(0.8)
}

/// How far the lowest lobe's own drip sits below the head's centre - the
/// neck's own (small) rise above centre subtracted back out, so this matches
/// [`build_template`]'s actual geometry rather than a guess at it.
pub(crate) fn drop_below_centre(shape: Shape) -> f32 {
    let r = shape.r.max(0.8);
    let hem_len = (shape.hem_base + shape.hem_amp).max(r * 0.6);
    let neck_y = r * THETA_NECK.cos();
    (hem_len + shape.hem_amp * 0.6 - neck_y).max(hem_len * 0.5)
}

/// Crown to the lowest a lobe's own drip ever reaches - what a vertical
/// placement has to clear at both ends, matching [`build_template`]'s own
/// geometry rather than a guess at it.
pub(crate) fn total_height(shape: Shape) -> f32 {
    rise_above_centre(shape) + drop_below_centre(shape)
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
    /// degenerate geometry (two coincident particles) must not poison a step.
    pub fn normalize(self) -> V3 {
        let l = self.length();
        if l > 1e-6 {
            self.scale(1.0 / l)
        } else {
            V3::ZERO
        }
    }
    /// Rotate around world Y by `yaw` radians - the only rotation a head ever
    /// does here (card 326's "turn").
    pub fn rot_y(self, yaw: f32) -> V3 {
        let (s, c) = yaw.sin_cos();
        V3::new(c * self.x + s * self.z, self.y, -s * self.x + c * self.z)
    }
}

/// To/from Rapier's own vector type (`glam::Vec3` under `f32`/`dim3`) at the
/// one boundary that needs it - every other line of geometry in this module
/// stays in [`V3`], unchanged from 326.
fn to_rapier(v: V3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}
fn from_rapier(v: Vector) -> V3 {
    V3::new(v.x, v.y, v.z)
}

/// The head's rigid placement in world space at one instant.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeadTarget {
    pub pos: V3,
    pub yaw: f32,
}

impl HeadTarget {
    fn to_world(self, local: V3) -> V3 {
        local.rot_y(self.yaw).add(self.pos)
    }
    /// This instant as a Rapier `Pose`, for the kinematic head collider.
    fn to_pose(self) -> Pose {
        Pose::from_parts(to_rapier(self.pos), Rot3::from_scaled_axis(Vector::new(0.0, self.yaw, 0.0)))
    }
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
    /// `true` for the pole and every head ring: driven by the head's rigid
    /// transform every step, never by physics.
    kinematic: Vec<bool>,
    ring_of: Vec<usize>,
    col_of: Vec<usize>,
}

fn ring_col_index(ring: usize, col: usize) -> usize {
    1 + ring * COLS + col % COLS
}

fn build_template(shape: Shape) -> Template {
    let r = shape.r.max(0.8);
    let hem_len = (shape.hem_base + shape.hem_amp).max(r * 0.6);
    let humps = shape.humps;

    let mut local = vec![V3::ZERO; VERTS];
    let mut kinematic = vec![false; VERTS];
    let mut ring_of = vec![0usize; VERTS];
    let mut col_of = vec![0usize; VERTS];

    local[0] = V3::new(0.0, r, 0.0);
    kinematic[0] = true;

    // Head rings: the first `HEAD_RINGS - 1` are rigid points on the dome
    // itself, theta running from just off the crown to short of the equator
    // (`THETA_DOME`) - a ball, not a shape still widening when the cloth
    // takes over. The last one is the neck: same rigid, kinematic ring, but
    // pulled in to `NECK_FACTOR` of the dome's own widest radius, which is
    // the pinch that separates "head" from "shoulders" at a glance.
    for ring in 0..HEAD_RINGS - 1 {
        let theta = THETA_TOP + (THETA_DOME - THETA_TOP) * ring as f32 / (HEAD_RINGS - 2) as f32;
        let (radius, y) = (r * theta.sin(), r * theta.cos());
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
    let (_, neck_radius) = dome_and_neck_radius(r);
    let neck_y = r * THETA_NECK.cos();
    for col in 0..COLS {
        let i = ring_col_index(neck_ring, col);
        let az = azimuth(col);
        local[i] = V3::new(neck_radius * az.cos(), neck_y, neck_radius * az.sin());
        kinematic[i] = true;
        ring_of[i] = neck_ring;
        col_of[i] = col;
    }

    // Skirt rings: hanging further, flaring wider than the head itself
    // (`FLARE`, most of it spent early - `flare_profile` - so the shoulders
    // are wide just below the neck), with a per-column wave (`humps`) that
    // only ever pulls the hem *down* and *out* at a few spots, never up -
    // "a few distinct lobes/points where the cloth hangs", not a symmetric
    // wobble.
    for j in 1..=SKIRT_RINGS {
        let ring = HEAD_RINGS + j - 1;
        let t = j as f32 / SKIRT_RINGS as f32;
        let base_radius = neck_radius * (1.0 + FLARE * flare_profile(t));
        let base_y = neck_y - hem_len * t;
        for col in 0..COLS {
            let i = ring_col_index(ring, col);
            let az = azimuth(col);
            let lobe = 0.5 + 0.5 * (humps * az + shape.phase0).cos();
            let drip = shape.hem_amp * 0.6 * t * t * lobe;
            let bulge = shape.hem_amp * 0.14 * t * lobe;
            let radius = base_radius + bulge;
            let y = base_y - drip;
            local[i] = V3::new(radius * az.cos(), y, radius * az.sin());
            ring_of[i] = ring;
            col_of[i] = col;
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
/// billows and settles rather than snapping straight - the "softer billow"
/// the orchestrator's review of 326 asked for.
fn edge_softness(sway: f32) -> SpringCoefficients<Real> {
    let s = sway.clamp(0.0, 1.0);
    SpringCoefficients::new(lerp(46.0, 12.0, s), lerp(0.95, 0.55, s))
}
/// Bending resists far less than stretching, at every `sway` - the same
/// ratio 326's hand-rolled weights used (`0.35` of structural) - but kept
/// stiff enough, and damped enough, that a lateral gust folds the skirt
/// rather than swinging the whole thing as one rigid triangular flap: tuned
/// down from an initial pass by rendering (see the Log).
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
/// mesh has a little padding against the head collider.
const PARTICLE_RADIUS: f32 = 0.4;
/// The head collider's own skin, so the cloth's contact margin has a little
/// clearance from the collider surface it is draped over, on top of the
/// solver's own contact prediction distance.
const HEAD_SKIN: f32 = 0.15;
/// Standard flat-plate aerodynamic drag (see the module doc): force along a
/// triangle's own normal, scaled by its area and the velocity component
/// along that normal. Tuned by rendering - big enough that a fast turn or a
/// swoop visibly trails and billows the hem, far below anything that would
/// overpower gravity and the structural springs at rest.
const AIR_DRAG: f32 = 0.018;

/// The sheet's live state: Rapier's own soft-body world (one per ghost - a
/// ghost's cloth is simulated continuously across frames, `mod.rs` keys one
/// by `Pose::key`), the fixed template and triangle list, plus the head's
/// own kinematic collider.
pub(crate) struct Cloth {
    template: Template,
    faces: Vec<[u32; 3]>,
    kinematic: Vec<usize>,
    world: PhysicsWorld,
    body: SoftBodyHandle,
    head_body: RigidBodyHandle,
    /// Simulated seconds already applied - the fixed-step accumulator's own
    /// clock, so `advance` never depends on how finely it is called (the
    /// house rule every simulation in this codebase follows).
    warped: f64,
}

impl Cloth {
    /// Cast a fresh sheet over a head held still at `head0`, and settle it
    /// under gravity before anyone can see it (card 326: "warms up
    /// invisibly ... the first frame is a settled sheet, not a falling
    /// one"). `sway` is applied at settle time too, so a very floppy sheet
    /// still starts from a sheet-shaped rest, not a mid-fall one.
    pub(crate) fn spawn(shape: Shape, head0: HeadTarget, sway: f32) -> Cloth {
        let template = build_template(shape);
        let faces = triangles();
        let kinematic: Vec<usize> = (0..VERTS).filter(|&i| template.kinematic[i]).collect();
        let (edges, bend_edges) = build_edges();
        let positions: Vec<Vector> = template.local.iter().map(|&l| to_rapier(head0.to_world(l))).collect();

        let material = SoftBodyMaterial {
            edge_softness: edge_softness(sway),
            bend_softness: bend_softness(sway),
            ..Default::default()
        };
        let builder = SoftBodyBuilder::new(positions)
            .pinned_particles(kinematic.iter().map(|&i| i as u32))
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
        let (_, neck_r) = dome_and_neck_radius(shape.r.max(0.8));
        let (head_body, _) = world.insert(
            RigidBodyBuilder::kinematic_position_based().pose(head0.to_pose()),
            ColliderBuilder::ball(neck_r).contact_skin(HEAD_SKIN),
        );

        let mut cloth = Cloth { template, faces, kinematic, world, body, head_body, warped: 0.0 };
        const SETTLE_STEPS: usize = 420;
        for _ in 0..SETTLE_STEPS {
            cloth.step(head0, PHYS_DT, sway);
        }
        cloth
    }

    /// One fixed-size physics step: move the head collider and the
    /// kinematic collar to `head`'s new pose, refresh the live `sway`
    /// softness, add this step's aerodynamic force, then let Rapier step.
    fn step(&mut self, head: HeadTarget, dt: f32, sway: f32) {
        self.world.integration_parameters.dt = dt;
        self.world.bodies[self.head_body].set_next_kinematic_position(head.to_pose());

        {
            let sb = &mut self.world.soft_bodies[self.body];
            for &i in &self.kinematic {
                sb.set_particle_kinematic_target(i, to_rapier(head.to_world(self.template.local[i])));
            }
            let mat = sb.material_mut();
            mat.edge_softness = edge_softness(sway);
            mat.bend_softness = bend_softness(sway);
        }

        self.apply_air();
        self.world.step();

        // A last safety net: anything that went non-finite (a pathological
        // parameter combination, not one this design should reach) is
        // snapped back to the head rather than left to poison every frame
        // after it - "no exploding cloth at any param setting" has to hold
        // even if a future edit gets a constant wrong.
        let sb = &mut self.world.soft_bodies[self.body];
        for i in 0..VERTS {
            let p = sb.particle_position(i);
            if !p.x.is_finite() || !p.y.is_finite() || !p.z.is_finite() {
                let target = to_rapier(head.to_world(self.template.local[i]));
                sb.set_particle_position(i, target);
                sb.set_particle_velocity(i, Vector::ZERO);
            }
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
    /// at the fixed [`PHYS_DT`], asking `head_at` for the head's rigid pose
    /// at each step's own absolute time. The step count comes from total
    /// simulated time, not a counter, so two callers stepping by different
    /// amounts still agree (card 326's rule, and `flock::Flock::advance`'s).
    pub(crate) fn advance(&mut self, target: f64, sway: f32, mut head_at: impl FnMut(f64) -> HeadTarget) {
        const CATCHUP: i64 = 240; // at most 1.3s of steps in one call, however far `target` jumped.
        let dt = f64::from(PHYS_DT);
        let already = (self.warped / dt).round() as i64;
        let want = (target / dt + 1e-6).floor() as i64;
        let from = already.max(want - CATCHUP);
        for s in from..want {
            let t = (s + 1) as f64 * dt;
            self.step(head_at(t), PHYS_DT, sway);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patches::ghosts::act::Shape;
    use crate::rng::Rng;

    fn shape(rng: &mut Rng) -> Shape {
        Shape::new(rng, 12.0)
    }

    fn head(pos: V3, yaw: f32) -> HeadTarget {
        HeadTarget { pos, yaw }
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
        let mut cloth = Cloth::spawn(s, head(V3::ZERO, 0.0), 0.45);
        let verts = cloth.render_vertices();
        for col in 0..COLS {
            let v = &verts[ring_col_index(2, col)];
            let outward = v.pos.sub(V3::new(0.0, v.pos.y, 0.0)).normalize();
            assert!(v.normal.dot(outward) > 0.5, "ring 2 col {col}: normal {:?} vs outward {outward:?}", v.normal);
        }
    }

    /// A freshly settled sheet, over a head that never moves, has every
    /// vertex finite and within a sane distance of the head - the first
    /// "does not explode" check, before anything is asked to move at all.
    #[test]
    fn settling_never_explodes() {
        let mut rng = Rng::new(11);
        for _ in 0..6 {
            let s = shape(&mut rng);
            let cloth = Cloth::spawn(s, head(V3::ZERO, 0.0), 0.5);
            for p in cloth.all_positions() {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                assert!(p.sub(V3::ZERO).length() < s.total_height() * 4.0 + 20.0, "{p:?} flew away for r={}", s.r);
            }
        }
    }

    /// The full `sway` range, and a wide range of shapes, all settle to
    /// something finite and bounded - not just the default.
    #[test]
    fn every_sway_setting_is_stable() {
        let mut rng = Rng::new(5);
        for sway in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let s = shape(&mut rng);
            let cloth = Cloth::spawn(s, head(V3::new(3.0, -2.0, 1.0), 0.4), sway);
            for p in cloth.all_positions() {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite(), "sway {sway}");
            }
        }
    }

    /// A moving, turning head over several seconds of real motion never
    /// blows the sheet up either - the case the settle-only test above does
    /// not reach.
    #[test]
    fn moving_head_stays_stable() {
        let mut rng = Rng::new(21);
        let s = shape(&mut rng);
        let mut cloth = Cloth::spawn(s, head(V3::ZERO, 0.0), 0.9);
        let mut t = 0.0_f64;
        while t < 8.0 {
            t += 1.0 / 30.0;
            let tt = t;
            cloth.advance(t, 0.9, |sub| {
                head(V3::new(20.0 * (tt as f32 * 0.5).sin(), 2.0 * (tt as f32).cos(), 0.0), (sub as f32 * 0.3).sin())
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
        let head_at = |t: f64| head(V3::new(t as f32, 0.0, 0.0), 0.0);

        // Unlike `act`'s own step-size test - which can compare `poses_at`
        // at one fixed instant regardless of how far a loop overshot it,
        // because a `Pose` is a stateless function of `t` - `Cloth::advance`
        // is stateful and cannot be asked about a moment it has already
        // stepped past. So both loops are driven by step sizes that divide
        // the target evenly (60 steps of 1/30s, 90 of 1/45s), landing on
        // exactly 2.0 with no overshoot for either.
        let mut coarse = Cloth::spawn(s, head_at(0.0), 0.4);
        for i in 1..=60 {
            let t = i as f64 / 30.0;
            coarse.advance(t, 0.4, &head_at);
        }

        let mut fine = Cloth::spawn(s, head_at(0.0), 0.4);
        for i in 1..=90 {
            let t = i as f64 / 45.0;
            fine.advance(t, 0.4, &head_at);
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
        let head_at = |t: f64| head(V3::new((t as f32 * 0.7).sin() * 10.0, 0.0, 0.0), (t as f32 * 0.3).sin());
        let run = || {
            let mut cloth = Cloth::spawn(s, head_at(0.0), 0.6);
            for i in 1..=150 {
                let t = i as f64 / 30.0;
                cloth.advance(t, 0.6, &head_at);
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
        let (_, neck_r) = dome_and_neck_radius(s.r.max(0.8));
        let head_at = |t: f64| head(V3::ZERO, (t as f32 * 4.0).sin() * 2.5); // a fast yaw whip in place.
        let mut cloth = Cloth::spawn(s, head_at(0.0), 0.8);
        let mut worst = f32::MAX;
        let mut t = 0.0_f64;
        while t < 4.0 {
            t += 1.0 / 60.0;
            cloth.advance(t, 0.8, head_at);
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
}
