//! Clocks: dials. The same instrument as the numerals piece (`super`), with
//! the other face: larger dials in continuous motion, which tell the time as
//! analog clocks rather than by drawing digits.
//!
//! The spirit of ClockClock rather than its letter: a grid of two-handed dials
//! in continuous, unhurried motion, order gathering and dissolving. Built for
//! this panel instead of copied onto it:
//!
//! - Fewer, larger dials, filling the panel edge to edge, so a hand has the
//!   length to be graceful.
//! - Hands reach the edge of their cell, so when the field is gentle the lines
//!   of neighbouring dials join into long curves across the whole panel.
//! - The hour and minute hands each have their own colour, fifteen steps
//!   each: 31 colours, an exact frame. They drift slowly, by palette alone.
//! - It never stops and never switches: moods glide into one another.
//! - It is still a clock, in the way these dials can be. Drawn digits need two
//!   dials side by side per digit, so eight columns, so 8-LED dials: numerals
//!   and large dials cannot both fit in 64 LEDs. But the dials *are* clocks. As
//!   each minute turns, the flow gathers until every dial reads the time, hour
//!   hand drawn in short, holds, and lets go. Order out of disarray, and the
//!   order is the time. So that the moment is not missed, the hour hand takes a
//!   highlight complementary to the hands' own colour, the minute hand goes
//!   white, and a mark appears at 12 on every dial.
//!
//! Motion is the clocks' ambient engine (`clocks/ambient.rs`): every hand a
//! servo under one motor's speed and acceleration.

use crate::frame::{Frame, W};
use crate::piece::{choice, param, Action, Ctx, ParamSpec, Piece, PieceDef, Playing};
use super::ambient::{Ambient, Mood, MOODS};
use super::dance::Motor;
use super::draw::{Dials, Tint};
use super::Hands as Pair;
use crate::rng::Rng;

pub const DEF: PieceDef = PieceDef {
    id: "clocks-dials",
    name: "Clocks: dials",
    blurb: "Larger dials in continuous, flowing motion; each minute they gather to read the time as analog clocks, then let go. For a moving field that knows the time.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[
    choice("grid", "Dials", GRID_CHOICES, 1.0),
    choice("mood", "Mood", MOOD_CHOICES, 0.0),
    param("dwell", "Seconds in a mood", 8.0, 120.0, 1.0, 32.0),
    param("tell", "Tell the time every (s, 0 = never)", 0.0, 600.0, 5.0, 60.0),
    param("hold", "Seconds the time is held", 2.0, 30.0, 1.0, 8.0),
    param("offset", "Time offset (minutes)", 0.0, 1439.0, 1.0, 0.0),
    param("speed", "Hand speed (deg/s)", 20.0, 240.0, 1.0, 90.0),
    param("weight", "Hand weight (0 = auto)", 0.0, 4.0, 0.05, 0.0),
    param("tip", "Hand tip (1 = blunt)", 0.2, 1.0, 0.05, 0.45),
    param("hue", "Hour hand hue", 0.0, 360.0, 1.0, 75.0),
    param("chroma", "Hour hand colour", 0.0, 0.2, 0.005, 0.09),
    param("hue2", "Minute hand hue", 0.0, 360.0, 1.0, 35.0),
    param("chroma2", "Minute hand colour", 0.0, 0.2, 0.005, 0.09),
    param("light", "Hand lightness (lower = more saturated)", 0.6, 0.95, 0.01, 0.92),
    param("wheel", "Hue drift (deg/min)", 0.0, 120.0, 1.0, 25.0),
    param("highlight", "Highlight while telling the time", 0.0, 1.0, 0.01, 1.0),
    param("contrast", "Highlight hue, relative to the hands", -180.0, 180.0, 1.0, 180.0),
    param("mark", "12 o'clock mark while telling", 0.0, 1.0, 0.01, 1.0),
];

/// All three fill the panel edge to edge: 16, 10.7 and 8 LEDs per dial. 6x3 is
/// the default: hands long enough to have a gesture, and enough of them for a
/// wave to travel through.
const GRIDS: [(usize, usize); 3] = [(4, 2), (6, 3), (8, 4)];

/// Card 163: `grid` and `mood` are lists of named things, so they say so.
/// `GRID_CHOICES` is `GRIDS` written out, small and checked; `MOOD_CHOICES` is
/// "wander" and then `ambient::MOOD_NAMES` itself (card 182), so a new mood is
/// one name in one place. `piece::tests::the_named_stops_are_the_pieces_own_names`
/// checks the grids against `GRIDS` and guards the moods' offset.
pub(crate) const GRID_CHOICES: &[&str] = &["4 x 2", "6 x 3", "8 x 4"];
pub(crate) const MOOD_CHOICES: &[&str] = &mood_choices();

const fn mood_choices() -> [&'static str; MOODS + 1] {
    let mut out = [""; MOODS + 1];
    out[0] = "wander";
    let mut i = 0;
    while i < MOODS {
        out[i + 1] = super::ambient::MOOD_NAMES[i];
        i += 1;
    }
    out
}

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

struct Flow {
    rng: Rng,
    grid: usize,
    angles: Vec<Pair>,
    field: Ambient,
    /// Which mood the field is in, or gliding towards.
    mood: usize,
    /// Engine time at which to glide on to another.
    change_at: f64,
    /// Which moods have been visited lately, so wandering keeps moving on.
    variety: crate::variety::Variety,
    /// The mood mixed into the current one, if enough to be worth naming.
    tinge: Option<&'static str>,
    /// What is happening, for the studio.
    doing: String,
    /// Asked from the studio: glide on to another mood now.
    move_on: bool,
}

fn make(seed: u64) -> Box<dyn Piece> {
    let mut rng = Rng::new(seed);
    let mood = REPERTOIRE[(rng.u64() % REPERTOIRE.len() as u64) as usize];
    let grid = 1;
    let field = Ambient::new(&mut rng, mood, GRIDS[grid].0, GRIDS[grid].1, W as f32 / GRIDS[grid].0 as f32);
    eprintln!("dials: t=0 {}", field.name());
    Box::new(Flow { rng, grid, angles: rest(GRIDS[grid]), field, mood, change_at: -1.0, variety: Default::default(), tinge: None, doing: String::new(), move_on: false })
}

/// Every dial starts with both hands at 7:30, as an idle ClockClock does.
fn rest((cols, rows): (usize, usize)) -> Vec<Pair> {
    vec![[225.0; 2]; cols * rows]
}

impl Flow {
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
        } else if ctx.t >= self.change_at || std::mem::take(&mut self.move_on) {
            // The mood least visited lately, allowing for how often each is
            // meant to come up, with a little chance so it is never a rota.
            (0..MOODS)
                .filter(|m| *m != self.mood)
                .map(|m| {
                    let share = REPERTOIRE.iter().filter(|r| **r == m).count().max(1) as f32;
                    (self.variety.staleness(&[format!("mood:{m}")]) / share + 0.03 * self.rng.f32(), m)
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, m)| m)
        } else {
            None
        };
        if let Some(next) = next {
            self.mood = next;
            self.variety.note("", &[format!("mood:{next}")]);
            self.tinge = None;
            if asked > 0 {
                self.field.drift_to(next, &mut self.rng);
            } else {
                // Wandering never lands on a pure mood twice: each stop is
                // mostly one, tinged with another.
                let other = (self.rng.u64() % MOODS as u64) as usize;
                let tinge = self.rng.range(0.0, 0.45);
                self.field.drift_to_blend(next, other, tinge, &mut self.rng);
                if tinge > 0.15 && other != next {
                    self.tinge = Some(Mood::new(other, &mut Rng::new(0)).name);
                }
            }
            self.change_at = ctx.t + ctx.get("dwell") as f64 * self.rng.range(0.7, 1.3) as f64;
            eprintln!("dials: t={:.0} {}", ctx.t, self.field.name());
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
        self.doing = if telling {
            "telling the time".to_string()
        } else if asked > 0 {
            "held in this mood".to_string()
        } else {
            format!("moving on in {:.0} s", (self.change_at - ctx.t).max(0.0))
        };

        let speed = ctx.get("speed");
        // Gentler than the clock's dances: this is never in a hurry.
        let motor = Motor { speed, acc: speed * 1.1 };
        self.field.step_holding(&mut self.angles, motor, ctx.dt as f32, false, pose.as_deref());
    }
}

impl Piece for Flow {
    fn playing(&self) -> Option<Playing> {
        let title = match self.tinge {
            Some(other) => format!("{}, tinged with {other}", self.field.name()),
            None => self.field.name().to_string(),
        };
        let actions = vec![Action { id: "move-on", label: "Move on" }];
        Some(Playing { title, detail: self.doing.clone(), actions, notes: Vec::new() })
    }

    fn act(&mut self, action: &str) {
        self.move_on |= action == "move-on";
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.step(ctx);

        let (cols, rows) = GRIDS[self.grid];
        let cell = self.cell();
        // About an eighth of the dial, and never thinner than the panel can
        // draw cleanly.
        let weight = match ctx.get("weight") {
            w if w > 0.0 => w,
            _ => (cell / 8.0).max(1.6),
        };
        let half = weight * 0.5;
        let len = cell * 0.5 - half;
        // While the time is held the hour hand draws in short.
        let grip = self.field.grip();
        let lens = [len * (1.0 - (1.0 - HOUR_HAND) * grip), len];

        // Each hand has its own colour, and both drift slowly round the wheel.
        // While the time is held the grid has to say so at a glance, so the
        // hands part company: the hour hand takes a saturated highlight (which
        // needs a lower lightness to have any chroma) and the minute hand goes
        // to clean white. The highlight's hue is set *relative* to the hands',
        // by default the complement, because the hands' own hue drifts: any
        // fixed colour would sooner or later be the one they already are.
        // All of it is palette animation.
        let wheel = ctx.get("wheel") * (ctx.t / 60.0) as f32;
        let lit = grip * ctx.get("highlight");
        let mix = |a: f32, b: f32| a + (b - a) * lit;
        let hue = ctx.get("hue") + wheel;
        let tints = [
            // Light enough to stay legible when the highlight falls on blue,
            // the panel's weakest primary.
            Tint { hue: hue + ctx.get("contrast") * lit, chroma: mix(ctx.get("chroma"), 0.15), light: mix(ctx.get("light"), 0.82) },
            Tint { hue: ctx.get("hue2") + wheel, chroma: mix(ctx.get("chroma2"), 0.01), light: mix(ctx.get("light"), 0.97) },
        ];
        let mark = grip * ctx.get("mark");
        Dials { angles: &self.angles, cols, rows, cell, lens, half, tip: ctx.get("tip"), tints, rest: &[], ring: 0.0, mark }.draw()
    }
}
