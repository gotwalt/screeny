//! Overland: flight over an endless procedural world, built for this panel
//! rather than shrunk onto it.
//!
//! The shader (`overland.wgsl`) paints by palette index; this file owns the 32
//! colours. Time of day is nothing but those colours changing, which costs a
//! palette's worth of bytes per frame and is exact on the wire. The day cycle
//! follows the brief's rules: no slow fades through the panel's crushed darks
//! (a colour that would fall below L 0.3 is cut to true black, so at dusk the
//! sky goes out band by band), lit surfaces stay in the mid-to-bright range,
//! and hues are chosen in OKLCH so brightness holds across them.

use crate::color::{oklch, Rgb};
use crate::gpu::{Scene, ShaderPatch, MAX_EXTRA};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef};
use std::f32::consts::TAU;

pub const DEF: PatchDef = PatchDef {
    id: "overland",
    name: "Overland",
    blurb: "GPU. A procedural world painted by palette index: 32 colours, exact on the wire, and the day cycle is palette animation.",
    params: PARAMS,
    make,
    // `u.seed` chooses the biome - green, savanna, red rock, alien, ice - and
    // offsets every terrain noise field and the path through it: a new seed is
    // a different country.
    seeded: true,
};

// The shader reads the first six as P(0)..P(5); the rest are used here.
const PARAMS: &[ParamSpec] = &[
    param("speed", "Flight speed", 0.0, 5.0, 0.01, 1.3),
    param("relief", "Relief", 0.5, 5.0, 0.01, 3.2),
    param("terraces", "Terracing", 0.0, 1.0, 0.01, 0.75),
    param("sea", "Sea level", 0.0, 0.8, 0.01, 0.24),
    param("towers", "Towers", 0.0, 1.0, 0.01, 0.3),
    param("clearance", "Altitude", 0.5, 4.0, 0.01, 2.1),
    param("day", "Day length (s)", 10.0, 600.0, 1.0, 120.0),
    param("hour", "Start time of day", 0.0, 1.0, 0.01, 0.08),
    param("hue", "Biome hue shift", 0.0, 360.0, 1.0, 0.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.85),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
];

fn make(seed: u64) -> Box<dyn Patch> {
    ShaderPatch::with_scene("overland", include_str!("overland.wgsl"), PARAMS, seed, scene)
}

/// Land hues a seed can land on: green, savanna, red rock, alien, ice.
const BIOMES: [f32; 5] = [145.0, 80.0, 35.0, 315.0, 195.0];

/// A colour as (lightness, chroma, hue in degrees).
type Lch = (f32, f32, f32);

fn mix_hue(a: f32, b: f32, t: f32) -> f32 {
    a + ((b - a + 540.0).rem_euclid(360.0) - 180.0) * t
}

fn mix(a: Lch, b: Lch, t: f32) -> Lch {
    // A black endpoint has no hue of its own; borrow the other's so the blend
    // does not swing through unrelated colours on the way.
    let (ha, hb) = (if a.0 < 0.01 { b.2 } else { a.2 }, if b.0 < 0.01 { a.2 } else { b.2 });
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, mix_hue(ha, hb, t))
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    crate::color::smoothstep(e0, e1, x)
}

/// Below this the panel has a handful of levels and they carry colour casts.
/// Go to true black instead: black as a shape, not as the bottom of a fade.
fn paint((l, c, h): Lch) -> Rgb {
    if l < 0.3 {
        Rgb::BLACK
    } else {
        oklch(l, c, h)
    }
}

const BLACK: Lch = (0.0, 0.0, 0.0);

fn scene(ctx: &Ctx, seed: f32) -> Scene {
    let t = ctx.t as f32;
    // phase: 0 sunrise, 0.25 noon, 0.5 sunset, 0.75 midnight
    let phase = (ctx.get("hour") + t / ctx.get("day")).rem_euclid(1.0);
    let elev = (phase * TAU).sin();
    let day = smooth(-0.2, 0.2, elev);
    let golden = (-(elev / 0.3).powi(2)).exp();
    let night = elev < 0.0;

    let biome = BIOMES[((seed * BIOMES.len() as f32) as usize).min(BIOMES.len() - 1)] + ctx.get("hue");

    // The light is whichever of sun and moon is up. Both stay ahead of the
    // camera, either side of the heading, so there is always a disc in frame.
    // Neither climbs higher than the frame shows above the horizon.
    let (azimuth, height) = if night { (-0.45_f32, -elev * 0.27) } else { (0.5_f32, elev * 0.3) };
    let height = height.max(0.04);
    let mut extra = [0.0; MAX_EXTRA];
    extra[0] = azimuth.sin() * height.cos();
    extra[1] = height.sin();
    extra[2] = azimuth.cos() * height.cos();
    extra[3] = if night { 1.0 } else { 0.0 };

    let mut pal = vec![Rgb::BLACK; 32];

    // 1..4 sky, zenith -> horizon. Night takes the zenith first.
    let golden_sky = [(0.4, 0.14, 285.0), (0.5, 0.15, 335.0), (0.64, 0.16, 15.0), (0.76, 0.16, 55.0)];
    let night_sky = [BLACK, BLACK, (0.33, 0.08, 275.0), (0.43, 0.1, 292.0)];
    for k in 0..4 {
        let kf = k as f32;
        let lo = -0.25 + 0.07 * (3.0 - kf);
        let noon = (0.44 + 0.09 * kf, 0.13 - 0.015 * kf, 252.0 - 6.0 * kf);
        pal[1 + k] = paint(mix(mix(night_sky[k], noon, smooth(lo, lo + 0.4, elev)), golden_sky[k], golden * 0.85));
    }

    // 5 sun / moon, 6 star, 7 beacon
    pal[5] = paint(if night { (0.9, 0.04, 240.0) } else { mix((0.96, 0.07, 100.0), (0.84, 0.18, 48.0), golden) });
    pal[6] = paint((0.84, 0.03, 250.0));
    let pulse = 0.5 + 0.5 * (t * TAU * 0.4).sin();
    pal[7] = paint((0.56 + 0.22 * pulse, 0.22, biome + 180.0));

    // 8..9 water near -> far. 10 is spare.
    let far_water = mix((0.62, 0.1, 228.0), (0.7, 0.13, 35.0), golden * 0.6);
    pal[8] = paint(mix(BLACK, (0.42, 0.13, 240.0), day));
    pal[9] = paint(mix((0.4, 0.07, 265.0), far_water, day));

    // 11..26 land: depth band * 4 + light step. Distance makes land lighter,
    // greyer and bluer, never darker.
    for b in 0..4 {
        for s in 0..4 {
            let (bf, sf) = (b as f32, s as f32);
            let mut noon = (0.4 + 0.13 * sf + 0.035 * bf, 0.17 - 0.025 * bf, mix_hue(biome, 250.0, 0.15 * bf));
            // Low sun: warm light, violet shadow.
            noon.2 = mix_hue(noon.2, if s >= 2 { 55.0 } else { 300.0 }, 0.55 * golden);
            // By moonlight only high ground and moon-facing slopes show; the
            // rest is silhouette.
            let moonlit = match s {
                0 | 1 => BLACK,
                2 => (0.34 + 0.02 * bf, 0.06, 268.0),
                _ => (0.52 + 0.02 * bf, 0.08, 250.0),
            };
            pal[11 + 4 * b + s] = paint(mix(moonlit, noon, day));
        }
    }

    // 27..29 caps: lit, shaded, far
    pal[27] = paint(mix((0.62, 0.03, 250.0), mix((0.94, 0.03, 230.0), (0.88, 0.09, 50.0), golden), day));
    pal[28] = paint(mix(BLACK, (0.7, 0.05, 262.0), day));
    pal[29] = paint(mix((0.5, 0.03, 250.0), (0.82, 0.04, 245.0), day));

    // 30..31 towers: lit, shaded
    pal[30] = paint(mix((0.45, 0.05, 260.0), (0.72, 0.07, biome + 180.0), day));
    pal[31] = paint(mix(BLACK, (0.46, 0.07, biome + 200.0), day));

    Scene { extra, palette: Some(Palette::new(pal, 0.07)), dither: ctx.get("dither") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Params;

    /// At every hour the palette is either true black or comfortably inside the
    /// range the panel can show: nothing lives in the crushed darks.
    #[test]
    fn no_colour_lives_in_the_shadows() {
        let mut params = Params::defaults(PARAMS);
        for step in 0..48 {
            params.set(PARAMS, "hour", step as f32 / 48.0);
            let s = scene(&Ctx { t: 0.0, dt: 0.0, now: 0.0, params: &params }, 0.5);
            for c in s.palette.expect("overland has a palette").colours() {
                let l = crate::color::to_oklab(*c)[0];
                assert!(*c == Rgb::BLACK || l > 0.28, "hour {step}/48: L {l}");
            }
        }
    }

    #[test]
    fn hue_mixes_the_short_way_round() {
        assert!((mix_hue(350.0, 10.0, 0.5).rem_euclid(360.0) - 0.0).abs() < 1e-3);
        assert!((mix_hue(10.0, 350.0, 0.25) - 5.0).abs() < 1e-3);
    }
}
