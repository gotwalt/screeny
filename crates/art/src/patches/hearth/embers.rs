//! Embers: a few sparks thrown up off the logs, carried by the fire's own
//! air.
//!
//! Each is a point with a velocity that the gas around it drags toward its
//! own (so a gust carries it sideways and the flame's curls turn it), its own
//! small lift while it is hot, a little weight, and a jitter of its own. It
//! cools as it climbs - which is all its colour is: the same blackbody ramp
//! as the flames, read at its temperature - and goes out before, or at, the
//! top of the frame.

use super::fluid::Fluid;
use crate::rng::Rng;

#[derive(Clone, Copy, Debug)]
pub struct Ember {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// `0..1`, on the same scale as the gas.
    pub heat: f32,
    /// Cooling per second, as a rate: each one burns its own length of time.
    cooling: f32,
    /// Where it was one step ago, for its motion blur.
    pub px: f32,
    pub py: f32,
}

/// At most this many at once; a burst never exceeds it.
pub const MAX: usize = 16;

/// Below this an ember is no longer visible and is dropped.
pub const OUT: f32 = 0.30;

pub struct Embers {
    pub live: Vec<Ember>,
    rng: Rng,
}

impl Embers {
    pub fn new(seed: u64) -> Embers {
        Embers { live: Vec::with_capacity(MAX), rng: Rng::new(seed) }
    }

    /// Throw one up from `(x, y)`, a cell position at the fuel bed. `vigour`
    /// in `0..1` is how hard the fire it came from is burning.
    pub fn spawn(&mut self, x: f32, y: f32, vigour: f32) {
        if self.live.len() >= MAX {
            return;
        }
        let r = &mut self.rng;
        let e = Ember {
            x: x + r.range(-3.0, 3.0),
            y,
            vx: r.range(-12.0, 12.0),
            vy: r.range(30.0, 55.0) * (0.7 + 0.5 * vigour),
            heat: r.range(0.88, 1.0),
            cooling: r.range(0.16, 0.30),
            px: x,
            py: y,
        };
        self.live.push(Ember { px: e.x, py: e.y, ..e });
    }

    /// One fixed step. `top` is the cell row above which an ember has left
    /// the frame.
    pub fn step(&mut self, dt: f32, fluid: &Fluid, top: f32) {
        let r = &mut self.rng;
        for e in &mut self.live {
            e.px = e.x;
            e.py = e.y;
            let (gu, gv) = fluid.velocity(e.x, e.y);
            // Small and light: the air has it within about half a second,
            // and it keeps a lift of its own only while it is hot.
            let grip = 2.2;
            let lift = 28.0 * e.heat * e.heat - 14.0;
            e.vx += dt * (grip * (gu - e.vx) + r.range(-40.0, 40.0));
            e.vy += dt * (grip * (gv - e.vy) + lift + r.range(-25.0, 25.0));
            e.x += dt * e.vx;
            e.y += dt * e.vy;
            // Hot gas around it keeps it alight a little longer.
            let around = fluid.temperature(e.x, e.y);
            e.heat -= dt * e.cooling * e.heat * (1.0 - 0.6 * around);
        }
        self.live.retain(|e| e.heat > OUT && e.y < top && e.x > -6.0 && e.x < fluid.w as f32 + 6.0);
    }
}
