//! Metaballs on black: hard-edged shapes, supersampled so the edges are
//! anti-aliased and the slow motion is sub-pixel smooth. Set Samples to 1 to see
//! what the panel does to it without that.

use crate::color::{oklch, smoothstep, Rgb};
use crate::frame::{Frame, H, W};
use crate::piece::{param, Ctx, ParamSpec, Piece, PieceDef};
use crate::rng::Rng;
use std::f32::consts::TAU;

pub const DEF: PieceDef = PieceDef {
    id: "metaballs",
    name: "Metaballs",
    blurb: "Continuous colour, supersampled in linear light. More than 32 colours, so it shows codec damage.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[
    param("count", "Bodies", 2.0, 8.0, 1.0, 5.0),
    param("speed", "Speed", 0.0, 3.0, 0.01, 0.5),
    param("size", "Size", 0.4, 2.5, 0.01, 1.0),
    param("hue", "Hue", 0.0, 360.0, 1.0, 20.0),
    param("spread", "Hue spread", 0.0, 360.0, 1.0, 200.0),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
];

const MAX_BODIES: usize = 8;

struct Body {
    radius: f32,
    amp: (f32, f32),
    freq: (f32, f32),
    phase: (f32, f32),
}

struct Metaballs {
    bodies: Vec<Body>,
}

fn make(seed: u64) -> Box<dyn Piece> {
    let mut rng = Rng::new(seed);
    let bodies = (0..MAX_BODIES)
        .map(|_| Body {
            radius: rng.range(4.0, 7.0),
            // Let the paths overflow the short axis a little: compose for wide.
            amp: (rng.range(14.0, 30.0), rng.range(6.0, 15.0)),
            freq: (rng.range(0.3, 1.0) * rng.sign(), rng.range(0.3, 1.0) * rng.sign()),
            phase: (rng.range(0.0, TAU), rng.range(0.0, TAU)),
        })
        .collect();
    Box::new(Metaballs { bodies })
}

impl Piece for Metaballs {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let t = ctx.t as f32 * ctx.get("speed");
        let count = (ctx.get("count") as usize).clamp(1, MAX_BODIES);
        let size = ctx.get("size");
        let (hue, spread) = (ctx.get("hue"), ctx.get("spread"));

        let live: Vec<(f32, f32, f32, Rgb)> = self.bodies[..count]
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let x = W as f32 * 0.5 + b.amp.0 * (b.freq.0 * t + b.phase.0).sin();
                let y = H as f32 * 0.5 + b.amp.1 * (b.freq.1 * t + b.phase.1).sin();
                let r = b.radius * size;
                (x, y, r * r, oklch(0.78, 0.2, hue + spread * i as f32 / count as f32))
            })
            .collect();

        Frame::supersample(ctx.get("samples") as usize, |x, y| {
            let mut field = 0.0;
            let mut colour = Rgb::BLACK;
            let mut weight = 0.0;
            for (bx, by, r2, c) in &live {
                let d2 = (x - bx).powi(2) + (y - by).powi(2) + 1e-3;
                let f = r2 / d2;
                field += f;
                colour = colour.add(c.scale(f * f));
                weight += f * f;
            }
            if field < 1.0 {
                return Rgb::BLACK;
            }
            // Brightest in the core, never dimmer than mid-range at the rim.
            colour.scale((0.3 + 0.7 * smoothstep(1.0, 3.0, field)) / weight)
        })
    }
}
