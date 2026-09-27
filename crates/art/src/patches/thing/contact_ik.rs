//! Per-finger contact IK: given a fingertip pinned to a spot on the floor
//! and a moving anchor (the MCP joint moves with the wrist as the hand
//! walks, per `hand_rig.rs`), solve the two joint angles that keep the
//! *reachable* part of the chain on target - "fingertips flagged as planted
//! stay pinned to the floor... so the walk does not skate" (the card).
//!
//! This is the standard two-link planar IK (a `PIP`-`DIP` chain treated as
//! two segments, `MCP`'s own flex playing the part of the first link's
//! angle) - the textbook law-of-cosines solve, done once in the plane the
//! finger actually bends in rather than in full 3D, because both hinges
//! share one axis (`hand_rig.rs`'s doc on why `mcp`/`pip`/`dip` all rotate
//! about the parent frame's own `right`). `dip` is left to the animation
//! (or a small fixed curl): three links pinned exactly is over-determined
//! for one target point, and the tip's small remaining length is inside
//! this solve's own tolerance (see the module's test).

#[cfg(test)]
use std::f32::consts::PI;

/// Solve for `(first_angle, second_angle)` so that a two-link chain rooted
/// at the origin, first link length `l1` then second link length `l2`, both
/// measured as an angle from the `a` axis rotating towards the `b` axis (see
/// `hand_rig.rs`'s doc for why that is the same convention `mcp`/`pip`
/// flexion already use), reaches `(a, b)`. Returns the *unreachable-clamped*
/// solution when `(a, b)` is farther than `l1 + l2` or closer than
/// `|l1 - l2|`, which is what keeps a foot that has wandered out of the
/// finger's reach from producing a NaN rather than a stretched, honest
/// best effort.
#[must_use]
pub fn solve_2link(a: f32, b: f32, l1: f32, l2: f32) -> (f32, f32) {
    let d = a.hypot(b).clamp((l1 - l2).abs() + 1e-5, l1 + l2 - 1e-5);
    let cos_theta2 = ((d * d - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
    let theta2 = cos_theta2.acos();
    let (k1, k2) = (l1 + l2 * theta2.cos(), l2 * theta2.sin());
    let theta1 = b.atan2(a) - k2.atan2(k1);
    (theta1, theta2)
}

/// Where the chain's end lands for `(theta1, theta2)` - the round trip
/// `solve_2link`'s own test checks against. (`player.rs::pin_contacts`
/// computes its own initial target through the real three-segment forward
/// kinematics directly rather than this two-link version, which is why
/// this is `#[cfg(test)]` - a production caller wanting the two-link
/// forward model can drop that.)
#[cfg(test)]
#[must_use]
pub fn forward_2link(theta1: f32, theta2: f32, l1: f32, l2: f32) -> (f32, f32) {
    let x = l1 * theta1.cos() + l2 * (theta1 + theta2).cos();
    let y = l1 * theta1.sin() + l2 * (theta1 + theta2).sin();
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn a_reachable_target_is_matched_exactly() {
        let mut rng = Rng::new(3);
        for _ in 0..500 {
            let (l1, l2) = (rng.range(0.02, 0.12), rng.range(0.02, 0.12));
            // Any angle pair is a reachable target by construction: forward
            // it, then solve back to it.
            let (t1, t2) = (rng.range(-2.0, 2.0), rng.range(0.0, PI));
            let (a, b) = forward_2link(t1, t2, l1, l2);
            let (s1, s2) = solve_2link(a, b, l1, l2);
            let (ra, rb) = forward_2link(s1, s2, l1, l2);
            assert!((ra - a).hypot(rb - b) < 1e-3, "({ra},{rb}) vs ({a},{b})");
        }
    }

    #[test]
    fn an_unreachable_target_clamps_instead_of_producing_nan() {
        let (theta1, theta2) = solve_2link(100.0, 0.0, 0.05, 0.05);
        assert!(theta1.is_finite() && theta2.is_finite());
        // Stretched as straight as the clamp allows.
        assert!(theta2 < 0.05, "an out-of-reach target should straighten the chain: {theta2}");
    }

    #[test]
    fn zero_second_angle_means_the_chain_stays_straight() {
        let (x, y) = forward_2link(0.3, 0.0, 0.1, 0.08);
        assert!((y / x - (0.3_f32).tan()).abs() < 1e-3, "a straight chain should lie along its own first-link angle");
    }
}
