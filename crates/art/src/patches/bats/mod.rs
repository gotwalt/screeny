//! Bats: a loose colony hunting at dusk, seen from a camera on the ground
//! looking up and out at a low, bright moon.
//!
//! Card 313's picture, in short: dark shapes crossing a big moon, erratic
//! insect-hunting flight rather than a flock's smooth wheeling, a bare
//! tree-line silhouette along the bottom few rows, and - now and then - the
//! whole colony pouring out of a gap in that tree line and dispersing. The
//! flight is [`sim`]; the bat's own silhouette is [`wing`]; this file is
//! everything you can see - the sky, the moon, the tree line and the colony
//! itself.
//!
//! **This is a sibling of `flock`, not a fork of it.** Both patches are a
//! seeded, indexed CPU patch with a camera, an anti-aliased coverage buffer
//! and a level-of-detail model, because that is the right shape for anything
//! small and flying on this panel - but flock's camera flies inside the
//! flock and steers by continuous boid forces, where this camera never moves
//! and the bats steer by picking a point and snapping onto it. Sharing the
//! *idea* is right; sharing the *code* is not, because flock has to keep
//! rendering byte-identically and bending its `Coverage`, its `V3` or its
//! `View` to fit a fixed-camera, event-driven flight would risk exactly that.
//! So [`Coverage`] below and `sim::V3` are **copied** from `flock/mod.rs` and
//! `flock/sim.rs` (generic rasterising and vector code, not flock-specific
//! logic) rather than imported, and the camera and steering are written
//! fresh. A follow-up card (see the Log) is the right place to lift the
//! generic half of that - `Coverage`, `V3` - into a shared module once a
//! third patch wants it too.

pub(crate) mod sim;
pub(crate) mod wing;

use crate::color::{oklch, smoothstep, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{choice, param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use sim::{v3, Sim, Tuning, V3, STEP};

pub const DEF: PatchDef = PatchDef {
    id: "bats",
    name: "Bats",
    blurb: "A colony hunting at dusk, seen from the ground: erratic jinking flight against a big low moon, over a bare tree line.",
    params: PARAMS,
    make,
    // The world (the tree line, the moon's place and colour, the camera's
    // own pitch) and the colony (who is in it, how it jinks, when it pours)
    // all come from the seed, same promise flock makes.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("bats", "How many bats", 4.0, 40.0, 1.0, 16.0),
    param("pace", "How fast the flight moves", 0.3, 2.5, 0.05, 1.0),
    param("jink", "How often it changes its mind, and how sharply", 0.0, 1.0, 0.01, 0.85),
    param("loose", "How loosely the colony holds together", 0.0, 1.0, 0.01, 0.55),
    param("size", "How big the bats are drawn (a longer lens, not a closer camera)", 0.5, 3.0, 0.1, 1.8),
    param("beat", "How fast the wings beat (Hz)", 4.0, 14.0, 0.5, 9.0),
    param("moon", "How big the moon is", 0.5, 2.5, 0.05, 1.4),
    param("dusk", "How far the sky has gone from a warm horizon to indigo night", 0.0, 1.0, 0.01, 0.10),
    param("cycle", "How fast dusk drifts on its own; 0 holds it still", 0.0, 1.0, 0.01, 0.0),
    param("stream", "How often the colony pours out of the roost", 0.0, 1.0, 0.01, 0.35),
    // The card asked for both value structures tried and the better one said
    // honestly (see the Log): pale bats on a near-black sky read more clearly
    // as *bats* - wings and a scalloped edge actually show - than dark
    // silhouettes on the lit dusk sky do, especially at `color` 0. Default to
    // it; the dusk picture is one click away for whoever prefers the mood.
    choice("scheme", "Pale bats on a near-black night, or dark silhouettes on a dusk sky", SCHEMES, 1.0),
    param("color", "How much colour (0 is pure grayscale)", 0.0, 1.0, 0.01, 1.0),
];

/// The two value structures card 313 asks to try. Dark-on-mid-value is the
/// classic dusk silhouette; light-on-near-black is its inverse - see the Log
/// for which the author kept.
const SCHEMES: &[&str] = &["dusk silhouettes", "night, pale bats"];

/// Horizontal field of view - the same number flock uses, for the same
/// reason: wide enough that the sky is around the camera, not a postcard.
const FOV: f32 = 76.0;

const SUPERSAMPLE: usize = 3;

/// Sky bands, dark end to horizon.
const SKY: usize = 10;
/// Sky bands plus the moon's halo and its disc.
const BANDS: usize = SKY + 2;
/// Levels of bat over sky. Small on purpose: with a moon and a sky behind
/// them, the ink axis is only ever anti-aliasing a thin wing edge, never
/// carrying a depth cue the way flock's does on true black.
const INK: usize = 6;

/// A colour as (lightness, chroma, hue in degrees).
type Lch = (f32, f32, f32);

fn mix_hue(a: f32, b: f32, t: f32) -> f32 {
    a + ((b - a + 540.0).rem_euclid(360.0) - 180.0) * t
}

fn mix(a: Lch, b: Lch, t: f32) -> Lch {
    let (ha, hb) = (if a.0 < 0.01 { b.2 } else { a.2 }, if b.0 < 0.01 { a.2 } else { b.2 });
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, mix_hue(ha, hb, t))
}

/// Below this the panel has a handful of levels and a colour cast; true black
/// instead, same reasoning as flock's `FLOOR_L`.
const FLOOR_L: f32 = 0.08;

fn paint((l, c, h): Lch) -> Rgb {
    if l < FLOOR_L {
        Rgb::BLACK
    } else {
        oklch(l, c, h)
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The sky ramp, the moon's colour, and the bat's own silhouette colour, all
/// for one scheme at one `dusk` position.
///
/// `moon_elev` (degrees above the horizon) decides the moon's *own* colour
/// independently of the sky - "harvest orange low, bone white higher"
/// (card 313's picture) - because a low moon is seen through more air
/// whatever the sky around it is doing.
fn scheme(which: usize, dusk: f32, moon_elev: f32) -> ([Lch; BANDS], Lch) {
    let night = which == 1;
    let mut bands = [(0.0, 0.0, 0.0); BANDS];

    // The horizon and the top of the ramp, at this `dusk`. Only the dusk
    // scheme actually moves through hue with `dusk`; the night sky stays
    // near-black throughout, since there is nothing left in it to move.
    let (top, horizon): (Lch, Lch) = if night {
        // The horizon needs enough lift over true black that the tree line
        // (literal black, drawn as a flat matte) still reads as a shape cut
        // out of *something* - a night sky that goes all the way to zero
        // right down to the ground leaves nothing for the silhouette to sit
        // against.
        ((0.012, 0.015, 250.0), (0.20, 0.04, mix_hue(255.0, 235.0, dusk)))
    } else {
        (
            (lerp(0.05, 0.02, dusk), lerp(0.035, 0.02, dusk), mix_hue(268.0, 250.0, dusk)),
            // Hue wraps, and the short way from a warm horizon (28) to a
            // violet one (275) runs down through red and magenta, not up
            // through yellow and green - `mix_hue` takes the short way,
            // where a plain `lerp` here would paint the whole colony an
            // implausible green partway through the evening.
            (lerp(0.50, 0.27, dusk), lerp(0.17, 0.10, dusk), mix_hue(58.0, 275.0, dusk)),
        )
    };
    for (b, band) in bands.iter_mut().enumerate().take(SKY) {
        let u = b as f32 / (SKY - 1) as f32;
        let l = top.0 + (horizon.0 - top.0) * u.powf(0.85);
        let c = top.1 + (horizon.1 - top.1) * u;
        let h = mix_hue(top.2, horizon.2, u);
        *band = (l, c, h);
    }

    // The moon: its own ramp from harvest orange to bone white, by elevation
    // alone, not by the sky's hue - a low moon looks the same colour whatever
    // the sky around it is doing.
    let mt = smoothstep(6.0, 40.0, moon_elev);
    let (ml, mc, mh) = (lerp(0.80, 0.975, mt), lerp(0.11, 0.02, mt), mix_hue(36.0, 220.0, mt));
    bands[SKY] = (ml * 0.55, mc * 0.55, mh);
    bands[SKY + 1] = (ml, mc, mh);

    let bat = if night {
        (0.84, 0.03, 222.0)
    } else {
        (0.045, 0.035, horizon.2)
    };
    (bands, bat)
}

/// `BANDS` x `INK` colours, plus one extra entry (index 0) for the tree line,
/// which is drawn as a flat silhouette rather than through the sky/ink
/// palette - see the module doc for why a dithered edge between it and the
/// sky was not worth the complexity for a shape this size.
fn palette(bands: &[Lch; BANDS], bat: Lch, colour: f32) -> Vec<Rgb> {
    let desat = |c: Lch| (c.0, c.1 * colour, c.2);
    let mut out = Vec::with_capacity(1 + BANDS * INK);
    out.push(Rgb::BLACK);
    for band in bands {
        for k in 0..INK {
            out.push(paint(desat(mix(*band, bat, k as f32 / (INK - 1) as f32))));
        }
    }
    out
}

// --------------------------------------------------------------------------
// Copied from `flock::Coverage` (card 313's Log): the same anti-aliased
// stroke/triangle rasteriser, unmodified in substance. Flock has to keep
// rendering byte-identically, so this is a copy rather than a shared module -
// see the file doc for the follow-up card that would fix that properly.
// --------------------------------------------------------------------------

struct Coverage {
    ss: usize,
    w: usize,
    h: usize,
    buf: Vec<f32>,
}

impl Coverage {
    fn new(ss: usize) -> Coverage {
        let (w, h) = (W * ss, H * ss);
        Coverage { ss, w, h, buf: vec![0.0; w * h] }
    }

    fn stroke(&mut self, a: (f32, f32), b: (f32, f32), half: f32, ink: f32) {
        let ss = self.ss as f32;
        let aa = 0.5 / ss;
        let pad = half + aa;
        let lo_x = (((a.0.min(b.0) - pad) * ss).floor().max(0.0)) as usize;
        let hi_x = (((a.0.max(b.0) + pad) * ss).ceil().max(0.0) as usize).min(self.w);
        let lo_y = (((a.1.min(b.1) - pad) * ss).floor().max(0.0)) as usize;
        let hi_y = (((a.1.max(b.1) + pad) * ss).ceil().max(0.0) as usize).min(self.h);
        if lo_x >= hi_x || lo_y >= hi_y {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        for j in lo_y..hi_y {
            let py = (j as f32 + 0.5) / ss;
            for i in lo_x..hi_x {
                let px = (i as f32 + 0.5) / ss;
                let t = (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0);
                let (ex, ey) = (px - a.0 - dx * t, py - a.1 - dy * t);
                let d = (ex * ex + ey * ey).sqrt();
                let cover = smoothstep(pad, half - aa, d);
                if cover > 0.0 {
                    let cell = &mut self.buf[j * self.w + i];
                    *cell += (ink - *cell) * cover;
                }
            }
        }
    }

    fn triangle(&mut self, a: (f32, f32), b: (f32, f32), c: (f32, f32), ink: f32) {
        let ss = self.ss as f32;
        let aa = 0.5 / ss;
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1e-6 {
            return;
        }
        let wind = area.signum();
        let (min_x, max_x) = (a.0.min(b.0).min(c.0), a.0.max(b.0).max(c.0));
        let (min_y, max_y) = (a.1.min(b.1).min(c.1), a.1.max(b.1).max(c.1));
        let lo_x = (((min_x - aa) * ss).floor().max(0.0)) as usize;
        let hi_x = (((max_x + aa) * ss).ceil().max(0.0) as usize).min(self.w);
        let lo_y = (((min_y - aa) * ss).floor().max(0.0)) as usize;
        let hi_y = (((max_y + aa) * ss).ceil().max(0.0) as usize).min(self.h);
        if lo_x >= hi_x || lo_y >= hi_y {
            return;
        }
        let edges = [(a, b), (b, c), (c, a)];
        for j in lo_y..hi_y {
            let py = (j as f32 + 0.5) / ss;
            for i in lo_x..hi_x {
                let px = (i as f32 + 0.5) / ss;
                let mut d = f32::INFINITY;
                for (p, q) in edges {
                    let (ex, ey) = (q.0 - p.0, q.1 - p.1);
                    let len = (ex * ex + ey * ey).sqrt().max(1e-6);
                    d = d.min(wind * (ex * (py - p.1) - ey * (px - p.0)) / len);
                }
                let cover = smoothstep(-aa, aa, d);
                if cover > 0.0 {
                    let cell = &mut self.buf[j * self.w + i];
                    *cell += (ink - *cell) * cover;
                }
            }
        }
    }

    fn pixel(&self, x: usize, y: usize) -> f32 {
        let mut sum = 0.0;
        for j in 0..self.ss {
            let row = (y * self.ss + j) * self.w + x * self.ss;
            sum += self.buf[row..row + self.ss].iter().sum::<f32>();
        }
        sum / (self.ss * self.ss) as f32
    }
}

// --------------------------------------------------------------------------

/// The fixed camera: on the ground, looking up and out. Built once from the
/// seed and never moved (unlike flock's, which flies).
struct View {
    right: V3,
    up: V3,
    fwd: V3,
    focal: f32,
    moon: V3,
    /// The moon disc's cosine radius (outer edge) and where it reaches full
    /// strength (inner edge), and how sharply the halo falls off outside it -
    /// all rebuilt each frame from the `moon` parameter.
    disc_lo: f32,
    disc_hi: f32,
    halo_k: f32,
}

impl View {
    fn project(&self, p: V3) -> Option<(f32, f32, f32)> {
        let z = p.dot(self.fwd);
        if z < 0.35 {
            return None;
        }
        let k = self.focal / z;
        Some((0.5 * W as f32 + p.dot(self.right) * k, 0.5 * H as f32 - p.dot(self.up) * k, z))
    }

    fn ray(&self, x: f32, y: f32) -> V3 {
        let px = (x - 0.5 * W as f32) / self.focal;
        let py = (0.5 * H as f32 - y) / self.focal;
        self.fwd.add(self.right.scale(px)).add(self.up.scale(py))
    }

    /// Where on the sky ramp this direction falls, plus the moon's glow.
    fn band_at(&self, dir: V3) -> f32 {
        let inv = 1.0 / dir.len().max(1e-6);
        let sine = dir.y * inv;
        let warp = |a: f32| (a * a * a).sqrt().sqrt();
        let a = sine.abs().min(1.0);
        let v = if sine >= 0.0 {
            1.0 - warp(a)
        } else {
            ((1.0 - 1.22 * warp(a)) * 0.88 - 0.05).max(0.0)
        };
        let sky = v * (SKY - 1) as f32;

        let cos = dir.dot(self.moon) * inv;
        let disc = smoothstep(self.disc_lo, self.disc_hi, cos);
        let halo = 0.55 * (-self.halo_k * (1.0 - cos).max(0.0)).exp();
        let glow = (disc + halo * (1.0 - disc)).clamp(0.0, 1.0);
        sky + glow * ((BANDS - 1) as f32 - sky)
    }
}

// --------------------------------------------------------------------------

struct Bats {
    sim: Sim,
    warped: f64,
    steps: i64,
    /// The camera never moves, but it lives in `sim` (which needs it for the
    /// spherical target sampling) rather than duplicated here - `sim.cam()`
    /// is the one copy.
    moon: V3,
    moon_elev: f32,
    tree: Vec<f32>,
    seen: usize,
    dusk_seed: f32,
}

/// Longest catch-up after a stall, in fixed steps - same reasoning as flock's
/// `CATCHUP`: a paused studio resumes hunting, it does not fast-forward.
const CATCHUP: i64 = 240;

const HAZE_DUSK: f32 = 0.92;
const HAZE_NIGHT: f32 = 0.38;

fn make(seed: u64) -> Box<dyn Patch> {
    let mut rng = crate::rng::Rng::new(seed ^ 0x00ba_751e);
    // The camera's own pitch: low enough that the horizon sits near the
    // bottom of the frame and the tree line has a few rows to stand in.
    let elev = rng.range(13.0, 21.0_f32).to_radians();
    let fwd = v3(0.0, elev.sin(), elev.cos());
    let world_up = v3(0.0, 1.0, 0.0);
    let right = world_up.cross(fwd).unit_or(v3(1.0, 0.0, 0.0));
    let up = fwd.cross(right).unit_or(world_up);

    let moon_az = rng.range(-18.0_f32, 18.0).to_radians();
    let moon_el = rng.range(9.0_f32, 32.0).to_radians();
    let (sa, ca) = moon_az.sin_cos();
    let (se, ce) = moon_el.sin_cos();
    let moon = fwd.scale(ca * ce).add(right.scale(sa * ce)).add(up.scale(se));

    let (tree, gap) = build_tree(&mut rng);

    // The roost point the stream pours from: near the gap, at the top of the
    // tree line there, a little way out - a real depth, not a screen
    // coordinate, so the flight can fly towards it.
    let edge_row = H as f32 - tree[gap] + 0.5;
    let focal = 0.5 * W as f32 / (0.5 * FOV.to_radians()).tan();
    // Only `ray()` is used here, so the moon/disc fields are placeholders -
    // this probe is never asked about the sky.
    let probe = View { right, up, fwd, focal, moon, disc_lo: 0.0, disc_hi: 0.0, halo_k: 0.0 };
    let roost = probe.ray(gap as f32 + 0.5, edge_row).unit_or(fwd).scale(9.0);

    let dusk_seed = rng.range(0.0, 1.0);
    Box::new(Bats {
        sim: Sim::new(seed, fwd, right, up, roost, 16),
        warped: 0.0,
        steps: 0,
        moon,
        moon_elev: moon_el.to_degrees(),
        tree,
        seen: 0,
        dusk_seed,
    })
}

/// A jagged tree/roofline silhouette across the bottom of the frame, from the
/// seed, with one gap - a real break in the line, wide enough to read as a
/// gap and not a notch - for the colony to pour out of.
fn build_tree(rng: &mut crate::rng::Rng) -> (Vec<f32>, usize) {
    let mut h = vec![0.0_f32; W];
    let mut cur = rng.range(2.2, 4.0);
    for v in h.iter_mut() {
        cur += rng.range(-0.85, 0.85);
        if rng.f32() < 0.08 {
            cur += rng.range(1.0, 2.2);
        }
        cur = cur.clamp(1.0, 6.5);
        *v = cur;
    }
    let gap = rng.range(12.0, (W - 13) as f32) as usize;
    let width = 5_i32;
    for (x, v) in h.iter_mut().enumerate() {
        let dx = x as i32 - gap as i32;
        if dx.abs() <= width * 2 {
            let t = (dx as f32 / (width * 2) as f32).clamp(-1.0, 1.0);
            let dip = (1.0 - t * t).max(0.0);
            *v = (*v - dip * 2.6).max(0.6);
        }
    }
    (h, gap)
}

impl Bats {
    fn tuning(ctx: &Ctx) -> Tuning {
        Tuning {
            pace: ctx.get("pace"),
            jink: ctx.get("jink"),
            loose: ctx.get("loose"),
            beat_hz: ctx.get("beat"),
            stream: ctx.get("stream"),
        }
    }

    fn advance(&mut self, ctx: &Ctx, tune: &Tuning) {
        self.warped += ctx.dt.clamp(0.0, 0.25) * f64::from(ctx.get("pace"));
        let target = (self.warped / f64::from(STEP) + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.sim.step(tune, STEP);
            self.steps += 1;
        }
    }
}

impl Patch for Bats {
    fn playing(&self) -> Option<Playing> {
        Some(Playing {
            title: format!("{} bats", self.sim.bats.len()),
            detail: if self.sim.pouring() {
                format!("{} in frame, pouring out", self.seen)
            } else {
                format!("{} in frame, nearest {:.1} m", self.seen, self.sim.nearest())
            },
            actions: Vec::new(),
            notes: Vec::new(),
        })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        let tune = Bats::tuning(ctx);
        self.sim.resize(ctx.get("bats") as usize);
        self.advance(ctx, &tune);

        // `dusk_seed` gives every seed a slightly different starting point on
        // the ramp, so "another one like this" is a different evening and
        // not just a different colony; `cycle` (0 holds it still) drifts it
        // on from there, a full pass of the ramp every four minutes at 1.
        let dusk = (ctx.get("dusk") + self.dusk_seed * 0.12 + ctx.get("cycle") * (ctx.t / 240.0) as f32)
            .rem_euclid(1.0);
        let which = ctx.get("scheme") as usize;
        let (bands, bat_colour) = scheme(which, dusk, self.moon_elev);
        let colours = palette(&bands, bat_colour, ctx.get("color"));

        let moon_size = ctx.get("moon").max(0.05);
        let disc_lo = 1.0 - (1.0 - 0.998_2) * moon_size;
        let disc_hi = disc_lo + 0.0006 * moon_size;
        let halo_k = 50.0 / moon_size;
        let focal = 0.5 * W as f32 / (0.5 * FOV.to_radians()).tan();
        let (fwd, right, up) = self.sim.cam();
        let view = View { right, up, fwd, focal, moon: self.moon, disc_lo, disc_hi, halo_k };

        let ss = SUPERSAMPLE;
        let mut cover = Coverage::new(ss);
        let night = which == 1;
        let size = ctx.get("size");
        self.seen = draw_bats(&self.sim, &view, &mut cover, night, size);

        let ink_dither = Dither::BlueNoise;
        let sky_dither = Dither::Bayer4;
        let indices = (0..N)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                if H as f32 - self.tree[x] <= y as f32 + 0.5 {
                    return 0_u8;
                }
                let sky_bias = sky_dither.threshold(x, y);
                let mut band = 0.0;
                for j in 0..ss {
                    for k in 0..ss {
                        let fx = x as f32 + (k as f32 + 0.5) / ss as f32;
                        let fy = y as f32 + (j as f32 + 0.5) / ss as f32;
                        band += view.band_at(view.ray(fx, fy));
                    }
                }
                band /= (ss * ss) as f32;
                let b = (band + sky_bias).round().clamp(0.0, (BANDS - 1) as f32) as usize;
                let bias = ink_dither.threshold(x, y);
                let ink = cover.pixel(x, y) * (INK - 1) as f32;
                let k = (ink + bias).round().clamp(0.0, (INK - 1) as f32) as usize;
                (1 + b * INK + k) as u8
            })
            .collect();

        Frame::Indexed { palette: colours, indices }
    }
}

/// How wide a bat has to be drawn, in LEDs, before it is given a wing's
/// surface rather than its skeleton. Bats are small subjects on this panel -
/// a full-grown one is a third of a bird's real-world size - so the surface
/// is allowed to grow in earlier than flock's birds do.
const AREA: (f32, f32) = (3.0, 7.0);

/// Every bat, far to near. Returns how many landed on the panel.
fn draw_bats(sim: &Sim, view: &View, cover: &mut Coverage, night: bool, size: f32) -> usize {
    let floor = if night { HAZE_NIGHT } else { HAZE_DUSK };
    let mut order: Vec<(f32, usize)> = Vec::with_capacity(sim.bats.len());
    for (i, b) in sim.bats.iter().enumerate() {
        if let Some((_, _, z)) = view.project(b.pos) {
            order.push((z, i));
        }
    }
    order.sort_by(|a, b| b.0.total_cmp(&a.0));

    let span_m = sim::SPAN * size;
    let mut seen = 0;
    for (z, i) in order {
        let bat = sim.bats[i];
        let span = span_m * view.focal / z;
        let pose = wing::pose(bat, span_m, smoothstep(AREA.0, AREA.1, span));

        let haze = floor + (1.0 - floor) * (-(z / 13.0).powf(1.5)).exp();
        // Close-up fade, same reasoning as flock: a bat that swoops closer
        // than the camera's own personal space fades rather than filling the
        // panel.
        let ink = haze * smoothstep(0.6, 1.6, z);
        if ink < 0.02 {
            continue;
        }

        let flat = |p: V3| view.project(p).map(|(x, y, _)| (x, y));
        let Some(nose) = flat(pose.body[0]) else { continue };
        let margin = span + 3.0;
        if nose.0 < -margin || nose.0 > W as f32 + margin || nose.1 < -margin || nose.1 > H as f32 + margin {
            continue;
        }
        let Some(body) = pose.body.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else { continue };
        seen += 1;

        let to_bat = pose.body[0];
        let away = 1.0 / to_bat.len().max(1e-6);
        for w in &pose.wings {
            let Some(spar) = w.spar.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else { continue };
            let Some(trail) = w.trail.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else { continue };
            let facing = (to_bat.dot(w.normal) * away).clamp(-1.0, 1.0);
            let lit = (ink * (1.0 + if night { -0.14 } else { 0.14 } * facing)).min(1.0);
            // Fanned from the shoulder across the scalloped trailing edge -
            // three small triangles instead of one, so each notch
            // anti-aliases on its own.
            cover.triangle(spar[0], trail[0], trail[1], lit);
            cover.triangle(spar[0], trail[1], trail[2], lit);
            cover.triangle(spar[0], trail[2], trail[3], lit);
            cover.triangle(spar[0], spar[1], trail[3], lit);
            cover.triangle(spar[1], spar[2], trail[3], lit);
            // These two strokes are what a distant bat actually is: at low
            // LOD the wing surface has no chord (see `wing::pose`) and every
            // triangle above is degenerate, so the leading-edge spar is the
            // whole picture. It needs the same kind of floor flock's wing
            // strokes have (`(span * 0.13).clamp(0.38, 1.05)`) - without one,
            // a two-LED bat's stroke width rounds to a fraction of a pixel
            // and never reaches the coverage buffer at all.
            cover.stroke(spar[0], spar[1], (span * 0.11).clamp(0.36, 0.85), lit);
            cover.stroke(spar[1], spar[2], (span * 0.08).clamp(0.32, 0.65), lit);
        }
        let body_w = (span * 0.11).clamp(0.40, 1.0);
        cover.stroke(body[0], body[1], body_w, ink);
        cover.stroke(body[1], body[2], body_w * 0.85, ink);
        if let Some(ears) = pose.ears {
            if let (Some(a), Some(b)) = (flat(ears[0]), flat(ears[1])) {
                let w = (span * 0.03).clamp(0.30, 0.6);
                cover.stroke(body[0], a, w, ink);
                cover.stroke(body[0], b, w, ink);
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests;
