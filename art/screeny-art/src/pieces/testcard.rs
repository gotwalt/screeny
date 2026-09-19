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
