//! Veil: something on the far side of a gossamer sheet reaches through towards
//! us. Made for the 32x32 Art Frame, whose diffuser is already a veil: the
//! sheet here is the panel's own front, and what it shows is only where the
//! fabric is pushed - the flat sheet is black.
//!
//! The shader (`veil.wgsl`) raymarches the sheet as a heightfield tented over
//! fingertips. This file is the choreography: a pure function of time and
//! seed, one visit per slot. Most visits are a single fingertip - a glow
//! gathers behind the sheet, the cloth tents, hesitates, presses further,
//! feels about, then is snatched back and the sheet shivers. Rarer ones are
//! two fingers, or a whole hand laid against it. Some slots stay empty.
//!
//! Everything is composed in the centre square (x in -1..1 of the shader's
//! -2..2), which is what the Art Frame shows of the 64x32 frame.

use crate::gpu::{Scene, ShaderPatch, MAX_EXTRA};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef};
use crate::rng::Rng;
use std::f32::consts::{PI, TAU};

pub const DEF: PatchDef = PatchDef {
    id: "veil",
    name: "Veil",
    blurb: "GPU, raymarched cloth. Something behind a gossamer veil presses a fingertip through towards us. Made for the 32x32 Art Frame.",
    params: PARAMS,
    make,
    // The seed picks where each visit presses, from which side, how far, and
    // which slots are a finger, two, a hand or nothing.
    seeded: true,
};

// The shader reads the first five as P(0)..P(4); the rest are used here.
const PARAMS: &[ParamSpec] = &[
    param("hue", "Hue", 0.0, 360.0, 1.0, 220.0),
    param("tint", "Tint", 0.0, 0.12, 0.001, 0.015),
    param("pallor", "Pallor at the fingertip", 0.0, 2.0, 0.01, 1.0),
    param("size", "Finger size", 0.05, 0.35, 0.005, 0.11),
    param("folds", "Folds", 0.0, 2.0, 0.01, 1.0),
    param("pause", "Stillness between visits (s)", 0.0, 60.0, 1.0, 12.0),
    param("steps", "Palette steps", 2.0, 200.0, 1.0, 120.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 5.0),
];

fn make(seed: u64) -> Box<dyn Patch> {
    ShaderPatch::with_scene("veil", include_str!("veil.wgsl"), PARAMS, seed, scene)
}

/// What can come through in a slot. The shader reads it as X(0).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Nothing = 0,
    Finger = 1,
    Two = 2,
    Hand = 3,
}

/// Longest a visit runs, start to settled, so a slot is this plus the pause.
const VISIT: f32 = 30.0;

/// The finger size the choreography's depths and spacings are written for;
/// at any other size they scale with it, so a smaller finger presses in
/// proportion and its neighbour stays a finger's width away.
const DRAWN_AT: f32 = 0.19;

/// Where the tip is behind the sheet at rest; the sheet is z = 0, we are +z.
const AWAY: f32 = -0.4;

/// Leave this long at the end of a slot for the sheet to settle.
const SETTLE: f32 = 6.0;

/// How far from the centre anything presses: the Art Frame shows x and y in
/// -1..1, and the cloth tents a little way out from the tip.
const BOUND: f32 = 0.6;

fn smooth(x: f32) -> f32 {
    crate::color::smoothstep(0.0, 1.0, x)
}

/// How a path arrives at a key.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ease {
    /// Starts and stops gently: every deliberate move.
    Gentle,
    /// Accelerates all the way: snatched back.
    Snatch,
}

/// Where the tip is at a moment of the visit. z is at `DRAWN_AT`.
#[derive(Clone, Copy, Debug)]
struct Key {
    t: f32,
    x: f32,
    y: f32,
    z: f32,
    ease: Ease,
}

/// One visit, laid out for its slot: a path of keys, built from gestures
/// chosen at random, so no two visits move alike.
struct Visit {
    kind: Kind,
    keys: Vec<Key>,
    /// When the cloth is let go, for the shiver after.
    release: f32,
    /// The direction, in the sheet, the hand is in from the tip (for `Hand`,
    /// the way its fingers point); how far the finger leans off pointing
    /// straight at us; and a phase for the slow turn of both.
    toward: f32,
    lean: f32,
    phase: f32,
    /// The second finger, relative to the first, and how far it lags it.
    other: (f32, f32),
    lag: f32,
    /// A hand's size, as a multiple of the finger size.
    span: f32,
}

/// Builds a path: each step adds a key, `t` seconds after the last.
struct Path {
    keys: Vec<Key>,
}

impl Path {
    fn last(&self) -> Key {
        *self.keys.last().expect("a path starts with a key")
    }

    fn to(&mut self, dt: f32, x: f32, y: f32, z: f32, ease: Ease) {
        let t = self.last().t + dt;
        self.keys.push(Key { t, x: x.clamp(-BOUND, BOUND), y: y.clamp(-BOUND, BOUND), z, ease });
    }

    fn stay(&mut self, dt: f32) {
        let k = self.last();
        self.to(dt, k.x, k.y, k.z, Ease::Gentle);
    }

    fn sample(&self, t: f32) -> (f32, f32, f32) {
        let first = self.keys[0];
        if t <= first.t {
            return (first.x, first.y, first.z);
        }
        for w in self.keys.windows(2) {
            let (a, b) = (w[0], w[1]);
            if t < b.t {
                let u = (t - a.t) / (b.t - a.t);
                let e = match b.ease {
                    Ease::Gentle => smooth(u),
                    Ease::Snatch => u * u,
                };
                return (a.x + (b.x - a.x) * e, a.y + (b.y - a.y) * e, a.z + (b.z - a.z) * e);
            }
        }
        let k = self.last();
        (k.x, k.y, k.z)
    }
}

impl Visit {
    fn plan(slot: u64, seed: f32) -> Self {
        let mut rng = Rng::new(((seed * 1e9) as u64) ^ slot.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // The first slot always has a finger in it, soon: whoever starts the
        // patch should see what it is.
        let roll = if slot == 0 { 0.5 } else { rng.f32() };
        let kind = match roll {
            r if r < 0.15 => Kind::Nothing,
            r if r < 0.70 => Kind::Finger,
            r if r < 0.88 => Kind::Two,
            _ => Kind::Hand,
        };
        let start = if slot == 0 { 1.5 } else { rng.range(0.0, 2.0) };
        let mut v = Visit {
            kind,
            keys: Vec::new(),
            release: VISIT,
            // Mostly from below, as an arm would be, sometimes from a side.
            toward: -PI / 2.0 + rng.range(-1.1, 1.1),
            lean: rng.range(0.35, 0.75),
            phase: rng.range(0.0, TAU),
            other: (0.0, 0.0),
            lag: rng.range(0.3, 1.2),
            span: rng.range(5.0, 6.2),
        };
        let side = v.toward + PI / 2.0 * rng.sign();
        let gap = rng.range(0.3, 0.4);
        v.other = (side.cos() * gap, side.sin() * gap);
        let path = if kind == Kind::Hand {
            // Fingers up, give or take: the hand's own direction.
            v.toward = PI / 2.0 + rng.range(-0.35, 0.35);
            Self::hand(&mut rng, start)
        } else {
            Self::finger(&mut rng, start)
        };
        // The cloth comes free a little way into the last move.
        let n = path.keys.len();
        let (a, b) = (path.keys[n - 2], path.keys[n - 1]);
        v.release = a.t + (b.t - a.t) * 0.4;
        v.keys = path.keys;
        v
    }

    /// A fingertip: in, then one to four gestures, then away.
    fn finger(rng: &mut Rng, start: f32) -> Path {
        let (x, y) = (rng.range(-0.42, 0.42), rng.range(-0.42, 0.34));
        let mut p = Path { keys: vec![Key { t: start, x, y, z: AWAY, ease: Ease::Gentle }] };
        // Some come in slowly and some quickly; some touch first and then push.
        if rng.f32() < 0.4 {
            p.to(rng.range(2.5, 6.0), x, y, rng.range(0.04, 0.12), Ease::Gentle);
            p.stay(rng.range(0.5, 2.5));
        }
        p.to(rng.range(2.0, 6.0), x, y, rng.range(0.3, 0.46), Ease::Gentle);
        let last_out = 4.0;
        let gestures = 1 + (rng.u64() % 4) as usize;
        for _ in 0..gestures {
            let before = p.keys.len();
            let k = p.last();
            match rng.u64() % 6 {
                // Linger, barely moving.
                0 => p.to(rng.range(2.0, 6.0), k.x + rng.range(-0.04, 0.04), k.y + rng.range(-0.04, 0.04), k.z + 0.03, Ease::Gentle),
                // Further through.
                1 => p.to(rng.range(1.5, 4.0), k.x, k.y, (k.z + rng.range(0.1, 0.25)).min(0.75), Ease::Gentle),
                // Trace across the cloth, dragging it.
                2 => {
                    let (a, d) = (rng.range(0.0, TAU), rng.range(0.15, 0.45));
                    p.to(d * rng.range(12.0, 24.0), k.x + a.cos() * d, k.y + a.sin() * d, k.z * rng.range(0.8, 1.15), Ease::Gentle);
                }
                // Ease off until it barely touches, then press again nearby.
                3 => {
                    p.to(rng.range(1.0, 2.5), k.x, k.y, rng.range(0.02, 0.1), Ease::Gentle);
                    p.stay(rng.range(0.3, 2.5));
                    let (a, d) = (rng.range(0.0, TAU), rng.range(0.0, 0.3));
                    p.to(rng.range(1.5, 4.0), k.x + a.cos() * d, k.y + a.sin() * d, rng.range(0.3, 0.5), Ease::Gentle);
                }
                // Taps: drawn back a little and pressed again, once or twice.
                4 => {
                    for _ in 0..1 + rng.u64() % 2 {
                        p.to(rng.range(0.5, 0.8), k.x, k.y, k.z - rng.range(0.15, 0.3), Ease::Gentle);
                        p.to(rng.range(0.6, 1.0), k.x, k.y, k.z, Ease::Gentle);
                    }
                }
                // Stillness.
                _ => p.stay(rng.range(1.5, 4.0)),
            }
            if p.last().t > VISIT - SETTLE - last_out {
                p.keys.truncate(before);
                break;
            }
        }
        Self::away(rng, &mut p);
        p
    }

    /// A hand laid flat against the cloth, pressing forward.
    fn hand(rng: &mut Rng, start: f32) -> Path {
        let (x, y) = (rng.range(-0.12, 0.12), rng.range(-0.2, 0.0));
        let mut p = Path { keys: vec![Key { t: start, x, y, z: AWAY, ease: Ease::Gentle }] };
        let z = rng.range(0.14, 0.24);
        p.to(rng.range(5.0, 8.0), x, y, z, Ease::Gentle);
        p.stay(rng.range(1.0, 2.5));
        // Then harder, or sliding a little across the cloth.
        if rng.f32() < 0.5 {
            p.to(rng.range(2.0, 4.0), x, y, z + rng.range(0.06, 0.14), Ease::Gentle);
        } else {
            let (a, d) = (rng.range(0.0, TAU), rng.range(0.05, 0.15));
            p.to(rng.range(3.0, 5.0), x + a.cos() * d, y + a.sin() * d, z, Ease::Gentle);
        }
        p.stay(rng.range(1.0, 2.0));
        Self::away(rng, &mut p);
        p
    }

    /// Snatched back, or sinking slowly away.
    fn away(rng: &mut Rng, p: &mut Path) {
        let k = p.last();
        if rng.f32() < 0.6 {
            p.to(rng.range(0.7, 1.6), k.x, k.y, AWAY - 0.1, Ease::Snatch);
        } else {
            p.to(rng.range(2.5, 4.0), k.x, k.y, AWAY - 0.1, Ease::Gentle);
        }
    }

    fn path(&self) -> Path {
        Path { keys: self.keys.clone() }
    }

    /// `k` is the finger size over [`DRAWN_AT`].
    fn extra(&self, local: f32, k: f32) -> [f32; MAX_EXTRA] {
        let mut x = [0.0; MAX_EXTRA];
        if self.kind == Kind::Nothing || local < self.keys[0].t {
            return x;
        }
        let path = self.path();
        let (tx, ty, tz) = path.sample(local);
        // Alive while it holds: a breath in the push, a slow turn of the hand.
        let breath = 0.012 * (local * 1.3 + self.phase).sin() * smooth(tz / 0.1);
        let turn = 0.22 * (local * 0.11 + self.phase).sin();
        // The glow gathers as it nears the cloth.
        let presence = smooth((tz - AWAY) / -AWAY);
        let age = local - self.release;
        let ripple = if age > 0.0 { 0.012 * (-age * 0.9).exp() } else { 0.0 };

        x[0] = self.kind as u8 as f32;
        x[1] = tx;
        x[2] = ty;
        x[3] = (tz + breath) * k;
        x[4] = self.toward + if self.kind == Kind::Hand { turn * 0.4 } else { turn };
        x[5] = self.lean + 0.12 * (local * 0.17 + self.phase * 2.0).sin();
        x[6] = presence;
        x[7] = ripple;
        x[8] = age.max(0.0);
        x[9] = tx;
        x[10] = ty;
        if self.kind == Kind::Two {
            // The second finger follows the first, a little behind, stopping shorter.
            let (sx, sy, sz) = path.sample(local - self.lag);
            x[11] = sx + self.other.0 * k - tx;
            x[12] = sy + self.other.1 * k - ty;
            x[13] = (sz - 0.08) * k;
        }
        x[14] = self.span;
        x
    }
}

fn scene(ctx: &Ctx, seed: f32) -> Scene {
    let slot_len = VISIT + ctx.get("pause");
    let t = ctx.t as f32;
    let slot = (t / slot_len).floor().max(0.0);
    let visit = Visit::plan(slot as u64, seed);

    Scene {
        extra: visit.extra(t - slot * slot_len, ctx.get("size") / DRAWN_AT),
        // As many steps as the panel can show, down to just above black, so
        // the light falls off the cloth in a gradient, not a step.
        palette: Some(Palette::ramps(&[ctx.get("hue")], ctx.get("steps") as usize, (0.1, 0.96), ctx.get("tint"))),
        dither: ctx.get("dither"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_visit_is_over_before_its_slot_ends() {
        for slot in 0..500 {
            let v = Visit::plan(slot, 0.37);
            let end = v.keys.last().unwrap().t;
            assert!(end + SETTLE <= VISIT, "slot {slot}: ends at {end}");
            let x = v.extra(VISIT, 1.0);
            assert!(x[0] == 0.0 || x[3] <= AWAY, "slot {slot}: still pressing at the end");
        }
    }

    #[test]
    fn the_first_slot_shows_a_finger_early() {
        let v = Visit::plan(0, 0.9);
        assert_eq!(v.kind, Kind::Finger);
        assert!((1..15).any(|t| v.extra(t as f32, 1.0)[3] > 0.2));
    }

    #[test]
    fn visits_move_differently() {
        let shapes: std::collections::HashSet<usize> = (1..60).map(|s| Visit::plan(s, 0.5).keys.len()).collect();
        assert!(shapes.len() >= 5, "only {} different lengths of path", shapes.len());
    }

    #[test]
    fn some_slots_are_empty_and_hands_are_rare() {
        let kinds: Vec<Kind> = (1..1000).map(|s| Visit::plan(s, 0.5).kind).collect();
        let count = |k| kinds.iter().filter(|&&x| x == k).count();
        assert!(count(Kind::Nothing) > 80, "{}", count(Kind::Nothing));
        assert!(count(Kind::Hand) < count(Kind::Finger) / 2);
    }
}
