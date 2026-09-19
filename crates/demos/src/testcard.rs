//! A test card for judging the *preview*, not the art: hue sweep, a bright and
//! a dark ramp (to see where the 64 linear levels fall), 1 px and 2 px lines,
//! single dots, and a line of type.

use crate::color::{lin_to_srgb8_3, oklch_to_srgb8};
use crate::font;
use crate::frame::{Frame, Piece, H, W};
use std::time::Duration;

pub struct TestCard;

impl Piece for TestCard {
    fn name(&self) -> &'static str {
        "testcard"
    }

    fn render(&mut self, t: Duration, out: &mut Frame) {
        out.fill([0, 0, 0]);
        let ts = t.as_secs_f32();
        // Row 0-6: hue sweep at constant Oklab lightness.
        for x in 0..W {
            let h = x as f32 / W as f32;
            let c = oklch_to_srgb8(0.72, 0.16, h);
            for y in 0..7 {
                out.set(x, y, c);
            }
        }
        // Row 8-11: a linear-light grey ramp. Watch the dark end die.
        for x in 0..W {
            let v = x as f32 / (W - 1) as f32;
            let c = lin_to_srgb8_3([v, v, v]);
            for y in 8..12 {
                out.set(x, y, c);
            }
        }
        // Row 13-16: the same ramp squeezed into the bottom quarter of light.
        for x in 0..W {
            let v = 0.25 * x as f32 / (W - 1) as f32;
            let c = lin_to_srgb8_3([v, v, v]);
            for y in 13..17 {
                out.set(x, y, c);
            }
        }
        // Lines and dots.
        for x in 0..W {
            if x % 2 == 0 {
                out.set(x, 18, [255, 255, 255]);
            }
            if x % 4 < 2 {
                out.set(x, 20, [255, 120, 0]);
                out.set(x, 21, [255, 120, 0]);
            }
        }
        // A word, and a dot moving sub-pixel across the bottom row.
        font::draw("SCREENY 30FPS", 2, 23, |x, y| {
            out.set(x as usize, y as usize, [150, 200, 255])
        });
        let px = (ts * 8.0) % W as f32;
        for x in 0..W {
            let d = (x as f32 - px).abs();
            let v = (1.0 - d).clamp(0.0, 1.0);
            if v > 0.0 {
                out.set(x, H - 1, lin_to_srgb8_3([v, v * 0.5, 0.0]));
            }
        }
    }
}
