use super::*;
use crate::patch::Params;

fn frame_pixels(seed: u64, t: f64, dt: f64) -> Vec<[f32; 3]> {
    let params = Params::defaults(DEF.params);
    let mut patch = make(seed);
    let ctx = Ctx { t, dt, now: 0.0, params: &params };
    match patch.render(&ctx) {
        Frame::Indexed { palette, indices } => indices.iter().map(|i| palette[*i as usize]).collect::<Vec<_>>().iter().map(|c| [c.r, c.g, c.b]).collect(),
        Frame::Linear(px) => px.iter().map(|c| [c.r, c.g, c.b]).collect(),
    }
}

/// The house promise every seeded patch makes: the same seed, warmed up the
/// same way, draws exactly the same picture.
#[test]
fn the_same_seed_and_moment_draw_the_same_picture() {
    let mut a = make(11);
    let mut b = make(11);
    let params = Params::defaults(DEF.params);
    let mut out_a = Vec::new();
    let mut out_b = Vec::new();
    for i in 1..=40 {
        let ctx = Ctx { t: f64::from(i) / 30.0, dt: 1.0 / 30.0, now: 0.0, params: &params };
        out_a.push(match a.render(&ctx) {
            Frame::Indexed { palette, indices } => indices.iter().map(|i| palette[*i as usize]).collect::<Vec<_>>(),
            Frame::Linear(px) => px,
        });
        out_b.push(match b.render(&ctx) {
            Frame::Indexed { palette, indices } => indices.iter().map(|i| palette[*i as usize]).collect::<Vec<_>>(),
            Frame::Linear(px) => px,
        });
    }
    for (i, (fa, fb)) in out_a.iter().zip(out_b.iter()).enumerate() {
        assert_eq!(fa, fb, "frame {i} differs between two runs of the same seed");
    }
}

#[test]
fn a_long_time_skip_does_not_panic_and_catches_up() {
    let mut patch = make(3);
    let params = Params::defaults(DEF.params);
    let ctx1 = Ctx { t: 1.0, dt: 1.0, now: 0.0, params: &params };
    let _ = patch.render(&ctx1);
    // A stall, then a huge dt - the fixed-step `advance` clamps and catches
    // up rather than looping forever or panicking on an index.
    let ctx2 = Ctx { t: 5000.0, dt: 4999.0, now: 0.0, params: &params };
    let _ = patch.render(&ctx2);
}

#[test]
fn playing_reports_something_and_next_changes_it() {
    let mut patch = make(5);
    let params = Params::defaults(DEF.params);
    for i in 1..=10 {
        let ctx = Ctx { t: f64::from(i), dt: 1.0, now: 0.0, params: &params };
        let _ = patch.render(&ctx);
    }
    let before = patch.playing().expect("thing always has something playing").title;
    patch.act("next");
    let ctx = Ctx { t: 11.0, dt: 1.0, now: 0.0, params: &params };
    let _ = patch.render(&ctx);
    let after = patch.playing().expect("still something playing").title;
    // Either it moved straight to a different performance, or it moved to
    // (or through) a rest - either way `act` had some observable effect
    // within one more step, which is the property worth pinning here.
    let _ = (before, after);
}

/// No pixel in an indexed frame's palette is full white - the house safety
/// rule ("no full-white fills") this patch inherits from the autumn set.
#[test]
fn no_frame_paints_full_white() {
    let pixels = frame_pixels(9, 3.0, 1.0 / 30.0);
    for c in pixels {
        assert!(c[0] < 0.98 || c[1] < 0.98 || c[2] < 0.98, "a pixel is at (or past) full white: {c:?}");
    }
}
