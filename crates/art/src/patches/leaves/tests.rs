//! What this patch promises, checked.

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
/// from the same seed must draw the same frame.
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
/// `each` after every frame with the patch's own state - the pile and the
/// leaves aloft - so the long-run tests below can look inside it directly
/// rather than only at the pixels it draws.
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

/// Ten minutes: the card's own bar for a long run. One flight, several
/// questions, exactly as `flock`'s ten-minute run is asked several things
/// rather than measured five times over.
const LONG_RUN_SECONDS: f64 = 600.0;

/// The pile is built only through [`PileCol::add`], which clamps to its cap
/// by construction - but that is a promise about one function, and this is
/// the promise about the patch: over a long run, at a fixed setting, no
/// column of the pile is ever seen above its cap.
#[test]
fn the_pile_never_exceeds_its_cap_over_a_long_run() {
    for pile_param in [0.0_f32, 0.4, 1.0] {
        let mut params = defaults();
        params.set(PARAMS, "pile", pile_param);
        params.set(PARAMS, "gusts", 1.2);
        params.set(PARAMS, "wind", 1.4);
        let cap = PILE_MAX_ROWS * pile_param;
        let mut worst = 0.0_f32;
        fly(7, &params, LONG_RUN_SECONDS, |p, _t| {
            for col in &p.pile {
                let h = col.height();
                worst = worst.max(h);
                assert!(h <= cap + 1e-4, "pile {pile_param}: a column reached {h} rows, cap is {cap}");
            }
        });
        eprintln!("leaves: pile={pile_param} cap={cap:.2} worst column reached {worst:.2}");
    }
}

/// No leaf is ever aloft longer than the model's own hard bound - the
/// backstop that fires if gravity and the wind ever conspire to suspend one
/// (see `MAX_ALOFT`'s doc). This is a check on the recycling logic actually
/// firing, not just on the bound existing: it fails if `stuck` were ever
/// computed but not acted on.
#[test]
fn no_leaf_is_ever_stuck_aloft() {
    let mut params = defaults();
    params.set(PARAMS, "gusts", 0.9);
    params.set(PARAMS, "wind", 0.3);
    let mut worst = 0.0_f32;
    fly(11, &params, LONG_RUN_SECONDS, |p, _t| {
        for leaf in &p.leaves {
            worst = worst.max(leaf.aloft);
            assert!(leaf.aloft <= MAX_ALOFT + 1.0, "a leaf was aloft {} s, bound is {MAX_ALOFT} s", leaf.aloft);
        }
    });
    eprintln!("leaves: longest any leaf stayed aloft was {worst:.1} s (bound {MAX_ALOFT} s)");
}

/// The flutter actually oscillates: a falling leaf's angle of attack changes
/// sign more than once, rather than settling motionless or spinning one way
/// forever. Checked directly on [`leaf::Leaf::step`] rather than through a
/// whole patch, so it is a statement about the model, not about how often
/// the patch happens to draw one leaf long enough to see it.
#[test]
fn a_falling_leaf_actually_flutters() {
    let wind = wind::Wind::new(3);
    let mut leaf = Leaf {
        pos: (32.0, -2.0),
        vel: (0.0, 0.0),
        theta: 0.0,
        omega: 0.0,
        build: Build { size: 1.0, inertia: 1.0, hue: 0 },
        aloft: 0.0,
        shimmer: 0.5,
    };
    let mut signs = Vec::new();
    let mut t = 0.0_f32;
    for _ in 0..(12.0 / STEP) as usize {
        leaf.step(&wind, t, 0.6, 0.6, 1.0);
        t += STEP;
        // The folded angle relative to straight down stands in for angle of
        // attack in still-ish air; its sign flipping is the rock.
        let folded = {
            let mut a = leaf.theta % std::f32::consts::PI;
            if a > std::f32::consts::FRAC_PI_2 {
                a -= std::f32::consts::PI;
            } else if a <= -std::f32::consts::FRAC_PI_2 {
                a += std::f32::consts::PI;
            }
            a
        };
        signs.push(folded.signum());
    }
    let flips = signs.windows(2).filter(|w| w[0] != w[1] && w[0] != 0.0 && w[1] != 0.0).count();
    assert!(flips >= 3, "a fluttering leaf should change its tilt's sign several times in 12 s; saw {flips}");
    assert!(leaf.pos.1 > 0.0, "twelve seconds should be enough for a leaf to actually fall");
}

/// How much a series looks like itself again after `lag` samples: Pearson
/// correlation between the two overlapping windows, each against its own
/// mean (so a smooth drift does not read as "repeats" the way correlating
/// against a shared mean would). 1 is "exactly this again"; this cannot ever
/// legitimately reach it on a run with an unbounded, noise-driven wind field
/// behind it.
fn correlation(series: &[f32], lag: usize) -> f32 {
    let n = series.len().saturating_sub(lag);
    if n < 60 {
        return 0.0;
    }
    let (a, b) = (&series[..n], &series[lag..lag + n]);
    let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
    let (ma, mb) = (mean(a), mean(b));
    let mut num = 0.0;
    let (mut da, mut db) = (0.0, 0.0);
    for i in 0..n {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    if da <= 1e-6 || db <= 1e-6 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// First differences: turns a position into a velocity, a height into a
/// fill rate. The raw series are asked to do too much on their own - the
/// pile's own height is supposed to sit near its cap most of the time, and a
/// stationary series correlates with itself trivially (this is the same trap
/// `flock`'s own periodicity test names in its Log: "a smooth wander
/// correlates with itself at every lag"). Differencing removes the
/// equilibrium and asks the real question: does the *pattern of change* ever
/// come back around.
fn changes(series: &[f32]) -> Vec<f32> {
    series.windows(2).map(|w| w[1] - w[0]).collect()
}

/// Ten minutes does not settle into a loop: sampled once a second, how the
/// leaves' mean downwind position and the pile's own total height are
/// *changing* should not come back around and correlate strongly with an
/// earlier stretch of themselves, at any lag from ten seconds to five
/// minutes.
#[test]
fn ten_minutes_does_not_repeat_itself() {
    let mut params = defaults();
    params.set(PARAMS, "gusts", 0.8);
    params.set(PARAMS, "wind", 0.7);
    let mut mean_x = Vec::new();
    let mut pile_total = Vec::new();
    let mut last_sample = -1.0_f64;
    fly(23, &params, LONG_RUN_SECONDS, |p, t| {
        if t - last_sample >= 1.0 {
            last_sample = t;
            let mx = p.leaves.iter().map(|l| l.pos.0).sum::<f32>() / p.leaves.len().max(1) as f32;
            mean_x.push(mx);
            pile_total.push(p.pile.iter().map(PileCol::height).sum::<f32>());
        }
    });

    for (name, series) in [("mean_x", changes(&mean_x)), ("pile_total", changes(&pile_total))] {
        let mut worst = 0.0_f32;
        for lag in 10..300 {
            worst = worst.max(correlation(&series, lag).abs());
        }
        eprintln!("leaves: 10 min, {name} worst self-correlation {worst:.2}");
        assert!(worst < 0.9, "{name}: a ten-minute run correlated with itself at {worst:.2} at some lag - it is looping");
    }
}

/// `leaves` is really in the studio's own patch list, the way the card asks.
#[test]
fn leaves_is_registered() {
    assert!(crate::patch::find("leaves").is_some());
}

/// Not an acceptance check - a one-off dump of a single leaf's flight, gated
/// behind an env var so it never runs in an ordinary `cargo test`, for making
/// the card's trail render (`LEAF_TRAIL=1 cargo test -p screeny-art --
/// dump_one_leaf_trail --nocapture`). Left in rather than a throwaway script
/// outside the crate so it exercises the same `Leaf::step` the patch does.
#[test]
fn dump_one_leaf_trail_for_the_card_render() {
    if std::env::var_os("LEAF_TRAIL").is_none() {
        return;
    }
    let wind = wind::Wind::new(3);
    let mut leaf = Leaf {
        pos: (10.0, -2.0),
        vel: (0.0, 0.0),
        theta: 0.0,
        omega: 0.0,
        build: Build { size: 1.0, inertia: 1.0, hue: 0 },
        aloft: 0.0,
        shimmer: 0.5,
    };
    let mut t = 0.0_f32;
    println!("t,x,y,theta,shimmer");
    for i in 0..(9.0 / STEP) as usize {
        leaf.step(&wind, t, 0.6, 0.5, 1.0);
        t += STEP;
        if i % 3 == 0 {
            println!("{t},{},{},{},{}", leaf.pos.0, leaf.pos.1, leaf.theta, leaf.presented());
        }
    }
}
