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
    // Card 322: this now steps a real Rapier soft body every physics tick
    // (and settles a fresh one on every respawn), which an unoptimized debug
    // build does far slower than release - two seeds over a few seconds each
    // is enough to exercise a respawn or two per run and prove the same
    // determinism a longer run would, at a fraction of the wall time.
    for seed in [1, 42] {
        let run = |seed: u64| {
            let mut patch = new_patch(seed);
            let mut last = None;
            let dt = 1.0 / 30.0;
            let mut t = 0.0;
            while t < 2.0 {
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
/// 314's ten-minute runs both were - kept as the thorough, `#[ignore]`d
/// version of every check below that can be shortened at all (see each
/// test's own doc for which ones cannot).
///
/// Card 322: every leaf respawn now settles a real Rapier soft body
/// ([`super::shell::LeafShell::spawn`], ~240 physics steps) before it is
/// ever drawn, which an unoptimized debug build - the one `cargo test` runs
/// by default - does far slower than the release build anyone actually
/// watches the panel with. Running four separate 600-simulated-second
/// checks (one of them four times over, for `resting_never_exceeds...`'s own
/// four `rest` values) through that costs tens of minutes in debug, which is
/// not what the routine `cargo test -p screeny-art` a person runs between
/// edits should ever cost (the orchestrator caught a stray debug run of
/// exactly this still going after 34 minutes). The house rule
/// ("iterate with host tests and the simulator... run long evidence once")
/// already says what to do about a check that is only expensive because it
/// is thorough: keep the thorough version, but make it opt-in.
const LONG_RUN_SECONDS: f64 = 600.0;

/// The routine, always-run horizon for the same checks: long enough, at
/// this patch's own fall speed, to see a leaf actually land under gusty
/// settings (checked empirically - the smoke version below asserts
/// `worst > 0`, so a horizon too short to ever see a landing would fail
/// loudly, not pass vacuously), short enough that paying Rapier's settle
/// cost on every respawn several times, in debug, is not a real delay.
const SMOKE_SECONDS: f64 = 12.0;

/// The resting list never exceeds the `rest` param's own cap - "the pile
/// never exceeds its cap" carried over from card 314, for the new shape of
/// "a pile". Shared by the fast, routine version and the thorough
/// `#[ignore]`d one below: only `seconds`/`rests`/`leaves` differ, so the
/// routine run proves the same invariant, just over less wall time (and,
/// to still see a landing in that much less time, more leaves aloft at once
/// and fewer `rest` values tried), not a weaker one.
fn resting_never_exceeds_its_cap(seconds: f64, rests: &[f32], leaves: f32) {
    for &rest in rests {
        let mut params = defaults();
        params.set(PARAMS, "rest", rest);
        params.set(PARAMS, "gusts", 1.2); // gusts pluck resting leaves back up; still must never overshoot
        params.set(PARAMS, "leaves", leaves);
        let cap = rest as usize;
        let mut worst = 0usize;
        fly(3, &params, seconds, |patch, t| {
            worst = worst.max(patch.resting.len());
            assert!(patch.resting.len() <= cap, "rest={rest}: {} resting at t={t}, cap {cap}", patch.resting.len());
        });
        if cap > 0 {
            assert!(worst > 0, "rest={rest}: never saw a single leaf come to rest in {seconds}s");
        }
    }
}

/// The smoke version: only the tightest `rest` value (1 - the one most
/// likely to actually be exceeded by a bug, and the middle/higher values are
/// the same code path) at the default leaf count (more leaves means more
/// per-tick soft-body cost for every one of them, which is the actual
/// expense here, not simulated seconds alone - card 322's own respawn settle
/// and per-tick shell step both scale with leaf count, not just time).
#[test]
fn resting_never_exceeds_its_cap_over_a_short_run() {
    resting_never_exceeds_its_cap(SMOKE_SECONDS, &[1.0], 4.0);
}

/// The thorough ten-minute version of the check above, at card 314's
/// original four `rest` values and the default leaf count - `cargo test -p
/// screeny-art --release -- --ignored
/// patches::leaves::tests::resting_never_exceeds_its_cap_over_a_long_run`
/// (release: the routine run above already proves this in debug at a
/// horizon short enough to be fast; this is the "run long evidence once"
/// pass over the full ten minutes, worth the release build's own speed).
#[test]
#[ignore = "10 simulated minutes x 4 rest values; run once with --release -- --ignored"]
fn resting_never_exceeds_its_cap_over_a_long_run() {
    resting_never_exceeds_its_cap(LONG_RUN_SECONDS, &[0.0, 1.0, 4.0, 6.0], 4.0);
}

/// Card 323's own ground plane: no leaf that has actually landed - settled
/// litter, or litter fading off the `rest` cap - ever has a vertex below it.
/// Checked directly against the real world constant, at a gusty setting
/// likely to produce several landings (and, via the cap, several overflows)
/// within the smoke horizon.
#[test]
fn no_litter_leaf_is_ever_below_the_ground() {
    let mut params = defaults();
    params.set(PARAMS, "gusts", 1.2);
    params.set(PARAMS, "leaves", 6.0);
    params.set(PARAMS, "rest", 2.0);
    let floor = ground_plane_y();
    let mut saw_litter = false;
    fly(41, &params, SMOKE_SECONDS, |patch, t| {
        for litter in patch.resting.iter().chain(patch.fading.iter().map(|f| &f.litter)) {
            saw_litter = true;
            for v in &litter.verts {
                assert!(v.pos.y >= floor - 0.05, "a litter vertex sank to {} at t={t}, ground is at {floor}", v.pos.y);
            }
        }
    });
    assert!(saw_litter, "never saw a single leaf land in {SMOKE_SECONDS}s");
}

/// Once a leaf lands, it is truly still - the card's own "put resting bodies
/// to sleep... no jitter, no wobble" - checked as a structural invariant,
/// not just a tuned probability: with the `rest` cap wide enough, and no
/// gusts to pluck one back up, nothing in this patch ever mutates an entry
/// already sitting in `resting` (only `push_back`, on a fresh landing, and
/// `pop_front`, on an overflow this test's own cap never reaches) - so every
/// litter present at an earlier moment must still be present, byte-for-byte,
/// at every later one. The render evidence's own pixel-diff strip
/// (`leaves-ground/settle/`) is the same claim, judged by eye on the actual
/// rendered frame rather than the physics state.
#[test]
fn a_settled_leaf_never_moves_again() {
    let mut params = defaults();
    params.set(PARAMS, "gusts", 0.0);
    params.set(PARAMS, "rest", MAX_REST as f32);
    // Few aloft at once, on purpose: this test's own point is "nothing
    // removes an entry" (checked separately from the cap/fade mechanism,
    // `overflow_fades_rather_than_popping_back_into_flight`, above) - a
    // high `leaves` count risks the initial warm scatter alone landing
    // enough leaves to overflow even this generous a cap within the smoke
    // horizon, which would exercise the *other* test's own mechanism
    // instead of this one's.
    params.set(PARAMS, "leaves", 2.0);
    let mut patch = new_patch(51);
    let dt = 1.0 / 30.0;
    let mut t = 0.0;
    let mut snapshot: Vec<Vec<geom::V3>> = Vec::new();
    let mut checked_any = false;
    while t < SMOKE_SECONDS {
        t += dt;
        let _ = patch.render(&ctx(t, dt, &params));
        assert!(patch.resting.len() <= MAX_REST, "cap grew past its own max: {}", patch.resting.len());
        // Every litter already known about (by its position in the queue,
        // which - with no overflow and no gusts - only ever grows from the
        // back) must still read exactly as it did when first seen.
        for (i, prev) in snapshot.iter().enumerate() {
            let now: Vec<geom::V3> = patch.resting[i].verts.iter().map(|v| v.pos).collect();
            assert_eq!(*prev, now, "litter {i} moved after settling, at t={t}");
            checked_any = true;
        }
        snapshot = patch.resting.iter().map(|l| l.verts.iter().map(|v| v.pos).collect()).collect();
    }
    assert!(checked_any, "never saw a second frame with an already-settled leaf in {SMOKE_SECONDS}s");
}

/// The `rest` cap's overflow fades a litter out over several seconds rather
/// than popping it back into flight on the same frame (321/322's own bump -
/// the owner's "wobble" complaint on this card names it directly). Checked
/// two ways: the cap itself never grows past `rest` (already covered above,
/// re-checked here for this specific scenario), and an overflowed leaf is
/// seen actually fading (present in `fading`, with `vanish_at` within
/// `FADE_SECONDS` of the moment it must have overflowed) rather than simply
/// vanishing.
#[test]
fn overflow_fades_rather_than_popping_back_into_flight() {
    let mut params = defaults();
    params.set(PARAMS, "rest", 1.0);
    params.set(PARAMS, "leaves", 6.0);
    let mut saw_fade = false;
    fly(61, &params, SMOKE_SECONDS, |patch, t| {
        assert!(patch.resting.len() <= 1, "cap exceeded: {} resting at t={t}", patch.resting.len());
        for f in &patch.fading {
            saw_fade = true;
            assert!(f.vanish_at <= t as f32 + FADE_SECONDS + 1e-3, "a fading entry's own vanish time is implausibly far off at t={t}");
        }
    });
    assert!(saw_fade, "never saw an overflow actually fading in {SMOKE_SECONDS}s");
}

/// No falling leaf is ever aloft longer than [`MAX_ALOFT`] (90s) - the "no
/// leaf stuck forever" backstop, over a long run at a gusty, windy setting
/// most likely to find an edge case. **Cannot be meaningfully shortened**:
/// the assertion this test exists to make (`aloft` never exceeds
/// `MAX_ALOFT`) is vacuously true over any run shorter than `MAX_ALOFT`
/// itself, so a "smoke" version would not be a cheaper version of this
/// check, it would be a different, much weaker one pretending to be this
/// one. `#[ignore]`d rather than shortened, per the house rule on long
/// evidence: `cargo test -p screeny-art --release -- --ignored
/// patches::leaves::tests::no_leaf_is_ever_stuck_aloft`.
#[test]
#[ignore = "needs a horizon longer than MAX_ALOFT (90s) to mean anything; run once with --release -- --ignored"]
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
/// run - `Quat::integrate` renormalises every step, but this is the
/// acceptance criterion stated at the patch level, not just the maths level
/// ([`geom::tests`] already checks the maths in isolation over 10,000 steps
/// of the same integrator in complete isolation, which is most of this
/// test's real assurance already - the patch-level version's own added
/// value is exercising the same integrator through respawns and landings,
/// which does not need ten minutes to show up if it were ever going to).
/// Shared body, same reasoning as [`resting_never_exceeds_its_cap`].
fn every_leafs_orientation_stays_normalised(seconds: f64) {
    let params = defaults();
    fly(5, &params, seconds, |patch, t| {
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

/// This check's own smoke horizon: shorter than [`SMOKE_SECONDS`], because
/// unlike the resting-cap check it does not need to witness a landing to
/// mean something - a drifted quaternion would already show up within the
/// first few steps of any falling leaf, respawned or not (the module doc
/// above explains why this test's own real assurance is mostly already
/// covered elsewhere; a few seconds of the patch's own respawn/landing
/// machinery on top of that is enough to be worth the wall time).
const ORIENTATION_SMOKE_SECONDS: f64 = 3.0;

#[test]
fn every_leafs_orientation_stays_normalised_over_a_short_run() {
    every_leafs_orientation_stays_normalised(ORIENTATION_SMOKE_SECONDS);
}

#[test]
#[ignore = "10 simulated minutes; run once with --release -- --ignored"]
fn every_leafs_orientation_stays_normalised_over_a_long_run() {
    every_leafs_orientation_stays_normalised(LONG_RUN_SECONDS);
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
/// at this patch's own signal. **Cannot be meaningfully shortened**: the
/// candidate periods checked go up to 120s, and the check for each one is
/// skipped outright once the run is shorter than that period (`lag >=
/// d.len()`), so a short run would not be a smoke version of this test, it
/// would silently stop checking most of what it claims to. `#[ignore]`d,
/// per the house rule on long evidence: `cargo test -p screeny-art
/// --release -- --ignored patches::leaves::tests::ten_minutes_does_not_
/// repeat_itself`.
#[test]
#[ignore = "needs a multi-minute horizon to check its own longest candidate periods; run once with --release -- --ignored"]
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
