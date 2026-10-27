//! Physarum: a slime-mould colony foraging on the panel.
//!
//! The owner, 2026-10-27: a piece "inspired by Sage Jenson's 36 Points", to
//! "make the most of the vast compute environment relative to the tiny
//! display size to create a visual experience that feels like magic even
//! despite the tiny resolution". Then, on the first version - six organisms
//! drifting past a window: "pretty hard to differentiate or make sense out
//! of ... basically rotating between various forms of blue noise". A
//! network's identity is in its filaments, and at 64 x 32 every network
//! averages to the same mesh. So this one is about what the colony *does*:
//! "we need to adapt the ideas for the actual medium". An experiment
//! (`exp/physarum`).
//!
//! **What is simulated.** Jones's agent model (2010): each agent senses the
//! trail ahead and to either side, turns toward the most, steps and
//! deposits; the trail spreads and fades. Jenson's extension makes every
//! agent parameter a law of the trail it stands in, `base + amp * s^exp`.
//!
//! **What is seen: foraging.** A few food sources sit on the panel,
//! breathing scent into the trail. The colony finds them, throws out fans,
//! and prunes back to the few thick veins that link them - the network the
//! Tokyo-rail experiment made famous. Food is eaten where the colony is
//! thick on it; a spent source goes dark, its veins starve, and the colony
//! reaches for the next one somewhere else. A handful of bright nodes, the
//! veins between them, and black: something the eye can follow at this
//! size.
//!
//! **The time.** Once a minute, food is scattered along the strokes of the
//! time (vesta's numerals, [`crate::faces::stroked`]) as morsels, a couple
//! of LEDs apart, arriving over a few seconds left to right. Nothing of the
//! time is drawn: the colony finds the morsels and links them with veins of
//! its own, so each numeral is the colony's guess at the glyph - bending,
//! shortcutting - and it eats them as it goes, so the time erodes unevenly
//! while you read it. The owner: "I'd kind of rather have an unstable
//! temporary rendering of the time than a fixed static render".
//!
//! **Why so many agents for 2048 LEDs.** The world is 512 x 256 cells, the
//! panel a window six cells to the LED with a margin all round; each LED is
//! the average over its footprint, with hundreds of agents behind it, and
//! each frame is exposed over several substeps. That is what keeps the
//! picture calm and its motion continuous.
//!
//! **Colour** comes from the colony's structure, not its density alone: the
//! agents lay down which way they lie as well as trail, so every vein has
//! an orientation, and that picks its hue from a muted palette - rust,
//! ochre, moss, verdigris, slate, mauve. Where veins cross the orientation
//! is mixed, and the colour goes to stone. Lightness is density. The owner:
//! "a muted but varied palette to provide some additional complexity that
//! the resolution lacks".
//!
//! Credit: Jeff Jones, "Characteristics of pattern formation and evolution
//! in approximations of Physarum transport networks" (Artificial Life,
//! 2010); Tero et al., "Rules for biologically inspired adaptive network
//! design" (Science, 2010); Sage Jenson, "36 Points" (2022); Antoine
//! Bleuje's interactive-physarum, studied, not copied.

mod gpu;

use crate::color::{oklch, smoothstep, to_oklab, Rgb};
use crate::dither::Dither;
use crate::faces::stroked;
use crate::frame::{Frame, H, W};
use crate::gpu::Gpu;
use crate::panel::aligned_ramp;
use crate::patch::{choice, param, toggle, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use gpu::{Params, Sim};

pub const DEF: PatchDef = PatchDef {
    id: "physarum",
    name: "Physarum",
    blurb: "GPU. A slime-mould colony foraging: it finds food, links it with veins, eats it and moves on. Once a minute it draws the time.",
    params: PARAMS,
    make,
    // The seed scatters the agents and places the food.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    choice("habit", "How the colony grows", HABIT_NAMES, 0.0),
    param("food", "How many food sources at once", 1.0, 8.0, 1.0, 4.0),
    param("appetite", "How fast a source is eaten (1: about a minute, well fed)", 0.0, 4.0, 0.05, 0.5),
    param("scent", "How strongly food draws the colony", 0.0, 4.0, 0.05, 1.0),
    toggle("time", "Draw the time once a minute", true),
    toggle("24h", "24-hour time", false),
    param("hold", "How long the time's food lasts at most, seconds", 5.0, 50.0, 1.0, 25.0),
    param(
        "gather",
        "How far the colony changes its habit to write the time (0: writes as it lives)",
        0.0,
        1.0,
        0.01,
        0.85,
    ),
    param("spacing", "How far apart the time's morsels are, LEDs (closer: a steadier numeral)", 1.2, 5.0, 0.1, 2.0),
    param("erode", "How fast the colony eats the time (1: a well-fed morsel lasts about 20 s)", 0.0, 4.0, 0.05, 1.0),
    param("scale", "World cells per LED: finer veins below, bolder above", 3.0, 7.0, 0.1, 6.0),
    param("palette", "Turns the palette: which orientation wears which colour", 0.0, 1.0, 0.01, 0.0),
    param("drift", "How fast the palette turns by itself, turns per hour", 0.0, 4.0, 0.05, 0.5),
    param("mute", "How much colour (0: stone only; 1: muted; 2: rich)", 0.0, 2.0, 0.01, 1.0),
    param("exposure", "How bright, against the world's mean trail", 0.05, 2.0, 0.01, 0.5),
    param("contrast", "How steeply the veins fall off into black", 0.5, 4.0, 0.05, 1.6),
    param("cap", "How much trail a vein can hold, times the mean", 1.5, 40.0, 0.5, 8.0),
    param("renewal", "Agents scattered afresh, thousandths per substep", 0.0, 5.0, 0.05, 0.2),
    param("agents", "Agents, millions (rebuilds the colony)", 0.1, 2.0, 0.05, 0.5),
    param("steps", "Substeps per frame: how smooth the motion is", 1.0, 8.0, 1.0, 4.0),
    param("tempo", "How fast the colony lives (1: as first built; lower is gentler)", 0.1, 1.5, 0.05, 0.5),
    param("glow", "How long an LED holds its light, seconds: motion glides rather than flutters", 0.0, 2.0, 0.05, 0.4),
];

/// The world, cells: the panel's window at the widest `scale` and a margin.
const WORLD_W: u32 = 512;
const WORLD_H: u32 = 256;
/// World cells to a food cell.
const FOOD_K: u32 = 4;
const FOOD_W: usize = (WORLD_W / FOOD_K) as usize;
const FOOD_H: usize = (WORLD_H / FOOD_K) as usize;

/// One substep's law for an agent parameter: `base + amp * s^exp`, `s` the
/// trail under the agent relative to the world's mean.
#[derive(Clone, Copy, Debug)]
struct Law(f32, f32, f32);

impl Law {
    const fn k(v: f32) -> Law {
        Law(v, 0.0, 1.0)
    }
    fn lerp(self, o: Law, t: f32) -> Law {
        Law(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t, self.2 + (o.2 - self.2) * t)
    }
}

/// How a colony grows: the laws for sensor distance (cells), sensor angle
/// and rotation angle (radians) and move distance (cells per substep), the
/// trail's fade and spread per substep, and a small random turn.
#[derive(Clone, Copy, Debug)]
struct Habit {
    sd: Law,
    sa: Law,
    ra: Law,
    md: Law,
    decay: f32,
    diffuse: f32,
    wobble: f32,
}

impl Habit {
    fn lerp(&self, o: &Habit, t: f32) -> Habit {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Habit {
            sd: self.sd.lerp(o.sd, t),
            sa: self.sa.lerp(o.sa, t),
            ra: self.ra.lerp(o.ra, t),
            md: self.md.lerp(o.md, t),
            decay: m(self.decay, o.decay),
            diffuse: m(self.diffuse, o.diffuse),
            wobble: m(self.wobble, o.wobble),
        }
    }
}

/// The habit the colony gathers toward to write the time: the classic
/// network, the one that holds a numeral's shape.
const WRITING: usize = 0;

const HABIT_NAMES: &[&str] = &["veins", "rivers", "worms"];

const HABITS: [Habit; 3] = [
    // Jones's classic network: narrow sensors, a wider turn.
    Habit {
        sd: Law::k(9.0),
        sa: Law::k(0.39),
        ra: Law::k(0.78),
        md: Law::k(1.0),
        decay: 0.06,
        diffuse: 0.6,
        wobble: 0.05,
    },
    // Long sensors, small turns: long flowing arcs. Takes the place of
    // "searching" (Jenson's turn-harder-where-thick law), which at a gentle
    // tempo circled in place into bright clumps and could not hold a
    // numeral (the owner: "has the problem of the clock being illegible").
    Habit {
        sd: Law(14.0, 4.0, 0.5),
        sa: Law::k(0.3),
        ra: Law::k(0.16),
        md: Law::k(1.0),
        decay: 0.05,
        diffuse: 0.6,
        wobble: 0.03,
    },
    // Faster where it is dense: travelling worms. Calmed (the owner:
    // "super frenetic and busy"): a gentler surge, less wobble.
    Habit {
        sd: Law::k(11.0),
        sa: Law::k(0.5),
        ra: Law::k(0.22),
        md: Law(0.5, 0.6, 1.0),
        decay: 0.07,
        diffuse: 0.5,
        wobble: 0.03,
    },
];

/// The palette's hues, around the loop of orientations: muted, mineral.
/// (OKLCH hue in degrees, chroma.) A vein wears the one nearest its
/// orientation; `palette` and `drift` turn which orientation is which.
const ANCHORS: [(f32, f32); 6] =
    [(35.0, 0.085), (78.0, 0.08), (122.0, 0.065), (178.0, 0.055), (240.0, 0.05), (318.0, 0.055)];
/// Stone: where the orientation is mixed. A warm grey.
const STONE: (f32, f32) = (80.0, 0.02);
/// Hues around the loop, and lightness steps per hue.
const HUES: usize = ANCHORS.len();
const LIGHTS: usize = 12;
/// The ramp's lightness, OKLab, darkest step to brightest. The darkest is
/// down among the panel's first few levels, so a vein falls away through
/// real steps before black rather than being cut out of it.
const L_DARK: f32 = 0.2;
const L_BRIGHT: f32 = 0.86;
/// Below this much of the first lightness step, black.
const FLOOR: f32 = 0.6;
/// How coherent a vein's orientation must be to wear a colour.
const COHERENT: f32 = 0.3;

/// The fixed step, seconds, and how many a stalled run may catch up.
const STEP: f64 = 1.0 / 30.0;
const CATCHUP: u32 = 3;

/// A food source, in LEDs.
struct Node {
    x: f32,
    y: f32,
    /// How big, LEDs, and how much is left (1 is a whole source).
    r: f32,
    left: f32,
    born: f64,
}

/// How long a source takes to fill in, and to fade once spent, seconds.
const NODE_IN: f64 = 8.0;
/// A source this close to empty is gone.
const SPENT: f32 = 0.02;
/// The time's food: fades in and out over these, seconds.
const TIME_IN: f64 = 3.0;
const TIME_OUT: f64 = 6.0;
/// While the time is drawn, the other food breathes at this share.
const QUIET: f32 = 0.1;
/// A time morsel's scent against a food source's, and its size, LEDs.
const TIME_SCENT: f32 = 2.2;
const MORSEL: f32 = 0.8;
/// The morsels arrive over this long, left to right, seconds, give or take
/// `ARRIVE_JITTER`.
const ARRIVE: f32 = 4.0;
const ARRIVE_JITTER: f32 = 1.5;

struct Physarum {
    seed: u64,
    /// `None` until first render; `Some(None)` if there is no GPU.
    sim: Option<Option<Sim>>,
    t: f64,
    owed: f64,
    substep: u32,
    nodes: Vec<Node>,
    dice: Rng,
    next_node: f64,
    /// The minute last drawn, and when it was laid down.
    minute: Option<i64>,
    time_at: f64,
    time_text: Vec<char>,
    /// The time's food, as morsels.
    morsels: Vec<Node>,
    food: Vec<f32>,
    /// The palette, and the `mute` it was worked out for.
    palette: Option<(f32, Vec<Rgb>)>,
    /// The last exposure, for how thick the colony is on each source.
    last_leds: Option<Vec<[f32; 4]>>,
    /// The exposure as shown, after the afterglow.
    glowing: Option<Vec<[f32; 4]>>,
    last: Option<Frame>,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Physarum {
        seed,
        sim: None,
        t: 0.0,
        owed: 0.0,
        substep: 0,
        nodes: Vec::new(),
        dice: Rng::new(seed ^ 0xf00d),
        next_node: 0.0,
        minute: None,
        time_at: f64::NEG_INFINITY,
        time_text: Vec::new(),
        morsels: Vec::new(),
        food: vec![0.0; FOOD_W * FOOD_H],
        palette: None,
        last_leds: None,
        glowing: None,
        last: None,
    })
}

fn agent_count(millions: f32) -> u32 {
    ((millions * 1e6) as u32).clamp(1 << 16, 8_000_000 - 1)
}

fn agents_for(n: u32, seed: u64) -> Vec<[f32; 4]> {
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|_| [rng.f32() * WORLD_W as f32, rng.f32() * WORLD_H as f32, rng.f32() * std::f32::consts::TAU, 0.0])
        .collect()
}

/// The panel's window on the world: its top-left cell, at `scale` cells per
/// LED, centred.
fn window(scale: f32) -> (f32, f32) {
    ((WORLD_W as f32 - W as f32 * scale) * 0.5, (WORLD_H as f32 - H as f32 * scale) * 0.5)
}

/// `HH:MM` (or `H:MM`) for the local time `now`.
fn time_text(now: f64, h24: bool) -> Vec<char> {
    let minutes = (now / 60.0).floor() as i64;
    let (h, m) = ((minutes / 60).rem_euclid(24), minutes.rem_euclid(60));
    let h = if h24 {
        h
    } else if h % 12 == 0 {
        12
    } else {
        h % 12
    };
    let text = if h24 { format!("{h:02}:{m:02}") } else { format!("{h}:{m:02}") };
    text.chars().collect()
}

/// Where each glyph of `text` sits, its centre in LEDs, centred on the
/// panel: a digit is vesta's 10 x 20 box and a gap, the colon narrower.
fn layout(text: &[char]) -> Vec<(char, f32)> {
    const DIGIT: f32 = 13.0;
    const COLON: f32 = 6.0;
    let advance = |c: char| if c == ':' { COLON } else { DIGIT };
    let total: f32 = text.iter().map(|c| advance(*c)).sum();
    let mut x = (W as f32 - total) * 0.5;
    text.iter()
        .map(|c| {
            let at = x + advance(*c) * 0.5;
            x += advance(*c);
            (*c, at)
        })
        .collect()
}

/// The time's food: morsels scattered along the strokes of `text`, about
/// `spacing` LEDs apart, each arriving a little after the one to its left.
fn morsels(text: &[char], spacing: f32, dice: &mut Rng, now: f64) -> Vec<Node> {
    const FINE: f32 = 0.25;
    let mid = H as f32 * 0.5;
    // Every point on a stroke's skeleton, on a fine grid, in a shuffled
    // order; then keep each that is clear of those kept.
    let mut on: Vec<(f32, f32)> = Vec::new();
    for (c, cx) in layout(text) {
        match c.to_digit(10) {
            Some(d) => {
                for j in 0..=((2.0 * stroked::B + 2.0) / FINE) as i32 {
                    for i in 0..=((2.0 * stroked::A + 2.0) / FINE) as i32 {
                        let (x, y) = (i as f32 * FINE - stroked::A - 1.0, j as f32 * FINE - stroked::B - 1.0);
                        if stroked::distance(d as usize, x, y) < FINE * 0.75 {
                            on.push((cx + x, mid + y));
                        }
                    }
                }
            }
            // The colon: its two dots.
            None => on.extend([(cx, mid - 5.0), (cx, mid + 5.0)]),
        }
    }
    for k in (1..on.len()).rev() {
        let j = (dice.u64() % (k as u64 + 1)) as usize;
        on.swap(k, j);
    }
    let mut kept: Vec<(f32, f32)> = Vec::new();
    for p in on {
        if kept.iter().all(|q| (p.0 - q.0).hypot(p.1 - q.1) >= spacing) {
            kept.push(p);
        }
    }
    kept.into_iter()
        .map(|(x, y)| {
            let arrive = ARRIVE * x / W as f32 + dice.range(0.0, ARRIVE_JITTER);
            Node {
                x: x + dice.range(-0.3, 0.3),
                y: y + dice.range(-0.3, 0.3),
                r: MORSEL,
                left: 1.0,
                born: now + f64::from(arrive),
            }
        })
        .collect()
}

impl Physarum {
    /// The habit now: the chosen one, gathered toward [`WRITING`] while the
    /// time is up - the colony changes how it lives to write, and relaxes
    /// back after, as 36 Points moves between its points.
    fn habit(&self, ctx: &Ctx) -> Habit {
        let own = HABITS[(ctx.get("habit") as usize).min(HABITS.len() - 1)];
        own.lerp(&HABITS[WRITING], ctx.get("gather") * self.time_strength(ctx))
    }

    /// The time's food now, 0..1: fading in when the minute turns, held,
    /// fading out.
    fn time_strength(&self, ctx: &Ctx) -> f32 {
        let age = self.t - self.time_at;
        let hold = f64::from(ctx.get("hold"));
        if age < 0.0 || age > hold + TIME_OUT {
            return 0.0;
        }
        let up = smoothstep(0.0, TIME_IN as f32, age as f32);
        let down = 1.0 - smoothstep(hold as f32, (hold + TIME_OUT) as f32, age as f32);
        up * down
    }

    /// Food comes and goes: a spent source is dropped, and a new one is
    /// placed a while later, away from the others.
    fn forage(&mut self, ctx: &Ctx, leds: Option<&[[f32; 4]]>, dt: f32, scale: f32) {
        let cap = ctx.get("cap");
        if let Some(leds) = leds {
            // How thick the colony is on a node, a share of the cap.
            let fed = |n: &Node| {
                let (cx, cy) = (n.x.round() as i32, n.y.round() as i32);
                let mut sum = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (x, y) =
                            ((cx + dx).clamp(0, W as i32 - 1) as usize, (cy + dy).clamp(0, H as i32 - 1) as usize);
                        sum += leds[y * W + x][0];
                    }
                }
                (sum / 9.0 / cap).clamp(0.0, 1.0)
            };
            let appetite = ctx.get("appetite") / 60.0;
            for n in &mut self.nodes {
                n.left -= dt * appetite * fed(n);
            }
            let erode = ctx.get("erode") / 20.0;
            for n in &mut self.morsels {
                if n.born <= self.t {
                    n.left -= dt * erode * fed(n);
                }
            }
        }
        self.nodes.retain(|n| n.left > SPENT);
        self.morsels.retain(|n| n.left > SPENT);
        if self.time_strength(ctx) <= 0.0 && self.t > self.time_at {
            self.morsels.clear();
        }
        let want = ctx.get("food") as usize;
        if self.nodes.len() < want && self.t >= self.next_node {
            let first = self.nodes.is_empty() && self.t < 1.0;
            // A few tries for a spot clear of the others.
            let mut best = (f32::MIN, 0.0, 0.0);
            for _ in 0..12 {
                let (x, y) = (self.dice.range(5.0, W as f32 - 5.0), self.dice.range(4.0, H as f32 - 4.0));
                let clear = self.nodes.iter().map(|n| (n.x - x).hypot((n.y - y) * 1.3)).fold(f32::MAX, f32::min);
                if clear > best.0 {
                    best = (clear, x, y);
                }
            }
            let left = if first { self.dice.range(0.4, 1.0) } else { 1.0 };
            self.nodes.push(Node {
                x: best.1,
                y: best.2,
                r: self.dice.range(0.7, 1.4) * 6.0 / scale,
                left,
                born: self.t,
            });
            // At the start, all at once; after, one at a time.
            self.next_node = if self.nodes.len() < want && self.t < 1.0 {
                self.t
            } else {
                self.t + f64::from(self.dice.range(2.0, 10.0))
            };
        }
    }

    /// The food grid: each source a soft blob, the time along its strokes.
    fn lay_food(&mut self, ctx: &Ctx, scale: f32) {
        let (ox, oy) = window(scale);
        let time = self.time_strength(ctx);
        let quiet = 1.0 - (1.0 - QUIET) * time;
        // A food cell's centre in panel LEDs.
        let to_led = |i: usize, j: usize| {
            (((i as f32 + 0.5) * FOOD_K as f32 - ox) / scale, ((j as f32 + 0.5) * FOOD_K as f32 - oy) / scale)
        };
        for j in 0..FOOD_H {
            for i in 0..FOOD_W {
                let (x, y) = to_led(i, j);
                let mut f = 0.0;
                for n in &self.nodes {
                    let grow = smoothstep(0.0, NODE_IN as f32, (self.t - n.born) as f32);
                    let d2 = ((x - n.x).powi(2) + (y - n.y).powi(2)) / (n.r * n.r);
                    f += grow * n.left.sqrt() * (-d2).exp() * quiet;
                }
                if time > 0.0 {
                    for n in &self.morsels {
                        let d2 = ((x - n.x).powi(2) + (y - n.y).powi(2)) / (n.r * n.r);
                        if d2 < 9.0 {
                            let grow = smoothstep(0.0, 1.5, (self.t - n.born) as f32);
                            f += TIME_SCENT * time * grow * n.left.sqrt() * (-d2).exp();
                        }
                    }
                }
                self.food[j * FOOD_W + i] = f;
            }
        }
    }

    /// [`palette_for`] at this frame's `mute`, worked out once per `mute`.
    fn palette(&mut self, ctx: &Ctx) -> Vec<Rgb> {
        let mute = ctx.get("mute");
        if let Some((m, p)) = &self.palette {
            if *m == mute {
                return p.clone();
            }
        }
        let palette = palette_for(mute);
        if std::env::var_os("PHYSARUM_PALETTE").is_some() {
            eprint!("{}", palette_dump(&palette));
        }
        self.palette = Some((mute, palette.clone()));
        palette
    }

    fn paint(&mut self, ctx: &Ctx, leds: &[[f32; 4]]) -> Frame {
        let (exposure, contrast) = (ctx.get("exposure"), ctx.get("contrast"));
        let turn = ctx.get("palette") + ctx.get("drift") * (ctx.now / 3600.0).rem_euclid(1.0) as f32;
        let dither = Dither::BlueNoise;
        let indices = leds
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let (x, y) = (i % W, i / W);
                let pos = (1.0 - (-(v[0] * exposure).max(0.0).powf(contrast)).exp()) * LIGHTS as f32;
                if pos < FLOOR {
                    return 0;
                }
                let light = (pos - 0.5 + dither.threshold(x, y)).round().clamp(0.0, LIGHTS as f32 - 1.0) as usize;
                let coherence = v[1].hypot(v[2]) / v[0].max(1e-6);
                // Hue and stone are not dithered: a vein keeps one colour
                // along its length, which is cheaper on the wire and reads
                // as a thing rather than as speckle.
                if coherence < COHERENT {
                    return (1 + HUES * LIGHTS + light) as u8;
                }
                // Orientation, 0..1 around the loop (a half turn of angle).
                let around = v[2].atan2(v[1]) / std::f32::consts::TAU + 0.5 + turn;
                let hue = ((around.rem_euclid(1.0) * HUES as f32).round() as usize) % HUES;
                (1 + hue * LIGHTS + light) as u8
            })
            .collect();
        Frame::Indexed { palette: self.palette(ctx), indices }
    }
}

/// The palette: black, then for each colour (the `HUES` anchors, then
/// stone) `LIGHTS` steps of lightness, every one a panel level triple
/// ([`aligned_ramp`]).
fn palette_for(mute: f32) -> Vec<Rgb> {
    let ramp = |hue: f32, chroma: f32| {
        aligned_ramp((0..LIGHTS).map(|i| {
            let t = i as f32 / (LIGHTS - 1) as f32;
            let l = L_DARK + (L_BRIGHT - L_DARK) * t;
            // Chroma is let in as the ramp rises (the bottom levels
            // cannot hold a hue anyway), and the densest veins pale
            // toward bone.
            let c = chroma * mute * smoothstep(0.0, 0.35, t) * (1.0 - 0.55 * t.powi(3));
            oklch(l, c, hue)
        }))
    };
    let ramps = ANCHORS.into_iter().chain([(STONE.0, STONE.1 / mute.max(0.5))]);
    std::iter::once(Rgb::BLACK).chain(ramps.flat_map(|(hue, chroma)| ramp(hue, chroma)).map(|a| a.rgb)).collect()
}

/// The `PHYSARUM_PALETTE` dump: one line per entry after black.
fn palette_dump(palette: &[Rgb]) -> String {
    let mut out = String::new();
    for (k, c) in palette.iter().enumerate().skip(1) {
        let [r, g, b] = c.to_srgb8();
        out += &format!(
            "physarum palette {:>2}/{:>2}: sRGB {r:>3} {g:>3} {b:>3}  L {:.3}\n",
            (k - 1) / LIGHTS,
            (k - 1) % LIGHTS,
            to_oklab(*c)[0]
        );
    }
    out
}

impl Patch for Physarum {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let wanted = agent_count(ctx.get("agents"));
        if matches!(&self.sim, Some(Some(s)) if s.agents() != wanted) {
            self.sim = None;
        }
        if self.sim.is_none() {
            self.sim = Some(match Gpu::shared() {
                Ok(gpu) => Some(Sim::new(gpu, WORLD_W, WORLD_H, &agents_for(wanted, self.seed), FOOD_W * FOOD_H)),
                Err(e) => {
                    eprintln!("screeny-art: physarum: {e}; rendering black");
                    None
                }
            });
        }
        self.owed += ctx.dt;
        let mut steps = 0;
        while self.owed >= STEP * 0.5 && steps < CATCHUP {
            self.owed -= STEP;
            steps += 1;
        }
        self.owed = self.owed.max(-STEP);
        if steps == 0 {
            if let Some(last) = &self.last {
                return last.clone();
            }
            steps = 1;
        }
        let dt = STEP * f64::from(steps);
        self.t += dt;

        // The minute turns: the time is laid down. The first frame lays it
        // too, so a patch just picked says what time it is.
        let minute = (ctx.now / 60.0).floor() as i64;
        if ctx.get("time") > 0.5 && self.minute != Some(minute) {
            self.time_at = self.t;
            self.time_text = time_text(ctx.now, ctx.get("24h") > 0.5);
            let mut dice = Rng::new(self.seed ^ (minute as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
            self.morsels = morsels(&self.time_text, ctx.get("spacing"), &mut dice, self.t);
        }
        self.minute = Some(minute);

        let scale = ctx.get("scale");
        let leds = self.last_leds.take();
        self.forage(ctx, leds.as_deref(), dt as f32, scale);
        self.lay_food(ctx, scale);

        let habit = self.habit(ctx);
        let Some(Some(sim)) = &mut self.sim else { return Frame::black() };
        let substeps = (ctx.get("steps") as u32).max(1) * steps;
        let density = sim.agents() as f32 / (WORLD_W * WORLD_H) as f32;
        // `tempo` slows the colony's time, not its shape: a shorter step and
        // a smaller turn per substep, and the trail fading and spreading
        // per substep as it would over that share of one.
        let tempo = ctx.get("tempo");
        let (decay, diffuse) = (1.0 - (1.0 - habit.decay).powf(tempo), 1.0 - (1.0 - habit.diffuse).powf(tempo));
        let mut laws = [0.0; 12];
        for (k, l) in [habit.sd, habit.sa, habit.ra, habit.md].iter().enumerate() {
            // Sensor distance and angle are shape; the turn and the step
            // are per substep, so they scale.
            let per = if k >= 2 { tempo } else { 1.0 };
            laws[3 * k..3 * k + 3].copy_from_slice(&[l.0 * per, l.1 * per, l.2]);
        }
        let (ox, oy) = window(scale);
        let params: Vec<Params> = (0..substeps)
            .map(|_| {
                self.substep = self.substep.wrapping_add(1);
                Params {
                    w: WORLD_W,
                    h: WORLD_H,
                    n: sim.agents(),
                    seed: self.substep.wrapping_mul(0x9e37_79b9),
                    laws,
                    deposit: tempo,
                    decay,
                    diffuse,
                    inv_ref: decay / (density * tempo),
                    ox,
                    oy,
                    scale,
                    first: 0,
                    wobble: habit.wobble * tempo.sqrt(),
                    respawn: ctx.get("renewal") * 1e-3 * tempo,
                    cap: ctx.get("cap"),
                    emit: ctx.get("scent") * 0.5,
                    food_k: FOOD_K,
                    pad: [0; 3],
                }
            })
            .collect();
        let Some(mut leds) = sim.frame(&params, &self.food) else { return Frame::black() };
        let inv = 1.0 / substeps as f32;
        leds.iter_mut().for_each(|v| v.iter_mut().for_each(|c| *c *= inv));
        // Afterglow: each LED eases toward its new light rather than taking
        // it at once. Display only - the colony does not see it.
        let glow = ctx.get("glow");
        if let Some(prev) = &self.glowing {
            let keep = if glow > 0.0 { (-(dt as f32) / glow).exp() } else { 0.0 };
            for (v, p) in leds.iter_mut().zip(prev) {
                for c in 0..4 {
                    v[c] = p[c] + (v[c] - p[c]) * (1.0 - keep);
                }
            }
        }
        self.glowing = Some(leds.clone());
        let frame = self.paint(ctx, &leds);
        self.last_leds = Some(leds);
        self.last = Some(frame.clone());
        frame
    }

    fn playing(&self) -> Option<Playing> {
        let title = format!("{} food source{}", self.nodes.len(), if self.nodes.len() == 1 { "" } else { "s" });
        let detail = if self.t - self.time_at < 40.0 {
            format!("drawing {}", self.time_text.iter().collect::<String>())
        } else {
            "foraging".to_string()
        };
        Some(Playing { title, detail, actions: Vec::new(), notes: Vec::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette's sRGB bytes, pinned as a 64-bit FNV-1a per `mute`: what
    /// physarum looks like must not move when the search behind it does. On
    /// a mismatch the dump is printed, so the diff is readable.
    #[test]
    fn the_palette_is_pinned() {
        let fnv = |palette: &[Rgb]| {
            palette
                .iter()
                .flat_map(|c| c.to_srgb8())
                .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
        };
        for (mute, want) in
            [(0.0, 0x8b26_b305_3308_4b88u64), (1.0, 0x98e0_adb5_2e82_f4ee), (2.0, 0x454d_e693_0c5f_d1a1)]
        {
            let palette = palette_for(mute);
            assert_eq!(palette.len(), 1 + (HUES + 1) * LIGHTS);
            let got = fnv(&palette);
            assert_eq!(got, want, "mute {mute}: palette hash {got:#018x}\n{}", palette_dump(&palette));
        }
    }
}
