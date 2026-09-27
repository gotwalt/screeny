//! Each leaf's own thin deformable shell (card 322): the leaf mesh
//! ([`super::mesh`]), simulated rather than drawn rigidly - a Rapier 0.36
//! soft body over the same mass-spring edges card 327's `ghosts` cloth uses
//! (this release's FEM elements are volumetric tetrahedra, nothing thin-shell
//! to attach to a one-particle-thick sheet - see `review/327-ghosts-rapier-
//! cloth.md`'s Log for the read-through; that finding carries straight over).
//!
//! **What stays, what's new.** Card 321's whole-leaf flight model
//! ([`super::leaf::Leaf3D::step`] - the Cd/Cl/torque law that decides how a
//! leaf falls, glides, rocks or tumbles) is untouched: it is what still
//! drives this leaf's own rigid position and orientation, exactly as before.
//! This module drapes the actual blade mesh over that rigid attachment the
//! way `ghosts`' cloth drapes over a moving head: the two stem rings
//! ([`super::mesh::STEM_RINGS`]) are pinned particles whose kinematic target
//! Rapier itself moves toward, every step, to wherever `leaf.orient.rotate(..)
//! .add(leaf.pos)` says the stem is right now; the blade's `LEAF_RINGS`
//! beyond it are free, connected to the stem and to each other by mass-spring
//! edges, so the blade can lag, sag and flex a little relative to the rigid
//! attachment instead of following it as a perfectly stiff card.
//!
//! **Air, again.** Rapier has no wind model (checked again for 0.36; same
//! finding as 327's own Log). [`LeafShell::apply_air`] is the same
//! standard flat-plate/pressure force `ghosts::cloth::apply_air` adds - one
//! per triangle, along its own current normal, proportional to face area and
//! the velocity component along that normal - which is "keeping 321's own
//! [aerodynamics] as external per-triangle forces": the same physical law
//! (a flat plate feels a force along its own normal proportional to the
//! wind's component through it) [`leaf::Leaf3D::step`] already applies once
//! for the whole leaf, applied again here per triangle so the *shape* can
//! respond to it, not just the attitude.
//!
//! **Determinism and scale.** Same fixed step as the rigid flight
//! ([`super::leaf::STEP`], 1/90 s - stepped once per call to
//! [`LeafShell::step`], kept in lockstep with `Leaf3D::step` by
//! `LeavesPatch::step_once` calling both together) so a snapshot at any
//! render rate is still the same picture; `enhanced-determinism` and
//! single-threading are the crate-level features `Cargo.toml` already turned
//! on for `ghosts` (card 327), inherited unchanged - this module does not
//! touch that. A leaf's world scale is small (half-length `HALF_LEN_M =
//! 0.75` - `gpu.rs`) next to `ghosts`' head-sized cloth, so every spring and
//! mass constant below is its own, independently tuned number, not 327's
//! reused.

use super::geom::{v3, Quat, V3};
use super::leaf::{Build, Leaf3D, STEP};
use super::mesh;
use super::wind::Wind;
use rapier3d::prelude::*;

pub(crate) const VERTS: usize = mesh::RINGS * mesh::LATERAL;
/// The first free (non-kinematic) vertex index: everything in the stem
/// rings is pinned, the blade rings beyond it are simulated.
const FIRST_FREE_RING: usize = mesh::STEM_RINGS;

fn to_rapier(v: V3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}
fn from_rapier(v: Vector) -> V3 {
    v3(v.x, v.y, v.z)
}

/// The rigid attachment this step: where the stem is and which way it
/// points, straight from [`Leaf3D`] - the same transform `gpu.rs::push_leaf`
/// used to apply rigidly before this card.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attachment {
    pub pos: V3,
    pub orient: Quat,
}

impl Attachment {
    pub(crate) fn of(leaf: &Leaf3D) -> Attachment {
        Attachment { pos: leaf.pos, orient: leaf.orient }
    }
    fn to_world(self, local: V3, scale: f32) -> V3 {
        self.orient.rotate(local.scale(scale)).add(self.pos)
    }
}

/// One vertex, ready for the GPU: world position, world normal (from the
/// shell's *actual*, possibly-flexed neighbours, not the flat rest mesh),
/// and a fold/crease scalar (card 322: "curl and crease catching the key
/// light" - the same curvature-proxy idea `ghosts::cloth::compute_fold`
/// uses, so a real bend really does darken its own crease).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderVertex {
    pub pos: V3,
    pub normal: V3,
    pub fold: f32,
}

impl RenderVertex {
    /// This vertex, taken out of `from`'s attachment-relative frame and
    /// re-posed into `to`'s: `to == from` is the identity. Used by
    /// `gpu.rs::push_shell_posed` for the motion-blur echoes - re-deriving
    /// the shell's own local (attachment-relative) shape once and re-posing
    /// it at each historical step's rigid transform, rather than drawing a
    /// second, geometrically inconsistent shape for echoes (see that
    /// function's own doc for the bug this fixes). The fold scalar is a
    /// curvature measure, not a position - pose-independent, passed through
    /// unchanged.
    pub(crate) fn reposed(self, from: Attachment, to: Attachment) -> RenderVertex {
        let inv = from.orient.conjugate();
        let local_pos = inv.rotate(self.pos.sub(from.pos));
        let local_normal = inv.rotate(self.normal);
        RenderVertex { pos: to.orient.rotate(local_pos).add(to.pos), normal: to.orient.rotate(local_normal), fold: self.fold }
    }
}

/// Structural (ring-to-ring, across-ring, shear diagonal) and bending
/// (skip-one along the length) edges, skipping any pair whose *both*
/// endpoints are pinned stem particles - two rigid points never need a
/// constraint between them (`ghosts::cloth::build_edges`'s own reasoning,
/// unchanged).
fn build_edges() -> (Vec<[u32; 2]>, Vec<[u32; 2]>) {
    let idx = |ring: usize, lateral: usize| mesh::vertex_index(ring, lateral) as u32;
    let both_pinned = |r0: usize, r1: usize| r0 < FIRST_FREE_RING && r1 < FIRST_FREE_RING;
    let mut edges = Vec::new();
    let mut bend = Vec::new();

    for ring in 0..mesh::RINGS {
        // Across the ring (left-mid, mid-right): every ring has this, stem
        // included, since a stem ring's own three points are still all
        // kinematic targets set independently and a spring between them
        // costs nothing to skip per the rule above - so only add it where at
        // least one of the two is free. Stem rings are entirely pinned, so
        // this never actually fires below `FIRST_FREE_RING`; kept as an
        // explicit `if` rather than relying on that being obviously true.
        if ring >= FIRST_FREE_RING {
            edges.push([idx(ring, 0), idx(ring, 1)]);
            edges.push([idx(ring, 1), idx(ring, 2)]);
        }
        if ring + 1 < mesh::RINGS && !both_pinned(ring, ring + 1) {
            for lateral in 0..mesh::LATERAL {
                edges.push([idx(ring, lateral), idx(ring + 1, lateral)]);
            }
            // Shear diagonals across each of the two lateral cells.
            edges.push([idx(ring, 0), idx(ring + 1, 1)]);
            edges.push([idx(ring, 1), idx(ring + 1, 0)]);
            edges.push([idx(ring, 1), idx(ring + 1, 2)]);
            edges.push([idx(ring, 2), idx(ring + 1, 1)]);
        }
        if ring + 2 < mesh::RINGS && !both_pinned(ring, ring + 2) {
            for lateral in 0..mesh::LATERAL {
                bend.push([idx(ring, lateral), idx(ring + 2, lateral)]);
            }
        }
    }
    (edges, bend)
}

/// A soft body's own material is tuned once for every leaf: a leaf is much
/// stiffer than a sheet (`ghosts`' cloth), so both families sit well above
/// that patch's own numbers - firm enough to hold a leaf's shape, soft
/// enough that a real gust or a hard tumble visibly bends it.
const EDGE_SOFTNESS: SpringCoefficients<Real> = SpringCoefficients { natural_frequency: 70.0, damping_ratio: 1.1 };
const BEND_SOFTNESS: SpringCoefficients<Real> = SpringCoefficients { natural_frequency: 26.0, damping_ratio: 1.1 };

const PARTICLE_MASS: f32 = 0.05;
const PARTICLE_RADIUS: f32 = 0.03;
/// A fraction of [`super::leaf::GRAVITY`]-equivalent: the blade's *own*
/// weight relative to the rigid stem it hangs from, not the leaf's bulk fall
/// (that is still entirely [`Leaf3D::step`]'s doing). Small on purpose - a
/// leaf sags a little under its own weight between gusts, it does not droop
/// like wet cloth. The stem's own anchor is thin (two rings, tapering to
/// almost nothing at the very tip - `mesh::ring`'s stem case), so it
/// constrains position far better than it constrains *rotation* about its
/// own long axis; tuned down hard from an initial pass that swung the whole
/// blade like a slow pendulum around that axis (see the card's Log) rather
/// than merely sagging.
const SHELL_GRAVITY: f32 = 0.02;
/// The per-triangle aerodynamic force's own strength (see the module doc):
/// tuned by rendering - see the card's Log - big enough that a real gust or
/// a fast tumble visibly bends the blade, far below anything that would
/// tear it away from its own rest shape at a becalmed moment.
const AIR_K: f32 = 0.06;

pub(crate) struct LeafShell {
    faces: Vec<[u32; 3]>,
    pinned: Vec<usize>,
    local: Vec<V3>,
    world: PhysicsWorld,
    body: SoftBodyHandle,
    /// This leaf's own scale ([`super::leaf::Build::size`] times
    /// [`super::gpu::HALF_LEN_M`]) - fixed for the leaf's whole life, needed
    /// every step to turn a kinematic target's *local* rest position into a
    /// *world* one.
    scale: f32,
}

impl LeafShell {
    /// Cast a fresh shell over a leaf just spawned at `attach`, and settle
    /// it briefly under its own weight before anyone can see it - the same
    /// "warm up invisibly" rule every simulation in this codebase follows
    /// (`ghosts::cloth::Cloth::spawn`'s own doc), so frame one is already a
    /// leaf with a settled, not a mid-snap, blade.
    pub(crate) fn spawn(build: Build, scale: f32, attach: Attachment) -> LeafShell {
        let (local, faces) = mesh::indexed(build.shape, build.cup, build.curl);
        let pinned: Vec<usize> = (0..VERTS).filter(|&i| i / mesh::LATERAL < FIRST_FREE_RING).collect();
        let (edges, bend_edges) = build_edges();
        let positions: Vec<Vector> = local.iter().map(|&l| to_rapier(attach.to_world(l, scale))).collect();

        let material = SoftBodyMaterial { edge_softness: EDGE_SOFTNESS, bend_softness: BEND_SOFTNESS, ..Default::default() };
        let builder = SoftBodyBuilder::new(positions)
            .pinned_particles(pinned.iter().map(|&i| i as u32))
            .edges(edges)
            .bend_edges(bend_edges)
            .surface(faces.clone())
            .material(material)
            .particle_mass(PARTICLE_MASS)
            .particle_radius(PARTICLE_RADIUS)
            .self_contacts(false);

        let mut world = PhysicsWorld::new();
        world.gravity = Vector::new(0.0, -SHELL_GRAVITY, 0.0);
        world.integration_parameters.dt = STEP;
        let body = world.insert_soft_body(builder);

        let mut shell = LeafShell { faces, pinned, local, world, body, scale };
        const SETTLE_STEPS: usize = 240;
        for _ in 0..SETTLE_STEPS {
            shell.step(attach, &Wind::new(0), 0.0, 0.0, 0.0);
        }
        shell
    }

    /// One fixed step, kept in lockstep with [`Leaf3D::step`]: move the
    /// pinned stem to `attach`'s new pose, add this step's per-triangle air
    /// force, then let Rapier step. `wind`/`t`/`mean`/`gusts` are the same
    /// field [`Leaf3D::step`] flies through, sampled at each triangle's own
    /// world position so a gust bends the blade where it actually reaches it.
    pub(crate) fn step(&mut self, attach: Attachment, wind: &Wind, t: f32, mean: f32, gusts: f32) {
        {
            let sb = &mut self.world.soft_bodies[self.body];
            for &i in &self.pinned {
                sb.set_particle_kinematic_target(i, to_rapier(attach.to_world(self.local[i], self.scale)));
            }
        }
        self.apply_air(wind, t, mean, gusts);
        self.world.step();

        // The same non-finite safety net `ghosts::cloth::Cloth::step` has: a
        // pathological parameter combination must not poison every frame
        // after it, however unlikely at this shell's own tuned range.
        let sb = &mut self.world.soft_bodies[self.body];
        for i in 0..VERTS {
            let p = sb.particle_position(i);
            if !p.x.is_finite() || !p.y.is_finite() || !p.z.is_finite() {
                let target = to_rapier(attach.to_world(self.local[i], self.scale));
                sb.set_particle_position(i, target);
                sb.set_particle_velocity(i, Vector::ZERO);
            }
        }
    }

    /// The standard flat-plate/pressure aerodynamic force (see the module
    /// doc), one triangle at a time, using the *actual* relative wind at
    /// that triangle's own current world position - still air is
    /// `Wind::at3(.., mean, gusts) = 0` almost everywhere gusts are off, so
    /// this is a no-op at rest exactly like `ghosts`'s version is.
    fn apply_air(&mut self, wind: &Wind, t: f32, mean: f32, gusts: f32) {
        let sb = &mut self.world.soft_bodies[self.body];
        // `wake_up: true` - a shell settled and asleep (a resting leaf, or a
        // becalmed falling one) must not stay asleep through a real gust: a
        // sleeping body ignores added force outright (checked against the
        // crate's own source - `add_particle_force`'s `wake_up` flag is the
        // only thing that can rouse it), which first showed up here as a
        // gust that visibly should have bent the blade and, by the test
        // below, provably did not.
        sb.reset_forces(true);
        for &[a, b, c] in &self.faces {
            let (a, b, c) = (a as usize, b as usize, c as usize);
            let (pa, pb, pc) = (sb.particle_position(a), sb.particle_position(b), sb.particle_position(c));
            let cross = (pb - pa).cross(pc - pa);
            let area2 = cross.length();
            if area2 < 1e-9 {
                continue;
            }
            let n = cross / area2;
            let centroid = (pa + pb + pc) / 3.0;
            let w = wind.at3(centroid.x, centroid.y, centroid.z, t, mean, gusts);
            let v = (sb.particle_velocity(a) + sb.particle_velocity(b) + sb.particle_velocity(c)) / 3.0;
            let vn = (v - to_rapier(w)).dot(n);
            let force = n * (-AIR_K * area2 * vn);
            let share = force / 3.0;
            sb.add_particle_force(a, share, false);
            sb.add_particle_force(b, share, false);
            sb.add_particle_force(c, share, false);
        }
    }

    fn pos(&self, i: usize) -> V3 {
        from_rapier(self.world.soft_bodies[self.body].particle_position(i))
    }

    fn neighbour(&self, i: usize, d_ring: i64, d_lateral: i64) -> Option<usize> {
        let ring = i / mesh::LATERAL;
        let lateral = i % mesh::LATERAL;
        let r = ring as i64 + d_ring;
        let l = lateral as i64 + d_lateral;
        if !(0..mesh::RINGS as i64).contains(&r) || !(0..mesh::LATERAL as i64).contains(&l) {
            return None;
        }
        Some(mesh::vertex_index(r as usize, l as usize))
    }

    /// A normal from this vertex's *actual*, live neighbours - a central
    /// difference along the ring direction and across it, exactly
    /// [`mesh::normal_at`]'s idea, just read off the physics body's current
    /// positions instead of the fixed rest template, so a real flex really
    /// does change how the surface catches the light.
    fn normal_at(&self, i: usize) -> V3 {
        let p = self.pos(i);
        let dx = match (self.neighbour(i, -1, 0), self.neighbour(i, 1, 0)) {
            (Some(a), Some(b)) => self.pos(b).sub(self.pos(a)),
            (Some(a), None) => p.sub(self.pos(a)),
            (None, Some(b)) => self.pos(b).sub(p),
            (None, None) => v3(1.0, 0.0, 0.0),
        };
        let dy = match (self.neighbour(i, 0, -1), self.neighbour(i, 0, 1)) {
            (Some(a), Some(b)) => self.pos(b).sub(self.pos(a)),
            (Some(a), None) => p.sub(self.pos(a)),
            (None, Some(b)) => self.pos(b).sub(p),
            (None, None) => v3(0.0, 1.0, 0.0),
        };
        dx.cross(dy).unit_or(v3(0.0, 0.0, 1.0))
    }

    /// A cheap curvature proxy at each vertex: how far it sits behind the
    /// plane its ring/lateral neighbours describe, along its own normal -
    /// `ghosts::cloth::compute_fold`'s exact idea, ported to this mesh's
    /// smaller neighbourhood (up to four neighbours: ring-, ring+, lateral-,
    /// lateral+, rather than a full quad grid). Negative (a valley, the
    /// midrib crease or a fresh fold) darkens in the shader; convex points
    /// are left alone.
    fn fold_at(&self, i: usize) -> f32 {
        let mut acc = V3::ZERO;
        let mut n = 0.0;
        for (dr, dl) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            if let Some(j) = self.neighbour(i, dr, dl) {
                acc = acc.add(self.pos(j));
                n += 1.0;
            }
        }
        if n < 2.0 {
            return 0.0;
        }
        let avg = acc.scale(1.0 / n);
        let laplacian = self.pos(i).sub(avg);
        laplacian.dot(self.normal_at(i))
    }

    /// Every vertex, ready for the renderer: world position, a normal from
    /// the live geometry, and its fold/crease scalar.
    pub(crate) fn render_vertices(&self) -> Vec<RenderVertex> {
        (0..VERTS).map(|i| RenderVertex { pos: self.pos(i), normal: self.normal_at(i), fold: self.fold_at(i) }).collect()
    }

    /// This leaf's own triangles, in [`render_vertices`]'s indexing - the
    /// *one* implementation of this leaf's mesh topology, shared by the
    /// physics body's own collision surface (built from this exact list at
    /// [`spawn`]) and [`super::gpu::push_shell`], so the render can never
    /// triangulate a leaf differently than the simulation does.
    pub(crate) fn faces(&self) -> &[[u32; 3]] {
        &self.faces
    }

    #[cfg(test)]
    pub(crate) fn all_positions(&self) -> Vec<V3> {
        (0..VERTS).map(|i| self.pos(i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn build(rng: &mut Rng) -> Build {
        Build {
            size: rng.range(0.75, 1.35),
            inertia: 1.0,
            shape: (rng.f32() * mesh::SHAPES as f32) as u8,
            hue: 0,
            cup: rng.range(0.06, 0.30),
            curl: rng.range(-0.22, 0.30),
            spin_bias: 0.0,
        }
    }

    fn attach(pos: V3, orient: Quat) -> Attachment {
        Attachment { pos, orient }
    }

    /// A freshly settled shell, over a leaf that never moves, has every
    /// particle finite and close to its own rest shape - the first "does not
    /// explode" check, before anything is asked to move at all.
    #[test]
    fn settling_never_explodes() {
        let mut rng = Rng::new(1);
        for _ in 0..8 {
            let b = build(&mut rng);
            let shell = LeafShell::spawn(b, 0.75 * b.size, attach(V3::ZERO, Quat::IDENTITY));
            for p in shell.all_positions() {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                assert!(p.sub(V3::ZERO).len() < 5.0, "{p:?} flew away for size {}", b.size);
            }
        }
    }

    /// A moving, tumbling attachment over several seconds of real motion
    /// never blows the shell up either - the case the settle-only test above
    /// does not reach.
    #[test]
    fn moving_attachment_stays_stable() {
        let mut rng = Rng::new(2);
        let b = build(&mut rng);
        let wind = Wind::new(3);
        let mut shell = LeafShell::spawn(b, 0.9, attach(V3::ZERO, Quat::IDENTITY));
        let mut t = 0.0_f32;
        for i in 0..2000 {
            t += STEP;
            let pos = v3(t.sin() * 3.0, -t * 0.6, t.cos() * 2.0);
            let orient = Quat::from_axis_angle(v3(0.3, 1.0, 0.2), t * 1.7);
            shell.step(attach(pos, orient), &wind, t, 0.8, 1.2);
            if i % 200 == 0 {
                for p in shell.all_positions() {
                    assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                }
            }
        }
        for p in shell.all_positions() {
            assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
        }
    }

    /// Every pinned (stem) particle exactly tracks the attachment's rigid
    /// transform - it is a kinematic target, not something the solver is
    /// merely encouraged toward.
    #[test]
    fn pinned_particles_exactly_track_the_attachment() {
        let mut rng = Rng::new(4);
        let b = build(&mut rng);
        let wind = Wind::new(5);
        let scale = 0.8;
        let mut shell = LeafShell::spawn(b, scale, attach(V3::ZERO, Quat::IDENTITY));
        let pos = v3(2.0, -1.0, 0.5);
        let orient = Quat::from_axis_angle(v3(0.0, 1.0, 0.0), 0.9);
        let a = attach(pos, orient);
        for i in 0..5 {
            shell.step(a, &wind, i as f32 * STEP, 0.3, 0.2);
        }
        for &i in &shell.pinned.clone() {
            let want = a.to_world(shell.local[i], scale);
            let got = shell.pos(i);
            assert!(got.sub(want).len() < 1e-3, "pinned particle {i}: {got:?} vs {want:?}");
        }
    }

    /// A held-still leaf's blade should be at rest (no residual air force,
    /// nothing but gravity's tiny sag) - not still visibly oscillating deep
    /// into the settle - the "warms up invisibly" promise checked directly.
    #[test]
    fn settled_shell_is_calm_not_still_ringing() {
        let mut rng = Rng::new(6);
        let b = build(&mut rng);
        let a = attach(V3::ZERO, Quat::IDENTITY);
        let mut shell = LeafShell::spawn(b, 0.9, a);
        let before = shell.all_positions();
        let wind = Wind::new(7);
        for i in 0..30 {
            shell.step(a, &wind, i as f32 * STEP, 0.0, 0.0);
        }
        let after = shell.all_positions();
        for (p0, p1) in before.iter().zip(after.iter()) {
            assert!(p1.sub(*p0).len() < 0.05, "still moving well after settle: {p0:?} -> {p1:?}");
        }
    }

    /// A real gust visibly bends the blade relative to its own rest shape -
    /// the whole point of the shell existing: it is not a rigid card that
    /// merely follows the attachment.
    #[test]
    fn a_strong_gust_visibly_bends_the_blade() {
        let mut rng = Rng::new(8);
        let b = build(&mut rng);
        let a = attach(V3::ZERO, Quat::IDENTITY);
        let mut shell = LeafShell::spawn(b, 1.2, a);
        let rest = shell.all_positions();
        let wind = Wind::new(9);
        let mut worst = 0.0_f32;
        for i in 0..400 {
            let t = i as f32 * STEP;
            shell.step(a, &wind, t, 0.0, 1.5);
            let now = shell.all_positions();
            for (p0, p1) in rest.iter().zip(now.iter()) {
                worst = worst.max(p1.sub(*p0).len());
            }
        }
        assert!(worst > 0.01, "the blade never moved relative to its rest shape: worst {worst}");
    }

    /// [`LeafShell::faces`], what [`super::gpu::push_shell`] actually draws,
    /// is exactly [`mesh::indexed`]'s own triangle list for the same build,
    /// not a second triangulation that could drift from it (the physics
    /// body's own collision surface is built from this same list at
    /// [`LeafShell::spawn`], so this is really "the render and the physics
    /// agree", checked at the one seam a future edit to either side could
    /// break silently).
    #[test]
    fn faces_agree_with_mesh_indexed() {
        let mut rng = Rng::new(11);
        let b = build(&mut rng);
        let (_, faces) = mesh::indexed(b.shape, b.cup, b.curl);
        let shell = LeafShell::spawn(b, 1.0, attach(V3::ZERO, Quat::IDENTITY));
        assert_eq!(shell.faces(), faces.as_slice());
    }
}
