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
use crate::face::{pose_of, resting_of, shown, Hands, Rest, CELL, CLOCKS, COLS, DEFAULT_REST, REST, RESTS, ROWS};
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

/// What the minute is for. Either way the hands land on the time as the
/// minute turns and the dials not in it recede; the mode is how long the time
/// is held before the hands are free again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The time from :00 to :10, then the hands drift until the next dance.
    Art,
    /// The time from :00 to :45, and the dance fits in what is left.
    Time,
}

impl Mode {
    /// The stops of a `mode` parameter, in its order.
    pub const NAMES: [&'static str; 2] = ["art", "time"];

    pub fn of(v: f32) -> Mode {
        if v >= 0.5 { Mode::Time } else { Mode::Art }
    }

    /// Seconds past the minute the time is held: the hands may not drift
    /// before it, and the next minute's dance may not set off before it.
    pub fn held(self) -> f32 {
        match self {
            Mode::Art => 10.0,
            Mode::Time => 45.0,
        }
    }
}

/// Everything about the hands' behaviour that is a matter of taste.
#[derive(Clone, Copy, Debug)]
pub struct Knobs {
    pub mode: Mode,
    /// `0` varies, `1..=DANCES` names a dance, one past them always composes.
    pub dance: usize,
    /// Hand speed, degrees per second.
    pub speed: f32,
    pub hours24: bool,
    pub rest: Rest,
    /// Seconds the resting dials take to dim, and to come back.
    pub fade: f32,
    /// Reduce motion: only the less active dances, and the drifting, with no
    /// hand ever turning faster than [`CALM_LIMIT`]. One switch: the owner
    /// found a top-speed slider in degrees a second confusing (2026-11-07).
    pub calm: bool,
}

/// Under reduce motion, the fastest any hand may turn, degrees per second.
pub const CALM_LIMIT: f32 = 60.0;

/// Under [`Knobs::calm`] the motor runs at this share of the limit, so that what
/// the slider says is about what the hands do. Room for overlaps was tried, at
/// 0.8, and no dance used it: the hands just stopped short of the limit.
const CALM_MOTOR: f32 = 0.95;
/// However early in the minute the hands landed, the time is held this long.
const HELD_AT_LEAST: f64 = 3.0;
/// If the minute is nearer than this, the next dance is planned at once, even
/// though the fade has not finished: it could be due before the fade is over.
const PLAN_NO_LATER: f64 = 25.0;
/// The most time one frame may take off a fade. Composing a dance takes the
/// chip about half a second, and a fade that lost that much would be a jump.
const FADE_STEP: f32 = 0.1;

impl Default for Knobs {
    /// The owner's "Gold and Blue" behaviour (2026-10-28).
    fn default() -> Self {
        Knobs {
            mode: Mode::Art,
            dance: 0,
            speed: 100.0,
            hours24: true,
            rest: RESTS[DEFAULT_REST],
            fade: 1.5,
            calm: false,
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
    angles: [Hands; CLOCKS],
    /// The minute the hands currently show, or are moving towards.
    minute: Option<i64>,
    /// How far the picture has settled onto the time, 0 while the hands are
    /// dancing or drifting and 1 while it is being held. Resting dials recede
    /// by this much, so a dance is at full strength throughout and the digits
    /// are left alone on the panel once it lands.
    settled: f32,
    /// How far each dial has receded towards its rest: 0 full, 1 at rest.
    dims: [f32; CLOCKS],
    plan: Option<Plan>,
    /// Engine time at which the hands last landed on a time.
    landed: f64,
    /// Ambient motion between holding the time and the next dance.
    ambient: Option<ambient::Ambient>,
    /// The dance chosen for a minute, kept so it is composed only once.
    chosen: Option<(i64, usize, (Option<u32>, u32), dance::Composition)>,
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
            angles: [[REST; 2]; CLOCKS],
            minute: None,
            settled: 0.0,
            dims: [0.0; CLOCKS],
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

    /// The four digits a minute-of-day is drawn with: in 12-hour time an hour
    /// before 10 has no leading zero, and its dials rest like any unused one.
    pub fn shown(minute: i64, hours24: bool) -> [u32; 4] {
        let (h, m) = Self::digits(minute, hours24);
        shown(h, m, !hours24)
    }

    pub fn target(minute: i64, hours24: bool, rest: Rest) -> [Hands; CLOCKS] {
        pose_of(Self::shown(minute, hours24), rest)
    }

    /// How far each dial has receded towards its rest, 0 to 1: dials not part
    /// of the time, and only while there is a time to read. During a dance
    /// every hand is a dancer and the grid is at full strength.
    pub fn dims(&self) -> &[f32; CLOCKS] {
        &self.dims
    }

    /// One frame: move the hands, then ease the picture towards "the time is
    /// being held" or back.
    pub fn step(&mut self, tick: Tick, knobs: &Knobs) {
        self.advance(tick, knobs);
        self.settle(tick.dt as f32, knobs);
    }

    /// The dance for the move to `minute`. Asked for by number it comes from
    /// the repertoire (or, at 13, is always composed). Left to vary, most are
    /// composed afresh and the rest drawn from the repertoire.
    fn dance_for(
        &mut self,
        minute: i64,
        choice: usize,
        to: &[Hands; CLOCKS],
        motor: Motor,
        limit: Option<f32>,
        room: f32,
    ) {
        let key = (limit.map(f32::to_bits), room.to_bits());
        if let Some((m, c, l, _)) = &self.chosen {
            if (*m, *c, *l) == (minute, choice, key) {
                return;
            }
        }
        let mut rng =
            Rng::new(self.seed ^ (minute as u64).wrapping_mul(0x9e37_79b9) ^ self.asked.wrapping_mul(0x85eb_ca6b));
        let composed = match choice {
            0 => rng.u64() % 10 < 6,
            n => n > dance::DANCES,
        };
        let drawn = if composed {
            None
        } else if choice == 0 {
            // From the repertoire, whichever has gone longest unperformed.
            // Under reduce motion, of the ones made for it.
            let first = if limit.is_some() { dance::GENTLE } else { 0 };
            let names: Vec<String> =
                (first..dance::DANCES).map(|i| dance::named(i, &mut Rng::new(0)).tags[0].clone()).collect();
            let freshest = self.variety.freshest(&names, || rng.f32()).and_then(|n| names.iter().position(|m| m == n));
            Some(dance::named(first + freshest.unwrap_or(0), &mut rng))
        } else {
            Some(dance::named(choice - 1, &mut rng))
        };
        // Reduce motion and the time mode are filters and nothing else: a
        // dance from the repertoire that breaks the limit, or would set off
        // before the time has been held long enough, gives way to one composed
        // to keep both.
        let drawn = drawn.filter(|d| dance::within(&d.phases, &self.angles, to, motor, limit, room));
        let composition = match drawn {
            Some(d) => d,
            None => dance::compose(&mut rng, &self.angles, to, motor, &self.variety, limit, room),
        };
        self.chosen = Some((minute, choice, key, composition));
    }

    fn perform(
        &mut self,
        t: f64,
        composition: dance::Composition,
        to: [Hands; CLOCKS],
        minute: i64,
        motor: Motor,
        limit: Option<f32>,
        room: f32,
    ) {
        // A dance is judged where the hands were when it was chosen, and they
        // may have drifted since, which moves its overlaps. The limit is a
        // limit: judged again here, and a dance that now breaks it, or no
        // longer fits, gives way to one that does.
        let composition = if dance::within(&composition.phases, &self.angles, &to, motor, limit, room) {
            composition
        } else {
            let mut rng = Rng::new(self.seed ^ minute as u64);
            dance::fallback(&mut rng, &self.angles, &to, motor, limit, room)
        };
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

    /// Ease the picture towards "the time is being held" and back, so that the
    /// resting dials recede once the hands land and are at full strength for
    /// every dance.
    fn settle(&mut self, dt: f32, knobs: &Knobs) {
        let holding = self.minute.is_some() && self.plan.is_none() && self.ambient.is_none();
        let by = (dt.min(FADE_STEP) / knobs.fade.max(0.05)).max(0.0);
        self.settled = if holding { (self.settled + by).min(1.0) } else { (self.settled - by).max(0.0) };

        let rests = self.minute.filter(|_| holding).map_or([false; CLOCKS], |m| {
            resting_of(Self::shown(m, knobs.hours24))
        });
        for (dim, rest) in self.dims.iter_mut().zip(rests) {
            *dim = if rest { (*dim + by).min(1.0) } else { (*dim - by).max(0.0) };
        }
    }

    fn advance(&mut self, tick: Tick, knobs: &Knobs) {
        let clock = tick.now;
        let limit = knobs.calm.then_some(CALM_LIMIT);
        let speed = limit.map_or(knobs.speed, |l| knobs.speed.min(l * CALM_MOTOR));
        let motor = Motor { speed, acc: speed * 1.6 };
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
                    self.dance_for(shown, dance::DANCES + 1, &to, motor, limit, f32::INFINITY);
                    self.chosen.take().expect("dance_for leaves its choice").3
                }
            };
            self.perform(tick.t, composition, to, shown, motor, limit, f32::INFINITY);
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
        // Composing a dance is the one slow thing here. It can wait for a fade
        // to finish, when nothing on the panel is moving; started as the hands
        // land, it stalled the first half second of every fade.
        let holding = self.minute.is_some() && self.ambient.is_none();
        if holding && self.settled < 1.0 && next as f64 * 60.0 - clock > PLAN_NO_LATER {
            self.doing = Doing::Holding { next: 0.0 };
            return;
        }
        let to = Self::target(next, hours24, rest);
        let choice = knobs.dance;
        let held = f64::from(knobs.mode.held());
        // The dance may not set off before the time has been held: it has the
        // rest of the minute, and no more.
        let room = (60.0 - held) as f32;
        self.dance_for(next, choice, &to, motor, limit, room);
        let total = self.total(next, choice, &to, motor);
        // Engine seconds until the dance has to begin.
        let slack = next as f64 * 60.0 - clock - total as f64;
        let ready = self.ambient.as_ref().is_none_or(|a| a.at_rest());
        if self.minute.is_none() || (slack <= 0.0 && ready) {
            let composition = self.chosen.take().expect("dance_for leaves its choice").3;
            self.perform(tick.t, composition, to, next, motor, limit, room);
            return;
        }
        let next_in = slack.max(0.0) as f32;
        self.doing = match &self.ambient {
            Some(a) => Doing::Drifting { mood: a.name(), next: next_in },
            None => Doing::Holding { next: next_in },
        };

        // Between the time and the next dance: hold the time until the mode
        // says, then drift. The ambient field is asked to settle with room to
        // spare, so the hands are at rest when the dance has to leave.
        const SETTLE: f64 = 4.0;
        let into_minute = clock - (clock / 60.0).floor() * 60.0;
        match &mut self.ambient {
            Some(ambient) => ambient.step(&mut self.angles, motor, tick.dt as f32, slack < SETTLE),
            None if into_minute >= held && tick.t - self.landed >= HELD_AT_LEAST && slack > SETTLE + 6.0 => {
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
        let composition = &self.chosen.as_ref().expect("dance_for leaves its choice").3;
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
    fn nothing_is_composed_while_the_dials_are_fading() {
        let (mut e, knobs, mut t) = (Engine::new(5), Knobs::default(), 0.0);
        run(&mut e, &knobs, &mut t, 0.0, 61.0);
        let mut seen = 0;
        while seen < 200 {
            run(&mut e, &knobs, &mut t, 0.0, 0.05);
            let landed = matches!(e.doing, Doing::Holding { .. }) && e.plan.is_none();
            if landed && e.settled < 1.0 {
                seen += 1;
                assert!(e.chosen.is_none(), "composed {:.2} into the fade", e.settled);
            } else if landed && seen > 0 {
                return;
            }
        }
        panic!("the fade never finished");
    }

    #[test]
    fn a_late_frame_does_not_skip_the_fade() {
        let (mut e, knobs, mut t) = (Engine::new(6), Knobs::default(), 0.0);
        run(&mut e, &knobs, &mut t, 0.0, 121.0);
        assert!(e.plan.is_none());
        e.settled = 0.0;
        t += 0.5;
        e.step(Tick { t, dt: 0.5, now: t }, &knobs);
        assert!(e.settled <= FADE_STEP / knobs.fade + 1e-4, "{}", e.settled);
    }

    /// Reduce motion is the whole show, slower: an hour of it must stay under
    /// the limit on every frame, and still dance, drift and tell the time.
    #[test]
    fn reduce_motion_is_the_same_show_under_a_speed_limit() {
        let knobs = Knobs { calm: true, ..Knobs::default() };
        let (mut e, mut t) = (Engine::new(7), 0.0);
        run(&mut e, &knobs, &mut t, 0.0, 70.0);
        let mut before = *e.angles();
        let (dt, mut fastest) = (0.05_f32, 0.0_f32);
        let (mut dancing, mut drifting) = (0, 0);
        let mut names = alloc::collections::BTreeSet::new();
        for _ in 0..(3600.0 / dt) as usize {
            run(&mut e, &knobs, &mut t, 0.0, dt as f64 - 0.01);
            for i in 0..CLOCKS {
                for h in 0..2 {
                    // The short way round: angles are wrapped between dances.
                    fastest = fastest.max(dance::shortest(before[i][h], e.angles()[i][h]).abs() / dt);
                }
            }
            before = *e.angles();
            dancing += usize::from(matches!(e.doing(), Doing::Dancing { .. }));
            drifting += usize::from(matches!(e.doing(), Doing::Drifting { .. }));
            if let Some(p) = e.performed() {
                names.insert(p.name.clone());
            }
        }
        assert!(fastest <= CALM_LIMIT, "a hand turned at {fastest:.1} deg/s");
        assert!(fastest >= CALM_LIMIT * 0.6, "nothing came near the limit: {fastest:.1} deg/s");
        assert!(dancing > 3000 && drifting > 3000, "it stopped moving: {dancing} dancing, {drifting} drifting");
        assert!(names.len() >= 30, "only {} different dances in an hour", names.len());
    }

    /// Art holds the time from :00 to :10 and Time from :00 to :45: through
    /// an hour of minutes, the hands are on the time for at least that long
    /// after each one turns, and at full speed and under reduce motion alike
    /// they go back to dancing and, in art, drifting.
    #[test]
    fn each_mode_holds_the_time_for_as_long_as_it_says() {
        for (mode, calm) in [(Mode::Art, false), (Mode::Time, false), (Mode::Art, true), (Mode::Time, true)] {
            let knobs = Knobs { mode, calm, ..Knobs::default() };
            let (mut e, mut t) = (Engine::new(11), 0.0);
            // Land on a minute first: the opening minute is whatever is left.
            run(&mut e, &knobs, &mut t, 0.0, 61.0);
            let (dt, mut early, mut drifted, mut least) = (0.05, 0, 0, 60.0_f64);
            let mut gone: Option<f64> = None;
            for _ in 0..(3600.0 / dt) as usize {
                run(&mut e, &knobs, &mut t, 0.0, dt - 0.01);
                let into = t.rem_euclid(60.0);
                let on_time = e.plan.is_none() && *e.angles() == Engine::target(shown(&e), true, knobs.rest);
                drifted += usize::from(matches!(e.doing(), Doing::Drifting { .. }));
                if into < 1.0 {
                    gone = None;
                } else if !on_time && gone.is_none() {
                    gone = Some(into);
                    least = least.min(into);
                    early += usize::from(into < f64::from(mode.held()));
                }
            }
            let names = alloc::format!("{mode:?}{}", if calm { " under reduce motion" } else { "" });
            assert_eq!(early, 0, "{names}: the time went at :{least:.1}, before :{}", mode.held());
            assert_eq!(drifted > 0, mode == Mode::Art, "{names}: drifted for {drifted} frames");
        }
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

#[cfg(test)]
mod twelve_hour {
    use super::*;

    /// 9:05 in 12-hour time has no leading zero: its six dials are at rest, and
    /// fade once the time is held. 24-hour time keeps its zero, and so does 10:05.
    #[test]
    fn a_twelve_hour_clock_drops_its_leading_zero() {
        let nine = 9 * 60 + 5;
        assert_eq!(Engine::shown(nine, false), [crate::face::BLANK, 9, 0, 5]);
        assert_eq!(Engine::shown(nine, true), [0, 9, 0, 5]);
        assert_eq!(Engine::shown(10 * 60 + 5, false), [1, 0, 0, 5]);
        assert_eq!(Engine::shown(21 * 60 + 5, false), [crate::face::BLANK, 9, 0, 5]);
        let resting = resting_of(Engine::shown(nine, false));
        for row in 0..ROWS {
            for col in 0..2 {
                assert!(resting[row * COLS + col], "dial ({col}, {row}) of the blank is not at rest");
            }
        }
        let knobs = Knobs { hours24: false, ..Knobs::default() };
        let (mut e, mut t) = (Engine::new(9), 0.0);
        while t < 63.0 {
            t += 0.05;
            e.step(Tick { t, dt: 0.05, now: nine as f64 * 60.0 + t }, &knobs);
        }
        assert_eq!(e.dims()[0], 1.0, "the blank's top-left dial has not faded");
        assert_eq!(e.dims()[2], 0.0, "the 9 has faded");
    }
}
