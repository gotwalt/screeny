//! Card 161: **every patch advances by time, not by frame count.**
//!
//! The rate went from 60 to 30, so a patch that stepped its own state once per
//! frame would now move at half speed - and nothing else in the system would
//! say so. This is the test that says so.
//!
//! The shape of it, per patch: render to the same engine time `T` twice, once
//! in 60 steps a second and once in 30, and compare the last frame with a
//! third run that takes the *same number of steps* as the 30 fps one but only
//! reaches `T/2` - which is exactly what a frame-counting patch would draw.
//! A patch that goes by time lands on the same picture at `T` however it got
//! there; a patch that counts frames lands on the half-time picture.
//!
//! `T/2` is a real difference for every patch here (asserted), so "the two
//! runs to `T` agree" is a statement rather than a tautology.
//!
//! The clock is pinned, so the patches that tell the time see the same seconds
//! in every run and the comparison is about motion rather than about when the
//! test happened to run.

use screeny_art::frame::{Frame, N};
use screeny_art::patch::{Ctx, Params, PatchDef};
use screeny_art::patches::{needs_gpu, ALL};

/// A moment on `local_now`'s scale, chosen so that **the minute rolls over at
/// engine time `T * 0.75`** - between the half-time run's end and the full
/// run's. 1700000000 is 20 s into a minute, so the next boundary is 40 s away.
///
/// It has to be between them. The clock patches hold a minute for a whole
/// minute, so two runs that both sit inside one draw the same frame and the
/// comparison below would be empty - which is what the `half_time` assertion
/// catches, and what this constant is for.
const CLOCK_AT: f64 = 1_700_000_000.0 + 40.0 - T * 0.75;

/// Engine seconds each run reaches. Long enough for the clock patches to have
/// planned and danced (their choreography runs for about 8 s), short enough to
/// keep the GPU patches quick.
const T: f64 = 6.0;

/// One run: `steps` steps of `dt`, ending at `steps as f64 * dt`. Returns the
/// last frame as plain sRGB-linear triples.
fn run(def: &PatchDef, seed: u64, dt: f64, steps: usize) -> Vec<[f32; 3]> {
    let params = Params::defaults(def.params);
    let mut patch = (def.make)(seed);
    let mut last = None;
    for i in 1..=steps {
        let t = i as f64 * dt;
        last = Some(patch.render(&Ctx { t, dt, now: CLOCK_AT + t, params: &params }));
    }
    let frame = last.expect("at least one frame");
    let px = match frame {
        Frame::Linear(px) => px,
        Frame::Indexed { palette, indices } => indices.iter().map(|i| palette[*i as usize]).collect(),
    };
    px.iter().map(|c| [c.r, c.g, c.b]).collect()
}

/// Mean absolute difference per channel, in linear light. 0 is byte-identical.
fn diff(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    assert_eq!(a.len(), N, "a frame is a frame");
    assert_eq!(b.len(), N);
    let total: f32 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x[0] - y[0]).abs() + (x[1] - y[1]).abs() + (x[2] - y[2]).abs())
        .sum();
    total / (N * 3) as f32
}

/// The patches whose picture is a pure function of `ctx.t` (and the clock and
/// the parameters): no integration, no accumulated state. They must agree
/// **exactly**, which is a much stronger statement than the tolerance below
/// and worth making separately - a patch that starts integrating would show up
/// here first.
///
/// Established by reading each one; see the card's Log for the per-patch note.
const PURELY_A_FUNCTION_OF_T: &[&str] = &["metaballs", "overland", "lattice", "knot"];

#[test]
fn every_patch_advances_by_time_and_not_by_frame_count() {
    let gpu = screeny_art::gpu_status().available;
    let mut checked = 0;
    let mut skipped = 0;
    for def in ALL {
        if needs_gpu(def.id) && !gpu {
            eprintln!("rate: skipping `{}`: no graphics adapter here", def.id);
            skipped += 1;
            continue;
        }
        let seed = 7;
        let fast = run(def, seed, 1.0 / 60.0, (T * 60.0) as usize);
        let slow = run(def, seed, 1.0 / 30.0, (T * 30.0) as usize);
        // What a frame-counting patch would have drawn at 30: the same number
        // of steps, half the time.
        let counted = run(def, seed, 1.0 / 60.0, (T * 30.0) as usize);

        let same_time = diff(&fast, &slow);
        let half_time = diff(&fast, &counted);

        println!("{:<16} 60 vs 30 = {same_time:.5}   60 vs half the time = {half_time:.5}", def.id);

        assert!(
            half_time > 0.002,
            "`{}`: half the time draws the same picture ({half_time:.5}), so this test proves nothing about it",
            def.id
        );
        assert!(
            same_time < half_time / 4.0,
            "`{}` moved with the frame rate: 60 vs 30 differs by {same_time:.5}, and 60 vs half the time \
             by only {half_time:.5}. A patch must step by `ctx.dt`, never once per frame (card 161)",
            def.id
        );
        if PURELY_A_FUNCTION_OF_T.contains(&def.id) {
            assert_eq!(
                same_time, 0.0,
                "`{}` is meant to be a pure function of `ctx.t`, so the two rates must draw the same frame exactly",
                def.id
            );
        }
        checked += 1;
    }
    // **Every patch this machine can draw was checked**, which is the statement
    // worth making and does not have to be re-tuned whenever the registry
    // changes size. It was `checked >= 6` until card 178 took two patches out
    // and left five on a bench with no adapter - a bound that would have gone
    // red for the wrong reason.
    assert_eq!(
        checked + skipped,
        ALL.len(),
        "{checked} checked and {skipped} skipped does not account for all {} patches",
        ALL.len()
    );
    assert!(checked >= 5, "only {checked} patches were checked; ALL has {}", ALL.len());
}

/// And the same statement about `screeny_art::FPS` itself: it is the one rate,
/// and it is the panel's.
#[test]
fn there_is_one_rate() {
    assert!((screeny_art::FPS - 30.0).abs() < f64::EPSILON, "the rate is 30 (card 161)");
    assert!(
        (screeny_art::snapshot::FPS - screeny_art::FPS).abs() < f64::EPSILON,
        "the snapshot steps at the live rate, and it is the same constant"
    );
}
