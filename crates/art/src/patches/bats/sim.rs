//! The flight: a small, loose colony of bats, hunting-erratic rather than
//! flocking-smooth, seen from a camera that never moves.
//!
//! Card 319 (the second pass) drops the tree line and its "pour from the
//! roost" mechanic - there is no roost any more, only open black space and a
//! moon - but keeps everything the first pass got right about *how a bat
//! moves* (card 313's Log): event-driven jinking rather than a boid's
//! continuous three-force steering. Each bat picks a target point at random
//! intervals, turns onto it hard and fast for a short burst, then cruises
//! gently until the next pick - "erratic, jinking, short sharp turns", never
//! a long glide. Vector math is copied from `flock::sim::V3` rather than
//! shared - see `patches/bats/mod.rs`'s doc comment for why.
//!
//! The one new mechanic is [`Phase::Grouping`]: card 319's "rarely, a small
//! group of 3-6 streams past as the event" replaces card 313's "colony pours
//! out of the roost" - the same shape (a few bats picked out, aimed
//! together, released back to ambient wandering when it ends) without a
//! roost point to aim *from*, since there is no tree line to have a gap in
//! any more. A handful of bats sweep together from one side of the roaming
//! cone to the other instead.

use crate::rng::Rng;
use std::f32::consts::TAU;

/// The fixed timestep, seconds. Everything here is integrated at this rate
/// however often a frame is asked for, so a run is the same flight at 30 fps
/// or 60.
pub const STEP: f32 = 1.0 / 60.0;

pub const UP: V3 = v3(0.0, 1.0, 0.0);

/// Wingspan, metres - the number [`super::AREA`] and the drawing scale against.
pub const SPAN: f32 = 0.32;

// --------------------------------------------------------------------------
// Vector math, copied from `flock::sim::V3` (card 313's Log: flock must stay
// byte-identical, so nothing there is imported or edited; this is the same
// generic vector algebra, not flock-specific logic).
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

    pub fn len(self) -> f32 {
        self.dot(self).sqrt()
    }

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
            return self.cross(UP).unit_or(self.cross(v3(1.0, 0.0, 0.0))).scale(take.sin()).add(self.scale(take.cos()));
        }
        self.scale((angle - take).sin() / sin).add(to.scale(take.sin() / sin)).unit_or(to)
    }
}

// --------------------------------------------------------------------------

/// Everything that is taste, rebuilt from the parameters every frame.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Overall flight speed multiplier.
    pub pace: f32,
    /// 0..1: how often, and how sharply, a bat changes its mind.
    pub jink: f32,
    /// 0..1: how loosely the colony holds together.
    pub loose: f32,
    /// Wingbeats a second.
    pub beat_hz: f32,
    /// 0..1: how often a small group streams past together.
    pub stream: f32,
}

/// How far out the roaming volume reaches, as an azimuth and elevation about
/// the fixed camera (degrees) and a depth range (metres). Sampling directly
/// in these camera-relative angles, rather than in a world-space box, is what
/// keeps a bat inside a sensible shot at any depth without a separate leash:
/// the safe cone is the same shape the lens itself has.
///
/// Card 319's "the moon, and nothing, or one bat" wants a colony that is
/// often out of shot - a low bat count and long idle gaps between jinks
/// already thin the picture out a great deal on their own (see the Log for
/// how this cone's width was actually chosen: wide enough that a bat spends
/// real time off to the side, not so wide that a default colony of three is
/// routinely invisible for seconds at a stretch, which reads as a patch that
/// forgot to animate rather than a calm one).
const AZ_CENTRE: f32 = 0.0;
const AZ_AMP: f32 = 13.0;
const EL_CENTRE: f32 = 2.0;
const EL_AMP: f32 = 9.0;
const DEPTH_CENTRE: f32 = 9.5;
const DEPTH_AMP: f32 = 5.0;

const AZ_JIT: f32 = 11.0;
const EL_JIT: f32 = 9.0;
const DEPTH_JIT: f32 = 7.0;
// `pub(crate)`, not private: `tests.rs` checks bats against these same
// bounds rather than duplicating the numbers.
pub(crate) const EL_MIN: f32 = -14.0;
pub(crate) const EL_MAX: f32 = 18.0;
pub(crate) const AZ_MIN: f32 = -26.0;
pub(crate) const AZ_MAX: f32 = 26.0;

pub const Z_NEAR: f32 = 2.6;
pub const Z_FAR: f32 = 42.0;

/// Chance, per retarget, that a bat is sent on a close flyby instead of an
/// ordinary waypoint - "up close it may span 12+" (the picture's own words,
/// carried over unchanged from card 313).
const SWOOP_CHANCE: f32 = 0.05;
const SWOOP_DEPTH: (f32, f32) = (Z_NEAR, 6.0);

/// Average seconds between a bat changing its mind, at `jink` 0 and 1. Short
/// even at 0: this is a hunting colony, never a glide.
const JINK_INTERVAL: (f32, f32) = (0.75, 0.22);
/// A bat that reaches its own target - real or, near enough, an insect
/// caught - jinks onto a new one at once rather than flying on past it. This
/// is also what stops a close swoop turning into a straight shot through the
/// lens: without it a bat aimed at a near point sails on past depth zero and
/// spends a long stretch fighting `contain`'s clamp back at the near plane.
const ARRIVE_RADIUS: f32 = 2.4;
/// How hard a bat turns onto a freshly picked target: nearly instantly, for
/// [`BURST`] seconds, then it eases off into a gentle cruise correction -
/// the "short sharp turn" against the flat stretches either side of it.
const SNAP_RATE: f32 = 15.0;
const CRUISE_RATE: f32 = 0.85;
const BURST: f32 = 0.16;

/// A small, always-on, never-repeating wobble layered on every bat's heading,
/// so it never flies a truly straight line even between jinks - real erratic
/// flight, not a metronome. Two per-bat frequencies at an irrational ratio
/// keep it from ever settling into a visible beat.
const WOBBLE_AMP: f32 = 0.55;

const BASE_SPEED: f32 = 6.4;
pub const PEP: (f32, f32) = (0.82, 1.22);

/// How many bats a group event pulls in - "a small group of 3-6" (card 319's
/// picture), a fixed small count rather than a fraction of the colony, since
/// the colony itself is now small ("bats: max at once, low default").
const GROUP_SIZE: (f32, f32) = (3.0, 6.0);
const GROUP_DURATION: (f32, f32) = (2.0, 3.4);
/// How far a grouped bat fans out from the shared sweep's own centre line,
/// so it reads as a loose handful rather than a single file.
const GROUP_FAN: f32 = 3.2;

#[derive(Clone, Copy, Debug)]
pub struct Bat {
    pub pos: V3,
    pub vel: V3,
    target: V3,
    retarget_at: f64,
    burst_until: f64,
    /// Wingbeat phase, radians. Grows without wraparound (nothing here ever
    /// takes its modulus): `sin`/`cos` do not care, and an unwrapped phase is
    /// what lets [`Bat::interpolate`] blend two nearby simulated moments by a
    /// plain lerp for motion blur, safely, since it is monotonic.
    pub phase: f32,
    /// Fixed per-bat variation: beat timing, a group event's lateral fan
    /// sign, and (with `jig`) the wobble. Not a clone of the bat beside it.
    pub trim: f32,
    pep: f32,
    off_az: f32,
    off_el: f32,
    jig: (f32, f32),
    /// Whether this bat is one the current group event picked up (kept while
    /// the event runs; irrelevant once it ends).
    grouped: bool,
}

impl Bat {
    pub fn heading(&self) -> V3 {
        self.vel.unit_or(v3(0.0, 0.0, 1.0))
    }

    /// A bat part-way between two simulated moments, `t` of the way from `a`
    /// to `b`: position, velocity (and so heading) and wingbeat phase
    /// lerped, everything else (which does not feed the drawing) carried
    /// from `b`. This is what [`super::Bats::render`]'s motion blur draws at
    /// several such in-between moments instead of only the frame's own
    /// landing point (card 319: "motion blur ... is welcome").
    #[must_use]
    pub fn interpolate(a: &Bat, b: &Bat, t: f32) -> Bat {
        let mut out = *b;
        out.pos = a.pos.lerp(b.pos, t);
        out.vel = a.vel.lerp(b.vel, t);
        out.phase = a.phase + (b.phase - a.phase) * t;
        out
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle { until: f64 },
    /// A handful of bats sweep together from `from` to `to` (both points in
    /// the same camera-relative world the bats fly in) over `until - start`
    /// seconds - card 319's "small group of 3-6 streams past", replacing
    /// card 313's roost-and-corridor pour now that there is no roost.
    Grouping { until: f64, start: f64, from: V3, to: V3 },
}

pub struct Sim {
    pub bats: Vec<Bat>,
    rng: Rng,
    t: f64,
    /// The camera's own axes, fixed for the life of the patch (the camera
    /// never moves).
    fwd: V3,
    right: V3,
    up: V3,
    wander_freq: [f32; 3],
    wander_phase: [f32; 3],
    phase: Phase,
    nearest: f32,
    /// The current `stream` parameter, stashed here because [`Sim::update_phase`]
    /// needs it and takes no arguments of its own (it runs from inside
    /// [`Sim::step`], which is where the fresh value arrives every frame).
    stream_hint: f32,
}

impl Sim {
    pub fn new(seed: u64, fwd: V3, right: V3, up: V3, birds: usize) -> Sim {
        let mut rng = Rng::new(seed ^ 0xba75_5eed);
        let wander_freq = [rng.range(0.028, 0.052), rng.range(0.021, 0.041), rng.range(0.017, 0.033)];
        let wander_phase = [rng.range(0.0, TAU), rng.range(0.0, TAU), rng.range(0.0, TAU)];
        let mut sim = Sim {
            bats: Vec::new(),
            rng,
            t: 0.0,
            fwd,
            right,
            up,
            wander_freq,
            wander_phase,
            phase: Phase::Idle { until: 4.0 },
            nearest: Z_FAR,
            stream_hint: 0.2,
        };
        sim.resize(birds);
        // Warm the colony in: run the flight a little before anyone asks for
        // a frame, so bats start mid-hunt, not lined up at their spawn points
        // (the brief's "warm up invisibly").
        let tune = Tuning { pace: 1.0, jink: 0.6, loose: 0.55, beat_hz: 9.0, stream: 0.2 };
        for _ in 0..(3.0 / STEP) as usize {
            sim.step(&tune, STEP);
        }
        sim
    }

    pub fn resize(&mut self, n: usize) {
        let n = n.clamp(1, 200);
        while self.bats.len() < n {
            let mut bat = self.spawn();
            // Stagger the first jink so a freshly added bat is not in lockstep
            // with everyone else.
            bat.retarget_at = self.t + f64::from(self.rng.range(0.0, 1.0));
            self.bats.push(bat);
        }
        self.bats.truncate(n);
    }

    fn spawn(&mut self) -> Bat {
        let az = AZ_CENTRE + self.rng.range(-AZ_AMP, AZ_AMP);
        let el = (EL_CENTRE + self.rng.range(-EL_AMP, EL_AMP)).clamp(EL_MIN, EL_MAX);
        let depth = DEPTH_CENTRE + self.rng.range(-DEPTH_AMP, DEPTH_AMP);
        let pos = self.dir_local(az.to_radians(), el.to_radians()).scale(depth);
        Bat {
            pos,
            vel: self.fwd.scale(0.1),
            target: pos,
            retarget_at: 0.0,
            burst_until: 0.0,
            phase: self.rng.range(0.0, TAU),
            trim: self.rng.f32(),
            pep: self.rng.range(PEP.0, PEP.1),
            off_az: self.rng.range(-9.0, 9.0),
            off_el: self.rng.range(-5.0, 5.0),
            jig: (self.rng.range(1.1, 2.3), self.rng.range(0.0, TAU)),
            grouped: false,
        }
    }

    /// A unit vector in the camera's own axes: `az` right of forward, `el`
    /// above it, both radians. The same construction the renderer's `ray`
    /// uses, so a target the flight picks and the cone the lens sees agree by
    /// definition.
    fn dir_local(&self, az: f32, el: f32) -> V3 {
        spherical(self.fwd, self.right, self.up, az, el)
    }

    fn wander(&self, t: f64) -> (f32, f32, f32) {
        let t = t as f32;
        let az = AZ_CENTRE + AZ_AMP * 0.5 * (self.wander_freq[0] * t + self.wander_phase[0]).sin();
        let el = EL_CENTRE + EL_AMP * 0.5 * (self.wander_freq[1] * t + self.wander_phase[1]).sin();
        let depth = DEPTH_CENTRE + DEPTH_AMP * 0.6 * (self.wander_freq[2] * t + self.wander_phase[2]).sin();
        (az, el, depth)
    }

    fn pick_target(&mut self, i: usize, loose: f32) -> V3 {
        let (az0, el0, depth0) = self.wander(self.t);
        let (off_az, off_el) = (self.bats[i].off_az, self.bats[i].off_el);
        let spread = 0.35 + 0.65 * loose;
        let mut az = az0 + off_az * spread + self.rng.range(-AZ_JIT, AZ_JIT) * spread;
        let mut el = el0 + off_el * spread + self.rng.range(-EL_JIT, EL_JIT) * spread;
        let mut depth = depth0 + self.rng.range(-DEPTH_JIT, DEPTH_JIT) * (0.5 + 0.5 * spread);
        if self.rng.f32() < SWOOP_CHANCE {
            depth = self.rng.range(SWOOP_DEPTH.0, SWOOP_DEPTH.1);
        }
        az = az.clamp(AZ_MIN, AZ_MAX);
        el = el.clamp(EL_MIN, EL_MAX);
        depth = depth.clamp(Z_NEAR, Z_FAR);
        self.dir_local(az.to_radians(), el.to_radians()).scale(depth)
    }

    /// Where a grouped bat is aimed right now: along the shared sweep from
    /// `from` to `to`, with this bat's own fan offset opening it out from the
    /// centre line so 3-6 bats read as a loose handful rather than a single
    /// file.
    fn group_target(&self, i: usize, start: f64, until: f64, from: V3, to: V3) -> V3 {
        let dur = (until - start).max(0.05);
        let frac = ((self.t - start) / dur).clamp(0.0, 1.0) as f32;
        let lead = from.lerp(to, frac);
        let perp = to.sub(from).cross(self.up).unit_or(self.right);
        let sign = self.bats[i].trim * 2.0 - 1.0;
        lead.add(perp.scale(sign * GROUP_FAN))
    }

    fn update_phase(&mut self) {
        match self.phase {
            Phase::Idle { until } if self.t >= until => {
                let n = self.bats.len();
                let take = (self.rng.range(GROUP_SIZE.0, GROUP_SIZE.1).round() as usize).clamp(1, n);
                let mut idx: Vec<usize> = (0..n).collect();
                // A fixed order would always group the same bats first;
                // shuffle a working index list instead (Fisher-Yates, small n).
                for j in (1..n).rev() {
                    let k = (self.rng.f32() * (j + 1) as f32) as usize % (j + 1);
                    idx.swap(j, k);
                }
                for &j in idx.iter().take(take) {
                    self.bats[j].grouped = true;
                }
                // A straight sweep across the roaming cone, entering from
                // one side and leaving out the other - "streams past".
                let sign = self.rng.sign();
                let el = self.rng.range(EL_MIN * 0.5, EL_MAX * 0.5);
                let depth = self.rng.range(DEPTH_CENTRE - 2.0, DEPTH_CENTRE + 2.0);
                let from = self.dir_local((sign * AZ_MAX * 1.1).to_radians(), el.to_radians()).scale(depth);
                let to = self.dir_local((-sign * AZ_MAX * 1.1).to_radians(), el.to_radians()).scale(depth);
                let dur = f64::from(self.rng.range(GROUP_DURATION.0, GROUP_DURATION.1));
                self.phase = Phase::Grouping { until: self.t + dur, start: self.t, from, to };
            }
            Phase::Grouping { until, .. } if self.t >= until => {
                for b in &mut self.bats {
                    b.grouped = false;
                    // Staggered so the group does not resume in lockstep -
                    // that stagger is most of what makes it disperse rather
                    // than simply switch off together.
                    b.retarget_at = self.t + f64::from(self.rng.range(0.0, 1.6));
                }
                self.phase = Phase::Idle { until: self.t + self.next_idle() };
            }
            _ => {}
        }
    }

    fn next_idle(&mut self) -> f64 {
        // `stream` shortens the average wait; the multiplier keeps any one
        // run from settling into a fixed period. The base wait is long -
        // card 319's "rarely" - most of a run is the ambient colony alone.
        f64::from(lerp(150.0, 22.0, self.stream_hint) * self.rng.range(0.65, 1.55))
    }

    /// Advance the flight by one fixed step. `tune` is rebuilt from the
    /// parameters every frame, so a slider changes the *next* step, never the
    /// past.
    pub fn step(&mut self, tune: &Tuning, dt: f32) {
        self.stream_hint = tune.stream;
        self.t += f64::from(dt);
        self.update_phase();

        let grouping = matches!(self.phase, Phase::Grouping { .. });
        let mut nearest = Z_FAR;
        for i in 0..self.bats.len() {
            let forced = if grouping && self.bats[i].grouped {
                match self.phase {
                    Phase::Grouping { until, start, from, to } => Some(self.group_target(i, start, until, from, to)),
                    _ => None,
                }
            } else {
                None
            };
            let arrived = self.bats[i].target.sub(self.bats[i].pos).len() < ARRIVE_RADIUS;
            if let Some(t) = forced {
                self.bats[i].target = t;
                self.bats[i].burst_until = self.t + f64::from(BURST);
            } else if self.t >= self.bats[i].retarget_at || arrived {
                let t = self.pick_target(i, tune.loose);
                self.bats[i].target = t;
                self.bats[i].burst_until = self.t + f64::from(BURST);
                let interval = lerp(JINK_INTERVAL.0, JINK_INTERVAL.1, tune.jink) * self.rng.range(0.55, 1.6);
                self.bats[i].retarget_at = self.t + f64::from(interval);
            }

            let bat = &mut self.bats[i];
            let want = bat.target.sub(bat.pos).unit_or(bat.heading());
            let tt = self.t as f32;
            let w1 = (tt * bat.jig.0 + bat.jig.1).sin();
            let w2 = (tt * bat.jig.0 * 1.741 + bat.jig.1 * 0.63).cos();
            let side = want.cross(self.up).unit_or(self.right);
            let lift = want.cross(side).unit_or(self.up);
            let wobble = side.scale(w1 * WOBBLE_AMP).add(lift.scale(w2 * WOBBLE_AMP * 0.6));
            let desired = want.add(wobble.scale(0.15)).unit_or(want);

            let bursting = self.t < bat.burst_until;
            let rate = if bursting { SNAP_RATE } else { CRUISE_RATE };
            let heading = bat.heading().turn_towards(desired, 1.0, rate * dt);
            let speed = BASE_SPEED * bat.pep * tune.pace * (1.0 + 0.05 * bat.phase.sin());
            bat.vel = heading.scale(speed);
            bat.pos = bat.pos.add(bat.vel.scale(dt));
            bat.phase += TAU * tune.beat_hz * dt * (1.0 + 0.06 * (bat.trim - 0.5));
            contain(self.fwd, self.right, self.up, bat);

            let depth = bat.pos.dot(self.fwd);
            if depth > 0.35 {
                nearest = nearest.min(depth);
            }
        }
        self.nearest = nearest;
    }

    pub fn nearest(&self) -> f32 {
        self.nearest
    }

    pub fn grouping(&self) -> bool {
        matches!(self.phase, Phase::Grouping { .. })
    }

    /// The fixed camera's own axes - forward, right, up. For the renderer
    /// (which rebuilds the same three independently from the seed, since it
    /// owns the world) and for `tests.rs`, which has no other way to check a
    /// bat against the roaming cone.
    pub fn cam(&self) -> (V3, V3, V3) {
        (self.fwd, self.right, self.up)
    }

    /// Total simulated time. For [`super::Bats::advance`]'s motion-blur trace,
    /// which timestamps each captured state.
    pub fn t(&self) -> f64 {
        self.t
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// A **planar** bearing, `az` right of `fwd` and `el` above it, in the basis
/// `(fwd, right, up)`: scaling the result by `depth` gives the point whose
/// *z-depth* (its dot product with `fwd`, the same convention `View::project`
/// and `contain` both use for "how far in front of the lens") is exactly
/// `depth`, not a true spherical direction's Euclidean range at an angle off
/// axis. Getting this wrong is not academic: it was card 313's own bug (see
/// its Log) - `contain` reads `az`/`el` back out with `atan(right-component /
/// depth)`, the exact inverse of *this* convention, so building the forward
/// direction with `sin`/`cos` instead left every correction landing a bat at
/// the wrong actual depth, a standing mismatch that trapped it flapping in
/// place just past the near plane.
///
/// A free function, not a method, so [`contain`] can call it without needing
/// a borrow of the whole [`Sim`] while it already holds a mutable borrow of
/// one bat inside it.
fn spherical(fwd: V3, right: V3, up: V3, az: f32, el: f32) -> V3 {
    fwd.add(right.scale(az.tan())).add(up.scale(el.tan()))
}

/// A gentle nudge back inside the roaming cone for anything that has drifted
/// well past it - a safety net behind the target sampling, which already
/// keeps new targets inside bounds; this only fires if a string of jinks (or
/// a group sweep) has carried a bat out past a generous margin.
///
/// Free rather than a method on `Sim` for the same borrow-checker reason as
/// [`spherical`]: its caller already holds `&mut self.bats[i]`.
fn contain(fwd: V3, right: V3, up: V3, bat: &mut Bat) {
    let v = bat.pos;
    let depth = v.dot(fwd);
    if depth < 0.2 {
        bat.pos = v.sub(fwd.scale(depth - 0.5));
        return;
    }
    let az = (v.dot(right) / depth).atan().to_degrees();
    let el = (v.dot(up) / depth).atan().to_degrees();
    let margin = 1.6;
    let az_c = az.clamp(AZ_MIN * margin, AZ_MAX * margin);
    let el_c = el.clamp(EL_MIN * margin, EL_MAX * margin);
    let depth_c = depth.clamp(Z_NEAR * 0.6, Z_FAR * 1.15);
    if (az - az_c).abs() > 0.01 || (el - el_c).abs() > 0.01 || (depth - depth_c).abs() > 0.01 {
        bat.pos = spherical(fwd, right, up, az_c.to_radians(), el_c.to_radians()).scale(depth_c);
    }
}
