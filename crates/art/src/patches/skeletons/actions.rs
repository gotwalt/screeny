//! The action vocabulary: pure functions from a phase (and a little context)
//! to a [`Pose`]. Nothing here owns time or decides what comes next - that
//! is [`super::actor`] - so every action can be rendered at any phase, from
//! any start, which is what makes a snapshot repeatable and a long run easy
//! to reason about.

use super::geom::Angles;
use super::rig::Pose;
use std::f32::consts::PI;

/// One full stride is this many radians of phase; legs and arms are keyed
/// off `phase.sin()` and `phase.cos()` rather than off time directly, so a
/// walk can change speed without a discontinuity.
const STRIDE: f32 = 0.62;
const KNEE_BASE: f32 = 0.20;
const KNEE_SWING: f32 = 0.95;
const LIFT_AMT: f32 = 0.09;
const ARM_AMP: f32 = 0.55;
const ELBOW_BASE: f32 = 0.35;
const ELBOW_LAG: f32 = 0.9;
const ELBOW_LAG_AMT: f32 = 0.30;
const BOB_AMT: f32 = 0.028;
const LEAN_FWD: f32 = 0.12;
const TWIST: f32 = 0.10;

/// A real walk cycle: `phase` runs at the gait's own rate (radians a
/// second), `speed` 0..1 scales how vigorous the swing is (a shuffle vs a
/// stride) without changing the rhythm, and `lean` biases the torso for
/// walking backward or turning.
#[must_use]
pub fn walk(phase: f32, speed: f32, lean: f32) -> Pose {
    let leg = |ph: f32| {
        let hip = STRIDE * speed * ph.sin();
        let swing = (ph - 0.9).sin().max(0.0);
        let knee = -(KNEE_BASE + KNEE_SWING * speed * swing);
        let lift = LIFT_AMT * speed * swing;
        (hip, knee, lift)
    };
    let (hip_l, knee_l, lift_l) = leg(phase);
    let (hip_r, knee_r, lift_r) = leg(phase + PI);

    let arm = |ph: f32| {
        let shoulder = ARM_AMP * speed * ph.sin();
        let elbow = -(ELBOW_BASE + ELBOW_LAG_AMT * speed * (ph - ELBOW_LAG).sin().max(0.0));
        (shoulder, elbow)
    };
    let (sh_l, el_l) = arm(phase + PI);
    let (sh_r, el_r) = arm(phase);

    Pose {
        torso: Angles::new(0.0, LEAN_FWD * speed + lean, TWIST * speed * phase.sin()),
        neck: Angles::new(0.0, -0.05 * speed, 0.0),
        skull: Angles::default(),
        shoulder: [Angles::new(0.0, sh_l, 0.0), Angles::new(0.0, sh_r, 0.0)],
        elbow: [Angles::new(0.0, el_l, 0.0), Angles::new(0.0, el_r, 0.0)],
        hip: [Angles::new(0.0, hip_l, 0.0), Angles::new(0.0, hip_r, 0.0)],
        knee: [Angles::new(0.0, knee_l, 0.0), Angles::new(0.0, knee_r, 0.0)],
        jaw: 0.0,
        foot_lift: [lift_l, lift_r],
        rise: -0.015 + BOB_AMT * (2.0 * phase).cos(),
    }
}

/// A relaxed, idle stand: not zero (a dead-still rig reads as broken, not
/// calm), a slow shift of weight and a slight, slow head turn.
#[must_use]
pub fn idle(t: f32, sway: f32) -> Pose {
    Pose {
        torso: Angles::new(0.03 * sway * (t * 0.31).sin(), 0.0, 0.05 * sway * (t * 0.23).sin()),
        neck: Angles::new(0.06 * sway * (t * 0.19).cos(), 0.0, 0.0),
        skull: Angles::new(0.12 * sway * (t * 0.17).sin(), 0.04 * sway * (t * 0.29).cos(), 0.0),
        shoulder: [Angles::new(0.0, 0.02 * sway * (t * 0.21).sin(), 0.0), Angles::new(0.0, -0.02 * sway * (t * 0.21).sin(), 0.0)],
        elbow: [Angles::new(0.0, -0.18, 0.0), Angles::new(0.0, -0.18, 0.0)],
        hip: [Angles::default(), Angles::default()],
        knee: [Angles::new(0.0, -0.05, 0.0), Angles::new(0.0, -0.05, 0.0)],
        jaw: 0.02,
        foot_lift: [0.0, 0.0],
        rise: 0.0,
    }
}

/// A jaunty jig: hips swaying, knees bouncing together, arms pumping - the
/// syncopation (the knee bounce runs at twice the hip sway, a beat the hips
/// only visit every other bar) is what keeps it from reading as a metronome.
#[must_use]
pub fn dance(phase: f32) -> Pose {
    let bounce = (2.0 * phase).sin().max(0.0);
    let kick_l = (phase).sin().max(0.0);
    let kick_r = (phase + PI).sin().max(0.0);
    Pose {
        torso: Angles::new(0.10 * phase.sin(), -0.04 * bounce, 0.30 * (phase * 0.5).sin()),
        neck: Angles::new(0.0, 0.0, -0.06 * (phase * 0.5).sin()),
        skull: Angles::new(0.20 * (phase * 0.5 + 0.4).sin(), 0.0, 0.0),
        shoulder: [
            Angles::new(0.0, -0.9 + 0.5 * (phase + 0.6).sin(), -0.7 - 0.5 * kick_r),
            Angles::new(0.0, -0.9 + 0.5 * (phase + 3.7).sin(), 0.7 + 0.5 * kick_l),
        ],
        elbow: [Angles::new(0.0, -1.1, 0.0), Angles::new(0.0, -1.1, 0.0)],
        hip: [Angles::new(0.0, 0.35 * kick_l, 0.0), Angles::new(0.0, 0.35 * kick_r, 0.0)],
        knee: [Angles::new(0.0, -0.25 - 0.55 * bounce, 0.0), Angles::new(0.0, -0.25 - 0.55 * bounce, 0.0)],
        jaw: 0.15 + 0.10 * bounce,
        foot_lift: [0.05 * kick_r, 0.05 * kick_l],
        rise: -0.10 * bounce,
    }
}

/// One arm up, waving at whoever is watching; `side` is `0` (left) or `1`.
#[must_use]
pub fn wave(t: f32, side: usize) -> Pose {
    let mut p = idle(t, 0.3);
    let sgn = if side == 0 { -1.0 } else { 1.0 };
    p.shoulder[side] = Angles::new(0.15 * (t * 6.0).sin(), -0.3, sgn * -2.35);
    p.elbow[side] = Angles::new(0.0, -0.5 + 0.35 * (t * 6.0).sin(), 0.0);
    p.skull = Angles::new(0.05 * sgn, 0.10, 0.0);
    p.torso.yaw = 0.10 * sgn;
    p
}

/// Coming right up to the glass: the walk-in is [`walk`], this is the
/// holding pose once it has arrived - jaw chattering, eyes (the skull)
/// sweeping side to side.
#[must_use]
pub fn peer(t: f32) -> Pose {
    Pose {
        torso: Angles::new(0.0, -0.06, 0.0),
        neck: Angles::new(0.0, -0.10, 0.0),
        skull: Angles::new(0.55 * (t * 1.05).sin(), -0.08, 0.0),
        shoulder: [Angles::new(0.0, -0.10, -0.10), Angles::new(0.0, -0.10, 0.10)],
        elbow: [Angles::new(0.0, -0.30, 0.0), Angles::new(0.0, -0.30, 0.0)],
        hip: [Angles::default(), Angles::default()],
        knee: [Angles::new(0.0, -0.05, 0.0), Angles::new(0.0, -0.05, 0.0)],
        jaw: 0.30 + 0.30 * (0.5 + 0.5 * (t * 7.4).sin()),
        foot_lift: [0.0, 0.0],
        rise: 0.0,
    }
}

/// Down on the floor, legs out in front.
#[must_use]
pub fn sit(t: f32) -> Pose {
    Pose {
        torso: Angles::new(0.0, -0.08, 0.03 * (t * 0.4).sin()),
        neck: Angles::new(0.0, 0.05, 0.0),
        skull: Angles::new(0.10 * (t * 0.3).sin(), 0.0, 0.0),
        shoulder: [Angles::new(0.0, -0.55, -0.15), Angles::new(0.0, -0.55, 0.15)],
        elbow: [Angles::new(0.0, -0.9, 0.0), Angles::new(0.0, -0.9, 0.0)],
        hip: [Angles::new(0.0, 1.45, 0.0), Angles::new(0.0, 1.45, 0.0)],
        knee: [Angles::new(0.0, -0.15, 0.0), Angles::new(0.0, -0.15, 0.0)],
        jaw: 0.05,
        foot_lift: [0.0, 0.0],
        rise: -0.34,
    }
}

/// A shiver: added on top of whatever pose is already playing, not a pose
/// of its own - see [`jitter`].
#[must_use]
pub fn jitter(base: Pose, t: f32, amount: f32) -> Pose {
    if amount <= 0.0 {
        return base;
    }
    let n = |k: f32| amount * (t * k).sin();
    let mut p = base;
    p.torso = p.torso.add(Angles::new(n(41.0), n(37.0), n(53.0)));
    p.neck = p.neck.add(Angles::new(n(47.0), n(59.0), 0.0));
    p.skull = p.skull.add(Angles::new(n(61.0), n(43.0), 0.0));
    for s in 0..2 {
        p.shoulder[s] = p.shoulder[s].add(Angles::new(0.0, n(67.0 + s as f32), 0.0));
        p.hip[s] = p.hip[s].add(Angles::new(0.0, n(53.0 + s as f32), 0.0));
        // A shiver may loosen a hinge, never invert it: an elbow or a knee
        // that wobbles past straight reads as broken, not cold.
        p.elbow[s].pitch = (p.elbow[s].pitch + n(71.0 + s as f32)).min(0.0);
        p.knee[s].pitch = (p.knee[s].pitch + n(59.0 + s as f32)).min(0.0);
    }
    p.jaw = (p.jaw + 0.4 * (0.5 + 0.5 * (t * 44.0).sin()) * amount * 4.0).clamp(0.0, 1.0);
    p
}

/// A flinch: torso jerks back, arms come up, held, and released. `p` 0..1.
#[must_use]
pub fn startle(p: f32) -> Pose {
    let k = super::box_scene::smooth(0.0, 0.18, p) * (1.0 - super::box_scene::smooth(0.55, 1.0, p));
    Pose {
        torso: Angles::new(0.0, -0.5 * k, 0.0),
        neck: Angles::new(0.0, -0.3 * k, 0.0),
        skull: Angles::new(0.0, -0.2 * k, 0.0),
        shoulder: [Angles::new(0.0, -0.6 * k, -1.3 * k), Angles::new(0.0, -0.6 * k, 1.3 * k)],
        elbow: [Angles::new(0.0, -1.2 * k, 0.0), Angles::new(0.0, -1.2 * k, 0.0)],
        hip: [Angles::default(), Angles::default()],
        knee: [Angles::new(0.0, -0.05, 0.0), Angles::new(0.0, -0.05, 0.0)],
        jaw: 0.5 * k,
        foot_lift: [0.0, 0.0],
        rise: 0.02 * k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action is a real pose over a couple of seconds of phase: no
    /// `NaN`, and nothing that asks a hinge to bend the anatomically wrong
    /// way by a wide margin (a knee or an elbow opening up past straight).
    #[test]
    fn every_action_stays_a_sane_pose() {
        let check = |p: Pose| {
            for a in [p.torso, p.neck, p.skull, p.shoulder[0], p.shoulder[1], p.elbow[0], p.elbow[1], p.hip[0], p.hip[1], p.knee[0], p.knee[1]] {
                assert!(a.yaw.is_finite() && a.pitch.is_finite() && a.roll.is_finite());
            }
            assert!(p.knee[0].pitch <= 0.05 && p.knee[1].pitch <= 0.05, "a knee should not hyperextend: {:?}", p.knee);
            assert!(p.elbow[0].pitch <= 0.05 && p.elbow[1].pitch <= 0.05, "an elbow should not hyperextend: {:?}", p.elbow);
            assert!((0.0..=1.0).contains(&p.jaw));
        };
        for i in 0..400 {
            let t = i as f32 * 0.03;
            check(walk(t, 1.0, 0.0));
            check(idle(t, 1.0));
            check(dance(t));
            check(wave(t, 0));
            check(wave(t, 1));
            check(peer(t));
            check(sit(t));
            check(jitter(idle(t, 1.0), t, 0.15));
            check(startle((t / 3.0) % 1.0));
        }
    }
}
