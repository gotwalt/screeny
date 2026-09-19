//! The one numerical shortcut in the encoder: a hand-rolled cube root.
//!
//! Oklab needs three cube roots per colour and the chooser does tens of
//! thousands of them per frame, so [`screeny::color::cbrt_fast`] replaces
//! `f32::cbrt`. Because every codec decision downstream is a *comparison* of
//! Oklab distances, the thing to pin is not absolute accuracy but that the
//! error is far smaller than the differences being ranked.

use screeny::color::{cbrt_fast, oklab, oklab_exact, oklab_srgb8};

#[test]
fn cbrt_fast_is_accurate_over_the_range_oklab_uses() {
    // Oklab's l, m, s are linear light, so 0..=1 is the whole domain. The
    // dense sampling near zero is where the relative error of a
    // seed-and-Newton cube root is worst.
    let mut worst = 0f64;
    let mut at = 0f32;
    let mut x = 0f32;
    while x <= 1.0 {
        let got = f64::from(cbrt_fast(x));
        let want = f64::from(x).cbrt();
        let rel = if want == 0.0 {
            got.abs()
        } else {
            ((got - want) / want).abs()
        };
        if rel > worst {
            worst = rel;
            at = x;
        }
        x += 1.0 / 65_536.0;
    }
    assert!(
        worst < 2e-6,
        "worst relative error {worst:.3e} at x={at}, above the 2e-6 the docs claim"
    );
}

#[test]
fn cbrt_fast_handles_the_edges() {
    assert_eq!(cbrt_fast(0.0), 0.0);
    assert_eq!(cbrt_fast(-0.0), -0.0);
    assert!(cbrt_fast(f32::NAN).is_nan());
    assert_eq!(cbrt_fast(f32::INFINITY), f32::INFINITY);
    // Negative arguments cannot arise from linear light, but must not blow up
    // if a caller ever hands one over.
    assert!((f64::from(cbrt_fast(-0.125)) + 0.5).abs() < 1e-6);
}

#[test]
fn oklab_matches_the_libm_version() {
    // Every sRGB grey, plus the primaries and a spread of mixtures: the error
    // must stay orders of magnitude below the dE differences the chooser
    // ranks, which are of order 1e-3 in Oklab units.
    let mut worst = 0f32;
    for v in 0..=255u8 {
        for c in [[v, v, v], [v, 0, 0], [0, v, 0], [0, 0, v], [v, 255 - v, 128]] {
            let lin = screeny::color::lin(c);
            let a = oklab(lin);
            let b = oklab_exact(lin);
            for k in 0..3 {
                worst = worst.max((a[k] - b[k]).abs());
            }
        }
    }
    assert!(worst < 1e-5, "worst Oklab component error {worst:.3e}");
}

#[test]
fn oklab_lands_where_ottosson_says_it_should() {
    // White is L=1, a=b=0 (Ottosson's reference values), and black is the
    // origin. Anything else means the matrices were mistyped.
    let w = oklab_srgb8([255, 255, 255]);
    assert!((w[0] - 1.0).abs() < 1e-3, "white L = {}", w[0]);
    assert!(w[1].abs() < 1e-3 && w[2].abs() < 1e-3);
    let k = oklab_srgb8([0, 0, 0]);
    assert!(k.iter().all(|v| v.abs() < 1e-6));
}

/// The panel model is what makes error "error the panel can show". At six
/// bitplanes it has to crush the bottom of the range, and the temporal model
/// has to get most of it back - that is the whole argument for scoring codec
/// choice against `TEMPORAL` (card 002, card 030).
#[test]
fn panel_model_quantises_as_card_001_measured() {
    use screeny::panel::{DEEP, NOMINAL, TEMPORAL};
    let (levels, crushed) = NOMINAL.distinct_levels();
    assert_eq!(levels, 64, "6 bitplanes should give 64 duty steps");
    assert!(
        crushed >= 20,
        "only {crushed} sRGB codes land on the lowest level; card 002 says the \
         bottom 8.6% of the code range does"
    );
    let (tlevels, tcrushed) = TEMPORAL.distinct_levels();
    assert!(tlevels > levels, "temporal dithering should add levels");
    assert!(tcrushed < crushed, "and should uncrush the dark end");
    // Even at eight bitplanes the duty steps are linear in light, so the sRGB
    // EOTF still collapses codes at the dark end: 256 codes do not reach 256
    // distinct levels, and that is the point of the whole panel model.
    let (dlevels, dcrushed) = DEEP.distinct_levels();
    assert!(dlevels > levels && dlevels < 256, "deep levels {dlevels}");
    assert!(dcrushed < crushed);

    // Emitted light is monotonic in the code value, always.
    for p in [NOMINAL, TEMPORAL, DEEP] {
        let mut last = -1.0f32;
        for v in 0..=255u8 {
            let e = p.emit1(v);
            assert!(e >= last, "{p:?} is not monotonic at {v}");
            last = e;
        }
        assert_eq!(p.emit1(0), 0.0);
        assert_eq!(p.emit1(255), 1.0);
    }
}
