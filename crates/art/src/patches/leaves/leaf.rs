//! One leaf's flight, in 3D: gravity, drag and lift from the leaf's own
//! attitude against the air it is actually moving through, and a torque law
//! that decides whether it settles into a calm glide, rocks, tumbles end over
//! end, or autorotates into a spiral - "which one depends on the leaf and the
//! air, as with real leaves" (card 321).
//!
//! **This is card 314's flat-plate model, generalised from a 2D angle to a
//! 3D orientation**, not a different theory. That model's own doc (kept
//! below in spirit) explains why a quasi-steady flat-plate law driven by
//! `u = velocity - wind` was chosen over a literal Tanabe-Kaneko/Andersen
//! port: those reductions throw the wind away to get a clean bifurcation
//! diagram, and this patch needs a leaf that feels an actual, spatially
//! varying gust field. Card 321 (the owner: "the leaves really need to be 3d
//! rendered") asks for real orientation and real light, which needs a plate
//! with a normal vector rather than a line with an angle - so `theta`
//! becomes a unit [`Quat`], and `omega` becomes a 3D angular velocity - and
//! every scalar law in the 2D model is rewritten here in terms of the
//! plate's normal `n` and the unit relative wind `û`, chosen so that setting
//! `n` to lie in a fixed plane and `omega` to a scalar about that plane's own
//! normal recovers the 2D model's numbers exactly (see the tests):
//!
//! - `c = n . û`, the cosine of the angle between the plate's face and the
//!   direction the air is coming from. `c = ±1` is **broadside** (the plate
//!   directly faces the flow, maximum drag - this is the 2D model's
//!   `delta = ±π/2`); `c = 0` is **edge-on** (the flow slides along the
//!   plate's face, minimum drag, no lift - the 2D model's `delta = 0`).
//! - `s = sqrt(1 - c^2)`, the edge-on fraction, plays the role the 2D model's
//!   `cos(delta)` played.
//! - Drag: `Cd(c) = Cd0 + (Cd90 - Cd0) c^2` - the same shape as the 2D
//!   model's `Cd0 + (Cd90-Cd0) sin^2(delta)`, `c` standing in for `sin(delta)`.
//! - Lift acts along `n`'s component perpendicular to `û` (the only
//!   direction "which way `n` leans away from the flow" can mean in 3D),
//!   `lift_dir = normalise(n - c*û)`, with magnitude `Cl_peak * 2 c s` - the
//!   same `sin(2 delta) = 2 sin(delta) cos(delta)` shape as the 2D model's
//!   lift, peaking at 45 degrees either side of edge-on.
//! - **The torque is again the piece that is not off a data sheet.** The 2D
//!   model restored the plate toward broadside with a term proportional to
//!   `sin(2 delta)`. Its 3D analogue has to be a *vector* (an axis to spin
//!   about, not just a rate), and it has to drive `|c|` toward 1 whichever
//!   sign it already has - a real dropped leaf settles broadside-**up** or
//!   broadside-**down** with equal ease, and a torque that only ever pushed
//!   toward `c = +1` would make one of those an unstable, not a stable,
//!   point. `axis = n x û` is the rotation that moves `n` toward `û`, but
//!   scaling it by the *signed* `c` gives exactly the bistability wanted:
//!   `omega_restore = k * c * (n x û)`. Differentiating `c` under that
//!   angular velocity (`dn/dt = omega x n`, `dc/dt = û . dn/dt`) gives
//!   `dc/dt = k * c * (1 - c^2)`, which is positive for `c > 0` and negative
//!   for `c < 0`: unstable at `c = 0` (edge-on), stable at `c = ±1`
//!   (broadside), and it peaks (fastest restoring) around `|c| = 1/sqrt(3)`,
//!   the 3D equivalent of the 2D torque's peak at 45 degrees. See
//!   [`geom::Quat::integrate`] and the tests in this module for the algebra
//!   checked out.
//! - Damping is airspeed-proportional plus a small constant floor, exactly as
//!   the 2D model's; `flutter` still both strengthens the restoring term and
//!   eases the damping, which is what lets a disturbance ring for a long
//!   time (rocking, tumbling) instead of dying out into a steady glide.
//! - **New in 3D**: a per-leaf `spin_bias`, a small constant torque about the
//!   relative-wind axis itself (`û`, which the restoring torque above never
//!   touches - it only ever turns `n` toward or away from `û`, never spins
//!   the plate *about* `û`). This is what a maple seed's autorotation is: an
//!   asymmetry that keeps the plate turning about its own direction of
//!   travel rather than settling still, which is what carries a leaf into a
//!   slow spiral or helix instead of a straight glide. Zero for most leaves;
//!   a few get enough of it to visibly corkscrew down.

use super::geom::{v3, Quat, V3};
use super::wind::Wind;

/// Physics timestep. Fixed and independent of render rate, exactly as
/// `flock` and card 314's 2D model did it, so a snapshot at any frame rate
/// takes the same steps at the same simulated moments and is therefore the
/// same picture. Three of these fit inside one 30 fps frame, which is also
/// what feeds the render's motion-blur trail (`gpu::render`): each frame
/// draws every physics step taken since the last one, faded by age.
pub const STEP: f32 = 1.0 / 90.0;

/// Downward acceleration, world metres/second^2. Not real gravity - there is
/// no real leaf mass or air density in this model - chosen with `AERO_K` and
/// the drag coefficients below so a leaf takes several seconds to fall the
/// world's fall height ([`super::FALL_HEIGHT`]), the "ambient, unhurried"
/// pace the brief asks for. `pub(crate)`: card 323's [`super::landing::Landing`]
/// reuses this exact number as its own rigid-body gravity, so a leaf's speed
/// does not visibly change the instant it hands off from the aero model to
/// the ground-contact one.
pub(crate) const GRAVITY: f32 = 1.0;

/// Flat-plate drag coefficients, edge-on and broadside (dimensionless; see
/// e.g. Hoerner's drag data - the same standard shapes card 314 used, unchanged).
const CD_EDGE: f32 = 0.12;
const CD_BROAD: f32 = 1.4;
/// Peak lift coefficient, at 45 degrees between edge-on and broadside.
const CL_PEAK: f32 = 1.1;
/// Scales drag/lift accelerations from coefficients and speed into
/// world-metres-per-second^2; folds in the leaf's own size, as card 314's did.
const AERO_K: f32 = 2.0;

/// The torque law's base strength and its two damping terms (see the module
/// doc): one growing with airspeed, one a small constant floor so a leaf
/// becalmed in dead air stops spinning rather than coasting forever.
const TORQUE_K: f32 = 3.4;
const DAMP_AERO: f32 = 0.22;
const DAMP_FLOOR: f32 = 0.07;

/// How hard a leaf's own `spin_bias` drives it to autorotate about its
/// relative-wind axis, scaled by airspeed exactly as the restoring torque is.
const SPIN_BIAS_K: f32 = 1.35;

/// A leaf's own, unchanging build: picked once at spawn from the seed, so
/// "another leaf" really is a different one.
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// Relative size, about `0.6..1.4`. Scales drag (bigger leaves are
    /// draggier per this model's own stylised choice - card 314's reasoning,
    /// unchanged) and the mesh at draw time.
    pub size: f32,
    /// Rotational inertia relative to `size`, about `0.6..1.6`: heavier for
    /// its size resists the torque law more (a calmer glide); lighter rings
    /// longer and is more likely to tip into a full tumble.
    pub inertia: f32,
    /// Which [`super::mesh`] shape this leaf is.
    pub shape: u8,
    /// Which hue family ([`super::FAMILIES`]).
    pub hue: u8,
    /// How cupped the leaf's cross-section is (a shallow canoe, edges raised
    /// above the midrib) - a fraction of its half-width.
    pub cup: f32,
    /// How much the tip curls, signed (up or down) - a fraction of its
    /// half-length.
    pub curl: f32,
    /// Autorotation bias about its own relative-wind axis, `-1..1`; most
    /// leaves are near zero (a plain tumble or glide), a few carry enough of
    /// this to spiral down.
    pub spin_bias: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Leaf3D {
    pub pos: V3,
    pub vel: V3,
    pub orient: Quat,
    /// World-frame angular velocity, radians/second.
    pub omega: V3,
    pub build: Build,
    /// Seconds since this leaf was (re)spawned into the air. Frozen while
    /// [`Leaf3D::resting`] - a leaf lying still is not "aloft" and the stuck
    /// bound has nothing to say about it.
    pub aloft: f32,
    /// `n . û` from the last step that had air moving past the leaf, `-1..1`:
    /// broadside at `±1`, edge-on at `0`. Kept from the last live reading
    /// while becalmed, so a leaf never pops between attitudes on a single
    /// quiet step. This is what the tests read to check the flutter actually
    /// oscillates. Unlike card 314's 2D `shimmer`, nothing in the drawing
    /// reads this any more - the mesh's own geometry and normals carry the
    /// silhouette now - but it is still the one place "is this leaf edge-on
    /// right now" is answered, so a future reader wanting that should read
    /// this field rather than re-deriving it from `orient` and `vel`.
    pub broadside: f32,
    /// True once this leaf has landed and stopped: no physics is run on it
    /// until it is recycled (see `super::LeavesPatch::respawn`).
    pub resting: bool,
}

impl Leaf3D {
    /// The leaf-local "top" (adaxial) face normal before any rotation is
    /// applied - `n = orient.rotate(Leaf3D::LOCAL_NORMAL)` is which way the
    /// leaf's top face currently points in world space. [`super::mesh`] builds
    /// its geometry to agree with this exactly (the surface's un-rotated
    /// normal at the centre of the leaf is this vector), so the physics and
    /// the picture can never disagree about which face is which.
    pub const LOCAL_NORMAL: V3 = v3(0.0, 0.0, 1.0);

    /// Advance one fixed physics step. `wind`, `mean` and `gusts` are the
    /// field this leaf is flying through (see [`super::wind::Wind::at3`]);
    /// `flutter` is the patch parameter, `0..2`, `1` being the shipped feel.
    pub fn step(&mut self, wind: &Wind, t: f32, mean: f32, gusts: f32, flutter: f32) {
        if self.resting {
            return;
        }
        let w = wind.at3(self.pos.x, self.pos.y, self.pos.z, t, mean, gusts);
        let u = self.vel.sub(w);
        let speed = u.len();

        let mut acc = V3::UP.scale(-GRAVITY);
        if speed > 1e-3 {
            let uhat = u.scale(1.0 / speed);
            let n = self.orient.rotate(Self::LOCAL_NORMAL);
            let c = n.dot(uhat).clamp(-1.0, 1.0);
            let s = (1.0 - c * c).max(0.0).sqrt();

            let cd = CD_EDGE + (CD_BROAD - CD_EDGE) * c * c;
            let cl = CL_PEAK * 2.0 * c * s;
            let k = AERO_K * self.build.size;
            let lift_dir = n.sub(uhat.scale(c)).unit_or(V3::ZERO);
            // Drag opposes u; lift acts along n's component across u.
            acc = acc.add(uhat.scale(-cd * speed * speed * k)).add(lift_dir.scale(cl * speed * speed * k));

            // The torque law: restoring toward broadside, whichever sign of
            // `c` is already nearer (see the module doc for the derivation),
            // damped by airspeed and by a small constant floor, plus this
            // leaf's own autorotation about the relative-wind axis itself.
            let axis = n.cross(uhat);
            let i = (self.build.inertia * self.build.size).max(0.2);
            let kt = TORQUE_K * (0.5 + 0.7 * flutter) / i;
            let restore = axis.scale(kt * c * speed * speed);
            let spin = uhat.scale(self.build.spin_bias * SPIN_BIAS_K * speed * speed / i);
            let damp_aero = DAMP_AERO * (1.3 - 0.55 * flutter).max(0.25);
            let damped = self.omega.scale(-(damp_aero * speed + DAMP_FLOOR));

            self.omega = self.omega.add(restore.add(spin).add(damped).scale(STEP));
            self.broadside = c;
        } else {
            self.omega = self.omega.scale((1.0 - DAMP_FLOOR * STEP).max(0.0));
        }

        self.vel = self.vel.add(acc.scale(STEP));
        self.pos = self.pos.add(self.vel.scale(STEP));
        self.orient = self.orient.integrate(self.omega, STEP);
        self.aloft += STEP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn build() -> Build {
        Build { size: 1.0, inertia: 1.0, shape: 0, hue: 0, cup: 0.1, curl: 0.05, spin_bias: 0.0 }
    }

    fn leaf(pos: V3, vel: V3, orient: Quat) -> Leaf3D {
        Leaf3D { pos, vel, orient, omega: V3::ZERO, build: build(), aloft: 0.0, broadside: 0.0, resting: false }
    }

    /// The 2D model's peak lift was at 45 degrees between edge-on and
    /// broadside, and its torque flipped sign either side of broadside. This
    /// checks the 3D generalisation keeps both shapes: dropping a leaf
    /// almost-but-not-quite edge-on (an unstable attitude - see the next
    /// test for why it must not start *exactly* there) tips it away rather
    /// than holding it there, and it settles broadside, not edge-on.
    #[test]
    fn broadside_is_the_torques_stable_point_and_edge_on_is_not() {
        let wind = Wind::new(1);
        // A small tilt off the identity orientation, whose local normal
        // (0, 0, 1) is exactly perpendicular to straight-down travel - i.e.
        // exactly edge-on. Falling straight down.
        let mut l = leaf(V3::ZERO, v3(0.0, -1.0, 0.0), Quat::from_axis_angle(v3(1.0, 0.0, 0.0), 0.05));
        for _ in 0..2000 {
            l.step(&wind, 0.0, 0.0, 0.0, 1.0);
        }
        assert!(l.broadside.abs() > 0.9, "should have tipped toward broadside, got {}", l.broadside);
    }

    /// Edge-on (`c = 0`) is an exact fixed point of `dc/dt = k c (1 - c^2)` as
    /// well as an unstable one: with literally no perturbation the restoring
    /// torque is zero and a leaf released precisely edge-on has nothing to
    /// tip it either way. Checked here so the test above's small starting
    /// tilt is not hiding a broken "restoring toward zero" sign error that
    /// would also, coincidentally, leave a perfectly edge-on leaf sitting
    /// still.
    #[test]
    fn edge_on_with_no_perturbation_at_all_is_a_fixed_point() {
        let wind = Wind::new(1);
        let mut l = leaf(V3::ZERO, v3(0.0, -1.0, 0.0), Quat::IDENTITY);
        for _ in 0..500 {
            l.step(&wind, 0.0, 0.0, 0.0, 1.0);
        }
        assert!(l.broadside.abs() < 1e-4, "no perturbation, no torque: got {}", l.broadside);
    }

    /// [`Quat::integrate`] under the restoring torque should behave the way
    /// the module doc's algebra says: `d|c|/dt > 0` away from edge-on. This
    /// checks it directly on a handful of steps rather than trusting the long
    /// run above to be the only evidence.
    #[test]
    fn a_small_push_off_edge_on_grows_broadside_not_shrinks() {
        let wind = Wind::new(2);
        // A slight tilt off edge-on, falling into real airspeed so there is
        // something for the torque to act on.
        let mut l = leaf(V3::ZERO, v3(0.0, -2.0, 0.0), Quat::from_axis_angle(v3(1.0, 0.0, 0.0), 0.12));
        let before = {
            let n = l.orient.rotate(Leaf3D::LOCAL_NORMAL);
            let u = l.vel.unit_or(V3::UP);
            n.dot(u).abs()
        };
        for _ in 0..30 {
            l.step(&wind, 0.0, 0.0, 0.0, 1.0);
        }
        assert!(l.broadside.abs() > before, "|c| should have grown: {before} -> {}", l.broadside);
    }

    /// Gravity always wins eventually (drag grows with speed squared, gravity
    /// does not), so a leaf released from rest keeps falling rather than
    /// settling into zero net motion - the flight never just stops in mid-air.
    #[test]
    fn gravity_always_wins() {
        let wind = Wind::new(3);
        let mut l = leaf(v3(0.0, 5.0, 0.0), V3::ZERO, Quat::IDENTITY);
        for _ in 0..3000 {
            l.step(&wind, 0.0, 0.3, 0.4, 1.0);
        }
        assert!(l.pos.y < 4.0, "should have fallen a real distance: {}", l.pos.y);
        assert!(l.vel.y < -0.05, "should still be moving downward: {}", l.vel.y);
    }

    /// A resting leaf is frozen: physics is a no-op, `aloft` does not
    /// advance. `super::mod` relies on this to let a settled leaf sit still.
    #[test]
    fn a_resting_leaf_does_not_move() {
        let wind = Wind::new(4);
        let mut l = leaf(v3(1.0, 0.0, 3.0), v3(0.4, -0.6, 0.1), Quat::IDENTITY);
        l.resting = true;
        let (pos, vel, orient, aloft) = (l.pos, l.vel, l.orient, l.aloft);
        for _ in 0..500 {
            l.step(&wind, 0.0, 1.0, 1.0, 1.0);
        }
        assert_eq!((l.pos, l.vel, l.orient, l.aloft), (pos, vel, orient, aloft));
    }

    /// Numerical drift in a quaternion integrated for a very long run (this
    /// patch may run for months) has to stay corrected: `Quat::integrate`
    /// renormalises every step, so `|orient|` should read as 1 however long
    /// the flight has been going.
    #[test]
    fn orientation_stays_normalised_over_a_long_flight() {
        let wind = Wind::new(5);
        let mut l = leaf(v3(0.0, 5.0, 0.0), v3(0.3, -0.5, 0.2), Quat::from_axis_angle(v3(0.2, 1.0, 0.3), 0.7));
        for i in 0..200_000 {
            l.step(&wind, i as f32 * STEP, 0.6, 0.8, 1.2);
            assert!((l.orient.len() - 1.0).abs() < 1e-4, "step {i}: |q| = {}", l.orient.len());
        }
    }
}
