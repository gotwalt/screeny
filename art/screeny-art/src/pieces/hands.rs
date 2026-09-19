//! Hands: the clocks without the time.
//!
//! The spirit of ClockClock rather than its letter: a grid of two-handed dials
//! in continuous, unhurried motion, order gathering and dissolving. Built for
//! this panel instead of copied onto it:
//!
//! - Fewer, larger dials, filling the panel edge to edge, so a hand has the
//!   length to be graceful.
//! - Hands reach the edge of their cell, so when the field is gentle the lines
//!   of neighbouring dials join into long curves across the whole panel.
//! - The two hands of each dial take two related tints, fifteen steps each:
//!   31 colours, an exact frame. The tints drift slowly, by palette alone.
//! - It never stops and never switches: moods glide into one another.
//! - It is still a clock, in the way these dials can be. Drawn digits need two
//!   dials side by side per digit, so eight columns, so 8-LED dials: numerals
//!   and large dials cannot both fit in 64 LEDs. But the dials *are* clocks. As
//!   each minute turns, the flow gathers until every dial reads the time, hour
//!   hand drawn in short, holds, and lets go. Order out of disarray, and the
//!   order is the time.
//!
//! Motion is the clocks' ambient engine (`clocks/ambient.rs`): every hand a
//! servo under one motor's speed and acceleration.

use crate::color::{oklch, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, W};
use crate::palette::Palette;
use crate::piece::{param, Ctx, ParamSpec, Piece, PieceDef};
use crate::pieces::clocks::ambient::{Ambient, MOODS};
use crate::pieces::clocks::dance::Motor;
use crate::pieces::clocks::Hands as Pair;
use crate::rng::Rng;

pub const DEF: PieceDef = PieceDef {
    id: "hands",
    name: "Hands",
    blurb: "The clocks without the time: dials in continuous motion, order gathering and dissolving. 31 colours, exact.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[
    param("grid", "Dials (0: 4x2, 1: 6x3, 2: 8x4)", 0.0, 2.0, 1.0, 1.0),
    param("mood", "Mood (0 = wander)", 0.0, MOODS as f32, 1.0, 0.0),
    param("dwell", "Seconds in a mood", 8.0, 120.0, 1.0, 32.0),
    param("tell", "Tell the time every (s, 0 = never)", 0.0, 600.0, 5.0, 60.0),
    param("hold", "Seconds the time is held", 2.0, 30.0, 1.0, 8.0),
    param("offset", "Time offset (minutes)", 0.0, 1439.0, 1.0, 0.0),
    param("speed", "Hand speed (deg/s)", 20.0, 240.0, 1.0, 90.0),
    param("weight", "Hand weight (0 = auto)", 0.0, 4.0, 0.05, 0.0),
    param("hue", "Hue", 0.0, 360.0, 1.0, 75.0),
    param("second", "Second hand's hue shift", -180.0, 180.0, 1.0, -40.0),
    param("chroma", "Colour", 0.0, 0.2, 0.005, 0.09),
    param("wheel", "Hue drift (deg/min)", 0.0, 120.0, 1.0, 25.0),
];

/// All three fill the panel edge to edge: 16, 10.7 and 8 LEDs per dial. 6x3 is
/// the default: hands long enough to have a gesture, and enough of them for a
/// wave to travel through.
const GRIDS: [(usize, usize); 3] = [(4, 2), (6, 3), (8, 4)];

/// Seconds before the mark that the dials start gathering, so that they are
/// reading the time as it arrives.
const GATHER: f64 = 6.0;
/// How much shorter the hour hand is drawn while the time is held.
const HOUR_HAND: f32 = 0.6;

/// What wandering draws from. Open-handed moods, where neighbours link into
/// long curves, are what this panel shows best, so they come up most; the
/// folded-needle moods are short dashes at this size and come up least.
/// (Indices into `Mood::new`: 7 streamlines, 5 tide, 2 breathe, 6 rings,
/// 4 unison, 3 corners, 0 drift, 1 sway.)
const REPERTOIRE: [usize; 12] = [7, 5, 2, 6, 7, 5, 4, 3, 2, 6, 0, 1];

struct Hands {
    rng: Rng,
    grid: usize,
    angles: Vec<Pair>,
    field: Ambient,
    /// Which mood the field is in, or gliding towards.
    mood: usize,
    /// Engine time at which to glide on to another.
    change_at: f64,
}

fn make(seed: u64) -> Box<dyn Piece> {
    let mut rng = Rng::new(seed);
    let mood = REPERTOIRE[(rng.u64() % REPERTOIRE.len() as u64) as usize];
    let grid = 1;
    let field = Ambient::new(&mut rng, mood, GRIDS[grid].0, GRIDS[grid].1, W as f32 / GRIDS[grid].0 as f32);
    eprintln!("hands: t=0 {}", field.name());
    Box::new(Hands { rng, grid, angles: rest(GRIDS[grid]), field, mood, change_at: -1.0 })
}

/// Every dial starts with both hands at 7:30, as an idle ClockClock does.
fn rest((cols, rows): (usize, usize)) -> Vec<Pair> {
    vec![[225.0; 2]; cols * rows]
}

impl Hands {
    fn cell(&self) -> f32 {
        W as f32 / GRIDS[self.grid].0 as f32
    }

    fn step(&mut self, ctx: &Ctx) {
        let grid = (ctx.get("grid") as usize).min(GRIDS.len() - 1);
        if grid != self.grid {
            self.grid = grid;
            self.angles = rest(GRIDS[grid]);
            let cell = self.cell();
            self.field = Ambient::new(&mut self.rng, self.mood, GRIDS[grid].0, GRIDS[grid].1, cell);
        }
        if self.change_at < 0.0 {
            self.change_at = ctx.t + ctx.get("dwell") as f64;
        }

        // A mood asked for by name is taken up at once. Otherwise wander on
        // when the dwell is up, never to the mood we are already in.
        let asked = ctx.get("mood") as usize;
        let next = if asked > 0 {
            Some(asked - 1).filter(|m| *m != self.mood)
        } else if ctx.t >= self.change_at {
            let others: Vec<usize> = REPERTOIRE.iter().copied().filter(|m| *m != self.mood).collect();
            Some(others[(self.rng.u64() % others.len() as u64) as usize])
        } else {
            None
        };
        if let Some(next) = next {
            self.mood = next;
            self.field.drift_to(next, &mut self.rng);
            self.change_at = ctx.t + ctx.get("dwell") as f64 * self.rng.range(0.7, 1.3) as f64;
            eprintln!("hands: t={:.0} {}", ctx.t, self.field.name());
        }

        // Telling the time: for a few seconds around each mark, every dial is
        // drawn to the hour and minute, as an analog clock.
        let every = ctx.get("tell") as f64;
        let clock = ctx.now + ctx.get("offset") as f64 * 60.0;
        let since = clock.rem_euclid(every.max(1.0));
        let telling = every > 0.0 && (since < ctx.get("hold") as f64 || since > every - GATHER);
        let pose = telling.then(|| {
            let minutes = (clock / 60.0).rem_euclid(720.0) as f32;
            vec![[minutes * 0.5, minutes.rem_euclid(60.0) * 6.0]; self.angles.len()]
        });
        if telling {
            // The mood waits until the time has been let go.
            self.change_at = self.change_at.max(ctx.t + 4.0);
        }

        let speed = ctx.get("speed");
        // Gentler than the clock's dances: this is never in a hurry.
        let motor = Motor { speed, acc: speed * 1.1 };
        self.field.step_holding(&mut self.angles, motor, ctx.dt as f32, false, pose.as_deref());
    }
}

fn hand_distance(px: f32, py: f32, angle: f32, len: f32) -> f32 {
    let (s, c) = angle.to_radians().sin_cos();
    let (dx, dy) = (s, -c);
    let along = (px * dx + py * dy).clamp(0.0, len);
    ((px - dx * along).powi(2) + (py - dy * along).powi(2)).sqrt()
}

impl Piece for Hands {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.step(ctx);

        let (cols, rows) = GRIDS[self.grid];
        let cell = self.cell();
        let top = (H as f32 - rows as f32 * cell) * 0.5;
        // About an eighth of the dial, and never thinner than the panel can
        // draw cleanly.
        let weight = match ctx.get("weight") {
            w if w > 0.0 => w,
            _ => (cell / 8.0).max(1.6),
        };
        let half = weight * 0.5;
        let len = cell * 0.5 - half;
        // While the time is held the first hand is the hour hand, and shortens.
        let lens = [len * (1.0 - (1.0 - HOUR_HAND) * self.field.grip()), len];

        // Two ramps of fifteen. While the time is held they part company, so
        // the hands can be told apart at a glance: the hour hand deepens to a
        // saturated amber (which needs a lower lightness to have any chroma),
        // the minute hand pales towards white. Palette animation only.
        let grip = self.field.grip();
        let hue = ctx.get("hue") + ctx.get("wheel") * (ctx.t / 60.0) as f32;
        let chroma = ctx.get("chroma");
        let mix = |a: f32, b: f32| a + (b - a) * grip;
        let turn_to = |from: f32, to: f32| from + ((to - from + 540.0).rem_euclid(360.0) - 180.0) * grip;
        let (dark, light) = (0.32, 0.92);
        // (hue, chroma, lightest step)
        let tints = [
            (turn_to(hue, 58.0), mix(chroma, 0.18), mix(light, 0.74)),
            (hue + ctx.get("second"), mix(chroma, 0.02), mix(light, 0.95)),
        ];
        let mut colours = vec![Rgb::BLACK];
        for (h, c, top) in tints {
            colours.extend((0..15).map(|k| oklch(dark + (top - dark) * k as f32 / 14.0, c, h)));
        }
        let palette = Palette::new(colours, (light - dark) / 14.0);
        let inks = tints.map(|(h, c, top)| oklch(top, c, h));

        let angles = &self.angles;
        let frame = Frame::supersample(6, |x, y| {
            let (cx, cy) = (x / cell, (y - top) / cell);
            if cy < 0.0 || cy >= rows as f32 {
                return Rgb::BLACK;
            }
            let i = (cy as usize).min(rows - 1) * cols + (cx as usize).min(cols - 1);
            let (px, py) = ((cx.fract() - 0.5) * cell, (cy.fract() - 0.5) * cell);
            // The first hand lies over the second where they cross.
            if hand_distance(px, py, angles[i][0], lens[0]) <= half {
                inks[0]
            } else if hand_distance(px, py, angles[i][1], lens[1]) <= half {
                inks[1]
            } else {
                Rgb::BLACK
            }
        });
        palette.map(&frame, Dither::None, 0.0)
    }
}
