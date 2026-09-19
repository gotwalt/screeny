//! What the hands do between telling the time: slow, smooth, never quite
//! regular fields of motion, after the original's ambient choreography.
//!
//! Unlike a dance, this cannot be planned as moves, because the target never
//! stops: every clock follows a field that is itself drifting. So each hand is
//! a small servo chasing its target under the same motor limits as the dances,
//! with a braking curve so it arrives without overshooting. `vigor` scales the
//! field's clock, which is how the whole grid eases into motion and back to
//! rest before a dance takes over.

use super::dance::Motor;
use super::{Hands, CLOCKS, COLS};
use crate::rng::Rng;

/// One travelling sine over the grid. `k` is radians per clock, `w` radians per
/// second.
#[derive(Clone, Copy, Debug)]
struct Ripple {
    amp: f32,
    k: (f32, f32),
    w: f32,
}

/// A character for the field. Angles are the sum of a slow overall turn and
/// two ripples with unrelated wavelengths, so rows and columns never line up
/// into anything as tidy as a single sine; `open` is how far the two hands of a
/// clock part, which has a wave of its own.
#[derive(Clone, Debug)]
pub struct Mood {
    pub name: &'static str,
    heading: f32,
    turn: f32,
    ripples: [Ripple; 2],
    open: (f32, f32),
    open_wave: Ripple,
    /// Fixed per-clock error, in degrees: the original is never quite regular.
    wobble: [f32; CLOCKS],
}

impl Mood {
    pub fn new(rng: &mut Rng) -> Mood {
        let sign = |rng: &mut Rng| rng.sign();
        let ripple = |rng: &mut Rng, amp: (f32, f32), k: (f32, f32), w: (f32, f32)| Ripple {
            amp: rng.range(amp.0, amp.1),
            k: (rng.range(k.0, k.1) * sign(rng), rng.range(0.2, 0.9) * sign(rng)),
            w: rng.range(w.0, w.1) * sign(rng),
        };
        let still = Ripple { amp: 0.0, k: (0.0, 0.0), w: 0.0 };
        let mut mood = match rng.u64() % 4 {
            // Folded needles turning steadily, each a little ahead of its neighbour.
            0 => Mood {
                name: "drift",
                heading: rng.range(0.0, 360.0),
                turn: rng.range(16.0, 28.0) * sign(rng),
                ripples: [ripple(rng, (35.0, 70.0), (0.35, 0.7), (0.15, 0.4)), ripple(rng, (10.0, 25.0), (0.9, 1.4), (0.3, 0.6))],
                open: (5.0, 9.0),
                open_wave: still,
                wobble: [0.0; CLOCKS],
            },
            // Near-level needles nodding like grass in a current.
            1 => Mood {
                name: "sway",
                heading: if rng.u64() & 1 == 0 { 90.0 } else { 270.0 },
                turn: 0.0,
                ripples: [ripple(rng, (30.0, 60.0), (0.4, 0.8), (0.5, 0.9)), ripple(rng, (10.0, 22.0), (1.0, 1.6), (0.7, 1.1))],
                open: (5.0, 9.0),
                open_wave: still,
                wobble: [0.0; CLOCKS],
            },
            // Needles open into chevrons and lines and close again, in waves.
            2 => Mood {
                name: "breathe",
                heading: rng.range(0.0, 360.0),
                turn: rng.range(6.0, 14.0) * sign(rng),
                ripples: [ripple(rng, (25.0, 50.0), (0.3, 0.6), (0.2, 0.4)), ripple(rng, (8.0, 18.0), (0.8, 1.3), (0.3, 0.5))],
                open: (6.0, 180.0),
                open_wave: ripple(rng, (1.0, 1.0), (0.35, 0.6), (0.35, 0.55)),
                wobble: [0.0; CLOCKS],
            },
            // Rigid right-angle corners, turned by a broad wave.
            _ => Mood {
                name: "corners",
                heading: rng.range(0.0, 360.0),
                turn: rng.range(14.0, 24.0) * sign(rng),
                ripples: [ripple(rng, (60.0, 110.0), (0.3, 0.55), (0.15, 0.3)), ripple(rng, (10.0, 20.0), (0.9, 1.3), (0.3, 0.5))],
                open: (90.0, 90.0),
                open_wave: still,
                wobble: [0.0; CLOCKS],
            },
        };
        mood.wobble = std::array::from_fn(|_| rng.range(-4.0, 4.0));
        mood
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

pub struct Ambient {
    mood: Mood,
    /// The field's own clock, in seconds. Runs at `vigor` times real time.
    clock: f32,
    vigor: f32,
    /// Seconds this has been running, in real time.
    age: f32,
    servos: [[Servo; 2]; CLOCKS],
    /// Which hand of each clock takes which side of the opening.
    swapped: [bool; CLOCKS],
    reverse: bool,
}

impl Ambient {
    pub fn new(rng: &mut Rng) -> Ambient {
        Ambient {
            mood: Mood::new(rng),
            clock: rng.range(0.0, 100.0),
            vigor: 0.0,
            age: 0.0,
            servos: [[Servo::default(); 2]; CLOCKS],
            swapped: [false; CLOCKS],
            reverse: rng.u64() & 1 == 0,
        }
    }

    pub fn name(&self) -> &'static str {
        self.mood.name
    }

    /// Where clock `i`'s hands should be when the field's clock reads `clock`.
    fn target(&self, i: usize, clock: f32) -> Hands {
        let (x, y) = ((i % COLS) as f32, (i / COLS) as f32);
        let m = &self.mood;
        let wave = |r: &Ripple| (r.k.0 * x + r.k.1 * y + r.w * clock).sin();
        let angle = m.heading
            + m.turn * clock
            + m.ripples[0].amp * wave(&m.ripples[0])
            + m.ripples[1].amp * wave(&m.ripples[1])
            + m.wobble[i];
        let s = 0.5 + 0.5 * wave(&m.open_wave);
        let open = m.open.0 + (m.open.1 - m.open.0) * s * s;
        if self.swapped[i] {
            [angle + open * 0.5, angle - open * 0.5]
        } else {
            [angle - open * 0.5, angle + open * 0.5]
        }
    }

    /// Advance by `dt` seconds. With `settle` set, the field slows to a stop.
    pub fn step(&mut self, angles: &mut [Hands; CLOCKS], motor: Motor, dt: f32, settle: bool) {
        if dt <= 0.0 {
            return;
        }
        self.age += dt;
        let goal = if settle { 0.0 } else { 1.0 };
        self.vigor += (goal - self.vigor).clamp(-dt / 1.5, dt / 2.5);
        let earlier = self.clock;
        self.clock += dt * self.vigor;

        for i in 0..CLOCKS {
            // Clocks join in one column after another rather than all at once.
            let col = i % COLS;
            let turn = if self.reverse { COLS - 1 - col } else { col };
            if self.age < turn as f32 * 0.3 {
                continue;
            }
            if !self.servos[i][0].released {
                let t = self.target(i, self.clock);
                let cost = |t: Hands| (0..2).map(|h| super::dance::shortest(angles[i][h], t[h]).abs()).sum::<f32>();
                self.swapped[i] = cost([t[1], t[0]]) < cost(t);
                let t = self.target(i, self.clock);
                for h in 0..2 {
                    let nearest = angles[i][h] + super::dance::shortest(angles[i][h], t[h]);
                    self.servos[i][h] = Servo { velocity: 0.0, offset: nearest - t[h], released: true };
                }
            }
            let (target, before) = (self.target(i, self.clock), self.target(i, earlier));
            for h in 0..2 {
                let servo = &mut self.servos[i][h];
                let feed = (target[h] - before[h]) / dt;
                let error = target[h] + servo.offset - angles[i][h];
                // The fastest approach that can still stop in time, softened
                // close in so it settles instead of chattering.
                let approach = (2.0 * motor.acc * error.abs()).sqrt().min(error.abs() * 5.0).min(motor.speed);
                let want = feed + approach * error.signum();
                servo.velocity += (want - servo.velocity).clamp(-motor.acc * dt, motor.acc * dt);
                angles[i][h] += servo.velocity * dt;
            }
        }
    }

    /// True once every hand has all but stopped.
    pub fn at_rest(&self) -> bool {
        self.vigor <= 0.0 && self.servos.iter().flatten().all(|s| s.velocity.abs() < 1.5)
    }

    /// How far round the field has turned: for tests.
    #[cfg(test)]
    fn phase(&self) -> f32 {
        (self.mood.turn * self.clock).rem_euclid(360.0).to_radians()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pieces::clocks::pose;

    /// From the digits, through a minute of ambient motion and back to rest:
    /// no hand ever exceeds the motor's speed by more than the field's own
    /// drift, accelerations stay bounded, and everything stops when asked.
    #[test]
    fn ambient_motion_respects_the_motor_and_comes_to_rest() {
        let motor = Motor { speed: 100.0, acc: 160.0 };
        for seed in 0..12 {
            let mut ambient = Ambient::new(&mut Rng::new(seed));
            let mut angles = pose(9, 25);
            let dt = 1.0 / 30.0;
            let mut prev = angles;
            let mut prev_v = [[0.0_f32; 2]; CLOCKS];
            for frame in 0..(40 * 30) {
                let settle = frame > 30 * 30;
                ambient.step(&mut angles, motor, dt, settle);
                for i in 0..CLOCKS {
                    for h in 0..2 {
                        let v = (angles[i][h] - prev[i][h]) / dt;
                        assert!(v.abs() <= motor.speed * 1.6, "{}: {v} deg/s", ambient.name());
                        assert!((v - prev_v[i][h]).abs() <= motor.acc * dt * 1.05 + 1e-3, "{}: jerked", ambient.name());
                        prev_v[i][h] = v;
                    }
                }
                prev = angles;
            }
            assert!(ambient.at_rest(), "{} (seed {seed}) still moving, phase {}", ambient.name(), ambient.phase());
        }
    }
}
