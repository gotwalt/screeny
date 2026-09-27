//! What this patch promises, checked.
//!
//! GPU patches are checked by eye and by shader source, not by pixel
//! equality (`crate::patches::tests`'s own doc: "a test machine may have no
//! adapter") - a machine with none renders every frame `Frame::black()`,
//! which would make a pixel-comparison test pass vacuously rather than fail
//! loudly. So every test here that has to hold *everywhere* reads the
//! physics state directly (`LeavesPatch::falling`/`resting`, `Leaf3D`'s own
//! fields), exactly as `flock`'s and card 314's own tests do for their
//! simulations, and only the determinism test touches rendered pixels - safe
//! either way, because a black frame twice in a row is still equal to
//! itself.

use super::*;
use crate::patch::Params;

fn ctx(t: f64, dt: f64, params: &Params) -> Ctx<'_> {
    Ctx { t, dt, now: 0.0, params }
}

fn defaults() -> Params {
    Params::defaults(PARAMS)
}

/// A frame's pixels, expanded to linear RGB, for comparing two runs.
fn pixels(frame: &Frame) -> Vec<[f32; 3]> {
    frame.to_linear().iter().map(|c| [c.r, c.g, c.b]).collect()
}

/// The same command twice: `screeny-art snapshot leaves --seed N --at S` has
/// to draw the same PNG every time, so a fresh patch stepped the same way
/// from the same seed must draw the same frame. Safe on a machine with no
/// GPU adapter too - both runs render `Frame::black()`, which is still equal
/// to itself.
#[test]
fn a_seed_and_a_moment_are_the_same_picture_every_time() {
    let params = defaults();
    for seed in [1, 42, 999_983] {
        let run = |seed: u64| {
            let mut patch = new_patch(seed);
            let mut last = None;
            let dt = 1.0 / 30.0;
            let mut t = 0.0;
            while t < 6.0 {
                t += dt;
                last = Some(pixels(&patch.render(&ctx(t, dt, &params))));
            }
            last.expect("at least one frame")
        };
        assert_eq!(run(seed), run(seed), "seed {seed} drew a different picture the second time");
    }
}

/// Runs the simulation for `seconds` of wall time at a fixed 30 fps, calling
/// `each` after every frame with the patch's own state, so the long-run
/// tests below can look inside it directly rather than only at the pixels it
/// draws.
fn fly(seed: u64, params: &Params, seconds: f64, mut each: impl FnMut(&LeavesPatch, f64)) {
    let mut patch = new_patch(seed);
    let dt = 1.0 / 30.0;
    let mut t = 0.0;
    while t < seconds {
        t += dt;
        let _ = patch.render(&ctx(t, dt, params));
        each(&patch, t);
    }
}

/// Ten minutes: the card's own bar for a long run, as `flock`'s and card
/// 314's ten-minute runs both were.
const LONG_RUN_SECONDS: f64 = 600.0;

/// The resting list never exceeds the `rest` param's own cap, over a long
/// run, at a few different settings of it - "the pile never exceeds its cap"
/// carried over from card 314, for the new shape of "a pile".
#[test]
fn resting_never_exceeds_its_cap_over_a_long_run() {
    for rest in [0.0, 1.0, 4.0, 6.0] {
        let mut params = defaults();
        params.set(PARAMS, "rest", rest);
        params.set(PARAMS, "gusts", 1.2); // gusts pluck resting leaves back up; still must never overshoot
        let cap = rest as usize;
        let mut worst = 0usize;
        fly(3, &params, LONG_RUN_SECONDS, |patch, t| {
            worst = worst.max(patch.resting.len());
            assert!(patch.resting.len() <= cap, "rest={rest}: {} resting at t={t}, cap {cap}", patch.resting.len());
        });
        if cap > 0 {
            assert!(worst > 0, "rest={rest}: never saw a single leaf come to rest in {LONG_RUN_SECONDS}s");
        }
    }
}

/// No falling leaf is ever aloft longer than [`MAX_ALOFT`] - the "no leaf
/// stuck forever" backstop, over a long run at a gusty, windy setting most
/// likely to find an edge case.
#[test]
fn no_leaf_is_ever_stuck_aloft() {
    let mut params = defaults();
    params.set(PARAMS, "wind", 2.2);
    params.set(PARAMS, "gusts", 1.4);
    params.set(PARAMS, "leaves", 8.0);
    fly(11, &params, LONG_RUN_SECONDS, |patch, t| {
        for leaf in &patch.falling {
            assert!(leaf.aloft <= MAX_ALOFT + 1.0, "a leaf has been aloft {} s at t={t}", leaf.aloft);
        }
    });
}

/// The orientation stays a unit quaternion for every falling leaf over a
/// long run - `Quat::integrate` renormalises every step, but this is the
/// acceptance criterion stated at the patch level, not just the maths level
/// ([`geom::tests`] already checks the maths in isolation).
#[test]
fn every_leafs_orientation_stays_normalised_over_a_long_run() {
    let params = defaults();
    fly(5, &params, LONG_RUN_SECONDS, |patch, t| {
        for leaf in &patch.falling {
            let n = leaf.orient.len();
            assert!((n - 1.0).abs() < 1e-3, "a leaf's |orientation| drifted to {n} at t={t}");
        }
        for leaf in &patch.resting {
            let n = leaf.orient.len();
            assert!((n - 1.0).abs() < 1e-3, "a resting leaf's |orientation| drifted to {n} at t={t}");
        }
    });
}

/// A falling leaf's `broadside` (the `n . u_hat` the physics and the picture
/// share) actually swings through a real range rather than sitting at one
/// steady attitude - the 3D "the flutter oscillates" acceptance criterion.
/// This is a *rocking* leaf (moderate flutter, a modest random starting
/// spin): it should visit both a near-edge-on moment and a near-broadside
/// one, which is real movement, whether or not it ever tips all the way
/// through into a full tumble (the next test below is the one that checks a
/// leaf *can* do that). Checked on one leaf tracked continuously, not
/// resampled through the patch's respawn logic - the same way card 314's 2D
/// version tracked one leaf's `shimmer`.
#[test]
fn a_falling_leaf_actually_flutters() {
    let wind = Wind::new(21);
    let mut leaf = spawn_scattered(&mut Rng::new(9), 0.6);
    leaf.pos = v3(0.0, top_y(6.0), 6.0);
    // Released almost exactly edge-on (a small tilt, not zero - see
    // `edge_on_with_no_perturbation_at_all_is_a_fixed_point` for why exactly
    // zero would never move at all) with only the ordinary small random spin
    // `spawn_scattered` gives every leaf - no dramatic kick, the everyday
    // case.
    leaf.orient = geom::Quat::from_axis_angle(v3(1.0, 0.3, 0.0), 0.06);

    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in 0..20_000 {
        leaf.step(&wind, i as f32 * STEP, 0.6, 0.7, 1.4);
        lo = lo.min(leaf.broadside.abs());
        hi = hi.max(leaf.broadside.abs());
    }
    assert!(lo < 0.25, "never came near edge-on: closest was {lo}");
    assert!(hi > 0.65, "never came near broadside: closest was {hi}");
}

/// A leaf spun up hard about an axis across its own face (not about its own
/// normal, which a spin cannot reorient at all - see the module doc's "which
/// one depends on the leaf and the air") really can tumble end over end: at
/// `flutter` on the high end, `broadside` should cross zero and change sign
/// more than once before the flight is over, which is only possible if the
/// leaf's normal has swept all the way through edge-on and out the other
/// side.
#[test]
fn a_hard_enough_spin_tumbles_all_the_way_through() {
    let wind = Wind::new(22);
    let mut leaf = spawn_scattered(&mut Rng::new(10), 0.3);
    leaf.pos = v3(0.0, top_y(5.0), 5.0);
    leaf.orient = geom::Quat::IDENTITY;
    leaf.omega = v3(7.0, 0.0, 0.0); // across the face, not about its own normal

    let mut sign_flips = 0;
    let mut last_sign = 0.0_f32;
    for i in 0..9_000 {
        leaf.step(&wind, i as f32 * STEP, 0.3, 0.4, 1.9);
        let s = leaf.broadside.signum();
        if last_sign != 0.0 && s != 0.0 && s != last_sign {
            sign_flips += 1;
        }
        if leaf.broadside != 0.0 {
            last_sign = s;
        }
    }
    assert!(sign_flips >= 2, "a hard spin across the face should tumble through edge-on repeatedly, got {sign_flips} flips");
}

fn correlation(series: &[f32], lag: usize) -> f32 {
    if series.len() <= lag {
        return 0.0;
    }
    let n = series.len() - lag;
    let a = &series[..n];
    let b = &series[lag..];
    let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
    let (ma, mb) = (mean(a), mean(b));
    let cov: f32 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let (va, vb) = (
        a.iter().map(|x| (x - ma).powi(2)).sum::<f32>(),
        b.iter().map(|y| (y - mb).powi(2)).sum::<f32>(),
    );
    if va <= 1e-9 || vb <= 1e-9 {
        return 0.0;
    }
    cov / (va.sqrt() * vb.sqrt())
}

/// A near-constant series (the resting count sitting at its cap, say)
/// trivially "correlates with itself" at every lag; differencing first
/// (card 314's fix, kept) turns it into a rate, which does not.
fn changes(series: &[f32]) -> Vec<f32> {
    series.windows(2).map(|w| w[1] - w[0]).collect()
}

/// Ten minutes does not settle into a visible loop: the mean depth of the
/// falling leaves (a cheap proxy for "what the picture looks like right
/// now") should not correlate strongly with itself at any of several
/// candidate periods - `flock`'s and card 314's own test for the same thing,
/// at this patch's own signal.
#[test]
fn ten_minutes_does_not_repeat_itself() {
    let mut params = defaults();
    params.set(PARAMS, "gusts", 0.7);
    let mut series = Vec::new();
    fly(17, &params, LONG_RUN_SECONDS, |patch, _t| {
        let mean_z = patch.falling.iter().map(|l| l.pos.z).sum::<f32>() / patch.falling.len().max(1) as f32;
        series.push(mean_z);
    });
    let d = changes(&series);
    let fps = 30.0;
    let mut worst: f32 = 0.0;
    for period_s in [5.0, 15.0, 30.0, 60.0, 120.0] {
        let lag = (period_s * fps) as usize;
        if lag == 0 || lag >= d.len() {
            continue;
        }
        worst = worst.max(correlation(&d, lag).abs());
    }
    assert!(worst < 0.85, "the flight looks like it repeats: worst |correlation| {worst}");
}

#[test]
fn leaves_is_registered() {
    assert!(crate::patch::find("leaves").is_some());
}

/// A near leaf tumbling through several full physics steps, dumped as a
/// `broadside` trail - the evidence for the card's own acceptance criterion
/// ("show a sequence of one near leaf tumbling"), and a cheap sanity check
/// that a leaf given a strong initial spin actually goes all the way round
/// rather than getting stuck.
#[test]
fn dump_one_leaf_trail_for_the_card_render() {
    let wind = Wind::new(3);
    let mut leaf = spawn_scattered(&mut Rng::new(4), 0.4);
    leaf.pos = v3(0.0, top_y(4.0), 4.0);
    // Across the face, not about its own normal - a spin about the normal
    // itself cannot turn it (see the module doc), it can only rotate it.
    leaf.omega = v3(6.5, 0.2, 0.1);
    leaf.orient = geom::Quat::IDENTITY;
    let mut trail = Vec::new();
    for i in 0..600 {
        leaf.step(&wind, i as f32 * STEP, 0.4, 0.5, 1.5);
        trail.push(leaf.broadside);
    }
    let saw_broadside = trail.iter().any(|b| b.abs() > 0.85);
    let saw_edge_on = trail.iter().any(|b| b.abs() < 0.15);
    assert!(saw_broadside && saw_edge_on, "a spun-up leaf should visit both broadside and edge-on: {trail:?}");
}
