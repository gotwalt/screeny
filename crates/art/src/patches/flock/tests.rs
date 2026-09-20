//! What this patch promises, checked - and the one long run that measures it.

use super::sim::{bearing, wrap, Sim, Tuning, STEP};
use super::*;
use crate::patch::Params;
use crate::snapshot::{self, Shot};

/// Ten minutes of flight, with everything worth knowing gathered as it goes.
///
/// One run, many questions: it is the same flight that has to keep the birds
/// out of the invisible geometry, keep the flock in frame, keep the view
/// gentle and never settle into a loop, so it is measured once and asked
/// several things, rather than flown five times.
struct Run {
    /// Sampled four times a second.
    nearest: Vec<f32>,
    in_frame: Vec<usize>,
    /// Apparent wingspan of the nearest bird in frame, in LEDs.
    biggest: Vec<f32>,
    /// Degrees a second the view yaws and rolls.
    yaw: Vec<f32>,
    roll: Vec<f32>,
    /// Smallest gap between any bird and any blob's surface, metres.
    clearance: f32,
    /// Worst speed seen outside the band, and worst angular rate, as a ratio
    /// of the limit (1.0 = exactly on it).
    speed_ratio: f32,
    turn_ratio: f32,
    /// How far the camera sat from the flock's centroid.
    seat: Vec<f32>,
    /// Once a second, for the periodicity question.
    centroid: Vec<[f32; 3]>,
    course: Vec<f32>,
}

fn percentile(v: &[f32], p: f32) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[((s.len() - 1) as f32 * p).round() as usize]
}

fn median_usize(v: &[usize]) -> usize {
    let mut s = v.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}

fn fly(seed: u64, seconds: f32, tune: &Tuning, birds: usize) -> Run {
    let mut sim = Sim::new(seed);
    sim.resize(birds);
    let sun = v3(0.0, 0.18, 0.983);
    let mut run = Run {
        nearest: Vec::new(),
        in_frame: Vec::new(),
        biggest: Vec::new(),
        yaw: Vec::new(),
        roll: Vec::new(),
        clearance: f32::INFINITY,
        speed_ratio: 1.0,
        turn_ratio: 0.0,
        seat: Vec::new(),
        centroid: Vec::new(),
        course: Vec::new(),
    };
    let steps = (seconds / STEP) as usize;
    let mut was: Vec<V3> = sim.birds.iter().map(|b| b.heading()).collect();
    let (mut look, mut roll) = (sim.look, sim.view_roll);
    for i in 0..steps {
        sim.step(tune, STEP);

        // Limits, every step: a single frame outside them is a failure. The
        // camera has a band of its own (it may push 30% harder to catch up),
        // so it is measured against that and not against the flock's.
        for (i, (b, old)) in sim.birds.iter().zip(&was).enumerate() {
            let s = b.speed();
            let (lo, hi) = tune.speed;
            let (lo, hi) = if i == 0 { (lo * 0.5, hi * 1.3) } else { (lo, hi) };
            run.speed_ratio = run.speed_ratio.max(s / hi).max(lo / s.max(1e-3));
            let swing = sim::angle_between(*old, b.heading()) / STEP;
            let limit = if i == 0 { tune.turn * 2.75 } else { tune.turn };
            run.turn_ratio = run.turn_ratio.max(swing / limit);
        }
        was = sim.birds.iter().map(|b| b.heading()).collect();
        run.clearance = run.clearance.min(sim.clearance(tune.blobs));

        let d_yaw = sim::angle_between(look, sim.look) / STEP;
        let d_roll = (sim.view_roll - roll).abs() / STEP;
        look = sim.look;
        roll = sim.view_roll;
        run.yaw.push(d_yaw.to_degrees());
        run.roll.push(d_roll.to_degrees());

        if i % 15 == 0 {
            let view = View::of(&sim, sun);
            let mut seen = 0;
            let mut biggest = 0.0_f32;
            for b in sim.flock() {
                if let Some((x, y, z)) = view.project(b.pos) {
                    if (-2.0..W as f32 + 2.0).contains(&x) && (-2.0..H as f32 + 2.0).contains(&y) {
                        seen += 1;
                        biggest = biggest.max(sim::SPAN * view.focal / z);
                    }
                }
            }
            run.in_frame.push(seen);
            run.biggest.push(biggest);
            run.nearest.push(sim.nearest());
            run.seat.push(sim.camera().pos.sub(sim.centre).len());
        }
        if i % 60 == 0 {
            let c = sim.centre;
            run.centroid.push([c.x, c.y, c.z]);
            run.course.push(bearing(sim.course));
        }
    }
    run
}

/// How much a series looks like itself `lag` samples later: Pearson
/// correlation of the two overlapping windows, each against its own mean. 1
/// is "exactly this again", 0 is "nothing in common", and it cannot exceed 1.
///
/// The first version of this normalised against the whole series' energy,
/// which let it read 1.00 on a path that never repeated at all - a smooth
/// wander correlates with itself at every lag. What is asked here is whether
/// the flight *comes round again*, so the series fed to it are the stationary
/// ones - how fast the flock is going and how fast it is turning - not where
/// it happens to be.
fn correlation(series: &[f32], lag: usize) -> f32 {
    let n = series.len() - lag;
    if n < 60 {
        return 0.0;
    }
    let (a, b) = (&series[..n], &series[lag..]);
    let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
    let (ma, mb) = (mean(a), mean(b));
    let mut num = 0.0;
    let (mut da, mut db) = (0.0, 0.0);
    for i in 0..n {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    if da <= 1e-9 || db <= 1e-9 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// Differences of a series: turns a path into a velocity, a bearing into a
/// turn rate. Bearings wrap, so they are differenced the short way round.
fn changes(series: &[f32], angles: bool) -> Vec<f32> {
    series
        .windows(2)
        .map(|w| if angles { wrap(w[1] - w[0]) } else { w[1] - w[0] })
        .collect()
}

fn defaults() -> (Tuning, usize) {
    let p = Params::defaults(PARAMS);
    let ctx = Ctx { t: 0.0, dt: 0.0, now: 0.0, params: &p };
    (Flock::tuning(&ctx), p.get("birds") as usize)
}

// ----------------------------------------------------------------------
// The flight
// ----------------------------------------------------------------------

/// The one long run. Ten simulated minutes at the defaults, asked everything.
#[test]
fn ten_minutes_of_flight() {
    let (tune, birds) = defaults();
    let run = fly(11, 600.0, &tune, birds);

    let in_min = *run.in_frame.iter().min().expect("samples");
    let in_med = median_usize(&run.in_frame);
    eprintln!(
        "flock, 10 min, {birds} birds:\n  \
         in frame  min {in_min}  median {in_med}  p05 {:.0}\n  \
         nearest   min {:.1} m  median {:.1} m   biggest bird median {:.1} LEDs, p95 {:.1}\n  \
         seat      median {:.1} m from the centroid, p95 {:.1} m\n  \
         view      yaw p95 {:.1} deg/s (max {:.1}), roll p95 {:.2} deg/s (max {:.2})\n  \
         limits    speed x{:.3}, turn rate x{:.3}, blob clearance {:.1} m",
        percentile(&run.in_frame.iter().map(|n| *n as f32).collect::<Vec<_>>(), 0.05),
        run.nearest.iter().copied().fold(f32::INFINITY, f32::min),
        percentile(&run.nearest, 0.5),
        percentile(&run.biggest, 0.5),
        percentile(&run.biggest, 0.95),
        percentile(&run.seat, 0.5),
        percentile(&run.seat, 0.95),
        percentile(&run.yaw, 0.95),
        percentile(&run.yaw, 1.0),
        percentile(&run.roll, 0.95),
        percentile(&run.roll, 1.0),
        run.speed_ratio,
        run.turn_ratio,
        run.clearance,
    );

    // Periodicity: the strongest the flock's motion ever resembles itself
    // again, over every lag from half a minute to five.
    let vx = changes(&run.centroid.iter().map(|c| c[0]).collect::<Vec<_>>(), false);
    let vz = changes(&run.centroid.iter().map(|c| c[2]).collect::<Vec<_>>(), false);
    let turn = changes(&run.course, true);
    let mut worst = (0.0_f32, 0usize, "");
    for (name, series) in [("drift x", &vx), ("drift z", &vz), ("turn rate", &turn)] {
        for lag in 30..300 {
            let r = correlation(series, lag).abs();
            if r > worst.0 {
                worst = (r, lag, name);
            }
        }
    }
    eprintln!("  loop      strongest self-similarity r={:.2} at {} s ({})", worst.0, worst.1, worst.2);

    assert!(in_min >= 6, "the flock left the frame: only {in_min} birds in shot at worst");
    assert!(in_med >= 20, "median {in_med} birds in frame is not a flock");
    assert!(run.clearance >= 0.0, "a bird was {:.2} m inside a blob", -run.clearance);
    assert!(run.speed_ratio <= 1.02, "speed went x{:.3} outside its band", run.speed_ratio);
    assert!(run.turn_ratio <= 1.10, "a bird turned at x{:.3} of its limit", run.turn_ratio);
    assert!(percentile(&run.yaw, 0.95) <= 18.0, "the view yaws at {:.1} deg/s", percentile(&run.yaw, 0.95));
    assert!(percentile(&run.yaw, 1.0) <= 26.0, "the view swung at {:.1} deg/s", percentile(&run.yaw, 1.0));
    assert!(percentile(&run.roll, 0.95) <= 4.0, "the view rolls at {:.2} deg/s", percentile(&run.roll, 0.95));
    assert!(worst.0 < 0.6, "the flight repeats itself: r={:.2} at {} s", worst.0, worst.1);
}

// ----------------------------------------------------------------------
// The picture
// ----------------------------------------------------------------------

/// The same seed at the same moment is the same PNG. Everything else here
/// leans on this.
#[test]
fn the_same_seed_at_the_same_moment_is_the_same_frame() {
    let params = Params::defaults(PARAMS);
    let shot = Shot { seed: 7, at: 9.0, warmup: 9.0, ..Shot::default() };
    let a = snapshot::take(&DEF, &params, &shot);
    let b = snapshot::take(&DEF, &params, &shot);
    assert_eq!(a.wire.rgb, b.wire.rgb);
}
