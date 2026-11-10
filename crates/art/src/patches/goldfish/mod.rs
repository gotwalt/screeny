//! Goldfish: a few goldfish in a tank, seen through a small window in its
//! wall - the Art Frame's frosted glass. The tank goes on past the frame on
//! either side and back into the dark, so a fish swims into view and out of
//! it, sharp where it comes near the glass and soft and faint as it goes
//! back, until the dark behind takes it.
//!
//! The fish are modelled and animated goldfish ("Comet goldfish" and
//! "Ryukin goldfish" by somitsu, CC BY 4.0, see `goldfish.LICENSE`): each
//! plays its own swimming, faster as it swims faster, and this moves it
//! about the tank - wandering, turning, keeping off the walls and off the
//! others, now and then hanging still or, restless, darting.
//!
//! It is seen as dove and veil are (`super::glass`): by its own light.

use super::glass::{self, Glass};
use super::skinned::Model;
use crate::frame::Frame;
use crate::patch::{choice, param, Ctx, ParamSpec, Patch, PatchDef};
use crate::rng::Rng;
use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

pub const DEF: PatchDef = PatchDef {
    id: "goldfish",
    name: "Goldfish",
    blurb: "A few goldfish in a tank, seen through a window in its wall: animated 3D goldfish (\"Comet goldfish\" and \"Ryukin goldfish\" by somitsu, CC BY 4.0) swimming into view and out of it, sharp only where they near the glass. Made for the 32x32 Art Frame.",
    params: PARAMS,
    make,
    seeded: true,
};

const KINDS: &[&str] = &["comets", "ryukins", "both"];

const PARAMS: &[ParamSpec] = &[
    choice("kind", "Fish", KINDS, 2.0),
    param("count", "How many fish", 1.0, 8.0, 1.0, 3.0),
    param("agitation", "How restless they are", 0.0, 1.0, 0.01, 0.3),
    param("tempo", "How fast time runs in the tank (1: as it is, less: slow motion)", 0.2, 1.5, 0.01, 0.6),
    param("size", "A fish's length, of the frame's width", 0.2, 1.2, 0.01, 0.7),
    param("tank", "The tank's width, in frames", 1.0, 5.0, 0.1, 2.5),
    param("deep", "How far back the tank goes (1: as far as the glass shows)", 0.3, 1.5, 0.01, 1.0),
    param("fins", "How much light comes through the fins (1: as much as the body)", 0.0, 1.0, 0.01, 0.3),
    param("depth", "How far behind the glass a fish stays clear", 0.1, 4.0, 0.01, 2.2),
    param("glow", "The fish's light through the glass", 0.0, 2.0, 0.01, 1.0),
    param("hue", "Hue", 0.0, 360.0, 1.0, 55.0),
    param("tint", "Tint (0: white)", 0.0, 0.12, 0.001, 0.09),
    param("steps", "Palette steps", 2.0, 200.0, 1.0, 120.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
];

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Tank::new(seed))
}

/// The Art Frame's width in sheet units (32 LEDs).
const FRAME: f32 = 2.16;
/// How far above and below the frame the tank goes, in frames.
const TALL: f32 = 1.3;

/// The two fish, and what they are like: the middle of each, and its length
/// nose to tail, read off the first moment of its swimming.
struct Kind {
    model: Model,
    centre: [f32; 3],
    length: f32,
    /// Half its width, fin to fin, as a share of its length (its tail sweeps
    /// further).
    flank: f32,
    /// Which of its points are fin rather than body.
    fin: Vec<bool>,
}

fn kinds() -> &'static [Kind; 2] {
    static KINDS: OnceLock<[Kind; 2]> = OnceLock::new();
    KINDS.get_or_init(|| {
        [include_bytes!("comet.glb").as_slice(), include_bytes!("ryukin.glb").as_slice()].map(|glb| {
            let model = Model::read(glb);
            let mut points = Vec::new();
            model.skin(&model.pose(0.0, 0.0, 0.0), &mut points);
            // Both models: x across the fish, y up, z forward (nose at +z).
            let (lo, hi) = points.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| (std::array::from_fn(|k| lo[k].min(p[k])), std::array::from_fn(|k| hi[k].max(p[k]))));
            let centre: V3 = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
            let length = hi[2] - lo[2];
            let flank = (hi[0] - lo[0]) / 2.0 / length;
            let fin = model.part.iter().map(|&m| model.meshes[m as usize].starts_with("fin")).collect();
            Kind { centre, length, flank, fin, model }
        })
    })
}

type V3 = [f32; 3];

fn add(a: V3, b: V3) -> V3 {
    std::array::from_fn(|k| a[k] + b[k])
}
fn sub(a: V3, b: V3) -> V3 {
    std::array::from_fn(|k| a[k] - b[k])
}
fn scale(a: V3, s: f32) -> V3 {
    a.map(|x| x * s)
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: V3) -> V3 {
    scale(a, 1.0 / dot(a, a).sqrt().max(1e-9))
}

/// The steepest a fish climbs or dives (radians).
const MOST_PITCH: f32 = 0.4;

/// The longest step the water is worked out in (seconds of tank time): a
/// frame is cut into as many of these as it takes, so the swimming is the
/// same at any frame rate and the contacts never overshoot.
const STEP: f32 = 1.0 / 120.0;

/// A heading's angle about the up-down axis (from +x towards +z) and up or
/// down.
fn angles(h: V3) -> (f32, f32) {
    (h[2].atan2(h[0]), h[1].clamp(-1.0, 1.0).asin())
}

fn heading(yaw: f32, pitch: f32) -> V3 {
    let (sp, cp) = pitch.sin_cos();
    [yaw.cos() * cp, sp, yaw.sin() * cp]
}

/// The nearest points of segments `a0..a1` and `b0..b1`, as how far along
/// each (0..1).
fn nearest(a0: V3, a1: V3, b0: V3, b1: V3) -> (f32, f32) {
    let (d1, d2, r) = (sub(a1, a0), sub(b1, b0), sub(a0, b0));
    let (a, e, f) = (dot(d1, d1), dot(d2, d2), dot(d2, r));
    let (c, b) = (dot(d1, r), dot(d1, d2));
    let denom = a * e - b * b;
    let mut s = if denom > 1e-9 { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
    let mut t = (b * s + f) / e.max(1e-9);
    if t < 0.0 {
        t = 0.0;
        s = (-c / a.max(1e-9)).clamp(0.0, 1.0);
    } else if t > 1.0 {
        t = 1.0;
        s = ((b - c) / a.max(1e-9)).clamp(0.0, 1.0);
    }
    (s, t)
}

/// The turn about the up-down axis that a push `force` at `lever` from a
/// fish's middle gives it (towards larger yaw), for a fish `long` long: a
/// fish is a rod, hardest to turn about its middle.
fn twist(lever: V3, force: V3, long: f32) -> f32 {
    // Yaw grows from +x towards +z, which is a turn about -y.
    -0.3 * (lever[2] * force[0] - lever[0] * force[2]) / (long * long / 12.0)
}

struct Fish {
    kind: usize,
    /// Its own size and pace, a little either side of the setting's.
    size: f32,
    pace: f32,
    /// Where its middle is and how it is moving (sheet units a second).
    at: V3,
    vel: V3,
    /// Which way it faces - about the up-down axis, and up or down - and how
    /// fast each is changing (radians a second). It turns as a body in water
    /// does: gathering its turn and easing out of it, never snapping.
    yaw: f32,
    pitch: f32,
    spin: f32,
    climb: f32,
    /// What it is turning towards this frame, and how fast it is swimming
    /// for (sheet units a second).
    aim_yaw: f32,
    aim_pitch: f32,
    aim_speed: f32,
    /// The aim as it turns towards it, eased: a new wish comes on gradually.
    toward: f32,
    /// Where it would like to go, how fast (a share of its cruising
    /// speed), and until when it keeps that wish.
    goal: V3,
    eager: f32,
    until: f32,
    /// How far through its swimming it is (0..1), and how far it leans into
    /// a turn (radians).
    phase: f32,
    roll: f32,
    /// Which way it turned last (radians).
    turned: f32,
    /// On its way out of the tank, the setting having fewer fish now.
    leaving: bool,
}

impl Fish {
    fn heading(&self) -> V3 {
        heading(self.yaw, self.pitch)
    }
}

struct Tank {
    rng: Rng,
    glass: Glass,
    fish: Vec<Fish>,
    skinned: Vec<V3>,
    /// The points pressed against the glass this frame, every fish's.
    placed: Vec<V3>,
    /// The tank's own time, which runs at `tempo`.
    clock: f32,
    last: f64,
    started: bool,
}

/// The tank: its half-width and half-height, and how far back it goes, for
/// these settings. It goes back a little past where the glass shows anything.
struct Bounds {
    wide: f32,
    tall: f32,
    back: f32,
}

impl Bounds {
    fn of(ctx: &Ctx) -> Bounds {
        let half = FRAME / 2.0;
        Bounds { wide: half * ctx.get("tank").max(1.0), tall: half * TALL, back: glass::GONE * ctx.get("depth") * ctx.get("deep") }
    }
}

impl Tank {
    fn new(seed: u64) -> Self {
        Tank { rng: Rng::new(seed ^ 0x901d), glass: Glass::new(), fish: Vec::new(), skinned: Vec::new(), placed: Vec::new(), clock: 0.0, last: 0.0, started: false }
    }

    /// A new fish: anywhere in the tank at the start, and from the dark at
    /// the back of it later, so it never appears out of nothing.
    fn spawn(&mut self, n: usize, kind: usize, tank: &Bounds, length: f32) -> Fish {
        let r = &mut self.rng;
        let kind = if kind == 2 { n % 2 } else { kind };
        // Clear of the others: the best of a few places.
        let mut at = [0.0; 3];
        let mut best = f32::MIN;
        for _ in 0..24 {
            let z = if self.started { -tank.back - length } else { -r.range(0.15, 0.8) * tank.back };
            let place = [r.range(-1.0, 1.0) * tank.wide * 0.8, r.range(-1.0, 1.0) * tank.tall * 0.7, z];
            let room = self.fish.iter().map(|f| dot(sub(f.at, place), sub(f.at, place))).fold(f32::MAX, f32::min);
            if room > best {
                (best, at) = (room, place);
            }
        }
        let z = at[2];
        let angle = r.range(0.0, TAU);
        let (yaw, pitch) = angles(unit([angle.cos(), r.range(-0.2, 0.2), 0.5 * angle.sin()]));
        Fish {
            kind,
            size: r.range(0.8, 1.15),
            pace: r.range(0.85, 1.15),
            at,
            vel: [0.0; 3],
            yaw,
            pitch,
            spin: 0.0,
            climb: 0.0,
            aim_yaw: yaw,
            aim_pitch: pitch,
            aim_speed: 0.0,
            toward: yaw,
            goal: [0.0, 0.0, z],
            eager: 1.0,
            until: 0.0,
            phase: r.f32(),
            roll: 0.0,
            turned: 0.0,
            leaving: false,
        }
    }

    /// How far in front of its middle a fish of length `long` and `flank`
    /// heading this way reaches towards the glass: its nose, or its side.
    fn reach(long: f32, flank: f32, heading: V3) -> f32 {
        long * (0.5 * heading[2].max(0.0) + flank)
    }

    /// Each fish decides, once a frame, where it is going and how fast: a
    /// wish, which the water then makes into motion (`flow`).
    fn decide(&mut self, tank: &Bounds, length: f32, agitation: f32) {
        let t = self.clock;
        let cruise = length * (0.05 + 0.35 * agitation);
        // Where each will be a little from now, to steer clear of it early.
        let others: Vec<(V3, V3)> = self.fish.iter().map(|f| (f.at, f.vel)).collect();
        for (i, f) in self.fish.iter_mut().enumerate() {
            let long = length * f.size;
            let flank = kinds()[f.kind].flank;
            let there = sub(f.goal, f.at);
            if t >= f.until || dot(there, there) < (0.6 * long).powi(2) {
                let r = &mut self.rng;
                // Somewhere in the tank, more often near the glass than far
                // back: a goldfish comes up to look out.
                let far = r.f32().powi(5);
                let front = Self::reach(long, flank, [1.0, 0.0, 0.0]) + 0.05 * long;
                // Well off to one side, so it swims mostly along the glass,
                // as goldfish in a tank do: in and out only on the way.
                let (wide, tall) = (tank.wide - (0.6 * long).min(0.4 * tank.wide), tank.tall - (0.4 * long).min(0.4 * tank.tall));
                let x = (f.at[0] + r.sign() * r.range(1.0, 2.5) * long).clamp(-wide, wide);
                f.goal = [x, r.range(-1.0, 1.0) * tall, -front - far * (tank.back - front).max(0.0)];
                // Mostly cruising; calm fish now and then hang nearly still,
                // restless ones now and then dart.
                let roll = r.f32();
                f.eager = if roll < 0.15 * (1.0 - agitation) {
                    0.1
                } else if roll > 1.0 - 0.2 * agitation {
                    2.5
                } else {
                    r.range(0.7, 1.2)
                };
                let calm = if f.eager > 2.0 { 1.2 } else { r.range(5.0, 12.0) * (1.0 - 0.6 * agitation) };
                f.until = t + calm;
            }
            // Away from the walls, the glass and the others, more strongly
            // the nearer it will be in a moment.
            let ahead = add(f.at, scale(f.vel, 1.5));
            let mut want = unit(sub(f.goal, f.at));
            let push = |d: f32, margin: f32| ((margin - d) / margin).max(0.0).powi(2) * 4.0;
            let (side, top) = ((0.8 * long).min(0.4 * tank.wide), (0.5 * long).min(0.4 * tank.tall));
            want[0] += push(tank.wide - ahead[0], side) * -1.0 + push(ahead[0] + tank.wide, side);
            want[1] += push(tank.tall - ahead[1], top) * -1.0 + push(ahead[1] + tank.tall, top);
            // The glass only turns it once it is nearly there: it may swim
            // right up to it.
            let reach = Self::reach(long, flank, f.heading());
            let close = long * 0.35;
            let glass = ((close - (-ahead[2] - reach)) / close).max(0.0).powi(2) * 4.0;
            want[2] += -glass + if f.leaving { 0.0 } else { push(ahead[2] + tank.back, long * 0.6) };
            // The others: where each pair will be nearest, over the next
            // couple of seconds, and how near that is.
            for (j, &(o, ov)) in others.iter().enumerate() {
                if j == i {
                    continue;
                }
                let (dp, dv) = (sub(f.at, o), sub(f.vel, ov));
                let when = (-dot(dp, dv) / dot(dv, dv).max(1e-9)).clamp(0.0, 2.5);
                let apart = add(dp, scale(dv, when));
                let d = dot(apart, apart).sqrt();
                let room = long * 1.4;
                if d < room {
                    let away = if d > 1e-4 { unit(apart) } else { [1.0, 0.0, 0.0] };
                    want = add(want, scale(away, 1.8 * (1.0 - d / room) * (1.0 - when / 4.0)));
                }
            }
            if f.leaving {
                want = [want[0] * 0.3, want[1] * 0.3, -1.0];
            }
            // It turns towards its wish, about the up-down axis and, less,
            // up or down - never steeply: a fish swims level.
            let (want_yaw, want_pitch) = angles(unit(want));
            // Turning right round, it keeps on the way it began, rather
            // than dithering between the two.
            let mut turn = (want_yaw - f.yaw + PI).rem_euclid(TAU) - PI;
            if turn.abs() > 0.8 * PI && f.turned != 0.0 {
                turn = turn.abs() * f.turned.signum();
            }
            f.turned = turn;
            f.aim_yaw = f.yaw + turn;
            f.aim_pitch = want_pitch.clamp(-MOST_PITCH, MOST_PITCH);
            f.aim_speed = cruise * f.pace * f.size * if f.leaving { 1.0 } else { f.eager };
        }
    }

    /// The water, for `dt` seconds: each fish is a body in it, driven by its
    /// tail along the way it faces, slowed by the water - little along its
    /// length, much across it, so it goes where it faces and glides on when
    /// it stops beating - carried a little by the water the others stir,
    /// and pushed off the others, the walls and the glass where it touches
    /// them, which turns it as well as moving it.
    fn flow(&mut self, dt: f32, tank: &Bounds, length: f32, agitation: f32) {
        let n = self.fish.len();
        // How quickly a fish gathers speed and loses it, and its turning: a
        // calm fish leans into a turn over seconds.
        let gather = 1.0 / (2.5 - 1.5 * agitation);
        let across = 4.0;
        let ease = 0.55 + 0.9 * agitation;
        let most_spin = 0.3 + 1.2 * agitation;
        let most_gather = 0.3 + 1.5 * agitation;
        let mut push = vec![[0.0f32; 3]; n];
        let mut turn = vec![0.0f32; n];
        let mut water = vec![[0.0f32; 3]; n];
        let steps = (dt / STEP).ceil().max(1.0) as usize;
        let h = dt / steps as f32;
        for _ in 0..steps {
            // Each fish as a capsule: a rod down its body, as thick as it.
            let bodies: Vec<(V3, V3, f32, f32)> = self
                .fish
                .iter()
                .map(|f| {
                    let long = length * f.size;
                    let half = scale(f.heading(), 0.38 * long);
                    let r = long * kinds()[f.kind].flank.max(0.12);
                    (sub(f.at, half), add(f.at, half), r, long)
                })
                .collect();
            push.iter_mut().for_each(|p| *p = [0.0; 3]);
            turn.iter_mut().for_each(|t| *t = 0.0);
            water.iter_mut().for_each(|w| *w = [0.0; 3]);
            for i in 0..n {
                for j in 0..n {
                    if i == j {
                        continue;
                    }
                    // The water a fish stirs: drawn along behind and beside
                    // it, and pushed aside ahead of it.
                    let (o, d) = (&self.fish[j], sub(self.fish[i].at, self.fish[j].at));
                    let reach = 0.8 * bodies[j].3;
                    let w = (-dot(d, d) / (reach * reach)).exp();
                    if w > 1e-3 {
                        let speed = dot(o.vel, o.vel).sqrt();
                        let ahead = dot(unit(d), unit(o.vel)).max(0.0);
                        water[i] = add(water[i], add(scale(o.vel, 0.35 * w), scale(unit(d), 0.3 * speed * ahead * w)));
                    }
                }
                for j in i + 1..n {
                    let ((a0, a1, ra, la), (b0, b1, rb, lb)) = (bodies[i], bodies[j]);
                    let (s, t) = nearest(a0, a1, b0, b1);
                    let (ca, cb) = (add(a0, scale(sub(a1, a0), s)), add(b0, scale(sub(b1, b0), t)));
                    let gap = sub(ca, cb);
                    let d = dot(gap, gap).sqrt();
                    // A soft cushion of water between them, then their
                    // bodies: a gentle nudge first, firmer the more they
                    // touch.
                    let room = ra + rb + 0.1 * (la + lb);
                    if d < room {
                        let normal = if d > 1e-5 { scale(gap, 1.0 / d) } else { [0.0, 1.0, 0.0] };
                        let deep = (room - d) / room;
                        let closing = dot(sub(self.fish[i].vel, self.fish[j].vel), normal);
                        let f = ((10.0 + 120.0 * deep * deep) * (room - d) - 8.0 * closing).max(0.0);
                        let force = scale(normal, f);
                        push[i] = add(push[i], force);
                        push[j] = sub(push[j], force);
                        turn[i] += twist(sub(ca, self.fish[i].at), force, la);
                        turn[j] += twist(sub(cb, self.fish[j].at), scale(force, -1.0), lb);
                    }
                }
                // The walls, the floor, the back and the glass, at each end
                // of it: firm at the glass, which it may not pass, and
                // softer elsewhere.
                let (a0, a1, r, long) = bodies[i];
                let f = &self.fish[i];
                for end in [a0, a1] {
                    let mut walls = vec![(-end[2], [0.0, 0.0, -1.0], 60.0), (tank.wide - end[0], [-1.0, 0.0, 0.0], 15.0), (end[0] + tank.wide, [1.0, 0.0, 0.0], 15.0), (tank.tall - end[1], [0.0, -1.0, 0.0], 15.0), (end[1] + tank.tall, [0.0, 1.0, 0.0], 15.0)];
                    if !f.leaving {
                        walls.push((end[2] + tank.back + long, [0.0, 0.0, 1.0], 15.0));
                    }
                    for (d, normal, firm) in walls {
                        if d < r {
                            let closing = dot(f.vel, normal);
                            let force = scale(normal, (firm * (r - d) - 4.0 * closing.min(0.0)).max(0.0));
                            push[i] = add(push[i], force);
                            turn[i] += twist(sub(end, f.at), force, long);
                        }
                    }
                }
            }
            for (i, f) in self.fish.iter_mut().enumerate() {
                let forward = f.heading();
                let moving = sub(f.vel, water[i]);
                let along = dot(moving, forward);
                let slip = sub(moving, scale(forward, along));
                let gather = gather * f.pace;
                let thrust = gather * f.aim_speed;
                let accel = add(add(scale(forward, thrust - gather * along), scale(slip, -across)), push[i]);
                f.vel = add(f.vel, scale(accel, h));
                f.at = add(f.at, scale(f.vel, h));
                // Turning: eased in and out, like a spring just stiff
                // enough not to overshoot, and never faster than a fish
                // of its temper turns.
                let ease = ease * f.pace;
                f.toward += (f.aim_yaw - f.toward) * (1.0 - (-h * ease).exp());
                let gathering = (ease * ease * (f.toward - f.yaw) - 2.0 * ease * f.spin).clamp(-most_gather, most_gather);
                f.spin += (gathering + turn[i]) * h;
                f.spin = f.spin.clamp(-most_spin * 1.5, most_spin * 1.5);
                f.yaw += f.spin * h;
                let rise = 0.7 * ease;
                f.climb += (rise * rise * (f.aim_pitch - f.pitch) - 2.0 * rise * f.climb) * h;
                f.pitch = (f.pitch + f.climb * h).clamp(-MOST_PITCH, MOST_PITCH);
                // Nothing goes far past a wall, whatever the push.
                let long = length * f.size;
                f.at[0] = f.at[0].clamp(-tank.wide - long * 0.3, tank.wide + long * 0.3);
                f.at[1] = f.at[1].clamp(-tank.tall - long * 0.1, tank.tall + long * 0.1);
            }
        }
        for f in &mut self.fish {
            // Leaning into a turn: the faster it turns and swims, the more.
            let long = length * f.size;
            let speed = dot(f.vel, f.heading()).max(0.0) / long;
            let lean = (-f.spin * (0.4 + 2.0 * speed)).clamp(-0.5, 0.5);
            f.roll += (lean - f.roll) * (1.0 - (-dt / 0.8).exp());
            // Its tail beats faster the faster it swims, and as it turns.
            let model = &kinds()[f.kind].model;
            let work = (f.aim_speed / long).max(speed) / 0.3;
            let rate = 0.4 + 1.4 * work.min(1.6) + 0.5 * f.spin.abs().min(1.0);
            f.phase = (f.phase + rate * dt / model.length).fract();
        }
    }
}

impl Patch for Tank {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        if ctx.t < self.last {
            // Time went backwards (a restart): begin again.
            self.fish.clear();
            self.started = false;
        }
        self.last = ctx.t;
        let real = (ctx.dt as f32).clamp(0.0, 0.1);
        let dt = real * ctx.get("tempo");
        self.clock += dt;
        let tank = Bounds::of(ctx);
        let length = ctx.get("size") * FRAME;
        let agitation = ctx.get("agitation");
        let kind = (ctx.get("kind") as usize).min(2);

        // As many fish as asked: new ones come from the back, and those no
        // longer wanted swim away into it.
        let count = ctx.get("count") as usize;
        let staying = self.fish.iter().filter(|f| !f.leaving).count();
        for n in staying..count {
            let fish = self.spawn(n, kind, &tank, length);
            self.fish.push(fish);
        }
        for f in self.fish.iter_mut().filter(|f| !f.leaving).skip(count) {
            f.leaving = true;
        }
        self.started = true;
        self.fish.retain(|f| !(f.leaving && f.at[2] < -tank.back - length));

        self.decide(&tank, length, agitation);
        self.flow(dt, &tank, length, agitation);

        // Goldfish fins are sheer: the body shows solid, the fins as a veil.
        let fins = ctx.get("fins");
        self.glass.clear();
        self.placed.clear();
        for f in &mut self.fish {
            let k = &kinds()[f.kind];
            k.model.skin(&k.model.pose(f.phase, 0.0, 0.0), &mut self.skinned);
            // The model's x, y and z (across, up, forward) as the fish heads.
            let forward = f.heading();
            let up = unit(sub([0.0, 1.0, 0.0], scale(forward, forward[1])));
            let across = cross(up, forward);
            let (s, c) = f.roll.sin_cos();
            let (across, up) = (add(scale(across, c), scale(up, s)), sub(scale(up, c), scale(across, s)));
            let to = length * f.size / k.length;
            let start = self.placed.len() as u32;
            self.placed.extend(self.skinned.iter().map(|&p| {
                let q = scale(sub(p, k.centre), to);
                add(f.at, add(scale(across, q[0]), add(scale(up, q[1]), scale(forward, q[2]))))
            }));
            // What of it still reaches the glass - a flick of the tail -
            // eases the whole fish back from it; until then it is pressed
            // against the glass, as sharp as it gets.
            let over = self.placed[start as usize..].iter().map(|p| p[2]).fold(f32::MIN, f32::max);
            if over > 0.0 {
                let back = over * (1.0 - (-real / 0.15).exp());
                f.at[2] -= back;
                f.vel[2] = f.vel[2].min(0.0);
            }
            for tri in &k.model.triangles {
                let [a, b, c] = tri.map(|i| self.placed[(start + i) as usize]);
                self.glass.press(a, b, c, tri.map(|i| if k.fin[i as usize] { fins } else { 1.0 }));
            }
        }
        self.glass.shine(0.0, ctx.get("depth"));
        self.glass.picture(&glass::Look::from(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame;
    use crate::patch::Params;

    fn quick() -> Params {
        let mut p = Params::defaults(PARAMS);
        p.set(PARAMS, "samples", 1.0);
        p
    }

    fn run(params: &Params, seconds: f32, mut each: impl FnMut(&Tank, &Frame, f32)) -> Tank {
        let mut tank = Tank::new(7);
        for i in 1..(seconds * 30.0) as usize {
            let t = i as f64 / 30.0;
            let frame = tank.render(&Ctx { t, dt: 1.0 / 30.0, now: 0.0, params });
            each(&tank, &frame, t as f32);
        }
        tank
    }

    fn lit(frame: &Frame) -> usize {
        (0..frame::N).filter(|&i| (16..48).contains(&(i % frame::W))).filter(|&i| frame.pixel(i).r > 0.02).count()
    }

    /// Over a minute fish come into view, and they move.
    #[test]
    fn fish_come_and_go() {
        let params = quick();
        let (mut most, mut changed) = (0, 0);
        let mut last: Option<Vec<f32>> = None;
        run(&params, 60.0, |_, f, _| {
            most = most.max(lit(f));
            let px: Vec<f32> = (0..frame::N).map(|i| f.pixel(i).r).collect();
            if let Some(a) = &last {
                changed += a.iter().zip(&px).any(|(x, y)| (x - y).abs() > 0.05) as usize;
            }
            last = Some(px);
        });
        assert!(most > 40, "at most {most} LEDs ever lit");
        assert!(changed > 250, "only {changed} frames changed");
    }

    /// Every kind, count and agitation: the fish stay in the tank and none
    /// comes through the glass, more than a flick of its tail.
    #[test]
    fn the_fish_stay_in_the_tank() {
        for (kind, count, agitation, tank) in [(0.0, 3.0, 0.3, 2.5), (1.0, 8.0, 1.0, 1.0), (2.0, 5.0, 0.0, 5.0)] {
            let mut params = quick();
            for (id, v) in [("kind", kind), ("count", count), ("agitation", agitation), ("tank", tank)] {
                params.set(PARAMS, id, v);
            }
            let mut outside = 0.0f32;
            let mut through = f32::MIN;
            run(&params, 90.0, |tank, _, _| {
                for f in &tank.fish {
                    outside = outside.max(f.at[0].abs() - FRAME / 2.0 * tank_width(&params)).max(f.at[1].abs() - FRAME / 2.0 * TALL);
                }
                through = through.max(tank.placed.iter().map(|p| p[2]).fold(f32::MIN, f32::max));
            });
            let what = format!("kind {kind} count {count} agitation {agitation} tank {tank}");
            // A fish may nose a little past a wall as it turns, never more.
            let most = 0.35 * params.get("size") * FRAME * 1.15;
            assert!(outside < most, "{what}: a fish got {outside} past a wall");
            // A flick of the tail may reach past the glass for a moment
            // (it shows pressed against it) before the fish eases back.
            let most = 0.2 * params.get("size") * FRAME * 1.15;
            assert!(through < most, "{what}: a fish came {through} through the glass");
        }
    }

    fn tank_width(params: &Params) -> f32 {
        params.get("tank")
    }

    /// Fewer fish asked for: the extra ones swim away into the dark and are
    /// gone; more: they come.
    #[test]
    fn the_count_is_kept() {
        let mut params = quick();
        params.set(PARAMS, "count", 6.0);
        let mut tank = Tank::new(3);
        let frame = |tank: &mut Tank, params: &Params, i: usize| {
            tank.render(&Ctx { t: i as f64 / 30.0, dt: 1.0 / 30.0, now: 0.0, params });
        };
        for i in 1..300 {
            frame(&mut tank, &params, i);
        }
        assert_eq!(tank.fish.len(), 6);
        params.set(PARAMS, "count", 2.0);
        for i in 300..3000 {
            frame(&mut tank, &params, i);
        }
        assert_eq!(tank.fish.len(), 2, "the extra fish never left");
    }

    /// Crowded and restless, the fish still bump rather than pass through
    /// one another: two bodies never sink far into each other.
    #[test]
    fn the_fish_do_not_pass_through_each_other() {
        let mut params = quick();
        for (id, v) in [("count", 8.0), ("agitation", 1.0), ("tank", 1.0), ("deep", 0.5)] {
            params.set(PARAMS, id, v);
        }
        let length = params.get("size") * FRAME;
        let mut worst = 0.0f32;
        run(&params, 90.0, |tank, _, _| {
            let bodies: Vec<(V3, V3, f32)> = tank
                .fish
                .iter()
                .map(|f| {
                    let half = scale(f.heading(), 0.38 * length * f.size);
                    (sub(f.at, half), add(f.at, half), length * f.size * kinds()[f.kind].flank.max(0.12))
                })
                .collect();
            for i in 0..bodies.len() {
                for j in i + 1..bodies.len() {
                    let ((a0, a1, ra), (b0, b1, rb)) = (bodies[i], bodies[j]);
                    let (s, t) = nearest(a0, a1, b0, b1);
                    let gap = sub(add(a0, scale(sub(a1, a0), s)), add(b0, scale(sub(b1, b0), t)));
                    let sink = 1.0 - dot(gap, gap).sqrt() / (ra + rb);
                    worst = worst.max(sink);
                }
            }
        });
        assert!(worst < 0.5, "two fish sank {worst} of the way into each other");
    }

    /// Alone and calm, a fish turns smoothly: how fast it is turning never
    /// jumps from one frame to the next.
    #[test]
    fn the_turns_are_smooth() {
        let mut params = quick();
        params.set(PARAMS, "count", 1.0);
        let (mut was, mut worst) = (None, 0.0f32);
        run(&params, 120.0, |tank, _, _| {
            let spin = tank.fish[0].spin;
            if let Some(w) = was {
                worst = worst.max((spin - w as f32).abs() * 30.0);
            }
            was = Some(spin);
        });
        assert!(worst < 1.0, "its turning changed at {worst} rad/s^2");
    }

    /// Each fish's swimming loops: its end runs into its start no more
    /// suddenly than any other step of it.
    #[test]
    fn the_swimming_loops_without_a_jump() {
        let gap = |a: &[V3], b: &[V3]| a.iter().zip(b).map(|(p, q)| dot(sub(*p, *q), sub(*p, *q)).sqrt()).fold(0.0, f32::max);
        for (n, k) in kinds().iter().enumerate() {
            let m = &k.model;
            let steps = (m.length * 30.0) as usize;
            let (mut a, mut b) = (Vec::new(), Vec::new());
            let mut worst = 0.0f32;
            for s in 0..steps {
                m.skin(&m.pose(s as f32 / steps as f32, 0.0, 0.0), &mut a);
                m.skin(&m.pose((s + 1) as f32 / steps as f32, 0.0, 0.0), &mut b);
                worst = worst.max(gap(&a, &b));
            }
            m.skin(&m.pose(1.0 - 1e-4, 0.0, 0.0), &mut a);
            m.skin(&m.pose(0.0, 0.0, 0.0), &mut b);
            let seam = gap(&a, &b);
            assert!(seam <= worst, "fish {n}: its swimming jumps {seam} at the seam (a step is at most {worst})");
        }
    }
}
