//! Bats: a glowing moon in black space, and a few lit bats crossing it.
//!
//! Card 319 - the owner's second pass on card 313, 2026-09-26: "a form of
//! visual poetry", "busyness is a thing we are trying to avoid", "we'll
//! prefer foreground animations against a black backdrop." The dusk sky and
//! tree line are gone; the moon is now the subject, not a small glow on a
//! bigger picture - real surface ([`moon`]: maria and craters, lit at a
//! phase, limb darkening, a faint halo), and the colony is small and calm,
//! most of the time one bat or none. The flight is [`sim`]; the bat's own
//! silhouette is [`wing`]; this file is everything you can see.
//!
//! **CPU, not GPU** - see the Log for the full reasoning, in short: the
//! moon's shading (an analytic sphere, ray-traced per supersample sample) and
//! the bats' coverage rasterising need no parallel throughput a 64x32 panel
//! can't already get from the CPU in the time this has to run, and staying
//! CPU keeps the "seeded, indexed, exact frame" promise the autumn set's
//! rules ask for without writing a new WGSL pipeline and a palette-readback
//! path for it. Nothing here touches `crates/art/src/gpu/` or `patches/mod.rs`'s
//! `GPU_PATCHES` list.
//!
//! **This is a sibling of `flock`, not a fork of it** (card 313's own doc
//! comment, carried forward): a seeded, indexed CPU patch with a camera, an
//! anti-aliased coverage buffer and a level-of-detail model, but a fixed
//! camera and event-driven jinking rather than flock's flying camera and
//! continuous boid steering. [`Coverage`] below and `sim::V3` are copied from
//! `flock/mod.rs` and `flock/sim.rs` (generic rasterising and vector code, not
//! flock-specific logic) rather than imported, so flock never has to bend to
//! fit a second patch's needs.

pub(crate) mod moon;
pub(crate) mod sim;
pub(crate) mod wing;

use crate::color::{oklch, smoothstep, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use moon::Moon;
use sim::{v3, Bat, Sim, Tuning, V3, STEP};
use std::collections::VecDeque;

pub const DEF: PatchDef = PatchDef {
    id: "bats",
    name: "Bats",
    blurb: "A glowing moon in black space, and a few lit bats crossing it: calm, foreground motion against true black.",
    params: PARAMS,
    make,
    // The moon's own placement, size-independent surface (craters, maria)
    // and the colony's own placement and temperament all come from the
    // seed, same promise flock and the first pass both made.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    // Card 319's own words are "max at once, low default". `rate.rs` briefly
    // forced this higher (see the Log) until the orchestrator fixed that
    // test on `main` (a peak-per-pixel-change escape hatch for exactly this
    // kind of sparse, calm patch) - back to a genuinely low default now that
    // the test no longer fights the brief.
    param("bats", "How many bats live in the colony", 1.0, 10.0, 1.0, 1.0),
    param("pace", "How fast the flight moves", 0.3, 2.5, 0.05, 1.0),
    param("jink", "How often it changes its mind, and how sharply", 0.0, 1.0, 0.01, 0.85),
    param("loose", "How loosely the colony holds together", 0.0, 1.0, 0.01, 0.55),
    // A little bigger than a strict "distant and small" reading would pick,
    // so the wing surface and the scalloped trailing edge actually grow in
    // at ordinary roaming depth now and again, not only on a rare close
    // swoop - see the Log for the taste call.
    param("size", "How big the bats are drawn (a longer lens, not a closer camera)", 0.5, 3.0, 0.1, 1.8),
    param("beat", "How fast the wings beat (Hz)", 4.0, 14.0, 0.5, 8.5),
    param("moon", "How big the moon is", 0.4, 2.2, 0.05, 1.0),
    param("phase", "The moon's phase: 0 and 1 are new, 0.5 is full", 0.0, 1.0, 0.01, 0.62),
    param("stream", "How often a small group streams past together", 0.0, 1.0, 0.01, 0.2),
    param("color", "How much colour (0 is pure grayscale)", 0.0, 1.0, 0.01, 0.45),
];

/// Horizontal field of view. Narrower than the first pass's 76 degrees: this
/// is a long lens on one big subject, not a wide sky, and a longer lens is
/// also what makes a distant bat cross more of the frame per metre of real
/// flight - useful when the whole point is a slow, legible crossing.
const FOV: f32 = 60.0;

const SUPERSAMPLE: usize = 3;

/// How many moments across each frame's interval are rendered and averaged
/// for the bats' motion blur (the moon does not move within a frame, so only
/// the bats' coverage is resampled). Same reasoning and the same number as
/// the since-removed `skeletons` patch (card 328's Log, 2026-09-26): the
/// owner lifted the compute-cost limit and asked by name for motion blur;
/// four moments is enough that a wingbeat leaves a real, soft trail rather than a stutter.
const BLUR_SAMPLES: usize = 4;

/// The background/moon brightness ramp, quantised into this many steps -
/// generous, since the moon's own terminator and craters are the patch's
/// main subject and a coarse ramp would band across them.
const BANDS: usize = 30;
/// Levels of bat opacity over the background. Small on purpose, same
/// reasoning as the first pass: the ink axis is mostly anti-aliasing a thin
/// wing edge, never carrying its own depth cue.
const INK: usize = 8;

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
const FLOOR_L: f32 = 0.06;

fn paint((l, c, h): Lch) -> Rgb {
    if l < FLOOR_L {
        Rgb::BLACK
    } else {
        oklch(l, c, h)
    }
}

/// "Well below full white, luxurious rather than flat" (card 319's picture):
/// the brightest the ramp ever reaches, at the moon's own sub-solar point.
const MAX_MOON_L: f32 = 0.80;

/// Where [`Moon::value`]'s own scale stops being "only the halo" and starts
/// being "the disc" in earnest - see [`band_colour`]'s doc for why the ramp
/// treats the two halves differently.
const DISC_U: f32 = 0.10;

/// The background ramp at `u` (`0..1`, [`moon::Moon::value`]'s own scale):
/// true black off the moon, up through the halo, up through the moon's own
/// shaded, textured surface.
///
/// Above [`DISC_U`], `l` is a cube root of `u` (OKLCH lightness is roughly
/// linear brightness cubed for a neutral colour), which is what keeps the terminator
/// and the craters' own shading from crushing into black before they have
/// had their say. Below it - which is only ever the halo, and the smooth
/// blend right at the limb - that same cube root would do the opposite of
/// what a "faint halo" (card 319's picture) asks for: it stretches a tiny,
/// physically near-zero glow into a lightness a person can still see, so a
/// halo tuned to fade out within a few degrees of the limb instead lingered
/// as a visible grey smear across a third of the panel (found by rendering
/// and looking, not by inspection - the bug the "read your own renders"
/// rule exists for). Below `DISC_U`, `l` is plain linear in `u` instead,
/// continuous with the cube-root curve at the seam, so a faint glow actually
/// reaches true black a short, honest distance from where it started.
fn band_colour(u: f32, color: f32) -> Lch {
    let u = u.clamp(0.0, 1.0);
    let seam = MAX_MOON_L * DISC_U.cbrt();
    let l = if u >= DISC_U { MAX_MOON_L * u.cbrt() } else { seam * (u / DISC_U) };
    let h = mix_hue(48.0, 228.0, smoothstep(0.04, 0.6, u));
    let c = color * (0.015 + 0.05 * smoothstep(0.0, 0.7, u));
    (l, c, h)
}

/// What a bat's own surface looks like when it is fully opaque at background
/// level `u`: a dark silhouette once the background itself is bright (over
/// the moon's own disc - "dark silhouette when fully in front of the moon's
/// disc", card 319's picture) fading to a pale, moonlit grey once the
/// background is near black. The same `u` this pixel's background band was
/// quantised from, so the flip from silhouette to pale happens exactly where
/// the bat actually crosses the limb, not by a scene-wide switch the first
/// pass needed (its "dusk" vs "night" `scheme` choice).
fn bat_material(u: f32, color: f32) -> Lch {
    let silhouette: Lch = (0.045, 0.0, 0.0);
    let moonlit: Lch = (0.70, color * 0.045, 205.0);
    // Steep on purpose: a bat is opaque, so it wants to read as a *clear*
    // silhouette as soon as the background behind it is doing any real work
    // (past the halo and the terminator's own dim shadow side), not a washy
    // half-blend the first render revealed - a bat over the lit two thirds
    // of the disc barely darker than the disc itself, easy to miss entirely
    // among the crater texture already varying every dot around it.
    mix(moonlit, silhouette, smoothstep(0.05, 0.16, u.clamp(0.0, 1.0)))
}

fn build_palette(color: f32) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(BANDS * INK);
    for i in 0..BANDS {
        let u = i as f32 / (BANDS - 1) as f32;
        let band = band_colour(u, color);
        let bat = bat_material(u, color);
        for k in 0..INK {
            out.push(paint(mix(band, bat, k as f32 / (INK - 1) as f32)));
        }
    }
    out
}

// --------------------------------------------------------------------------
// Copied from `flock::Coverage` (card 313's Log): the same anti-aliased
// stroke/triangle rasteriser, unmodified in substance. Flock has to keep
// rendering byte-identically, so this is a copy rather than a shared module -
// see the module doc for the follow-up card that would fix that properly.
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

/// The fixed camera: level, on the ground or a stand, looking at the moon.
/// Built once from the seed and never moved.
struct View {
    right: V3,
    up: V3,
    fwd: V3,
    focal: f32,
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
}

// --------------------------------------------------------------------------

struct Bats {
    sim: Sim,
    warped: f64,
    steps: i64,
    moon: Moon,
    seen: usize,
    /// Every simulated state from roughly the last [`BLUR_WINDOW`] sim-seconds,
    /// oldest first, persisted *across* render calls rather than rebuilt
    /// inside one - see [`Bats::advance`]'s doc for why that persistence is
    /// what makes the motion blur frame-rate independent.
    history: VecDeque<(f64, Vec<Bat>)>,
}

/// Longest catch-up after a stall, in fixed steps - same reasoning as flock's
/// `CATCHUP`: a paused studio resumes hunting, it does not fast-forward.
const CATCHUP: i64 = 240;

/// How far back the motion blur looks, in *sim* seconds - a fixed quantity
/// of simulated time, not of wall-clock frame time. `sim::Sim::step` always
/// advances its own clock by exactly [`STEP`] regardless of `pace` or the
/// render's frame rate (pace changes how many steps a render call takes,
/// never what one step means), so a window measured in sim-seconds is
/// automatically the same physical stretch of flight at 30 fps, 60 fps, or
/// any `pace` - which is what keeps `the_flight_does_not_depend_on_the_frame_rate`
/// true with blur turned on. Two steps: about one nominal video frame.
const BLUR_WINDOW: f64 = 2.0 * STEP as f64;
/// How many of [`Bats::history`]'s entries to keep. Only the last two or
/// three are ever read (`BLUR_WINDOW`'s worth), but a generous cushion costs
/// nothing (a `Vec<Bat>` of a handful of bats) and comfortably covers a
/// stall's catch-up taking many steps in one render call.
const HISTORY_CAP: usize = 16;

/// Everything [`make`] builds, as a concrete, inspectable value - split out
/// so `tests.rs` can drive the real geometry (the moon's actual screen
/// position, not a stand-in camera) directly, rather than only through the
/// `Patch` trait object `make` hands back to everyone else.
fn build(seed: u64) -> Bats {
    let mut rng = crate::rng::Rng::new(seed ^ 0x00ba_751e);
    let world_up = v3(0.0, 1.0, 0.0);
    // A level camera: there is no ground or horizon left to pitch it
    // against, only the moon, so its own placement carries the composition.
    let fwd = v3(0.0, 0.0, 1.0);
    let right = world_up.cross(fwd).unit_or(v3(1.0, 0.0, 0.0));
    let up = fwd.cross(right).unit_or(world_up);

    // The moon, placed so it usually sits well inside the frame but is not
    // glued to the centre - "it may sit partly off an edge if that composes
    // better" (card 319's picture).
    let moon_az = rng.range(-14.0_f32, 14.0).to_radians();
    let moon_el = rng.range(-6.0_f32, 9.0).to_radians();
    let (sa, ca) = moon_az.sin_cos();
    let (se, ce) = moon_el.sin_cos();
    let moon_dir = fwd.scale(ca * ce).add(right.scale(sa * ce)).add(up.scale(se)).unit_or(fwd);
    let moon = Moon::new(&mut rng, moon_dir);

    // The ambient roaming cone is centred on the moon's own direction, not
    // dead ahead - see the Log (orchestrator review round 1): bats that
    // wander around screen centre while the moon sits off to one side cross
    // it only by luck, and "crossings happening regularly" (the owner's
    // words) wants the colony's home range to actually be built around its
    // one bright landmark. `sim::gnomonic_of` inverts the same planar
    // convention `sim::spherical` builds targets from, so the cone's centre
    // and the moon's own screen position agree by construction.
    let (moon_az_g, moon_el_g) = sim::gnomonic_of(fwd, right, up, moon_dir);

    let sim = Sim::new(seed, fwd, right, up, 3, moon_az_g, moon_el_g);
    let mut history = VecDeque::with_capacity(HISTORY_CAP);
    history.push_back((sim.t(), sim.bats.clone()));
    Bats { sim, warped: 0.0, steps: 0, moon, seen: 0, history }
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(build(seed))
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

    /// Steps the flight up to where this frame's clock has reached, appending
    /// every simulated state it passes through to [`Bats::history`] - which
    /// persists across calls, unlike a per-frame trace would, so the motion
    /// blur can always look back a fixed [`BLUR_WINDOW`] of *sim* time even
    /// when this particular render call took only one step or none (a high
    /// frame rate, or a pause).
    fn advance(&mut self, ctx: &Ctx, tune: &Tuning) {
        self.warped += ctx.dt.clamp(0.0, 0.25) * f64::from(ctx.get("pace"));
        let target = (self.warped / f64::from(STEP) + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.sim.step(tune, STEP);
            self.steps += 1;
            self.history.push_back((self.sim.t(), self.sim.bats.clone()));
            if self.history.len() > HISTORY_CAP {
                self.history.pop_front();
            }
        }
    }

    /// The camera and the moon's current angular radius and sun direction,
    /// exactly as `render` draws them - shared with it (not just kept in
    /// step with it) so `tests.rs` can find the moon's own screen position
    /// without duplicating this arithmetic and risking the two drifting
    /// apart.
    fn geometry(&self, ctx: &Ctx) -> (View, f32, V3) {
        let focal = 0.5 * W as f32 / (0.5 * FOV.to_radians()).tan();
        let (fwd, right, up) = self.sim.cam();
        let view = View { right, up, fwd, focal };
        let moon_size = ctx.get("moon").max(0.05);
        let ang_r = (ANG_R_BASE * moon_size).to_radians();
        let phase = ctx.get("phase").rem_euclid(1.0);
        let light = self.moon.light_for(phase);
        (view, ang_r, light)
    }
}

/// At `moon` 1.0 the disc is about two fifths of the panel's height - "a
/// third to half the panel height or more" (card 319's picture).
const ANG_R_BASE: f32 = 7.0;

/// A bat state part-way through [`Bats::history`], at simulated time `t`
/// (linear interpolation between the two bracketing recorded states - see
/// `Bat::interpolate`'s doc for why that is a sound way to draw motion blur
/// on top of a fixed-timestep simulation without re-simulating anything).
/// `t` before the earliest recorded state clamps to it rather than
/// extrapolating - only reachable right at start-up, before `history` has
/// `BLUR_WINDOW`'s worth behind it.
fn bats_at(history: &[(f64, Vec<Bat>)], t: f64) -> Vec<Bat> {
    if history.len() < 2 {
        return history[0].1.clone();
    }
    let mut idx = 1;
    while idx < history.len() - 1 && history[idx].0 < t {
        idx += 1;
    }
    let (t0, a) = &history[idx - 1];
    let (t1, b) = &history[idx];
    let span = (t1 - t0).max(1e-9);
    let frac = ((t - t0) / span).clamp(0.0, 1.0) as f32;
    a.iter().zip(b.iter()).map(|(pa, pb)| Bat::interpolate(pa, pb, frac)).collect()
}

impl Patch for Bats {
    fn playing(&self) -> Option<Playing> {
        Some(Playing {
            title: format!("{} bats", self.sim.bats.len()),
            detail: if self.sim.grouping() {
                format!("{} in frame, a group streaming past", self.seen)
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
        let (view, ang_r, light) = self.geometry(ctx);
        let history: &[(f64, Vec<Bat>)] = self.history.make_contiguous();

        let color = ctx.get("color");
        let palette = build_palette(color);

        let size = ctx.get("size");
        let ss = SUPERSAMPLE;
        let t1 = self.sim.t();
        let t0 = t1 - BLUR_WINDOW;
        let mut last_seen = 0;
        let covers: Vec<Coverage> = (0..BLUR_SAMPLES)
            .map(|i| {
                let frac = (i as f64 + 0.5) / BLUR_SAMPLES as f64;
                let t = t0 + (t1 - t0) * frac;
                let bats = bats_at(history, t);
                let mut cov = Coverage::new(ss);
                last_seen = draw_bats(&bats, &view, &mut cov, light, size);
                cov
            })
            .collect();
        self.seen = last_seen;

        let band_dither = Dither::Bayer4;
        let ink_dither = Dither::BlueNoise;
        let indices = (0..N)
            .map(|i| {
                let (x, y) = (i % W, i / W);
                let mut u = 0.0;
                for j in 0..ss {
                    for k in 0..ss {
                        let fx = x as f32 + (k as f32 + 0.5) / ss as f32;
                        let fy = y as f32 + (j as f32 + 0.5) / ss as f32;
                        u += self.moon.value(view.ray(fx, fy), ang_r, light);
                    }
                }
                u /= (ss * ss) as f32;
                let band_bias = band_dither.threshold(x, y);
                let b = (u * (BANDS - 1) as f32 + band_bias).round().clamp(0.0, (BANDS - 1) as f32) as usize;
                let ink = covers.iter().map(|c| c.pixel(x, y)).sum::<f32>() / BLUR_SAMPLES as f32;
                let ink_bias = ink_dither.threshold(x, y);
                let k = (ink * (INK - 1) as f32 + ink_bias).round().clamp(0.0, (INK - 1) as f32) as usize;
                (b * INK + k) as u8
            })
            .collect();

        Frame::Indexed { palette, indices }
    }
}

/// How wide a bat has to be drawn, in LEDs, before it is given a wing's
/// surface rather than its skeleton. Bats are small subjects on this panel -
/// a full-grown one is a third of a bird's real-world size - so the surface
/// is allowed to grow in earlier than flock's birds do.
const AREA: (f32, f32) = (3.0, 7.0);

/// A bat's base opacity never falls under this, however far off it is - a
/// distant bat is still a small flutter, not a smudge merged into the black.
const HAZE_FLOOR: f32 = 0.42;

/// The wing membrane is drawn a little less opaque than the body: it is thin
/// skin over finger bones, not solid like the body and skull, and letting a
/// touch of whatever is behind it show through (background or moon alike)
/// is this patch's stand-in for the card's "thin translucent glow where the
/// membrane is backlit against the moon" - most legible exactly where that
/// matters, over the bright disc, without needing a second material lane in
/// an already fairly large palette.
const WING_OPACITY: f32 = 0.86;

/// Every bat, far to near, at one instant. Returns how many landed on the
/// panel. `light` is the moon's current sun direction (see
/// [`Moon::light_for`]): a wing's own facing against it is "moonlight
/// catching the membrane's upper surface on the upstroke" (card 319's
/// picture).
fn draw_bats(bats: &[Bat], view: &View, cover: &mut Coverage, light: V3, size: f32) -> usize {
    let mut order: Vec<(f32, usize)> = Vec::with_capacity(bats.len());
    for (i, b) in bats.iter().enumerate() {
        if let Some((_, _, z)) = view.project(b.pos) {
            order.push((z, i));
        }
    }
    order.sort_by(|a, b| b.0.total_cmp(&a.0));

    let span_m = sim::SPAN * size;
    let mut seen = 0;
    for (z, i) in order {
        let bat = bats[i];
        let span = span_m * view.focal / z;
        let pose = wing::pose(bat, span_m, smoothstep(AREA.0, AREA.1, span));

        let haze = HAZE_FLOOR + (1.0 - HAZE_FLOOR) * (-(z / 13.0).powf(1.5)).exp();
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

        for w in &pose.wings {
            let Some(spar) = w.spar.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else { continue };
            let Some(trail) = w.trail.iter().map(|p| flat(*p)).collect::<Option<Vec<_>>>() else { continue };
            // `facing` is how squarely the membrane's own upper face is
            // turned towards the sun - a real wingbeat sweeps this from lit
            // (the upstroke's upper surface) to shadowed (the downstroke's
            // underside) every cycle.
            let facing = w.normal.dot(light).clamp(-1.0, 1.0);
            let sheen = 0.5 + 0.5 * facing;
            let lit = (ink * WING_OPACITY * (0.78 + 0.4 * sheen)).min(1.0);
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
            // whole picture. It needs a floor (flock's wing strokes have the
            // same kind: `(span * 0.13).clamp(0.38, 1.05)`) - without one, a
            // two-LED bat's stroke width rounds to a fraction of a pixel and
            // never reaches the coverage buffer at all.
            cover.stroke(spar[0], spar[1], (span * 0.11).clamp(0.36, 0.85), lit);
            cover.stroke(spar[1], spar[2], (span * 0.08).clamp(0.32, 0.65), lit);
            if let Some([a, b]) = w.thumb {
                if let (Some(a), Some(b)) = (flat(a), flat(b)) {
                    cover.stroke(a, b, (span * 0.03).clamp(0.28, 0.5), ink);
                }
            }
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
