//! Veil: something living inside the frame comes up to the glass. Made for
//! the 32x32 Art Frame, whose white diffuser is frosted glass: what is
//! behind it is seen only by its own light, sharp where it touches and soft
//! and faint further back (`super::glass`).
//!
//! What comes is a real hand (`hand.rs`, a skinned model posed by bending
//! its fingers), always the whole hand, like something living in the box:
//! held up palm first (fingertips first was tried: at 32 LEDs it is a
//! lump), its
//! fingers working all the while as a captured take of a real hand moved
//! them (`clips.rs`). This file is the
//! choreography - one visit after another, each planned from the seed and
//! its number as a path of gestures picked at random - and the light.
//!
//! Everything is composed in the centre square, x in -1..1 of the frame's
//! -2..2, which is what the Art Frame shows of the 64x32 frame.

mod clips;
mod hand;

use crate::frame::Frame;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef};
use crate::rng::Rng;
use super::glass::{self, Glass};
use hand::{Model, Pose, Rigid};
use std::f32::consts::{PI, TAU};

pub const DEF: PatchDef = PatchDef {
    id: "veil",
    name: "Veil",
    blurb: "A hand inside the frame comes up to the frosted glass and draws back: a posed 3D hand, its fingers moving as a captured hand moved. Made for the 32x32 Art Frame.",
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
    param("size", "Finger size", 0.05, 0.35, 0.005, 0.11),
    param("pause", "Stillness between visits (s)", 0.0, 60.0, 1.0, 12.0),
    param("steps", "Palette steps", 2.0, 200.0, 1.0, 120.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
    param("speed", "Speed of the whole visit (1: the fingers as fast as they were captured)", 0.25, 1.0, 0.01, 0.6),
    param("depth", "How far behind the glass the hand stays clear", 0.1, 4.0, 0.01, 1.0),
    param("take", "Captured take (0: a different one each visit)", 0.0, clips::COUNT as f32, 1.0, 0.0),
    param("ease", "How slowly the fingers follow a take (s)", 0.0, 1.0, 0.01, 0.2),
];

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Veil { seed: Rng::new(seed).f32(), glass: Glass::new(), skinned: Vec::new(), visit: None, clock: 0.0, body: None, dark: true })
}

/// The finger size the choreography's depths and spacings are written for;
/// at any other size they scale with it, so a smaller finger presses in
/// proportion and its neighbour stays a finger's width away.
const DRAWN_AT: f32 = 0.19;

/// Where the tip is at rest, far back in the box, and we are +z; the hand
/// comes as near as a visit's `near`, where the diffuser shows it sharpest.
/// A visit starts and ends further back than this if that is what it takes
/// for its light to have gone entirely (see `Visit::far`).
const AWAY: f32 = -1.4;

/// How much further than `glass::GONE` a visit goes, to be sure.
const BEYOND: f32 = 0.05;

/// How long a visit may wait, after its path is over, for the glass to go
/// dark (in its own seconds), should `depth` have grown while it was out.
const DARK_WAIT: f32 = 10.0;

/// Where the first visit's hand starts: near enough to glow faintly.
const FIRST: f32 = -0.5;

/// After a visit, this long in the dark before `pause`.
const REST: f32 = 0.5;

/// How far from the centre anything presses: the Art Frame shows x and y in
/// -1..1, and the glow spreads a little way out from the hand.
const BOUND: f32 = 0.6;

/// The furthest the forearm may swing off pointing straight back into the
/// box. The model ends at the wrist, and its light fades out before it (see
/// `Model::light`), but turned any more towards us the hand would look
/// propped on nothing.
const ARM: f32 = 1.4;

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
    keys: Vec<Key>,
    /// The way the fingers point, in the sheet, and a phase for its slow
    /// turn.
    toward: f32,
    phase: f32,
    /// A hand's size, as a multiple of life size.
    span: f32,
    /// How near the hand comes: the diffuser shows it sharpest there, at
    /// `DRAWN_AT`.
    near: f32,
    /// How far a `Stop` hand tips its fingers towards us off the sheet's
    /// plane, so its wrist and the arm behind it run back into the dark.
    pitch: f32,
    /// The captured take its fingers play, how fast (never faster than it
    /// was captured), and from where in it.
    take: usize,
    rate: f32,
    from: f32,
    /// Where the hand starts and where it goes back to: far enough behind
    /// the glass that none of its light comes through.
    far: f32,
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
    /// `reach` is how far behind the glass (in path z) the hand's light has
    /// gone entirely, for the size and depth it is drawn at.
    fn plan(slot: u64, seed: f32, reach: f32) -> Self {
        let mut rng = Rng::new(((seed * 1e9) as u64) ^ slot.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // The first slot comes soon: whoever starts the patch should see what
        // it is.
        let start = if slot == 0 { 0.0 } else { rng.range(0.0, 0.5) };
        let mut v = Visit {
            keys: Vec::new(),
            // Fingers up, give or take: the arm is below.
            toward: PI / 2.0 + rng.range(-0.3, 0.3),
            phase: rng.range(0.0, TAU),
            span: rng.range(0.78, 0.9),
            near: rng.range(0.22, 0.3),
            // Nearly flat to the glass, as a hand held up to it.
            pitch: PI / 2.0 - ARM + rng.range(0.03, 0.25),
            take: clips::ROTATION[(rng.u64() % clips::ROTATION.len() as u64) as usize],
            rate: rng.range(0.85, 1.0),
            from: rng.range(0.0, 20.0),
            far: AWAY,
        };
        v.far = (v.near - reach - BEYOND).min(AWAY);
        v.keys = Self::approach(&mut rng, start, v.near, v.far).keys;
        if slot == 0 {
            // Already faintly there when the patch starts, not far back in
            // the dark.
            v.keys[0].z = FIRST;
        }
        v
    }

    /// A hand that comes up near out of the dark at `far`, holds, perhaps
    /// eases back and comes again, and always goes the same way: drawn back
    /// away from us until none of it shows, never fading out in place.
    fn approach(rng: &mut Rng, start: f32, near: f32, far: f32) -> Path {
        let (x, y) = (rng.range(-0.1, 0.1), rng.range(-0.15, 0.05));
        let mut p = Path { keys: vec![Key { t: start, x, y, z: far, ease: Ease::Gentle }] };
        let full = near;
        // The timings are written for a hand that comes from `AWAY`; from
        // further back it takes longer, so it moves as fast where it shows -
        // up to a point: at the deepest depths and smallest sizes it is a
        // long way back, and moves faster through the dark.
        let further = ((near - far) / (near - AWAY)).min(1.6);
        p.to(rng.range(4.0, 7.0) * further, x, y, full, Ease::Gentle);
        p.stay(rng.range(1.5, 3.0));
        if rng.f32() < 0.4 {
            p.to(rng.range(1.5, 2.5), x, y, full - rng.range(0.08, 0.15), Ease::Gentle);
            p.to(rng.range(1.5, 2.5), x, y, full, Ease::Gentle);
            p.stay(rng.range(1.0, 2.0));
        }
        // Snatched back, or sinking slowly away.
        let k = p.last();
        if rng.f32() < 0.4 {
            p.to(rng.range(0.7, 1.6) * further, k.x, k.y, far, Ease::Snatch);
        } else {
            p.to(rng.range(2.5, 4.0) * further, k.x, k.y, far, Ease::Gentle);
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
            tip: (x, y, z + breath),
            toward: self.toward + turn * 0.4,
            span: self.span,
            near: self.near,
            pitch: self.pitch,
            captured: None,
        })
    }
}

/// One instant of a visit.
struct Moment {
    /// The front-most point of the hand: x, y on the sheet, z at `DRAWN_AT`.
    tip: (f32, f32, f32),
    toward: f32,
    span: f32,
    near: f32,
    pitch: f32,
    /// How the captured take has the fingers; until then, the hand at rest.
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
    let scale = LIFE * size * m.span;
    // A captured take bends the fingers instead of the pose.
    let feeling = |p: Pose| match m.captured {
        Some(c) => Pose { curl: [0.0; 5], spread: 0.0, captured: Some(c) },
        None => p,
    };
    // The hand is placed by its pose at rest, never by where its moving
    // fingers are, so a finger's bend cannot carry the hand with it; how
    // near it comes is settled after, slowly, by its nearest point.
        // Palm to us, fingers up the sheet and tipped towards us, so the
        // wrist runs back and down to the arm; arched forward at rest.
        let (up, (s, c)) = ([m.toward.cos(), m.toward.sin(), 0.0], m.pitch.sin_cos());
        let along = [up[0] * c, up[1] * c, s];
        let facing = [-up[0] * s, -up[1] * s, c];
        let turn = Rigid::between(fingers, palm, along, facing).scaled(scale);
        let arched = Pose { curl: [0.15, 0.32, 0.3, 0.32, 0.36], spread: 0.1, captured: None };
        // The knuckles come to the push.
        let knuckle = turn.apply(model.joint(&model.bones(&arched), 11));
        let place = turn.moved([m.tip.0 - knuckle[0], m.tip.1 - knuckle[1], m.tip.2 * k - knuckle[2]]);
        (feeling(arched), place)
}

struct Veil {
    seed: f32,
    glass: Glass,
    skinned: Vec<[f32; 3]>,
    /// The visit under way: its number, when it began, and its plan. The next
    /// begins as soon as this one has settled and `pause` has passed.
    visit: Option<(u64, f32, Visit)>,
    /// How far into the visit it is, in its own time: seconds at `speed`.
    clock: f32,
    /// The hand as a body with weight, following its path; `None` between
    /// visits.
    body: Option<Body>,
    /// Whether the last frame showed nothing of the hand: a visit is over
    /// only when it has gone entirely.
    dark: bool,
}

/// The hand's own motion: where it is and how fast it moves, following the
/// choreography through a critically damped spring, so the hand has weight -
/// it eases in and out - but never bounces.
struct Body {
    at: [f32; 3],
    speed: [f32; 3],
    /// Where the fingers have got to in the captured take they follow.
    take: Option<[[f32; 3]; 21]>,
    /// How far the hand is moved towards us to bring its nearest point
    /// where the path has it.
    lift: Option<f32>,
}

/// A spring's stiffness (as an angular frequency) and damping ratio.
const HAND_SPRING: (f32, f32) = (6.0, 1.0);

/// How slowly the hand follows its nearest point (s): slower than a
/// finger moves, so the hand never twitches with one.
const LIFT_EASE: f32 = 0.8;

/// The fastest any bone of a finger turns, radians a second of the visit's
/// own clock (so a quarter of this at the slowest `speed`): hardly ever
/// slower than the captured hand, but a check on the moments the model
/// sweeps a whole finger where the hand only moved a knuckle. A cap on how
/// fast joints move is not enough: a fingertip's last bone is short, so a
/// little drift at either end swings it a long way.
const BEND_SPEED: f32 = 3.0;

/// Each finger's bones, as MediaPipe numbers its joints, root to tip.
const FINGER_BONES: [[usize; 4]; 5] = [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16], [17, 18, 19, 20]];

/// Holds back any bone of `now` that has turned more than `most` radians
/// since `was`, carrying the rest of its finger with it.
fn unflick(was: &[[f32; 3]; 21], now: &mut [[f32; 3]; 21], most: f32) {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let len = |a: [f32; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-6);
    for finger in FINGER_BONES {
        for k in 0..3 {
            let (a, b) = (finger[k], finger[k + 1]);
            let (old, new) = (sub(was[b], was[a]), sub(now[b], now[a]));
            let (lo, ln) = (len(old), len(new));
            let (o, n) = (old.map(|x| x / lo), new.map(|x| x / ln));
            let angle = (o[0] * n[0] + o[1] * n[1] + o[2] * n[2]).clamp(-1.0, 1.0).acos();
            if angle <= most {
                continue;
            }
            // Turned only `most` of the way from where it was.
            let u = most / angle;
            let (wa, wb) = (((1.0 - u) * angle).sin() / angle.sin(), (u * angle).sin() / angle.sin());
            let dir: [f32; 3] = std::array::from_fn(|c| o[c] * wa + n[c] * wb);
            let shift: [f32; 3] = std::array::from_fn(|c| now[a][c] + dir[c] * ln - now[b][c]);
            for &j in &finger[k + 1..] {
                for c in 0..3 {
                    now[j][c] += shift[c];
                }
            }
        }
    }
}

/// One step of a damped spring pulling `x` (moving at `v`) towards `to`.
fn spring(x: &mut f32, v: &mut f32, to: f32, (w, zeta): (f32, f32), dt: f32) {
    *v += (w * w * (to - *x) - 2.0 * zeta * w * *v) * dt;
    *x += *v * dt;
}

impl Patch for Veil {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let t = ctx.t as f32;
        let dt = (ctx.dt as f32).clamp(0.0, 0.1);
        // Everything in a visit runs on its own clock, at `speed`; the pause
        // after it is in real seconds.
        let pace = ctx.get("speed");
        self.clock += dt * pace;
        let next = match &self.visit {
            // Time went backwards (a restart): begin again.
            Some((_, start, _)) if t < *start => Some(0),
            Some((n, _, v)) if self.clock >= v.keys.last().unwrap().t + REST + ctx.get("pause") * pace && (self.dark || self.clock >= v.keys.last().unwrap().t + DARK_WAIT) => Some(n + 1),
            Some(_) => None,
            None => Some(0),
        };
        if let Some(n) = next {
            let reach = glass::GONE * ctx.get("depth") * DRAWN_AT / ctx.get("size");
            self.visit = Some((n, if n == 0 { 0.0 } else { t }, Visit::plan(n, self.seed, reach)));
            self.clock = 0.0;
            // A new visit is a new hand: nothing carries over.
            self.body = None;
        }
        let (_, _, visit) = self.visit.as_ref().unwrap();
        let local = self.clock;
        let mut moment = visit.moment(local);
        let size = ctx.get("size");
        let model = Model::get();

        self.glass.clear();
        let glass_at = moment.as_ref().map_or(0.0, |m| m.near * size / DRAWN_AT);
        if moment.is_none() {
            self.body = None;
        }
        if let Some(m) = &mut moment {
            // The body follows the path, with a faint tremor: something alive
            // holding as still as it can.
            let tremor = [
                0.006 * ((local * 5.1 + visit.phase).sin() + 0.6 * (local * 8.3 + 2.0 * visit.phase).sin()),
                0.006 * ((local * 4.3 + 3.0 * visit.phase).sin() + 0.6 * (local * 7.7 + visit.phase).sin()),
                0.004 * (local * 6.1 + 5.0 * visit.phase).sin(),
            ];
            let goal = [m.tip.0 + tremor[0], m.tip.1 + tremor[1], m.tip.2 + tremor[2]];
            let body = self.body.get_or_insert(Body { at: goal, speed: [0.0; 3], take: None, lift: None });
            for k in 0..3 {
                spring(&mut body.at[k], &mut body.speed[k], goal[k], HAND_SPRING, dt);
            }
            m.tip = (body.at[0], body.at[1], body.at[2]);
            let k = 1.0 - (-dt / ctx.get("ease").max(1e-3)).exp();
            let all = clips::all();
            let pick = ctx.get("take") as usize;
            let take = if pick >= 1 { &all[(pick - 1).min(all.len() - 1)] } else { &all[visit.take] };
            let want = take.at(local * visit.rate + visit.from);
            // The fingers ease after the take rather than snapping to it,
            // and no bone of them ever turns faster than `BEND_SPEED`.
            let held = body.take.get_or_insert(want);
            let was = *held;
            for (h, w) in held.iter_mut().zip(&want) {
                for c in 0..3 {
                    h[c] += (w[c] - h[c]) * k;
                }
            }
            unflick(&was, held, BEND_SPEED * pace * dt);
            m.captured = Some(model.captured(held));

            let (pose, place) = arrange(model, m, size);
            let bones = model.bones(&pose);
            model.skin(&bones, &place, &mut self.skinned);
            // The hand's nearest point, whatever its fingers are doing, comes
            // where the path says; the hand follows that slowly, so a finger
            // that bends never pulls the hand along with it.
            let front = self.skinned.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
            let want = m.tip.2 * size / DRAWN_AT - front;
            let lift = body.lift.get_or_insert(want);
            *lift += (want - *lift) * (1.0 - (-dt / LIFT_EASE).exp());
            for p in &mut self.skinned {
                p[2] += *lift;
            }
            for tri in &model.triangles {
                let [a, b, c] = tri.map(|i| self.skinned[i as usize]);
                self.glass.press(a, b, c, tri.map(|i| model.light[i as usize]));
            }
        }
        self.glass.shine(glass_at, ctx.get("depth"));
        self.dark = self.glass.dark();

        self.glass.picture(&glass::Look::from(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(a: [f32; 3]) -> [f32; 3] {
        let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
        [a[0] / l, a[1] / l, a[2] / l]
    }

    /// How far back the light goes at the default size and depth.
    const REACH: f32 = glass::GONE * DRAWN_AT / 0.11;

    /// Longest a visit should run, and time left after it.
    const VISIT: f32 = 30.0;
    const SETTLE: f32 = 6.0;

    #[test]
    fn every_visit_is_over_before_its_slot_ends() {
        for slot in 0..500 {
            let v = Visit::plan(slot, 0.37, REACH);
            let end = v.keys.last().unwrap().t;
            assert!(end + SETTLE <= VISIT, "slot {slot}: ends at {end}");
            assert!(v.moment(VISIT).is_none_or(|m| m.tip.2 <= v.far), "slot {slot}: still pressing at the end");
        }
    }

    #[test]
    fn the_first_slot_shows_a_hand_early() {
        let v = Visit::plan(0, 0.9, REACH);
        assert!((1..15).any(|t| v.moment(t as f32).is_some_and(|m| m.tip.2 > 0.2)));
    }

    #[test]
    fn visits_move_differently() {
        let shapes: std::collections::HashSet<(usize, u32)> =
            (1..60).map(|s| Visit::plan(s, 0.5, REACH)).map(|v| (v.keys.len(), v.keys.last().unwrap().t as u32)).collect();
        assert!(shapes.len() >= 10, "only {} different paths", shapes.len());
    }

    #[test]
    fn the_arm_runs_back_into_the_box() {
        let model = Model::get();
        for slot in 0..120 {
            let v = Visit::plan(slot, 0.61, REACH);
            for i in 0..60 {
                let Some(mut m) = v.moment(i as f32 * 0.5) else { continue };
                if i % 2 == 0 {
                    m.captured = Some(model.captured(&clips::all()[slot as usize % clips::COUNT].at(i as f32 * 0.5)));
                }
                let (pose, place) = arrange(model, &m, 0.11);
                let bones = model.bones(&pose);
                let (w, k) = (place.apply(model.joint(&bones, hand::WRIST)), place.apply(model.joint(&bones, 11)));
                let arm = norm([w[0] - k[0], w[1] - k[1], w[2] - k[2]]);
                assert!(-arm[2] >= (ARM + 0.05).cos(), "slot {slot}: the arm is {:.2} off straight back", (-arm[2]).acos());
            }
        }
    }

    /// The hand's shape never changes suddenly: against its own centre (so
    /// a snatch back does not count), no point of it moves more than a
    /// little in a frame - a little more the faster it plays, up to as fast
    /// as the captured fingers really moved.
    #[test]
    fn the_hand_never_jumps() {
        let mut params = crate::patch::Params::defaults(PARAMS);
        params.set(PARAMS, "pause", 0.0);
        params.set(PARAMS, "samples", 1.0);
        for (seed, speed) in [(3, 1.0), (62, 1.0), (7, 0.6), (62, 0.25), (101, 0.25)] {
            params.set(PARAMS, "speed", speed);
            let mut veil = Veil { seed: Rng::new(seed).f32(), glass: Glass::new(), skinned: Vec::new(), visit: None, clock: 0.0, body: None, dark: true };
            let mut last: Option<Vec<[f32; 3]>> = None;
            let mut worst = (0.0f32, 0.0);
            for i in 1..30 * 80 {
                let t = i as f64 / 30.0;
                veil.render(&Ctx { t, dt: 1.0 / 30.0, now: 0.0, params: &params });
                let here = if veil.body.is_some() && !veil.skinned.is_empty() { Some(centred(&veil.skinned)) } else { None };
                if let (Some(a), Some(b)) = (&last, &here) {
                    let moved = a.iter().zip(b).map(|(p, q)| dist(p, q)).fold(0.0, f32::max);
                    if moved > worst.0 {
                        worst = (moved, t);
                    }
                }
                last = here;
            }
            let what = format!("seed {seed} at speed {speed}");
            assert!(worst.0 < 0.05 * speed / 0.25, "{what}: the hand's shape moved {:.3} units in a frame at {:.1} s", worst.0, worst.1);
        }
    }

    /// Every visit ends with the hand drawn back until none of it shows: the
    /// glass is dark before the next visit begins and as it begins, so
    /// nothing is ever cut off, at any depth or size.
    #[test]
    fn every_visit_ends_in_the_dark() {
        let mut params = crate::patch::Params::defaults(PARAMS);
        params.set(PARAMS, "samples", 1.0);
        for (seed, pause, depth, size, speed) in [(5, 2.0, 1.0, 0.11, 0.6), (9, 0.0, 1.0, 0.11, 1.0), (13, 0.0, 3.0, 0.11, 1.0), (21, 0.0, 4.0, 0.05, 1.0), (34, 1.0, 0.1, 0.35, 0.6)] {
            for (name, v) in [("pause", pause), ("depth", depth), ("size", size), ("speed", speed)] {
                params.set(PARAMS, name, v);
            }
            let mut veil = Veil { seed: Rng::new(seed).f32(), glass: Glass::new(), skinned: Vec::new(), visit: None, clock: 0.0, body: None, dark: true };
            let (mut was, mut prev, mut changes) = (true, None, 0);
            for i in 1..30 * 240 {
                veil.render(&Ctx { t: i as f64 / 30.0, dt: 1.0 / 30.0, now: 0.0, params: &params });
                let n = veil.visit.as_ref().map(|v| v.0);
                if prev.is_some() && n != prev {
                    let what = format!("seed {seed}, depth {depth}, size {size}: visit {prev:?}");
                    assert!(was, "{what} still showed when the next began");
                    assert!(veil.dark, "{what}: the next one began already showing");
                    changes += 1;
                }
                (was, prev) = (veil.dark, n);
            }
            assert!(changes >= 3, "seed {seed}: only {changes} visits in 4 minutes");
        }
    }

    fn dist(p: &[f32; 3], q: &[f32; 3]) -> f32 {
        ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
    }

    fn centred(v: &[[f32; 3]]) -> Vec<[f32; 3]> {
        let n = v.len() as f32;
        let c: [f32; 3] = std::array::from_fn(|k| v.iter().map(|p| p[k]).sum::<f32>() / n);
        v.iter().map(|p| std::array::from_fn(|k| p[k] - c[k])).collect()
    }
}

