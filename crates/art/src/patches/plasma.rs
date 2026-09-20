//! Palette-cycled plasma, rendered straight into indices. The index image drifts
//! slowly; most of the motion is the palette rotating underneath it, which is
//! lossless and nearly free on the wire.

use crate::color::{oklch, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, GUARANTEED_PALETTE, H, N, W};
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef};
use crate::rng::Rng;
use std::f32::consts::{PI, TAU};

pub const DEF: PatchDef = PatchDef {
    id: "plasma",
    name: "Plasma",
    blurb: "Indexed and palette-cycled. Black is a band in the palette, not the bottom of a fade.",
    params: PARAMS,
    make,
    // Three wave directions, frequencies, speeds and phases, the centre of the
    // rings and their frequency: every one of them comes out of the seed, so a
    // new seed is a visibly different plasma.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("scale", "Scale", 0.3, 4.0, 0.01, 1.2),
    param("drift", "Drift", 0.0, 2.0, 0.01, 0.35),
    param("cycle", "Palette cycle (rev/s)", -1.0, 1.0, 0.01, 0.12),
    param("bands", "Bands", 0.5, 4.0, 0.05, 1.5),
    param("colours", "Colours", 4.0, 32.0, 1.0, 32.0),
    param("black", "Black share", 0.0, 0.8, 0.01, 0.45),
    param("hue", "Hue", 0.0, 360.0, 1.0, 300.0),
    param("spread", "Hue spread", -360.0, 360.0, 1.0, 140.0),
    param("dither", "Index dither", 0.0, 1.0, 0.01, 1.0),
];

fn make(seed: u64) -> Box<dyn Patch> {
    let mut rng = Rng::new(seed);
    let waves = std::array::from_fn(|_| {
        let angle = rng.range(0.0, TAU);
        Wave {
            dx: angle.cos(),
            dy: angle.sin(),
            freq: rng.range(2.0, 6.0),
            speed: rng.range(0.4, 1.2) * rng.sign(),
            phase: rng.range(0.0, TAU),
        }
    });
    let centre = (rng.range(0.4, 1.6), rng.range(0.2, 0.8));
    Box::new(Plasma { waves, centre, ring_freq: rng.range(4.0, 9.0) })
}

struct Wave {
    dx: f32,
    dy: f32,
    freq: f32,
    speed: f32,
    phase: f32,
}

struct Plasma {
    waves: [Wave; 3],
    centre: (f32, f32),
    ring_freq: f32,
}

impl Patch for Plasma {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let t = ctx.t as f32;
        let scale = ctx.get("scale");
        let drift = ctx.get("drift") * t;
        let n = (ctx.get("colours") as usize).clamp(2, GUARANTEED_PALETTE);

        let phase = (ctx.get("cycle") * t).rem_euclid(1.0);
        let palette = (0..n)
            .map(|i| {
                // Average across the entry's slice of the cycle, so the black
                // edge slides through an entry instead of popping.
                const TAPS: usize = 4;
                let mut c = Rgb::BLACK;
                for k in 0..TAPS {
                    let u = (i as f32 + (k as f32 + 0.5) / TAPS as f32) / n as f32;
                    c = c.add(self.colour((u + phase).rem_euclid(1.0), ctx));
                }
                c.scale(1.0 / TAPS as f32)
            })
            .collect();

        let bands = ctx.get("bands");
        let dither = ctx.get("dither");
        let mut indices = Vec::with_capacity(N);
        for y in 0..H {
            for x in 0..W {
                // Isotropic coordinates: 0..2 across, 0..1 down.
                let (u, v) = ((x as f32 + 0.5) / H as f32, (y as f32 + 0.5) / H as f32);
                let mut f = 0.0;
                for w in &self.waves {
                    f += (w.freq * scale * (u * w.dx + v * w.dy) + w.speed * drift + w.phase).sin();
                }
                let d = ((u - self.centre.0).powi(2) + (v - self.centre.1).powi(2)).sqrt();
                f += (self.ring_freq * scale * d - drift).sin();
                let level = (f / 8.0 + 0.5) * bands * n as f32;
                let idx = (level + Dither::BlueNoise.threshold(x, y) * dither).floor();
                indices.push(idx.rem_euclid(n as f32) as u8);
            }
        }
        Frame::Indexed { palette, indices }
    }
}

impl Plasma {
    /// The colour cycle: a share of true black, then one arc of hue that stays
    /// in the panel's well-populated mid-to-bright range.
    fn colour(&self, u: f32, ctx: &Ctx) -> Rgb {
        let black = ctx.get("black");
        if u < black {
            return Rgb::BLACK;
        }
        let s = (u - black) / (1.0 - black);
        oklch(0.52 + 0.33 * (PI * s).sin(), 0.24, ctx.get("hue") + ctx.get("spread") * s)
    }
}
