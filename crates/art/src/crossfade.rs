//! Cross-fading one picture into another (card 304).
//!
//! The blend happens **before** the limiter, in linear light: two [`Frame`]s
//! become one, and that one goes through the pipeline like any other frame. So
//! the limiter owns a transition's brightness safety the same way it owns a
//! patch's - a fade from a dark picture to a bright one can be slowed by the
//! rise cap, which is correct, and a fade can never be brighter than the APL
//! cap allows.
//!
//! **The easing is smoothstep**, not equal-power. Equal-power (`cos`/`sin`
//! weights) is the audio answer: two *uncorrelated* signals add in power, so
//! the weights' squares must sum to one. Light does not work that way - linear
//! light adds as values, so weights that sum to one keep the picture's
//! brightness where it was, and equal-power would put a bump of up to
//! `sqrt(2)` (41 % brighter) into the middle of every fade between two pictures
//! of similar level. Smoothstep sums to one and has zero slope at both ends,
//! so the outgoing picture lets go gently and the incoming one settles in
//! without a visible "arrival".
//!
//! Nothing here knows about time sources or threads: a [`Crossfade`] is told
//! how much wall-clock time has passed, which is what makes it testable.

use crate::color::Rgb;
use crate::frame::Frame;

/// Smoothstep on `0..1`, clamped: 0 at 0, 1 at 1, flat at both ends.
#[must_use]
pub fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// `from` and `to` mixed in linear light: `w = 0` is `from`, `w = 1` is `to`.
///
/// Always a `Linear` frame, whatever went in: an `Indexed` frame is expanded
/// through its palette first. The caller passes the incoming frame through
/// untouched once a fade is over, which is what keeps an indexed patch exact.
#[must_use]
pub fn blend(from: &Frame, to: &Frame, w: f32) -> Frame {
    let w = w.clamp(0.0, 1.0);
    let a = from.to_linear();
    let b = to.to_linear();
    Frame::Linear(a.into_iter().zip(b).map(|(a, b): (Rgb, Rgb)| a.lerp(b, w)).collect())
}

/// How far through a fade of a given length, on wall-clock time.
#[derive(Clone, Copy, Debug)]
pub struct Crossfade {
    /// Seconds. Zero or less is a cut.
    len: f64,
    elapsed: f64,
}

impl Crossfade {
    /// A fade that lasts `len` seconds. Anything not finite, or not above
    /// zero, is a cut: done before its first frame.
    #[must_use]
    pub fn new(len: f64) -> Self {
        Crossfade { len: if len.is_finite() { len.max(0.0) } else { 0.0 }, elapsed: 0.0 }
    }

    /// The length asked for.
    #[must_use]
    pub fn len(&self) -> f64 {
        self.len
    }

    /// Move on by `dt` seconds of wall clock.
    pub fn advance(&mut self, dt: f64) {
        if dt.is_finite() && dt > 0.0 {
            self.elapsed += dt;
        }
    }

    /// True once the incoming picture has the panel to itself.
    #[must_use]
    pub fn done(&self) -> bool {
        self.elapsed >= self.len
    }

    /// The incoming picture's weight now, eased.
    #[must_use]
    pub fn weight(&self) -> f32 {
        if self.done() {
            1.0
        } else {
            ease((self.elapsed / self.len) as f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::N;
    use crate::limiter::LimiterConfig;
    use crate::pipeline::{Output, Pipeline};

    fn flat(c: Rgb) -> Frame {
        Frame::Linear(vec![c; N])
    }

    /// Two constant colours, blended at every step of a two-second fade: the
    /// weight follows smoothstep, the pixels follow the weight, and the two
    /// weights always sum to one - no brightness bump in the middle.
    #[test]
    fn the_weights_follow_smoothstep_over_time() {
        let a = Rgb::new(0.30, 0.02, 0.10);
        let b = Rgb::new(0.04, 0.20, 0.25);
        let mut f = Crossfade::new(2.0);
        let dt = 1.0 / 30.0;
        let mut last = 0.0_f32;
        let mut frames = 0;
        while !f.done() {
            f.advance(dt);
            frames += 1;
            let w = f.weight();
            let x = (frames as f64 * dt / 2.0).min(1.0) as f32;
            assert!((w - ease(x)).abs() < 1e-6, "frame {frames}: {w} against {}", ease(x));
            assert!(w >= last, "the weight never goes backwards");
            last = w;
            let px = blend(&flat(a), &flat(b), w).pixel(7);
            let want = Rgb::new(a.r * (1.0 - w) + b.r * w, a.g * (1.0 - w) + b.g * w, a.b * (1.0 - w) + b.b * w);
            assert!((px.r - want.r).abs() < 1e-6 && (px.g - want.g).abs() < 1e-6 && (px.b - want.b).abs() < 1e-6);
        }
        assert!((59..=61).contains(&frames), "two seconds at 30 fps, not {frames} frames");
        assert_eq!(f.weight(), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6, "halfway is an even mix");
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
    }

    #[test]
    fn a_zero_length_fade_is_a_cut() {
        for len in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let f = Crossfade::new(len);
            assert!(f.done(), "{len} should be a cut");
            assert_eq!(f.weight(), 1.0);
        }
    }

    /// An indexed frame blends through its palette.
    #[test]
    fn an_indexed_frame_blends_through_its_palette() {
        let indexed = Frame::Indexed { palette: vec![Rgb::BLACK, Rgb::splat(0.5)], indices: (0..N).map(|i| (i % 2) as u8).collect() };
        let out = blend(&flat(Rgb::splat(0.1)), &indexed, 0.5);
        assert!(matches!(out, Frame::Linear(_)));
        assert!((out.pixel(0).r - 0.05).abs() < 1e-6);
        assert!((out.pixel(1).r - 0.30).abs() < 1e-6);
    }

    /// The limiter sees the blended frame, so a fade from black into full
    /// white is capped like any other frame: the rise per frame stays inside
    /// the rise cap and the picture settles at the APL cap, not at white.
    #[test]
    fn the_limiter_owns_a_fade_into_white() {
        let mut pipe = Pipeline::new(Output::default());
        let cfg = LimiterConfig::default();
        let dt = 1.0 / 30.0;
        let mut f = Crossfade::new(2.0);
        let mut prev = 0.0_f32;
        for _ in 0..120 {
            f.advance(dt);
            let frame = blend(&Frame::black(), &flat(Rgb::splat(1.0)), f.weight());
            let s = pipe.process(frame, dt).stats;
            assert!(s.luma - prev <= cfg.max_rise_per_s * dt as f32 + 1e-4, "rose {} in one frame", s.luma - prev);
            assert!(s.apl <= cfg.apl_cap + 1e-4, "apl {} over the cap", s.apl);
            prev = s.luma;
        }
        assert!((prev - cfg.apl_cap).abs() < 0.01, "settles at the cap, got {prev}");
    }
}
