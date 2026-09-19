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

mod ambient;
mod dance;

use crate::color::Rgb;
use crate::dither::Dither;
use crate::frame::Frame;
use crate::palette::Palette;
use crate::piece::{param, Ctx, ParamSpec, Piece, PieceDef};
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
    param("dance", "Choreography (0 = vary)", 0.0, 12.0, 1.0, 0.0),
    param("speed", "Hand speed (deg/s)", 30.0, 360.0, 1.0, 100.0),
    param("hours24", "24-hour", 0.0, 1.0, 1.0, 1.0),
    param("offset", "Time offset (minutes)", 0.0, 1439.0, 1.0, 0.0),
    param("weight", "Hand weight (LEDs)", 1.0, 2.4, 0.05, 2.0),
    param("dials", "Dial rings", 0.0, 1.0, 0.01, 0.0),
    param("hue", "Hue", 0.0, 360.0, 1.0, 80.0),
    param("chroma", "Colour", 0.0, 0.2, 0.005, 0.05),
];

pub(crate) const COLS: usize = 8;
pub(crate) const ROWS: usize = 3;
pub(crate) const CLOCKS: usize = COLS * ROWS;
/// LEDs per clock; the grid is 64 x 24, centred in 64 x 32.
const CELL: f32 = 8.0;
const TOP: f32 = 4.0;

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

struct Clocks {
    seed: u64,
    /// Local wall-clock seconds when the piece was made; the simulated clock
    /// (pace < 60) runs on from here.
    born: f64,
    angles: [Hands; CLOCKS],
    /// The minute the hands currently show, or are moving towards.
    minute: Option<i64>,
    plan: Option<Plan>,
    /// Engine time at which the hands last landed on a time.
    landed: f64,
    /// Ambient motion between holding the time and the next dance.
    ambient: Option<ambient::Ambient>,
}

fn make(seed: u64) -> Box<dyn Piece> {
    Box::new(Clocks { seed, born: local_seconds(), angles: [[REST; 2]; CLOCKS], minute: None, plan: None, landed: 0.0, ambient: None })
}

/// Seconds since the epoch, shifted into the local time zone.
fn local_seconds() -> f64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    #[cfg(unix)]
    {
        let secs = now as libc::time_t;
        // SAFETY: `tm` is plain data and localtime_r writes all of it.
        let offset = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&secs, &mut tm);
            tm.tm_gmtoff
        };
        now + offset as f64
    }
    #[cfg(not(unix))]
    now
}

impl Clocks {
    fn target(minute: i64, hours24: bool) -> [Hands; CLOCKS] {
        let (h, m) = ((minute.div_euclid(60).rem_euclid(24)) as u32, minute.rem_euclid(60) as u32);
        let h = if hours24 { h } else { (h + 11) % 12 + 1 };
        pose(h, m)
    }

    /// The dance for the move to `minute`: a fixed one if asked for, otherwise
    /// drawn from the repertoire, never the same as the minute before.
    fn dance_for(&self, minute: i64, choice: f32) -> (&'static str, Vec<dance::Phase>) {
        let mut rng = Rng::new(self.seed ^ (minute as u64).wrapping_mul(0x9e37_79b9));
        let pick = |m: i64| (Rng::new(self.seed ^ (m as u64).wrapping_mul(0x2545_f491)).u64() % dance::DANCES as u64) as usize;
        let which = match choice as usize {
            0 if pick(minute) == pick(minute - 1) => pick(minute) + 1,
            0 => pick(minute),
            n => n - 1,
        };
        dance::dance(which, &mut rng)
    }

    fn step(&mut self, ctx: &Ctx) {
        let pace = ctx.get("pace");
        // Display seconds per engine second. At pace 60 the wall clock is used
        // directly, so the time is right however long the piece has run.
        let rate = 60.0 / pace as f64;
        let clock = if pace >= 59.5 { local_seconds() } else { self.born + ctx.t * rate } + ctx.get("offset") as f64 * 60.0;
        let motor = Motor { speed: ctx.get("speed"), acc: ctx.get("speed") * 1.6 };
        let hours24 = ctx.get("hours24") >= 0.5;

        if let Some(p) = self.plan.take() {
            let tau = (ctx.t - p.began) as f32;
            for i in 0..CLOCKS {
                for h in 0..2 {
                    self.angles[i][h] = dance::angle_at(p.from[i][h], &p.moves[i][h], motor, tau);
                }
            }
            if tau >= p.total {
                self.angles = p.to;
                self.minute = Some(p.minute);
                self.landed = ctx.t;
            } else {
                self.plan = Some(p);
            }
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
        let (name, phases) = self.dance_for(next, ctx.get("dance"));
        let (moves, total) = dance::plan(&phases, &self.angles, &to, motor);
        // Engine seconds until the dance has to begin.
        let slack = (next as f64 * 60.0 - clock) / rate - total as f64;
        let ready = self.ambient.as_ref().map_or(true, |a| a.at_rest());
        if self.minute.is_none() || (slack <= 0.0 && ready) {
            eprintln!("clocks: t={:.1} {:02}:{:02} by {name}, {total:.1} s", ctx.t, next.div_euclid(60).rem_euclid(24), next.rem_euclid(60));
            for hands in &mut self.angles {
                *hands = hands.map(|a| a.rem_euclid(360.0));
            }
            self.ambient = None;
            self.plan = Some(Plan { began: ctx.t, from: self.angles, to, moves, total, minute: next });
            return;
        }

        // Between the time and the next dance: hold still for a while, then
        // drift. The ambient field is asked to settle with room to spare, so
        // the hands are at rest when the dance has to leave.
        const SETTLE: f64 = 4.0;
        let held = ctx.get("still") as f64 * pace as f64 / 60.0;
        match &mut self.ambient {
            Some(ambient) => ambient.step(&mut self.angles, motor, ctx.dt as f32, slack < SETTLE),
            None if ctx.t - self.landed >= held && slack > SETTLE + 6.0 => {
                let ambient = ambient::Ambient::new(&mut Rng::new(self.seed ^ (next as u64).wrapping_mul(0x51ed_270b)));
                eprintln!("clocks: t={:.1} ambient {}", ctx.t, ambient.name());
                self.ambient = Some(ambient);
            }
            None => {}
        }
    }
}

/// Distance from `p` to the segment from the origin along `angle` for `len`.
fn hand_distance(px: f32, py: f32, angle: f32, len: f32) -> f32 {
    let (s, c) = angle.to_radians().sin_cos();
    // 0 degrees is up; y grows downwards on the panel.
    let (dx, dy) = (s, -c);
    let along = (px * dx + py * dy).clamp(0.0, len);
    ((px - dx * along).powi(2) + (py - dy * along).powi(2)).sqrt()
}

impl Piece for Clocks {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.step(ctx);

        let half = ctx.get("weight") * 0.5;
        // The rounded tip ends exactly on the cell edge, meeting its neighbour's.
        let len = CELL * 0.5 - half;
        let dials = ctx.get("dials");
        let angles = self.angles;

        let frame = Frame::supersample(6, |x, y| {
            let (cx, cy) = (x / CELL, (y - TOP) / CELL);
            if !(0.0..ROWS as f32).contains(&cy) {
                return Rgb::BLACK;
            }
            let i = cy as usize * COLS + cx as usize;
            let (px, py) = ((cx.fract() - 0.5) * CELL, (cy.fract() - 0.5) * CELL);
            let d = hand_distance(px, py, angles[i][0], len).min(hand_distance(px, py, angles[i][1], len));
            if d <= half {
                return Rgb::splat(1.0);
            }
            let ring = ((px * px + py * py).sqrt() - (CELL * 0.5 - 0.45)).abs();
            if ring < 0.3 { Rgb::splat(0.1 * dials) } else { Rgb::BLACK }
        });

        // One hue, fifteen lightnesses: enough steps that the anti-aliased edge
        // of a slowly turning hand is smooth without any dither on it.
        let palette = Palette::ramps(&[ctx.get("hue")], 15, (0.32, 0.93), ctx.get("chroma"));
        let tinted = match frame {
            Frame::Linear(px) => {
                let ink = *palette.colours().last().expect("ramp is not empty");
                Frame::Linear(px.into_iter().map(|c| ink.scale(c.r)).collect())
            }
            other => other,
        };
        palette.map(&tinted, Dither::None, 0.0)
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
