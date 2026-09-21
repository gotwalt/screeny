//! One frame of a patch, rendered through the whole pipeline: what
//! `screeny-art snapshot` writes a PNG of.
//!
//! It lives here rather than in the binary so that the thing the tests check
//! is the thing the command runs. The run is simulated, not paced: the patch
//! is stepped at 30 fps from the start of its warmup up to the wanted moment,
//! and its time of day comes from a [`Clock`] the caller chooses, so a
//! time-telling patch can be aimed at 21:12 and answer the same picture
//! whenever the command is typed (card 162).

use crate::patch::{Clock, Ctx, Params, PatchDef};
use crate::pipeline::{self, Output, Pipeline};

/// Frames a second the run is stepped at. Stateful patches and the limiter are
/// then where a live run would have put them.
///
/// Card 161: this is [`crate::FPS`], re-exported rather than written down
/// twice. The snapshot has always stepped at 30 - it is now the only rate
/// there is.
pub use crate::FPS;

/// Which frame of which run to keep.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub seed: u64,
    /// Engine time of the frame that is kept, in seconds.
    pub at: f64,
    /// How many seconds of the run before `at` are actually rendered. Patches
    /// with long-lived state - anything that settles, drifts or remembers -
    /// want all of it: `warmup >= at`, so the run starts at engine time zero.
    pub warmup: f64,
    /// Where the run's time of day comes from.
    pub clock: Clock,
    pub output: Output,
}

impl Default for Shot {
    fn default() -> Self {
        Shot { seed: 1, at: 5.0, warmup: 2.0, clock: Clock::Live, output: Output::default() }
    }
}

/// Render `def` and return the pipeline's output for the frame at [`Shot::at`].
///
/// Deterministic for a pinned clock: nothing here reads the machine's clock,
/// the step is fixed, and the patch's own randomness comes from `seed`.
#[must_use]
pub fn take(def: &PatchDef, params: &Params, shot: &Shot) -> pipeline::Processed {
    take_acting(def, params, shot, None)
}

/// [`take`], with one of the patch's own actions pressed part way through.
///
/// `act` is `(id, when)`: the action [`Patch::playing`] offers, and the engine
/// time - the same axis as [`Shot::at`] - at the start of the first frame that
/// should see it. A patch whose interesting behaviour is a *response* cannot
/// be photographed otherwise; `screeny-art snapshot --act` is this.
#[must_use]
pub fn take_acting(def: &PatchDef, params: &Params, shot: &Shot, act: Option<(&str, f64)>) -> pipeline::Processed {
    let mut patch = (def.make)(shot.seed);
    let mut pipeline = Pipeline::new(shot.output);
    let dt = 1.0 / FPS;
    let first = (shot.at - shot.warmup).max(0.0);
    let steps = ((shot.at - first) / dt).round() as usize;
    // The time of day is simulated too, so a patch that tells the time sees a
    // consistent clock however fast this runs. An unpinned clock is read once,
    // here, and carried forward exactly as it was before card 162.
    let began = shot.clock.now(first);
    let mut result = None;
    let mut pressed = false;
    for i in 0..=steps {
        let t = first + i as f64 * dt;
        if let Some((id, when)) = act {
            if !pressed && t >= when {
                patch.act(id);
                pressed = true;
            }
        }
        let frame = patch.render(&Ctx { t, dt, now: began + (t - first), params });
        result = Some(pipeline.process(frame, dt));
    }
    result.expect("at least one frame")
}
