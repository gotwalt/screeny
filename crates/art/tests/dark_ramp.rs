//! Card 102's acceptance: **the dark end of the ramp is a ramp on the device**,
//! and four bands without the panel model.
//!
//! These two tests used to live inside the `testcard` patch, which card 178
//! took out of the registry: the author does not want a chart on the menu of
//! things to play ("they're not interesting"). What the chart was *for* is not
//! a patch, though - it is a measuring instrument for the pipeline and the
//! panel model, and there is no camera to point at the panel - so the drawing
//! comes with the tests and lives here, in test-only code, drawn by nothing
//! that ships.
//!
//! Only the two strips the tests read are drawn: strip 4, the darkest quarter
//! of sRGB stretched across all 64 columns, and strip 1, the full grey ramp.
//! The hue sweeps and the moving line went with the patch - nothing asserted
//! anything about them, and a test card nobody looks at is not worth keeping
//! honest.

use screeny_art::color::{srgb_to_linear, Rgb};
use screeny_art::dither::Dither;
use screeny_art::frame::{Frame, W};
use screeny_art::limiter::LimiterConfig;
use screeny_art::panel::Panel;
use screeny_art::pipeline::{linear_frame, Output, Pipeline};
use std::collections::BTreeSet;

/// The two grey strips of the old test card, in linear light, **at the rows
/// they were on**.
///
/// Strip 0 (rows 0..3) is the whole sRGB ramp; strip 4 (rows 16..19) is its
/// darkest quarter stretched across the full width. Both are neutral, so one
/// channel is the ramp. Everything else is black: nothing ever read it.
///
/// The rows matter and are not a detail to tidy: [`Dither::BlueNoise`]'s
/// threshold is a function of `(x, y)`, so moving a strip would change what
/// the dither does to it and quietly re-baseline the second test.
///
/// Ramps are even steps in **sRGB**, the way they are usually authored, which
/// is the point of the chart - the panel is linear and 6-bit, and what happens
/// to evenly authored steps on the way there is what card 102 is about.
fn grey_strips() -> Frame {
    linear_frame(|x, y| {
        let u = x as f32 / (W - 1) as f32;
        match y / 4 {
            0 => Rgb::splat(srgb_to_linear(u)),
            4 => Rgb::splat(srgb_to_linear(u * 0.25)),
            _ => Rgb::BLACK,
        }
    })
}

/// One row of the chart through the pipeline, as sRGB codes in the preview.
///
/// The limiter and the codec preview are off: this is about the panel model,
/// and the chart is deliberately the worst case for the encoder, so leaving
/// the codec in would measure the codec instead.
fn row(panel: Panel, dither: Dither, y: usize) -> Vec<u8> {
    let output = Output {
        panel,
        dither,
        codec_preview: false,
        limiter: LimiterConfig { enabled: false, ..Default::default() },
        ..Output::default()
    };
    let out = Pipeline::new(output).process(grey_strips(), 1.0 / 30.0);
    // The green channel of each column: the strips are neutral grey, so one
    // channel is the ramp.
    (0..W).map(|x| out.preview[(y * W + x) * 3 + 1]).collect()
}

/// Row 17: the middle of the dark strip.
fn dark_ramp(panel: Panel, dither: Dither) -> Vec<u8> {
    row(panel, dither, 17)
}

/// Row 4 of the test card is the darkest quarter of sRGB stretched across all
/// 64 columns: one column per sRGB code, near enough. Card 102's acceptance,
/// since there is no camera to point at the panel.
///
/// What the row is **for**: it is the one place the dark end is visible as a
/// ramp rather than as an adjective, and the numbers below are what the author
/// should expect to see by eye. On the device it is a ramp - 38 distinct greys
/// out of 64 columns (card 248: was 43 - the dead zone trades a few of them
/// for a dither that does not blink), rising the whole way, lit from the
/// sixth column. On the bit planes alone it is four bands with the first
/// third of the row black, which is what the preview used to draw and what
/// the panel really looked like before card 030.
#[test]
fn the_dark_ramp_is_a_ramp_on_the_device_and_four_bands_without_it() {
    let device = dark_ramp(Panel::Dithered, Dither::None);
    let planes = dark_ramp(Panel::BitPlanes, Dither::None);

    assert_eq!(device.iter().copied().collect::<BTreeSet<_>>().len(), 38, "{device:?}");
    assert_eq!(planes.iter().copied().collect::<BTreeSet<_>>().len(), 4, "{planes:?}");

    assert_eq!(&device[..5], &[0, 0, 0, 0, 0], "the five darkest columns are still black (card 248's dead zone)");
    assert!(device[5] > 0, "and the device lights the sixth one");
    assert_eq!(planes[..21], [0u8; 21], "the bit planes alone crush the first third of the row");

    for w in device.windows(2) {
        assert!(w[1] >= w[0], "the ramp never goes backwards: {device:?}");
    }
    assert!(device[63] >= 62, "and reaches the top of the quarter it covers");
}

/// The dither only has somewhere to go where the panel is coarser than the
/// 8-bit codes. On this row - entirely inside that range - it spreads the
/// collapse across several codes; on the bright half of the grey ramp, a
/// full-amplitude dither can move a code by **one**, and only where a value
/// sits on a code boundary. That is the whole change card 102 made to dither:
/// it used to be a visible screen-door texture everywhere.
#[test]
fn dither_moves_the_dark_row_and_barely_touches_the_bright_one() {
    let plain = dark_ramp(Panel::Dithered, Dither::None);
    let dithered = dark_ramp(Panel::Dithered, Dither::BlueNoise);
    let dark_swing = swing(&plain, &dithered);
    assert!(dark_swing >= 3, "the dark end is where dither earns its keep: {dark_swing}");

    // Row 1: the grey ramp, right half, which is all above sRGB 128.
    let bright = |dither| row(Panel::Dithered, dither, 1)[32..].to_vec();
    let bright_swing = swing(&bright(Dither::None), &bright(Dither::BlueNoise));
    assert!(bright_swing <= 1, "no noise up here the panel could not show anyway: {bright_swing}");
}

/// Largest difference, in sRGB codes, between two rows.
fn swing(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap_or(0)
}
