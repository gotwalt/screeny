//! What the whole patch promises, checked end to end: the same seed and
//! moment draw the same picture, and a long run keeps every skeleton inside
//! the box and clear of one another. `rig::tests` has the FK sanity
//! (lengths), `actions::tests` has the pose sanity, `actor::tests` has the
//! motif variety over a long run.

use super::*;
use crate::patch::{Clock, Params};
use crate::snapshot::{self, Shot};

#[test]
fn a_render_is_deterministic_for_the_same_seed_and_moment() {
    let params = Params::defaults(DEF.params);
    let shot = Shot { seed: 7, at: 14.0, warmup: 14.0, clock: Clock::Pinned(0.0), output: crate::pipeline::Output::default() };
    let a = snapshot::take(&DEF, &params, &shot);
    let b = snapshot::take(&DEF, &params, &shot);
    assert_eq!(a.preview, b.preview, "the same seed and moment should paint the same picture");
}

#[test]
fn another_seed_draws_a_different_box() {
    let params = Params::defaults(DEF.params);
    let shot = |seed| Shot { seed, at: 14.0, warmup: 14.0, clock: Clock::Pinned(0.0), output: crate::pipeline::Output::default() };
    let a = snapshot::take(&DEF, &params, &shot(1));
    let b = snapshot::take(&DEF, &params, &shot(999_983));
    assert_ne!(a.preview, b.preview);
}

/// Ten minutes, three skeletons: nobody ever leaves the walkable floor and
/// nobody ever passes through anybody else. The one long run this patch is
/// measured by, in the spirit of `flock`'s.
#[test]
fn a_long_run_keeps_everyone_in_the_box_and_apart() {
    let mut world = actor::World::new(2026);
    const STEPS: i64 = 30 * 60 * 10;
    for step in 1..=STEPS {
        let t = step as f64 * STEP;
        world.step(t, 3, 2026);
        for a in &world.actors {
            assert!((-box_scene::WALK_HALF_W - 0.02..=box_scene::WALK_HALF_W + 0.02).contains(&a.pos.0), "x {} out of bounds at step {step}", a.pos.0);
            assert!((box_scene::WALK_NEAR_Z - 0.02..=box_scene::WALK_FAR_Z + 0.02).contains(&a.pos.1), "z {} out of bounds at step {step}", a.pos.1);
        }
        for i in 0..world.actors.len() {
            for j in (i + 1)..world.actors.len() {
                let (ax, az) = world.actors[i].pos;
                let (bx, bz) = world.actors[j].pos;
                let d = (ax - bx).hypot(az - bz);
                assert!(d >= actor::MIN_SEP - 0.02, "skeletons {i} and {j} overlapped at step {step}: {d}");
            }
        }
    }
}
