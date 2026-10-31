//! Veil: something on the far side of a gossamer sheet reaches through towards
//! us. Made for the 32x32 Art Frame, whose diffuser is already a veil: the
//! sheet here is the panel's own front, and what it shows is only where the
//! fabric is pushed - the flat sheet is black.
//!
//! What presses is a real hand (`hand.rs`, a skinned model posed by bending
//! its fingers), always the whole hand, like something living in the box:
//! held up to stop against the cloth, or reaching fingertips first, its
//! fingers working all the while - as a captured take of a real hand moved
//! them (`clips.rs`), or by a few waves of their own. The sheet (`cloth.rs`) is a membrane relaxed over it every frame. This file is
//! the choreography - one visit after another, each planned from the seed and
//! its number as a path of gestures picked at random - and the lighting.
//!
//! Everything is composed in the centre square, x in -1..1 of the frame's
//! -2..2, which is what the Art Frame shows of the 64x32 frame.

mod clips;
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
    param("glow", "The hand's light through the veil", 0.0, 2.0, 0.01, 1.0),
    param("sheen", "Light on the cloth", 0.0, 2.0, 0.01, 0.5),
    param("pallor", "Pallor where the cloth lies on the hand", 0.0, 2.0, 0.01, 1.0),
    param("size", "Finger size", 0.05, 0.35, 0.005, 0.11),
    param("stiff", "Cloth stiffness", 0.0, 0.4, 0.001, 0.08),
    param("pause", "Stillness between visits (s)", 0.0, 60.0, 1.0, 12.0),
    param("steps", "Palette steps", 2.0, 200.0, 1.0, 120.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
    param("mocap", "Fingers move as captured (0: by their own waves)", 0.0, 1.0, 1.0, 1.0),
    param("take", "Captured take (0: a different one each visit)", 0.0, clips::COUNT as f32, 1.0, 0.0),
    param("pace", "How fast a take plays", 0.1, 1.5, 0.01, 0.4),
    param("ease", "How slowly the fingers follow a take (s)", 0.0, 1.0, 0.01, 0.2),
];

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Veil { seed: Rng::new(seed).f32(), cloth: Cloth::new(), skinned: Vec::new(), visit: None, body: None })
}

/// What comes through.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// An open hand held up, palm to us, fingers arched forward: they meet
    /// the cloth first and are pushed flat as the palm comes on, then it backs
    /// off.
    Stop,
    /// The fingers pointing at us: as it pushes in, each bends just enough
    /// that its tip stops at the cloth, until all five rest there together.
    Tips,
}

/// The finger size the choreography's depths and spacings are written for;
/// at any other size they scale with it, so a smaller finger presses in
/// proportion and its neighbour stays a finger's width away.
const DRAWN_AT: f32 = 0.19;

/// Where the tip is behind the sheet at rest; the sheet is z = 0, we are +z.
const AWAY: f32 = -0.4;

/// After a visit, this long for the sheet to stop shivering before `pause`.
const REST: f32 = 2.0;

/// How far from the centre anything presses: the Art Frame shows x and y in
/// -1..1, and the cloth tents a little way out from the tip.
const BOUND: f32 = 0.6;

/// The furthest the forearm may swing off pointing straight back into the
/// box. The model ends at the wrist; turned any more towards us, the cut end
/// shows through the cloth and the hand is plainly not on an arm.
const ARM: f32 = 0.95;

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
    /// How far a `Stop` hand tips its fingers towards us off the sheet's
    /// plane, so its wrist and the arm behind it run back into the dark.
    pitch: f32,
    /// The captured take its fingers play, how fast (times `pace`), and from
    /// where in it.
    take: usize,
    rate: f32,
    from: f32,
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
        // The first slot is fingertips coming towards us, soon: whoever
        // starts the patch should see what it is.
        let kind = if slot == 0 || rng.f32() < 0.5 { Kind::Tips } else { Kind::Stop };
        let start = if slot == 0 { 0.3 } else { rng.range(0.0, 0.5) };
        let mut v = Visit {
            kind,
            keys: Vec::new(),
            // Mostly from below, as an arm would be, sometimes from a side.
            toward: -PI / 2.0 + rng.range(-1.1, 1.1),
            lean: rng.range(0.05, 0.3),
            phase: rng.range(0.0, TAU),
            span: rng.range(0.78, 0.9),
            wall: rng.range(0.22, 0.3),
            pitch: PI / 2.0 - ARM + rng.range(0.05, 0.25),
            take: (rng.u64() % clips::COUNT as u64) as usize,
            rate: rng.range(0.85, 1.15),
            from: rng.range(0.0, 20.0),
        };
        let path = match kind {
            Kind::Stop => {
                // Fingers up, give or take.
                v.toward = PI / 2.0 + rng.range(-0.3, 0.3);
                Self::press(&mut rng, start, v.wall, 0.04)
            }
            Kind::Tips => Self::press(&mut rng, start, v.wall, 0.5),
        };
        v.keys = path.keys;
        v
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
        // Snatched back, or sinking slowly away.
        let k = p.last();
        if rng.f32() < 0.4 {
            p.to(rng.range(0.7, 1.6), k.x, k.y, AWAY - 0.1, Ease::Snatch);
        } else {
            p.to(rng.range(2.5, 4.0), k.x, k.y, AWAY - 0.1, Ease::Gentle);
        }
        p
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
        Some(Moment {
            kind: self.kind,
            tip: (x, y, z + breath),
            toward: self.toward + turn * 0.4,
            lean: self.lean + 0.06 * (local * 0.17 + self.phase * 2.0).sin(),
            span: self.span,
            wall: self.wall,
            pitch: self.pitch,
            feel: [0.0; 5],
            captured: None,
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
    pitch: f32,
    /// What the fingers are doing on their own, added to the pose's bends.
    feel: [f32; 5],
    /// Or how a captured take has them, instead of the pose's own bends.
    captured: Option<hand::Captured>,
}

/// Life size on the Art Frame: 8 inches is 2.16 units, so 10.6 units a metre,
/// at the default finger size.
const LIFE: f32 = 10.6 / 0.11;

/// How the hand is posed and placed for a moment: a pose, and a rigid
/// placement from the model's rest space into the sheet's (x, y across it,
/// z towards us, at the real finger size).
fn arrange(model: &Model, m: &Moment, size: f32) -> (Pose, Rigid) {
    let (fingers, palm) = model.axes();
    let k = size / DRAWN_AT;
    let scale = LIFE * size;
    // A captured take bends the fingers instead of the pose; the wall then
    // bends them further from there.
    let feeling = |mut p: Pose| {
        if let Some(c) = m.captured {
            return Pose { curl: [0.0; 5], spread: 0.0, captured: Some(c) };
        }
        for d in 0..5 {
            p.curl[d] = (p.curl[d] + m.feel[d]).clamp(-0.1, 1.0);
        }
        p
    };
    match m.kind {
        Kind::Stop => {
            // Palm to us, fingers up the sheet and tipped towards us, so the
            // wrist runs back and down to the arm; arched forward at rest.
            let scale = scale * m.span;
            let (up, (s, c)) = ([m.toward.cos(), m.toward.sin(), 0.0], m.pitch.sin_cos());
            let along = [up[0] * c, up[1] * c, s];
            let facing = [-up[0] * s, -up[1] * s, c];
            let turn = Rigid::between(fingers, palm, along, facing).scaled(scale);
            let arched = feeling(Pose { curl: [0.15, 0.32, 0.3, 0.32, 0.36], spread: 0.1, captured: None });
            // The knuckles come to the push, but never past the wall; the
            // fingers beyond them meet it first.
            let bones = model.bones(&arched);
            let knuckle = turn.apply(model.joint(&bones, 11));
            let front = (m.tip.2 * k).min(m.wall * k) - hand::PALM * scale;
            let place = turn.moved([m.tip.0 - knuckle[0], m.tip.1 - knuckle[1], front - knuckle[2]]);
            // Each finger keeps as much of its arch as the wall allows, and
            // bends back flat against it when pressed.
            let pose = model.against(arched, &place, m.wall * k, hand::PAD * scale, -0.6);
            (pose, place)
        }
        Kind::Tips => {
            // Fingers to us, the palm down the sheet, a little apart.
            let scale = scale * m.span;
            let out = [-m.toward.cos() * m.lean.sin(), -m.toward.sin() * m.lean.sin(), m.lean.cos()];
            let down = [(m.toward + 0.3).cos(), (m.toward + 0.3).sin(), 0.0];
            let turn = Rigid::between(fingers, palm, out, down).scaled(scale);
            let reaching = feeling(Pose { curl: [0.55, 0.12, 0.1, 0.12, 0.16], spread: 0.09, captured: None });
            // The hand's push carries the middle fingertip; past the wall each
            // finger bends so its tip stays there.
            let bones = model.bones(&reaching);
            let tip = turn.apply(model.joint(&bones, 14));
            let place = turn.moved([m.tip.0 - tip[0], m.tip.1 - tip[1], m.tip.2 * k - tip[2] - hand::PAD * scale]);
            let pose = model.against(reaching, &place, m.wall * k, hand::PAD * scale, 1.4);
            (pose, place)
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
    /// The hand as a body with weight, following its path; `None` between
    /// visits.
    body: Option<Body>,
}

/// The hand's own motion: where it is and how fast it moves, and the same for
/// each digit's bend. Each follows what the choreography and the cloth ask of
/// it through a spring, so the hand eases in, overshoots a hair and settles,
/// and the fingers trail it and re-arch a beat after the palm lets go.
struct Body {
    at: [f32; 3],
    speed: [f32; 3],
    curl: [f32; 5],
    curling: [f32; 5],
    /// Where the fingers have got to in the captured take they follow.
    take: Option<[[f32; 3]; 21]>,
}

/// A spring's stiffness (as an angular frequency) and damping ratio.
const HAND_SPRING: (f32, f32) = (6.0, 0.8);
const FINGER_SPRING: (f32, f32) = (9.0, 0.65);

/// The fingers working on their own, like the legs of something feeling its
/// way out of a shell: a ripple from the little finger to the index while
/// the hand comes towards us, barely a stir while it holds, and all of them
/// drawn in as it goes back. `closing` is how fast it comes (z a second).
fn feel(t: f32, phase: f32, closing: f32) -> [f32; 5] {
    let coming = smooth(closing / 0.15);
    let going = smooth(-closing / 0.25);
    let stir = 0.05 + 0.25 * coming;
    let mut out = [0.0; 5];
    for (d, f) in out.iter_mut().enumerate() {
        let d_ = d as f32;
        let w = (TAU * 0.85 * t + d_ * 0.9 + phase).sin() + 0.5 * (TAU * 1.45 * t + 2.3 * d_ + 3.0 * phase).sin();
        // The thumb moves less.
        let a = w / 1.5 * stir * if d == 0 { 0.4 } else { 1.0 };
        *f = a + 0.35 * going;
    }
    out
}

/// One step of a damped spring pulling `x` (moving at `v`) towards `to`.
fn spring(x: &mut f32, v: &mut f32, to: f32, (w, zeta): (f32, f32), dt: f32) {
    *v += (w * w * (to - *x) - 2.0 * zeta * w * *v) * dt;
    *x += *v * dt;
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
        let mut moment = visit.moment(t - start);
        let size = ctx.get("size");
        let model = Model::get();
        let dt = (ctx.dt as f32).clamp(0.0, 0.1);

        self.cloth.clear_under();
        if moment.is_none() {
            self.body = None;
        }
        if let Some(m) = &mut moment {
            // The body follows the path, with a faint tremor: something alive
            // holding as still as it can.
            let tremor = [
                0.006 * ((t * 5.1 + visit.phase).sin() + 0.6 * (t * 8.3 + 2.0 * visit.phase).sin()),
                0.006 * ((t * 4.3 + 3.0 * visit.phase).sin() + 0.6 * (t * 7.7 + visit.phase).sin()),
                0.004 * (t * 6.1 + 5.0 * visit.phase).sin(),
            ];
            let goal = [m.tip.0 + tremor[0], m.tip.1 + tremor[1], m.tip.2 + tremor[2]];
            let body = self.body.get_or_insert(Body { at: goal, speed: [0.0; 3], curl: [f32::NAN; 5], curling: [0.0; 5], take: None });
            for k in 0..3 {
                spring(&mut body.at[k], &mut body.speed[k], goal[k], HAND_SPRING, dt);
            }
            m.tip = (body.at[0], body.at[1], body.at[2]);
            if ctx.get("mocap") >= 0.5 {
                let all = clips::all();
                let pick = ctx.get("take") as usize;
                let take = if pick >= 1 { &all[(pick - 1).min(all.len() - 1)] } else { &all[visit.take] };
                let want = take.at((t - start) * visit.rate * ctx.get("pace") + visit.from);
                // The fingers ease after the take rather than snapping to it.
                let k = 1.0 - (-dt / ctx.get("ease").max(1e-3)).exp();
                let held = body.take.get_or_insert(want);
                for (h, w) in held.iter_mut().zip(&want) {
                    for c in 0..3 {
                        h[c] += (w[c] - h[c]) * k;
                    }
                }
                m.captured = Some(model.captured(held));
            } else {
                m.feel = feel(t, visit.phase, body.speed[2]);
            }

            let (mut pose, place) = arrange(model, m, size);
            // The fingers follow their bends through springs, but never into
            // the cloth: the wall's answer is a bound, not a suggestion.
            for d in 0..5 {
                let target = pose.curl[d];
                if body.curl[d].is_nan() {
                    body.curl[d] = target;
                }
                spring(&mut body.curl[d], &mut body.curling[d], target, FINGER_SPRING, dt);
                let bound = match m.kind {
                    Kind::Stop => body.curl[d].min(target),
                    Kind::Tips => body.curl[d].max(target),
                };
                if bound != body.curl[d] {
                    body.curl[d] = bound;
                    body.curling[d] = 0.0;
                }
                pose.curl[d] = bound;
            }
            let bones = model.bones(&pose);
            model.skin(&bones, &place, &mut self.skinned);
            for tri in &model.triangles {
                let [a, b, c] = tri.map(|i| self.skinned[i as usize]);
                self.cloth.press(a, b, c);
            }
        }
        self.cloth.step(dt, ctx.get("stiff"));
        self.cloth.shine();

        let colour = oklch(0.97, ctx.get("tint"), ctx.get("hue"));
        let pallor = ctx.get("pallor");
        let cloth = &self.cloth;
        let (glow_w, sheen_w) = (ctx.get("glow"), ctx.get("sheen"));
        let height = |x: f32, y: f32| {
            let (h, touch, _) = cloth.at(x, y);
            (h, touch)
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
            let glow = cloth.at(x, y).2;
            let light = glow * glow_w
                + (key * 0.8 + gleam * 0.45 + rim * 0.12) * open * sheen_w
                + crate::color::smoothstep(0.3, 1.0, touch) * 0.3 * pallor;
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

    /// Longest a visit should run, and time left after it for the cloth.
    const VISIT: f32 = 30.0;
    const SETTLE: f32 = 6.0;

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
    fn the_first_slot_shows_a_hand_early() {
        let v = Visit::plan(0, 0.9);
        assert_eq!(v.kind, Kind::Tips);
        assert!((1..15).any(|t| v.moment(t as f32).is_some_and(|m| m.tip.2 > 0.2)));
    }

    #[test]
    fn visits_move_differently() {
        let shapes: std::collections::HashSet<(usize, u32)> =
            (1..60).map(|s| Visit::plan(s, 0.5)).map(|v| (v.keys.len(), v.keys.last().unwrap().t as u32)).collect();
        assert!(shapes.len() >= 10, "only {} different paths", shapes.len());
    }

    #[test]
    fn the_arm_runs_back_into_the_box() {
        let model = Model::get();
        for slot in 0..120 {
            let v = Visit::plan(slot, 0.61);
            for i in 0..60 {
                let Some(mut m) = v.moment(i as f32 * 0.5) else { continue };
                m.feel = feel(i as f32 * 0.5, v.phase, 0.2);
                if i % 2 == 0 {
                    m.captured = Some(model.captured(&clips::all()[slot as usize % clips::COUNT].at(i as f32 * 0.5)));
                }
                let (pose, place) = arrange(model, &m, 0.11);
                let bones = model.bones(&pose);
                let (w, k) = (place.apply(model.joint(&bones, hand::WRIST)), place.apply(model.joint(&bones, 11)));
                let arm = norm([w[0] - k[0], w[1] - k[1], w[2] - k[2]]);
                assert!(-arm[2] >= (ARM + 0.05).cos(), "slot {slot} {:?}: the arm is {:.2} off straight back", m.kind, (-arm[2]).acos());
            }
        }
    }

    #[test]
    fn every_kind_comes() {
        let kinds: Vec<Kind> = (1..1000).map(|s| Visit::plan(s, 0.5).kind).collect();
        let count = |k| kinds.iter().filter(|&&x| x == k).count();
        assert!(count(Kind::Stop) > 400 && count(Kind::Tips) > 400, "{} {}", count(Kind::Stop), count(Kind::Tips));
    }
}
