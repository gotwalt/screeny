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

/// How a grid is shared between two formations: 0 is all the first, 1 all the
/// second. The gradient is the interesting one: a soft band whose position is
/// a wave, so stepping its phase carries one formation across the other.
#[derive(Clone, Copy, Debug)]
pub enum Mask {
    Checker,
    Columns,
    Rows,
    Halves { reverse: bool },
    Gradient(Wave),
}

impl Mask {
    fn weight(self, i: usize) -> f32 {
        let (col, row) = (i % COLS, i / COLS);
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        match self {
            Mask::Checker => on((col + row) % 2 == 1),
            Mask::Columns => on(col % 2 == 1),
            Mask::Rows => on(row % 2 == 1),
            Mask::Halves { reverse } => on((col >= COLS / 2) != reverse),
            Mask::Gradient(wave) => {
                let (x, y) = centre(i);
                let s = 0.5 + 0.5 * (wave.k.0 * x + wave.k.1 * y + wave.phase).sin();
                let t = ((s - 0.3) / 0.4).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Mask::Checker => "checker",
            Mask::Columns => "columns",
            Mask::Rows => "rows",
            Mask::Halves { .. } => "halves",
            Mask::Gradient(_) => "band",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Phase {
    pub to: Formation,
    /// A second formation sharing the grid with `to`, and how they share it.
    pub with: Option<(Formation, Mask)>,
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
            // A hand a hair past its place steps back that hair rather than
            // making a needless full turn, so it still lands exactly.
            let d = (s * (to - from)).rem_euclid(360.0);
            s * (if d > 359.9 { d - 360.0 } else { d } + turns)
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
            let mut symmetric = phase.to.symmetric();
            if let Some((other, mask)) = phase.with {
                let (b, w) = (other.pose(i, digits), mask.weight(i));
                // Pair each hand with whichever of the other formation's is
                // nearer, then go part of the way by the shorter arc.
                let straight = shortest(target[0], b[0]).abs() + shortest(target[1], b[1]).abs();
                let crossed = shortest(target[0], b[1]).abs() + shortest(target[1], b[0]).abs();
                let b = if other.symmetric() && crossed < straight { [b[1], b[0]] } else { b };
                target = [target[0] + shortest(target[0], b[0]) * w, target[1] + shortest(target[1], b[1]) * w];
                symmetric &= other.symmetric();
            }
            if symmetric {
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
    let step = |to, timing, turn, extra, rest| Phase { to, with: None, timing, turn, extra, rest };
    let wave = Wave {
        base: if flip { 90.0 } else { 270.0 },
        amp: rng.range(15.0, 30.0),
        k: (rng.range(0.4, 0.8), rng.range(0.3, 0.9) * rng.sign()),
        phase: rng.range(0.0, std::f32::consts::TAU),
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

// ---------------------------------------------------------------------------
// Composing dances
//
// The repertoire above is twelve sentences in a small language. `compose`
// writes new ones. Sampling the grammar blindly gives mostly incoherent motion,
// so a composition has things a random one lacks:
//
//  - a *theme*: one idea of direction that every phase shares. It may change
//    once, and only after a hold, so the change reads as a decision;
//  - an *arc*: gather into order, develop that order, resolve into the time;
//  - a *critic*: every sketch is planned for real. Ones that are too long or
//    short, freeze the grid part-way, barely move, or overdrive a hand are
//    thrown away; the rest are scored for flow, structure, pacing and, above
//    all, freshness against what has been performed lately, and the best is
//    performed.

/// One idea of direction for a dance.
#[derive(Clone, Copy, Debug)]
enum Theme {
    Sweep { reverse: bool },
    Cascade { reverse: bool },
    Diagonal,
    Point((f32, f32)),
}

impl Theme {
    fn new(rng: &mut Rng) -> Theme {
        match rng.u64() % 5 {
            0 => Theme::Sweep { reverse: rng.u64() & 1 == 0 },
            1 => Theme::Cascade { reverse: rng.u64() & 1 == 0 },
            2 => Theme::Diagonal,
            _ => Theme::Point((rng.range(0.5, 7.5), rng.range(0.3, 2.7))),
        }
    }

    /// `back` is the return journey: the same idea, run the other way.
    fn timing(self, gap: f32, back: bool) -> Timing {
        match self {
            Theme::Sweep { reverse } => Timing::Columns { gap, reverse: reverse != back },
            Theme::Cascade { reverse } => Timing::Rows { gap: gap * 1.6, reverse: reverse != back },
            Theme::Diagonal => Timing::Diagonal { gap: gap * 0.8 },
            Theme::Point(focus) => Timing::Ripple { focus, gap, inward: back },
        }
    }

    fn name(self) -> &'static str {
        match self {
            Theme::Sweep { .. } => "sweep",
            Theme::Cascade { .. } => "cascade",
            Theme::Diagonal => "diagonal",
            Theme::Point(_) => "point",
        }
    }
}

/// Ways to develop a formation into a related one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Develop {
    /// The same formation, reached by a full turn: a wave passing through it.
    Spin,
    /// Turned a quarter: lines cross over, rings become spokes.
    Quarter,
    /// The hands of each dial open or close.
    Open,
    /// A wave deepens, or a fan reverses.
    Swell,
    /// A compass focus is carried to the far side.
    Carry,
    /// Alternate dials take the quarter-turned formation: lines become
    /// zigzags and lattices.
    Weave,
    /// Half the grid takes a different formation altogether.
    Split,
    /// A soft band crosses the grid, leaving the quarter-turned (or opened)
    /// formation behind it. Two phases.
    Morph,
}

impl Develop {
    fn name(self) -> &'static str {
        match self {
            Develop::Spin => "spin",
            Develop::Quarter => "quarter",
            Develop::Open => "open",
            Develop::Swell => "swell",
            Develop::Carry => "carry",
            Develop::Weave => "weave",
            Develop::Split => "split",
            Develop::Morph => "morph",
        }
    }
}

impl Formation {
    fn name(self) -> &'static str {
        match self {
            Formation::Digits => "digits",
            Formation::Lines(_) => "lines",
            Formation::Needles(_) => "needles",
            Formation::Fan { .. } => "fan",
            Formation::Rings { spokes: false, .. } => "rings",
            Formation::Rings { spokes: true, .. } => "spokes",
            Formation::Compass { .. } => "compass",
            Formation::Chevron { .. } => "chevrons",
            Formation::Flow { .. } => "flow",
            Formation::Turned { .. } => "scatter",
        }
    }

    /// What this formation can become, and how.
    fn developments(self) -> &'static [Develop] {
        use Develop::*;
        match self {
            Formation::Digits => &[],
            // The time with every corner turned: it can spin as rigid corners
            // (the original's most recognisable move), turn a quarter, or have
            // its wave ebb.
            Formation::Turned { .. } => &[Spin, Quarter, Swell],
            Formation::Lines(_) | Formation::Chevron { .. } => &[Spin, Quarter, Open, Weave, Split, Morph],
            Formation::Needles(_) => &[Spin, Quarter, Open, Weave, Morph],
            Formation::Fan { .. } => &[Spin, Quarter, Swell, Weave],
            Formation::Rings { .. } => &[Spin, Quarter, Morph, Split],
            Formation::Compass { .. } => &[Carry, Spin],
            Formation::Flow { .. } => &[Spin, Quarter, Open, Swell, Morph],
        }
    }

    fn develop(self, how: Develop) -> Formation {
        use Formation::*;
        match (self, how) {
            (Lines(a), Develop::Quarter) => Lines(a + 90.0),
            (Needles(a), Develop::Quarter) => Needles(a + 90.0),
            (Chevron { axis, spread }, Develop::Quarter) => Chevron { axis: axis + 180.0, spread },
            (Fan { base, per_col }, Develop::Quarter) => Fan { base: base + 90.0, per_col },
            (Rings { focus, spokes }, Develop::Quarter) => Rings { focus, spokes: !spokes },
            (Flow { wave, open }, Develop::Quarter) => Flow { wave: Wave { base: wave.base + 90.0, ..wave }, open },
            (Lines(a), Develop::Open) => Chevron { axis: a + 90.0, spread: 90.0 },
            (Needles(a), Develop::Open) => Lines(a + 90.0),
            (Chevron { axis, .. }, Develop::Open) => Lines(axis + 90.0),
            (Flow { wave, open }, Develop::Open) => Flow { wave, open: if open > 90.0 { 8.0 } else { 180.0 } },
            (Fan { base, per_col }, Develop::Swell) => Fan { base, per_col: -per_col },
            (Flow { wave, open }, Develop::Swell) => {
                Flow { wave: Wave { amp: wave.amp * 2.8, phase: wave.phase + 2.0, ..wave }, open }
            }
            (Turned { wave }, Develop::Quarter) => Turned { wave: Wave { base: wave.base + 90.0, ..wave } },
            (Turned { wave }, Develop::Swell) => Turned { wave: Wave { amp: wave.amp * 0.4, phase: wave.phase + 2.0, ..wave } },
            (Compass { focus }, Develop::Carry) => Compass { focus: (COLS as f32 - focus.0, ROWS as f32 - focus.1) },
            (f, _) => f,
        }
    }
}

fn pick<T: Copy>(rng: &mut Rng, from: &[T]) -> T {
    from[(rng.u64() % from.len() as u64) as usize]
}

/// A formation to gather into.
fn motif(rng: &mut Rng, focus: (f32, f32), wave: Wave) -> Formation {
    let edge = if rng.u64() & 1 == 0 { (-1.5, 1.5) } else { (COLS as f32 + 1.5, 1.5) };
    match rng.u64() % 8 {
        0 => Formation::Lines(pick(rng, &[0.0, 45.0, 90.0, 135.0])),
        1 => Formation::Needles(pick(rng, &[0.0, 90.0, 180.0, 270.0])),
        2 => Formation::Fan { base: wave.base, per_col: rng.range(15.0, 30.0) * rng.sign() },
        3 => Formation::Rings { focus, spokes: rng.u64() & 1 == 0 },
        4 => Formation::Compass { focus: edge },
        5 => Formation::Chevron { axis: pick(rng, &[0.0, 180.0]), spread: rng.range(60.0, 120.0) },
        6 => Formation::Flow { wave, open: pick(rng, &[8.0, 180.0]) },
        _ => Formation::Turned { wave: Wave { base: 0.0, amp: rng.range(80.0, 150.0), ..wave } },
    }
}

/// A dance, and how it describes itself.
#[derive(Clone, Debug)]
pub struct Composition {
    pub name: String,
    pub phases: Vec<Phase>,
    /// What it is made of, so that variety can be kept: "motif:rings", "op:weave"...
    pub tags: Vec<String>,
}

/// One candidate: a theme, a formation to gather into, up to two developments,
/// and the way home.
fn sketch(rng: &mut Rng) -> Composition {
    let mut theme = Theme::new(rng);
    let focus = match theme {
        Theme::Point(focus) => focus,
        _ => (COLS as f32 / 2.0, ROWS as f32 / 2.0),
    };
    let way = if rng.u64() & 1 == 0 { Turn::Clockwise } else { Turn::Anticlockwise };
    let gap = rng.range(0.2, 0.42);
    let overlap = |rng: &mut Rng| -rng.range(0.5, 1.2);
    let mut tags = vec![format!("theme:{}", theme.name())];

    // One in five goes straight to the time, with a full turn on the way.
    if rng.u64() % 5 == 0 {
        let (turn, turn_name) = pick(
            rng,
            &[(way, "together"), (Turn::Counter, "counter"), (Turn::Mirror, "mirror"), (Turn::CounterMirror, "bloom"), (Turn::Checker, "checker")],
        );
        tags.extend(["motif:direct".to_string(), format!("turn:{turn_name}")]);
        let phase = Phase { to: Formation::Digits, with: None, timing: theme.timing(gap, false), turn, extra: 1, rest: 0.0 };
        return Composition { name: format!("direct {turn_name}, {}", theme.name()), phases: vec![phase], tags };
    }

    let wave = Wave {
        base: pick(rng, &[0.0, 90.0, 180.0, 270.0]),
        amp: rng.range(15.0, 40.0),
        k: (rng.range(0.35, 0.8), rng.range(0.3, 0.9) * rng.sign()),
        phase: rng.range(0.0, std::f32::consts::TAU),
        bow: rng.range(-30.0, 30.0),
    };
    let gather = motif(rng, focus, wave);
    tags.push(format!("motif:{}", gather.name()));

    let hold = rng.u64() & 1 == 0;
    let mut name = gather.name().to_string();
    let mut phases = vec![Phase {
        to: gather,
        with: None,
        timing: theme.timing(gap, false),
        turn: if rng.u64() % 5 == 0 { way } else { Turn::Shortest },
        extra: 0,
        rest: if hold { rng.range(0.4, 1.0) } else { overlap(rng) },
    }];

    let mut shape = gather;
    let steps = match rng.u64() % 20 {
        0..=2 => 0,
        3..=13 => 1,
        _ => 2,
    };
    for _ in 0..steps {
        let options = shape.developments();
        if options.is_empty() {
            break;
        }
        // A change of theme, at most once, and only out of stillness: hold the
        // formation, then carry on from somewhere else.
        if tags.iter().all(|t| !t.starts_with("theme2:")) && rng.u64() % 4 == 0 {
            if let Some(last) = phases.last_mut() {
                last.rest = rng.range(0.5, 0.9);
            }
            theme = Theme::new(rng);
            tags.push(format!("theme2:{}", theme.name()));
        }

        let how = pick(rng, options);
        let timing = theme.timing(gap, false);
        let phase = |to, with, turn, extra, rest| Phase { to, with, timing, turn, extra, rest };
        match how {
            Develop::Weave => {
                let mask = pick(rng, &[Mask::Checker, Mask::Columns, Mask::Rows]);
                phases.push(phase(shape, Some((shape.develop(Develop::Quarter), mask)), Turn::Shortest, 0, overlap(rng)));
                tags.push(format!("mask:{}", mask.name()));
            }
            Develop::Split => {
                let other = loop {
                    let m = motif(rng, focus, wave);
                    if m.name() != shape.name() && !matches!(m, Formation::Turned { .. }) {
                        break m;
                    }
                };
                let reverse = rng.u64() & 1 == 0;
                let mask = pick(rng, &[Mask::Halves { reverse }, Mask::Rows]);
                phases.push(phase(shape, Some((other, mask)), Turn::Shortest, 0, overlap(rng)));
                tags.extend([format!("mask:{}", mask.name()), format!("motif:{}", other.name())]);
            }
            Develop::Morph => {
                let becomes = shape.develop(if shape.developments().contains(&Develop::Open) && rng.u64() & 1 == 0 {
                    Develop::Open
                } else {
                    Develop::Quarter
                });
                let band = Wave { k: (rng.range(0.5, 0.8) * rng.sign(), rng.range(0.0, 0.4)), ..wave };
                for advance in [0.0, 2.6] {
                    let mask = Mask::Gradient(Wave { phase: band.phase + advance, ..band });
                    phases.push(phase(shape, Some((becomes, mask)), Turn::Shortest, 0, overlap(rng)));
                }
                shape = becomes;
                tags.push("mask:band".to_string());
            }
            _ => {
                shape = shape.develop(how);
                let (turn, extra) = match how {
                    Develop::Spin => (pick(rng, &[way, way, Turn::Mirror, Turn::Checker]), 1),
                    Develop::Open => (Turn::Counter, 0),
                    Develop::Quarter => (pick(rng, &[way, Turn::Shortest]), 0),
                    _ => (Turn::Shortest, 0),
                };
                phases.push(phase(shape, None, turn, extra, overlap(rng)));
            }
        }
        tags.push(format!("op:{}", how.name()));
        name = format!("{name} > {}", how.name());
    }
    if let Some(last) = phases.last_mut() {
        last.rest = last.rest.max(-0.6) + rng.range(0.0, 0.4);
    }

    phases.push(Phase {
        to: Formation::Digits,
        with: None,
        timing: theme.timing(gap, true),
        turn: if rng.u64() % 4 == 0 { way } else { Turn::Shortest },
        extra: 0,
        rest: 0.0,
    });
    let themes: Vec<&str> = tags.iter().filter_map(|t| t.strip_prefix("theme:").or(t.strip_prefix("theme2:"))).collect();
    let name = format!("{name}, {}", themes.join(" then "));
    Composition { name, phases, tags }
}

/// What the critic measures of a planned dance.
struct Review {
    total: f32,
    /// Fastest any hand goes, in multiples of the motor's speed.
    peak: f32,
    /// Longest stretch, in seconds, during which nothing on the grid moves.
    frozen: f32,
    /// Mean degrees travelled per hand.
    travel: f32,
    /// Mean share of hands in motion: 1 is everything always moving.
    flow: f32,
}

fn review(phases: &[Phase], from: &[Hands; CLOCKS], digits: &[Hands; CLOCKS], motor: Motor) -> Review {
    let (moves, total) = plan(phases, from, digits, motor);
    let dt = 0.1;
    let (mut peak, mut frozen, mut still, mut moving) = (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
    let mut prev: Vec<Hands> = from.to_vec();
    let samples = (total / dt) as usize;
    for k in 1..=samples {
        let (mut fastest, mut busy) = (0.0_f32, 0);
        for i in 0..CLOCKS {
            for h in 0..2 {
                let a = angle_at(from[i][h], &moves[i][h], motor, k as f32 * dt);
                let v = (a - prev[i][h]).abs() / dt;
                fastest = fastest.max(v);
                busy += (v > 2.0) as usize;
                prev[i][h] = a;
            }
        }
        peak = peak.max(fastest / motor.speed);
        moving += busy as f32 / (CLOCKS * 2) as f32;
        still = if fastest < 1.0 { still + dt } else { 0.0 };
        frozen = frozen.max(still);
    }
    let travel = moves.iter().flatten().flatten().map(|m| m.travel.abs()).sum::<f32>() / (CLOCKS * 2) as f32;
    Review { total, peak, frozen, travel, flow: moving / samples.max(1) as f32 }
}

/// A new dance from `from` to `digits`: the best of several sketches that pass
/// the critic. Above all it should not repeat itself: `variety` remembers what
/// has been performed, a shape seen lately is not allowed back, and sketches
/// made of well-worn parts score lower.
pub fn compose(
    rng: &mut Rng,
    from: &[Hands; CLOCKS],
    digits: &[Hands; CLOCKS],
    motor: Motor,
    variety: &crate::variety::Variety,
) -> Composition {
    // Bounds scale with the motor: a slow motor is allowed a long dance.
    let pace = 100.0 / motor.speed;
    let mut best: Option<(f32, Composition)> = None;
    let mut passed = 0;
    for _ in 0..48 {
        let c = sketch(rng);
        if variety.too_soon(&c.name) {
            continue;
        }
        let r = review(&c.phases, from, digits, motor);
        if !(6.0 * pace..=20.0 * pace).contains(&r.total) || r.peak > 2.05 || r.frozen > 1.3 || r.travel < 80.0 {
            continue;
        }
        // Flow alone would always choose the simplest dance, where everything
        // moves all the time; structure is worth something too.
        let structure = 0.17 * (c.phases.len().min(4) as f32 - 1.0);
        let score = 1.2 * r.flow + structure - (r.total / pace - 12.0).abs() / 6.0 - r.frozen - (r.peak - 1.3).max(0.0)
            - 3.0 * variety.staleness(&c.tags)
            + rng.range(0.0, 0.5);
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, c));
        }
        passed += 1;
        if passed == 8 {
            break;
        }
    }
    best.map(|(_, c)| c).unwrap_or_else(|| named((rng.u64() % DANCES as u64) as usize, rng))
}

/// A dance from the repertoire, as a composition.
pub fn named(which: usize, rng: &mut Rng) -> Composition {
    let (name, phases) = dance(which, rng);
    Composition { name: name.to_string(), phases, tags: vec![format!("named:{name}")] }
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

    /// Composed dances are dances: they land on the time, within the critic's
    /// bounds, and there are a great many different ones.
    #[test]
    fn composed_dances_land_and_vary() {
        let (from, to) = (pose(23, 59), pose(0, 0));
        let mut names = std::collections::BTreeSet::new();
        let (mut fell_back, mut direct_runs) = (0, 0);
        for seed in 0..300 {
            let Composition { name, phases, tags } =
                compose(&mut Rng::new(seed), &from, &to, MOTOR, &Default::default());
            assert!(!tags.is_empty());
            fell_back += !name.contains(',') as usize;
            direct_runs += name.starts_with("direct") as usize;
            let (moves, total) = plan(&phases, &from, &to, MOTOR);
            assert!(total <= 20.5, "{name}: {total}s");
            for i in 0..CLOCKS {
                for h in 0..2 {
                    let end = angle_at(from[i][h], &moves[i][h], MOTOR, total + 1.0);
                    assert!(shortest(end, to[i][h]).abs() < 0.01, "{name}: clock {i} hand {h}");
                }
            }
            names.insert(name);
        }
        assert!(names.len() > 60, "only {} distinct shapes", names.len());
        assert!(fell_back < 15, "critic rejected everything {fell_back} times");
        assert!((20..100).contains(&direct_runs), "{direct_runs} of 300 took the plain route: the critic is biased");
        let direct = names.iter().filter(|n| n.starts_with("direct")).count();
        eprintln!("{} distinct shapes in 300; {direct_runs} runs direct ({direct} shapes); {fell_back} fell back", names.len());
    }

    /// A day of dances, one a minute, each starting where the last ended. The
    /// point of the composer is that the tenth hour does not look like the
    /// first: no shape comes back too soon, there are a great many shapes, and
    /// every part of the vocabulary gets used.
    #[test]
    fn a_day_of_dances_does_not_repeat_itself() {
        let mut variety = crate::variety::Variety::default();
        let mut rng = Rng::new(2026);
        let mut names: Vec<String> = Vec::new();
        let mut uses = std::collections::BTreeMap::<String, usize>::new();
        for minute in 0..1440_u32 {
            let (from, to) = (pose(minute / 60, minute % 60), pose((minute + 1) / 60 % 24, (minute + 1) % 60));
            let c = compose(&mut rng, &from, &to, MOTOR, &variety);
            variety.note(&c.name, &c.tags);
            for t in &c.tags {
                *uses.entry(t.clone()).or_default() += 1;
            }
            names.push(c.name);
        }
        let gap = (0..names.len())
            .filter_map(|i| names[..i].iter().rposition(|n| *n == names[i]).map(|j| i - j))
            .min()
            .unwrap_or(usize::MAX);
        let distinct = names.iter().collect::<std::collections::BTreeSet<_>>().len();
        let share = |prefix: &str| {
            let of: Vec<(&String, &usize)> = uses.iter().filter(|(t, _)| t.starts_with(prefix)).collect();
            let total: usize = of.iter().map(|(_, n)| **n).sum();
            let (lo, hi) = (of.iter().map(|(_, n)| **n).min().unwrap_or(0), of.iter().map(|(_, n)| **n).max().unwrap_or(0));
            (of.len(), lo as f32 / total as f32, hi as f32 / total as f32)
        };
        let table: Vec<String> = uses.iter().filter(|(t, _)| t.starts_with("motif:") || t.starts_with("op:")).map(|(t, n)| format!("{t}={n}")).collect();
        eprintln!("{}", table.join(" "));
        let (motifs, rarest, commonest) = share("motif:");
        let (ops, rarest_op, commonest_op) = share("op:");
        eprintln!(
            "1440 dances: {distinct} distinct shapes, soonest repeat after {gap}; {motifs} motifs used {:.0}%..{:.0}%, {ops} operators {:.0}%..{:.0}%",
            rarest * 100.0, commonest * 100.0, rarest_op * 100.0, commonest_op * 100.0
        );
        assert!(gap > crate::variety::Variety::SPACING, "a shape came back after {gap} dances");
        assert!(distinct > 250, "only {distinct} shapes in a day");
        assert!(motifs >= 10 && rarest > 0.04, "a motif is being neglected: {rarest}");
        assert!(ops >= 8 && rarest_op > 0.04, "an operator is being neglected: {rarest_op}");
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
