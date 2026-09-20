//! The flight: Reynolds' boids in 3D, the invisible world they steer round,
//! and the camera that is one of them.
//!
//! Nothing here knows about pixels. It is stepped on a **fixed 1/60 s
//! timestep** ([`STEP`]) so the flight is the same whatever rate the frames are
//! drawn at, and every number in it is a metre, a metre per second or a radian
//! per second, so the limits can be read and checked as what they are.
//!
//! The camera is `birds[0]`. It is a boid: it is in every other bird's
//! neighbour list and they are in its own, and it obeys the same speed band and
//! the same kind of turn limit. What it has on top is only what it takes to be
//! a good seat - see [`Sim::steer`]'s camera arm and [`Sim::aim`].

use crate::rng::Rng;
use std::f32::consts::{PI, TAU};

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
pub const BOUND: f32 = 60.0;
pub const FLOOR: f32 = -18.0;
pub const CEILING: f32 = 20.0;

/// How many drifting blobs the world is laid out with. They are all built
/// from the seed and the `terrain` parameter says how many of them are real
/// this frame, so turning it up and down does not re-roll the world.
pub const BLOBS: usize = 5;

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
        Tuning::of(0.65, 6.0, 0.8, 2.4)
    }
}

impl Tuning {
    /// `calm` 0..1: 1 is the widest, slowest turns.
    pub fn of(calm: f32, near: f32, bank: f32, beat: f32) -> Self {
        let calm = calm.clamp(0.0, 1.0);
        Tuning {
            separation: 2.6,
            alignment: 8.0,
            cohesion: 14.0,
            // A calm flock also flies a little slower, which is most of what
            // "gentle" looks like from inside it.
            speed: (4.6 - 1.4 * calm, 7.4 - 1.8 * calm),
            turn: 1.55 - 1.08 * calm,
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
                let r = rng.range(22.0, 48.0);
                Blob {
                    base: v3(r * a.cos(), rng.range(-7.0, 9.0), r * a.sin()),
                    amp: v3(rng.range(5.0, 14.0), rng.range(2.0, 6.0), rng.range(5.0, 14.0)),
                    // Periods of 50 to 210 s, drawn independently, so the three
                    // axes of one blob and the blobs among themselves have no
                    // period in common.
                    rate: v3(
                        TAU / rng.range(50.0, 210.0),
                        TAU / rng.range(50.0, 210.0),
                        TAU / rng.range(50.0, 210.0),
                    ),
                    phase: v3(rng.range(0.0, TAU), rng.range(0.0, TAU), rng.range(0.0, TAU)),
                    radius: rng.range(8.0, 16.0),
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
        6.0 + 1.1 * tune.speed.1 / tune.turn.max(0.15)
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
    /// Which flank the camera is riding, drifting between them over minutes.
    seat_phase: f32,
    rng: Rng,
    acc: Vec<V3>,
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

        for i in 0..n {
            self.fly(i, acc[i], tune, dt);
        }
        self.acc = acc;

        self.aim(tune, dt);
    }

    /// Reynolds, plus the invisible geometry, plus - for `birds[0]` only - a
    /// seat in the flock.
    fn steer(&self, i: usize, tune: &Tuning, blobs: &[(V3, f32)], reach: f32) -> V3 {
        let me = self.birds[i];
        let camera = i == 0;
        let fwd = me.heading();

        let sep_r = if camera { tune.separation.max(tune.near * 0.55) } else { tune.separation };
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
        let mut a = sep.scale(3.4).add(align.scale(1.1 * herd)).add(coh.scale(0.13 * herd));

        // Invisible geometry. The push is *across* the flight path, not back
        // along it: a bird goes round a thing, it does not stop in front of it.
        for (centre, radius) in blobs {
            let away = me.pos.sub(*centre);
            let dist = away.len().max(0.01);
            let clear = dist - radius;
            if clear > reach {
                continue;
            }
            let urgency = (1.0 - clear / reach).clamp(0.0, 1.4);
            let out = away.scale(1.0 / dist);
            // Head on, `across` is nothing; lean on the bird's own up so the
            // choice is made rather than left to rounding.
            let side = out.across(fwd).unit_or(fwd.cross(UP).unit_or(UP));
            a = a.add(side.scale(14.0 * urgency * urgency)).add(out.scale(3.0 * urgency * urgency));
        }

        // Floor, ceiling and the soft edge of the world, all the same shape:
        // a push that grows as the margin closes.
        let head = (CEILING - me.pos.y) / 9.0;
        let feet = (me.pos.y - FLOOR) / 9.0;
        a.y += 5.0 * ((1.0 - feet).max(0.0).powi(2) - (1.0 - head).max(0.0).powi(2));
        // and a gentle wish to be near the cruising band
        a.y += (2.0 - me.pos.y) * 0.02;

        let flat = v3(me.pos.x, 0.0, me.pos.z);
        let out = flat.len();
        if out > BOUND * 0.72 {
            let over = (out - BOUND * 0.72) / (BOUND * 0.28);
            a = a.sub(flat.scale(over * over * 7.0 / out.max(0.01)));
        }

        if camera {
            // The one thing a camera wants that a bird does not: to sit at the
            // trailing edge or on a flank, where the flock is in front of it.
            // In the middle of a flock a camera sees one bird's tail.
            let side = self.course.cross(UP).unit_or(v3(1.0, 0.0, 0.0));
            let seat = self
                .centre
                .sub(self.course.scale(self.spread * 0.9 + tune.near))
                .add(side.scale(self.spread * 0.55 * self.seat_phase.sin()))
                .add(UP.scale(tune.near * 0.3));
            a = a.add(seat.sub(me.pos).scale(0.45));
        } else {
            // Something over there, once in a while.
            let d = self.world.interest.at.sub(me.pos);
            if d.len() > 12.0 {
                a = a.add(d.unit_or(V3::ZERO).scale(0.9 * self.world.interest.pull));
            }
        }
        a
    }

    /// Integrate one bird: speed band, turn-rate limit, bank, wingbeat.
    fn fly(&mut self, i: usize, a: V3, tune: &Tuning, dt: f32) {
        let camera = i == 0;
        let me = &mut self.birds[i];
        let fwd = me.heading();
        let speed = me.speed().max(0.1);

        // A turn is the part of the acceleration across the flight path, and
        // limiting it *is* the turn-rate limit: |a_across| = omega * v.
        let turn = if camera { tune.turn * 0.55 } else { tune.turn };
        let along = a.dot(fwd).clamp(-3.5, 3.5);
        let across = a.across(fwd).clamp_len(turn * speed);
        let a = fwd.scale(along).add(across);

        me.vel = me.vel.add(a.scale(dt));
        let s = me.speed();
        let (lo, hi) = tune.speed;
        if s > 1e-4 {
            me.vel = me.vel.scale(s.clamp(lo, hi) / s);
        }

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
        let fwd = me.heading();

        // The flock, weighted towards what is ahead and not too far off.
        let mut focus = V3::ZERO;
        let mut weight = 0.0;
        for b in &self.birds[1..] {
            let d = b.pos.sub(me.pos);
            let dist = d.len().max(0.5);
            let dir = d.scale(1.0 / dist);
            let w = (dir.dot(fwd) + 0.35).max(0.0) / (1.0 + dist * dist / 500.0);
            focus = focus.add(dir.scale(w));
            weight += w;
        }
        let focus = if weight > 1e-4 { focus.scale(1.0 / weight).unit_or(fwd) } else { fwd };

        let mut want = fwd.lerp(focus, 0.4).unit_or(fwd);
        // Hold the horizon in the band the panel can show it in.
        let lift = want.y.clamp(-0.34, 0.34);
        let flat = v3(want.x, 0.0, want.z).unit_or(fwd);
        want = flat.scale((1.0 - lift * lift).sqrt()).add(UP.scale(lift));

        self.look = self.look.lerp(want, 1.0 - (-dt / 0.9_f32).exp()).unit_or(want);

        let want_roll = (me.roll * tune.bank).clamp(-0.42, 0.42);
        self.view_roll += (want_roll - self.view_roll) * (1.0 - (-dt / 1.4_f32).exp());
    }

    /// Right, up, forward for the view: the low-passed look direction, rolled.
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
    pub fn clearance(&self, live: usize) -> f32 {
        let mut worst = f32::INFINITY;
        for blob in &self.world.blobs[..live.min(self.world.blobs.len())] {
            let centre = blob.at(self.t);
            for b in &self.birds {
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

/// Elevation of a direction above the horizon, radians.
pub fn elevation(d: V3) -> f32 {
    d.y.clamp(-1.0, 1.0).asin()
}

/// Bearing of a direction, radians, for measuring yaw rate.
pub fn bearing(d: V3) -> f32 {
    d.x.atan2(d.z)
}

/// The smaller of the two ways round, radians.
pub fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}
