//! Card 323: a falling leaf's landing on the ground, physically.
//!
//! The owner, watching card 322 on the panel: "the leaves motion really dies
//! at the bottom of the screen. They wobble around randomly until they get
//! removed." Two real causes, both here before this card: a leaf kept flying
//! its full, unconstrained aerodynamic flight model
//! ([`super::leaf::Leaf3D::step`]) right up to the instant its `y` crossed a
//! *per-depth* line and was instantly frozen there mid-motion (never a real
//! landing, just a snap), and a leaf bumped off the `rest` cap vanished on the
//! very same frame a new one took its place (a pop, not a fade). Neither is
//! what "settles into a quiet layer of leaf litter" asks for.
//!
//! **The handoff.** [`Leaf3D::step`] and [`super::shell::LeafShell::step`] are
//! unchanged and keep driving a leaf right up to the moment it comes within
//! about one leaf-length of [`super::ground_plane_y`] - now a real, single,
//! constant-height world plane rather than a line that used to slide with
//! depth ([`super::LeavesPatch::step_once`]'s own trigger). At that instant
//! this module takes over: the shell's *current*, possibly-flexed shape is
//! baked as a rigid mesh and dropped into a small Rapier **rigid-body** world
//! with a real static ground collider and a real (also static) collider for
//! every leaf already lying down, so it can skid, tip, flip, or come to rest
//! against them through Rapier's own contact solver - not a hand-written
//! snap - carrying the rigid velocity and spin it already had from the flight
//! it was living (so nothing pops at the handoff either).
//!
//! **Why a rigid body, not another soft one.** Each falling leaf's shell
//! already lives in its own isolated `PhysicsWorld` (card 322, deliberately,
//! so two players never corrupt each other's frame) - there is no single
//! shared world two different leaves' soft bodies could contact each other
//! through without inventing one. This card's own text names the way out: "a
//! lying leaf may convert to a rigid thin body (keeping its final flexed
//! shape)". Taken here, for two reasons: a rigid body's contact-and-sleep
//! pipeline is far better-trodden than soft-vs-many-statics would be for a
//! feature this new in the engine (`review/327-ghosts-rapier-cloth.md`'s own
//! "edge cases" caution), and once a leaf is lying still it never needs to
//! flex again - only to have settled into a plausible rigid pose, which is
//! exactly what a convex-hull collider over its own baked shape buys, at a
//! fraction of a soft body's cost.
//!
//! **What ends the settle.** [`Landing::settled`] is true once Rapier's own
//! body goes to sleep (`RigidBody::is_sleeping`, the literal "put resting
//! bodies to sleep" the card asks for), backed by a manual low-motion streak
//! in case a particular shape's inertia sits just outside the default
//! thresholds, and a hard step cap ([`MAX_STEPS`]) as the same non-finite-
//! style safety net every simulation in this codebase keeps. Once settled,
//! the leaf's own little physics world is dropped entirely: a resting leaf
//! becomes a static, baked [`Litter`] - exactly the promise "resting" has
//! made since card 321 (no physics ever runs on it again), so no jitter is
//! even possible from this point on, not merely unlikely.

use super::geom::{v3, Quat, V3};
use super::leaf::{Build, GRAVITY, STEP};
use super::shell::RenderVertex;
use rapier3d::prelude::*;

fn to_rapier(v: V3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}
fn from_rapier(v: Vector) -> V3 {
    v3(v.x, v.y, v.z)
}

/// How much a leaf (and the ground, and every other leaf) grips rather than
/// slides - high, on purpose: a dry leaf's own texture grips more than it
/// slides once it is actually down, which is what turns "skid" into "settle"
/// within a second or two rather than a long, slow slither.
const LEAF_FRICTION: f32 = 0.7;
const GROUND_FRICTION: f32 = 0.85;
/// Leaves do not bounce - zero, exactly: even a whisper of restitution feeds
/// a little energy back into a thin, nearly-flat hull's own contact chatter
/// every step, which is exactly the "wobble around randomly" the owner
/// complained about, just moved from the old per-depth snap into the new
/// physical settle instead of fixed by it (found by rendering the settle
/// itself, not by a test - see the card's Log for the numbers).
const RESTITUTION: f32 = 0.0;
/// Damping beyond the contact solver's own: a real leaf's own air resistance
/// (however still the air is meant to be right at the ground - the card's
/// own "no relative wind under a flat leaf") bleeds a tumble's energy off
/// faster than a bare rigid body would, which is what keeps a settle to a
/// second or two rather than a long, physically-correct but visually fussy
/// skitter.
const LINEAR_DAMPING: f32 = 1.2;
const ANGULAR_DAMPING: f32 = 3.0;

/// Every collider here is a **rounded** convex hull, not a bare one: a leaf
/// is a thin, nearly-flat sliver, and a bare convex hull's sharp, coplanar
/// edge resting on a flat ground is a textbook contact-chatter case (which
/// contact point is "the" one alternates every step or two) - found by
/// rendering the settle itself, not by a test: a dropped leaf's linear
/// velocity died away within half a second, but its angular velocity kept
/// oscillating at a small but non-decaying amplitude indefinitely, never
/// once crossing below [`CALM_ANGVEL`] for [`CALM_STREAK`] steps in a row,
/// however long the safety cap was raised. Rounding every hull by a small
/// border radius is parry's own standard fix for exactly this (a slightly
/// domed edge has one unambiguous contact point, not several tied ones).
const HULL_ROUNDING: f32 = 0.03;

/// Safety cap: ten simulated seconds. Most real landings settle within two
/// or three (checked by rendering and by [`tests::a_plain_drop_settles_
/// above_the_ground`]'s own timing, across a range of leaf shapes); this
/// exists for the rare shape/attitude combination whose contact stays right
/// at a marginal balance for a long time (a leaf dropped dead flat with
/// exactly zero spin onto bare, flat ground is the one synthetic case found
/// by testing that can ride a slow, barely-decaying rock for tens of
/// seconds - real landings always arrive already tumbling, per
/// [`super::leaf::Leaf3D::step`]'s own physics, so land off-balance and tip
/// decisively rather than balance on an edge). Once this cap fires, the leaf
/// is frozen wherever it is - possibly not perfectly still yet in Rapier's
/// own terms, but *frozen from that instant on* regardless (its own physics
/// world is simply dropped, per the module doc), so "no jitter after
/// freezing" still holds exactly, the same non-finite-net role `LeafShell`'s
/// own safety net plays for the soft body.
const MAX_STEPS: u32 = (10.0 / STEP) as u32;
/// Consecutive steps of near-zero motion that count as "settled" even if
/// Rapier's own sleep timer has not fired yet - a second, independent way to
/// reach the same "no jitter" answer, not merely trusting the engine default.
const CALM_STREAK: u32 = 15;
const CALM_LINVEL: f32 = 0.05;
const CALM_ANGVEL: f32 = 0.05;

/// A leaf's shape and rigid motion at the instant it began landing - what
/// [`super::LeavesPatch::step_once`] gathers from a falling leaf and its
/// shell, so this module never reaches back into either type's fields
/// directly.
pub(crate) struct Touchdown {
    pub verts: Vec<RenderVertex>,
    pub faces: Vec<[u32; 3]>,
    pub build: Build,
    pub vel: V3,
    pub omega: V3,
    /// The leaf's own orientation the instant landing began. Not the exact
    /// final rest orientation (this module never converts Rapier's own
    /// rotation back into this crate's [`Quat`] - the baked mesh already
    /// carries the true final shape and pose for rendering, which is what
    /// matters there) - kept only so a gust that plucks this leaf's litter
    /// back into the air ([`super::LeavesPatch::step_once`]'s relaunch path)
    /// resumes from a real orientation rather than an arbitrary one. A leaf
    /// about to be picked back up by a gust and flown across the whole
    /// frame again is not a case where a few degrees of starting tilt reads
    /// as wrong.
    pub orient: Quat,
}

/// A leaf lying on the ground: an immutable, baked snapshot. No physics ever
/// runs on it again (unless a gust plucks it back into the air - see
/// [`Touchdown::orient`]'s doc) - its vertices are already in world space,
/// at whatever pose the settle actually ended at, so drawing it is just
/// drawing a fixed mesh.
pub(crate) struct Litter {
    pub verts: Vec<RenderVertex>,
    pub faces: Vec<[u32; 3]>,
    pub hue: u8,
    /// The baked mesh's own centroid at settle - read for
    /// [`super::depth_fade`] and, if a gust ever plucks this leaf back up,
    /// where it rises from.
    pub pos: V3,
    pub orient: Quat,
    /// This leaf's own build, kept so a gust-plucked relaunch keeps flying
    /// the same leaf (shape, hue, size), not a fresh random one.
    pub build: Build,
}

impl Litter {
    /// A static collider over this leaf's own final shape, for a *later*
    /// landing to stack against - `None` only for a degenerate (near-zero-
    /// volume) mesh, which [`fallback_box`] covers.
    fn hull(&self) -> Option<ColliderBuilder> {
        let pts: Vec<Vector> = self.verts.iter().map(|v| to_rapier(v.pos)).collect();
        ColliderBuilder::round_convex_hull(&pts, HULL_ROUNDING)
    }
}

fn centroid(verts: &[RenderVertex]) -> V3 {
    if verts.is_empty() {
        return V3::ZERO;
    }
    let sum = verts.iter().fold(V3::ZERO, |acc, v| acc.add(v.pos));
    sum.scale(1.0 / verts.len() as f32)
}

/// A thin box spanning the same local bounding box as `local` - the fallback
/// for a mesh too degenerate (near-collinear, zero-thickness) for
/// [`ColliderBuilder::convex_hull`] to accept, so a landing can never simply
/// have no collider at all. Should not happen for a real leaf (cup/curl
/// always give it some real thickness), but a fallback beats a panic.
fn fallback_box(local: &[RenderVertex]) -> ColliderBuilder {
    let mut lo = v3(f32::INFINITY, f32::INFINITY, f32::INFINITY);
    let mut hi = v3(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for v in local {
        lo = v3(lo.x.min(v.pos.x), lo.y.min(v.pos.y), lo.z.min(v.pos.z));
        hi = v3(hi.x.max(v.pos.x), hi.y.max(v.pos.y), hi.z.max(v.pos.z));
    }
    let half = v3(((hi.x - lo.x) * 0.5).max(0.01), ((hi.y - lo.y) * 0.5).max(0.01), ((hi.z - lo.z) * 0.5).max(0.01));
    let centre = lo.add(hi).scale(0.5);
    ColliderBuilder::cuboid(half.x, half.y, half.z).translation(to_rapier(centre))
}

/// A leaf in the middle of landing: a small Rapier rigid-body world (the
/// ground, every already-lying leaf, and this one leaf, all real colliders)
/// being stepped toward a real, physically settled pose.
pub(crate) struct Landing {
    world: PhysicsWorld,
    body: RigidBodyHandle,
    /// This leaf's own mesh, in the rigid body's *local* frame (relative to
    /// the body's own origin, with the body's initial rotation identity - so
    /// the body's rotation from here on is exactly "how far this already-
    /// oriented mesh has additionally turned since landing began"). Never
    /// recomputed once landing begins: the module doc's "keeping its final
    /// flexed shape".
    local: Vec<RenderVertex>,
    faces: Vec<[u32; 3]>,
    build: Build,
    trigger_orient: Quat,
    steps: u32,
    calm: u32,
}

impl Landing {
    /// Start a real physical settle for a leaf whose shell has just reached
    /// [`super::LeavesPatch::step_once`]'s landing trigger. `litter` is every
    /// leaf already lying down (this settle's own snapshot of the floor -
    /// see the module doc's honest limitation about a landing that starts
    /// slightly later while this one is still settling): each becomes a
    /// static collider of its own, so this leaf can physically stack on top
    /// of, not through, them.
    pub(crate) fn begin<'a>(touchdown: Touchdown, litter: impl Iterator<Item = &'a Litter>) -> Landing {
        let origin = centroid(&touchdown.verts);
        let local: Vec<RenderVertex> = touchdown
            .verts
            .iter()
            .map(|v| RenderVertex { pos: v.pos.sub(origin), normal: v.normal, fold: v.fold })
            .collect();

        let mut world = PhysicsWorld::new();
        world.gravity = Vector::new(0.0, -GRAVITY, 0.0);
        world.integration_parameters.dt = STEP;

        // The ground: one large static slab, its top at the real world
        // ground plane, big enough to sit under every leaf this patch could
        // ever land (the fall range plus a margin) regardless of where this
        // particular one touches down.
        let gy = super::ground_plane_y();
        let half_x = super::half_width(super::Z_FAR) + 5.0;
        let half_z = (super::Z_FAR - super::Z_NEAR) * 0.5 + 5.0;
        let cz = (super::Z_NEAR + super::Z_FAR) * 0.5;
        world.insert(
            RigidBodyBuilder::fixed(),
            ColliderBuilder::cuboid(half_x, 0.1, half_z)
                .translation(Vector::new(0.0, gy - 0.1, cz))
                .friction(GROUND_FRICTION)
                .restitution(0.0),
        );

        // Every leaf already lying down: a static collider over its own
        // final shape, so this leaf can rest on top of one rather than
        // sinking into or through it.
        for l in litter {
            let hull = l.hull().unwrap_or_else(|| fallback_box(&l.verts));
            world.insert(RigidBodyBuilder::fixed(), hull.friction(LEAF_FRICTION).restitution(0.0));
        }

        let local_points: Vec<Vector> = local.iter().map(|v| to_rapier(v.pos)).collect();
        let collider = ColliderBuilder::round_convex_hull(&local_points, HULL_ROUNDING).unwrap_or_else(|| fallback_box(&local));
        let body_builder = RigidBodyBuilder::dynamic()
            .translation(to_rapier(origin))
            .linvel(to_rapier(touchdown.vel))
            .angvel(to_rapier(touchdown.omega))
            .linear_damping(LINEAR_DAMPING)
            .angular_damping(ANGULAR_DAMPING);
        let (body, _) = world.insert(body_builder, collider.friction(LEAF_FRICTION).restitution(RESTITUTION));

        Landing { world, body, local, faces: touchdown.faces, build: touchdown.build, trigger_orient: touchdown.orient, steps: 0, calm: 0 }
    }

    /// One fixed physics step - kept in lockstep with every other step in
    /// this patch ([`super::leaf::STEP`]). A no-op once [`Self::settled`]
    /// would already be true from the step cap, so a caller that steps one
    /// extra time before checking never overruns it.
    pub(crate) fn step(&mut self) {
        if self.steps >= MAX_STEPS {
            return;
        }
        self.world.step();
        self.steps += 1;
        let body = &self.world.bodies[self.body];
        let (lv, av) = (from_rapier(body.linvel()).len(), from_rapier(body.angvel()).len());
        if lv < CALM_LINVEL && av < CALM_ANGVEL {
            self.calm += 1;
        } else {
            self.calm = 0;
        }
    }

    /// True once this leaf has truly come to rest: Rapier's own sleep (the
    /// card's "put resting bodies to sleep"), or this module's own
    /// low-motion streak, or the safety cap - whichever comes first.
    pub(crate) fn settled(&self) -> bool {
        self.world.bodies[self.body].is_sleeping() || self.calm >= CALM_STREAK || self.steps >= MAX_STEPS
    }

    /// This leaf's current world-space mesh, at whatever pose the settle has
    /// reached so far - what a still-landing leaf is drawn with every frame
    /// (`gpu::push_baked`), exactly as a falling leaf's shell is drawn from
    /// its own live state.
    pub(crate) fn render_vertices(&self) -> Vec<RenderVertex> {
        let pose = self.world.bodies[self.body].position();
        self.local
            .iter()
            .map(|v| RenderVertex {
                pos: from_rapier(pose.transform_point(to_rapier(v.pos))),
                normal: from_rapier(pose.transform_vector(to_rapier(v.normal))),
                fold: v.fold,
            })
            .collect()
    }

    pub(crate) fn faces(&self) -> &[[u32; 3]] {
        &self.faces
    }

    pub(crate) fn hue(&self) -> u8 {
        self.build.hue
    }

    /// This leaf's current depth (world `z`), for [`super::depth_fade`] -
    /// the body's own current translation, not the trigger-time one.
    pub(crate) fn depth(&self) -> f32 {
        let pose = self.world.bodies[self.body].position();
        from_rapier(pose.transform_point(Vector::ZERO)).z
    }

    /// Freeze this leaf: its current mesh becomes a [`Litter`], and this
    /// `Landing`'s own physics world is dropped with it - from here on
    /// nothing ever steps this leaf again.
    pub(crate) fn bake(&self) -> Litter {
        let verts = self.render_vertices();
        let pos = centroid(&verts);
        Litter { verts, faces: self.faces.clone(), hue: self.build.hue, pos, orient: self.trigger_orient, build: self.build }
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
            shape: (rng.f32() * super::super::mesh::SHAPES as f32) as u8,
            hue: 0,
            cup: rng.range(0.08, 0.30),
            curl: rng.range(-0.22, 0.30),
            spin_bias: 0.0,
        }
    }

    /// A leaf's own mesh, in world space, dropped from a small height above
    /// the ground plane, falling straight down with no spin - the plainest
    /// possible landing.
    /// `vel`/`omega` a small, non-zero tilt and spin, roughly matching a real
    /// touchdown (see the test's own doc: a leaf arriving perfectly flat with
    /// exactly zero spin is the one synthetic case, not a real one, that can
    /// balance on an edge for a long time - [`MAX_STEPS`]'s own doc).
    fn touchdown_over(rng: &mut Rng, build: Build, y: f32) -> Touchdown {
        let scale = 0.9 * build.size;
        let (local, faces) = super::super::mesh::indexed(build.shape, build.cup, build.curl);
        let verts: Vec<RenderVertex> = local
            .iter()
            .map(|&p| RenderVertex { pos: v3(0.0, y, 6.0).add(p.scale(scale)), normal: v3(0.0, 1.0, 0.0), fold: 0.0 })
            .collect();
        let vel = v3(rng.range(-0.3, 0.3), -1.2, rng.range(-0.3, 0.3));
        let omega = v3(rng.range(-1.5, 1.5), rng.range(-1.5, 1.5), rng.range(-1.5, 1.5));
        Touchdown { verts, faces, build, vel, omega, orient: Quat::IDENTITY }
    }

    /// A leaf dropped from just above the ground, with no litter to stack
    /// on, always ends up with every final vertex at or above the ground
    /// plane - "no leaf below the ground", checked directly against the real
    /// collider rather than assumed from adding one - whether it truly went
    /// calm or was frozen by the safety cap (either way it is frozen, and
    /// [`MAX_STEPS`]'s own doc is why the cap alone is not a bug). Real
    /// (non-zero) touchdown velocity/spin, per [`touchdown_over`]'s doc,
    /// checked separately below to also finish well inside the cap.
    #[test]
    fn a_plain_drop_settles_above_the_ground() {
        let mut rng = Rng::new(1);
        for _ in 0..12 {
            let b = build(&mut rng);
            let touchdown = touchdown_over(&mut rng, b, super::super::ground_plane_y() + 0.3);
            let mut landing = Landing::begin(touchdown, std::iter::empty());
            let mut steps = 0;
            while !landing.settled() && steps < MAX_STEPS {
                landing.step();
                steps += 1;
            }
            let litter = landing.bake();
            let worst = litter.verts.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min);
            assert!(worst > super::super::ground_plane_y() - 0.05, "a vertex sank to {worst}, ground is at {}", super::super::ground_plane_y());
        }
    }

    /// The same drop, but timed: a *real* touchdown (non-zero velocity and
    /// spin, as every leaf actually arrives with - `Leaf3D::step` never hands
    /// off a leaf that is perfectly still) settles well inside the safety
    /// cap, not merely "eventually, or the cap papers over it" - the
    /// distinction [`MAX_STEPS`]'s own doc draws between a real landing and
    /// the one synthetic edge case that needs the cap at all.
    #[test]
    fn a_real_touchdown_settles_well_within_the_safety_cap() {
        let mut rng = Rng::new(21);
        for _ in 0..12 {
            let b = build(&mut rng);
            let touchdown = touchdown_over(&mut rng, b, super::super::ground_plane_y() + 0.3);
            let mut landing = Landing::begin(touchdown, std::iter::empty());
            let mut steps = 0;
            while !landing.settled() && steps < MAX_STEPS {
                landing.step();
                steps += 1;
            }
            assert!(steps < MAX_STEPS / 2, "took {steps} steps ({:.1}s) - not well within the cap", steps as f32 * STEP);
        }
    }

    /// Once baked, a litter's own vertices genuinely stay put: nothing in
    /// this module keeps stepping it (there is no world left to step - the
    /// `Landing` that produced it is simply dropped), so calling `bake`
    /// again is not even possible; this checks the one thing that *could*
    /// silently move it instead - reading `render_vertices` twice on the
    /// same still-settling `Landing` without stepping between reads gives
    /// the same answer twice, i.e. reading never advances the sim.
    #[test]
    fn reading_a_landing_twice_without_stepping_does_not_move_it() {
        let mut rng = Rng::new(2);
        let b = build(&mut rng);
        let touchdown = touchdown_over(&mut rng, b, super::super::ground_plane_y() + 1.0);
        let landing = Landing::begin(touchdown, std::iter::empty());
        let a = landing.render_vertices();
        let b = landing.render_vertices();
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.pos, y.pos);
        }
    }

    /// A leaf dropped onto another leaf's own litter comes to rest on top of
    /// it, not through it - "overlapping leaves stack (not interpenetrate)".
    #[test]
    fn a_leaf_dropped_onto_litter_stacks_rather_than_sinking_through() {
        let mut rng = Rng::new(3);
        let base_build = build(&mut rng);
        // A first leaf, settled flat on the bare ground, becomes the litter
        // the second one lands on.
        let base_touchdown = touchdown_over(&mut rng, base_build, super::super::ground_plane_y() + 0.2);
        let mut base = Landing::begin(base_touchdown, std::iter::empty());
        for _ in 0..(MAX_STEPS as usize) {
            if base.settled() {
                break;
            }
            base.step();
        }
        let base_litter = base.bake();
        let base_top = base_litter.verts.iter().map(|v| v.pos.y).fold(f32::NEG_INFINITY, f32::max);

        // A second leaf, dropped from well above that one, straight down.
        let top_build = build(&mut rng);
        let top_touchdown = touchdown_over(&mut rng, top_build, base_top + 1.5);
        let mut top = Landing::begin(top_touchdown, std::iter::once(&base_litter));
        let mut steps = 0;
        while !top.settled() && steps < MAX_STEPS {
            top.step();
            steps += 1;
        }
        let top_litter = top.bake();
        let top_bottom = top_litter.verts.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min);
        assert!(
            top_bottom > base_litter.pos.y.min(base_top) - 0.15,
            "the second leaf's lowest point ({top_bottom}) sank well below the first leaf's own litter (top {base_top})"
        );
    }
}
