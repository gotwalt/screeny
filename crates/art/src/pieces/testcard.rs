//! Not art: a chart for checking the panel model and the preview against the
//! claims in the brief. Eight strips of four rows each.

use crate::color::{oklch, srgb_to_linear, Rgb};
use crate::frame::{Frame, W};
use crate::pipeline::linear_frame;
use crate::piece::{param, Ctx, ParamSpec, Piece, PieceDef};

pub const DEF: PieceDef = PieceDef {
    id: "testcard",
    name: "Test card",
    blurb: "Ramps, hue sweeps and a slow-moving line, for judging banding, brightness by hue and sub-pixel motion.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[param("speed", "Line speed (px/s)", 0.0, 30.0, 0.1, 3.0)];

fn make(_seed: u64) -> Box<dyn Piece> {
    Box::new(TestCard)
}

struct TestCard;

impl Piece for TestCard {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let line_x = (ctx.t as f32 * ctx.get("speed")).rem_euclid(W as f32);
        let line = Rgb::new(1.0, 0.75, 0.3);
        linear_frame(|x, y| {
            let u = x as f32 / (W - 1) as f32;
            // Ramps are even steps in sRGB, the way they are usually authored.
            let ramp = srgb_to_linear(u);
            match y / 4 {
                0 => Rgb::splat(ramp),
                1 => Rgb::new(ramp, 0.0, 0.0),
                2 => Rgb::new(0.0, ramp, 0.0),
                3 => Rgb::new(0.0, 0.0, ramp),
                // The darkest quarter of sRGB, stretched across the full width.
                4 => Rgb::splat(srgb_to_linear(u * 0.25)),
                // Hue at constant OKLCH lightness, then at constant RGB "value".
                5 => oklch(0.72, 0.14, u * 360.0),
                6 => hsv(u),
                // Top two rows: anti-aliased 1.5 px line. Bottom two: snapped to the grid.
                _ if y % 4 < 2 => line.scale(coverage(x as f32, line_x, 1.5)),
                _ => line.scale(if x == line_x as usize { 1.0 } else { 0.0 }),
            }
        })
    }
}

/// Fully saturated, full-value HSV hue, in linear light.
fn hsv(u: f32) -> Rgb {
    let h = u * 6.0;
    let f = |shift: f32| {
        let k = (h + shift).rem_euclid(6.0);
        srgb_to_linear(1.0 - k.min(4.0 - k).clamp(0.0, 1.0))
    };
    Rgb::new(f(5.0), f(3.0), f(1.0))
}

/// How much of pixel `[px, px + 1]` a vertical line of `width` centred on
/// `centre` covers, wrapping at the panel edge.
fn coverage(px: f32, centre: f32, width: f32) -> f32 {
    let w = W as f32;
    [-w, 0.0, w]
        .iter()
        .map(|shift| {
            let (lo, hi) = (centre + shift - width / 2.0, centre + shift + width / 2.0);
            (hi.min(px + 1.0) - lo.max(px)).clamp(0.0, 1.0)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dither::Dither;
    use crate::panel::Panel;
    use crate::piece::Params;
    use crate::pipeline::{Pipeline, Settings};
    use std::collections::BTreeSet;

    /// Row 4 of the test card is the darkest quarter of sRGB stretched across
    /// all 64 columns: one column per sRGB code, near enough. Card 102's
    /// acceptance, since there is no camera to point at the panel.
    ///
    /// What the row is **for**: it is the one place the dark end is visible as
    /// a ramp rather than as an adjective, and the numbers below are what the
    /// owner should expect to see by eye. On the device it is a ramp - 43
    /// distinct greys out of 64 columns, rising the whole way, lit from the
    /// third column. On the bit planes alone it is four bands with the first
    /// third of the row black, which is what the preview used to draw and what
    /// the panel really looked like before card 030.
    fn dark_ramp(panel: Panel, dither: Dither) -> Vec<u8> {
        // The limiter and the codec preview are off: this row is about the
        // panel model, and a test card is deliberately the worst case for the
        // encoder (four ramps and two hue sweeps, so it goes lossy).
        let settings = Settings {
            panel,
            dither,
            codec_preview: false,
            limiter: crate::limiter::LimiterSettings { enabled: false, ..Default::default() },
            ..Settings::default()
        };
        let params = Params::defaults(PARAMS);
        let frame = TestCard.render(&Ctx { t: 0.0, dt: 0.0, now: 0.0, params: &params });
        let out = Pipeline::new(settings).process(frame, 1.0 / 30.0);
        // Row 17: the middle of strip 4 (rows 16..19), green channel of each
        // column. The strip is neutral grey, so one channel is the ramp.
        (0..W).map(|x| out.preview[(17 * W + x) * 3 + 1]).collect()
    }

    #[test]
    fn the_dark_ramp_is_a_ramp_on_the_device_and_four_bands_without_it() {
        let device = dark_ramp(Panel::Dithered, Dither::None);
        let planes = dark_ramp(Panel::BitPlanes, Dither::None);

        assert_eq!(device.iter().copied().collect::<BTreeSet<_>>().len(), 43, "{device:?}");
        assert_eq!(planes.iter().copied().collect::<BTreeSet<_>>().len(), 4, "{planes:?}");

        assert_eq!(&device[..2], &[0, 0], "the two darkest columns are still black");
        assert!(device[2] > 0, "and the device lights the third one");
        assert_eq!(planes[..21], [0u8; 21], "the bit planes alone crush the first third of the row");

        for w in device.windows(2) {
            assert!(w[1] >= w[0], "the ramp never goes backwards: {device:?}");
        }
        assert!(device[63] >= 62, "and reaches the top of the quarter it covers");
    }

    /// The dither only has somewhere to go where the panel is coarser than the
    /// 8-bit codes. On this row - entirely inside that range - it spreads the
    /// collapse across several codes; on the bright half of the grey ramp, a
    /// full-amplitude dither can move a code by **one**, and only where a
    /// value sits on a code boundary. That is the whole change card 102 made
    /// to dither: it used to be a visible screen-door texture everywhere.
    #[test]
    fn dither_moves_the_dark_row_and_barely_touches_the_bright_one() {
        let plain = dark_ramp(Panel::Dithered, Dither::None);
        let dithered = dark_ramp(Panel::Dithered, Dither::BlueNoise);
        let dark_swing = swing(&plain, &dithered);
        assert!(dark_swing >= 3, "the dark end is where dither earns its keep: {dark_swing}");

        let bright = |dither| {
            let settings = Settings {
                dither,
                codec_preview: false,
                limiter: crate::limiter::LimiterSettings { enabled: false, ..Default::default() },
                ..Settings::default()
            };
            let params = Params::defaults(PARAMS);
            let frame = TestCard.render(&Ctx { t: 0.0, dt: 0.0, now: 0.0, params: &params });
            let out = Pipeline::new(settings).process(frame, 1.0 / 30.0);
            // Row 1: the grey ramp, right half, which is all above sRGB 128.
            (32..W).map(|x| out.preview[(W + x) * 3 + 1]).collect::<Vec<u8>>()
        };
        let bright_swing = swing(&bright(Dither::None), &bright(Dither::BlueNoise));
        assert!(bright_swing <= 1, "no noise up here the panel could not show anyway: {bright_swing}");
    }

    /// Largest difference, in sRGB codes, between two rows.
    fn swing(a: &[u8], b: &[u8]) -> i32 {
        a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap_or(0)
    }
}
