//! What the hands do from one frame to the next: dance to the coming minute,
//! land on it as it turns, hold the time, drift, and dance again.
//!
//! The caller owns the clock and hands in [`Tick`]; the settings are
//! [`Knobs`]. The Studio's patch and the firmware are both thin wrappers
//! round [`Engine::step`], so the panel and the page cannot disagree.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ambient;
use crate::dance::{self, Motor};
use crate::face::{pose, resting, Hands, Rest, CELL, CLOCKS, COLS, DEFAULT_REST, REST, RESTS, ROWS};
#[allow(unused_imports)]
use crate::math::{F32Ext, F64Ext};
use crate::rng::Rng;
use crate::variety::Variety;

/// One frame's worth of time.
#[derive(Clone, Copy, Debug)]
pub struct Tick {
    /// Seconds since the engine started.
    pub t: f64,
    /// Seconds since the last frame.
    pub dt: f64,
    /// Local time of day: seconds whose remainder by 86,400 is the time on the
    /// wall. The date is not used.
    pub now: f64,
}

/// Everything about the hands' behaviour that is a matter of taste.
#[derive(Clone, Copy, Debug)]
pub struct Knobs {
    /// Seconds per displayed minute; 60 is a real clock.
    pub pace: f32,
    /// Seconds the time is held before the hands may drift.
    pub still: f32,
    /// `0` varies, `1..=DANCES` names a dance, one past them always composes.
    pub dance: usize,
    /// Hand speed, degrees per second.
    pub speed: f32,
    pub hours24: bool,
    /// Minutes added to the time.
    pub offset: f32,
    pub rest: Rest,
}

impl Default for Knobs {
    /// The owner's "Gold and Blue" behaviour (2026-10-28).
    fn default() -> Self {
        Knobs {
            pace: 60.0,
            still: 5.0,
            dance: 0,
            speed: 100.0,
            hours24: true,
            offset: 0.0,
            rest: RESTS[DEFAULT_REST],
        }
    }
}

/// What the hands are up to, for whoever is showing that in words.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Doing {
    Starting,
    Dancing { to_go: f32 },
    Drifting { mood: &'static str, next: f32 },
    Holding { next: f32 },
}

/// Something asked for by hand, done as soon as possible.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Request {
    /// Perform the last dance again.
    Again,
    /// Compose a new dance and perform it now.
    Another,
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

/// A dance's length is asked for every frame while waiting to set off, and
/// working it out allocates. While the hands are still the answer cannot change.
struct Timed {
    minute: i64,
    choice: usize,
    from: [Hands; CLOCKS],
    motor: (f32, f32),
    total: f32,
}

pub struct Engine {
    seed: u64,
    /// The time of day on the first frame; a sped-up clock (pace < 60) runs on
    /// from here.
    born: Option<f64>,
    angles: [Hands; CLOCKS],
    /// The minute the hands currently show, or are moving towards.
    minute: Option<i64>,
    /// How far the picture has settled onto the time, 0 while the hands are
    /// dancing or drifting and 1 while it is being held. Resting dials recede
    /// by this much, so a dance is at full strength throughout and the digits
    /// are left alone on the panel once it lands.
    settled: f32,
    plan: Option<Plan>,
    /// Engine time at which the hands last landed on a time.
    landed: f64,
    /// Ambient motion between holding the time and the next dance.
    ambient: Option<ambient::Ambient>,
    /// The dance chosen for a minute, kept so it is composed only once.
    chosen: Option<(i64, usize, dance::Composition)>,
    timed: Option<Timed>,
    /// The dance being performed, or the last one that was.
    performed: Option<dance::Composition>,
    /// What has been performed, so the composer does not repeat itself.
    variety: Variety,
    request: Option<Request>,
    /// How many dances have been asked for by hand, to vary them.
    asked: u64,
    doing: Doing,
}

impl Engine {
    pub fn new(seed: u64) -> Self {
        Engine {
            seed,
            born: None,
            angles: [[REST; 2]; CLOCKS],
            minute: None,
            settled: 0.0,
            plan: None,
            landed: 0.0,
            ambient: None,
            chosen: None,
            timed: None,
            performed: None,
            variety: Variety::default(),
            request: None,
            asked: 0,
            doing: Doing::Starting,
        }
    }

    pub fn angles(&self) -> &[Hands; CLOCKS] {
        &self.angles
    }

    pub fn settled(&self) -> f32 {
        self.settled
    }

    pub fn doing(&self) -> Doing {
        self.doing
    }

    pub fn performed(&self) -> Option<&dance::Composition> {
        self.performed.as_ref()
    }

    pub fn ask(&mut self, request: Request) {
        self.request = Some(request);
    }

    /// The hours and minutes a minute-of-day shows, in the patch's format.
    pub fn digits(minute: i64, hours24: bool) -> (u32, u32) {
        let (h, m) = ((minute.div_euclid(60).rem_euclid(24)) as u32, minute.rem_euclid(60) as u32);
        (if hours24 { h } else { (h + 11) % 12 + 1 }, m)
    }

    pub fn target(minute: i64, hours24: bool, rest: Rest) -> [Hands; CLOCKS] {
        let (h, m) = Self::digits(minute, hours24);
        pose(h, m, rest)
    }

    /// Which dials are not part of the time, so are drawn as being at rest.
    /// Only while there is a time to read: during a dance every hand is a
    /// dancer and the grid is at full strength.
    pub fn idle(&self, hours24: bool) -> [bool; CLOCKS] {
        match self.minute {
            Some(m) if self.settled > 0.0 => {
                let (h, mm) = Self::digits(m, hours24);
                resting(h, mm)
            }
            _ => [false; CLOCKS],
        }
    }

    /// One frame: move the hands, then ease the picture towards "the time is
    /// being held" or back.
    pub fn step(&mut self, tick: Tick, knobs: &Knobs) {
        self.advance(tick, knobs);
        self.settle(tick.dt as f32);
    }

    /// The dance for the move to `minute`. Asked for by number it comes from
    /// the repertoire (or, at 13, is always composed). Left to vary, most are
    /// composed afresh and the rest drawn from the repertoire.
    fn dance_for(&mut self, minute: i64, choice: usize, to: &[Hands; CLOCKS], motor: Motor) {
        if let Some((m, c, _)) = &self.chosen {
            if (*m, *c) == (minute, choice) {
                return;
            }
        }
        let mut rng =
            Rng::new(self.seed ^ (minute as u64).wrapping_mul(0x9e37_79b9) ^ self.asked.wrapping_mul(0x85eb_ca6b));
        let composed = match choice {
            0 => rng.u64() % 10 < 6,
            n => n > dance::DANCES,
        };
        let composition = if composed {
            dance::compose(&mut rng, &self.angles, to, motor, &self.variety)
        } else if choice == 0 {
            // From the repertoire, whichever has gone longest unperformed.
            let names: Vec<String> =
                (0..dance::DANCES).map(|i| dance::named(i, &mut Rng::new(0)).tags[0].clone()).collect();
            let freshest = self.variety.freshest(&names, || rng.f32()).and_then(|n| names.iter().position(|m| m == n));
            dance::named(freshest.unwrap_or(0), &mut rng)
        } else {
            dance::named(choice - 1, &mut rng)
        };
        self.chosen = Some((minute, choice, composition));
    }

    fn perform(&mut self, t: f64, composition: dance::Composition, to: [Hands; CLOCKS], minute: i64, motor: Motor) {
        let (moves, total) = dance::plan(&composition.phases, &self.angles, &to, motor);
        for hands in &mut self.angles {
            *hands = hands.map(|a| a.rem_euclid(360.0));
        }
        self.ambient = None;
        self.variety.note(&composition.name, &composition.tags);
        self.performed = Some(composition);
        self.chosen = None;
        self.timed = None;
        self.plan = Some(Plan { began: t, from: self.angles, to, moves, total, minute });
    }

    /// Six tenths of a second: slow enough not to read as a blink, quick
    /// enough that the digits are clean by the time anyone looks up.
    fn settle(&mut self, dt: f32) {
        const SETTLING: f32 = 0.6;
        let holding = self.minute.is_some() && self.plan.is_none() && self.ambient.is_none();
        let by = (dt / SETTLING).max(0.0);
        self.settled = if holding { (self.settled + by).min(1.0) } else { (self.settled - by).max(0.0) };
    }

    fn advance(&mut self, tick: Tick, knobs: &Knobs) {
        let pace = knobs.pace;
        // Display seconds per engine second. At pace 60 the wall clock is used
        // directly, so the time is right however long the patch has run.
        let rate = 60.0 / pace as f64;
        let born = *self.born.get_or_insert(tick.now - tick.t);
        let clock = if pace >= 59.5 { tick.now } else { born + tick.t * rate } + knobs.offset as f64 * 60.0;
        let motor = Motor { speed: knobs.speed, acc: knobs.speed * 1.6 };
        let (hours24, rest) = (knobs.hours24, knobs.rest);

        if let Some(p) = self.plan.take() {
            let tau = (tick.t - p.began) as f32;
            for i in 0..CLOCKS {
                for h in 0..2 {
                    self.angles[i][h] = dance::angle_at(p.from[i][h], &p.moves[i][h], motor, tau);
                }
            }
            self.doing = Doing::Dancing { to_go: (p.total - tau).max(0.0) };
            if tau >= p.total {
                self.angles = p.to;
                self.minute = Some(p.minute);
                self.landed = tick.t;
            } else {
                self.plan = Some(p);
            }
            return;
        }

        // A dance asked for by hand is performed at once, to the time already
        // showing, and the minute's own dance follows when it is due.
        if let (Some(request), Some(shown)) = (self.request.take(), self.minute) {
            let to = Self::target(shown, hours24, rest);
            let composition = match (request, &self.performed) {
                (Request::Again, Some(last)) => last.clone(),
                _ => {
                    self.asked += 1;
                    self.chosen = None;
                    self.dance_for(shown, dance::DANCES + 1, &to, motor);
                    self.chosen.take().expect("dance_for leaves its choice").2
                }
            };
            self.perform(tick.t, composition, to, shown, motor);
            return;
        }

        // Not dancing. On the first frame go straight to the current time;
        // after that, set off early enough to land on the next minute as it turns.
        let now_minute = (clock / 60.0).floor() as i64;
        let next = match self.minute {
            None => now_minute,
            // Behind, or ahead by more than the minute in hand: the wall clock
            // has moved under the hands. The clocks going back at the end of
            // summer time is the common case, and a new time zone the other;
            // without the second test the hands would stop for the hour.
            Some(shown) if shown < now_minute || shown > now_minute + 1 => now_minute,
            Some(shown) => shown + 1,
        };
        let to = Self::target(next, hours24, rest);
        let choice = knobs.dance;
        self.dance_for(next, choice, &to, motor);
        let total = self.total(next, choice, &to, motor);
        // Engine seconds until the dance has to begin.
        let slack = (next as f64 * 60.0 - clock) / rate - total as f64;
        let ready = self.ambient.as_ref().is_none_or(|a| a.at_rest());
        if self.minute.is_none() || (slack <= 0.0 && ready) {
            let composition = self.chosen.take().expect("dance_for leaves its choice").2;
            self.perform(tick.t, composition, to, next, motor);
            return;
        }
        let next_in = slack.max(0.0) as f32;
        self.doing = match &self.ambient {
            Some(a) => Doing::Drifting { mood: a.name(), next: next_in },
            None => Doing::Holding { next: next_in },
        };

        // Between the time and the next dance: hold still for a while, then
        // drift. The ambient field is asked to settle with room to spare, so
        // the hands are at rest when the dance has to leave.
        const SETTLE: f64 = 4.0;
        let held = knobs.still as f64 * pace as f64 / 60.0;
        match &mut self.ambient {
            Some(ambient) => ambient.step(&mut self.angles, motor, tick.dt as f32, slack < SETTLE),
            None if tick.t - self.landed >= held && slack > SETTLE + 6.0 => {
                let mut rng = Rng::new(self.seed ^ (next as u64).wrapping_mul(0x51ed_270b));
                let moods: Vec<String> = (0..ambient::MOODS).map(|m| format!("mood:{m}")).collect();
                let mood = self
                    .variety
                    .freshest(&moods, || rng.f32())
                    .and_then(|n| moods.iter().position(|m| m == n))
                    .unwrap_or(0);
                self.variety.note("", core::slice::from_ref(&moods[mood]));
                self.ambient = Some(ambient::Ambient::new(&mut rng, mood, COLS, ROWS, CELL));
            }
            None => {}
        }
    }

    /// How long the chosen dance takes from where the hands are now.
    fn total(&mut self, minute: i64, choice: usize, to: &[Hands; CLOCKS], motor: Motor) -> f32 {
        let key = (motor.speed, motor.acc);
        if let Some(t) = &self.timed {
            if t.minute == minute && t.choice == choice && t.motor == key && t.from == self.angles {
                return t.total;
            }
        }
        let (_, composition) = (0, &self.chosen.as_ref().expect("dance_for leaves its choice").2);
        let (_, total) = dance::plan(&composition.phases, &self.angles, to, motor);
        self.timed = Some(Timed { minute, choice, from: self.angles, motor: key, total });
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(e: &mut Engine, knobs: &Knobs, t: &mut f64, offset: f64, seconds: f64) {
        let end = *t + seconds;
        while *t < end {
            *t += 0.05;
            e.step(Tick { t: *t, dt: 0.05, now: *t + offset }, knobs);
        }
    }

    fn shown(e: &Engine) -> i64 {
        e.minute.expect("the hands show a time")
    }

    #[test]
    fn the_hands_land_on_each_minute_as_it_turns() {
        let (mut e, knobs, mut t) = (Engine::new(1), Knobs::default(), 0.0);
        let start = 10.0 * 3600.0;
        // Three seconds into a minute: landed, held, and not yet off again.
        run(&mut e, &knobs, &mut t, start, 303.0);
        assert_eq!(shown(&e), ((start + t) / 60.0).floor() as i64);
        assert_eq!(*e.angles(), Engine::target(shown(&e), true, knobs.rest));
    }

    #[test]
    fn the_clocks_going_back_do_not_stop_the_hands() {
        let (mut e, knobs, mut t) = (Engine::new(2), Knobs::default(), 0.0);
        let summer = 2.0 * 3600.0;
        run(&mut e, &knobs, &mut t, summer, 183.0);
        let before = shown(&e);
        run(&mut e, &knobs, &mut t, summer - 3600.0, 180.0);
        assert_eq!(shown(&e), before + 3 - 60, "the hands are still on the old hour");
        assert_eq!(*e.angles(), Engine::target(shown(&e), true, knobs.rest));
    }

    #[test]
    fn the_clocks_going_forward_arrive_straight_away() {
        let (mut e, knobs, mut t) = (Engine::new(3), Knobs::default(), 0.0);
        run(&mut e, &knobs, &mut t, 3600.0, 120.0);
        let before = shown(&e);
        run(&mut e, &knobs, &mut t, 7200.0, 120.0);
        assert_eq!(shown(&e), before + 2 + 60);
    }

    #[test]
    fn a_dance_asked_for_starts_at_once() {
        let (mut e, knobs, mut t) = (Engine::new(4), Knobs::default(), 0.0);
        run(&mut e, &knobs, &mut t, 0.0, 123.0);
        e.ask(Request::Another);
        run(&mut e, &knobs, &mut t, 0.0, 0.2);
        assert!(matches!(e.doing(), Doing::Dancing { .. }), "{:?}", e.doing());
        assert!(e.settled() < 1.0);
    }
}
