//! What the hands do when they are not telling the time: slow, smooth, never
//! quite regular fields of motion, after the original's ambient choreography.
//!
//! Unlike a dance, this cannot be planned as moves, because the target never
//! stops: every clock follows a field that is itself drifting. So each hand is
//! a small servo chasing its target under the same motor limits as the dances,
//! with a braking curve so it arrives without overshooting.
//!
//! The field has a *mood*: a handful of numbers. Moods never switch; the
//! numbers glide from one mood's to the next, and every oscillator keeps its
//! own running phase, so the motion is continuous through any change. `vigor`
//! scales the field's clock, which is how a grid eases into motion and back to
//! rest.
//!
//! Positions are measured in units of 8 LEDs, whatever the grid, so a mood has
//! the same wavelengths on the panel for 8 clocks as for 32.

use super::dance::{shortest, Motor};
use super::Hands;
use crate::rng::Rng;

/// A travelling sine. `k` is radians per 8 LEDs, `w` radians per second.
#[derive(Clone, Copy, Debug, Default)]
struct Ripple {
    amp: f32,
    k: (f32, f32),
    w: f32,
}

impl Ripple {
    /// The same thing `Ripple::default()` is, where a `const` is needed.
    const ZERO: Ripple = Ripple { amp: 0.0, k: (0.0, 0.0), w: 0.0 };
}

#[derive(Clone, Copy, Debug)]
pub struct Mood {
    pub name: &'static str,
    /// Degrees per second the whole field turns.
    turn: f32,
    /// Two plane waves of unrelated wavelength, so rows and columns never line
    /// up into anything as tidy as one sine.
    ripples: [Ripple; 2],
    /// Rings spreading from a focus that wanders about the panel. `k.0` is
    /// radians per 8 LEDs of distance.
    rings: Ripple,
    /// How far apart a clock's two hands are, from and to, in degrees...
    open: (f32, f32),
    /// ...and the wave that moves them between the two.
    open_wave: Ripple,
}

pub const MOODS: usize = 8;

/// What each mood is called, in the order [`Mood::new`] builds them.
///
/// Card 182: **the one list**. `Mood::new` puts the name on from here rather
/// than each arm carrying a literal, and `dials::MOOD_CHOICES` - the `mood`
/// parameter's named stops - is built from it.
pub const MOOD_NAMES: [&str; MOODS] =
    ["drift", "sway", "breathe", "corners", "unison", "tide", "rings", "streamlines"];

impl Mood {
    /// Nothing moving. The arms of [`Mood::new`] say only what they change, and
    /// the name is put on at the end, so this one is never seen.
    const STILL: Mood = Mood {
        name: "",
        turn: 0.0,
        ripples: [Ripple::ZERO; 2],
        rings: Ripple::ZERO,
        open: (0.0, 0.0),
        open_wave: Ripple::ZERO,
    };

    /// `which` is 0..MOODS; the numbers within a mood are drawn afresh each time.
    pub fn new(which: usize, rng: &mut Rng) -> Mood {
        let mut ripple = |amp: (f32, f32), k: (f32, f32), w: (f32, f32)| Ripple {
            amp: rng.range(amp.0, amp.1),
            k: (rng.range(k.0, k.1) * rng.sign(), rng.range(0.2, 0.9) * rng.sign()),
            w: rng.range(w.0, w.1) * rng.sign(),
        };
        let none = Ripple::default();
        let folded = (5.0, 9.0);
        let which = which % MOODS;
        let mood = match which {
            // Folded needles turning steadily, each a little ahead of its neighbour.
            0 => Mood {
                turn: 22.0,
                ripples: [ripple((35.0, 70.0), (0.35, 0.7), (0.15, 0.4)), ripple((10.0, 25.0), (0.9, 1.4), (0.3, 0.6))],
                open: folded,
                ..Mood::STILL
            },
            // Needles nodding like grass in a current; the field does not turn.
            1 => Mood {
                turn: 0.0,
                ripples: [ripple((30.0, 60.0), (0.4, 0.8), (0.5, 0.9)), ripple((10.0, 22.0), (1.0, 1.6), (0.7, 1.1))],
                open: folded,
                ..Mood::STILL
            },
            // Needles open into chevrons and lines and close again, in waves.
            2 => Mood {
                turn: 9.0,
                ripples: [ripple((25.0, 50.0), (0.3, 0.6), (0.2, 0.4)), ripple((8.0, 18.0), (0.8, 1.3), (0.3, 0.5))],
                open: (6.0, 180.0),
                open_wave: ripple((1.0, 1.0), (0.35, 0.6), (0.35, 0.55)),
                ..Mood::STILL
            },
            // Rigid right-angle corners, carried round by a broad wave.
            3 => Mood {
                turn: 18.0,
                ripples: [ripple((60.0, 110.0), (0.3, 0.55), (0.15, 0.3)), ripple((10.0, 20.0), (0.9, 1.3), (0.3, 0.5))],
                open: (90.0, 90.0),
                ..Mood::STILL
            },
            // Everything agrees: parallel lines, turning as one.
            4 => Mood {
                turn: 14.0,
                ripples: [ripple((0.0, 4.0), (0.3, 0.5), (0.2, 0.3)), none],
                open: (180.0, 180.0),
                ..Mood::STILL
            },
            // One long slow wave through open lines.
            5 => Mood {
                turn: 0.0,
                ripples: [ripple((50.0, 80.0), (0.25, 0.4), (0.25, 0.4)), ripple((5.0, 12.0), (0.7, 1.0), (0.2, 0.4))],
                open: (150.0, 180.0),
                open_wave: ripple((1.0, 1.0), (0.2, 0.35), (0.15, 0.25)),
                ..Mood::STILL
            },
            // Rings spreading from a focus that wanders about the panel.
            6 => Mood {
                turn: 4.0,
                ripples: [ripple((8.0, 16.0), (0.4, 0.7), (0.2, 0.4)), none],
                rings: Ripple { amp: 90.0, k: (0.9, 0.0), w: 0.65 },
                open: (20.0, 70.0),
                open_wave: ripple((1.0, 1.0), (0.3, 0.5), (0.2, 0.35)),
                ..Mood::STILL
            },
            // Open lines on a field so gentle that neighbours join end to end
            // into long curves across the whole panel.
            _ => Mood {
                turn: 5.0,
                ripples: [ripple((30.0, 50.0), (0.18, 0.3), (0.12, 0.22)), ripple((6.0, 12.0), (0.45, 0.7), (0.15, 0.3))],
                open: (180.0, 180.0),
                ..Mood::STILL
            },
        };
        Mood { name: MOOD_NAMES[which], turn: mood.turn * rng.range(0.75, 1.25) * rng.sign(), ..mood }
    }

    /// Glide a fraction `f` of the way to `goal`.
    fn toward(&mut self, goal: &Mood, f: f32) {
        let mix = |a: &mut f32, b: f32| *a += (b - *a) * f;
        let ripple = |a: &mut Ripple, b: &Ripple| {
            *a = Ripple {
                amp: a.amp + (b.amp - a.amp) * f,
                k: (a.k.0 + (b.k.0 - a.k.0) * f, a.k.1 + (b.k.1 - a.k.1) * f),
                w: a.w + (b.w - a.w) * f,
            }
        };
        self.name = goal.name;
        mix(&mut self.turn, goal.turn);
        ripple(&mut self.ripples[0], &goal.ripples[0]);
        ripple(&mut self.ripples[1], &goal.ripples[1]);
        ripple(&mut self.rings, &goal.rings);
        ripple(&mut self.open_wave, &goal.open_wave);
        mix(&mut self.open.0, goal.open.0);
        mix(&mut self.open.1, goal.open.1);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Servo {
    velocity: f32,
    /// Whole turns between the target's angle and this hand's, fixed when the
    /// hand is released so it never unwinds the long way round.
    offset: f32,
    released: bool,
}

/// Running phase of every oscillator, in radians (heading in degrees). Kept
/// as state, not computed from the time, so rates can change without a jump.
#[derive(Clone, Copy, Debug, Default)]
struct Phases {
    heading: f32,
    ripples: [f32; 2],
    rings: f32,
    open: f32,
    wander: f32,
}

pub struct Ambient {
    mood: Mood,
    goal: Mood,
    phases: Phases,
    vigor: f32,
    /// Seconds this has been running, in real time.
    age: f32,
    /// Centre of each clock, in units of 8 LEDs.
    places: Vec<(f32, f32)>,
    extent: (f32, f32),
    servos: Vec<[Servo; 2]>,
    /// Which hand of each clock takes which side of the opening.
    swapped: Vec<bool>,
    /// Fixed per-clock error, in degrees: the original is never quite regular.
    wobble: Vec<f32>,
    reverse: bool,
    /// 0..1: how firmly a held pose (see `step_holding`) has taken over from
    /// the field.
    grip: f32,
    /// The held pose, as the turn-for-turn equivalent nearest each hand.
    held: Vec<Hands>,
    /// Where each hand was last told to be, for feed-forward.
    told: Vec<Option<Hands>>,
}

impl Ambient {
    /// A field for a `cols` x `rows` grid of clocks `cell` LEDs across, in mood
    /// `which` (see [`Mood::new`]).
    pub fn new(rng: &mut Rng, which: usize, cols: usize, rows: usize, cell: f32) -> Ambient {
        let n = cols * rows;
        let unit = cell / 8.0;
        let mood = Mood::new(which, rng);
        Ambient {
            mood,
            goal: mood,
            phases: Phases {
                heading: rng.range(0.0, 360.0),
                ripples: [rng.range(0.0, std::f32::consts::TAU), rng.range(0.0, std::f32::consts::TAU)],
                rings: rng.range(0.0, std::f32::consts::TAU),
                open: rng.range(0.0, std::f32::consts::TAU),
                wander: rng.range(0.0, 100.0),
            },
            vigor: 0.0,
            age: 0.0,
            places: (0..n).map(|i| (((i % cols) as f32 + 0.5) * unit, ((i / cols) as f32 + 0.5) * unit)).collect(),
            extent: (cols as f32 * unit, rows as f32 * unit),
            servos: vec![[Servo::default(); 2]; n],
            swapped: vec![false; n],
            wobble: (0..n).map(|_| rng.range(-4.0, 4.0)).collect(),
            reverse: rng.u64() & 1 == 0,
            grip: 0.0,
            held: vec![[0.0; 2]; n],
            told: vec![None; n],
        }
    }

    pub fn name(&self) -> &'static str {
        self.goal.name
    }

    /// Start gliding towards another mood.
    pub fn drift_to(&mut self, which: usize, rng: &mut Rng) {
        self.goal = Mood::new(which, rng);
    }

    /// Start gliding towards a mood that is mostly `which` with `amount` (0..1)
    /// of `other` mixed in. A mood is only a handful of numbers, so any blend
    /// of two is a mood too: the eight named ones are landmarks in a continuous
    /// space, not the whole of it.
    pub fn drift_to_blend(&mut self, which: usize, other: usize, amount: f32, rng: &mut Rng) {
        let mut goal = Mood::new(which, rng);
        let name = goal.name;
        goal.toward(&Mood::new(other, rng), amount.clamp(0.0, 1.0));
        self.goal = Mood { name, ..goal };
    }

    fn target(&self, i: usize, p: &Phases) -> Hands {
        let (x, y) = self.places[i];
        let m = &self.mood;
        let wave = |r: &Ripple, phase: f32| (r.k.0 * x + r.k.1 * y + phase).sin();
        // The focus wanders on a slow Lissajous path inside the panel.
        let focus = (
            self.extent.0 * (0.5 + 0.38 * (p.wander * 0.11).sin()),
            self.extent.1 * (0.5 + 0.35 * (p.wander * 0.17).cos()),
        );
        let distance = ((x - focus.0).powi(2) + (y - focus.1).powi(2)).sqrt();
        let angle = p.heading
            + m.ripples[0].amp * wave(&m.ripples[0], p.ripples[0])
            + m.ripples[1].amp * wave(&m.ripples[1], p.ripples[1])
            + m.rings.amp * (m.rings.k.0 * distance - p.rings).sin()
            + self.wobble[i];
        let s = 0.5 + 0.5 * wave(&m.open_wave, p.open);
        let open = m.open.0 + (m.open.1 - m.open.0) * s * s;
        if self.swapped[i] {
            [angle + open * 0.5, angle - open * 0.5]
        } else {
            [angle - open * 0.5, angle + open * 0.5]
        }
    }

    /// How firmly a held pose has taken over, eased: 0 free, 1 held.
    pub fn grip(&self) -> f32 {
        self.grip * self.grip * (3.0 - 2.0 * self.grip)
    }

    /// Advance by `dt` seconds. With `settle` set, the field slows to a stop.
    pub fn step(&mut self, angles: &mut [Hands], motor: Motor, dt: f32, settle: bool) {
        self.step_holding(angles, motor, dt, settle, None)
    }

    /// As `step`, but while `pose` is given the field slows and every hand is
    /// drawn to that pose instead, arriving exactly; when it is withdrawn the
    /// hands are let back into the field. This is how a flowing grid pauses to
    /// say something, such as the time.
    pub fn step_holding(&mut self, angles: &mut [Hands], motor: Motor, dt: f32, settle: bool, pose: Option<&[Hands]>) {
        if dt <= 0.0 {
            return;
        }
        let settle = settle || pose.is_some();
        let taking_hold = pose.is_some() && self.grip <= 0.0;
        self.grip = (self.grip + if pose.is_some() { dt / 3.5 } else { -dt / 3.0 }).clamp(0.0, 1.0);
        let grip = self.grip();
        self.age += dt;
        let goal = if settle { 0.0 } else { 1.0 };
        self.vigor += (goal - self.vigor).clamp(-dt / 1.5, dt / 2.5);
        let goal_mood = self.goal;
        self.mood.toward(&goal_mood, 1.0 - (-dt / 7.0).exp());

        let step = dt * self.vigor;
        let m = self.mood;
        self.phases.heading += m.turn * step;
        self.phases.ripples[0] += m.ripples[0].w * step;
        self.phases.ripples[1] += m.ripples[1].w * step;
        self.phases.rings += m.rings.w * step;
        self.phases.open += m.open_wave.w * step;
        self.phases.wander += step;
        let now = self.phases;

        for i in 0..angles.len().min(self.places.len()) {
            // Clocks join in from one side to the other rather than all at once.
            let across = if self.reverse { self.extent.0 - self.places[i].0 } else { self.places[i].0 };
            if self.age < across * 0.3 {
                continue;
            }
            if !self.servos[i][0].released {
                let t = self.target(i, &now);
                let cost = |t: Hands| (0..2).map(|h| shortest(angles[i][h], t[h]).abs()).sum::<f32>();
                self.swapped[i] = cost([t[1], t[0]]) < cost(t);
                let t = self.target(i, &now);
                for h in 0..2 {
                    let nearest = angles[i][h] + shortest(angles[i][h], t[h]);
                    self.servos[i][h] = Servo { velocity: 0.0, offset: nearest - t[h], released: true };
                }
            }
            let field = self.target(i, &now);
            let mut target = [field[0] + self.servos[i][0].offset, field[1] + self.servos[i][1].offset];
            if let Some(pose) = pose {
                for h in 0..2 {
                    // Follow the pose by the nearest way round: from the field
                    // when first taking hold, from where it was held after that.
                    let from = if taking_hold { target[h] } else { self.held[i][h] };
                    self.held[i][h] = from + shortest(from, pose[i][h]);
                }
            }
            for (h, angle) in target.iter_mut().enumerate() {
                *angle += (self.held[i][h] - *angle) * grip;
            }
            let before = self.told[i].replace(target).unwrap_or(target);
            for h in 0..2 {
                let servo = &mut self.servos[i][h];
                let feed = (target[h] - before[h]) / dt;
                let error = target[h] - angles[i][h];
                // The fastest approach that can still stop in time, softened
                // close in so it settles instead of chattering.
                let approach = (2.0 * motor.acc * error.abs()).sqrt().min(error.abs() * 5.0).min(motor.speed);
                // The motor's top speed is a hard limit: if the field outruns
                // it, the hand simply lags.
                let want = (feed + approach * error.signum()).clamp(-motor.speed, motor.speed);
                servo.velocity += (want - servo.velocity).clamp(-motor.acc * dt, motor.acc * dt);
                angles[i][h] += servo.velocity * dt;
            }
        }
    }

    /// True once every hand has all but stopped.
    pub fn at_rest(&self) -> bool {
        self.vigor <= 0.0 && self.servos.iter().flatten().all(|s| s.velocity.abs() < 1.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patches::clocks::{pose, Rest, DEFAULT_REST};

    /// From the digits, through every mood and a change of mood, and back to
    /// rest: speed stays bounded, acceleration never exceeds the motor's, and
    /// everything stops when asked.
    #[test]
    fn ambient_motion_respects_the_motor_and_comes_to_rest() {
        let motor = Motor { speed: 100.0, acc: 160.0 };
        for which in 0..MOODS {
            let mut rng = Rng::new(which as u64 * 7 + 1);
            let mut ambient = Ambient::new(&mut rng, which, 8, 3, 8.0);
            let mut angles = pose(9, 25, Rest::of(DEFAULT_REST as f32)).to_vec();
            let dt = 1.0 / 30.0;
            let mut prev = angles.clone();
            let mut prev_v = vec![[0.0_f32; 2]; angles.len()];
            for frame in 0..(60 * 30) {
                if frame == 15 * 30 {
                    ambient.drift_to(which + 3, &mut rng);
                }
                ambient.step(&mut angles, motor, dt, frame > 50 * 30);
                for i in 0..angles.len() {
                    for h in 0..2 {
                        let v = (angles[i][h] - prev[i][h]) / dt;
                        assert!(v.abs() <= motor.speed * 1.01, "{}: {v} deg/s", ambient.name());
                        assert!((v - prev_v[i][h]).abs() <= motor.acc * dt * 1.05 + 1e-3, "{}: jerked", ambient.name());
                        prev_v[i][h] = v;
                    }
                }
                prev.clone_from(&angles);
            }
            assert!(ambient.at_rest(), "{} still moving", ambient.name());
        }
    }

    /// A held pose is reached exactly, under the same limits, and let go of
    /// without a jolt.
    #[test]
    fn a_held_pose_is_reached_exactly() {
        let motor = Motor { speed: 70.0, acc: 77.0 };
        let pose = [[305.0, 252.0]; 18];
        for which in 0..MOODS {
            let mut rng = Rng::new(which as u64 + 40);
            let mut ambient = Ambient::new(&mut rng, which, 6, 3, 64.0 / 6.0);
            let mut angles = vec![[225.0_f32; 2]; 18];
            let dt = 1.0 / 30.0;
            let (mut prev, mut prev_v) = (angles.clone(), vec![[0.0_f32; 2]; 18]);
            for frame in 0..(45 * 30) {
                let holding = (12 * 30..26 * 30).contains(&frame);
                ambient.step_holding(&mut angles, motor, dt, false, holding.then_some(&pose[..]));
                for i in 0..18 {
                    for h in 0..2 {
                        let v = (angles[i][h] - prev[i][h]) / dt;
                        assert!(v.abs() <= motor.speed * 1.01, "{}: {v} deg/s", ambient.name());
                        assert!((v - prev_v[i][h]).abs() <= motor.acc * dt * 1.05 + 1e-3, "{}: jerked", ambient.name());
                        prev_v[i][h] = v;
                        if frame == 26 * 30 - 1 {
                            assert!(shortest(angles[i][h], pose[i][h]).abs() < 0.5, "{}: dial {i} hand {h} at {}", ambient.name(), angles[i][h]);
                        }
                    }
                }
                prev.clone_from(&angles);
            }
        }
    }
}
