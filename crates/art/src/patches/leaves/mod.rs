//! Leaves: autumn leaves falling with gravity, flutter and gusts, piling up
//! along the bottom.
//!
//! The flight is [`leaf`]: a simplified 2D flat-plate model per leaf (see that
//! module for the physics and why it is shaped the way it is) stepped on a
//! fixed timestep, exactly as `flock` steps its boids - `advance` below is
//! the same "simulated seconds asked for so far, vs. steps actually taken"
//! pattern, for the same reason: a snapshot at any render rate takes the same
//! steps at the same moments and so is the same picture.
//!
//! The wind is [`wind`]: layered noise in space and time, so groups of leaves
//! drift together rather than each finding its own way down.
//!
//! **The frame is indexed and exact**, as the card asks: the palette is one
//! flat background plus four leaf hues, each in two discrete lightness tiers
//! ("fresh" and "aged" - see [`FAMILIES`] and the module's tier logic) and
//! three coverage steps for anti-aliasing. `1 + 4 * 2 * 3 = 25` colours,
//! inside the 32 that are exact whatever the index plane looks like (brief
//! section 2.3) - and this picture, mostly one flat background with a
//! scatter of small shapes, is exactly the kind that also compresses.

mod leaf;
mod wind;

use crate::color::{oklch, smoothstep, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::patch::{choice, param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use leaf::{Build, Leaf};
use std::collections::VecDeque;
use std::f32::consts::PI;
use wind::Wind;

pub const DEF: PatchDef = PatchDef {
    id: "leaves",
    name: "Leaves",
    blurb: "Autumn leaves falling with gravity, flutter and gusts, piling up along the bottom.",
    params: PARAMS,
    make,
    // The wind field and every leaf's build (size, inertia, hue) and spawn
    // moment come from the seed, so "another one like this" is a different
    // gust pattern and a different scatter of leaves, not a relabelling.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("leaves", "How many leaves are in the air at once", 4.0, 40.0, 1.0, 16.0),
    param("wind", "Mean wind: the steady push downwind", 0.0, 2.5, 0.05, 0.6),
    param("gusts", "How strong the gusts are, and how often they hit", 0.0, 1.5, 0.05, 0.6),
    param("flutter", "How much a falling leaf rocks and tumbles", 0.0, 2.0, 0.05, 1.0),
    param("pile", "How high the fallen leaves may build up before the drain or a gust clears them", 0.0, 1.0, 0.02, 0.5),
    choice("scheme", "Light leaves on a dark ground, or dark leaves on a pale overcast sky", SCHEMES, 0.0),
    param("color", "Colour; 0 is grayscale", 0.0, 1.0, 0.01, 1.0),
];

/// The two pictures the card asks to have tried, in its own words.
const SCHEMES: &[&str] = &["dusk ground", "overcast sky"];

/// Safety clamp matching the `leaves` param's own max - `resize` never grows
/// past this even if a stored value somehow got past `sanitise`.
const MAX_LEAVES: usize = 40;

/// Discrete lightness tiers per hue: "fresh" high in the air, "aged" once a
/// leaf has fallen most of the way down. A leaf really does darken a touch as
/// it falls, but the brief's own dark-ramp guidance (section 2.1.1: "four
/// deliberate dark shades beat twelve computed ones") says to make that a
/// short list of chosen swatches, not a continuous fade, so it stays inside a
/// small exact palette. One switch, at [`FALL_AGE_FRAC`] of the way down, is
/// how a real leaf's colour reads anyway: gradual up close, a memory of two
/// or three shades from across a room.
const TIER: usize = 2;
/// Coverage steps for anti-aliasing a leaf's soft edge against the
/// background: none, half, and fully the leaf's own colour.
const INK: usize = 3;
const HUES: usize = 4;

/// How far down the panel (`0..1`) a leaf switches from its fresh tier to its
/// aged one.
const FALL_AGE_FRAC: f32 = 0.55;

/// Leaf radii, panel pixels: edge-on (about a pixel across) to broadside
/// (three to four), the spread the card asks for.
const R_EDGE: f32 = 0.55;
const R_BROAD: f32 = 1.95;
/// Softness of a leaf's edge, in pixels either side of its radius.
const AA: f32 = 0.6;

/// A leaf not yet landed or exited for this long is recycled outright: the
/// "no leaf stuck aloft forever" backstop. Nothing in the model should ever
/// reach this - gravity always wins eventually - but a gust field is noisy
/// enough that "always" is worth a hard bound rather than an assumption.
const MAX_ALOFT: f32 = 45.0;

/// Tallest the pile may ever be, in panel rows, at `pile = 1.0`: "a few rows
/// at most" per the card, and `PILE_MAX_ROWS / H` is under a sixth of the
/// panel even at full.
const PILE_MAX_ROWS: f32 = 5.0;
/// One landed leaf's thickness, in rows, before its own `size` scales it.
const LEAF_THICKNESS: f32 = 0.42;
/// How fast the pile erodes on its own, rows per second: slow enough that a
/// column collecting a few leaves holds a recognisable pile for minutes,
/// fast enough that a run left alone for a long time is not a still life. The
/// cap itself (`PileCol::add`, not this) is what stops the pile ever filling
/// the screen - drain is purely "it does not just sit there forever".
const DRAIN_ROWS_PER_SEC: f32 = 0.006;

/// How often the gust-relaunch condition is even checked, and how long after
/// one relaunch before another may fire - so a sustained strong wind produces
/// occasional swirls, not a constant fountain.
const GUST_CHECK_INTERVAL: f32 = 1.0;
const GUST_RELAUNCH_COOLDOWN: f32 = 6.0;
/// How strong the sampled wind has to be, at the ground, to count as the
/// "strong gust now and then" that lifts part of the pile.
const GUST_RELAUNCH_THRESHOLD: f32 = 2.6;

/// A named autumn shade: one hue, and the (lightness, chroma) of its fresh
/// and aged tiers under each [`SCHEMES`] entry - "dusk ground" (light leaves,
/// so both tiers stay well clear of black) and "overcast sky" (dark leaves,
/// so both tiers stay well clear of the pale sky).
struct HueFamily {
    hue: f32,
    dusk: [(f32, f32); TIER],
    overcast: [(f32, f32); TIER],
}

/// Maple red, amber, ochre, brown - the card's own words. Hues in OKLCH
/// degrees; (lightness, chroma) picked by eye against each background, then
/// checked in the render (see the Log).
const FAMILIES: [HueFamily; HUES] = [
    HueFamily { hue: 22.0, dusk: [(0.58, 0.17), (0.40, 0.13)], overcast: [(0.34, 0.15), (0.20, 0.11)] },
    HueFamily { hue: 64.0, dusk: [(0.66, 0.15), (0.46, 0.12)], overcast: [(0.40, 0.14), (0.24, 0.11)] },
    HueFamily { hue: 82.0, dusk: [(0.60, 0.11), (0.42, 0.09)], overcast: [(0.36, 0.10), (0.22, 0.08)] },
    HueFamily { hue: 45.0, dusk: [(0.46, 0.09), (0.30, 0.07)], overcast: [(0.26, 0.08), (0.16, 0.06)] },
];

fn swatch(fam: &HueFamily, tier: usize, scheme: usize, color: f32) -> Rgb {
    let (l, c) = if scheme == 0 { fam.dusk[tier] } else { fam.overcast[tier] };
    oklch(l, c * color, fam.hue)
}

fn bg_color(scheme: usize, color: f32) -> Rgb {
    // Quiet on purpose (the card: "keep the background quiet"): a single
    // flat colour, no gradient, nothing to compress or to compete with the
    // leaves for the eye.
    let (l, c, h) = if scheme == 0 { (0.045, 0.018, 250.0) } else { (0.84, 0.02, 95.0) };
    oklch(l, c * color, h)
}

fn palette_index(hue: usize, tier: usize, k: usize) -> u8 {
    (1 + (hue * TIER + tier) * INK + k) as u8
}

// --------------------------------------------------------------------------
// The pile: a heightfield of leaf colours along the bottom.
// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct PileLayer {
    thickness: f32,
    hue: u8,
    tier: u8,
}

#[derive(Clone, Debug, Default)]
struct PileCol {
    /// Oldest (bottom of the pile) first, newest (top) last: a plain stack.
    layers: Vec<PileLayer>,
}

impl PileCol {
    fn height(&self) -> f32 {
        self.layers.iter().map(|l| l.thickness).sum()
    }

    /// Add a freshly landed leaf's worth of material, clamped to `cap`: the
    /// only place height is ever added, so "the pile never exceeds its cap"
    /// is true by construction here, not by a check somewhere else.
    fn add(&mut self, thickness: f32, hue: u8, tier: u8, cap: f32) {
        let room = (cap - self.height()).max(0.0);
        let add = thickness.min(room);
        if add > 1e-4 {
            self.layers.push(PileLayer { thickness: add, hue, tier });
        }
    }

    /// Slow continuous erosion, most recently landed leaf first - the "and
    /// drains" half of the card's rule, independent of any gust.
    fn drain(&mut self, amount: f32) {
        let mut remaining = amount;
        while remaining > 1e-6 {
            let Some(top) = self.layers.last_mut() else { break };
            if top.thickness > remaining {
                top.thickness -= remaining;
                remaining = 0.0;
            } else {
                remaining -= top.thickness;
                self.layers.pop();
            }
        }
    }

    /// A gust's worth of material for a swirl: the single thickest layer,
    /// so a relaunch is a recognisable leaf's worth and not a sliver.
    fn pop_for_relaunch(&mut self) -> Option<PileLayer> {
        let (mut best_i, mut best_t) = (None, 0.0);
        for (i, l) in self.layers.iter().enumerate() {
            if l.thickness > best_t {
                best_t = l.thickness;
                best_i = Some(i);
            }
        }
        best_i.map(|i| self.layers.remove(i))
    }

    /// The hue and tier at height `h` above the ground, bottom to top.
    /// `None` above the pile's own top.
    fn at(&self, h: f32) -> Option<(u8, u8)> {
        let mut acc = 0.0;
        for l in &self.layers {
            acc += l.thickness;
            if h < acc {
                return Some((l.hue, l.tier));
            }
        }
        None
    }
}

/// Material a gust has lifted, waiting for a leaf slot to become it (see
/// [`LeavesPatch::respawn`]): reusing the ordinary respawn point rather than
/// growing the flock keeps the number of leaves in the air exactly `leaves`
/// at all times, gust or no gust.
struct Relaunch {
    x: f32,
    y: f32,
    hue: u8,
}

// --------------------------------------------------------------------------

struct LeavesPatch {
    rng: Rng,
    wind: Wind,
    /// Simulated seconds asked for so far, and steps actually taken - see
    /// [`LeavesPatch::advance`] and `flock`'s identical pattern.
    warped: f64,
    steps: i64,
    leaves: Vec<Leaf>,
    pile: Vec<PileCol>,
    pending: VecDeque<Relaunch>,
    gust_clock: f32,
    last_relaunch: f32,
    /// True until the first resize: the one moment leaves are scattered
    /// through the whole fall (and the pile pre-seeded) rather than spawned
    /// only at the entry edges, so frame one already looks mid-scene rather
    /// than like a fresh rain just starting (the card: "warm up invisibly").
    warm: bool,
}

/// The physics step, re-exported from [`leaf`] so the tests share one
/// definition of "a fixed step" with the model itself.
use leaf::STEP;

/// Longest catch-up after a stall, in fixed steps: about ten seconds, plenty
/// for a leaf to fall the height of the panel, so a studio paused for a
/// minute resumes falling rather than fast-forwarding through a minute of
/// leaves at once.
const CATCHUP: i64 = (10.0 / STEP) as i64;

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(new_patch(seed))
}

/// The concrete constructor `make` wraps. Split out so the tests can drive a
/// `LeavesPatch` directly - reading its pile and its flock of leaves to check
/// the acceptance criteria - without downcasting a `Box<dyn Patch>`.
fn new_patch(seed: u64) -> LeavesPatch {
    LeavesPatch {
        rng: Rng::new(seed ^ 0x6c_65_61_76),
        wind: Wind::new(seed),
        warped: 0.0,
        steps: 0,
        leaves: Vec::new(),
        pile: (0..W).map(|_| PileCol::default()).collect(),
        pending: VecDeque::new(),
        gust_clock: 0.0,
        last_relaunch: -GUST_RELAUNCH_COOLDOWN,
        warm: true,
    }
}

/// A leaf entering from the top or the upwind edge, as the card asks.
fn spawn_entering(rng: &mut Rng, mean_wind: f32) -> Leaf {
    let (pos, vel) = if rng.f32() < 0.25 {
        // The upwind edge: whichever side the wind is blowing *from*.
        let x = if mean_wind >= 0.0 { -rng.range(1.0, 3.0) } else { W as f32 + rng.range(1.0, 3.0) };
        let y = rng.range(0.0, H as f32 * 0.55);
        ((x, y), (mean_wind * 0.7, rng.range(0.2, 1.4)))
    } else {
        let x = rng.range(0.0, W as f32);
        let y = -rng.range(1.0, 4.0);
        ((x, y), (mean_wind * 0.3 + rng.range(-0.3, 0.3), rng.range(0.0, 1.0)))
    };
    build_leaf(rng, pos, vel)
}

/// A leaf scattered anywhere mid-fall, for the initial, invisible warm-up.
fn spawn_scattered(rng: &mut Rng, mean_wind: f32) -> Leaf {
    let pos = (rng.range(0.0, W as f32), rng.range(-2.0, H as f32 * 0.92));
    let vel = (mean_wind * 0.5 + rng.range(-0.4, 0.4), rng.range(0.5, 3.0));
    build_leaf(rng, pos, vel)
}

/// A gust's worth of pile lifted back into the air, in a swirl.
fn spawn_relaunch(rng: &mut Rng, r: Relaunch) -> Leaf {
    let pos = (r.x, r.y);
    let vel = (rng.range(-1.6, 1.6), -rng.range(2.2, 5.5));
    let mut leaf = build_leaf(rng, pos, vel);
    leaf.build.hue = r.hue;
    leaf.omega = rng.range(-5.0, 5.0);
    leaf
}

fn build_leaf(rng: &mut Rng, pos: (f32, f32), vel: (f32, f32)) -> Leaf {
    Leaf {
        pos,
        vel,
        theta: rng.range(0.0, PI),
        omega: rng.range(-1.0, 1.0),
        build: Build { size: rng.range(0.75, 1.3), inertia: rng.range(0.6, 1.6), hue: (rng.f32() * HUES as f32) as u8 },
        aloft: 0.0,
        shimmer: 0.5,
    }
}

impl LeavesPatch {
    fn resize(&mut self, n: usize, wind_mean: f32, pile_cap: f32) {
        let n = n.clamp(1, MAX_LEAVES);
        if self.warm {
            self.warm = false;
            self.leaves = (0..n).map(|_| spawn_scattered(&mut self.rng, wind_mean)).collect();
            self.prime_pile(pile_cap);
            return;
        }
        while self.leaves.len() < n {
            let l = spawn_entering(&mut self.rng, wind_mean);
            self.leaves.push(l);
        }
        self.leaves.truncate(n);
    }

    /// A modest, plausible amount of pile already there on frame one, well
    /// under the current cap, aged (it has been sitting a while).
    fn prime_pile(&mut self, pile_cap: f32) {
        let cap = PILE_MAX_ROWS * pile_cap;
        for col in &mut self.pile {
            if self.rng.f32() < 0.6 {
                let h = self.rng.range(0.0, cap * 0.35);
                let hue = (self.rng.f32() * HUES as f32) as u8;
                col.add(h, hue, 1, cap);
            }
        }
    }

    fn advance(&mut self, ctx: &Ctx, wind_mean: f32, gusts: f32, flutter: f32, pile_cap: f32) {
        self.warped += ctx.dt.clamp(0.0, 0.25);
        let target = (self.warped / f64::from(STEP) + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            let t = self.steps as f32 * STEP;
            self.step_once(t, wind_mean, gusts, flutter, pile_cap);
            self.steps += 1;
        }
    }

    fn step_once(&mut self, t: f32, wind_mean: f32, gusts: f32, flutter: f32, pile_cap: f32) {
        for col in &mut self.pile {
            col.drain(DRAIN_ROWS_PER_SEC * STEP);
        }

        self.gust_clock += STEP;
        if self.gust_clock >= GUST_CHECK_INTERVAL {
            self.gust_clock = 0.0;
            let (gx, _) = self.wind.at(W as f32 * 0.5, H as f32 - 1.0, t, wind_mean, gusts);
            if gx.abs() > GUST_RELAUNCH_THRESHOLD && t - self.last_relaunch > GUST_RELAUNCH_COOLDOWN {
                self.last_relaunch = t;
                for _ in 0..2 {
                    let x = (self.rng.f32() * W as f32) as usize % W;
                    if let Some(layer) = self.pile[x].pop_for_relaunch() {
                        let surface = H as f32 - self.pile[x].height();
                        self.pending.push_back(Relaunch { x: x as f32 + 0.5, y: surface, hue: layer.hue });
                    }
                }
            }
        }

        for i in 0..self.leaves.len() {
            self.leaves[i].step(&self.wind, t, wind_mean, gusts, flutter);
            let leaf = &self.leaves[i];

            let col = leaf.pos.0.round().clamp(0.0, (W - 1) as f32) as usize;
            let surface = H as f32 - self.pile[col].height();
            // The `aloft` guard (rather than requiring `vel.1 > 0.0`) is what
            // stops a leaf just relaunched from a gust - spawned exactly at
            // the surface, moving up - from "landing" again on its very next
            // step; a settling leaf's vertical speed can wobble near zero or
            // even briefly negative from lift as it flutters down, and that
            // is still a landing, not a bounce.
            let landed = leaf.pos.1 >= surface && leaf.aloft > 0.15;
            let off_side = leaf.pos.0 < -4.0 || leaf.pos.0 > W as f32 + 4.0;
            let stuck = leaf.aloft > MAX_ALOFT;

            if landed {
                let tier = u8::from(leaf.pos.1 > H as f32 * FALL_AGE_FRAC);
                let hue = leaf.build.hue;
                let cap = PILE_MAX_ROWS * pile_cap;
                let total = LEAF_THICKNESS * leaf.build.size;
                // A landed leaf spreads a little to either side instead of
                // spiking a single column, so a drift reads as a drift and
                // not a picket fence.
                self.pile[col].add(total * 0.6, hue, tier, cap);
                if col > 0 {
                    self.pile[col - 1].add(total * 0.2, hue, tier, cap);
                }
                if col + 1 < W {
                    self.pile[col + 1].add(total * 0.2, hue, tier, cap);
                }
                self.respawn(i, wind_mean);
            } else if off_side || stuck {
                self.respawn(i, wind_mean);
            }
        }
    }

    fn respawn(&mut self, i: usize, wind_mean: f32) {
        let fresh = match self.pending.pop_front() {
            Some(r) => spawn_relaunch(&mut self.rng, r),
            None => spawn_entering(&mut self.rng, wind_mean),
        };
        self.leaves[i] = fresh;
    }
}

impl Patch for LeavesPatch {
    fn playing(&self) -> Option<Playing> {
        let mean_height = self.pile.iter().map(PileCol::height).sum::<f32>() / self.pile.len().max(1) as f32;
        Some(Playing {
            title: format!("{} leaves aloft", self.leaves.len()),
            detail: format!("pile averages {mean_height:.1} rows deep"),
            actions: Vec::new(),
            notes: Vec::new(),
        })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        let wind_mean = ctx.get("wind");
        let gusts = ctx.get("gusts");
        let flutter = ctx.get("flutter");
        let pile_cap = ctx.get("pile");
        let scheme = ctx.get("scheme") as usize;
        let color = ctx.get("color");

        self.resize(ctx.get("leaves") as usize, wind_mean, pile_cap);
        self.advance(ctx, wind_mean, gusts, flutter, pile_cap);

        let bg = bg_color(scheme, color);
        let mut palette = vec![bg];
        for fam in &FAMILIES {
            for tier in 0..TIER {
                let sw = swatch(fam, tier, scheme, color);
                for k in 0..INK {
                    let t = k as f32 / (INK - 1) as f32;
                    palette.push(bg.lerp(sw, t));
                }
            }
        }

        let dither = Dither::BlueNoise;
        let mut idx = vec![0u8; N];
        let mut cover = vec![0.0f32; N];

        paint_pile(&self.pile, &dither, &mut idx);
        paint_leaves(&self.leaves, &dither, &mut idx, &mut cover);

        Frame::Indexed { palette, indices: idx }
    }
}

fn paint_pile(pile: &[PileCol], dither: &Dither, idx: &mut [u8]) {
    for (x, col) in pile.iter().enumerate() {
        let height = col.height();
        if height <= 0.001 {
            continue;
        }
        let full_rows = height.floor() as i32;
        for r in 0..full_rows {
            let row = H as i32 - 1 - r;
            if row < 0 {
                break;
            }
            if let Some((hue, tier)) = col.at(height - (r as f32 + 0.5)) {
                idx[row as usize * W + x] = palette_index(hue as usize, tier as usize, INK - 1);
            }
        }
        let frac = height - full_rows as f32;
        let row = H as i32 - 1 - full_rows;
        if frac > 0.02 && row >= 0 {
            if let Some(top) = col.layers.last() {
                let bias = dither.threshold(x, row as usize);
                let k = ((frac * (INK - 1) as f32) + bias).round().clamp(0.0, (INK - 1) as f32) as usize;
                idx[row as usize * W + x] = palette_index(top.hue as usize, top.tier as usize, k);
            }
        }
    }
}

fn paint_leaves(leaves: &[Leaf], dither: &Dither, idx: &mut [u8], cover: &mut [f32]) {
    for leaf in leaves {
        let shimmer = leaf.presented();
        let r = R_EDGE + (R_BROAD - R_EDGE) * shimmer;
        let (lx, ly) = leaf.pos;
        let lo_x = (lx - r - 1.0).floor().max(0.0) as usize;
        let hi_x = ((lx + r + 1.0).ceil().max(0.0) as usize).min(W);
        let lo_y = (ly - r - 1.0).floor().max(0.0) as usize;
        let hi_y = ((ly + r + 1.0).ceil().max(0.0) as usize).min(H);
        if lo_x >= hi_x || lo_y >= hi_y {
            continue;
        }
        let tier = usize::from(ly > H as f32 * FALL_AGE_FRAC);
        for y in lo_y..hi_y {
            for x in lo_x..hi_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = ((px - lx).powi(2) + (py - ly).powi(2)).sqrt();
                let c = smoothstep(r + AA, r - AA, d);
                if c <= 0.0 {
                    continue;
                }
                let p = y * W + x;
                if c > cover[p] {
                    let bias = dither.threshold(x, y);
                    let k = ((c * (INK - 1) as f32) + bias).round().clamp(0.0, (INK - 1) as f32) as usize;
                    cover[p] = c;
                    idx[p] = palette_index(leaf.build.hue as usize, tier, k);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
