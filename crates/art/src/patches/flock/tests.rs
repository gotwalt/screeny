//! What this patch promises, checked - and the one long run that measures it.

use super::sim::{bearing, wrap, Sim, Tuning, STEP};
use super::*;
use crate::frame::Frame;
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
    /// Smallest gap between any bird and any blob's surface, metres, for the
    /// flock and for the camera. Kept apart: "no bird flies into the
    /// invisible geometry" is a promise about the flock, and the camera -
    /// which is also holding a seat - is worth knowing about separately.
    clearance: f32,
    cam_clearance: f32,
    /// Worst speed seen outside the band, and worst angular rate, as a ratio
    /// of the limit (1.0 = exactly on it).
    speed_ratio: f32,
    turn_ratio: f32,
    /// Steepest climb and steepest dive anything flew at, degrees. The climb
    /// limit is a spring rather than a clamp, so how far past it the flight
    /// ever gets is a measurement and not an assumption.
    steepest: f32,
    /// How far the camera sat from the flock's centroid.
    seat: Vec<f32>,
    /// Once a second, for the periodicity question.
    centroid: Vec<[f32; 3]>,
    course: Vec<f32>,
    /// How far across the flock was, rms.
    spread: Vec<f32>,
    /// Every bird's distance to its nearest neighbour, sampled every two
    /// seconds, the camera left out of it. The *shape* of this distribution is
    /// the author's "very evenly separated ... not lifelike": a lattice is a
    /// spike (coefficient of variation near 0), a live flock is broad and
    /// skewed (card 177 asks for 0.4-0.6).
    nn: Vec<f32>,
    /// The flock centroid's altitude, and how fast it is climbing or diving
    /// (m/s), four times a second. "Some more vertical motion change", as a
    /// number.
    alt: Vec<f32>,
    climb: Vec<f32>,
    /// How far off level the view actually points, degrees. The *rate* says
    /// whether the picture is calm; this says whether the horizon ever moves
    /// at all, which is what makes a dive read.
    tilt: Vec<f32>,
    /// Degrees a second the view *pitches* - the elevation of the look
    /// direction, which is the horizon moving up and down the panel. Card 168
    /// never measured it because the view was pinned level.
    pitch: Vec<f32>,
    /// Seconds between noticeable changes of the flock's course (20 degrees
    /// away from wherever it was last marked). A regular flight gives a narrow
    /// distribution; the card wants a broad one.
    turns: Vec<f32>,
    /// How far the furthest bird was from the flock's middle, every two
    /// seconds, and how many were more than 20 m out. A clumpy flock and a
    /// flock that is shedding birds look the same in a mean; they do not look
    /// the same here.
    furthest: Vec<f32>,
    lost: Vec<f32>,
}

fn percentile(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[((s.len() - 1) as f32 * p).round() as usize]
}

/// Mean, and the coefficient of variation (standard deviation over mean) -
/// which is the shape number, independent of how big the flock happens to be.
fn mean_cv(v: &[f32]) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32;
    (mean, if mean > 1e-6 { var.sqrt() / mean } else { 0.0 })
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
        cam_clearance: f32::INFINITY,
        speed_ratio: 1.0,
        turn_ratio: 0.0,
        steepest: 0.0,
        seat: Vec::new(),
        centroid: Vec::new(),
        course: Vec::new(),
        spread: Vec::new(),
        nn: Vec::new(),
        alt: Vec::new(),
        climb: Vec::new(),
        tilt: Vec::new(),
        pitch: Vec::new(),
        turns: Vec::new(),
        furthest: Vec::new(),
        lost: Vec::new(),
    };
    let steps = (seconds / STEP) as usize;
    let mut was: Vec<V3> = sim.birds.iter().map(|b| b.heading()).collect();
    let (mut look, mut roll) = (sim.look, sim.view_roll);
    // The last course the flight was marked at, and when: a "noticeable
    // heading change" is 20 degrees away from that mark, and the mark then
    // moves to where the flock now points.
    let (mut marked, mut marked_at) = (bearing(sim.course), 0.0_f32);
    for i in 0..steps {
        sim.step(tune, STEP);

        // Limits, every step: a single frame outside them is a failure. The
        // camera has a band of its own (it may push 30% harder to catch up),
        // so it is measured against that and not against the flock's.
        for (i, (b, old)) in sim.birds.iter().zip(&was).enumerate() {
            let s = b.speed();
            let (lo, hi) = tune.speed;
            // The camera may push 30% harder to catch up; a bird flies its own
            // band, `pep` wide of the flock's.
            let (lo, hi) =
                if i == 0 { (lo * 0.5, hi * 1.3) } else { (lo * sim::PEP.0, hi * sim::PEP.1) };
            run.speed_ratio = run.speed_ratio.max(s / hi).max(lo / s.max(1e-3));
            let swing = sim::angle_between(*old, b.heading()) / STEP;
            // Camera: altitude slack. Bird: the dodge allowance.
            let limit =
                tune.turn * if i == 0 { sim::CAM_SLACK } else { 1.0 + sim::DODGE_TURN * 1.6 };
            run.turn_ratio = run.turn_ratio.max(swing / limit);
            let sin_g = (b.vel.y / s.max(1e-3)).clamp(-1.0, 1.0);
            run.steepest = run.steepest.max(sin_g.asin().abs().to_degrees());
        }
        was = sim.birds.iter().map(|b| b.heading()).collect();
        run.clearance = run.clearance.min(sim.clearance(tune.blobs, false));
        run.cam_clearance = run.cam_clearance.min(sim.clearance(tune.blobs, true));

        let d_yaw = sim::angle_between(look, sim.look) / STEP;
        let d_roll = (sim.view_roll - roll).abs() / STEP;
        // Pitch on its own: the elevation of the look direction. The `yaw`
        // above is the whole swing, pitch included, and is kept that way so it
        // is the same number card 168 reported.
        let elev = |d: V3| (d.y / d.len().max(1e-6)).clamp(-1.0, 1.0).asin();
        let d_pitch = (elev(sim.look) - elev(look)).abs() / STEP;
        look = sim.look;
        roll = sim.view_roll;
        run.yaw.push(d_yaw.to_degrees());
        run.roll.push(d_roll.to_degrees());
        run.pitch.push(d_pitch.to_degrees());
        run.tilt.push(elev(sim.look).to_degrees());

        let now = i as f32 * STEP;
        let course = bearing(sim.course);
        if wrap(course - marked).abs() > 20.0_f32.to_radians() {
            run.turns.push(now - marked_at);
            marked = course;
            marked_at = now;
        }

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
            run.spread.push(sim.spread);
            run.alt.push(sim.centre.y);
            let flock = sim.flock();
            run.climb
                .push(flock.iter().map(|b| b.vel.y).sum::<f32>() / flock.len().max(1) as f32);
        }
        if i % 120 == 0 {
            // Nearest neighbour, every bird, every two seconds. O(n^2) at this
            // rate is nothing, and the whole point is the distribution.
            let flock = sim.flock();
            for (j, b) in flock.iter().enumerate() {
                let mut best = f32::INFINITY;
                for (k, o) in flock.iter().enumerate() {
                    if j != k {
                        best = best.min(b.pos.sub(o.pos).len());
                    }
                }
                if best.is_finite() {
                    run.nn.push(best);
                }
            }
            let out: Vec<f32> = flock.iter().map(|b| b.pos.sub(sim.centre).len()).collect();
            run.furthest.push(out.iter().copied().fold(0.0, f32::max));
            let lost = out.iter().filter(|d| **d > 20.0).count() as f32;
            run.lost.push(lost);
        }
        if i % 60 == 0 {
            if std::env::var_os("FLOCK_PATH").is_some() {
                eprintln!(
                    "PATH {seed} {:.1} {:.2} {:.2} {:.2}",
                    sim.t, sim.centre.x, sim.centre.y, sim.centre.z
                );
            }
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
    // Three seeds, not one. The flight is a chaotic system: a small change to
    // any parameter gives a completely different trajectory, so a promise
    // checked on a single seed is a promise about one trajectory. Tuning
    // against one seed is how `near` was very nearly shipped at a value that
    // happened to suit seed 11 and flew the camera into an obstacle on others.
    for seed in [11, 29, 404] {
        one_flight(seed, &tune, birds);
    }
}

fn one_flight(seed: u64, tune: &Tuning, birds: usize) {
    let run = fly(seed, 600.0, tune, birds);

    let in_min = *run.in_frame.iter().min().expect("samples");
    let in_med = median_usize(&run.in_frame);
    eprintln!(
        "flock, 10 min, {birds} birds, seed {seed}:\n  \
         in frame  min {in_min}  median {in_med}  p05 {:.0}\n  \
         nearest   min {:.1} m  median {:.1} m   biggest bird median {:.1} LEDs, p95 {:.1}\n  \
         seat      median {:.1} m from the centroid, p95 {:.1} m; flock {:.1} m across\n  \
         view      yaw p95 {:.1} deg/s (max {:.1}), roll p95 {:.2} deg/s (max {:.2})\n  \
         limits    speed x{:.3}, turn rate x{:.3}, steepest {:.0} deg, \
         blob clearance {:.1} m (camera {:.1} m)",
        percentile(&run.in_frame.iter().map(|n| *n as f32).collect::<Vec<_>>(), 0.05),
        run.nearest.iter().copied().fold(f32::INFINITY, f32::min),
        percentile(&run.nearest, 0.5),
        percentile(&run.biggest, 0.5),
        percentile(&run.biggest, 0.95),
        percentile(&run.seat, 0.5),
        percentile(&run.seat, 0.95),
        percentile(&run.spread, 0.5),
        percentile(&run.yaw, 0.95),
        percentile(&run.yaw, 1.0),
        percentile(&run.roll, 0.95),
        percentile(&run.roll, 1.0),
        run.speed_ratio,
        run.turn_ratio,
        run.steepest,
        run.clearance,
        run.cam_clearance,
    );

    // Card 177's three questions, as numbers.
    let (nn_mean, nn_cv) = mean_cv(&run.nn);
    let (gap_mean, gap_cv) = mean_cv(&run.turns);
    let up = run.climb.iter().map(|v| v.abs()).collect::<Vec<_>>();
    eprintln!(
        "  spacing   nearest neighbour mean {nn_mean:.2} m, CV {nn_cv:.2}, \
         p05 {:.2} m, p95 {:.2} m, worst {:.1} m\n  \
         stragglers furthest bird median {:.1} m, p95 {:.1} m; more than 20 m out: \
         median {:.0}, worst {:.0}\n  \
         vertical  centroid altitude {:.1} .. {:.1} m (p05 {:.1}, p95 {:.1}), \
         climb rate |v_y| median {:.2}, p95 {:.2}, max {:.2} m/s\n  \
         turns     {} noticeable (20 deg) changes, gap mean {gap_mean:.1} s, CV {gap_cv:.2}, \
         p05 {:.1} s, p95 {:.1} s\n  \
         view      pitch p95 {:.2} deg/s (max {:.2}); points {:.1} .. {:.1} deg off level \
         (p05 {:.1}, p95 {:.1})",
        percentile(&run.nn, 0.05),
        percentile(&run.nn, 0.95),
        percentile(&run.nn, 1.0),
        percentile(&run.furthest, 0.5),
        percentile(&run.furthest, 0.95),
        percentile(&run.lost, 0.5),
        percentile(&run.lost, 1.0),
        run.alt.iter().copied().fold(f32::INFINITY, f32::min),
        run.alt.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        percentile(&run.alt, 0.05),
        percentile(&run.alt, 0.95),
        percentile(&up, 0.5),
        percentile(&up, 0.95),
        percentile(&up, 1.0),
        run.turns.len(),
        percentile(&run.turns, 0.05),
        percentile(&run.turns, 0.95),
        percentile(&run.pitch, 0.95),
        percentile(&run.pitch, 1.0),
        run.tilt.iter().copied().fold(f32::INFINITY, f32::min),
        run.tilt.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        percentile(&run.tilt, 0.05),
        percentile(&run.tilt, 0.95),
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

    assert!(in_min >= 15, "the flock left the frame: only {in_min} birds in shot at worst");
    assert!(in_med >= 35, "median {in_med} birds in frame is not a flock");
    assert!(percentile(&run.seat, 0.95) <= 22.0, "the camera lost its seat: {:.1} m at p95", percentile(&run.seat, 0.95));
    assert!(percentile(&run.nearest, 0.5) >= 4.0, "the camera rides too close: {:.1} m", percentile(&run.nearest, 0.5));
    assert!(run.clearance >= 0.0, "a bird was {:.2} m inside a blob", -run.clearance);
    assert!(run.cam_clearance >= 0.0, "the camera was {:.2} m inside a blob", -run.cam_clearance);
    assert!(run.speed_ratio <= 1.02, "speed went x{:.3} outside its band", run.speed_ratio);
    assert!(run.turn_ratio <= 1.02, "a bird turned at x{:.3} of its limit", run.turn_ratio);
    assert!(percentile(&run.yaw, 0.95) <= 22.0, "the view yaws at {:.1} deg/s", percentile(&run.yaw, 0.95));
    assert!(percentile(&run.yaw, 1.0) <= 26.0, "the view swung at {:.1} deg/s", percentile(&run.yaw, 1.0));
    assert!(percentile(&run.roll, 0.95) <= 6.0, "the view rolls at {:.2} deg/s", percentile(&run.roll, 0.95));
    assert!(worst.0 < 0.6, "the flight repeats itself: r={:.2} at {} s", worst.0, worst.1);

    // Card 177's three, in the same order the author said them.
    assert!(
        nn_cv >= 0.30,
        "the flock is a lattice again: nearest-neighbour CV {nn_cv:.2} (it was 0.10-0.16 \
         before card 177, and a live flock is 0.4-0.6)"
    );
    assert!(
        percentile(&run.lost, 0.95) <= 3.0,
        "the flock is coming apart rather than clumping: {:.0} birds more than 20 m out at p95",
        percentile(&run.lost, 0.95)
    );
    assert!(
        percentile(&up, 0.95) >= 2.4,
        "there is not enough vertical motion: the flock's climb rate is only {:.2} m/s at p95 \
         (it was 2.1-2.3 before card 177)",
        percentile(&up, 0.95)
    );
    assert!(
        run.steepest <= 70.0,
        "something went down at {:.0} degrees - the climb spring is being overwhelmed",
        run.steepest
    );
    assert!(
        percentile(&run.pitch, 0.95) <= 8.0 && percentile(&run.pitch, 1.0) <= 20.0,
        "the view pitches too fast to watch: p95 {:.1} deg/s, max {:.1}",
        percentile(&run.pitch, 0.95),
        percentile(&run.pitch, 1.0)
    );
    assert!(
        gap_cv >= 0.8,
        "the flock changes its mind on a schedule: the gaps between 20-degree course changes \
         have a CV of only {gap_cv:.2}"
    );
}

/// Card 122: the author wants a handful of birds, not thirty. `birds` now goes
/// down to 3 - so a flock that small has to still be a flock, not three
/// strangers that happen to share a sky. Ten minutes, three seeds, the
/// shipped default `near` (the personal-space wall that keeps the camera from
/// scattering a small flock does not depend on how many birds there are).
///
/// The 55-bird test's own thresholds (`in_med >= 35`, `nn_cv >= 0.30`, ...)
/// are sized for a flock that big and do not apply here - three birds cannot
/// have a broad nearest-neighbour distribution, and "median 35 in frame" is
/// meaningless when there are only 3 to begin with. What has to hold at any
/// size is the physics (speed and turn stay inside their bands, nobody flies
/// into the invisible geometry) and the picture (most of the flock stays on
/// the panel, and it does not balloon into a scattered mess).
#[test]
fn the_smallest_flock_still_flocks() {
    let birds = 3;
    assert_eq!(
        param_spec("birds").min,
        birds as f32,
        "this test assumes `birds`'s minimum is what it is measuring"
    );
    let tune = Tuning::of(0.90, 6.0, 0.8, 2.4); // the shipped default `near`
    for seed in [11u64, 29, 404] {
        let run = fly(seed, 600.0, &tune, birds);
        let in_min = *run.in_frame.iter().min().expect("samples");
        let in_med = median_usize(&run.in_frame);
        let spread = percentile(&run.spread, 0.5);
        eprintln!(
            "flock, 10 min, {birds} birds (the minimum), seed {seed}: \
             in frame min {in_min} median {in_med}, seat p95 {:.1} m, spread {spread:.1} m, \
             speed x{:.3}, turn rate x{:.3}, clearance {:.1} m (camera {:.1} m)",
            percentile(&run.seat, 0.95),
            run.speed_ratio,
            run.turn_ratio,
            run.clearance,
            run.cam_clearance,
        );

        assert!(in_min >= 1, "the whole flock left the panel at once");
        assert!(in_med >= birds - 1, "median {in_med} of {birds} in frame is not much of a flock");
        assert!(run.clearance >= 0.0, "a bird was {:.2} m inside a blob", -run.clearance);
        assert!(run.cam_clearance >= 0.0, "the camera was {:.2} m inside a blob", -run.cam_clearance);
        assert!(run.speed_ratio <= 1.02, "speed went x{:.3} outside its band", run.speed_ratio);
        assert!(run.turn_ratio <= 1.02, "a bird turned at x{:.3} of its limit", run.turn_ratio);
        assert!(spread <= 10.0, "three birds spread {spread:.1} m apart - that is scattered, not a flock");
    }
}

fn param_spec(id: &str) -> &'static ParamSpec {
    PARAMS.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("no such param `{id}`"))
}

// ----------------------------------------------------------------------
// The picture
// ----------------------------------------------------------------------

/// The filled triangle a wing's surface is made of covers the area it says it
/// does, whichever way round its corners are given, and a wing the view has
/// turned exactly edge-on covers nothing at all.
#[test]
fn a_filled_triangle_covers_its_own_area() {
    let corners = [(10.0, 10.0), (26.0, 10.0), (10.0, 22.0)];
    let want = 0.5 * 16.0 * 12.0;
    for order in [[0, 1, 2], [0, 2, 1]] {
        let mut cover = Coverage::new(SUPERSAMPLE);
        cover.triangle(corners[order[0]], corners[order[1]], corners[order[2]], 1.0);
        let area: f32 = (0..N).map(|i| cover.pixel(i % W, i / W)).sum();
        eprintln!("triangle {order:?}: {area:.2} of {want} square LEDs");
        assert!(
            (area - want).abs() < 0.05 * want,
            "a {want} LED triangle covered {area:.2}, whichever way round its corners are"
        );
    }

    // Three points on a line, which is what a wing with no chord left is.
    let mut cover = Coverage::new(SUPERSAMPLE);
    cover.triangle((4.0, 4.0), (12.0, 8.0), (20.0, 12.0), 1.0);
    let area: f32 = (0..N).map(|i| cover.pixel(i % W, i / W)).sum();
    assert_eq!(area, 0.0, "a degenerate triangle put {area} LEDs of ink down");
}

/// **The wing folds on the upstroke.** The hand wing sweeps back and draws in
/// through the quick half of the beat, so the bird is visibly narrower going
/// up than it is coming down - the single strongest cue that this is a wing
/// beating and not a pair of rods rocking.
///
/// Measured where it shows: the *drawn* tip-to-tip span, in LEDs, projected
/// through a camera looking at the bird's planform from below. The two
/// moments are the extremes of the wing's own vertical speed - phase 0 is
/// mid-upstroke, phase pi mid-downstroke - so this is the worst case for the
/// claim, not a flattering pair.
#[test]
fn the_wing_is_narrower_going_up_than_coming_down() {
    let mut sim = Sim::new(3);
    sim.resize(2);
    let span = sim::SPAN * 2.5;

    // Straight and level, flying along +x, with the camera 6 m below it.
    let mut b = sim.flock()[0];
    b.pos = v3(0.0, 0.0, 0.0);
    b.vel = v3(5.0, 0.0, 0.0);
    b.roll = 0.0;
    b.glide = 0.0;
    let eye = v3(0.0, -6.0, 0.0);
    let fwd = v3(0.0, 1.0, 0.0);
    let right = v3(1.0, 0.0, 0.0);
    let view = View {
        eye,
        right,
        up: right.cross(fwd),
        fwd,
        focal: 0.5 * W as f32 / (0.5 * FOV.to_radians()).tan(),
        sun: v3(0.0, 0.0, 0.0),
    };
    let drawn = |phase: f32| {
        let mut b = b;
        b.phase = phase;
        let pose = bird::pose(b, span, 1.0);
        let tip = |w: &bird::Wing| view.project(w.spar[2]).expect("a tip in front of the lens");
        let (l, r) = (tip(&pose.wings[0]), tip(&pose.wings[1]));
        ((l.0 - r.0).powi(2) + (l.1 - r.1).powi(2)).sqrt()
    };
    let (up, down) = (drawn(0.0), drawn(std::f32::consts::PI));
    eprintln!("drawn span: {up:.1} LEDs mid-upstroke, {down:.1} mid-downstroke");
    assert!(
        up < 0.85 * down,
        "the wrist is not folding: {up:.1} LEDs across mid-upstroke against {down:.1} \
         mid-downstroke, and a real bird draws its hand wing in on the way up"
    );
}

/// A bird too far away to have a wing's surface is the bird card 168 drew,
/// and a near one has a body that belongs to its wings.
///
/// The level of detail is a fade, not a switch between two models, so this is
/// what its two ends mean. Far: the trailing edge sits on the leading edge and
/// the tail fan has no width, every triangle is degenerate and draws nothing,
/// and the spine is card 168's **0.70 of a wingspan** from beak to tail - the
/// long dart that is the only thing left saying "flying" once the wings are a
/// pixel each. Near: the same spine has drawn in to a real bird's proportion,
/// because at sixteen LEDs seen from behind - the commonest view there is,
/// with the camera inside the flock - the body carries the whole silhouette
/// and a dart's body makes a dart.
#[test]
fn a_distant_bird_has_no_surface_left() {
    let mut sim = Sim::new(3);
    sim.resize(2);
    let length = |p: &bird::Pose| p.spine[0].sub(p.spine[4]).len() / sim::SPAN;

    let far = bird::pose(sim.flock()[0], sim::SPAN, 0.0);
    assert_eq!(far.tail[0].sub(far.tail[1]).len(), 0.0, "the tail fan still has width");
    for w in &far.wings {
        assert_eq!(w.trail[0].sub(w.spar[0]).len(), 0.0, "the wing root still has chord");
        assert_eq!(w.trail[1].sub(w.spar[1]).len(), 0.0, "the wrist still has chord");
    }
    assert!(
        (length(&far) - 0.70).abs() < 1e-4,
        "a distant bird is {:.3} of a wingspan long; card 168's dart is 0.70",
        length(&far)
    );

    let near = bird::pose(sim.flock()[0], sim::SPAN, 1.0);
    eprintln!("body: {:.3} of a span far, {:.3} near", length(&far), length(&near));
    assert!(
        (0.44..=0.52).contains(&length(&near)),
        "a near bird is {:.3} of a wingspan long, and a gull is about 0.46",
        length(&near)
    );

    assert_eq!(
        smoothstep(AREA.0, AREA.1, 4.9),
        0.0,
        "the surface is being faded in below the span it is meant to start at"
    );
}

/// **The birds visibly lean into their turns**, and the lean is a drawing.
///
/// Two claims, one flight each, at the settings the author actually watches
/// (card 124: six birds, the shipped `calm` and `wild`, seed 7).
///
/// 1. At `lean` 1 the drawn bank reaches something the eye can see. The honest
///    roll tops out around seven degrees here - a little over one LED of
///    difference across a sixteen-LED wing - and that is the whole reason this
///    card exists.
/// 2. At `lean` 0 the drawn bank *is* the honest roll, to within the easing.
///    That is what the parameter's zero end promises.
#[test]
fn birds_lean_into_their_turns() {
    let over = |lean: f32| {
        let mut sim = Sim::new(7);
        sim.resize(6);
        let tune = Tuning { lean, ..Tuning::default() };
        let (mut roll, mut drawn, mut wrong) = (0.0_f32, 0.0_f32, 0.0_f32);
        for i in 0..(100.0 / STEP) as usize {
            sim.step(&tune, STEP);
            // `Sim::new` settles the flock on the default tuning, so the first
            // moment of a run still carries the lean that warm-up left behind.
            // Two seconds is several time constants of it.
            if i < (2.0 / STEP) as usize {
                continue;
            }
            for b in sim.flock() {
                roll = roll.max(b.roll.abs());
                drawn = drawn.max(b.lean.abs());
                // A bird that leans the *other* way out of a turn is a bird
                // falling over, so the two never disagree about the direction.
                if b.roll * b.lean < 0.0 {
                    wrong = wrong.max(b.lean.abs().min(b.roll.abs()));
                }
            }
        }
        eprintln!(
            "lean {lean}: honest roll up to {:.1} deg, drawn {:.1} deg, \
             worst disagreement {:.2} deg",
            roll.to_degrees(),
            drawn.to_degrees(),
            wrong.to_degrees()
        );
        (roll.to_degrees(), drawn.to_degrees(), wrong.to_degrees())
    };

    let (roll, drawn, wrong) = over(1.0);
    assert!(roll < 12.0, "the flight itself has got wilder: {roll:.1} deg of honest roll");
    assert!(drawn > 25.0, "the birds only lean {drawn:.1} deg, which is not something you see");
    // The ease lags the roll, so the two cross zero a little apart; a couple of
    // degrees of that is the lag and not a bird leaning out of its own turn.
    assert!(wrong < 3.0, "a bird leaned {wrong:.1} deg the wrong way out of a turn");

    let (roll, drawn, _) = over(0.0);
    assert!(
        (drawn - roll).abs() < 1.0,
        "`lean` 0 is meant to be the honest roll: {drawn:.1} deg drawn against {roll:.1}"
    );
}

/// ...and it is a **drawing** quantity: nothing about where a bird goes can
/// depend on it.
///
/// The same seed flown twice, once with the lean off and once at twice what
/// anyone can ask for, is the same flight to the last bit - which is what makes
/// the exaggeration safe to have at all, and why `ten_minutes_of_flight` and
/// the view's promises cannot be moved by this control.
#[test]
fn the_lean_never_touches_the_flight() {
    let fly = |lean: f32| {
        let mut sim = Sim::new(7);
        sim.resize(24);
        let tune = Tuning { lean, ..Tuning::default() };
        for _ in 0..(60.0 / STEP) as usize {
            sim.step(&tune, STEP);
        }
        sim.flock().iter().map(|b| (b.pos, b.vel, b.roll, b.phase)).collect::<Vec<_>>()
    };
    let (off, on) = (fly(0.0), fly(2.0));
    for (i, (a, b)) in off.iter().zip(&on).enumerate() {
        assert_eq!(
            (a.0.x, a.0.y, a.0.z, a.1.x, a.1.y, a.1.z, a.2, a.3),
            (b.0.x, b.0.y, b.0.z, b.1.x, b.1.y, b.1.z, b.2, b.3),
            "bird {i} flew somewhere else with the lean turned up"
        );
    }
}

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

/// **The far corner of both controls**, which is not a picture but is a place
/// the sliders go: 150 birds each drawn three times life size (card 124).
///
/// Card 123 left this corner two bytes inside the budget and nothing testing
/// it. Flown properly - four backdrops, three seeds, a minute each - it is
/// *over*: a few frames in a hundred cannot be encoded exactly and take the
/// encoder's lossy fallback, and this card's banked wings, which show more area
/// than an edge-on one, made that a little commoner (seed 11, dusk, 1800
/// frames: 10 lossy before, 94 after). It is deliberately **not** in
/// [`every_frame_goes_out_exactly`], because the honest answer is not "this is
/// exact" - it is "at 150 birds at size 3 the panel is a wall of overlapping
/// wings with no composition left in it at all (see the card's
/// `corner-150-3.png`), the picture is already noise, and a frame in fifty
/// being approximated there is the fallback doing its job".
///
/// What must still hold, and is what this checks: every frame **fits the
/// datagram**, the limiter never has to pull the picture down, and the
/// approximation stays rare rather than becoming the normal case. The corner
/// starts at 150 birds: at 110 birds drawn the same size, and at 150 birds at
/// `size` 2, every frame of the same runs was exact.
#[test]
fn the_far_corner_of_both_controls_still_fits() {
    for (backdrop, scheme, name) in [
        (0.0, 0.0, "sky, light on dark"),
        (0.0, 1.0, "sky, dusk silhouettes"),
        (1.0, 0.0, "horizon line"),
        (2.0, 0.0, "black, a wall of big white birds"),
    ] {
        let mut params = Params::defaults(PARAMS);
        params.set(PARAMS, "backdrop", backdrop);
        params.set(PARAMS, "scheme", scheme);
        params.set(PARAMS, "birds", param_spec("birds").max);
        params.set(PARAMS, "size", param_spec("size").max);
        if scheme > 0.5 {
            params.set(PARAMS, "hue", 35.0);
            params.set(PARAMS, "spread", 95.0);
        }
        let mut patch = (DEF.make)(11);
        let mut pipeline = crate::Pipeline::new(crate::pipeline::Output::default());
        let dt = 1.0 / 30.0;
        let frames = 1800;
        let (mut worst, mut lossy, mut apl, mut gain) = (0, 0, 0.0_f32, 1.0_f32);
        for i in 0..frames {
            let out = pipeline.process(
                patch.render(&Ctx { t: f64::from(i) * dt, dt, now: 0.0, params: &params }),
                dt,
            );
            worst = worst.max(out.stats.encoded_bytes);
            apl = apl.max(out.stats.apl);
            if i > 30 {
                gain = gain.min(out.stats.limiter_gain);
            }
            lossy += u32::from(!out.stats.exact);
        }
        eprintln!(
            "{name} at both extremes: {frames} frames, worst {worst} of {} bytes, \
             peak APL {:.0}%, limiter down to x{gain:.2}, {lossy} approximated",
            crate::meter::PAYLOAD_BYTES,
            apl * 100.0,
        );
        assert!(worst <= crate::meter::PAYLOAD_BYTES, "{name}: {worst} bytes is over the datagram");
        assert!(gain > 0.95, "{name}: the limiter had to pull the picture down to x{gain:.2}");
        assert!(
            lossy * 10 < frames,
            "{name}: {lossy} of {frames} frames approximated - the fallback has become the \
             normal case, which is a different question from this corner being tight"
        );
    }
}

/// A minute of each picture the patch can draw, straight through the real
/// pipeline and measured by the real encoder: **every frame is exact**.
///
/// The two *schemes* cost the same because they are the same index image with
/// a different set of colours in front of it - which is the whole point of
/// making the palette two-dimensional; only the birds' contrast floor differs,
/// so the counts are close rather than identical. The two backdrops without a
/// sky cost far less, because most of the index plane is one value.
/// **Card 123**: the big birds have filled wings now, so every backdrop is
/// flown twice - once at the defaults and once at the picture the author
/// actually asked card 123 for, a handful of birds drawn as large as `size`
/// goes. Black sky with big light birds is the case that matters for the
/// panel: it is the most lit area this patch can put on it while the frame is
/// still nearly all black, and the limiter has to still be the thing deciding
/// that, not the drawing.
#[test]
fn every_frame_goes_out_exactly() {
    for (backdrop, scheme, birds, size, name) in [
        (0.0, 0.0, 55.0, 1.0, "sky, light on dark"),
        (0.0, 1.0, 55.0, 1.0, "sky, dusk silhouettes"),
        (1.0, 0.0, 55.0, 1.0, "horizon line"),
        (2.0, 0.0, 55.0, 1.0, "black"),
        (0.0, 0.0, 6.0, 3.0, "sky, six birds as big as they go"),
        (0.0, 1.0, 6.0, 3.0, "dusk silhouettes, six birds as big as they go"),
        (1.0, 0.0, 6.0, 3.0, "horizon line, six birds as big as they go"),
        (2.0, 0.0, 6.0, 3.0, "black, six big white birds"),
    ] {
        let mut params = Params::defaults(PARAMS);
        params.set(PARAMS, "backdrop", backdrop);
        params.set(PARAMS, "scheme", scheme);
        params.set(PARAMS, "birds", birds);
        params.set(PARAMS, "size", size);
        if scheme > 0.5 {
            // The dusk sky wants a warm horizon; see the README.
            params.set(PARAMS, "hue", 35.0);
            params.set(PARAMS, "spread", 95.0);
        }
        let mut patch = (DEF.make)(11);
        let mut pipeline = crate::Pipeline::new(crate::pipeline::Output::default());
        let dt = 1.0 / 30.0;
        let (mut worst, mut lossy, mut colours, mut apl, mut gain) = (0, 0, 0, 0.0_f32, 1.0_f32);
        for i in 0..1800 {
            let out = pipeline.process(
                patch.render(&Ctx { t: f64::from(i) * dt, dt, now: 0.0, params: &params }),
                dt,
            );
            worst = worst.max(out.stats.encoded_bytes);
            colours = colours.max(out.stats.distinct_colours);
            apl = apl.max(out.stats.apl);
            // Not the opening second: a lit sky arriving out of black is a
            // luminance rise, the limiter holds it down for a few frames, and
            // that is the limiter working rather than anything about the
            // picture. What matters is whether it ever bites once flying.
            if i > 30 {
                gain = gain.min(out.stats.limiter_gain);
            }
            lossy += u32::from(!out.stats.exact);
        }
        eprintln!(
            "{name}: 1800 frames, worst {worst} of {} bytes, up to {colours} colours, \
             peak APL {:.0}%, limiter down to x{gain:.2} after the first second, {lossy} lossy",
            crate::meter::PAYLOAD_BYTES,
            apl * 100.0,
        );
        assert_eq!(lossy, 0, "{name}: {lossy} frames of 1800 could not be sent exactly");
        assert!(colours as usize <= BANDS * ink_levels(backdrop as usize), "{name}: more colours than the palette has");
        assert!(worst < crate::meter::PAYLOAD_BYTES, "{name}: {worst} bytes leaves no headroom");
        assert!(gain > 0.95, "{name}: the limiter had to pull the picture down to x{gain:.2}");
    }
}

/// The flight is the same at any render rate.
///
/// The simulation runs on a fixed 1/60 s step and the step count comes from
/// the total simulated time, floored with a small bias - so 30 fps and 60 fps
/// agree on the integer at a step boundary even though their floating-point
/// sums differ in the last bits. Without the bias they disagree by one step
/// roughly every other frame and the two runs drift apart.
#[test]
fn the_flight_does_not_depend_on_the_frame_rate() {
    let params = Params::defaults(PARAMS);
    let at = |fps: f64| {
        let mut patch = (DEF.make)(5);
        let dt = 1.0 / fps;
        let mut frame = Frame::black();
        for i in 1..=(60.0 * fps) as usize {
            frame = patch.render(&Ctx { t: i as f64 * dt, dt, now: 0.0, params: &params });
        }
        match frame {
            Frame::Indexed { palette, indices } => {
                indices.iter().map(|i| palette[*i as usize]).collect::<Vec<_>>()
            }
            Frame::Linear(px) => px,
        }
    };
    let (a, b) = (at(30.0), at(60.0));
    let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    eprintln!("a minute flown at 30 and at 60 fps: {differ} of {} pixels differ", a.len());
    // A minute of flight, 3600 fixed steps, from two different arrival
    // patterns: the same steps happen, so the same picture comes out.
    assert_eq!(differ, 0, "the flight drifted apart between 30 and 60 fps");
}


