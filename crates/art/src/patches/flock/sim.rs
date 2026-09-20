//! The flight: Reynolds' boids in 3D, the invisible world they steer round,
//! and the camera that is one of them.
//!
//! Nothing here knows about pixels. It is stepped on a **fixed 1/60 s
//! timestep** ([`STEP`]) so the flight is the same whatever rate the frames are
//! drawn at, and every number in it is a metre, a metre per second or a radian
//! per second, so the limits can be read and checked as what they are.
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
//!   ground track and a little below it;
//! - its **speed band is wider at both ends** (0.5x to 1.3x). The bottom end
//!   is the one that matters: a camera that cannot fly slower than the flock
//!   can never drop back once it has drifted ahead;
//! - it may **turn harder** - 1.15x a bird's, and up to 2.75x while its
//!   altitude is off the flock's. A camera whose turn radius is larger than
//!   the flock's circle is thrown off it every time they wheel;
//! - it keeps more **personal space** (`near * 1.15`) and pushes harder to
//!   keep it, because a bird at arm's length fills the panel with one wing.
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
pub const BOUND: f32 = 100.0;
pub const FLOOR: f32 = -24.0;
pub const CEILING: f32 = 28.0;
/// How deep into the floor or ceiling the push reaches. Wide, because a bird
/// that may only turn at its limit needs the room to do it in.
const MARGIN: f32 = 13.0;

/// Ceilings on the view itself, whatever the flock does: radians a second it
/// may swing, radians a second it may roll, and how far over it may lean.
/// These are the numbers that decide whether this is calm or nauseous, so
/// they are named and they are hard.
const VIEW_YAW: f32 = 0.35;
const ROLL_RATE: f32 = 0.10;
const VIEW_ROLL: f32 = 0.30;
/// How far off the view's axis the flock's middle is ever allowed to get.
/// The panel is 38 degrees from the middle to the side edge and 21 to the top,
/// so 16 keeps it in shot with room for the birds around it.
const LEASH: f32 = 0.28;
/// The view's absolute angular rate ceiling, radians a second. Unlike
/// [`VIEW_YAW`] - which is what the smoothing aims for - nothing may exceed
/// this, not even the leash.
const HARD_YAW: f32 = 0.45;
/// Steepest climb or dive, as a sine of the flight-path angle. 0.42 is 25
/// degrees.
const CLIMB: f32 = 0.42;

/// How many drifting blobs the world is laid out with. They are all built
/// from the seed and the `terrain` parameter says how many of them are real
/// this frame, so turning it up and down does not re-roll the world.
pub const BLOBS: usize = 5;

/// How far from its middle a flock is allowed to stray before it is gathered
/// back. Nothing at all happens inside it.
const FLOCK_RADIUS: f32 = 14.0;

/// How much of its cruise turn rate a bird may add while avoiding something,
/// at full urgency.
pub const DODGE_TURN: f32 = 2.2;

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
    pub separation: f32,
    pub alignment: f32,
    pub cohesion: f32,
    pub speed: (f32, f32),
    /// Largest angular rate a bird may fly at, radians per second.
    pub turn: f32,
    /// How close the camera rides; sets its seat and its personal space.
    pub near: f32,
    /// How much of a real bird's bank angle the view is allowed to take.
    pub bank: f32,
    /// Wingbeats a second at cruise.
    pub beat: f32,
    /// How many of the world's blobs are in play, 0..=[`BLOBS`].
    pub blobs: usize,
}

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
            separation: 3.6,
            alignment: 12.0,
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
            beat,
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
    pub roll: f32,
    /// Wingbeat phase, radians.
    pub phase: f32,
    /// 0 beating, 1 gliding. Low-passed, so it eases in and out.
    pub glide: f32,
    /// Fixed per-bird variation, so no two beat quite alike.
    pub trim: f32,
}

impl Bird {
    fn new(pos: V3, vel: V3, rng: &mut Rng) -> Bird {
        Bird {
            pos,
            vel,
            roll: 0.0,
            phase: rng.range(0.0, TAU),
            glide: 0.0,
            trim: rng.range(0.86, 1.16),
        }
    }

    pub fn speed(self) -> f32 {
        self.vel.len()
    }

    pub fn heading(self) -> V3 {
        self.vel.unit_or(v3(0.0, 0.0, 1.0))
    }

    /// Right, up, forward for this bird, with its roll applied. Drawing a bird
    /// and pointing a camera want exactly the same three vectors.
    pub fn frame(self) -> (V3, V3, V3) {
        let fwd = self.heading();
        let right = fwd.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
        let up = right.cross(fwd);
        let (s, c) = self.roll.sin_cos();
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
}

/// The invisible geometry. Laid out from the seed; the blobs drift on periods
/// that share no common multiple, so the space the flock moves through is
/// never the same twice.
#[derive(Clone, Debug)]
pub struct World {
    pub blobs: Vec<Blob>,
    interest: Interest,
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
            interest: Interest { at: V3::ZERO, to: V3::ZERO, next: 0.0, pull: 0.0 },
        }
    }

    /// How far outside a blob a bird starts to steer round it. Wide enough
    /// that a bird flying at the middle of one at full speed, turning no
    /// harder than its limit, is round it with room to spare.
    fn reach(&self, tune: &Tuning) -> f32 {
        7.0 + 1.4 * tune.speed.1 / tune.turn.max(0.15)
    }

    fn step(&mut self, t: f32, dt: f32, rng: &mut Rng) {
        if t >= self.interest.next {
            let a = rng.range(0.0, TAU);
            let r = rng.range(10.0, BOUND * 0.7);
            self.interest.to = v3(r * a.cos(), rng.range(-6.0, 10.0), r * a.sin());
            self.interest.next = t + rng.range(40.0, 110.0);
            self.interest.pull = rng.range(0.0, 1.0);
        }
        // It moves to where it is going over about half a minute, so the flock
        // is drawn rather than yanked.
        self.interest.at = self.interest.at.lerp(self.interest.to, 1.0 - (-dt / 16.0).exp());
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
        let birds = (0..=BIRDS)
            .map(|_| {
                let off = v3(rng.range(-9.0, 9.0), rng.range(-4.0, 4.0), rng.range(-9.0, 9.0));
                let jitter = v3(rng.range(-0.8, 0.8), rng.range(-0.4, 0.4), rng.range(-0.8, 0.8));
                Bird::new(off, course.scale(cruise).add(jitter), &mut rng)
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
        };
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
            let bird = Bird::new(self.centre.add(off), self.course.scale(5.0), &mut self.rng);
            self.birds.push(bird);
        }
    }

    /// One fixed timestep.
    pub fn step(&mut self, tune: &Tuning, dt: f32) {
        self.t += dt;
        let mut rng = std::mem::replace(&mut self.rng, Rng::new(0));
        self.world.step(self.t, dt, &mut rng);
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
        let off = (self.birds[0].pos.y - (self.centre.y - tune.near * 0.45)).abs();
        let slack = 1.15 + 1.6 * (off / 4.0).clamp(0.0, 1.0);
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
        // a bird with a wingbeat and there is a flock behind it.
        let sep_r = if camera { tune.separation.max(tune.near * 1.15) } else { tune.separation };
        let (mut sep, mut align, mut coh) = (V3::ZERO, V3::ZERO, V3::ZERO);
        let (mut n_align, mut n_coh) = (0.0_f32, 0.0_f32);
        for (j, other) in self.birds.iter().enumerate() {
            if j == i {
                continue;
            }
            let d = other.pos.sub(me.pos);
            let dist2 = d.len2();
            if dist2 > tune.cohesion * tune.cohesion {
                continue;
            }
            let dist = dist2.sqrt().max(0.05);
            if dist < sep_r {
                // Falls off as 1/d, so a near miss pushes hard and a far one
                // barely at all.
                sep = sep.sub(d.scale((sep_r / dist - 1.0) / dist));
            }
            if dist < tune.alignment {
                align = align.add(other.vel);
                n_align += 1.0;
            }
            coh = coh.add(other.pos);
            n_coh += 1.0;
        }
        if n_align > 0.0 {
            align = align.scale(1.0 / n_align).sub(me.vel);
        }
        if n_coh > 0.0 {
            coh = coh.scale(1.0 / n_coh).sub(me.pos);
        }

        let herd = if camera { 0.30 } else { 1.0 };
        // The camera holds its distance harder than a bird does: a bird in a
        // flock is happy at arm's length, a lens is not.
        let keep = if camera { 7.0 } else { 3.4 };
        let mut a = sep.scale(keep).add(align.scale(1.1 * herd)).add(coh.scale(0.28 * herd));

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
            // Head on, `across` is nothing; lean on the bird's own up so the
            // choice is made rather than left to rounding.
            let side = out.across(fwd).unit_or(fwd.cross(UP).unit_or(UP));
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
        if stray > 0.0 && dodge < 0.05 {
            a = a.add(home.unit_or(V3::ZERO).scale((stray * 0.25).min(4.0)));
        }

        // Floor, ceiling and the soft edge of the world, all the same shape:
        // a push that grows as the margin closes.
        let head = (CEILING - me.pos.y) / MARGIN;
        let feet = (me.pos.y - FLOOR) / MARGIN;
        a.y += 6.0 * ((1.0 - feet).max(0.0).powi(2) - (1.0 - head).max(0.0).powi(2));
        // and a standing wish to be near the cruising band, so the limits are
        // something the flock rarely reaches rather than something it rides
        a.y += (3.0 - me.pos.y) * 0.05;

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
                .sub(UP.scale(tune.near * 0.45));
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
        let along = a.dot(fwd).clamp(-3.5, 3.5);
        let across = a.across(fwd).clamp_len(turn * speed);
        let a = fwd.scale(along).add(across);

        me.vel = me.vel.add(a.scale(dt));
        let s = me.speed();
        // The camera's airspeed envelope is wider than a bird's at both ends,
        // and the bottom end is the one that matters: a camera that cannot fly
        // slower than the flock can never *drop back* into its seat, so once
        // it drifts ahead it spends twenty seconds with the flock behind it
        // and the panel empty. That was every empty frame in the long run.
        let (lo, hi) = if camera { (tune.speed.0 * 0.5, tune.speed.1 * 1.3) } else { tune.speed };
        if s > 1e-4 {
            me.vel = me.vel.scale(s.clamp(lo, hi) / s);
        }
        // Nothing here climbs or dives more steeply than CLIMB. Birds in
        // cruise do not, it is most of what makes the flight read as calm
        // rather than as aerobatics, and it is what makes the camera's seat
        // possible at all: a flock that goes up at sixty degrees leaves a
        // camera that must hold the horizon staring at empty sky.
        let s = me.vel.len();
        let lift = s * CLIMB;
        if me.vel.y.abs() > lift {
            let flat = v3(me.vel.x, 0.0, me.vel.z);
            let want = (s * s - lift * lift).max(0.0).sqrt();
            me.vel = flat.unit_or(v3(0.0, 0.0, 1.0)).scale(want).add(UP.scale(lift * me.vel.y.signum()));
        }
        me.pos = me.pos.add(me.vel.scale(dt));

        // Bank into the turn, as a bird does: roll = atan(lateral / g). Right
        // wing up is positive, so a turn to the right is a negative roll.
        let right = fwd.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
        let want = -(across.dot(right) / G).atan();
        let tau = if camera { 0.55 } else { 0.35 };
        me.roll += (want - me.roll) * (1.0 - (-dt / tau).exp());

        // Beating: harder when climbing, a glide when coming down. The rate
        // rises a little with airspeed, so a flock that is working looks like
        // it is working.
        let want_glide = crate::color::smoothstep(0.5, -1.1, me.vel.y);
        me.glide += (want_glide - me.glide) * (1.0 - (-dt / 1.3).exp());
        let rate = tune.beat * me.trim * (0.7 + 0.55 * speed / 5.5) * (1.0 - 0.5 * me.glide);
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
        self.drift = self.drift.turn_towards(me.heading(), 1.0 - (-dt / 2.0_f32).exp(), 1.0);
        let fwd = level(self.drift);

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
        // Levelled here, before anything is aimed at it. `level` used to be
        // applied only at the very end, which left one way for the picture to
        // snap: a focus pointing steeply up or down makes a levelled vector
        // out of a horizontal part that is nearly nothing, and its azimuth is
        // then whatever the rounding says. The long run caught it as a 399
        // deg/s swing. Levelling the aim point makes that state unreachable.
        let focus = level(focus.unit_or(home));

        // Where it is going, blended towards where the flock is - and then put
        // on a leash. Blending alone is not enough: manoeuvring into its seat
        // the camera's heading can be seventy degrees off the flock, and a
        // blend of that is still outside a 38-degree half-field. The leash
        // says the flock's middle is never more than LEASH off the view axis,
        // which is the promise "the birds stay in frame as it turns with them"
        // written as a number.
        let want = fwd.lerp(focus, 0.55).unit_or(focus);

        // Low-passed, and then rate-limited on top: the low pass makes it
        // unhurried, the ceiling makes it impossible for any one moment to
        // throw the picture about.
        self.aim_at = focus;
        let was = self.look;
        self.look = self.look.turn_towards(want, 1.0 - (-dt / 0.9_f32).exp(), VIEW_YAW * dt);
        // And then the leash, which is the one thing that is not negotiable:
        // whatever the smoothing would rather do, the flock's middle is never
        // more than LEASH off the view axis. Putting this on the *target*
        // instead was not enough - a low pass that is 50 degrees behind its
        // target still shows an empty panel.
        if angle_between(self.look, focus) > LEASH {
            self.look = focus.turn_towards(self.look, 1.0, LEASH);
        }
        // Hold the horizon in the band the panel can show it in, and hold it
        // *last*: the panel is 21 degrees from the middle to the top edge, so
        // a view that may point 9 degrees off level always has the horizon in
        // shot. Applied before the leash, as it was at first, the leash simply
        // undid it and pushed the horizon off the top of the frame - which is
        // most of what says this is flying, and it was not there.
        self.look = level(self.look);
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
/// horizon in.
fn level(d: V3) -> V3 {
    let lift = d.y.clamp(-0.20, 0.20);
    let flat = v3(d.x, 0.0, d.z).unit_or(v3(0.0, 0.0, 1.0));
    flat.scale((1.0 - lift * lift).sqrt()).add(UP.scale(lift))
}

/// How hard something at `clear` metres of clearance is being avoided, given
/// that avoidance begins at `reach`.
fn urgency_of(clear: f32, reach: f32) -> f32 {
    (1.0 - clear / reach).clamp(0.0, 1.6)
}
