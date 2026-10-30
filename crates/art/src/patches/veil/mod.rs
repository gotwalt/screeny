//! Veil: something on the far side of a gossamer sheet reaches through towards
//! us. Made for the 32x32 Art Frame, whose diffuser is already a veil: the
//! sheet here is the panel's own front, and what it shows is only where the
//! fabric is pushed - the flat sheet is black.
//!
//! What presses is a real hand (`hand.rs`, a skinned model posed by bending
//! its fingers): pointing, two fingers, a crooked beckoning finger, or a whole
//! hand flat against the cloth that may hook its fingers while it holds. The
//! sheet (`cloth.rs`) is a membrane relaxed over it every frame. This file is
//! the choreography - one visit after another, each planned from the seed and
//! its number as a path of gestures picked at random - and the lighting.
//!
//! Everything is composed in the centre square, x in -1..1 of the frame's
//! -2..2, which is what the Art Frame shows of the 64x32 frame.

mod cloth;
mod hand;

use crate::color::{oklch, Rgb};
use crate::dither::Dither;
use crate::frame::{self, Frame};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef};
use crate::rng::Rng;
use cloth::Cloth;
use hand::{Model, Pose, Rigid};
use std::f32::consts::{PI, TAU};

pub const DEF: PatchDef = PatchDef {
    id: "veil",
    name: "Veil",
    blurb: "A hand behind a gossamer veil presses through towards us: a posed 3D hand, cloth relaxed over it, lit on black. Made for the 32x32 Art Frame.",
    params: PARAMS,
    make,
    // The seed picks where each visit presses, from which side, how far, in
    // what pose, and whether each visit is a finger, two or a hand.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("hue", "Hue", 0.0, 360.0, 1.0, 220.0),
    param("tint", "Tint", 0.0, 0.12, 0.001, 0.015),
    param("pallor", "Pallor where the cloth lies on the hand", 0.0, 2.0, 0.01, 1.0),
    param("size", "Finger size", 0.05, 0.35, 0.005, 0.11),
    param("stiff", "Cloth stiffness", 0.0, 0.4, 0.001, 0.08),
    param("pause", "Stillness between visits (s)", 0.0, 60.0, 1.0, 12.0),
    param("steps", "Palette steps", 2.0, 200.0, 1.0, 120.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
];

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Veil { seed: Rng::new(seed).f32(), cloth: Cloth::new(), skinned: Vec::new(), visit: None })
}

/// What comes through.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// The index finger alone, pointing at us.
    Pointer,
    /// An open hand held up, palm to us, fingers arched forward: they meet
    /// the cloth first and are pushed flat as the palm comes on, then it backs
    /// off.
    Stop,
    /// The fingers pointing at us: as it pushes in, each bends just enough
    /// that its tip stops at the cloth, until all five rest there together.
    Tips,
}

/// Longest a visit runs, start to settled, so a slot is this plus the pause.
const VISIT: f32 = 30.0;

/// The finger size the choreography's depths and spacings are written for;
/// at any other size they scale with it, so a smaller finger presses in
/// proportion and its neighbour stays a finger's width away.
const DRAWN_AT: f32 = 0.19;

/// Where the tip is behind the sheet at rest; the sheet is z = 0, we are +z.
const AWAY: f32 = -0.4;

/// The most a visit's gestures may run past the longest planned, for the
/// last move away.
const SETTLE: f32 = 6.0;

/// After a visit, this long for the sheet to stop shivering before `pause`.
const REST: f32 = 2.0;

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
    /// A hand's size, as a multiple of life size.
    span: f32,
    /// How far the cloth will stretch towards us before it stops the hand
    /// (`Stop`, `Tips`), at `DRAWN_AT`.
    wall: f32,
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
        let roll = if slot == 0 { 0.2 } else { rng.f32() };
        let kind = match roll {
            r if r < 0.5 => Kind::Pointer,
            r if r < 0.75 => Kind::Stop,
            _ => Kind::Tips,
        };
        let start = if slot == 0 { 0.3 } else { rng.range(0.0, 0.5) };
        let mut v = Visit {
            kind,
            keys: Vec::new(),
            release: VISIT,
            // Mostly from below, as an arm would be, sometimes from a side.
            toward: -PI / 2.0 + rng.range(-1.1, 1.1),
            lean: rng.range(0.05, 0.3),
            phase: rng.range(0.0, TAU),
            span: rng.range(0.78, 0.9),
            wall: rng.range(0.22, 0.3),
        };
        let path = match kind {
            Kind::Pointer => Self::finger(&mut rng, start),
            Kind::Stop => {
                // Fingers up, give or take.
                v.toward = PI / 2.0 + rng.range(-0.3, 0.3);
                Self::press(&mut rng, start, v.wall, 0.04)
            }
            Kind::Tips => Self::press(&mut rng, start, v.wall, 0.5),
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

    /// A hand that comes up against the cloth and is stopped there: in until
    /// it is `over` past the wall, a hold, perhaps a second press, and back.
    /// Its z is the hand's own push; the wall decides what the fingers do.
    fn press(rng: &mut Rng, start: f32, wall: f32, over: f32) -> Path {
        let (x, y) = (rng.range(-0.1, 0.1), rng.range(-0.15, 0.05));
        let mut p = Path { keys: vec![Key { t: start, x, y, z: AWAY, ease: Ease::Gentle }] };
        let full = wall + over;
        p.to(rng.range(4.0, 7.0), x, y, full, Ease::Gentle);
        p.stay(rng.range(1.5, 3.0));
        if rng.f32() < 0.4 {
            p.to(rng.range(1.5, 2.5), x, y, full - rng.range(0.08, 0.15), Ease::Gentle);
            p.to(rng.range(1.5, 2.5), x, y, full, Ease::Gentle);
            p.stay(rng.range(1.0, 2.0));
        }
        let k = p.last();
        p.to(rng.range(2.5, 4.0), k.x, k.y, AWAY - 0.1, Ease::Gentle);
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

    /// Where everything is at `local` seconds into the slot; `None` while
    /// nothing is there.
    fn moment(&self, local: f32) -> Option<Moment> {
        if local < self.keys[0].t {
            return None;
        }
        let (x, y, z) = self.path().sample(local);
        // Alive while it holds: a breath in the push, a slow turn of the hand.
        let breath = 0.012 * (local * 1.3 + self.phase).sin() * smooth(z / 0.1);
        let turn = 0.22 * (local * 0.11 + self.phase).sin();
        let age = local - self.release;
        Some(Moment {
            kind: self.kind,
            tip: (x, y, z + breath),
            toward: self.toward + if self.kind == Kind::Pointer { turn } else { turn * 0.4 },
            lean: self.lean + 0.06 * (local * 0.17 + self.phase * 2.0).sin(),
            span: self.span,
            wall: self.wall,
            ripple: if age > 0.0 { 0.012 * (-age * 0.9).exp() } else { 0.0 },
            age: age.max(0.0),
        })
    }
}

/// One instant of a visit.
struct Moment {
    kind: Kind,
    /// The front-most point of the hand: x, y on the sheet, z at `DRAWN_AT`.
    tip: (f32, f32, f32),
    toward: f32,
    lean: f32,
    span: f32,
    wall: f32,
    ripple: f32,
    age: f32,
}

/// Life size on the Art Frame: 8 inches is 2.16 units, so 10.6 units a metre,
/// at the default finger size.
const LIFE: f32 = 10.6 / 0.11;

/// How the hand is posed and placed for a moment: a pose, and a rigid
/// placement from the model's rest space into the sheet's (x, y across it,
/// z towards us, at the real finger size). `true` when the placement already
/// puts the hand at its depth; a pointer is placed by its front instead.
fn arrange(model: &Model, m: &Moment, size: f32) -> (Pose, Rigid, bool) {
    let (fingers, palm) = model.axes();
    let k = size / DRAWN_AT;
    let scale = LIFE * size;
    match m.kind {
        Kind::Pointer => {
            let pose = Pose::POINT;
            let bones = model.bones(&pose);
            // The index finger towards us, leaning back a little the way the
            // hand is, the palm facing down the sheet.
            let out = [-m.toward.cos() * m.lean.sin(), -m.toward.sin() * m.lean.sin(), m.lean.cos()];
            let down = [(m.toward + 0.3).cos(), (m.toward + 0.3).sin(), 0.0];
            let turn = Rigid::between(model.index_dir(&bones), palm, out, down).scaled(scale);
            let at = turn.apply(model.joint(&bones, hand::INDEX_TIP));
            (pose, turn.moved([m.tip.0 - at[0], m.tip.1 - at[1], -at[2]]), false)
        }
        Kind::Stop => {
            // Palm to us, fingers up the sheet, arched forward at rest.
            let scale = scale * m.span;
            let up = [m.toward.cos(), m.toward.sin(), 0.0];
            let turn = Rigid::between(fingers, palm, up, [0.0, 0.0, 1.0]).scaled(scale);
            let arched = Pose { curl: [0.15, 0.32, 0.3, 0.32, 0.36], spread: 0.1 };
            // The palm's front comes to the push, but never past the wall.
            let bones = model.bones(&arched);
            let centre = hand::mid(model.joint(&bones, hand::WRIST), model.joint(&bones, 11));
            let c = turn.apply(centre);
            let front = (m.tip.2 * k).min(m.wall * k) - hand::PALM * scale;
            let place = turn.moved([m.tip.0 - c[0], m.tip.1 - c[1], front - c[2]]);
            // Each finger keeps as much of its arch as the wall allows.
            let pose = model.against(arched, &place, m.wall * k, hand::PAD * scale, -0.15);
            (pose, place, true)
        }
        Kind::Tips => {
            // Fingers to us, the palm down the sheet, a little apart.
            let scale = scale * m.span;
            let out = [-m.toward.cos() * m.lean.sin(), -m.toward.sin() * m.lean.sin(), m.lean.cos()];
            let down = [(m.toward + 0.3).cos(), (m.toward + 0.3).sin(), 0.0];
            let turn = Rigid::between(fingers, palm, out, down).scaled(scale);
            let reaching = Pose { curl: [0.55, 0.12, 0.1, 0.12, 0.16], spread: 0.09 };
            // The hand's push carries the middle fingertip; past the wall each
            // finger bends so its tip stays there.
            let bones = model.bones(&reaching);
            let tip = turn.apply(model.joint(&bones, 14));
            let place = turn.moved([m.tip.0 - tip[0], m.tip.1 - tip[1], m.tip.2 * k - tip[2] - hand::PAD * scale]);
            let pose = model.against(reaching, &place, m.wall * k, hand::PAD * scale, 1.4);
            (pose, place, true)
        }
    }
}

struct Veil {
    seed: f32,
    cloth: Cloth,
    skinned: Vec<[f32; 3]>,
    /// The visit under way: its number, when it began, and its plan. The next
    /// begins as soon as this one has settled and `pause` has passed.
    visit: Option<(u64, f32, Visit)>,
}

impl Patch for Veil {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let t = ctx.t as f32;
        let next = match &self.visit {
            // Time went backwards (a restart): begin again.
            Some((_, start, _)) if t < *start => Some(0),
            Some((n, start, v)) if t >= start + v.keys.last().unwrap().t + REST + ctx.get("pause") => Some(n + 1),
            Some(_) => None,
            None => Some(0),
        };
        if let Some(n) = next {
            self.visit = Some((n, if n == 0 { 0.0 } else { t }, Visit::plan(n, self.seed)));
        }
        let (_, start, visit) = self.visit.as_ref().unwrap();
        let moment = visit.moment(t - start);
        let size = ctx.get("size");
        let model = Model::get();

        self.cloth.clear_under();
        let (mut ripple, mut ripple_at) = (0.0, (0.0, 0.0, 0.0));
        if let Some(m) = &moment {
            let (pose, place, placed) = arrange(model, m, size);
            let bones = model.bones(&pose);
            model.skin(&bones, &place, &mut self.skinned);
            // A pointer's front-most point is where the choreography says the
            // tip is.
            if !placed {
                let front = self.skinned.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
                let lift = m.tip.2 * size / DRAWN_AT - front;
                for p in &mut self.skinned {
                    p[2] += lift;
                }
            }
            for tri in &model.triangles {
                let [a, b, c] = tri.map(|i| self.skinned[i as usize]);
                self.cloth.press(a, b, c);
            }
            ripple = m.ripple;
            ripple_at = (m.tip.0, m.tip.1, m.age);
        }
        self.cloth.settle(ctx.get("stiff"));

        let colour = oklch(0.97, ctx.get("tint"), ctx.get("hue"));
        let pallor = ctx.get("pallor");
        let cloth = &self.cloth;
        let height = |x: f32, y: f32| {
            let (h, touch) = cloth.at(x, y);
            let r = ((x - ripple_at.0).powi(2) + (y - ripple_at.1).powi(2)).sqrt();
            let age = ripple_at.2;
            let wave = ripple * (r * 16.0 - age * 4.5).sin() * (-r * 1.2).exp() * smooth(age * 4.5 - r * 16.0 + 6.0);
            (h + wave, touch)
        };
        // The Art Frame's 32 LEDs span 2.16 units; the frame is 64 wide.
        let unit = 2.16 / frame::H as f32;
        let key_l = norm([-0.55, 0.78, 0.3]);
        let rim_l = norm([0.6, -0.75, 0.05]);
        let ss = ctx.get("samples") as usize;
        let lit = Frame::supersample(ss, |px, py| {
            let x = (px - frame::W as f32 / 2.0) * unit;
            let y = (frame::H as f32 / 2.0 - py) * unit;
            if x.abs() > 1.15 {
                return Rgb::BLACK;
            }
            let e = 0.012;
            let (h, touch) = height(x, y);
            let gx = (height(x + e, y).0 - height(x - e, y).0) / (2.0 * e);
            let gy = (height(x, y + e).0 - height(x, y - e).0) / (2.0 * e);
            let n = norm([-gx, -gy, 1.0]);
            let slope = (gx * gx + gy * gy).sqrt();
            // Only what turns towards the light more than the flat sheet does
            // is lit, so the sheet is black and the form has a lit side and a
            // dark one.
            let key = crate::color::smoothstep(0.0, 0.85, (dot(n, key_l) - key_l[2]) / (1.0 - key_l[2]));
            let half = norm([key_l[0], key_l[1], key_l[2] + 1.0]);
            let gleam = dot(n, half).max(0.0).powf(18.0) * crate::color::smoothstep(0.05, 0.3, slope);
            let rim = crate::color::smoothstep(0.15, 0.9, (dot(n, rim_l) - rim_l[2]) / (1.0 - rim_l[2]));
            // Creases: where the sheet is cupped it sees less of anything.
            let d = 0.02;
            let lap = height(x + d, y).0 + height(x - d, y).0 + height(x, y + d).0 + height(x, y - d).0 - 4.0 * h;
            let open = (1.0 - lap.max(0.0) * 40.0).clamp(0.35, 1.0);
            // Where it lies on the hand the cloth is thinnest, and a little of
            // the hand's pallor comes through.
            let light = (key * 0.8 + gleam * 0.45 + rim * 0.12) * open + crate::color::smoothstep(0.3, 1.0, touch) * 0.3 * pallor;
            Rgb { r: colour.r * light, g: colour.g * light, b: colour.b * light }
        });
        // As many steps as the panel can show, down to just above black, so
        // the light falls off the cloth in a gradient, not a step.
        Palette::ramps(&[ctx.get("hue")], ctx.get("steps") as usize, (0.1, 0.96), ctx.get("tint")).map(&lit, Dither::BlueNoise, ctx.get("dither"))
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
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
            assert!(v.moment(VISIT).is_none_or(|m| m.tip.2 <= AWAY), "slot {slot}: still pressing at the end");
        }
    }

    #[test]
    fn the_first_slot_shows_a_finger_early() {
        let v = Visit::plan(0, 0.9);
        assert_eq!(v.kind, Kind::Pointer);
        assert!((1..15).any(|t| v.moment(t as f32).is_some_and(|m| m.tip.2 > 0.2)));
    }

    #[test]
    fn visits_move_differently() {
        let shapes: std::collections::HashSet<usize> = (1..60).map(|s| Visit::plan(s, 0.5).keys.len()).collect();
        assert!(shapes.len() >= 5, "only {} different lengths of path", shapes.len());
    }

    #[test]
    fn every_kind_comes() {
        let kinds: Vec<Kind> = (1..1000).map(|s| Visit::plan(s, 0.5).kind).collect();
        let count = |k| kinds.iter().filter(|&&x| x == k).count();
        assert!(count(Kind::Stop) > 150 && count(Kind::Tips) > 150, "{} {}", count(Kind::Stop), count(Kind::Tips));
    }
}
