//! The vocabulary of entrances, crossings and exits, and the director that
//! schedules them.
//!
//! **An act is a closed-form function of elapsed time.** Nothing here
//! integrates: a ghost's whole path - where it is, how it is squashed, which
//! way it looks - is a formula in `el` (seconds since the act began), decided
//! the instant the act is drawn from the seed. That is what makes the
//! schedule reproducible at any step size: [`Director::advance`] only ever
//! asks "does the timeline reach this instant yet?", never "how many frames
//! have we drawn?" - the same discipline `flock` uses for its physics
//! ([`crate::patches::flock::Flock::advance`]), minus the physics, because
//! nothing here needs it.
//!
//! **The vocabulary** (card 315's list): drift, bounce, peek, swoop, and two
//! ways for a pair to share a moment - crossing paths, or one chasing
//! another. [`Director`] draws from it with [`crate::variety::Variety`], the
//! same tool the clocks' `dance` composer uses, so a long run keeps finding
//! new combinations rather than favouring whichever the dice like.

use crate::color::smoothstep;
use crate::frame::W;
use crate::patch::Ctx;
use crate::rng::Rng;
use crate::variety::Variety;
use std::f32::consts::{PI, TAU};

pub(crate) const KINDS: usize = 6;
pub(crate) const NAMES: [&str; KINDS] = ["drift", "bounce", "peek", "swoop", "cross", "chase"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Left to right or back, on a slow floating bob.
    Drift,
    /// Along like a ball, squash and stretch on every landing.
    Bounce,
    /// In from an edge, looks around, ducks back out - never crosses.
    Peek,
    /// In from a top corner in an arc, out the other side.
    Swoop,
    /// Two ghosts, opposite edges, passing through the middle.
    Cross,
    /// Two ghosts, the same path, one a beat behind the other.
    Chase,
}

impl Kind {
    fn of(i: u64) -> Kind {
        match i % KINDS as u64 {
            0 => Kind::Drift,
            1 => Kind::Bounce,
            2 => Kind::Peek,
            3 => Kind::Swoop,
            4 => Kind::Cross,
            _ => Kind::Chase,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        NAMES[match self {
            Kind::Drift => 0,
            Kind::Bounce => 1,
            Kind::Peek => 2,
            Kind::Swoop => 3,
            Kind::Cross => 4,
            Kind::Chase => 5,
        }]
    }

    /// One ghost, or two performing together.
    fn cast(self) -> usize {
        match self {
            Kind::Cross | Kind::Chase => 2,
            _ => 1,
        }
    }
}

/// One ghost's silhouette, fixed for its whole appearance from the seed: a
/// dome of radius `r`, straight sides for `hem_base` LEDs, then `humps`
/// scalloped points that dip `hem_amp` further - see `mod.rs::body_sdf`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shape {
    pub r: f32,
    pub hem_base: f32,
    pub hem_amp: f32,
    pub humps: f32,
    pub phase0: f32,
    /// Ripple speed, radians a second: how fast the hem's wave travels.
    pub ripple_rate: f32,
    /// How solid it is: where two overlap, or it crosses something, this is
    /// what lets you tell.
    pub alpha: f32,
}

impl Shape {
    fn new(rng: &mut Rng, size: f32) -> Shape {
        // A little variety in size and proportion between ghosts, from the
        // seed (card 315's brief): a touch wider or narrower, a rounder or
        // taller dome, three or four points on the hem.
        let aspect = rng.range(0.6, 0.8);
        let r = (size * aspect * 0.5).max(1.4);
        let body = (size - r).max(size * 0.32);
        let hem_amp = body * rng.range(0.3, 0.42);
        let hem_base = (body - hem_amp).max(0.25);
        Shape {
            r,
            hem_base,
            hem_amp,
            humps: (3 + (rng.u64() % 2)) as f32,
            phase0: rng.range(0.0, TAU),
            ripple_rate: rng.range(1.6, 2.6),
            alpha: rng.range(0.78, 0.94),
        }
    }

    /// Half the width a silhouette needs clearing before nothing of it is on
    /// the panel: where an entrance starts and an exit ends.
    pub(crate) fn margin(self) -> f32 {
        self.r + 1.6
    }

    pub(crate) fn total_height(self) -> f32 {
        self.r + self.hem_base + self.hem_amp
    }
}

/// Everything the picture needs to draw one ghost this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pose {
    pub shape: Shape,
    pub cx: f32,
    /// The shoulder - dome meets sides - not the ghost's midpoint: the dome
    /// is measured upward from here and the hem downward.
    pub cy: f32,
    pub stretch_x: f32,
    pub stretch_y: f32,
    /// Where it is looking, as a unit vector (0, -1 is straight up the panel).
    pub gaze: (f32, f32),
    /// The hem wave's current phase.
    pub ripple: f32,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smooth01(t: f32) -> f32 {
    smoothstep(0.0, 1.0, t)
}

/// A straight crossing, off-screen to off-screen: where an entrance starts
/// and an exit ends for every kind but [`Kind::Peek`].
fn travel(flip: bool, u: f32, margin: f32) -> f32 {
    let (from, to) = if flip { (W as f32 + margin, -margin) } else { (-margin, W as f32 + margin) };
    lerp(from, to, u)
}

/// One ghost's part in an act: its shape and everything a trajectory needs.
#[derive(Clone, Copy, Debug)]
struct Plan {
    shape: Shape,
    flip: bool,
    y: f32,
    /// Bob height (drift), hop height (bounce), arc depth (swoop) or how far
    /// past the edge it comes (peek) - one number, meaning depends on `Kind`.
    amp: f32,
    freq: f32,
    bounces: f32,
    /// The follower's head start behind the leader, seconds (chase only).
    delay: f64,
}

/// A scheduled performance: what it is, when, and by whom.
pub(crate) struct Act {
    pub kind: Kind,
    pub start: f64,
    pub duration: f64,
    /// The leg every ghost in the act travels at its own pace (chase's
    /// follower starts `delay` into it); equal to `duration` except chase.
    travel_dur: f64,
    pub name: String,
    pub tags: Vec<String>,
    plans: Vec<Plan>,
}

/// How much a bounce's energy - squash landing, stretch in flight - answers
/// to the `bounce` param, and how far a drifting or peeking ghost's float bob
/// answers to its opposite: the same knob runs both, so "bouncy" and "floaty"
/// are really one dial (card 315: "bounce (how bouncy vs floaty)").
const STRETCH_K: f32 = 0.5;
const SQUASH_K: f32 = 0.42;
const CONTACT_W: f32 = 0.1;

fn squash_stretch(p: f32, energy: f32) -> (f32, f32) {
    let speed = (1.0 - 2.0 * p).abs();
    let stretch = 1.0 + STRETCH_K * energy * speed;
    let contact = smoothstep(CONTACT_W, 0.0, p.min(1.0 - p));
    let squash = 1.0 - SQUASH_K * energy * contact;
    let sy = (stretch * squash).max(0.4);
    let sx = (1.0 / sy).clamp(0.55, 1.9);
    (sx, sy)
}

impl Act {
    /// Every ghost's pose at engine time `t`. Ghosts not yet born or already
    /// gone (before `start` or after `start + duration`, and for a chased
    /// follower, before its own delayed start) are left out.
    pub(crate) fn poses_at(&self, t: f64) -> Vec<Pose> {
        let el = t - self.start;
        if el < 0.0 || el > self.duration {
            return Vec::new();
        }
        let el = el as f32;
        let dur = self.travel_dur.max(1.0 / 60.0) as f32;

        // Pass one: where every ghost's centre and squash are, ignoring
        // gaze - a follower's gaze at the leader needs the leader's centre
        // already known, and vice versa for the ghosts who glance at a
        // passer-by.
        let centres: Vec<(f32, f32, f32, f32, f32)> = self
            .plans
            .iter()
            .enumerate()
            .map(|(i, p)| self.centre_of(i, p, el, dur))
            .collect();

        (0..self.plans.len())
            .filter_map(|i| {
                let (x, y, sx, sy, active_el) = centres[i];
                if active_el.is_nan() {
                    return None;
                }
                let p = &self.plans[i];
                let ripple = active_el * p.shape.ripple_rate + p.shape.phase0;
                let gaze = self.gaze_of(i, active_el, dur, &centres);
                Some(Pose { shape: p.shape, cx: x, cy: y, stretch_x: sx, stretch_y: sy, gaze, ripple })
            })
            .collect()
    }

    /// `(x, y, stretch_x, stretch_y, active_el)`. `active_el` is this ghost's
    /// own elapsed time - `NAN` while a chased follower has not been let go
    /// yet, which `poses_at` reads as "not born".
    fn centre_of(&self, i: usize, p: &Plan, el: f32, dur: f32) -> (f32, f32, f32, f32, f32) {
        let margin = p.shape.margin();
        match self.kind {
            Kind::Drift => {
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                let y = p.y + p.amp * (TAU * p.freq * el + p.shape.phase0).sin();
                (x, y, 1.0, 1.0, el)
            }
            Kind::Bounce => {
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                let hops = p.bounces.max(1.0);
                let hop = (u * hops).rem_euclid(1.0);
                let height = 4.0 * hop * (1.0 - hop);
                let y = p.y - p.amp * height;
                let (sx, sy) = squash_stretch(hop, p.freq);
                (x, y, sx, sy, el)
            }
            Kind::Peek => {
                let (x, y) = peek_pos(p, el, dur, margin);
                (x, y, 1.0, 1.0, el)
            }
            Kind::Swoop => {
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                let y = p.y + p.amp * (PI * u).sin();
                let (sx, sy) = swoop_stretch(u, p.freq);
                (x, y, sx, sy, el)
            }
            Kind::Cross => {
                // Ghost 1 goes the opposite way to ghost 0 - its own `y`
                // (built with the separation already in it, and already
                // clamped to the panel) is otherwise exactly ghost 0's plan.
                let flip = if i == 1 { !p.flip } else { p.flip };
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(flip, u, margin);
                (x, p.y, 1.0, 1.0, el)
            }
            Kind::Chase => {
                let el2 = if i == 1 { el - p.delay as f32 } else { el };
                if el2 < 0.0 {
                    return (0.0, 0.0, 1.0, 1.0, f32::NAN);
                }
                let u = (el2 / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                (x, p.y, 1.0, 1.0, el2)
            }
        }
    }

    /// Look at the other ghost in `centres` when it is within `within` LEDs
    /// and actually born yet; otherwise look ahead.
    fn glance_or_forward(&self, i: usize, forward: f32, centres: &[(f32, f32, f32, f32, f32)], within: f32) -> (f32, f32) {
        let j = 1 - i.min(1);
        let (ox, oy, _, _, oel) = centres[j];
        if oel.is_nan() {
            return (forward, 0.0);
        }
        let (mx, my, _, _, _) = centres[i];
        let (dx, dy) = (ox - mx, oy - my);
        let d = (dx * dx + dy * dy).sqrt();
        if d < within && d > 1e-3 {
            (dx / d, dy / d)
        } else {
            (forward, 0.0)
        }
    }

    fn gaze_of(&self, i: usize, el: f32, dur: f32, centres: &[(f32, f32, f32, f32, f32)]) -> (f32, f32) {
        let p = &self.plans[i];
        let forward = if p.flip { -1.0 } else { 1.0 };
        match self.kind {
            Kind::Peek => {
                let (enter, leave) = (dur * 0.28, dur * 0.28);
                let hold = (dur - enter - leave).max(0.05);
                if el >= enter && el < enter + hold {
                    let u = (el - enter) / hold;
                    // Looking around: side to side, a slow, deliberate sweep.
                    ((TAU * 0.6 * u).sin(), -0.2)
                } else {
                    (forward, 0.0)
                }
            }
            Kind::Cross => {
                // Both glance at each other, but only right around the
                // moment they actually cross - otherwise each is looking
                // ahead at where it is going, which is most of the act.
                self.glance_or_forward(i, forward, centres, p.shape.margin() * 2.2)
            }
            Kind::Chase => {
                // The leader keeps its eyes ahead; the follower is the one
                // with someone to look at.
                if i == 0 {
                    (forward, 0.0)
                } else {
                    self.glance_or_forward(i, forward, centres, p.shape.margin() * 3.5)
                }
            }
            Kind::Bounce => {
                let u = (el / dur).clamp(0.0, 1.0);
                let hops = p.bounces.max(1.0);
                let hop = (u * hops).rem_euclid(1.0);
                let up = -(1.0 - 2.0 * hop) * 0.4;
                normalise((forward, up))
            }
            Kind::Swoop => {
                let u = (el / dur).clamp(0.0, 1.0);
                let up = -(PI * u).cos() * 0.5;
                normalise((forward, up))
            }
            Kind::Drift => normalise((forward, 0.0)),
        }
    }
}

fn normalise((x, y): (f32, f32)) -> (f32, f32) {
    let d = (x * x + y * y).sqrt().max(1e-6);
    (x / d, y / d)
}

/// Squash/stretch for a swoop's arc: stretched at the fast middle of the dive,
/// rounder at the still moments it enters and leaves on.
fn swoop_stretch(u: f32, energy: f32) -> (f32, f32) {
    let speed = (PI * u).cos().abs();
    let sy = 1.0 + 0.22 * energy * speed;
    (1.0 / sy.max(0.6), sy)
}

/// In from an edge, a slow look around, back out - never reaching the far
/// side. `amp` is how far past the edge it comes, in LEDs.
fn peek_pos(p: &Plan, el: f32, dur: f32, margin: f32) -> (f32, f32) {
    let (enter, leave) = (dur * 0.28, dur * 0.28);
    let hold = (dur - enter - leave).max(0.05);
    let edge_x = if p.flip { W as f32 + margin } else { -margin };
    let peek_x = if p.flip { W as f32 - p.amp } else { p.amp };
    let x = if el < enter {
        lerp(edge_x, peek_x, smooth01(el / enter))
    } else if el < enter + hold {
        peek_x
    } else {
        lerp(peek_x, edge_x, smooth01((el - enter - hold) / leave))
    };
    (x, p.y)
}

// --------------------------------------------------------------------------

/// Seconds of nothing on screen between beats - "empty moments between", the
/// brief's own words.
const REST: (f32, f32) = (1.2, 4.5);
/// How often a beat is more than a single ghost, when the `ghosts` param
/// allows it - a livelier moment among the ordinary solo ones.
const ENSEMBLE_CHANCE: f32 = 0.4;

pub(crate) struct Director {
    rng: Rng,
    variety: Variety,
    acts: Vec<Act>,
    /// The instant up to which the schedule is decided. Strictly increases
    /// every call to `spawn_one`, which is what keeps `advance`'s loop
    /// finite, and depends on nothing but itself and `t`, which is what
    /// keeps the whole schedule a pure function of `t` (the module doc).
    frontier: f64,
}

/// How far ahead of the frame being drawn the schedule stays decided.
const LOOKAHEAD: f64 = 1.0;

impl Director {
    pub(crate) fn new(seed: u64) -> Director {
        Director { rng: Rng::new(seed ^ 0x67_68_73_74), variety: Variety::default(), acts: Vec::new(), frontier: 0.0 }
    }

    /// Extend the schedule until it comfortably covers `t`.
    pub(crate) fn advance(&mut self, t: f64, ctx: &Ctx) {
        while self.frontier < t + LOOKAHEAD {
            self.spawn_one(ctx);
        }
        // Acts fully in the past cost nothing to keep, but a run of hours
        // should not grow this list forever; a small tail is kept for
        // `Patch::playing`'s "what just happened".
        if self.acts.len() > 96 {
            let keep = self.acts.len() - 48;
            self.acts.drain(..keep);
        }
    }

    /// Every ghost on screen at `t`, across every act active there.
    pub(crate) fn poses_at(&self, t: f64) -> Vec<Pose> {
        self.acts.iter().filter(|a| a.start <= t && t < a.start + a.duration).flat_map(|a| a.poses_at(t)).collect()
    }

    /// The most recently started act, for `Patch::playing`.
    pub(crate) fn last(&self) -> Option<&Act> {
        self.acts.last()
    }

    /// One beat: every ghost in it is born together at `self.frontier`, and
    /// the next beat never starts until every one of them has left and a
    /// rest has passed. That is what makes "empty moments between" real -
    /// filling a slot the instant an old act frees it, the first design
    /// tried, kept the screen at cap for ever after the first beat and never
    /// offered a cross or a chase a pair of free slots to land in again.
    fn spawn_one(&mut self, ctx: &Ctx) {
        let cap = ctx.get("ghosts").round().max(1.0) as usize;
        let pace = ctx.get("pace").max(0.1);

        let mut slots = 1_usize;
        if cap >= 2 && self.rng.f32() < ENSEMBLE_CHANCE {
            slots = 2 + (self.rng.u64() % cap.saturating_sub(1) as u64) as usize;
        }

        let start = self.frontier;
        let mut beat_end = start;
        let mut remaining = slots;
        while remaining > 0 {
            let kind = self.choose_kind(remaining, ctx);
            remaining -= kind.cast();
            let act = self.build_act(kind, start, pace, ctx);
            beat_end = beat_end.max(act.start + act.duration);
            self.variety.note(&act.name, &act.tags);
            self.acts.push(act);
        }
        self.frontier = beat_end + f64::from(self.rng.range(REST.0, REST.1)) / f64::from(pace);
    }

    fn choose_kind(&mut self, free: usize, ctx: &Ctx) -> Kind {
        // A pair (cross, chase) can only ever be offered the instant both
        // slots are free at once, which - once the screen is busy at cap - is
        // rare; weighed on equal footing against four solo rivals for every
        // one of those rare instants, a pair would get crowded out and never
        // played at all (measured: zero in ten minutes at the default cap).
        // So when a pair *can* fit, it gets a coin flip of first refusal,
        // freshest of the two, before the general vocabulary gets a turn.
        if free >= 2 && self.rng.f32() < 0.5 {
            let cross = self.variety.staleness(&["kind:cross".to_string()]);
            let chase = self.variety.staleness(&["kind:chase".to_string()]);
            return if cross <= chase { Kind::Cross } else { Kind::Chase };
        }
        let bounce = ctx.get("bounce");
        let bias = |k: Kind| match k {
            Kind::Bounce | Kind::Swoop => bounce,
            Kind::Drift | Kind::Peek => 1.0 - bounce,
            Kind::Cross | Kind::Chase => 0.5,
        };
        let mut best: Option<(f32, Kind)> = None;
        for i in 0..KINDS as u64 {
            let k = Kind::of(i);
            if k.cast() > free {
                continue;
            }
            let tag = format!("kind:{}", k.name());
            let fresh = 1.0 - self.variety.staleness(std::slice::from_ref(&tag));
            let score = 0.65 * fresh + 0.25 * bias(k) + self.rng.range(0.0, 0.3);
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, k));
            }
        }
        best.map_or(Kind::Drift, |(_, k)| k)
    }

    fn build_act(&mut self, kind: Kind, start: f64, pace: f32, ctx: &Ctx) -> Act {
        let size = ctx.get("size");
        let bounce = ctx.get("bounce");
        let flip = self.rng.u64() & 1 == 0;
        let cast = kind.cast();
        let mut shapes: Vec<Shape> = (0..cast)
            .map(|_| {
                let jitter = self.rng.range(0.85, 1.18);
                Shape::new(&mut self.rng, size * jitter)
            })
            .collect();
        // A pair reads as two ghosts, not twins: give the second a shape of
        // its own rather than sharing the first's exactly.
        if cast == 2 {
            let jitter = self.rng.range(0.85, 1.18);
            shapes[1] = Shape::new(&mut self.rng, size * jitter);
        }

        // Vertical band for the shoulder (`y`): the dome reaches `r` above it,
        // the hem `hem_base + hem_amp` below it, and both have to clear the
        // panel - the dome the top, the hem the bottom, with a little more
        // room at the bottom for the ground line. A single "tallest shape"
        // number does not answer this on its own: a wide-domed, short-hemmed
        // ghost and a small-domed, long-hemmed one need different limits at
        // each end, so both are measured from the shapes actually in this
        // act, not guessed from their total height.
        let tallest = shapes.iter().map(|s| s.total_height()).fold(0.0_f32, f32::max);
        let dome_reach = shapes.iter().map(|s| s.r).fold(0.0_f32, f32::max);
        let hem_reach = shapes.iter().map(|s| s.hem_base + s.hem_amp).fold(0.0_f32, f32::max);
        let y_lo = dome_reach + 2.0;
        let y_hi = (crate::frame::H as f32 - hem_reach - 1.5).max(y_lo + 0.5);
        let y = self.rng.range(y_lo, y_hi);

        // Floatier at `bounce` 0, snappier and higher-hopping at 1 - the one
        // dial the brief asks this param to be.
        let base_speed = (5.5 + 3.5 * bounce) * pace;
        let bob_amp = tallest * (0.12 + 0.1 * (1.0 - bounce));
        let bob_freq = self.rng.range(0.12, 0.22);

        let margin = shapes[0].margin();
        let width = W as f32 + 2.0 * margin;

        let (name, tags, plans, travel_dur, duration) = match kind {
            Kind::Drift => {
                let dur = (width / base_speed) as f64 * self.rng.range(0.9, 1.2) as f64;
                let plan = Plan { shape: shapes[0], flip, y, amp: bob_amp, freq: bob_freq, bounces: 0.0, delay: 0.0 };
                (drift_name(flip), vec!["kind:drift".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Bounce => {
                let bounces = self.rng.range(2.0, 4.0).round().max(2.0);
                let dur = (width / (base_speed * 1.05)) as f64;
                // Ground contact near the bottom of the safe band - by the
                // ground line, appropriately - with the hop clamped so its
                // *peak* never lifts the dome off the top of the panel: the
                // generic `y` band above only promises the ghost clears the
                // top while sitting still, not after adding a hop on top.
                let ground = y_hi;
                let max_hop = (ground - shapes[0].r - 1.0).max(1.0);
                let hop_amp = (shapes[0].total_height() * self.rng.range(0.8, 1.3) * (0.6 + 0.6 * bounce)).min(max_hop);
                let plan = Plan { shape: shapes[0], flip, y: ground, amp: hop_amp, freq: bounce, bounces, delay: 0.0 };
                (format!("bounce x{} {}", bounces as u32, side_word(flip)), vec!["kind:bounce".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Peek => {
                let peek_depth = shapes[0].r * self.rng.range(1.4, 2.4);
                let dur = self.rng.range(2.2, 3.6) as f64 / f64::from(pace);
                let plan = Plan { shape: shapes[0], flip, y, amp: peek_depth, freq: 0.0, bounces: 0.0, delay: 0.0 };
                (format!("peek, {}", side_word(flip)), vec!["kind:peek".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Swoop => {
                // A top-corner entrance: the shoulder starts just clear of the
                // top and dips down by `arc`, so the dip has to fit between
                // there and the bottom, hem included.
                let top = shapes[0].r + 1.5;
                let max_arc = (y_hi + hem_reach - top).max(1.0);
                let arc = (self.rng.range(0.4, 0.7) * (crate::frame::H as f32 - tallest)).min(max_arc);
                let dur = (width / (base_speed * 1.3)) as f64;
                let plan = Plan { shape: shapes[0], flip, y: top, amp: arc, freq: bounce, bounces: 0.0, delay: 0.0 };
                (format!("swoop, {}", side_word(flip)), vec!["kind:swoop".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Cross => {
                let dur = (width / base_speed) as f64 * self.rng.range(0.95, 1.15) as f64;
                // The second ghost a little further down, so two silhouettes
                // cross rather than one passing through itself - clamped to
                // the same safe band `y` itself was drawn from, since a
                // shape's own height offset can otherwise push it past the
                // hem's clearance at the bottom of the panel.
                let plans = shapes
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let y = if i == 1 { (y + s.total_height() * 0.35).min(y_hi) } else { y };
                        Plan { shape: *s, flip, y, amp: bob_amp, freq: bob_freq, bounces: 0.0, delay: 0.0 }
                    })
                    .collect();
                ("cross, opposite edges".to_string(), vec!["kind:cross".into()], plans, dur, dur)
            }
            Kind::Chase => {
                let dur = (width / (base_speed * 1.1)) as f64;
                // A real head start, not a shadow riding on the leader's
                // heels: enough that the two read as chaser and chased rather
                // than one wide silhouette.
                let delay = self.rng.range(0.7, 1.5) as f64 / f64::from(pace);
                let plans = shapes
                    .iter()
                    .enumerate()
                    .map(|(i, s)| Plan {
                        shape: *s,
                        flip,
                        y: if i == 1 { (y + s.total_height() * 0.5).min(y_hi) } else { y },
                        amp: bob_amp,
                        freq: bob_freq,
                        bounces: 0.0,
                        delay,
                    })
                    .collect();
                (format!("chase, {}", side_word(flip)), vec!["kind:chase".into(), side_tag(flip)], plans, dur, dur + delay)
            }
        };

        Act { kind, start, duration, travel_dur, name, tags, plans }
    }
}

fn side_word(flip: bool) -> &'static str {
    if flip {
        "right to left"
    } else {
        "left to right"
    }
}

fn side_tag(flip: bool) -> String {
    format!("side:{}", side_word(flip))
}

fn drift_name(flip: bool) -> String {
    format!("drift, {}", side_word(flip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Params;

    fn ctx(t: f64, params: &Params) -> Ctx<'_> {
        Ctx { t, dt: 1.0 / 30.0, now: 0.0, params }
    }

    fn params(set: &[(&str, f32)]) -> Params {
        let mut p = Params::defaults(super::super::PARAMS);
        for (k, v) in set {
            assert!(p.set(super::super::PARAMS, k, *v), "no parameter `{k}`");
        }
        p
    }

    /// The same seed, asked for the same instant by different step sizes,
    /// plans the identical schedule: nothing here reads how many times or how
    /// finely `advance` has been called, only `t` itself.
    #[test]
    fn the_schedule_does_not_depend_on_step_size() {
        let p = params(&[]);
        let coarse = {
            let mut d = Director::new(7);
            let mut t = 0.0;
            while t < 120.0 {
                d.advance(t, &ctx(t, &p));
                t += 1.0 / 3.0;
            }
            d.poses_at(119.5)
        };
        let fine = {
            let mut d = Director::new(7);
            let mut t = 0.0;
            while t < 120.0 {
                d.advance(t, &ctx(t, &p));
                t += 1.0 / 97.0;
            }
            d.poses_at(119.5)
        };
        let key = |poses: &[Pose]| -> Vec<(i64, i64)> { poses.iter().map(|p| ((p.cx * 1000.0) as i64, (p.cy * 1000.0) as i64)).collect() };
        assert_eq!(key(&coarse), key(&fine));
    }

    /// Every ghost that is ever on screen was, at some moment before it
    /// arrived and some moment after it left, entirely off the panel: it
    /// entered, and it left again.
    #[test]
    fn every_ghost_that_enters_eventually_leaves() {
        let p = params(&[("ghosts", 4.0)]);
        let mut d = Director::new(42);
        let mut t = 0.0;
        while t < 300.0 {
            d.advance(t, &ctx(t, &p));
            t += 1.0 / 30.0;
        }
        for act in &d.acts {
            // A hair inside the act's own window, on both edges, so a chased
            // follower's late start (which reports NAN before it) is skipped
            // rather than asserted about.
            let start_poses = act.poses_at(act.start + 1e-3);
            let end_poses = act.poses_at((act.start + act.duration - 1e-3).max(act.start));
            for pose in start_poses.iter().chain(end_poses.iter()) {
                let margin = pose.shape.margin();
                let clear = pose.cx < -margin + 0.5 || pose.cx > W as f32 + margin - 0.5;
                assert!(clear, "{}: ghost at x={} (margin {margin}) is on screen at an edge of its act", act.name, pose.cx);
            }
        }
    }

    /// Never more ghosts on screen at once than the `ghosts` param allows.
    #[test]
    fn never_more_ghosts_than_the_cap() {
        for cap in [1.0, 2.0, 3.0, 4.0] {
            let p = params(&[("ghosts", cap)]);
            let mut d = Director::new(9);
            let mut t = 0.0;
            while t < 180.0 {
                d.advance(t, &ctx(t, &p));
                let n = d.poses_at(t).len();
                assert!(n as f32 <= cap, "cap {cap}: {n} ghosts at t={t}");
                t += 0.5;
            }
        }
    }

    /// A long run keeps finding new combinations rather than settling into a
    /// loop: many distinct move kinds get used, not just the freshest one
    /// over and over, and every kind gets a real share of a ten-minute run.
    #[test]
    fn a_long_run_does_not_settle_into_a_loop() {
        let p = params(&[]);
        let mut d = Director::new(2026);
        let mut t = 0.0;
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        while t < 600.0 {
            d.advance(t, &ctx(t, &p));
            t += 0.25;
        }
        for act in &d.acts {
            *counts.entry(act.kind.name()).or_default() += 1;
        }
        eprintln!("10 minutes: {counts:?}");
        assert!(counts.len() >= 5, "only {} of {KINDS} kinds used in ten minutes: {counts:?}", counts.len());
        let total: usize = counts.values().sum();
        assert!(total > 20, "only {total} acts in ten minutes");
        let rarest = *counts.values().min().unwrap_or(&0);
        assert!(rarest as f32 / total as f32 > 0.03, "a kind is being neglected: {counts:?}");
        // No exact repeat back to back too often: the freshness scoring
        // should keep the same move from following itself most of the time.
        let seq: Vec<&str> = d.acts.iter().map(|a| a.kind.name()).collect();
        let repeats = seq.windows(2).filter(|w| w[0] == w[1]).count();
        assert!((repeats as f32 / seq.len() as f32) < 0.35, "{repeats} of {} acts repeated the last kind", seq.len());
    }
}
