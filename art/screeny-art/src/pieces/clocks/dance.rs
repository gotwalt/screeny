//! Choreography for the clock hands.
//!
//! A dance is a sequence of phases. Each phase sends every hand to a
//! *formation* (a pose for the whole grid), setting off according to a *timing*
//! field and turning according to a *turn* rule. All motion goes through one
//! [`Motor`] profile, which is what keeps dozens of different dances feeling
//! like the same machine. Phases may overlap: a hand's moves simply add, so it
//! cruises through a formation instead of stopping on it.

use super::{Hands, CLOCKS, COLS, ROWS};
use crate::rng::Rng;

/// A stepper-like move: accelerate at `acc`, cruise at `speed`, decelerate.
/// No springs, no overshoot: a hand with further to go just takes longer.
#[derive(Clone, Copy, Debug)]
pub struct Motor {
    pub speed: f32,
    pub acc: f32,
}

impl Motor {
    pub fn duration(self, distance: f32) -> f32 {
        let ramp = self.speed * self.speed / self.acc;
        if distance >= ramp {
            distance / self.speed + self.speed / self.acc
        } else {
            2.0 * (distance / self.acc).sqrt()
        }
    }

    /// Distance covered `t` seconds into a move of `distance`.
    pub fn position(self, distance: f32, t: f32) -> f32 {
        let total = self.duration(distance);
        if t <= 0.0 {
            return 0.0;
        }
        if t >= total {
            return distance;
        }
        let t_ramp = (self.speed / self.acc).min(total / 2.0);
        if t < t_ramp {
            0.5 * self.acc * t * t
        } else if t > total - t_ramp {
            distance - 0.5 * self.acc * (total - t).powi(2)
        } else {
            0.5 * self.acc * t_ramp * t_ramp + self.acc * t_ramp * (t - t_ramp)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Move {
    /// Seconds after the plan starts.
    pub start: f32,
    /// Signed degrees; positive is clockwise.
    pub travel: f32,
}

/// Signed shortest rotation from `a` to `b`, in -180..=180.
pub fn shortest(a: f32, b: f32) -> f32 {
    (b - a + 540.0).rem_euclid(360.0) - 180.0
}

/// A smooth angle over the grid: one sine with its own direction, plus a bow
/// across the width. Never uniform, never quite symmetrical.
#[derive(Clone, Copy, Debug)]
pub struct Wave {
    pub base: f32,
    pub amp: f32,
    /// Radians per clock, across and down.
    pub k: (f32, f32),
    pub phase: f32,
    /// Degrees added at the left and right edges relative to the middle.
    pub bow: f32,
}

impl Wave {
    fn at(self, i: usize) -> f32 {
        let (x, y) = centre(i);
        let across = (x - COLS as f32 / 2.0) / (COLS as f32 / 2.0);
        self.base + self.amp * (self.k.0 * x + self.k.1 * y + self.phase).sin() + self.bow * across * across
    }
}

/// A pose for the whole grid. Positions are in clock units: (0, 0) is the top
/// left of the grid, (8, 3) the bottom right.
#[derive(Clone, Copy, Debug)]
pub enum Formation {
    /// The time being moved to.
    Digits,
    /// Hands opposite each other, every clock the same: a field of parallel lines.
    Lines(f32),
    /// Hands together, every clock the same.
    Needles(f32),
    /// Lines whose angle steps from column to column: a frozen wave.
    Fan { base: f32, per_col: f32 },
    /// Lines around a focus, like rings on water (or pointing at it, like spokes).
    Rings { focus: (f32, f32), spokes: bool },
    /// Needles all pointing at a focus, like compasses at a magnet.
    Compass { focus: (f32, f32) },
    /// Hands closed upwards like a bud, or a chevron opening by `spread` degrees.
    Chevron { axis: f32, spread: f32 },
    /// Hands `open` degrees apart about an angle that follows a wave: folded
    /// needles when nearly closed, lines at 180.
    Flow { wave: Wave, open: f32 },
    /// The digits, but every clock's pair of hands turned rigidly by a wave:
    /// the time, scattered, with its corners intact.
    Turned { wave: Wave },
}

#[derive(Clone, Copy, Debug)]
pub enum Timing {
    Together,
    Columns { gap: f32, reverse: bool },
    Rows { gap: f32, reverse: bool },
    Diagonal { gap: f32 },
    /// Outwards from a focus (or inwards to it), `gap` seconds per clock of distance.
    Ripple { focus: (f32, f32), gap: f32, inward: bool },
}

#[derive(Clone, Copy, Debug)]
pub enum Turn {
    /// Whichever way is nearer.
    Shortest,
    Clockwise,
    Anticlockwise,
    /// The two hands of a clock go opposite ways.
    Counter,
    /// Left half clockwise, right half anticlockwise.
    Mirror,
    /// Both of the above at once: fully symmetric about the middle.
    CounterMirror,
    /// Alternate clocks go opposite ways.
    Checker,
}

#[derive(Clone, Copy, Debug)]
pub struct Phase {
    pub to: Formation,
    pub timing: Timing,
    pub turn: Turn,
    /// Full turns added on top of what it takes to get there.
    pub extra: u8,
    /// Seconds of stillness after the last hand arrives (negative overlaps the
    /// next phase, which is what makes a dance flow).
    pub rest: f32,
}

fn centre(i: usize) -> (f32, f32) {
    ((i % COLS) as f32 + 0.5, (i / COLS) as f32 + 0.5)
}

/// Degrees clockwise from up, from a clock towards a point.
fn bearing(i: usize, (fx, fy): (f32, f32)) -> f32 {
    let (cx, cy) = centre(i);
    (fx - cx).atan2(cy - fy).to_degrees().rem_euclid(360.0)
}

impl Formation {
    fn pose(self, i: usize, digits: &[Hands; CLOCKS]) -> Hands {
        match self {
            Formation::Digits => digits[i],
            Formation::Lines(a) => [a, a + 180.0],
            Formation::Needles(a) => [a, a],
            Formation::Fan { base, per_col } => {
                let a = base + per_col * (i % COLS) as f32;
                [a, a + 180.0]
            }
            Formation::Rings { focus, spokes } => {
                let a = bearing(i, focus) + if spokes { 0.0 } else { 90.0 };
                [a, a + 180.0]
            }
            Formation::Compass { focus } => [bearing(i, focus); 2],
            Formation::Chevron { axis, spread } => [axis - spread * 0.5, axis + spread * 0.5],
            Formation::Flow { wave, open } => [wave.at(i) - open * 0.5, wave.at(i) + open * 0.5],
            Formation::Turned { wave } => [digits[i][0] + wave.at(i), digits[i][1] + wave.at(i)],
        }
    }

    /// Whether the two hands are interchangeable in this pose, so each clock
    /// may give either hand either end.
    fn symmetric(self) -> bool {
        !matches!(self, Formation::Digits | Formation::Turned { .. })
    }
}

impl Timing {
    fn delay(self, i: usize) -> f32 {
        let (col, row) = (i % COLS, i / COLS);
        match self {
            Timing::Together => 0.0,
            Timing::Columns { gap, reverse } => gap * if reverse { COLS - 1 - col } else { col } as f32,
            Timing::Rows { gap, reverse } => gap * if reverse { ROWS - 1 - row } else { row } as f32,
            Timing::Diagonal { gap } => gap * (col + row) as f32,
            Timing::Ripple { focus, gap, inward } => {
                let dist = |j: usize| {
                    let (cx, cy) = centre(j);
                    ((cx - focus.0).powi(2) + (cy - focus.1).powi(2)).sqrt()
                };
                let furthest = (0..CLOCKS).map(dist).fold(0.0_f32, f32::max);
                gap * if inward { furthest - dist(i) } else { dist(i) }
            }
        }
    }
}

impl Turn {
    /// +1 clockwise, -1 anticlockwise, 0 for "whichever is nearer".
    fn sign(self, i: usize, hand: usize) -> f32 {
        let (col, row) = (i % COLS, i / COLS);
        let by = |clockwise: bool| if clockwise { 1.0 } else { -1.0 };
        match self {
            Turn::Shortest => 0.0,
            Turn::Clockwise => 1.0,
            Turn::Anticlockwise => -1.0,
            Turn::Counter => by(hand == 0),
            Turn::Mirror => by(col < COLS / 2),
            Turn::CounterMirror => by(hand == 0) * by(col < COLS / 2),
            Turn::Checker => by((col + row) % 2 == 0),
        }
    }

    fn travel(self, i: usize, hand: usize, from: f32, to: f32, extra: u8) -> f32 {
        let turns = extra as f32 * 360.0;
        let s = self.sign(i, hand);
        if s == 0.0 {
            let d = shortest(from, to);
            d + turns * if d < 0.0 { -1.0 } else { 1.0 }
        } else {
            // Allow a hair of slack so a hand already in place does not make a
            // needless full turn.
            let d = (s * (to - from)).rem_euclid(360.0);
            s * (if d > 359.9 { 0.0 } else { d } + turns)
        }
    }
}

pub type Moves = Vec<[Vec<Move>; 2]>;

/// Plan every hand's moves for `phases`, starting from `from`. The last phase
/// must end on `Formation::Digits`. Returns the moves and the total duration.
pub fn plan(phases: &[Phase], from: &[Hands; CLOCKS], digits: &[Hands; CLOCKS], motor: Motor) -> (Moves, f32) {
    let mut moves: Moves = (0..CLOCKS).map(|_| [Vec::new(), Vec::new()]).collect();
    let mut pose = *from;
    let mut begin = 0.0_f32;
    let mut total = 0.0_f32;
    for phase in phases {
        let mut arrived = begin;
        for i in 0..CLOCKS {
            let mut target = phase.to.pose(i, digits);
            if phase.to.symmetric() {
                let cost = |t: Hands| shortest(pose[i][0], t[0]).abs() + shortest(pose[i][1], t[1]).abs();
                let swapped = [target[1], target[0]];
                if cost(swapped) < cost(target) {
                    target = swapped;
                }
            }
            let start = begin + phase.timing.delay(i);
            for h in 0..2 {
                let travel = phase.turn.travel(i, h, pose[i][h], target[h], phase.extra);
                if travel.abs() > 1e-3 {
                    moves[i][h].push(Move { start, travel });
                    arrived = arrived.max(start + motor.duration(travel.abs()));
                }
                pose[i][h] += travel;
            }
        }
        total = total.max(arrived);
        begin = (arrived + phase.rest).max(begin);
    }
    (moves, total)
}

/// Hand angle `tau` seconds into a plan.
pub fn angle_at(from: f32, moves: &[Move], motor: Motor, tau: f32) -> f32 {
    moves.iter().fold(from, |a, m| a + m.travel.signum() * motor.position(m.travel.abs(), tau - m.start))
}

pub const DANCES: usize = 12;

/// The repertoire. `which` is 0..DANCES; `rng` varies direction, focus and
/// angles, so the same dance is rarely performed the same way twice.
pub fn dance(which: usize, rng: &mut Rng) -> (&'static str, Vec<Phase>) {
    use Formation::*;
    let flip = rng.u64() & 1 == 0;
    let focus = (rng.range(1.0, 7.0), rng.range(0.5, 2.5));
    let middle = (COLS as f32 / 2.0, ROWS as f32 / 2.0);
    let side = if flip { (-1.5, 1.5) } else { (9.5, 1.5) };
    let far_side = (COLS as f32 - side.0, side.1);
    let sweep = Timing::Columns { gap: 0.35, reverse: flip };
    let sweep_back = Timing::Columns { gap: 0.3, reverse: !flip };
    let way = if flip { Turn::Anticlockwise } else { Turn::Clockwise };
    let step = |to, timing, turn, extra, rest| Phase { to, timing, turn, extra, rest };
    let wave = Wave {
        base: if flip { 90.0 } else { 270.0 },
        amp: rng.range(15.0, 30.0),
        k: (rng.range(0.4, 0.8), rng.range(0.3, 0.9) * rng.sign()),
        phase: rng.range(0.0, 6.28),
        bow: rng.range(-25.0, 25.0),
    };

    match which % DANCES {
        // Every corner spins as a rigid shape, column after column.
        0 => ("formation", vec![step(Digits, sweep, way, 1, 0.0)]),
        // Hands part in opposite directions, symmetric about the middle.
        1 => (
            "bloom",
            vec![step(Digits, Timing::Ripple { focus: middle, gap: 0.3, inward: false }, Turn::CounterMirror, 1, 0.0)],
        ),
        // Gather into parallel lines, hold, roll a full-turn wave across, resolve.
        2 => (
            "line wave",
            vec![
                step(Lines(45.0), Timing::Together, Turn::Shortest, 0, 0.8),
                step(Lines(45.0), sweep, way, 1, 0.4),
                step(Digits, sweep_back, Turn::Shortest, 0, 0.0),
            ],
        ),
        // Rings round a focus; a ripple turns them to spokes and back out to digits.
        3 => (
            "ripple",
            vec![
                step(Rings { focus, spokes: false }, Timing::Ripple { focus, gap: 0.25, inward: false }, Turn::Shortest, 0, -1.0),
                step(Rings { focus, spokes: true }, Timing::Ripple { focus, gap: 0.3, inward: false }, way, 1, -1.0),
                step(Digits, Timing::Ripple { focus, gap: 0.25, inward: true }, Turn::Shortest, 0, 0.0),
            ],
        ),
        // Every needle follows a magnet carried across the panel.
        4 => (
            "magnet",
            vec![
                step(Compass { focus: side }, sweep, Turn::Shortest, 0, -0.5),
                step(Compass { focus: (middle.0, -2.0) }, sweep, Turn::Shortest, 0, -1.5),
                step(Compass { focus: far_side }, sweep, Turn::Shortest, 0, -0.5),
                step(Digits, sweep, Turn::Shortest, 0, 0.0),
            ],
        ),
        // All hands drop, row by row, then climb into the digits from the bottom up.
        5 => (
            "cascade",
            vec![
                step(Needles(180.0), Timing::Rows { gap: 0.5, reverse: false }, Turn::Mirror, 0, 0.3),
                step(Digits, Timing::Rows { gap: 0.6, reverse: true }, Turn::Mirror, 1, 0.0),
            ],
        ),
        // A frozen wave of fanned lines that travels through itself.
        6 => (
            "fan",
            vec![
                step(Fan { base: 0.0, per_col: 22.5 }, Timing::Together, Turn::Shortest, 0, -0.5),
                step(Fan { base: 180.0, per_col: 22.5 }, sweep, way, 0, -1.0),
                step(Fan { base: 0.0, per_col: -22.5 }, sweep, way, 0, 0.2),
                step(Digits, sweep_back, Turn::Shortest, 0, 0.0),
            ],
        ),
        // Neighbours turn against each other along the diagonal.
        7 => ("checker", vec![step(Digits, Timing::Diagonal { gap: 0.3 }, Turn::Checker, 1, 0.0)]),
        // Close to a bud, open like scissors to a line, carry on round to the digits.
        8 => (
            "scissors",
            vec![
                step(Needles(0.0), Timing::Together, Turn::Shortest, 0, 0.3),
                step(Lines(90.0), Timing::Ripple { focus: middle, gap: 0.2, inward: false }, Turn::Counter, 0, -0.8),
                step(Chevron { axis: 180.0, spread: 90.0 }, Timing::Ripple { focus: middle, gap: 0.2, inward: false }, Turn::Counter, 0, 0.2),
                step(Digits, Timing::Ripple { focus: middle, gap: 0.25, inward: true }, Turn::Shortest, 0, 0.0),
            ],
        ),
        // Folded needles settle into a shallow wave, which deepens and rolls
        // on before the digits surface out of it.
        9 => (
            "swell",
            vec![
                step(Flow { wave, open: 6.0 }, sweep, Turn::Shortest, 0, -1.0),
                step(Flow { wave: Wave { amp: wave.amp * 3.0, phase: wave.phase + 2.0, ..wave }, open: 6.0 }, sweep, way, 0, -1.0),
                step(Digits, sweep_back, Turn::Shortest, 0, 0.0),
            ],
        ),
        // The new time appears at once but scattered, each clock's corner
        // turned by a wave, then the wave drains away.
        10 => (
            "scatter",
            vec![
                step(Turned { wave: Wave { base: 0.0, amp: rng.range(90.0, 150.0), ..wave } }, Timing::Ripple { focus, gap: 0.2, inward: false }, way, 0, 0.3),
                step(Digits, Timing::Ripple { focus, gap: 0.3, inward: true }, Turn::Shortest, 0, 0.0),
            ],
        ),
        // A vortex: rings about the middle, spun a full turn from the rim inwards.
        _ => (
            "vortex",
            vec![
                step(Rings { focus: middle, spokes: false }, Timing::Together, Turn::Shortest, 0, 0.2),
                step(Rings { focus: middle, spokes: false }, Timing::Ripple { focus: middle, gap: 0.35, inward: true }, way, 1, -1.5),
                step(Digits, Timing::Ripple { focus: middle, gap: 0.3, inward: false }, way, 0, 0.0),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pieces::clocks::pose;

    const MOTOR: Motor = Motor { speed: 100.0, acc: 160.0 };

    #[test]
    fn motor_moves_are_smooth_and_exact() {
        for distance in [3.0, 45.0, 360.0, 540.0] {
            let total = MOTOR.duration(distance);
            assert!((MOTOR.position(distance, total) - distance).abs() < 1e-3);
            let mut prev = 0.0;
            for k in 1..=200 {
                let p = MOTOR.position(distance, total * k as f32 / 200.0);
                let v = (p - prev) / (total / 200.0);
                assert!(v >= -1e-3 && v <= MOTOR.speed * 1.01, "distance {distance}: speed {v}");
                prev = p;
            }
        }
    }

    /// Every dance, in many variations, ends exactly on the digits, in a
    /// reasonable time, and never asks a hand to go much faster than two
    /// overlapping moves allow.
    #[test]
    fn every_dance_lands_on_the_time() {
        let (from, to) = (pose(9, 25), pose(9, 26));
        for which in 0..DANCES {
            for variation in 0..8 {
                let (name, phases) = dance(which, &mut Rng::new(variation));
                let (moves, total) = plan(&phases, &from, &to, MOTOR);
                assert!((2.0..45.0).contains(&total), "{name} takes {total}s");
                for i in 0..CLOCKS {
                    for h in 0..2 {
                        let end = angle_at(from[i][h], &moves[i][h], MOTOR, total + 1.0);
                        assert!(shortest(end, to[i][h]).abs() < 0.01, "{name}: clock {i} hand {h} ends at {end}");
                        let mut prev = from[i][h];
                        for k in 1..=(total * 30.0) as usize {
                            let a = angle_at(from[i][h], &moves[i][h], MOTOR, k as f32 / 30.0);
                            let v = (a - prev).abs() * 30.0;
                            assert!(v <= MOTOR.speed * 2.05, "{name}: clock {i} hand {h} at {v} deg/s");
                            prev = a;
                        }
                    }
                }
            }
        }
    }
}
