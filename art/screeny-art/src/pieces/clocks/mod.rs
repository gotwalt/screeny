//! Twenty-four clocks: kinetic choreography, after Humans since 1982's
//! ClockClock 24. A 3 x 8 grid of two-handed analog clocks whose hands line up
//! to draw the digits of the time, and dance their way to the next minute.
//!
//! It fits this panel exactly: 8 clocks x 8 LEDs wide, 3 x 8 tall, with black
//! margins above and below. Hands reach their cell's edge, so neighbouring
//! clocks' strokes join into continuous digit lines, which the physical clock
//! cannot do and which is what makes a 16 x 24 LED digit legible.
//!
//! There is no physics and no tweening. The real hands are on stepper motors,
//! and what makes them look mechanical is a motor's constraint: every hand has
//! the same top speed and the same acceleration, so a hand with further to go
//! simply takes longer. Each choreography is a plan of such moves, timed to land
//! on the new time exactly as the minute turns.

pub(crate) mod ambient;
pub(crate) mod dance;
pub(crate) mod draw;

use crate::frame::Frame;
use crate::piece::{param, Action, Ctx, ParamSpec, Piece, PieceDef, Playing};
use crate::rng::Rng;
use dance::Motor;

pub const DEF: PieceDef = PieceDef {
    id: "clocks",
    name: "Twenty-four clocks",
    blurb: "Kinetic choreography after ClockClock 24: clock hands draw the time, moved like stepper motors. 16 colours, exact.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[
    param("pace", "Seconds per minute (60 = real clock)", 5.0, 60.0, 1.0, 60.0),
    param("still", "Seconds the time is held", 3.0, 60.0, 1.0, 20.0),
    param("dance", "Choreography (0 = vary, 13 = always composed)", 0.0, 13.0, 1.0, 0.0),
    param("speed", "Hand speed (deg/s)", 30.0, 360.0, 1.0, 100.0),
    param("hours24", "24-hour", 0.0, 1.0, 1.0, 1.0),
    param("offset", "Time offset (minutes)", 0.0, 1439.0, 1.0, 0.0),
    param("weight", "Hand weight (LEDs)", 1.0, 2.4, 0.05, 2.0),
    param("dials", "Dial rings", 0.0, 1.0, 0.01, 0.0),
    param("hue", "Hour hand hue", 0.0, 360.0, 1.0, 80.0),
    param("chroma", "Hour hand colour", 0.0, 0.2, 0.005, 0.05),
    param("hue2", "Minute hand hue", 0.0, 360.0, 1.0, 80.0),
    param("chroma2", "Minute hand colour", 0.0, 0.2, 0.005, 0.05),
    param("light", "Hand lightness (lower = more saturated)", 0.6, 0.95, 0.01, 0.93),
];

pub(crate) const COLS: usize = 8;
pub(crate) const ROWS: usize = 3;
pub(crate) const CLOCKS: usize = COLS * ROWS;
/// LEDs per clock; the grid is 64 x 24, centred in 64 x 32.
const CELL: f32 = 8.0;

/// Both hands at 7:30: how a clock that is not part of a digit rests.
const REST: f32 = 225.0;
/// Degrees clockwise from 12 o'clock.
pub(crate) type Hands = [f32; 2];

/// Each digit is 2 clocks wide and 3 tall, read in rows. U R D L are hand
/// directions and N is the rest pose. Where a junction would need three hands,
/// something has to give. 0, 2, 5, 6 and 9 were checked against footage of
/// the original (its 9 keeps the vertical and lets the bar fall short); the
/// rest follow manu.ninja's table, drawn from the studio's promotional films,
/// including the "cyclops" 8: a closed box over a cup.
const DIGITS: [[&str; 6]; 10] = [
    ["RD", "LD", "UD", "UD", "UR", "UL"],
    ["NN", "DD", "NN", "UD", "NN", "UU"],
    ["RR", "LD", "RD", "LU", "UR", "LL"],
    ["RR", "LD", "RR", "LU", "RR", "LU"],
    ["DD", "DD", "UR", "UD", "NN", "UU"],
    ["RD", "LL", "UR", "LD", "RR", "LU"],
    ["RD", "LL", "UD", "LD", "UR", "UL"],
    ["RR", "LD", "NN", "UD", "NN", "UU"],
    ["RD", "LD", "UR", "UL", "UR", "UL"],
    ["RD", "LD", "UR", "UD", "RR", "LU"],
];

fn direction(c: u8) -> f32 {
    match c {
        b'U' => 0.0,
        b'R' => 90.0,
        b'D' => 180.0,
        b'L' => 270.0,
        _ => REST,
    }
}

/// Hand angles for all 24 clocks showing `hh:mm`.
pub(crate) fn pose(hh: u32, mm: u32) -> [Hands; CLOCKS] {
    let mut out = [[REST; 2]; CLOCKS];
    for (d, digit) in [hh / 10, hh % 10, mm / 10, mm % 10].into_iter().enumerate() {
        for (k, cell) in DIGITS[digit as usize % 10].iter().enumerate() {
            let (row, col) = (k / 2, d * 2 + k % 2);
            let b = cell.as_bytes();
            out[row * COLS + col] = [direction(b[0]), direction(b[1])];
        }
    }
    out
}

/// Every hand's moves for one transition.
struct Plan {
    began: f64,
    from: [Hands; CLOCKS],
    to: [Hands; CLOCKS],
    moves: dance::Moves,
    total: f32,
    minute: i64,
}

/// Something the person at the studio asked for, to do as soon as possible.
#[derive(Clone, Copy, PartialEq)]
enum Request {
    /// Perform the last dance again.
    Again,
    /// Compose a new dance and perform it now.
    Another,
}

struct Clocks {
    seed: u64,
    /// The time of day on the first frame; a sped-up clock (pace < 60) runs on
    /// from here.
    born: Option<f64>,
    angles: [Hands; CLOCKS],
    /// The minute the hands currently show, or are moving towards.
    minute: Option<i64>,
    plan: Option<Plan>,
    /// Engine time at which the hands last landed on a time.
    landed: f64,
    /// Ambient motion between holding the time and the next dance.
    ambient: Option<ambient::Ambient>,
    /// The dance chosen for a minute, kept so it is composed only once.
    chosen: Option<(i64, usize, dance::Composition)>,
    /// The dance being performed, or the last one that was: what a rating
    /// applies to.
    performed: Option<dance::Composition>,
    /// Tags of the last few dances, so the composer can avoid repeating itself.
    recent: std::collections::VecDeque<Vec<String>>,
    taste: crate::taste::Taste,
    request: Option<Request>,
    /// How many dances have been asked for by hand, to vary them.
    asked: u64,
    /// What the hands are doing, for the studio.
    doing: String,
}

fn make(seed: u64) -> Box<dyn Piece> {
    Box::new(Clocks {
        seed,
        born: None,
        angles: [[REST; 2]; CLOCKS],
        minute: None,
        plan: None,
        landed: 0.0,
        ambient: None,
        chosen: None,
        performed: None,
        recent: Default::default(),
        taste: crate::taste::Taste::load("clocks"),
        request: None,
        asked: 0,
        doing: String::new(),
    })
}

impl Clocks {
    fn target(minute: i64, hours24: bool) -> [Hands; CLOCKS] {
        let (h, m) = ((minute.div_euclid(60).rem_euclid(24)) as u32, minute.rem_euclid(60) as u32);
        let h = if hours24 { h } else { (h + 11) % 12 + 1 };
        pose(h, m)
    }

    /// The dance for the move to `minute`. Asked for by number it comes from
    /// the repertoire (or, at 13, is always composed). Left to vary, most are
    /// composed afresh and the rest drawn from the repertoire.
    fn dance_for(&mut self, minute: i64, choice: usize, to: &[Hands; CLOCKS], motor: Motor) -> dance::Composition {
        if let Some((m, c, composition)) = &self.chosen {
            if (*m, *c) == (minute, choice) {
                return composition.clone();
            }
        }
        let mut rng = Rng::new(self.seed ^ (minute as u64).wrapping_mul(0x9e37_79b9) ^ self.asked.wrapping_mul(0x85eb_ca6b));
        let composed = match choice {
            0 => rng.u64() % 10 < 6,
            n => n > dance::DANCES,
        };
        let composition = if composed {
            let recent: Vec<Vec<String>> = self.recent.iter().cloned().collect();
            dance::compose(&mut rng, &self.angles, to, motor, &self.taste, &recent)
        } else {
            dance::named(if choice == 0 { (rng.u64() % dance::DANCES as u64) as usize } else { choice - 1 }, &mut rng)
        };
        self.chosen = Some((minute, choice, composition.clone()));
        composition
    }

    fn perform(&mut self, ctx: &Ctx, composition: dance::Composition, to: [Hands; CLOCKS], minute: i64, motor: Motor) {
        let (moves, total) = dance::plan(&composition.phases, &self.angles, &to, motor);
        eprintln!(
            "clocks: t={:.1} {:02}:{:02} by {}, {total:.1} s",
            ctx.t,
            minute.div_euclid(60).rem_euclid(24),
            minute.rem_euclid(60),
            composition.name
        );
        for hands in &mut self.angles {
            *hands = hands.map(|a| a.rem_euclid(360.0));
        }
        self.ambient = None;
        self.recent.push_back(composition.tags.clone());
        while self.recent.len() > 3 {
            self.recent.pop_front();
        }
        self.performed = Some(composition);
        self.chosen = None;
        self.plan = Some(Plan { began: ctx.t, from: self.angles, to, moves, total, minute });
    }

    fn step(&mut self, ctx: &Ctx) {
        let pace = ctx.get("pace");
        // Display seconds per engine second. At pace 60 the wall clock is used
        // directly, so the time is right however long the piece has run.
        let rate = 60.0 / pace as f64;
        let born = *self.born.get_or_insert(ctx.now - ctx.t);
        let clock = if pace >= 59.5 { ctx.now } else { born + ctx.t * rate } + ctx.get("offset") as f64 * 60.0;
        let motor = Motor { speed: ctx.get("speed"), acc: ctx.get("speed") * 1.6 };
        let hours24 = ctx.get("hours24") >= 0.5;

        if let Some(p) = self.plan.take() {
            let tau = (ctx.t - p.began) as f32;
            for i in 0..CLOCKS {
                for h in 0..2 {
                    self.angles[i][h] = dance::angle_at(p.from[i][h], &p.moves[i][h], motor, tau);
                }
            }
            self.doing = format!("dancing, {:.0} s to go", (p.total - tau).max(0.0));
            if tau >= p.total {
                self.angles = p.to;
                self.minute = Some(p.minute);
                self.landed = ctx.t;
            } else {
                self.plan = Some(p);
            }
            return;
        }

        // A dance asked for from the studio is performed at once, to the time
        // already showing, and the minute's own dance follows when it is due.
        if let (Some(request), Some(shown)) = (self.request.take(), self.minute) {
            let to = Self::target(shown, hours24);
            let composition = match (request, &self.performed) {
                (Request::Again, Some(last)) => last.clone(),
                _ => {
                    self.asked += 1;
                    self.chosen = None;
                    self.dance_for(shown, dance::DANCES + 1, &to, motor)
                }
            };
            self.perform(ctx, composition, to, shown, motor);
            return;
        }

        // Not dancing. On the first frame go straight to the current time;
        // after that, set off early enough to land on the next minute as it turns.
        let now_minute = (clock / 60.0).floor() as i64;
        let next = match self.minute {
            None => now_minute,
            Some(shown) if shown < now_minute => now_minute,
            Some(shown) => shown + 1,
        };
        let to = Self::target(next, hours24);
        let composition = self.dance_for(next, ctx.get("dance") as usize, &to, motor);
        let (_, total) = dance::plan(&composition.phases, &self.angles, &to, motor);
        // Engine seconds until the dance has to begin.
        let slack = (next as f64 * 60.0 - clock) / rate - total as f64;
        let ready = self.ambient.as_ref().map_or(true, |a| a.at_rest());
        if self.minute.is_none() || (slack <= 0.0 && ready) {
            self.perform(ctx, composition, to, next, motor);
            return;
        }
        self.doing = match &self.ambient {
            Some(a) => format!("drifting ({}), next dance in {:.0} s", a.name(), slack.max(0.0)),
            None => format!("holding the time, next dance in {:.0} s", slack.max(0.0)),
        };

        // Between the time and the next dance: hold still for a while, then
        // drift. The ambient field is asked to settle with room to spare, so
        // the hands are at rest when the dance has to leave.
        const SETTLE: f64 = 4.0;
        let held = ctx.get("still") as f64 * pace as f64 / 60.0;
        match &mut self.ambient {
            Some(ambient) => ambient.step(&mut self.angles, motor, ctx.dt as f32, slack < SETTLE),
            None if ctx.t - self.landed >= held && slack > SETTLE + 6.0 => {
                let mut rng = Rng::new(self.seed ^ (next as u64).wrapping_mul(0x51ed_270b));
                let mood = (rng.u64() % ambient::MOODS as u64) as usize;
                let ambient = ambient::Ambient::new(&mut rng, mood, COLS, ROWS, CELL);
                eprintln!("clocks: t={:.1} ambient {}", ctx.t, ambient.name());
                self.ambient = Some(ambient);
            }
            None => {}
        }
    }
}

/// Tags as a person would say them: "op:weave" -> "weave".
fn plain(tag: &str) -> String {
    match tag.split_once(':') {
        Some(("theme" | "theme2", t)) => format!("from a {t}").replace("from a diagonal", "on the diagonal").replace("from a sweep", "in a sweep").replace("from a cascade", "in a cascade"),
        Some(("mask", t)) => format!("split by {t}"),
        Some((_, t)) => t.to_string(),
        None => tag.to_string(),
    }
}

impl Piece for Clocks {
    fn playing(&self) -> Option<Playing> {
        let performed = self.performed.as_ref()?;
        let opinions = self.taste.opinions();
        let said = |liked: bool| {
            let tags: Vec<String> = opinions.iter().filter(|(_, w)| (*w > 0.0) == liked).take(5).map(|(t, _)| plain(t)).collect();
            (!tags.is_empty()).then(|| format!("{}: {}", if liked { "You like" } else { "You like less" }, tags.join(", ")))
        };
        let mut notes: Vec<String> = [said(true), said(false)].into_iter().flatten().collect();
        if notes.is_empty() {
            notes.push("Rate a few dances and the composer will lean towards what you like.".into());
        }
        let mut actions = vec![
            Action { id: "like", label: "More like this" },
            Action { id: "dislike", label: "Less like this" },
            Action { id: "again", label: "Play it again" },
            Action { id: "another", label: "Compose another" },
        ];
        if !opinions.is_empty() {
            actions.push(Action { id: "forget", label: "Forget what I like" });
        }
        Some(Playing { title: performed.name.clone(), detail: self.doing.clone(), actions, notes })
    }

    fn act(&mut self, action: &str) {
        let tags = self.performed.as_ref().map(|p| p.tags.clone()).unwrap_or_default();
        match action {
            "like" => self.taste.rate(&tags, 1.0),
            "dislike" => self.taste.rate(&tags, -1.0),
            "forget" => self.taste.forget(),
            "again" => self.request = Some(Request::Again),
            "another" => self.request = Some(Request::Another),
            _ => {}
        }
        // What was just learned should show in the very next composition.
        if matches!(action, "like" | "dislike" | "forget") {
            self.chosen = None;
        }
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.step(ctx);
        let half = ctx.get("weight") * 0.5;
        // The rounded tip ends exactly on the cell edge, meeting its neighbour's.
        let len = CELL * 0.5 - half;
        let tint = |hue: &str, chroma: &str| draw::Tint { hue: ctx.get(hue), chroma: ctx.get(chroma), light: ctx.get("light") };
        draw::Dials {
            angles: &self.angles,
            cols: COLS,
            rows: ROWS,
            cell: CELL,
            lens: [len; 2],
            half,
            tints: [tint("hue", "chroma"), tint("hue2", "chroma2")],
            ring: ctx.get("dials"),
        }
        .draw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_distinct_and_well_formed() {
        for (a, da) in DIGITS.iter().enumerate() {
            assert!(da.iter().all(|c| c.len() == 2 && c.bytes().all(|b| b"URDLN".contains(&b))));
            for db in &DIGITS[a + 1..] {
                assert_ne!(da, db);
            }
        }
    }
}
