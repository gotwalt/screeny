//! The fractal's failure mode is not a crash, it is a picture that quietly
//! stops being one: a centre that drifts into smooth exterior and fades to a
//! flat colour, or an iteration budget too small for the depth so the frame
//! goes black. Both have happened during this card. These tests watch for
//! them at every target and every depth of the tour.

use screeny_demos::fractal::{self, FractalZoom};
use screeny_demos::frame::{Frame, Indexed, Piece};
use screeny_demos::panel;
use screeny_demos::stats;
use std::time::Duration;

/// Ages spread across a leg, including both ends.
fn ages(z: &FractalZoom) -> Vec<f64> {
    (0..=6).map(|i| z.leg_secs * i as f64 / 6.0).collect()
}

#[test]
fn no_target_degenerates_at_any_depth() {
    let mut z = FractalZoom::new(1);
    z.ss = 2;
    let mut worst_colors = usize::MAX;
    let mut worst_apl = 0f32;
    for t in z.targets.clone() {
        for age in ages(&z) {
            let mut f = Frame::black();
            fractal::render_target(&mut z, t, age, &mut f);
            let s = stats::frame_stats(&f, &panel::NOMINAL);
            // A flat frame (the smooth-exterior failure) has a handful of
            // colours; a starved frame (the black-rectangle failure) has an
            // APL near zero. Both are caught here.
            assert!(
                s.colors >= 24,
                "{} at {age:.0}s went flat: {} colours",
                t.name,
                s.colors
            );
            // The floor is low on purpose: the seahorse leg passes through a
            // thin red spike on black at 2% APL, which is one of the best
            // frames in the tour. Darkness is only a fault when there is
            // nothing left, and the colour count above already catches that.
            assert!(
                s.apl > 0.005,
                "{} at {age:.0}s went dark: APL {:.3}",
                t.name,
                s.apl
            );
            assert!(
                s.apl < 0.42,
                "{} at {age:.0}s is too bright for the panel: APL {:.3}",
                t.name,
                s.apl
            );
            worst_colors = worst_colors.min(s.colors);
            worst_apl = worst_apl.max(s.apl);
        }
    }
    println!("fewest colours {worst_colors}, highest APL {worst_apl:.2}");
}

#[test]
fn typical_colour_count_and_wire_size() {
    // Reported, not asserted beyond a sane ceiling: the sender reduces the
    // palette when a frame does not fit, and the indexed path below is exact
    // either way.
    let mut z = FractalZoom::new(3);
    let mut f = Frame::black();
    let (mut sum, mut n, mut exact) = (0usize, 0usize, 0usize);
    for i in 0..24 {
        let t = Duration::from_secs_f64(i as f64 * 7.0);
        z.render(t, &mut f);
        let s = stats::frame_stats(&f, &panel::NOMINAL);
        assert!(s.colors < 1200, "{} colours is a mess", s.colors);
        if s.wire != stats::Wire::Lossy {
            exact += 1;
        }
        sum += s.colors;
        n += 1;
    }
    println!(
        "full-colour path: {} colours mean, {exact}/{n} frames bit-exact by the estimate",
        sum / n
    );
}

#[test]
fn the_indexed_path_is_always_exact() {
    let mut z = FractalZoom::new(2);
    let mut idx = Indexed::default();
    let mut worst = 0usize;
    for i in 0..16 {
        let t = Duration::from_secs_f64(i as f64 * 11.0);
        assert!(z.render_indexed(t, &mut idx));
        assert!(idx.palette.len() <= 32, "{} entries", idx.palette.len());
        let s = stats::frame_stats(&idx.to_frame(), &panel::NOMINAL);
        assert!(s.colors <= 32);
        assert_ne!(s.wire, stats::Wire::Lossy, "at {t:?}: ~{} B", s.est_bytes);
        worst = worst.max(s.est_bytes);
    }
    println!("indexed path: worst ~{worst} B of 1464");
}

#[test]
fn a_moment_renders_the_same_every_time() {
    let mut a = FractalZoom::new(7);
    let mut b = FractalZoom::new(7);
    let mut fa = Frame::black();
    let mut fb = Frame::black();
    for secs in [0.0, 41.5, 90.4] {
        a.render(Duration::from_secs_f64(secs), &mut fa);
        b.render(Duration::from_secs_f64(secs), &mut fb);
        assert_eq!(fa.px, fb.px, "at {secs}s");
    }
}

#[test]
fn different_seeds_start_at_different_targets() {
    let a = FractalZoom::new(0);
    let b = FractalZoom::new(1);
    assert_ne!(a.leg_name(1.0), b.leg_name(1.0));
}

#[test]
fn the_tour_keeps_going_for_hours() {
    // Four hours in, a long way past one lap, it is still a picture.
    let mut z = FractalZoom::new(5);
    z.ss = 2;
    let mut f = Frame::black();
    for hours in [1.0f64, 2.5, 4.0] {
        let t = Duration::from_secs_f64(hours * 3600.0);
        z.render(t, &mut f);
        let s = stats::frame_stats(&f, &panel::NOMINAL);
        assert!(s.colors >= 24, "{hours} h in: {} colours", s.colors);
        assert!(s.apl > 0.005, "{hours} h in: APL {:.3}", s.apl);
    }
}

#[test]
fn bisection_lands_on_the_boundary() {
    // Every resolved target must have both interior and exterior within a
    // pixel of itself at full zoom - that is what makes the leg endless.
    let z = FractalZoom::new(0);
    let px = 2.0 * 5.4e-4 / 64.0;
    for t in &z.targets {
        let mut inside = 0;
        let mut outside = 0;
        for k in -3..=3 {
            for j in -3..=3 {
                let c = (t.cx + k as f64 * px, t.cy + j as f64 * px);
                match fractal::escape(c.0, c.1, 20_000) {
                    Some(_) => outside += 1,
                    None => inside += 1,
                }
            }
        }
        assert!(
            inside > 0 && outside > 0,
            "{} is not on the boundary",
            t.name
        );
    }
}

#[test]
fn the_panel_never_flashes() {
    let mut z = FractalZoom::new(4);
    z.ss = 2;
    let mut prev = Frame::black();
    let mut worst = 0f32;
    // Across a dissolve, which is the piece's largest change.
    for i in 0..40 {
        let t = Duration::from_secs_f64(89.0 + i as f64 * 0.1);
        let mut f = Frame::black();
        z.render(t, &mut f);
        if i > 0 {
            worst = worst.max(stats::luma_delta(&prev, &f, &panel::NOMINAL));
        }
        prev = f;
    }
    println!("worst frame-to-frame luma delta across a dissolve: {worst:.4}");
    assert!(worst < 0.08, "{worst} is a flash");
}
