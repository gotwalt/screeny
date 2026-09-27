//! What this patch promises, checked: determinism, that ten minutes of
//! hunting does not settle into a loop, and that the colony stays mostly in
//! shot without piling up against one edge of it.

use super::sim::{self, v3, Sim, Tuning};
use super::*;
use crate::patch::Params;
use crate::snapshot::{self, Shot};

/// A camera for the sim-level tests: fixed axes and a roaming-cone centre
/// dead ahead, independent of `build`'s own seeded moon placement, so these
/// tests are about the flight mechanics and not about where a particular
/// seed happened to put the moon.
fn test_sim(seed: u64, birds: usize) -> Sim {
    let fwd = v3(0.0, 0.0, 1.0);
    let world_up = v3(0.0, 1.0, 0.0);
    let right = world_up.cross(fwd).unit_or(v3(1.0, 0.0, 0.0));
    let up = fwd.cross(right).unit_or(world_up);
    Sim::new(seed, fwd, right, up, birds, 0.0, 2.0)
}

fn reference_tuning() -> Tuning {
    Tuning { pace: 1.0, jink: 0.6, loose: 0.55, beat_hz: 9.0, stream: 0.45 }
}

// ---------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------

/// The same seed at the same moment is the same PNG. Everything else here
/// leans on this being true.
#[test]
fn the_same_seed_at_the_same_moment_is_the_same_frame() {
    let params = Params::defaults(PARAMS);
    let shot = Shot { seed: 7, at: 8.0, warmup: 8.0, ..Shot::default() };
    let a = snapshot::take(&DEF, &params, &shot);
    let b = snapshot::take(&DEF, &params, &shot);
    assert_eq!(a.wire.rgb, b.wire.rgb);
}

/// The flight is the same at any render rate: the fixed 1/60 s step and the
/// step count taken from total simulated time (not an accumulator) are what
/// make that true, same reasoning as flock's own version of this test.
#[test]
fn the_flight_does_not_depend_on_the_frame_rate() {
    let params = Params::defaults(PARAMS);
    let at = |fps: f64| {
        let mut patch = (DEF.make)(5);
        let dt = 1.0 / fps;
        let mut frame = Frame::black();
        for i in 1..=(45.0 * fps) as usize {
            frame = patch.render(&Ctx { t: i as f64 * dt, dt, now: 0.0, params: &params });
        }
        match frame {
            Frame::Indexed { palette, indices } => indices.iter().map(|i| palette[*i as usize]).collect::<Vec<_>>(),
            Frame::Linear(px) => px,
        }
    };
    let (a, b) = (at(30.0), at(60.0));
    let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    eprintln!("45 s flown at 30 and at 60 fps: {differ} of {} pixels differ", a.len());
    assert_eq!(differ, 0, "the flight drifted apart between 30 and 60 fps");
}

// ---------------------------------------------------------------------
// Ten minutes: no loop
// ---------------------------------------------------------------------

/// How much a series looks like itself `lag` samples later - Pearson
/// correlation of the two overlapping windows, each against its own mean.
/// Copied from `flock/tests.rs`'s helper of the same name (a tiny, generic
/// statistic, not flock-specific logic; nothing about flock is at risk by
/// having a second copy of it).
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
    if da <= 1e-9 || db <= 1e-9 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// Ten simulated minutes, three seeds: the heading of bat 0 (as an azimuth
/// about the fixed camera, sampled twice a second) must not correlate
/// strongly with itself at any lag from thirty seconds out to five minutes -
/// which is what "comes round again" would look like - and the colony's
/// group-streaming events, which are on their own semi-random timer, must
/// not land at evenly spaced times either.
#[test]
fn ten_minutes_of_hunting_does_not_settle_into_a_loop() {
    for seed in [3, 41, 907] {
        let mut sim = test_sim(seed, 16);
        let tune = reference_tuning();
        let (fwd, right, _up) = sim.cam();
        let mut heading_az = Vec::new();
        let mut group_starts = Vec::new();
        let mut was_grouping = false;
        let steps = (600.0 / sim::STEP) as usize;
        for i in 0..steps {
            sim.step(&tune, sim::STEP);
            let now = sim.grouping();
            if now && !was_grouping {
                group_starts.push(i as f32 * sim::STEP);
            }
            was_grouping = now;
            if i % 30 == 0 {
                let h = sim.bats[0].heading();
                heading_az.push(h.dot(right).atan2(h.dot(fwd)));
            }
        }

        for lag_s in [30.0, 90.0, 200.0] {
            let lag = (lag_s / 0.5) as usize;
            let c = correlation(&heading_az, lag).abs();
            eprintln!("bats seed {seed}: heading autocorrelation at {lag_s}s lag = {c:.2}");
            assert!(c < 0.85, "seed {seed}: heading repeats itself after {lag_s}s (r={c:.2})");
        }

        assert!(
            group_starts.len() >= 2,
            "seed {seed}: ten minutes at `stream` 0.45 should group more than once, got {}",
            group_starts.len()
        );
        let gaps: Vec<f32> = group_starts.windows(2).map(|w| w[1] - w[0]).collect();
        let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
        let spread = gaps.iter().map(|g| (g - mean).abs()).fold(0.0, f32::max);
        eprintln!("seed {seed}: {} groups, gaps {gaps:?}, mean {mean:.1}s, spread {spread:.1}s", group_starts.len());
        assert!(spread > mean * 0.05, "seed {seed}: the colony groups up on too regular a beat to be believed");
    }
}

// ---------------------------------------------------------------------
// Framing: mostly in shot, not piled on an edge
// ---------------------------------------------------------------------

/// Five minutes, sampled every half second: every bat's position, as an
/// azimuth and elevation about the fixed camera, must mostly fall inside the
/// roaming cone the flight is meant to keep to, and must not spend the run
/// pressed against either wall of it - which is what "stay mostly in frame
/// without clumping on an edge" (card 313's acceptance) asks for.
#[test]
fn the_colony_stays_in_frame_and_does_not_clump_on_an_edge() {
    let mut sim = test_sim(17, 20);
    let tune = reference_tuning();
    let (fwd, right, up) = sim.cam();
    let (az_min, az_max) = sim.az_bounds();
    let (el_min, el_max) = sim.el_bounds();
    let (mut inside, mut total) = (0usize, 0usize);
    let (mut at_az_wall, mut at_el_wall) = (0usize, 0usize);
    let steps = (300.0 / sim::STEP) as usize;
    for i in 0..steps {
        sim.step(&tune, sim::STEP);
        if i % 30 != 0 {
            continue;
        }
        for b in &sim.bats {
            let depth = b.pos.dot(fwd).max(0.05);
            let az = (b.pos.dot(right) / depth).atan().to_degrees();
            let el = (b.pos.dot(up) / depth).atan().to_degrees();
            total += 1;
            if (az_min - 4.0..=az_max + 4.0).contains(&az) && (el_min - 4.0..=el_max + 4.0).contains(&el) {
                inside += 1;
            }
            if az <= az_min + 1.5 || az >= az_max - 1.5 {
                at_az_wall += 1;
            }
            if el <= el_min + 1.5 || el >= el_max - 1.5 {
                at_el_wall += 1;
            }
        }
    }
    let frac_inside = inside as f32 / total as f32;
    let frac_az_wall = at_az_wall as f32 / total as f32;
    let frac_el_wall = at_el_wall as f32 / total as f32;
    eprintln!(
        "framing over 5 min: {frac_inside:.2} of samples inside the cone, \
         {frac_az_wall:.2} pressed on an azimuth wall, {frac_el_wall:.2} on an elevation wall"
    );
    assert!(frac_inside > 0.85, "only {frac_inside:.2} of the colony stayed in the roaming cone");
    assert!(frac_az_wall < 0.12, "{frac_az_wall:.2} of samples are clumped on the left/right edge of the cone");
    assert!(frac_el_wall < 0.12, "{frac_el_wall:.2} of samples are clumped on the top/bottom edge of the cone");
}
