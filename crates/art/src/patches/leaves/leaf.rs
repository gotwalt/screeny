//! One leaf's flight: a simplified 2D flat-plate model, in the spirit of the
//! falling-card literature (Tanabe & Kaneko 1994; Andersen, Pesavento & Wang
//! 2005; Belmonte, Eisenberg & Moses 1998) rather than a literal
//! implementation of any one of them.
//!
//! **Why this model and not Tanabe-Kaneko's own equations.** Their reduced
//! system is a beautiful two-parameter bifurcation (steady fall / flutter /
//! tumbling / chaotic), but it is non-dimensionalised against the plate's own
//! terminal velocity and has no separate translational wind term - the whole
//! point of their reduction is to throw the wind away. This patch needs a
//! leaf that feels an actual, spatially varying gust field, so the model
//! works the other way round: quasi-steady lift and drag are computed from
//! the leaf's velocity *relative to the moving air* (a plain vector
//! subtraction), and the flutter/glide/tumble behaviour falls out as a
//! side effect of one torque law rather than being the thing solved for.
//!
//! **The physics, per leaf:**
//! - State: position, velocity, `theta` (the plate's orientation, a line in
//!   the screen plane) and `omega` (its spin).
//! - `u = velocity - wind`: the leaf's motion through the air it is actually
//!   in, not through still air.
//! - `delta = theta - direction(u)`, folded into `(-pi/2, pi/2]` because a
//!   flat plate looks the same rotated by a half turn. `delta = 0` is the
//!   plate edge-on to the air passing it (least drag, no lift); `delta =
//!   +-pi/2` is broadside to it (most drag, no lift; lift peaks in between,
//!   at 45 degrees, and changes sign either side of edge-on).
//! - Drag and lift are the standard flat-plate forms, `Cd(delta) = Cd0 +
//!   (Cd90-Cd0) sin^2(delta)` and `Cl(delta) = Cl_max sin(2 delta)`, applied
//!   opposing and perpendicular to `u`.
//! - **The torque is the one piece that is not off a data sheet**: `sin(2
//!   delta) * speed^2`, which is a restoring torque around *broadside* (its
//!   sign flips either side of `delta = +-pi/2`, not around edge-on). Broadside
//!   is where sustained falling flat plates actually sit on average - it is
//!   why a dropped leaf tends to end up lying flat rather than knife-edge to
//!   its own fall - so this is the torque's stable point, not an arbitrary
//!   choice. Whether the leaf settles quietly there, rocks back and forth
//!   through it, or has enough spin to carry it all the way round through
//!   edge-on and out the other side, is decided by how much rotational
//!   damping opposes that one restoring term - which is exactly what
//!   `flutter` turns: low damping lets an initial disturbance ring for a
//!   long time (rocking, occasionally enough to tip into a full turn -
//!   tumbling); higher damping kills the ring quickly and the leaf glides
//!   nose-into-the-relative-wind at a shallow, steady tilt instead. One
//!   torque law, three looks, because that is what a real falling plate does.

use super::wind::Wind;
use std::f32::consts::{FRAC_PI_2, PI};

/// Physics timestep. Fixed and independent of render rate (as `flock` does
/// it with `STEP`), so a snapshot at any frame rate takes the same steps at
/// the same simulated moments and is therefore the same picture.
pub const STEP: f32 = 1.0 / 90.0;

/// Downward acceleration, panel rows per second squared. Not SI gravity -
/// there is no real leaf mass or air density here - chosen so a leaf crosses
/// the panel's 32 rows in a handful of seconds once drag limits its fall,
/// which is the "ambient, unhurried" pace the brief asks for.
const GRAVITY: f32 = 8.0;

/// Flat-plate drag coefficients, edge-on and broadside (dimensionless,
/// standard shapes for a thin plate; see e.g. Hoerner's drag data).
const CD_EDGE: f32 = 0.10;
const CD_BROAD: f32 = 1.9;
/// Peak lift coefficient, at 45 degrees angle of attack.
const CL_PEAK: f32 = 1.6;
/// Scales drag/lift accelerations from coefficients and speed into
/// panel-rows-per-second^2; folds in the leaf's own size (bigger leaf, more
/// drag per unit mass at this stylised scale - real physics is the other way
/// for a fixed thickness, but the sizes here are a visual choice, not a
/// species).
const AERO_K: f32 = 0.26;

/// The torque law's base strength and the two damping terms (see the module
/// doc): one that grows with airspeed (a plate spinning through fast air is
/// braked harder) and a small constant floor so a leaf becalmed in dead air
/// still stops spinning rather than coasting forever.
const TORQUE_K: f32 = 3.4;
const DAMP_AERO: f32 = 0.15;
const DAMP_FLOOR: f32 = 0.08;

/// A leaf's own, unchanging build: how big it is drawn and how it responds to
/// the torque law, both picked once at spawn from the seed so "another leaf"
/// really is a different one, not a relabelled twin.
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// Relative size, about `0.75..1.3`. Scales drag (see `AERO_K` above:
    /// bigger leaves are draggier per the model's own stylised choice, so
    /// they fall a little slower and flutter a little more readily - smaller
    /// leaves punch through gusts and glide straighter, which is the
    /// silhouette the brief wants ("broadside 3-4 LEDs, edge-on 1-2" scales
    /// with this).
    pub size: f32,
    /// Rotational inertia relative to `size`, about `0.6..1.6`: at the same
    /// size, a "heavier for its size" leaf resists the torque law more, so
    /// it settles into a calmer glide; a "lighter" one rings longer and is
    /// more likely to tip into a full tumble.
    pub inertia: f32,
    pub hue: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub pos: (f32, f32),
    pub vel: (f32, f32),
    pub theta: f32,
    pub omega: f32,
    pub build: Build,
    /// Seconds since this leaf was (re)spawned. Drives the "darkens a touch
    /// as it falls" rule and the stuck-leaf bound.
    pub aloft: f32,
    /// `|sin(delta)|` from the last step that had air moving past the leaf:
    /// `0` edge-on to its own relative motion, `1` broadside. Held from the
    /// last live reading while the leaf is briefly becalmed, rather than
    /// snapped to some default, so a leaf never visibly pops between sizes
    /// on a single quiet step.
    pub shimmer: f32,
}

/// Fold `a` into `(-pi/2, pi/2]`: the plate reads the same rotated by a half
/// turn, and every angle law above is written in terms of that folded angle.
fn fold_half_pi(mut a: f32) -> f32 {
    while a > FRAC_PI_2 {
        a -= PI;
    }
    while a <= -FRAC_PI_2 {
        a += PI;
    }
    a
}

impl Leaf {
    /// Advance one fixed physics step. `wind`, `mean` and `gusts` are the
    /// field this leaf is flying through (see [`super::wind::Wind`]);
    /// `flutter` is the patch parameter, `0..2`, `1` being the shipped feel.
    pub fn step(&mut self, wind: &Wind, t: f32, mean: f32, gusts: f32, flutter: f32) {
        let (wx, wy) = wind.at(self.pos.0, self.pos.1, t, mean, gusts);
        let (ux, uy) = (self.vel.0 - wx, self.vel.1 - wy);
        let speed = (ux * ux + uy * uy).sqrt();

        let mut ay = GRAVITY;
        let mut ax = 0.0;
        if speed > 1e-3 {
            let phi = uy.atan2(ux);
            let delta = fold_half_pi(self.theta - phi);
            let (sd, cd2) = (delta.sin(), (2.0 * delta).sin());
            let cd = CD_EDGE + (CD_BROAD - CD_EDGE) * sd * sd;
            let cl = CL_PEAK * cd2;
            let k = AERO_K * self.build.size;
            // Drag opposes u; lift is perpendicular to u, `(-uy, ux)`.
            ax += k * (-cd * speed * ux + cl * speed * -uy);
            ay += k * (-cd * speed * uy + cl * speed * ux);

            // The torque law: restoring around broadside (`sin(2 delta)`
            // flips sign either side of `delta = +-pi/2`, not around zero -
            // see the module doc for why that is the right fixed point),
            // damped by airspeed and by a small constant floor. `flutter`
            // both strengthens the restoring term and eases the airspeed
            // damping, which is what lets it ring longer instead of just
            // ringing harder.
            let kt = TORQUE_K * (0.5 + 0.7 * flutter);
            let damp_aero = DAMP_AERO * (1.3 - 0.55 * flutter).max(0.25);
            let i = (self.build.inertia * self.build.size).max(0.2);
            let torque = kt * cd2 * speed * speed / i;
            self.omega += (torque - damp_aero * speed * self.omega - DAMP_FLOOR * self.omega) * STEP;
            self.shimmer = sd.abs();
        } else {
            self.omega *= 1.0 - DAMP_FLOOR * STEP;
        }

        self.vel.0 += ax * STEP;
        self.vel.1 += ay * STEP;
        self.theta += self.omega * STEP;
        self.pos.0 += self.vel.0 * STEP;
        self.pos.1 += self.vel.1 * STEP;
        self.aloft += STEP;
    }

    /// How broadside-to-its-own-motion this leaf looks right now, `0`
    /// (edge-on) to `1` (broadside): `self.shimmer`, the same folded
    /// angle-of-attack the aerodynamics above are computed from, so the
    /// silhouette and the physics can never disagree about which way the
    /// leaf is turned. This is the "shimmer" the card asks for: as `delta`
    /// rocks between the two, the drawn size pulses between a slim 1-2 LED
    /// sliver and a fuller 3-4 LED blob.
    #[must_use]
    pub fn presented(&self) -> f32 {
        self.shimmer
    }
}
