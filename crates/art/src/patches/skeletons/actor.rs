//! One skeleton's choreography: what it is doing, where that puts it on the
//! floor, and what to do next when the current thing runs out. [`World`]
//! owns every actor in the box and is the only thing that knows about more
//! than one of them - a duet or a startle is `World` reaching into two
//! actors' queues, never an actor reaching into another's.

use super::actions;
use super::box_scene::{WALK_FAR_Z, WALK_HALF_W, WALK_NEAR_Z};
use super::rig::Pose;
use crate::rng::Rng;
use crate::variety::Variety;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Idle,
    Walk,
    Dance,
    Wave,
    PeerHold,
    Sit,
    Rattle,
    HeadPop,
    Startle,
    WaveTogether,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Idle => "standing",
            Kind::Walk => "walking",
            Kind::Dance => "dancing",
            Kind::Wave => "waving",
            Kind::PeerHold => "peering in",
            Kind::Sit => "sitting",
            Kind::Rattle => "shivering",
            Kind::HeadPop => "losing its head",
            Kind::Startle => "startled",
            Kind::WaveTogether => "waving at the other one",
        }
    }
}

const MOTIFS: &[&str] = &["wander", "dance", "wave", "peer", "sit", "rattle", "headpop"];

/// Metres a second: a leisurely amble, not a march.
const WALK_SPEED: f32 = 0.55;
/// Radians a second of gait phase: about one stride a second and a bit.
const GAIT_OMEGA: f32 = 3.6;
const DANCE_OMEGA: f32 = 3.4;

/// How close two skeletons may come before they are pushed apart - closer
/// than a high five needs, not so close they merge into one shape.
pub const MIN_SEP: f32 = 0.32;
const STARTLE_RADIUS: f32 = 1.15;

/// Which motif comes next: the least-worn of [`MOTIFS`], with a little
/// jitter so it is not a fixed rotation. A pure function of the wear so far,
/// the same shape as `clocks::dance::compose` - kept apart from the state
/// mutation in [`Actor::choose_next`] so a long run of choices can be tested
/// on its own, the way `dance.rs` tests a day of dances.
fn pick_motif(variety: &Variety, rng: &mut Rng) -> String {
    let names: Vec<String> = MOTIFS.iter().map(|s| s.to_string()).collect();
    variety.freshest(&names, || rng.f32()).cloned().unwrap_or_else(|| "wander".to_string())
}

fn heading_towards(from: (f32, f32), to: (f32, f32)) -> f32 {
    let (dx, dz) = (to.0 - from.0, to.1 - from.1);
    if dx.hypot(dz) < 1e-4 {
        return 0.0;
    }
    dx.atan2(-dz)
}

struct Planned {
    kind: Kind,
    target: Option<(f32, f32)>,
    dur: Option<f64>,
}

struct Current {
    kind: Kind,
    start: f64,
    dur: f64,
    from: (f32, f32),
    to: (f32, f32),
    side: usize,
}

pub struct Actor {
    pub pos: (f32, f32),
    pub heading: f32,
    pub height: f32,
    rng: Rng,
    variety: Variety,
    current: Current,
    queue: VecDeque<Planned>,
}

fn random_point(rng: &mut Rng) -> (f32, f32) {
    (rng.range(-WALK_HALF_W, WALK_HALF_W), rng.range(WALK_NEAR_Z, WALK_FAR_Z))
}

impl Actor {
    pub fn new(seed: u64) -> Actor {
        let mut rng = Rng::new(seed);
        let pos = random_point(&mut rng);
        let height = rng.range(1.45, 1.85);
        Actor {
            pos,
            heading: rng.range(-std::f32::consts::PI, std::f32::consts::PI),
            height,
            rng,
            variety: Variety::default(),
            // Expired on arrival: the first `advance` call picks a real motif.
            current: Current { kind: Kind::Idle, start: 0.0, dur: 0.0, from: pos, to: pos, side: 0 },
            queue: VecDeque::new(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.current.kind
    }

    fn just_started(&self, t: f64) -> bool {
        self.current.start == t
    }

    fn start(&mut self, t: f64, planned: Planned) {
        let from = self.pos;
        let to = planned.target.unwrap_or(from);
        let dist = (to.0 - from.0).hypot(to.1 - from.1);
        let dur = match planned.kind {
            Kind::Walk => f64::from((dist / WALK_SPEED).max(0.6)),
            _ => planned.dur.unwrap_or(2.0),
        };
        if planned.kind == Kind::Walk && dist > 1e-3 {
            self.heading = heading_towards(from, to);
        } else if matches!(planned.kind, Kind::Wave | Kind::PeerHold | Kind::Sit | Kind::Dance) {
            // Face the one thing in the box worth performing for.
            self.heading = 0.0;
        }
        let side = (self.rng.u64() % 2) as usize;
        self.current = Current { kind: planned.kind, start: t, dur, from, to, side };
    }

    /// Force whatever this actor was doing aside for a scripted sequence -
    /// [`World`]'s hand, for a duet or a startle. Anything already queued is
    /// dropped: a flinch does not wait its turn.
    fn force(&mut self, t: f64, seq: Vec<Planned>) {
        self.queue = seq.into();
        if let Some(first) = self.queue.pop_front() {
            self.start(t, first);
        }
    }

    fn choose_next(&mut self, t: f64) {
        let pick = pick_motif(&self.variety, &mut self.rng);
        self.variety.note(&pick, std::slice::from_ref(&pick));
        let mut rng = std::mem::replace(&mut self.rng, Rng::new(0));
        let seq = match pick.as_str() {
            "dance" => vec![Planned { kind: Kind::Dance, target: None, dur: Some(f64::from(rng.range(5.0, 9.0))) }],
            "wave" => vec![Planned { kind: Kind::Wave, target: None, dur: Some(f64::from(rng.range(2.2, 3.6))) }],
            "peer" => {
                let near = (rng.range(-0.30, 0.30), WALK_NEAR_Z + rng.range(0.0, 0.05));
                let after = random_point(&mut rng);
                vec![
                    Planned { kind: Kind::Walk, target: Some(near), dur: None },
                    Planned { kind: Kind::PeerHold, target: None, dur: Some(f64::from(rng.range(2.2, 3.2))) },
                    Planned { kind: Kind::Walk, target: Some(after), dur: None },
                ]
            }
            "sit" => vec![Planned { kind: Kind::Sit, target: None, dur: Some(f64::from(rng.range(4.0, 6.5))) }],
            "rattle" => vec![Planned { kind: Kind::Rattle, target: None, dur: Some(f64::from(rng.range(1.4, 2.4))) }],
            "headpop" => vec![Planned { kind: Kind::HeadPop, target: None, dur: Some(f64::from(rng.range(3.2, 3.8))) }],
            _ => vec![
                Planned { kind: Kind::Walk, target: Some(random_point(&mut rng)), dur: None },
                Planned { kind: Kind::Idle, target: None, dur: Some(f64::from(rng.range(1.5, 4.0))) },
            ],
        };
        self.rng = rng;
        self.queue = seq.into();
        let first = self.queue.pop_front().expect("every motif plans at least one step");
        self.start(t, first);
    }

    /// Advance the choreography: pop the queue or choose the next motif once
    /// the current step has run its course.
    fn advance(&mut self, t: f64) {
        if t < self.current.start + self.current.dur {
            return;
        }
        if let Some(next) = self.queue.pop_front() {
            self.start(t, next);
        } else {
            self.choose_next(t);
        }
    }

    /// This step's position, before any other actor is taken into account:
    /// a walk in flight, or wherever the current stationary action is
    /// standing.
    fn desired_pos(&self, t: f64) -> (f32, f32) {
        if self.current.kind != Kind::Walk {
            return self.current.to;
        }
        let u = (((t - self.current.start) / self.current.dur.max(1e-6)) as f32).clamp(0.0, 1.0);
        let e = u * u * (3.0 - 2.0 * u);
        (self.current.from.0 + (self.current.to.0 - self.current.from.0) * e, self.current.from.1 + (self.current.to.1 - self.current.from.1) * e)
    }

    fn local(&self, t: f64) -> f32 {
        (t - self.current.start) as f32
    }

    /// This frame's pose, everything but the head-pop's own displacement
    /// (see [`Actor::head_off`], applied post-FK by the caller). `sway`
    /// scales how loose the idle and rattling motion is (the patch's own
    /// parameter); nothing else reads it.
    #[must_use]
    pub fn pose(&self, t: f64, sway: f32) -> Pose {
        let local = self.local(t);
        match self.current.kind {
            Kind::Idle => actions::idle(local, 0.6 * sway),
            Kind::Walk => {
                let speed = if self.current.dur > 0.05 {
                    let dist = (self.current.to.0 - self.current.from.0).hypot(self.current.to.1 - self.current.from.1);
                    (f64::from(dist) / self.current.dur / f64::from(WALK_SPEED)).clamp(0.35, 1.6) as f32
                } else {
                    1.0
                };
                actions::walk(local * GAIT_OMEGA * speed, speed.min(1.0), 0.0)
            }
            Kind::Dance => actions::dance(local * DANCE_OMEGA),
            Kind::Wave | Kind::WaveTogether => actions::wave(local, self.current.side),
            Kind::PeerHold => actions::peer(local),
            Kind::Sit => actions::sit(local),
            Kind::Rattle => actions::jitter(actions::idle(local, 0.2 * sway), local, 0.16 * sway.max(0.15)),
            Kind::HeadPop => actions::jitter(actions::idle(local, 0.15 * sway), local, 0.03),
            Kind::Startle => actions::startle((local / self.current.dur.max(0.01) as f32).clamp(0.0, 1.0)),
        }
    }

    /// `0..1`: how far the head has left the neck, for [`Kind::HeadPop`].
    /// `0` for every other action, so the caller can apply it unconditionally.
    #[must_use]
    pub fn head_off(&self, t: f64) -> f32 {
        if self.current.kind != Kind::HeadPop {
            return 0.0;
        }
        let u = (self.local(t) / self.current.dur.max(0.01) as f32).clamp(0.0, 1.0);
        // Up over the first quarter, held aloft (with a small bob) over the
        // middle half, back down over the last quarter.
        let rise = super::box_scene::smooth(0.0, 0.22, u);
        let fall = 1.0 - super::box_scene::smooth(0.78, 1.0, u);
        rise * fall
    }

    pub fn playing_label(&self) -> &'static str {
        self.current.kind.label()
    }
}

/// Everyone in the box. The only thing that knows there is more than one
/// skeleton: an [`Actor`] never reads another's state.
pub struct World {
    pub actors: Vec<Actor>,
    rng: Rng,
    next_duet: f64,
}

impl World {
    pub fn new(seed: u64) -> World {
        let mut rng = Rng::new(seed ^ 0xd06_5eed);
        World { actors: Vec::new(), rng: Rng::new(seed ^ 0x5ca1_ab1e), next_duet: f64::from(rng.range(20.0, 50.0)) }
    }

    fn resize(&mut self, want: usize, seed: u64) {
        while self.actors.len() < want {
            let i = self.actors.len() as u64;
            self.actors.push(Actor::new(seed.wrapping_add(0x9e37_79b9 * (i + 1))));
        }
        self.actors.truncate(want.max(1));
    }

    /// One fixed step, `dt` seconds, at engine time `t` (after the step).
    pub fn step(&mut self, t: f64, count: usize, seed: u64) {
        self.resize(count, seed);
        for a in &mut self.actors {
            a.advance(t);
        }

        // A duet: two skeletons walk in from opposite sides of a shared
        // point and wave at each other, which needs no explicit "face your
        // partner" logic - walking towards the same point from either side
        // already leaves them looking at one another.
        if self.actors.len() >= 2 && t >= self.next_duet {
            let calm = |a: &Actor| !matches!(a.kind(), Kind::Startle | Kind::HeadPop | Kind::WaveTogether);
            if calm(&self.actors[0]) && calm(&self.actors[1]) {
                let mid = (0.0, (WALK_NEAR_Z + WALK_FAR_Z) * 0.42);
                self.actors[0].force(
                    t,
                    vec![
                        Planned { kind: Kind::Walk, target: Some((mid.0 - 0.28, mid.1)), dur: None },
                        Planned { kind: Kind::WaveTogether, target: None, dur: Some(3.2) },
                    ],
                );
                self.actors[1].force(
                    t,
                    vec![
                        Planned { kind: Kind::Walk, target: Some((mid.0 + 0.28, mid.1)), dur: None },
                        Planned { kind: Kind::WaveTogether, target: None, dur: Some(3.2) },
                    ],
                );
                self.next_duet = t + f64::from(self.rng.range(45.0, 95.0));
            }
        }

        // A startle: one skeleton doing something sudden nearby is worth a
        // flinch from whoever is just standing there, some of the time.
        let snapshot: Vec<(f32, f32, bool)> = self.actors.iter().map(|a| (a.pos.0, a.pos.1, a.just_started(t) && matches!(a.kind(), Kind::Rattle | Kind::HeadPop))).collect();
        for j in 0..self.actors.len() {
            if self.actors[j].kind() != Kind::Idle {
                continue;
            }
            let (jx, jz) = self.actors[j].pos;
            let startled = snapshot.iter().enumerate().any(|(i, &(ix, iz, sudden))| i != j && sudden && (ix - jx).hypot(iz - jz) < STARTLE_RADIUS);
            if startled && self.rng.f32() < 0.7 {
                self.actors[j].force(t, vec![Planned { kind: Kind::Startle, target: None, dur: Some(1.0) }]);
            }
        }

        let desired: Vec<(f32, f32)> = self.actors.iter().map(|a| a.desired_pos(t)).collect();
        let resolved = resolve(desired);
        for (a, p) in self.actors.iter_mut().zip(resolved) {
            a.pos = p;
        }
    }
}

/// Push apart anyone nearer than [`MIN_SEP`], then keep everyone inside the
/// walkable floor. Two passes: pushing away from a neighbour can put a
/// skeleton against the wall, or against a third skeleton, on the first.
fn resolve(mut pos: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    for _ in 0..4 {
        for i in 0..pos.len() {
            for j in (i + 1)..pos.len() {
                let (dx, dz) = (pos[j].0 - pos[i].0, pos[j].1 - pos[i].1);
                let d = dx.hypot(dz).max(1e-4);
                if d < MIN_SEP {
                    let push = (MIN_SEP - d) * 0.5;
                    let (ux, uz) = (dx / d, dz / d);
                    pos[i].0 -= ux * push;
                    pos[i].1 -= uz * push;
                    pos[j].0 += ux * push;
                    pos[j].1 += uz * push;
                }
            }
        }
        for p in &mut pos {
            p.0 = p.0.clamp(-WALK_HALF_W, WALK_HALF_W);
            p.1 = p.1.clamp(WALK_NEAR_Z, WALK_FAR_Z);
        }
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// A long run of motif choices: every motif in the vocabulary comes up,
    /// none of them dominates, and nothing gets stuck repeating itself -
    /// `clocks::dance`'s "a day of dances does not repeat itself", at this
    /// patch's own grain (a motif, not a single shape).
    #[test]
    fn a_long_run_of_motifs_does_not_settle_into_a_loop() {
        let mut variety = Variety::default();
        let mut rng = Rng::new(2026);
        let mut picks = Vec::new();
        for _ in 0..2000 {
            let pick = pick_motif(&variety, &mut rng);
            variety.note(&pick, std::slice::from_ref(&pick));
            picks.push(pick);
        }
        let distinct: BTreeSet<&String> = picks.iter().collect();
        assert_eq!(distinct.len(), MOTIFS.len(), "every motif should turn up in 2000 choices: {distinct:?}");

        let mut streak = 1usize;
        let mut longest = 1usize;
        for i in 1..picks.len() {
            if picks[i] == picks[i - 1] {
                streak += 1;
                longest = longest.max(streak);
            } else {
                streak = 1;
            }
        }
        assert!(longest <= 3, "the same motif repeated {longest} times running");

        let mut counts: BTreeMap<&String, usize> = BTreeMap::new();
        for p in &picks {
            *counts.entry(p).or_default() += 1;
        }
        let (lo, hi) = (*counts.values().min().unwrap(), *counts.values().max().unwrap());
        assert!(lo as f32 / hi as f32 > 0.25, "a motif is neglected against the rest: {counts:?}");
    }

    #[test]
    fn a_new_actor_leaves_its_placeholder_action_on_the_first_step() {
        // The placeholder `Current` an actor is born with expires at once
        // (`dur: 0.0`), and no motif's first step is `Idle` - `wander`'s
        // only reaches it after a walk - so a real motif has begun by the
        // first fixed step.
        let mut a = Actor::new(1);
        a.advance(1.0 / 30.0);
        assert_ne!(a.kind(), Kind::Idle, "still on the placeholder after the first step");
    }
}
