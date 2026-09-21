//! The flight: Reynolds' boids in 3D, the invisible world they steer round,
//! and the camera that is one of them.
//!
//! Nothing here knows about pixels. It is stepped on a **fixed 1/60 s
//! timestep** ([`STEP`]) so the flight is the same whatever rate the frames are
//! drawn at, and every number in it is a metre, a metre per second or a radian
//! per second, so the limits can be read and checked as what they are.
//!
//! A bird is not a clone of the bird beside it, and the flock is not a cloud
//! of interchangeable dots (card 177, the owner: "the boids seem to be very
//! evenly separated from each other, which is not lifelike"). Four things put
//! the unevenness there, and all four are spatial - none of them makes the
//! flight busier:
//!
//! - **topological** neighbours ([`NEIGH`]), not a radius, so a dense knot is
//!   not pushed apart by the twenty birds behind it;
//! - **soft** separation ([`CORE`]) that only becomes a wall at about a
//!   wingspan, so two birds may pass close and the spacing is a preference
//!   rather than a lattice constant;
//! - **individuals**: per-bird preferred spacing and airspeed ([`ROOM`],
//!   [`PEP`]) and two slow oscillators of restlessness each;
//! - **clans** ([`CLANS`]): loose sub-groups that fly closer to one another
//!   than to strangers and keep gently together, drawn with a shared taste for
//!   room so that some knots are tight and some are loose.
//!
//! The camera is `birds[0]`. It is a boid: it separates, aligns and coheres by
//! the same rules, it is in every other bird's neighbour list and they are in
//! its own, and it is held by the same kind of speed band and turn-rate limit.
//!
//! Where it differs is worth stating plainly, because every one of these was
//! put there by a measurement that failed without it, and a camera that is
//! quietly not a bird is a lie if it is not written down:
//!
//! - it wants a **seat** at the trailing edge or flank, along the flock's
//!   ground track, a little below it, and [`SEAT_LEAD`] seconds **ahead of the
//!   flock's vertical motion** - seated on where the flock is, it is a second
//!   behind every dive, and a second is the flock leaving the frame;
//! - its **speed band is wider at both ends** (0.5x to 1.3x). The bottom end
//!   is the one that matters: a camera that cannot fly slower than the flock
//!   can never drop back once it has drifted ahead;
//! - it may **turn harder** - 1.15x a bird's, and up to [`CAM_SLACK`] while
//!   its altitude is off the flock's. A camera whose turn radius is larger
//!   than the flock's circle is thrown off it every time they wheel;
//! - it may **climb and dive a quarter steeper**, and it pays only a third of
//!   what a bird pays in speed for a climb ([`TRADE`]). A chaser held to the
//!   limits of the thing it chases arrives late every time;
//! - it keeps more **personal space** (`near * 1.15`) and pushes harder to
//!   keep it, because a bird at arm's length fills the panel with one wing -
//!   and its separation is the old **hard** 1/d wall, where a bird's is soft;
//! - it is in **no clan**, and it feels none of the per-bird restlessness.
//!
//! None of that reaches the picture directly: what the panel sees is
//! [`Sim::aim`], which is smoothed, rate-limited and leashed on its own.

use crate::rng::Rng;
use std::f32::consts::TAU;

/// The simulation's fixed timestep. Everything in here is integrated at this
/// rate however often frames are asked for.
pub const STEP: f32 = 1.0 / 60.0;

/// Wingspan, metres. The one number that sets the scale of everything seen.
pub const SPAN: f32 = 0.92;

/// World up.
pub const UP: V3 = V3 { x: 0.0, y: 1.0, z: 0.0 };

/// What a bird's bank angle is worked out against: a turn at lateral
/// acceleration `a` is flown at a roll of `atan(a / G)`, as a real one is.
const G: f32 = 9.81;

/// How far out the flock's world reaches (metres, in the horizontal plane),
/// and the floor and ceiling it flies between.
/// The world has to be large against how fast the flock crosses it, or the
/// flock lives at the boundary and every minute is a hard turn back. At
/// 5 m/s this is about half a minute wide.
pub const BOUND: f32 = 135.0;
/// The world is **tall**: a flock that may dive for eight seconds needs
/// somewhere to dive to. Nothing in the picture depends on how high the flock
/// actually is - the sky is drawn from the view ray's elevation, not from an
/// altitude - so height costs nothing but room, and the old -24..28 with a
/// 13 m margin left a free band only 26 m deep, which is four seconds of
/// descent. Altitude is now free and only the *rate* is limited.
pub const FLOOR: f32 = -70.0;
pub const CEILING: f32 = 70.0;
/// How deep into the floor or ceiling the push reaches. Wide, because a bird
/// that may only turn at its limit needs the room to do it in.
const MARGIN: f32 = 18.0;

/// Ceilings on the view itself, whatever the flock does: radians a second it
/// may swing, radians a second it may roll, and how far over it may lean.
/// These are the numbers that decide whether this is calm or nauseous, so
/// they are named and they are hard.
const VIEW_YAW: f32 = 0.35;
const ROLL_RATE: f32 = 0.10;
const VIEW_ROLL: f32 = 0.30;
/// How far the view may tilt off level, as a sine, at `lift` 0 and 1. The
/// panel is 21 degrees from the middle to the top edge, so at 0.30 (17
/// degrees) the horizon is still in shot - and a horizon that rides up and
/// down the panel is the whole reason a dive reads as a dive.
const VIEW_TILT: (f32, f32) = (0.16, 0.30);
/// How far off the view's axis the flock's middle is ever allowed to get.
/// The panel is 38 degrees from the middle to the side edge and 21 to the top,
/// so 16 keeps it in shot with room for the birds around it - sideways. Up
/// and down it does not: a flock 16 degrees above the axis has half of itself
/// off the top of a panel that is 21 degrees tall. The panel is not square
/// and neither is the leash, and the vertical one only started to matter when
/// the flight got some vertical in it.
const LEASH: f32 = 0.28;
const LEASH_UP: f32 = 0.17;
/// The outer wall on the view's pitch, as a sine: 19 degrees, just inside the
/// 21 the panel has from the middle to the top edge, so some horizon is always
/// in shot however hard the leash is pulling.
const TILT_MAX: f32 = 0.33;
/// The view's absolute angular rate ceiling, radians a second. Unlike
/// [`VIEW_YAW`] - which is what the smoothing aims for - nothing may exceed
/// this, not even the leash.
const HARD_YAW: f32 = 0.45;
/// Steepest climb and steepest dive, as sines of the flight-path angle, at
/// `lift` 0 and at `lift` 1.
///
/// They are **not the same number**, because a climb and a dive are not the
/// same manoeuvre: a diving bird is trading height for speed and can go much
/// steeper than one dragging itself uphill. That asymmetry, with the speed
/// trade below, is most of what makes vertical motion look like flying rather
/// than like a lift.
///
/// Both are held by a spring, not a clamp (see [`Sim::fly`]), so they are the
/// angle a bird runs out of lift at rather than a wall - `PITCH_SPRING` says
/// how firmly, in metres a second squared per unit of sine over.
const CLIMB_UP: (f32, f32) = (0.30, 0.52);
const CLIMB_DOWN: (f32, f32) = (0.40, 0.80);
const PITCH_SPRING: f32 = 110.0;
/// How much of gravity a bird gets back diving and pays climbing, metres a
/// second squared per unit of sine. A dive is faster; a climb is slower.
const TRADE: f32 = 2.6;

/// **The drawn lean** (card 124), which is a picture and not a flight.
///
/// `roll` is honest: `atan(lateral / G)`, which at the turn rates cards 168 and
/// 177 tuned comes out at six or seven degrees at its very worst. That is what
/// a real bird at that turn radius does, and at sixteen LEDs across it is about
/// one LED of difference in the wing's projection - invisible. The owner asked
/// for birds that visibly lean into their turns, so [`Bird::lean`] is the roll
/// the bird is *drawn* at: the honest one, multiplied up and then bent over
/// towards a ceiling, so a gentle turn reads and a hard one does not become a
/// barrel roll.
///
/// `LEAN_GAIN` is the multiplier at `lean` 1; `LEAN_MAX` is the ceiling the
/// tanh approaches, radians (54 degrees, which is a real bird's hard turn).
/// Nothing in the flight reads either of them.
const LEAN_GAIN: f32 = 6.0;
const LEAN_MAX: f32 = 0.95;
/// How long the drawn lean takes to follow the honest roll, going over and
/// coming back, seconds. A bird **commits** to a turn and levels out lazily,
/// so the two are not the same number; and easing it at all is what keeps a
/// six-fold gain from turning the roll's own small movements into a flicker.
const LEAN_IN: f32 = 0.22;
const LEAN_OUT: f32 = 0.45;

/// How many drifting blobs the world is laid out with. They are all built
/// from the seed and the `terrain` parameter says how many of them are real
/// this frame, so turning it up and down does not re-roll the world.
pub const BLOBS: usize = 5;

/// How far from its middle a flock is allowed to stray before it is gathered
/// back. Nothing at all happens inside it.
const FLOCK_RADIUS: f32 = 11.0;

/// How much of its cruise turn rate a bird may add while avoiding something,
/// at full urgency.
pub const DODGE_TURN: f32 = 2.2;

/// How many seconds ahead of the flock's vertical motion the camera sits.
const SEAT_LEAD: f32 = 0.85;

/// The most turn budget the camera may be given while its altitude is off the
/// flock's, as a multiple of a bird's. Following a dive is the one thing it
/// cannot do with a bird's agility.
pub const CAM_SLACK: f32 = 3.40;

/// How many neighbours a bird attends to, whatever their distance.
///
/// This is the **topological** neighbourhood - Ballerini et al. 2008 measured
/// six or seven in real starling flocks - and it is most of the difference
/// between a lattice and a live flock. Under a *metric* rule every bird inside
/// one radius pushes, so a dense patch pushes itself apart and the flock
/// relaxes into equal gaps in every direction, which is exactly what the owner
/// saw. Under a topological one a bird in a knot is pushed by its seven
/// nearest and not at all by the twenty behind them, so the knot may stay a
/// knot while the air beside it stays empty.
pub const NEIGH: usize = 7;

/// How many loose sub-groups the flock is drawn into. Clan-mates keep less
/// distance from one another and pull together gently wherever they have
/// drifted to, so the flock is a few travelling knots rather than a cloud of
/// interchangeable dots. Nothing in the picture marks them: they are only a
/// reason for the gaps to be uneven.
pub const CLANS: usize = 9;
/// What is not a clan: the camera.
const NO_CLAN: u8 = 255;

/// How much closer clan-mates are content to fly than strangers, and how far
/// apart they may drift before they stop being a group at all.
const CLAN_ROOM: f32 = 0.55;
const CLAN_REACH: f32 = 15.0;

/// Inside about a wingspan and a quarter the separation stops being a
/// preference and becomes a wall, so birds never fly through one another
/// however soft the rest of the curve is.
const CORE: f32 = 1.15;

/// The spread of preferred spacing, as a multiple of `separation`, and of
/// preferred airspeed, as a multiple of the flock's band. Birds are
/// individuals: some fly tight and quick, some hang back with room around
/// them, and it does not change over the flight.
///
/// Room is drawn **per clan** and then jittered per bird. Drawn per bird it
/// averages out - every knot ends up the same density as every other and the
/// flock is uniform again, which measured as a nearest-neighbour CV of 0.3
/// where the card asks for 0.4-0.6. Per clan, some sub-groups fly tight and
/// some fly loose, and the distribution of gaps is broad because the flock
/// really is made of different things.
pub const ROOM: (f32, f32) = (0.40, 1.85);
pub const ROOM_JITTER: (f32, f32) = (0.82, 1.20);
pub const PEP: (f32, f32) = (0.90, 1.12);

// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl V3 {
    pub const ZERO: V3 = v3(0.0, 0.0, 0.0);

    pub fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    pub fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }

    pub fn scale(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }

    pub fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: V3) -> V3 {
        v3(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn len2(self) -> f32 {
        self.dot(self)
    }

    pub fn len(self) -> f32 {
        self.len2().sqrt()
    }

    /// Unit vector, or `fallback` if there is no direction to speak of.
    pub fn unit_or(self, fallback: V3) -> V3 {
        let n = self.len();
        if n > 1e-6 {
            self.scale(1.0 / n)
        } else {
            fallback
        }
    }

    pub fn lerp(self, o: V3, t: f32) -> V3 {
        self.add(o.sub(self).scale(t))
    }

    /// Turn this unit vector towards `to` by at most `most` radians, taking
    /// `frac` of the way there if that is less.
    ///
    /// A plain lerp between directions is what the view used to do, and it has
    /// a hole in it: fed a direction nearly opposite its own it produces a very
    /// short vector whose direction is whatever the rounding says, and the view
    /// snaps through 180 degrees. Turning along the arc cannot do that, and
    /// `most` is a hard ceiling on how fast the picture may swing.
    pub fn turn_towards(self, to: V3, frac: f32, most: f32) -> V3 {
        let dot = self.dot(to).clamp(-1.0, 1.0);
        let angle = dot.acos();
        if angle < 1e-5 {
            return to;
        }
        let take = (angle * frac).min(most);
        if take >= angle {
            return to;
        }
        let sin = angle.sin();
        if sin < 1e-6 {
            // Exactly opposite: any arc will do, so take the one the world's
            // up gives and let the next step continue it.
            return self.across(UP).unit_or(self.cross(UP)).scale(take.sin()).add(self.scale(take.cos()));
        }
        self.scale((angle - take).sin() / sin).add(to.scale(take.sin() / sin)).unit_or(to)
    }

    /// The part of `self` at right angles to the unit vector `dir`.
    pub fn across(self, dir: V3) -> V3 {
        self.sub(dir.scale(self.dot(dir)))
    }

    /// Shortened to at most `max`, never lengthened.
    pub fn clamp_len(self, max: f32) -> V3 {
        let n = self.len();
        if n > max && n > 1e-9 {
            self.scale(max / n)
        } else {
            self
        }
    }
}

// --------------------------------------------------------------------------

/// Everything that is taste, turned into the numbers the flight is flown by.
/// Rebuilt from the parameters every frame, so a slider moves the flock
/// without restarting it.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// The distance a bird of average taste keeps. Each bird scales it by its
    /// own `room`; alignment has no radius at all any more (see [`NEIGH`]).
    pub separation: f32,
    pub cohesion: f32,
    pub speed: (f32, f32),
    /// Largest angular rate a bird may fly at, radians per second.
    pub turn: f32,
    /// How close the camera rides; sets its seat and its personal space.
    pub near: f32,
    /// How much of a real bird's bank angle the view is allowed to take.
    pub bank: f32,
    /// 0..2: how far the birds are *drawn* leaning into a turn. 0 is the
    /// honest roll and nothing else; 1 is [`LEAN_GAIN`]. It is read by the
    /// drawing alone - see [`Bird::lean`] - so it never changes the flight.
    pub lean: f32,
    /// Wingbeats a second at cruise.
    pub beat: f32,
    /// 0..1: how often and how hard the flight changes its mind. It sets the
    /// per-bird restlessness, how quickly - and how unevenly - the flock finds
    /// something new to fly towards, and how often it is taken by a shared
    /// dive or climb.
    pub wild: f32,
    /// 0..1: how much of the motion is vertical. It opens the climb and dive
    /// angles, the height the flock's next interest may be at, the size of a
    /// shared surge, and how far the view is allowed to pitch to show it.
    pub lift: f32,
    /// How many of the world's blobs are in play, 0..=[`BLOBS`].
    pub blobs: usize,
}

/// How hard a bird's own restlessness steers it, metres a second squared, at
/// `wild` 1. A cruising bird's whole lateral budget is about 1.1, so this is a
/// lean rather than a jink - but it never stops, and no two birds lean the
/// same way at the same time.
const REST: f32 = 0.75;

impl Default for Tuning {
    fn default() -> Self {
        Tuning::of(0.90, 6.0, 0.8, 2.4)
    }
}

impl Tuning {
    /// `calm` 0..1: 1 is the widest, slowest turns.
    pub fn of(calm: f32, near: f32, bank: f32, beat: f32) -> Self {
        let calm = calm.clamp(0.0, 1.0);
        Tuning {
            separation: 5.6,
            // Wide enough that the whole flock is always in one another's
            // cohesion range. At 14 m - a plausible-looking number, and the
            // first one tried - a flock that is ever pulled apart further than
            // that cannot see itself any more and never comes back together.
            cohesion: 26.0,
            // A calm flock also flies a little slower, which is most of what
            // "gentle" looks like from inside it.
            speed: (4.6 - 1.4 * calm, 7.4 - 1.8 * calm),
            // A turn rate is a turn *radius*: at 5 m/s this is a 4.5 m wheel
            // at `calm` 0 and a 33 m one at 1. It is also, more than anything
            // else, how fast the view has to pan to hold the flock - a camera
            // twelve metres behind a flock wheeling at 48 deg/s must pan at
            // nearly 48 deg/s - so this is the calmness control in the
            // strongest sense.
            turn: 1.10 - 0.95 * calm,
            near,
            bank,
            lean: 1.0,
            beat,
            wild: 0.45,
            lift: 0.50,
            blobs: 3,
        }
    }
}

// --------------------------------------------------------------------------

/// A bird. The camera is one of these.
#[derive(Clone, Copy, Debug)]
pub struct Bird {
    pub pos: V3,
    pub vel: V3,
    /// Roll, radians. Positive rolls the right wing up.
    ///
    /// The honest one - `atan(lateral / G)` - and the flight's own: the view
    /// leans on it through `bank`. What the bird is *drawn* at is `lean`.
    pub roll: f32,
    /// The roll the bird is **drawn** at, radians: `roll` exaggerated towards
    /// [`LEAN_MAX`] and eased (card 124). A drawing quantity only - no force,
    /// no limit and nothing the tests measure about the flight reads it, so
    /// turning `lean` up and down cannot change where a single bird goes.
    pub lean: f32,
    /// Wingbeat phase, radians.
    pub phase: f32,
    /// 0 beating, 1 gliding. Low-passed, so it eases in and out.
    pub glide: f32,
    /// Fixed per-bird variation, so no two beat quite alike.
    pub trim: f32,
    /// How much room this one likes, as a multiple of `separation`, and how
    /// fast it likes to fly, as a multiple of the flock's band. Individuals,
    /// not clones: identical birds with identical radii settle into identical
    /// gaps, which is the lattice the owner saw.
    pub room: f32,
    pub pep: f32,
    /// Which loose sub-group it belongs to, or [`NO_CLAN`] for the camera.
    clan: u8,
    /// Two slow, independently drawn oscillators - rate and phase each - that
    /// steer this bird gently sideways and up and down for ever. Smooth, so it
    /// is restlessness and not a twitch; incommensurate between birds, so the
    /// flock's shape breathes instead of pulsing.
    rest: [f32; 4],
}

impl Bird {
    fn new(pos: V3, vel: V3, rooms: &[f32; CLANS], rng: &mut Rng) -> Bird {
        let clan = (rng.range(0.0, CLANS as f32) as usize).min(CLANS - 1);
        Bird {
            pos,
            vel,
            roll: 0.0,
            lean: 0.0,
            phase: rng.range(0.0, TAU),
            glide: 0.0,
            trim: rng.range(0.86, 1.16),
            room: rooms[clan] * rng.range(ROOM_JITTER.0, ROOM_JITTER.1),
            pep: rng.range(PEP.0, PEP.1),
            clan: clan as u8,
            rest: [
                rng.range(0.055, 0.40),
                rng.range(0.0, TAU),
                rng.range(0.055, 0.40),
                rng.range(0.0, TAU),
            ],
        }
    }

    pub fn speed(self) -> f32 {
        self.vel.len()
    }

    pub fn heading(self) -> V3 {
        self.vel.unit_or(v3(0.0, 0.0, 1.0))
    }

    /// Right, up, forward for this bird **as it is drawn**: its heading,
    /// rolled by `lean`.
    ///
    /// The drawn lean enters here and nowhere else, which is what makes it
    /// safe: this is read by the pose and by nothing that flies.
    pub fn frame(self) -> (V3, V3, V3) {
        let fwd = self.heading();
        let right = fwd.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
        let up = right.cross(fwd);
        let (s, c) = self.lean.sin_cos();
        (right.scale(c).add(up.scale(s)), up.scale(c).sub(right.scale(s)), fwd)
    }
}

// --------------------------------------------------------------------------

/// A sphere of air the birds will not fly into, drifting on three long
/// periods of its own. Never drawn.
#[derive(Clone, Copy, Debug)]
pub struct Blob {
    base: V3,
    amp: V3,
    rate: V3,
    phase: V3,
    pub radius: f32,
}

impl Blob {
    pub fn at(&self, t: f32) -> V3 {
        v3(
            self.base.x + self.amp.x * (self.rate.x * t + self.phase.x).sin(),
            self.base.y + self.amp.y * (self.rate.y * t + self.phase.y).sin(),
            self.base.z + self.amp.z * (self.rate.z * t + self.phase.z).sin(),
        )
    }
}

/// Something over there worth flying towards, for a while.
#[derive(Clone, Copy, Debug)]
struct Interest {
    at: V3,
    to: V3,
    next: f32,
    pull: f32,
    /// How long it takes to move to where it is going, seconds. Drawn afresh
    /// each time, so the flock is sometimes drawn gently and sometimes swings
    /// round after it.
    over: f32,
}

/// A thing the whole flock does at once: a dive and its recovery (what a
/// murmuration does when a hawk comes), or a climb, sometimes with a swirl
/// through it so it winds upwards rather than going straight up.
///
/// It is an **event**, which means the quiet between them matters as much as
/// the surge itself: the gap is drawn from a squared uniform, so most of the
/// flight is cruise and now and then, without warning, the flock drops.
#[derive(Clone, Copy, Debug)]
struct Surge {
    start: f32,
    over: f32,
    /// Metres a second squared at the peak. Positive dives first.
    amp: f32,
    /// Sideways, metres a second squared, for the spiral.
    swirl: f32,
    next: f32,
}

/// The invisible geometry. Laid out from the seed; the blobs drift on periods
/// that share no common multiple, so the space the flock moves through is
/// never the same twice.
#[derive(Clone, Debug)]
pub struct World {
    pub blobs: Vec<Blob>,
    interest: Interest,
    surge: Surge,
}

impl World {
    fn new(rng: &mut Rng, count: usize) -> World {
        let blobs = (0..count)
            .map(|_| {
                let a = rng.range(0.0, TAU);
                let r = rng.range(0.25, 0.80) * BOUND;
                Blob {
                    base: v3(r * a.cos(), rng.range(-8.0, 12.0), r * a.sin()),
                    amp: v3(rng.range(8.0, 18.0), rng.range(3.0, 8.0), rng.range(8.0, 18.0)),
                    // Periods of 50 to 210 s, drawn independently, so the three
                    // axes of one blob and the blobs among themselves have no
                    // period in common.
                    // Long periods: a blob that drifts faster than about a
                    // metre a second stops being scenery and starts chasing
                    // birds, which no turn rate can answer.
                    rate: v3(
                        TAU / rng.range(90.0, 260.0),
                        TAU / rng.range(90.0, 260.0),
                        TAU / rng.range(90.0, 260.0),
                    ),
                    phase: v3(rng.range(0.0, TAU), rng.range(0.0, TAU), rng.range(0.0, TAU)),
                    radius: rng.range(10.0, 18.0),
                }
            })
            .collect();
        World {
            blobs,
            interest: Interest { at: V3::ZERO, to: V3::ZERO, next: 0.0, pull: 0.0, over: 16.0 },
            // The first one is a good half minute away, so the flight is
            // already cruising when anybody looks.
            surge: Surge { start: 0.0, over: 8.0, amp: 0.0, swirl: 0.0, next: 35.0 },
        }
    }

    /// How far outside a blob a bird starts to steer round it. Wide enough
    /// that a bird flying at the middle of one at full speed, turning no
    /// harder than its limit, is round it with room to spare.
    fn reach(&self, tune: &Tuning) -> f32 {
        7.0 + 1.4 * tune.speed.1 / tune.turn.max(0.15)
    }

    fn step(&mut self, t: f32, dt: f32, wild: f32, lift: f32, rng: &mut Rng) {
        if t >= self.interest.next {
            let a = rng.range(0.0, TAU);
            let r = rng.range(10.0, BOUND * 0.7);
            // Somewhere over there, and - this is new - somewhere *up* or
            // *down* there. An attractor picked in a 16 m band is a flight
            // that is horizontal whatever else it does.
            let high = rng.range(-1.0, 1.0) * (8.0 + 46.0 * lift);
            self.interest.to = v3(r * a.cos(), high, r * a.sin());
            // What made the old flight predictable was not how often it turned
            // but that the gaps between turns were all alike (40-110 s). A
            // cubed uniform is mostly short with a long tail: a run of quick
            // changes of mind, then a long quiet cruise, then another. The
            // *spread* is what `wild` opens up, and the mean follows.
            self.interest.next = t + 6.0 + rng.f32().powi(3) * (170.0 - 120.0 * wild);
            // Mostly a gentle interest, once in a while a strong one.
            self.interest.pull = rng.f32().powi(2) * (0.7 + 1.3 * wild);
            self.interest.over = rng.range(5.0, 24.0);
        }
        // It moves to where it is going over its own handful of seconds, so
        // the flock is drawn rather than yanked.
        self.interest.at =
            self.interest.at.lerp(self.interest.to, 1.0 - (-dt / self.interest.over).exp());

        if t >= self.surge.next {
            self.surge.start = t;
            self.surge.over = rng.range(5.0, 13.0);
            // Mostly a dive and recovery; about a third of the time a climb.
            let down = if rng.f32() < 0.66 { 1.0 } else { -1.0 };
            self.surge.amp = down * rng.range(1.6, 4.6) * (0.35 + 1.3 * lift);
            self.surge.swirl = rng.range(-1.0, 1.0) * 1.4 * wild;
            // The quiet afterwards, squared so most gaps are long: these have
            // to stay events. At `wild` 0 they are two or three minutes apart,
            // at 1 about forty seconds.
            self.surge.next =
                t + self.surge.over + 18.0 + rng.f32().powi(2) * (190.0 - 150.0 * wild);
        }
    }

    /// The shared surge this instant: metres a second squared up (positive)
    /// and sideways. A full sine over the event, so a dive recovers into a
    /// climb and comes back to level flight - a dive that does not recover is
    /// a crash - and both ends are zero, so nothing ever steps.
    fn surge_at(&self, t: f32) -> (f32, f32) {
        let u = (t - self.surge.start) / self.surge.over;
        if !(0.0..1.0).contains(&u) {
            return (0.0, 0.0);
        }
        (-self.surge.amp * (TAU * u).sin(), self.surge.swirl * (std::f32::consts::PI * u).sin())
    }
}

// --------------------------------------------------------------------------

pub struct Sim {
    /// `birds[0]` is the camera; the flock is `birds[1..]`.
    pub birds: Vec<Bird>,
    pub world: World,
    /// Simulated seconds. Not the render clock.
    pub t: f32,
    /// Where the camera is looking: low-passed, so the flock may jink and the
    /// view may not.
    pub look: V3,
    /// The view's roll, low-passed harder still.
    pub view_roll: f32,
    /// Where the flock is, as the view aims at it. Kept for measurement.
    pub aim_at: V3,
    /// The camera's heading, low-passed over a couple of seconds. "Where it
    /// is going" for the purposes of aiming: the instantaneous heading has
    /// every correction it makes in it, and the view should not.
    drift: V3,
    /// Which flank the camera is riding, drifting between them over minutes.
    seat_phase: f32,
    rng: Rng,
    acc: Vec<(V3, f32)>,
    /// Flock centroid, mean heading and rms spread, without the camera.
    pub centre: V3,
    pub course: V3,
    pub spread: f32,
    /// How fast the flock as a whole is climbing, metres a second. The camera
    /// needs it: seated on where the flock *is*, it is always a second behind
    /// a dive.
    pub rise: f32,
    /// This step's shared surge, worked out once for every bird.
    surge: (f32, f32),
    /// How much room each clan likes, drawn once from the seed.
    clan_room: [f32; CLANS],
    /// Sum of positions and head count of each clan, recomputed every step.
    /// A clan-mate's pull does not depend on how far away it is, so this is
    /// one pass rather than another `n^2`.
    clans: [(V3, f32); CLANS],
}

/// The count a fresh flock is built with; [`Sim::resize`] follows the
/// parameter from there.
const BIRDS: usize = 80;

impl Sim {
    /// A flock already formed and already flying: the world is laid out, the
    /// birds are seeded into a loose, aligned ball, and fifteen seconds are
    /// flown before anyone looks, so the first frame is birds in flight and
    /// not a cloud condensing.
    pub fn new(seed: u64) -> Sim {
        let mut rng = Rng::new(seed ^ 0x62_6972_6473);
        let world = World::new(&mut rng, BLOBS);
        let course = {
            let a = rng.range(0.0, TAU);
            v3(a.cos(), 0.0, a.sin())
        };
        let cruise = 5.4;
        let mut clan_room = [1.0_f32; CLANS];
        for room in clan_room.iter_mut() {
            *room = rng.range(ROOM.0, ROOM.1);
        }
        let birds = (0..=BIRDS)
            .map(|_| {
                let off = v3(rng.range(-9.0, 9.0), rng.range(-4.0, 4.0), rng.range(-9.0, 9.0));
                let jitter = v3(rng.range(-0.8, 0.8), rng.range(-0.4, 0.4), rng.range(-0.8, 0.8));
                Bird::new(off, course.scale(cruise).add(jitter), &clan_room, &mut rng)
            })
            .collect();
        let mut sim = Sim {
            birds,
            world,
            t: 0.0,
            look: course,
            view_roll: 0.0,
            aim_at: course,
            drift: course,
            seat_phase: rng.range(0.0, TAU),
            rng,
            acc: Vec::new(),
            centre: V3::ZERO,
            course,
            spread: 8.0,
            rise: 0.0,
            surge: (0.0, 0.0),
            clan_room,
            clans: [(V3::ZERO, 0.0); CLANS],
        };
        // The camera is in no clan: it is in the flock's lists, but it is not
        // anybody's friend.
        sim.birds[0].clan = NO_CLAN;
        let tune = Tuning::default();
        for _ in 0..900 {
            sim.step(&tune, STEP);
        }
        sim.t = 0.0;
        sim
    }

    pub fn flock(&self) -> &[Bird] {
        &self.birds[1..]
    }

    pub fn camera(&self) -> Bird {
        self.birds[0]
    }

    /// Follow the `birds` parameter without restarting the flight: new birds
    /// join at the edge of the flock already up to speed, and leaving ones
    /// simply are not there any more.
    pub fn resize(&mut self, want: usize) {
        let want = want.max(1) + 1;
        while self.birds.len() > want {
            self.birds.pop();
        }
        while self.birds.len() < want {
            let off = v3(
                self.rng.range(-1.0, 1.0),
                self.rng.range(-1.0, 1.0),
                self.rng.range(-1.0, 1.0),
            )
            .unit_or(UP)
            .scale(self.spread);
            let rooms = self.clan_room;
            let bird = Bird::new(self.centre.add(off), self.course.scale(5.0), &rooms, &mut self.rng);
            self.birds.push(bird);
        }
    }

    /// One fixed timestep.
    pub fn step(&mut self, tune: &Tuning, dt: f32) {
        self.t += dt;
        let mut rng = std::mem::replace(&mut self.rng, Rng::new(0));
        self.world.step(self.t, dt, tune.wild, tune.lift, &mut rng);
        self.rng = rng;

        let n = self.birds.len();
        let inv = 1.0 / (n - 1).max(1) as f32;
        self.centre = self.birds[1..].iter().fold(V3::ZERO, |a, b| a.add(b.pos)).scale(inv);
        self.course = self.birds[1..]
            .iter()
            .fold(V3::ZERO, |a, b| a.add(b.vel))
            .unit_or(self.course);
        self.spread = (self.birds[1..].iter().map(|b| b.pos.sub(self.centre).len2()).sum::<f32>() * inv)
            .sqrt()
            .max(2.0);
        self.rise = self.birds[1..].iter().map(|b| b.vel.y).sum::<f32>() * inv;
        self.surge = self.world.surge_at(self.t);
        self.clans = [(V3::ZERO, 0.0); CLANS];
        for b in &self.birds[1..] {
            let clan = &mut self.clans[(b.clan as usize).min(CLANS - 1)];
            clan.0 = clan.0.add(b.pos);
            clan.1 += 1.0;
        }

        // Where the blobs are this instant, worked out once for all birds.
        let live = tune.blobs.min(self.world.blobs.len());
        let blobs: Vec<(V3, f32)> =
            self.world.blobs[..live].iter().map(|b| (b.at(self.t), b.radius)).collect();
        let reach = self.world.reach(tune);

        let mut acc = std::mem::take(&mut self.acc);
        acc.clear();
        acc.reserve(n);
        for i in 0..n {
            acc.push(self.steer(i, tune, &blobs, reach));
        }

        // How much harder than a bird the camera may turn this step. Holding
        // a seat is mostly a vertical problem: a flock that dives at four
        // metres a second leaves a camera with a bird's turn budget eight
        // metres above it within seconds, and the view - which must keep the
        // horizon - then has the flock below its feet and an empty panel.
        // Every zero-birds-in-frame sample in the long run was this.
        let seat_y = self.centre.y - tune.near * 0.26 + self.rise * SEAT_LEAD;
        let off = (self.birds[0].pos.y - seat_y).abs();
        let slack = 1.15 + (CAM_SLACK - 1.15) * (off / 5.0).clamp(0.0, 1.0);
        for (i, &(a, dodge)) in acc.iter().enumerate() {
            self.fly(i, a, tune, dt, slack, dodge);
        }
        self.acc = acc;

        self.aim(tune, dt);
    }

    /// Reynolds, plus the invisible geometry, plus - for `birds[0]` only - a
    /// seat in the flock.
    fn steer(&self, i: usize, tune: &Tuning, blobs: &[(V3, f32)], reach: f32) -> (V3, f32) {
        let me = self.birds[i];
        let camera = i == 0;
        let fwd = me.heading();

        // The camera's personal space is what `near` really means. A bird at
        // 1.5 m spans twenty-five LEDs and the panel is one wing; at six it is
        // a bird with a wingbeat and there is a flock behind it. A bird keeps
        // the room *it* likes, which is not what its neighbour likes.
        let sep_r =
            if camera { tune.separation.max(tune.near * 1.15) } else { tune.separation * me.room };

        // The seven nearest, whatever their distance: separation and alignment
        // are topological (see [`NEIGH`]). Cohesion stays metric, because that
        // is what holds a flock of fifty together as one body and card 168
        // paid for it - a flock that can only see seven birds splits and never
        // finds itself again.
        let mut near: [(f32, usize); NEIGH] = [(f32::INFINITY, usize::MAX); NEIGH];
        let (mut coh, mut n_coh) = (V3::ZERO, 0.0_f32);
        for (j, other) in self.birds.iter().enumerate() {
            if j == i {
                continue;
            }
            let d = other.pos.sub(me.pos);
            let dist2 = d.len2();
            if dist2 < near[NEIGH - 1].0 {
                let mut k = NEIGH - 1;
                while k > 0 && near[k - 1].0 > dist2 {
                    near[k] = near[k - 1];
                    k -= 1;
                }
                near[k] = (dist2, j);
            }
            if dist2 <= tune.cohesion * tune.cohesion {
                coh = coh.add(other.pos);
                n_coh += 1.0;
            }
        }
        let (mut sep, mut align) = (V3::ZERO, V3::ZERO);
        let mut n_align = 0.0_f32;
        for &(dist2, j) in &near {
            if j == usize::MAX {
                continue;
            }
            let other = self.birds[j];
            let d = other.pos.sub(me.pos);
            let dist = dist2.sqrt().max(0.05);
            // Friends fly closer. This, more than anything else here, is what
            // makes the gaps uneven: a clan holds together at two thirds of
            // the distance a stranger is kept at, so the flock is knots with
            // air between them.
            let r = if !camera && other.clan == me.clan { sep_r * CLAN_ROOM } else { sep_r };
            if dist < r {
                // **Soft, except close in.** The old rule fell off as 1/d from
                // the full separation radius, which is a wall: every bird
                // settles exactly where the wall stops pushing, and fifty
                // birds with the same wall settle into a lattice. This is a
                // squared falloff - nothing much until a bird is well inside
                // the room it likes, so two may pass close and a knot may stay
                // a knot - with a stiff core at about a wingspan so they still
                // never fly through one another.
                //
                // The camera keeps the old wall: its personal space is not a
                // preference, it is the difference between a bird and a wing
                // across the whole panel.
                let push = if camera {
                    r / dist - 1.0
                } else {
                    let u = 1.0 - dist / r;
                    1.7 * u * u + if dist < CORE { 2.6 * (CORE / dist - 1.0) } else { 0.0 }
                };
                sep = sep.sub(d.scale(push / dist));
            }
            align = align.add(other.vel);
            n_align += 1.0;
        }
        if n_align > 0.0 {
            align = align.scale(1.0 / n_align).sub(me.vel);
        }
        if n_coh > 0.0 {
            coh = coh.scale(1.0 / n_coh).sub(me.pos);
        }
        // Flocks are wider than they are tall. Birds will stack closer above
        // and below one another than they will fly side by side, so the
        // vertical half of the push is weaker - which is all "flattened" needs
        // to be, and it costs nothing.
        sep.y *= 0.55;

        let herd = if camera { 0.30 } else { 1.0 };
        // The camera holds its distance harder than a bird does: a bird in a
        // flock is happy at arm's length, a lens is not.
        let keep = if camera { 7.0 } else { 3.4 };
        let mut a = sep.scale(keep).add(align.scale(1.1 * herd)).add(coh.scale(0.28 * herd));
        // A weak wish to fly the flock's own course, on top of the seven
        // neighbours it can see. Topological alignment is what gives the flock
        // its local structure, but on its own it lets the headings inside the
        // flock spread far enough that a shared shove - a blob, the world's
        // edge - is clipped differently by every bird's turn limit and takes
        // the flock apart. Measured on seed 404: fifty-five of fifty-five
        // birds more than twenty metres out, for a minute. This is the
        // cheapest thing that holds a flock of fifty together as one body.
        if !camera {
            a = a.add(self.course.scale(me.speed()).sub(me.vel).scale(0.22));
        }

        // The clan, wherever it has got to: a gentle, *non*-local pull, which
        // is the one thing here that can make a knot travel as a knot rather
        // than dissolve into the flock's average.
        if !camera {
            let (sum, n) = self.clans[me.clan as usize];
            if n > 1.5 {
                let mid = sum.sub(me.pos).scale(1.0 / (n - 1.0)).sub(me.pos);
                let d = mid.len();
                // A friendship, not a beacon. Left unbounded this is a
                // runaway: a clan that drifts a little out of the flock pulls
                // the rest of itself after it, and the measurement caught
                // eleven birds seventy metres out. Beyond [`CLAN_REACH`] a
                // clan is not a knot any more and its members are simply
                // birds in a flock again.
                if d < CLAN_REACH {
                    a = a.add(mid.scale(0.50 * (1.0 - d / CLAN_REACH)));
                }
            }
        }

        // Invisible geometry. The push is *across* the flight path, not back
        // along it: a bird goes round a thing, it does not stop in front of it.
        let mut dodge = 0.0_f32;
        for (centre, radius) in blobs {
            let away = me.pos.sub(*centre);
            let dist = away.len().max(0.01);
            let clear = dist - radius;
            if clear > reach {
                continue;
            }
            dodge = dodge.max(urgency_of(clear, reach));
            // Linear in how close it is, not squared. A stiff repulsion is
            // fifteen times cohesion at the surface, and it does not push the
            // flock round the blob - it shoves the near birds one way and the
            // far ones another and takes the flock apart. Softer, and
            // starting much further out, curves the whole flock as one body.
            let urgency = urgency_of(clear, reach);
            let out = away.scale(1.0 / dist);
            // Which way *round* - and this is taken across the flock's own
            // course, not the bird's heading. Once alignment went topological
            // the headings inside the flock spread out, and a sideways push
            // worked out per bird then sent neighbours round opposite sides of
            // the same blob: the long run caught forty-five of fifty-five
            // birds more than twenty metres from the middle. Off one shared
            // course the whole flock curves as one body, which is what card
            // 168 found the first time and what this quietly undid.
            // Head on, `across` is nothing; lean on the bird's own up so the
            // choice is made rather than left to rounding.
            let basis = if camera { fwd } else { self.course };
            let side = out.across(basis).unit_or(basis.cross(UP).unit_or(UP));
            a = a.add(side.scale(9.0 * urgency)).add(out.scale(3.5 * urgency));
        }

        // Come home. Local cohesion only reaches `tune.cohesion` metres, so a
        // flock pulled further apart than that stops being able to see itself
        // and never comes back: over ten minutes it splits, and the camera
        // spends the rest of the run chasing one half of it. This does nothing
        // at all inside `FLOCK_RADIUS`, and it is most of what makes the
        // flight hold up over three seeds rather than the one it was tuned on.
        //
        // But getting round the thing in front of you comes first: pulling a
        // bird home *while* it is dodging is how a flock gets squeezed into a
        // blob, because the two forces cancel and the avoidance loses.
        let home = self.centre.sub(me.pos);
        let stray = home.len() - FLOCK_RADIUS;
        if stray > 0.0 {
            // ...but *weakened* rather than switched off while dodging, which
            // is what it used to be. A blob's reach is nearly forty metres, so
            // "dodging" is most of the time, and a flock with no gather for
            // most of its life is a flock that stays scattered once anything
            // scatters it: the long run caught fifty-five of fifty-five birds
            // out of the flock for a minute at a time.
            let ease = if dodge < 0.05 { 1.0 } else { 0.40 };
            a = a.add(home.unit_or(V3::ZERO).scale((stray * 0.25).min(4.0) * ease));
        }

        // Floor, ceiling and the soft edge of the world, all the same shape:
        // a push that grows as the margin closes.
        let head = (CEILING - me.pos.y) / MARGIN;
        let feet = (me.pos.y - FLOOR) / MARGIN;
        a.y += 6.0 * ((1.0 - feet).max(0.0).powi(2) - (1.0 - head).max(0.0).powi(2));
        // Where the standing spring to one cruise altitude used to be. It was
        // `(3 - y) * 0.05`, and it is the reason the old flight was a wobble
        // about a single height: every climb was paid back within seconds by a
        // force that only ever pointed at 3 m. What replaces it pulls towards
        // the **flock's own** altitude, not the world's - so the flock stays
        // one flat body (flocks are wider than they are tall) and may take
        // that body anywhere it likes vertically.
        a.y += (self.centre.y - me.pos.y) * 0.09;
        // ...and one very weak string on the flock as a whole, so that over
        // ten minutes it does not random-walk down onto the floor and stay
        // there. Measured without it: the centroid spent the second half of
        // the run between -60 and -67 m, with half the world's height above it
        // and unused. A tenth of the weakest surge, and it acts over minutes.
        a.y -= self.centre.y * 0.012;

        let flat = v3(me.pos.x, 0.0, me.pos.z);
        let out = flat.len();
        if out > BOUND * 0.70 {
            let over = (out - BOUND * 0.70) / (BOUND * 0.30);
            a = a.sub(flat.scale(over * over * 9.0 / out.max(0.01)));
        }

        if camera {
            // The one thing a camera wants that a bird does not: to sit at the
            // trailing edge or on a flank, where the flock is in front of it.
            // In the middle of a flock a camera sees one bird's tail.
            let side = self.course.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
            // Trailing along the flock's *ground track*, not its full course.
            // Following a diving flock down its own vector parks the camera
            // above it - and a view that must hold the horizon then has the
            // flock below its feet. Behind and a little under, always.
            let track = v3(self.course.x, 0.0, self.course.z).unit_or(side.cross(UP));
            let seat = self
                .centre
                .sub(track.scale(self.spread * 1.5 + tune.near))
                .add(side.scale(self.spread * 0.40 * self.seat_phase.sin()))
                // *Below* the flock, so the view rides a little nose-up and
                // the horizon sits in the lower third with the birds against
                // the sky. Seated level or above, the view spends its whole
                // life pinned at the bottom of `level`'s band, the horizon is
                // in the top quarter and two thirds of the panel is dark
                // ground - which is the picture upside down.
                .sub(UP.scale(tune.near * 0.26))
                // ...and **ahead of the flock's vertical motion**, not merely
                // under where it is now. Seated on a position alone, a camera
                // is a second behind every dive, which is a second of the
                // flock leaving the top of the frame. Looking at how fast they
                // are going down, and going down with them, is the difference
                // between a dive you watch and a dive you lose.
                .add(UP.scale(self.rise * SEAT_LEAD));
            // The camera keeps its seat force even while dodging: its reach
            // is nearly thirty metres, so "near a blob" is most of the time,
            // and dropping the seat there costs it the flock.
            a = a.add(seat.sub(me.pos).clamp_len(30.0).scale(0.55));
            // Fly the flock's course, not just the neighbours it happens to
            // have. Without this the camera cuts the corner when the flock
            // turns, overshoots, and spends the next half minute coming back -
            // which is where every empty frame came from.
            // Matching the flock's course, gently. Pushed hard - it was 0.9 -
            // the camera darts about correcting itself, and since it sits only
            // twelve metres away, its own darting swings the bearing to the
            // flock faster than the flock ever wheels. The view then pans at
            // the camera's fidgeting rather than at the flight.
            a = a.add(self.course.scale(me.speed()).sub(me.vel).scale(0.65));
        } else {
            // Something over there, once in a while.
            let d = self.world.interest.at.sub(me.pos);
            if d.len() > 12.0 {
                a = a.add(d.unit_or(V3::ZERO).scale(0.9 * self.world.interest.pull));
            }
            // Its own restlessness: two slow oscillators it was born with, one
            // sideways and one vertical. Every bird is always leaning somewhere
            // of its own, and no two of them agree, so the flock's shape
            // breathes instead of setting. This is the cheapest of everything
            // here and, at the panel's size, a good deal of what "not a
            // lattice" looks like in motion.
            let side = fwd.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
            let stir = REST * (0.25 + 0.75 * tune.wild.clamp(0.0, 1.0));
            a = a
                .add(side.scale(stir * (self.t * me.rest[0] + me.rest[1]).sin()))
                .add(UP.scale(stir * 0.8 * (self.t * me.rest[2] + me.rest[3]).sin()));

            // And the shared surge, if one is running. `trim` is already this
            // bird's own small number, so the flock leans into a dive at
            // slightly different rates and arrives as a flock rather than as a
            // plate.
            let (up, swirl) = self.surge;
            if up != 0.0 || swirl != 0.0 {
                a.y += up * me.trim;
                a = a.add(side.scale(swirl * me.trim));
            }
        }
        (a, dodge)
    }

    /// Integrate one bird: speed band, turn-rate limit, bank, wingbeat.
    fn fly(&mut self, i: usize, a: V3, tune: &Tuning, dt: f32, slack: f32, dodge: f32) {
        let camera = i == 0;
        let me = &mut self.birds[i];
        let fwd = me.heading();
        let speed = me.speed().max(0.1);

        // A turn is the part of the acceleration across the flight path, and
        // limiting it *is* the turn-rate limit: |a_across| = omega * v.
        //
        // The camera is *more* agile than a bird, not less. It has to be: it
        // is holding a station on a flock that is wheeling, and a camera whose
        // turn radius is larger than the flock's circle is thrown off it every
        // time. Every empty and every lurching frame in the long run came from
        // making this smaller, not larger. Smoothness belongs in where it
        // *looks* - `aim`, and the ceilings above - never in how it flies.
        // A bird avoiding a collision turns harder than it cruises, as a real
        // one does. Without this the avoidance force is simply thrown away:
        // whatever it asks for, the cruise limit clips it to about two metres
        // a second squared, and a turn-rate-limited bird at five metres a
        // second physically cannot get round anything inside thirteen metres.
        let turn = if camera { tune.turn * slack } else { tune.turn * (1.0 + DODGE_TURN * dodge) };

        // The climb limit, as a **force** and not as a clamp on the velocity.
        //
        // It used to snap the velocity flat once the flight-path angle went
        // past `CLIMB`, which rotates the heading by however much it takes -
        // a turn nobody authorised and nothing measured. The long run caught
        // birds swinging at x1.03 of their limit and this line was the
        // offender every time. As a strong restoring acceleration it goes
        // through the same turn-rate clamp as everything else, so the limit on
        // how fast anything may change direction is the *only* one, and the
        // climb limit becomes what it is in a real bird: the angle past which
        // it runs out of lift, approached and not snapped to.
        let sin_g = me.vel.y / speed;
        let lift = tune.lift.clamp(0.0, 1.0);
        // The camera gets a quarter more of both, for the same reason it turns
        // harder: it is *following* a dive, and a chaser held to the same
        // limit as the thing it chases arrives late every time.
        let room = if camera { 1.25 } else { 1.0 };
        let up_max = (CLIMB_UP.0 + (CLIMB_UP.1 - CLIMB_UP.0) * lift) * room;
        let down_max = (CLIMB_DOWN.0 + (CLIMB_DOWN.1 - CLIMB_DOWN.0) * lift) * room;
        let over = sin_g.clamp(-down_max, up_max) - sin_g;
        let a = a.add(UP.scale(over * PITCH_SPRING));

        // Height for speed. A diving bird gains and a climbing one pays, which
        // is why a dive reads as a dive from inside the flock: the birds are
        // going somewhere faster, not merely lower.
        let along = (a.dot(fwd) - TRADE * sin_g * if camera { 0.35 } else { 1.0 }).clamp(-3.5, 3.5);
        let across = a.across(fwd).clamp_len(turn * speed);
        let a = fwd.scale(along).add(across);

        me.vel = me.vel.add(a.scale(dt));
        let s = me.speed();
        // The camera's airspeed envelope is wider than a bird's at both ends,
        // and the bottom end is the one that matters: a camera that cannot fly
        // slower than the flock can never *drop back* into its seat, so once
        // it drifts ahead it spends twenty seconds with the flock behind it
        // and the panel empty. That was every empty frame in the long run.
        // A bird's own band, shifted by its `pep`: some of them are simply
        // quicker than others, which keeps the flock's shape stirring instead
        // of frozen even when nothing is steering it.
        let (lo, hi) = if camera {
            (tune.speed.0 * 0.5, tune.speed.1 * 1.3)
        } else {
            (tune.speed.0 * me.pep, tune.speed.1 * me.pep)
        };
        if s > 1e-4 {
            me.vel = me.vel.scale(s.clamp(lo, hi) / s);
        }
        me.pos = me.pos.add(me.vel.scale(dt));

        // Bank into the turn, as a bird does: roll = atan(lateral / g). Right
        // wing up is positive, so a turn to the right is a negative roll.
        let right = fwd.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
        let want = -(across.dot(right) / G).atan();
        let tau = if camera { 0.55 } else { 0.35 };
        me.roll += (want - me.roll) * (1.0 - (-dt / tau).exp());

        // And the lean it is **drawn** at, which is the same turn told louder
        // (card 124). A plain multiplier would be enough for the gentle
        // turns this flight actually flies, but `calm` goes down to 0 and the
        // roll with it goes to twenty degrees, so it is bent over towards
        // LEAN_MAX instead: six times as much lean where there is hardly any,
        // and a hard turn arriving at a bird's own limit rather than past it.
        let gain = 1.0 + (LEAN_GAIN - 1.0) * tune.lean.max(0.0);
        let want = LEAN_MAX * (gain * me.roll / LEAN_MAX).tanh();
        // Quicker going over than coming back: a bird rolls into a turn in one
        // movement and levels out of it in its own time. Symmetrical, the lean
        // reads as a dial being turned; asymmetrical, it reads as a decision.
        let tau = if want.abs() > me.lean.abs() { LEAN_IN } else { LEAN_OUT };
        me.lean += (want - me.lean) * (1.0 - (-dt / tau).exp());

        // Beating: harder when climbing, a glide when coming down. The rate
        // rises a little with airspeed, so a flock that is working looks like
        // it is working.
        let want_glide = crate::color::smoothstep(0.5, -1.1, me.vel.y);
        me.glide += (want_glide - me.glide) * (1.0 - (-dt / 1.3).exp());
        // Climbing is work: the wings beat harder going up, and the glide
        // above already holds them still coming down. That pair is the whole
        // of what says "vertical" when there is no sky behind the birds to say
        // it - which is exactly the case with the backdrop turned off.
        let climbing = (me.vel.y / (speed * 0.45)).clamp(0.0, 1.0);
        let rate = tune.beat
            * me.trim
            * (0.7 + 0.55 * speed / 5.5)
            * (1.0 - 0.5 * me.glide)
            * (1.0 + 0.55 * climbing);
        me.phase = (me.phase + TAU * rate * dt).rem_euclid(TAU);
    }

    /// Where the camera looks, and how far over it leans.
    ///
    /// A blend of where it is going and where the flock is, low-passed hard.
    /// The horizon is held inside a band, which is the gimbal a bird does not
    /// have and the picture does need: without it the view pitches to follow a
    /// climb and there is nothing in frame to say which way is up.
    fn aim(&mut self, tune: &Tuning, dt: f32) {
        self.seat_phase += dt * 0.018;
        let me = self.birds[0];
        // How far the horizon is allowed off the middle of the panel. Card
        // 168 pinned this at 11 degrees and never measured the view's pitch,
        // because there was nothing vertical to show; `lift` opens it, and
        // what it buys is the horizon sliding up the panel through a dive.
        let tilt = VIEW_TILT.0 + (VIEW_TILT.1 - VIEW_TILT.0) * tune.lift.clamp(0.0, 1.0);
        self.drift = self.drift.turn_towards(me.heading(), 1.0 - (-dt / 2.0_f32).exp(), 1.0);
        let fwd = level(self.drift, tilt);

        // Where the nearby flock is: the mean direction to the birds, the
        // nearer ones counting for more.
        //
        // This used to be weighted towards whatever was ahead of the camera as
        // well, which coupled it to the camera's own heading - so the aim point
        // moved whenever the camera manoeuvred, even with the flock perfectly
        // still, and the view was dragged along at up to forty degrees a
        // second. Distance only, and it is a property of the flock alone.
        // The weighted mean *position* of the flock, and then the direction to
        // it - not the mean of the directions. Averaging unit vectors falls
        // apart when the birds are spread around you: the horizontal parts
        // cancel, what is left is short, and its bearing spins. The long run
        // caught exactly that as a two-second burst where the aim point
        // rotated at eighty degrees a second with the flock sitting still.
        let mut focus = V3::ZERO;
        let mut weight = 0.0;
        for b in &self.birds[1..] {
            let w = 1.0 / (1.0 + b.pos.sub(me.pos).len2() / 500.0);
            focus = focus.add(b.pos.scale(w));
            weight += w;
        }
        let focus = if weight > 1e-4 { focus.scale(1.0 / weight).sub(me.pos) } else { V3::ZERO };
        // If nothing is ahead at all, fall back to the flock itself rather
        // than to the heading: the heading is exactly what has gone wrong in
        // that case.
        let home = self.centre.sub(me.pos).unit_or(fwd);
        // Where the flock really is, and no longer where the composition
        // would prefer it to be.
        //
        // This used to be levelled into the horizon's own band, which is a
        // lie the leash below then believed: with the flock 35 degrees above
        // the camera through a dive's recovery, the aim point said 14, the
        // leash was satisfied, and ten of fifty-five birds were on the panel.
        // The only clamp left is the degenerate one - an azimuth taken from a
        // nearly vertical vector is rounding noise, and the long run once
        // caught that as a 399 deg/s swing - so 33 degrees, far outside
        // anything the composition cares about.
        let focus = level(focus.unit_or(home), 0.55);

        // Where it is going - levelled, because the camera's own attitude
        // through a dive is far steeper than the picture wants - blended
        // towards where the flock is, and then put on a leash. Blending alone is not enough: manoeuvring into its seat
        // the camera's heading can be seventy degrees off the flock, and a
        // blend of that is still outside a 38-degree half-field. The leash
        // says the flock's middle is never more than LEASH off the view axis,
        // which is the promise "the birds stay in frame as it turns with them"
        // written as a number.
        let want = fwd.lerp(level(focus, tilt), 0.55).unit_or(focus);

        // Low-passed, and then rate-limited on top: the low pass makes it
        // unhurried, the ceiling makes it impossible for any one moment to
        // throw the picture about.
        self.aim_at = focus;
        let was = self.look;
        self.look = self.look.turn_towards(want, 1.0 - (-dt / 0.9_f32).exp(), VIEW_YAW * dt);

        // Then three things in this order, and the order *is* the priority:
        // composition, the birds, and how fast the picture may move.
        //
        // First, hold the horizon in the band the panel can show it in. Card
        // 168 held this last, because with a flight that was horizontal
        // anyway the only thing that could push the horizon off the panel was
        // the leash overreacting. With real climbs and dives in the flight
        // that is no longer true, and holding it last means losing the flock
        // out of the top of the frame to keep a horizon - which is the wrong
        // way round. **Birds first.**
        self.look = level(self.look, tilt);
        // Second, the leash, and it may push the horizon out of that band:
        // the flock's middle is never more than LEASH off the view axis
        // sideways nor LEASH_UP above or below it. Vertically it is tighter,
        // because the panel is. Putting this on the *target* instead was not
        // enough - a low pass that is 50 degrees behind its target still shows
        // an empty panel.
        let off = elev_of(focus) - elev_of(self.look);
        if off.abs() > LEASH_UP {
            self.look = with_elev(self.look, elev_of(focus) - off.signum() * LEASH_UP);
        }
        if angle_between(self.look, focus) > LEASH {
            self.look = focus.turn_towards(self.look, 1.0, LEASH);
        }
        // And an outer wall on the whole argument: however much the leash
        // wants to follow the flock up or down, the view never points more
        // than [`TILT_MAX`] off level, so there is always a horizon somewhere
        // on the panel. Without it a steep recovery took the view 26 degrees
        // nose-up, which on a panel 21 degrees tall is no horizon at all.
        self.look = level(self.look, TILT_MAX);
        // And last of all, the ceiling that nothing may argue with. The leash
        // is best effort - it is about composition - but how fast the picture
        // is allowed to move is about whether it can be watched at all, so it
        // wins. If the two ever disagree the flock drifts towards the edge of
        // the frame for a second, which is a far better failure than a pan
        // nobody can follow.
        self.look = was.turn_towards(self.look, 1.0, HARD_YAW * dt);

        let want_roll = (me.roll * tune.bank).clamp(-VIEW_ROLL, VIEW_ROLL);
        let ease = (want_roll - self.view_roll) * (1.0 - (-dt / 1.4_f32).exp());
        self.view_roll += ease.clamp(-ROLL_RATE * dt, ROLL_RATE * dt);
    }

    /// Right, up, forward for the view: the low-passed look direction, rolled.
    ///
    pub fn view(&self) -> (V3, V3, V3) {
        let fwd = self.look;
        let right = fwd.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
        let up = right.cross(fwd);
        let (s, c) = self.view_roll.sin_cos();
        (right.scale(c).add(up.scale(s)), up.scale(c).sub(right.scale(s)), fwd)
    }

    /// How far the nearest bird is, in metres. For the studio's "now playing".
    pub fn nearest(&self) -> f32 {
        let me = self.birds[0].pos;
        self.birds[1..]
            .iter()
            .map(|b| b.pos.sub(me).len())
            .fold(f32::INFINITY, f32::min)
    }

    /// Clearance between the nearest bird and the nearest blob's surface,
    /// metres. Negative would mean a bird inside the invisible geometry.
    #[cfg(test)]
    pub fn clearance(&self, live: usize, camera: bool) -> f32 {
        let mut worst = f32::INFINITY;
        let who = if camera { &self.birds[..1] } else { &self.birds[1..] };
        for blob in &self.world.blobs[..live.min(self.world.blobs.len())] {
            let centre = blob.at(self.t);
            for b in who {
                worst = worst.min(b.pos.sub(centre).len() - blob.radius);
            }
        }
        worst
    }
}

/// The angle between two directions, radians. Used to measure how fast the
/// view is turning.
pub fn angle_between(a: V3, b: V3) -> f32 {
    a.dot(b).clamp(-1.0, 1.0).acos()
}

/// Bearing of a direction, radians, for measuring yaw rate.
#[cfg(test)]
pub fn bearing(d: V3) -> f32 {
    d.x.atan2(d.z)
}

/// The smaller of the two ways round, radians.
#[cfg(test)]
pub fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI
}

/// A direction with its elevation held inside the band the panel can show the
/// horizon in. `band` is a sine, not an angle.
fn level(d: V3, band: f32) -> V3 {
    let lift = d.y.clamp(-band, band);
    let flat = v3(d.x, 0.0, d.z).unit_or(v3(0.0, 0.0, 1.0));
    flat.scale((1.0 - lift * lift).sqrt()).add(UP.scale(lift))
}

/// How far above the horizon a direction points, radians.
fn elev_of(d: V3) -> f32 {
    (d.y / d.len().max(1e-6)).clamp(-1.0, 1.0).asin()
}

/// The same bearing, at a different elevation.
fn with_elev(d: V3, e: f32) -> V3 {
    let (s, c) = e.sin_cos();
    v3(d.x, 0.0, d.z).unit_or(v3(0.0, 0.0, 1.0)).scale(c).add(UP.scale(s))
}

/// How hard something at `clear` metres of clearance is being avoided, given
/// that avoidance begins at `reach`.
fn urgency_of(clear: f32, reach: f32) -> f32 {
    (1.0 - clear / reach).clamp(0.0, 1.6)
}
