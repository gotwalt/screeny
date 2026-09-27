//! The vocabulary of entrances, crossings and exits, and the director that
//! schedules them.
//!
//! **An act is a closed-form function of elapsed time.** Nothing here
//! integrates: a ghost's whole path - where its head is, which way it is
//! looking - is a formula in `el` (seconds since the act began), decided the
//! instant the act is drawn from the seed. That is what makes the schedule
//! reproducible at any step size: [`Director::advance`] only ever asks "does
//! the timeline reach this instant yet?", never "how many frames have we
//! drawn?" - the same discipline `flock` uses for its physics
//! ([`crate::patches::flock::Flock::advance`]), minus the physics, because
//! nothing here needs it. `crate::patches::ghosts::cloth`'s cloth sim is the
//! one thing in this patch that *is* an integration, and it is driven by the
//! head positions this module hands it, not the other way round.
//!
//! **The vocabulary** (card 315's list, card 326's proportions): drift,
//! bounce, peek, swoop, and two ways for a pair to share a moment - crossing
//! paths, or one chasing another. [`Director`] draws from it with
//! [`crate::variety::Variety`], the same tool the clocks' `dance` composer
//! uses, so a long run keeps finding new combinations rather than favouring
//! whichever the dice like - weighted, since card 326, so bounce and chase
//! stay rare accents against drift and peek's calm default.

use crate::color::smoothstep;
use crate::frame::W;
use crate::patch::Ctx;
use crate::rng::Rng;
use crate::variety::Variety;
use std::f32::consts::{PI, TAU};

pub(crate) const KINDS: usize = 8;
pub(crate) const NAMES: [&str; KINDS] = ["drift", "bounce", "peek", "swoop", "cross", "chase", "boo", "materialize"];

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
    /// In from an edge toward the panel's centre, closer than the ambient
    /// depth breathe ever comes, arms thrown dramatically up, then straight
    /// back out the way it came - the card's own "boo": "drift close, pause,
    /// arms thrown up with the sheet flaring, then retreat" (card 339).
    Boo,
    /// Fades in in place, holds, fades back out - never a positional
    /// entrance at all. The one deliberate, named exception to "always enter
    /// and leave by moving across an edge" (card 339's own acceptance
    /// criterion), and a rare one by construction (see `choose_kind`).
    Materialize,
}

impl Kind {
    fn of(i: u64) -> Kind {
        match i % KINDS as u64 {
            0 => Kind::Drift,
            1 => Kind::Bounce,
            2 => Kind::Peek,
            3 => Kind::Swoop,
            4 => Kind::Cross,
            5 => Kind::Chase,
            6 => Kind::Boo,
            _ => Kind::Materialize,
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
            Kind::Boo => 6,
            Kind::Materialize => 7,
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

/// One ghost's body and cloth, fixed for its whole appearance from the seed:
/// a small round `head_r`-radius head, shoulders `shoulder_x` apart, and two
/// arms (`upper_arm` shoulder->elbow, `forearm` elbow->wrist) - the body
/// `cloth::build_template`'s sheet is dropped over and hangs from (card 336:
/// "a real bedsheet over a body with a person's arms held out"). `hem_amp`
/// and `humps` are the hem's own corner-to-corner unevenness - see
/// `cloth::build_template`. `turn_*` and `depth_*` are the slow, seeded
/// oscillations behind "ghosts can come nearer and go further ... and turn"
/// (card 326): each ghost breathes its own depth and turn at its own pace.
/// `gesture_*`/`wave_*`/`arm_pitch0` seed [`arm_gesture`]'s own slow cycle
/// between held-out (default), raised ("boo") and drooped, plus one arm's
/// occasional wave (card 336: "the body's arms are the ghost's gesture").
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shape {
    /// Crown-to-hem, nominally - the actual worst-case reach (what framing
    /// uses) is [`cloth::total_height`], a hair more once corner droop is
    /// added on top.
    pub height: f32,
    pub head_r: f32,
    /// Half the shoulder-to-shoulder width.
    pub shoulder_x: f32,
    /// The shoulder joint's height below the head's own centre (negative).
    pub shoulder_y: f32,
    pub upper_arm: f32,
    pub forearm: f32,
    pub hem_amp: f32,
    pub humps: f32,
    pub phase0: f32,
    /// How solid it is: where two overlap, or it crosses something, this is
    /// what lets you tell.
    pub alpha: f32,
    /// Radians a second, and amplitude in radians, of the slow head-turn
    /// that sometimes shows a three-quarter view.
    pub turn_rate: f32,
    pub turn_amp: f32,
    /// Radians a second, and amplitude in world units, of the slow
    /// nearer/further breathing.
    pub depth_rate: f32,
    pub depth_amp: f32,
    /// The held-out/raised/drooped cycle's own rate and phase (radians/s,
    /// radians) - see [`arm_gesture`].
    pub gesture_rate: f32,
    pub gesture_phase: f32,
    /// The occasional one-arm wave's own rate and phase.
    pub wave_rate: f32,
    pub wave_phase: f32,
    /// Which arm waves: +1 (right) or -1 (left).
    pub wave_side: f32,
    /// The baseline "held out" shoulder pitch, radians above horizontal -
    /// driven by the `arms` param (card 339: "arm height control"; see
    /// [`arm_baseline_pitch`]), plus a small per-ghost jitter on top so a
    /// beat of several ghosts is not all holding at the identical angle.
    /// [`arm_gesture`]'s raise/droop/wave excursions all move *relative* to
    /// this baseline, whatever it is set to.
    pub arm_pitch0: f32,
}

/// The owner, 2026-09-27, on card 336/338's ghost: "the arms are too up. Can
/// you make it so I can adjust their up/down position?" `arms` is that knob:
/// `-1` hangs the arms down near the body, `0` holds them dead level
/// ("straight out"), `+1` raises them toward vertical. The two halves of the
/// range are deliberately different lengths - hanging down has more travel
/// than raising, since "raised" is already mostly the domain of the
/// occasional "boo" gesture in [`arm_gesture`], which moves *relative* to
/// whatever this baseline is (the card's own words), not to a fixed absolute
/// target - so a ghost holding its arms low still throws them up dramatically
/// for a "boo", and one holding them raised still has room to go further.
const ARMS_RAISE_MAX: f32 = 0.9; // ~51.6 degrees above horizontal, at `arms = 1`.
const ARMS_HANG_MAX: f32 = 1.15; // ~65.9 degrees below horizontal, at `arms = -1`.

fn arm_baseline_pitch(arms: f32) -> f32 {
    let arms = arms.clamp(-1.0, 1.0);
    if arms >= 0.0 {
        arms * ARMS_RAISE_MAX
    } else {
        arms * ARMS_HANG_MAX
    }
}

impl Shape {
    pub(crate) fn new(rng: &mut Rng, size: f32, arms: f32) -> Shape {
        // A little variety in proportion between ghosts, from the seed
        // (card 315's brief, still true at full body scale): a touch
        // narrower or wider shoulders, a longer or shorter reach, three to
        // five low waves around the hem.
        let height = size.max(6.0);
        Shape {
            height,
            head_r: height * rng.range(0.17, 0.19),
            shoulder_x: height * rng.range(0.15, 0.19),
            shoulder_y: -height * rng.range(0.16, 0.20),
            upper_arm: height * rng.range(0.17, 0.21),
            forearm: height * rng.range(0.16, 0.2),
            hem_amp: height * rng.range(0.05, 0.09),
            humps: (3 + (rng.u64() % 3)) as f32,
            phase0: rng.range(0.0, TAU),
            alpha: rng.range(0.82, 0.96),
            turn_rate: rng.range(0.12, 0.26),
            turn_amp: rng.range(0.3, 0.8),
            depth_rate: rng.range(0.08, 0.2),
            depth_amp: rng.range(size * 0.12, size * 0.3),
            gesture_rate: rng.range(0.025, 0.05),
            gesture_phase: rng.range(0.0, TAU),
            wave_rate: rng.range(0.5, 0.9),
            wave_phase: rng.range(0.0, TAU),
            wave_side: rng.sign(),
            arm_pitch0: arm_baseline_pitch(arms) + rng.range(-0.04, 0.04),
        }
    }

    /// Half the width a silhouette needs clearing before nothing of it is on
    /// the panel: where an entrance starts and an exit ends.
    pub(crate) fn margin(self) -> f32 {
        super::cloth::extent(self) + 1.6
    }

    pub(crate) fn total_height(self) -> f32 {
        super::cloth::total_height(self)
    }
}

/// One arm's joint angles: `pitch` is radians above horizontal (0 = held
/// straight out to the side, positive = raised, negative = drooped);
/// `elbow` is the forearm's own further bend past the upper arm's own
/// direction (see `cloth::arm_points`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArmPose {
    pub pitch: f32,
    pub elbow: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Arms {
    pub left: ArmPose,
    pub right: ArmPose,
}

/// The arm gesture at `el` seconds into this ghost's own appearance: held out
/// (the baseline, [`Shape::arm_pitch0`] - card 339's `arms` param) most of the
/// time, with slow, occasional excursions to a "boo" raise or a droop, plus an
/// independent, occasional wave on whichever arm this ghost's own seed picked
/// (card 336: "held out (default), slowly raised for a 'boo', drooped, one
/// waving ... the sheet follows them physically"). Cubed, not a plain sine -
/// as `poses_at`'s own yaw bias does - so both cycles spend most of their time
/// near the baseline, with brief excursions to the extremes, rather than
/// drifting through every angle in between at equal length. The raise/droop
/// excursions are a fixed *delta* off the baseline (card 339: "gestures move
/// relative to it"), not a fixed absolute target - so a ghost holding its
/// arms down near its side still throws them dramatically upward for a "boo",
/// and one holding them raised still visibly droops.
pub(crate) fn arm_gesture(shape: Shape, el: f32) -> Arms {
    const RAISE_DELTA: f32 = 1.0; // a "boo": thrown up, relative to the baseline.
    const DROOP_DELTA: f32 = -0.5; // relative to the baseline.
    const WAVE_ELBOW_AMP: f32 = 0.9;
    const WAVE_PITCH_AMP: f32 = 0.32;
    const SWAY_AMP: f32 = 0.05; // "a gentle bob and sway as it floats".

    let g = (shape.gesture_rate * el + shape.gesture_phase).sin();
    let g3 = g * g * g;
    let raise = g3.max(0.0);
    let droop = (-g3).max(0.0);
    let base = shape.arm_pitch0;
    let sway = SWAY_AMP * (shape.turn_rate * 0.6 * el + shape.phase0 * 0.9).sin();
    // Clamped well short of a full flip (+/- PI/2 would be dead vertical) so
    // an extreme `arms` baseline plus a full "boo" excursion still lands on a
    // sane arm pose rather than folding the arm back on itself.
    let pitch = (base + raise * RAISE_DELTA + droop * DROOP_DELTA + sway).clamp(-1.5, 1.5);

    let wg = (shape.wave_rate * el + shape.wave_phase).sin();
    let waving = (wg * wg * wg).max(0.0);
    let wave_osc = (shape.wave_rate * 4.5 * el).sin();

    let mut left = ArmPose { pitch, elbow: 0.16 };
    let mut right = ArmPose { pitch, elbow: 0.16 };
    let waved = ArmPose { pitch: pitch + waving * WAVE_PITCH_AMP * wave_osc, elbow: 0.16 + waving * WAVE_ELBOW_AMP * wave_osc };
    if shape.wave_side > 0.0 {
        right = waved;
    } else {
        left = waved;
    }
    Arms { left, right }
}

/// Everything the picture needs to draw one ghost this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pose {
    /// Identifies this ghost's *appearance*, stable for as long as it is on
    /// screen and never reused: what the renderer keys a persistent
    /// [`crate::patches::ghosts::cloth::Cloth`] on, so the sheet is simulated
    /// continuously across frames rather than rebuilt from scratch.
    pub key: u64,
    pub shape: Shape,
    pub cx: f32,
    /// The head's own centre - the old "shoulder" line, now the sphere's
    /// middle - in panel/LED coordinates.
    pub cy: f32,
    /// Nearer or further than the reference distance, in world units (card
    /// 326: "come nearer and go further ... size by depth").
    pub depth: f32,
    /// Head turn around world up, radians (card 326: "turn, so you
    /// sometimes see a ghost at three-quarter view").
    pub yaw: f32,
    /// Where it is looking, as a unit vector (0, -1 is straight up the panel)
    /// - shifts the eyes within the cloth's own UV, not the head turn above.
    pub gaze: (f32, f32),
    /// This instant's arm gesture - see [`arm_gesture`].
    pub arms: Arms,
    /// This instant's opacity: `shape.alpha` for every kind but
    /// [`Kind::Materialize`], which ramps it from `0` up to `shape.alpha` and
    /// back down instead of moving on or off panel - the named fade the
    /// acceptance criteria carve out as the one exception to "always enter
    /// and leave by crossing an edge" (card 339).
    pub alpha: f32,
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
    /// This act's own identity, drawn fresh from the seed's RNG stream when
    /// it is built - what [`Act::poses_at`] keys a ghost's cloth on (via
    /// `mod.rs`'s `HashMap<u64, Cloth>`), together with the member index
    /// within this act.
    ///
    /// Card 340: this used to be `self.start.to_bits()` - the act's own
    /// start time - which is exactly right for telling the two members of
    /// *one* two-ghost act apart, but wrong across acts: an "ensemble" beat
    /// (`spawn_one`'s `slots > 1`) builds *several separate, solo* `Act`s
    /// that all share the exact same `start` (the beat's own start,
    /// unchanged through `spawn_one`'s `while remaining > 0` loop) - so
    /// every one of them computed the *identical* key (`start.to_bits() ^
    /// (0).wrapping_mul(..)`, since a solo act's only member is index `0`).
    /// Two or more simultaneously visible ghosts then collided on the same
    /// `HashMap` slot in `mod.rs`'s `clothes`: each still got its own,
    /// correct `Pose` (position, yaw, gaze), but both fought over *one*
    /// shared `Cloth`, stepping its physics toward two different targets
    /// every frame - exactly the owner's own words on card 336/338's ghost
    /// ("I think the rendering is crashing somehow ... rendering with some
    /// crazy artifacts", card 339) and, later, the "dark mark on the lower
    /// body" the orchestrator saw in card 339's own contact sheet (a
    /// correctly-projected face stamped over a mesh that was, at that
    /// instant, some contested blend of two different ghosts' bodies).
    /// Found by instrumenting, not guessed: a diagnostic in `mod.rs`'s test
    /// module recorded every pixel `face::stamp` actually painted alongside
    /// the exact `head_pos` it used, and cross-checked it against the
    /// director's own `poses_at(t)` for that same key - the two disagreed by
    /// tens of LEDs, which is only possible if two different `Pose`s really
    /// did share one key. A random 64-bit draw per act, from the same seeded
    /// stream everything else in this module already uses, is unique for
    /// all practical purposes (regardless of how many acts share a `start`)
    /// and costs nothing else about the schedule's own determinism.
    id: u64,
}

/// How far a drifting or peeking ghost's float bob answers to `bounce`'s
/// opposite: the same knob that governs the hop height below, so "bouncy"
/// and "floaty" are really one dial (card 315). The 3D cloth does its own
/// squash and billow physically now (card 326) - nothing here scales the
/// mesh any more.
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

        // Pass one: where every ghost's centre is, ignoring gaze - a
        // follower's gaze at the leader needs the leader's centre already
        // known, and vice versa for the ghosts who glance at a passer-by.
        let centres: Vec<(f32, f32, f32)> =
            self.plans.iter().enumerate().map(|(i, p)| self.centre_of(i, p, el, dur)).collect();

        (0..self.plans.len())
            .filter_map(|i| {
                let (x, y, active_el) = centres[i];
                if active_el.is_nan() {
                    return None;
                }
                let p = &self.plans[i];
                let key = self.id ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                // Cubed, not a plain sine: mostly near zero (facing the
                // camera), with brief excursions to the peak rather than
                // spending equal time at every angle in between (card 326
                // review: "bias the yaw so the face is toward the camera
                // most of the time; turns are brief").
                let swing = (p.shape.turn_rate * active_el + p.shape.phase0 * 1.3).sin();
                // The owner's review of an early render: the face must read
                // face-on during a hold, not mid-turn - a `Peek`'s own
                // enter/hold/leave split (`gaze_of`'s own windows) gates the
                // turn to exactly zero for the hold itself, easing back in
                // only for the brief entrance/exit legs.
                let yaw = p.shape.turn_amp * swing * swing * swing * self.yaw_gate(active_el, dur);
                let depth = if self.kind == Kind::Boo {
                    boo_depth(p.shape, active_el, dur)
                } else {
                    p.shape.depth_amp * (p.shape.depth_rate * active_el + p.shape.phase0 * 0.7 + std::f32::consts::FRAC_PI_2).sin()
                };
                let gaze = self.gaze_of(i, active_el, dur, &centres);
                let arms = if self.kind == Kind::Boo { boo_arms(p.shape, active_el, dur) } else { arm_gesture(p.shape, active_el) };
                let alpha =
                    if self.kind == Kind::Materialize { materialize_alpha(p.shape, active_el, dur) } else { p.shape.alpha };
                Some(Pose { key, shape: p.shape, cx: x, cy: y, depth, yaw, gaze, arms, alpha })
            })
            .collect()
    }

    /// Every performer's own entrance pose - the instant it is first on
    /// stage (`el = 0` for everyone but a chase's own follower, whose stage
    /// entrance is its own `delay` seconds later). Lets a ghost's cloth be
    /// created and settled well before the act actually starts, at the same
    /// static, off-screen pose it will really enter at (card 339: "spawning
    /// and settling a ghost's cloth off-screen ahead of its entrance, spread
    /// across frames" - see [`Director::upcoming`] and `mod.rs`'s prewarm
    /// pass, and the module doc on why every act is a pure function of
    /// elapsed time - `poses_at` at `el = 0` is exactly as valid, and exactly
    /// as reproducible, as at any other instant).
    pub(crate) fn entrance_poses(&self) -> Vec<Pose> {
        let mut out = self.poses_at(self.start + 1e-4);
        if self.kind == Kind::Chase {
            if let Some(p) = self.plans.get(1) {
                for extra in self.poses_at(self.start + p.delay + 1e-4) {
                    if !out.iter().any(|o| o.key == extra.key) {
                        out.push(extra);
                    }
                }
            }
        }
        out
    }

    /// `(x, y, active_el)`. `active_el` is this ghost's own elapsed time -
    /// `NAN` while a chased follower has not been let go yet, which
    /// `poses_at` reads as "not born".
    fn centre_of(&self, i: usize, p: &Plan, el: f32, dur: f32) -> (f32, f32, f32) {
        let margin = p.shape.margin();
        match self.kind {
            Kind::Drift => {
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                let y = p.y + p.amp * (TAU * p.freq * el + p.shape.phase0).sin();
                (x, y, el)
            }
            Kind::Bounce => {
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                let hops = p.bounces.max(1.0);
                let hop = (u * hops).rem_euclid(1.0);
                let height = 4.0 * hop * (1.0 - hop);
                let y = p.y - p.amp * height;
                (x, y, el)
            }
            Kind::Peek => {
                let (x, y) = peek_pos(p, el, dur, margin);
                (x, y, el)
            }
            Kind::Swoop => {
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                let y = p.y + p.amp * (PI * u).sin();
                (x, y, el)
            }
            Kind::Cross => {
                // Ghost 1 goes the opposite way to ghost 0 - its own `y`
                // (built with the separation already in it, and already
                // clamped to the panel) is otherwise exactly ghost 0's plan.
                let flip = if i == 1 { !p.flip } else { p.flip };
                let u = (el / dur).clamp(0.0, 1.0);
                let x = travel(flip, u, margin);
                (x, p.y, el)
            }
            Kind::Chase => {
                let el2 = if i == 1 { el - p.delay as f32 } else { el };
                if el2 < 0.0 {
                    return (0.0, 0.0, f32::NAN);
                }
                let u = (el2 / dur).clamp(0.0, 1.0);
                let x = travel(p.flip, u, margin);
                (x, p.y, el2)
            }
            Kind::Boo => {
                let (x, y) = boo_pos(p, el, dur, margin);
                (x, y, el)
            }
            // Never moves - the position is fixed at the panel spot picked
            // when the act was built (`p.amp`); only its own alpha (see
            // `materialize_alpha`) changes.
            Kind::Materialize => (p.amp, p.y, el),
        }
    }

    /// Look at the other ghost in `centres` when it is within `within` LEDs
    /// and actually born yet; otherwise look ahead.
    fn glance_or_forward(&self, i: usize, forward: f32, centres: &[(f32, f32, f32)], within: f32) -> (f32, f32) {
        let j = 1 - i.min(1);
        let (ox, oy, oel) = centres[j];
        if oel.is_nan() {
            return (forward, 0.0);
        }
        let (mx, my, _) = centres[i];
        let (dx, dy) = (ox - mx, oy - my);
        let d = (dx * dx + dy * dy).sqrt();
        if d < within && d > 1e-3 {
            (dx / d, dy / d)
        } else {
            (forward, 0.0)
        }
    }

    /// `0` while the head should hold face-on (a `Peek`'s own hold), `1`
    /// otherwise - what [`poses_at`](Self::poses_at) multiplies the slow
    /// turn-in-place by. Ramped over the same enter/leave legs
    /// [`gaze_of`](Self::gaze_of)'s own `Peek` case uses, so the turn eases
    /// back in exactly as the ghost starts moving again, not as a jump cut.
    fn yaw_gate(&self, el: f32, dur: f32) -> f32 {
        match self.kind {
            Kind::Peek => {
                let (enter, leave) = (dur * 0.28, dur * 0.28);
                let hold = (dur - enter - leave).max(0.05);
                if el < enter {
                    1.0 - smooth01(el / enter)
                } else if el < enter + hold {
                    0.0
                } else {
                    smooth01((el - enter - hold) / leave)
                }
            }
            // The face has to read during the whole point of the move - a
            // "boo" thrown while three-quarter turned away is not a boo, and
            // a materialising ghost is staring right at you the whole time.
            Kind::Boo => {
                let (enter, hold, leave) = boo_windows(dur);
                if el < enter {
                    1.0 - smooth01(el / enter)
                } else if el < enter + hold {
                    0.0
                } else {
                    smooth01((el - enter - hold) / leave)
                }
            }
            Kind::Materialize => 0.0,
            _ => 1.0,
        }
    }

    fn gaze_of(&self, i: usize, el: f32, dur: f32, centres: &[(f32, f32, f32)]) -> (f32, f32) {
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
            // Straight at the viewer - the point of both moves.
            Kind::Boo | Kind::Materialize => (0.0, 0.0),
        }
    }
}

fn normalise((x, y): (f32, f32)) -> (f32, f32) {
    let d = (x * x + y * y).sqrt().max(1e-6);
    (x / d, y / d)
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

/// The shared enter/hold/leave split every `Boo` phase function
/// (`boo_pos`/`boo_depth`/`boo_arms`/`yaw_gate`) reads - factored out once so
/// the pause a `Boo` holds is, by construction, the same window its
/// position, depth, arms and gaze all agree it is holding through.
fn boo_windows(dur: f32) -> (f32, f32, f32) {
    let enter = dur * 0.32;
    let leave = dur * 0.32;
    let hold = (dur - enter - leave).max(0.05);
    (enter, hold, leave)
}

/// In from an edge toward the panel's own centre - much further in than a
/// `Peek` ever comes - a pause, then straight back out the way it came
/// (never crossing to the far side): card 339's "boo".
fn boo_pos(p: &Plan, el: f32, dur: f32, margin: f32) -> (f32, f32) {
    let (enter, hold, leave) = boo_windows(dur);
    let edge_x = if p.flip { W as f32 + margin } else { -margin };
    let close_x = p.amp; // picked in `build_act`: near the panel's own centre.
    let x = if el < enter {
        lerp(edge_x, close_x, smooth01(el / enter))
    } else if el < enter + hold {
        close_x
    } else {
        lerp(close_x, edge_x, smooth01((el - enter - hold) / leave))
    };
    (x, p.y)
}

/// How close a "boo" can safely come before its own apparent extent, at the
/// perspective scale that depth implies, would no longer clear the panel -
/// card 336's own "cut off at the edge" bug (an earlier hold's own framing
/// mistake), checked here with real perspective math rather than repeated by
/// guesswork. `REF_DISTANCE` is `mod.rs`'s own constant, repeated locally
/// rather than imported - the same choice `cloth::extent`'s own doc explains
/// (this module already reasons about the camera's perspective growth
/// without reaching into `mod.rs` for it).
fn boo_close_depth(shape: Shape) -> f32 {
    const REF_DISTANCE: f32 = 40.0;
    let half = (crate::frame::H as f32 * 0.5 - 1.5).max(2.0);
    // Whichever the hold would clip on first, at a uniform perspective
    // zoom: the figure's own height (crown to hem, about the panel's own
    // shorter side already at the ambient distance) or its arms-out width.
    let reach = (super::cloth::total_height(shape) * 0.5).max(super::cloth::extent(shape));
    let safe_scale = (half / reach).clamp(1.05, 2.2);
    REF_DISTANCE * (1.0 / safe_scale - 1.0)
}

/// The depth (nearer/further than the reference distance) a `Boo` holds:
/// a touch further than the ambient breathe on the way in and out, a
/// dramatic (but panel-safe - see [`boo_close_depth`]) close approach
/// through the hold.
fn boo_depth(shape: Shape, el: f32, dur: f32) -> f32 {
    let (enter, hold, leave) = boo_windows(dur);
    let far = shape.depth_amp * 0.5;
    let close = boo_close_depth(shape);
    if el < enter {
        lerp(far, close, smooth01(el / enter))
    } else if el < enter + hold {
        close
    } else {
        lerp(close, far, smooth01((el - enter - hold) / leave))
    }
}

/// The arm gesture a `Boo` holds: both arms thrown dramatically up (the
/// card's own "arms thrown up with the sheet flaring") through the pause,
/// eased in and back out of the ordinary held-out baseline over the same
/// enter/leave legs everything else about the move uses.
fn boo_arms(shape: Shape, el: f32, dur: f32) -> Arms {
    let (enter, hold, leave) = boo_windows(dur);
    let base = ArmPose { pitch: shape.arm_pitch0, elbow: 0.16 };
    let raised = ArmPose { pitch: (shape.arm_pitch0 + 1.05).clamp(-1.5, 1.5), elbow: 0.05 };
    let u = if el < enter {
        smooth01(el / enter)
    } else if el < enter + hold {
        1.0
    } else {
        1.0 - smooth01((el - enter - hold) / leave)
    };
    let mix = |a: ArmPose, b: ArmPose, t: f32| ArmPose { pitch: lerp(a.pitch, b.pitch, t), elbow: lerp(a.elbow, b.elbow, t) };
    Arms { left: mix(base, raised, u), right: mix(base, raised, u) }
}

/// A `Materialize`'s own opacity: `0` at both ends, `shape.alpha` (the
/// ordinary "solid enough to tell where it crosses something" level) through
/// the hold - the one deliberate, named fade the acceptance criteria carve
/// out as an exception to "always enter and leave by crossing an edge" (card
/// 339). Never full black-to-full-alpha in a single frame: eased the same
/// way every other soft transition in this module is.
fn materialize_alpha(shape: Shape, el: f32, dur: f32) -> f32 {
    let enter = dur * 0.3;
    let leave = dur * 0.3;
    let hold = (dur - enter - leave).max(0.05);
    let u = if el < enter {
        smooth01(el / enter)
    } else if el < enter + hold {
        1.0
    } else {
        1.0 - smooth01((el - enter - hold) / leave)
    };
    shape.alpha * u
}

// --------------------------------------------------------------------------

/// Seconds of nothing on screen between beats. Card 326, on top of card
/// 315's "empty moments between": "long empty moments between visits are
/// good" - widened from the first pass's (1.2, 4.5).
const REST: (f32, f32) = (2.5, 7.5);
/// How often a beat is more than a single ghost, when the `ghosts` param
/// allows it. Card 326: "usually one ghost; two only as an occasional duet" -
/// turned down hard from the first pass's 0.4.
const ENSEMBLE_CHANCE: f32 = 0.14;

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

/// How far ahead of the frame being drawn the schedule stays decided - at
/// least as far as `mod.rs`'s own `PREWARM`, so an act is always already on
/// the books for the whole window its prewarm pass looks ahead through;
/// otherwise a ghost due soon could be discovered late and lose its lead time
/// to settle off-screen (card 339).
const LOOKAHEAD: f64 = 1.75;

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

    /// Every ghost that is not yet on screen at `t` but will be within
    /// `horizon` seconds, at its own entrance pose - `mod.rs`'s prewarm pass
    /// creates and settles a cloth for each of these well ahead of time
    /// (card 339), so the burst of physics steps a brand-new sheet needs
    /// happens off-screen, spread over many ordinary frames, rather than all
    /// at once the instant the ghost is due on stage.
    pub(crate) fn upcoming(&self, t: f64, horizon: f64) -> Vec<Pose> {
        self.acts.iter().filter(|a| a.start > t && a.start <= t + horizon).flat_map(|a| a.entrance_poses()).collect()
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
            Kind::Cross | Kind::Chase | Kind::Boo | Kind::Materialize => 0.5,
        };
        let mut best: Option<(f32, Kind)> = None;
        for i in 0..KINDS as u64 {
            let k = Kind::of(i);
            if k.cast() > free {
                continue;
            }
            let tag = format!("kind:{}", k.name());
            let fresh = 1.0 - self.variety.staleness(std::slice::from_ref(&tag));
            let mut score = 0.65 * fresh + 0.25 * bias(k) + self.rng.range(0.0, 0.3);
            // Card 326: "bounce and chase become rare accents" - chase is
            // already rare by needing two free slots (`ENSEMBLE_CHANCE`);
            // bounce is a solo kind and needs its own down-weighting here to
            // stay an accent rather than an equal member of the rotation. A
            // flat multiplier here (tried 0.5, then 0.7) shut bounce out of
            // a ten-minute run altogether for more than one seed - the
            // random term above already decides most close calls, so
            // knocking a fixed fraction off *after* it is drawn is much
            // more punishing than it looks; a smaller, additive penalty on
            // the freshness term leaves the variety system's own "it always
            // gets its turn eventually" guarantee intact.
            if k == Kind::Bounce {
                score -= 0.08;
            }
            // "Mostly one thing at a time; a surprise every so often" (card
            // 339) - `Boo` and `Materialize` are the surprises, kept rarer
            // than the everyday `Bounce` accent by the same additive-penalty
            // logic just above (a flat multiplier would shut them out
            // entirely for some seeds, per the same finding).
            if k == Kind::Boo || k == Kind::Materialize {
                score -= 0.12;
            }
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, k));
            }
        }
        best.map_or(Kind::Drift, |(_, k)| k)
    }

    fn build_act(&mut self, kind: Kind, start: f64, pace: f32, ctx: &Ctx) -> Act {
        // Drawn first, before anything else pulls from the same stream: see
        // `Act::id`'s own doc for why a beat-shared `start` can't be the
        // key's own basis any more.
        let id = self.rng.u64();
        let size = ctx.get("size");
        let bounce = ctx.get("bounce");
        let arms = ctx.get("arms");
        let flip = self.rng.u64() & 1 == 0;
        let cast = kind.cast();
        let mut shapes: Vec<Shape> = (0..cast)
            .map(|_| {
                let jitter = self.rng.range(0.85, 1.18);
                Shape::new(&mut self.rng, size * jitter, arms)
            })
            .collect();
        // A pair reads as two ghosts, not twins: give the second a shape of
        // its own rather than sharing the first's exactly.
        if cast == 2 {
            let jitter = self.rng.range(0.85, 1.18);
            shapes[1] = Shape::new(&mut self.rng, size * jitter, arms);
        }

        // Vertical band for the shoulder (`y`): the head reaches `head_r`
        // above it, the hem reaches `drop_below_centre` below it, and both
        // have to clear the panel - the dome the top, the hem the bottom, with a little more
        // room at the bottom for the ground line. A single "tallest shape"
        // number does not answer this on its own: a wide-domed, short-hemmed
        // ghost and a small-domed, long-hemmed one need different limits at
        // each end, so both are measured from the shapes actually in this
        // act, not guessed from their total height.
        let tallest = shapes.iter().map(|s| s.total_height()).fold(0.0_f32, f32::max);
        let dome_reach = shapes.iter().map(|s| super::cloth::rise_above_centre(*s)).fold(0.0_f32, f32::max);
        let hem_reach = shapes.iter().map(|s| super::cloth::drop_below_centre(*s)).fold(0.0_f32, f32::max);
        let y_lo = dome_reach + 2.5;
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
                // Ground contact near the bottom of the safe band, with the
                // hop clamped so its *peak* never lifts the head off the top
                // of the panel: the
                // generic `y` band above only promises the ghost clears the
                // top while sitting still, not after adding a hop on top.
                let ground = y_hi;
                let max_hop = (ground - shapes[0].head_r - 1.0).max(1.0);
                let hop_amp = (shapes[0].total_height() * self.rng.range(0.8, 1.3) * (0.6 + 0.6 * bounce)).min(max_hop);
                let plan = Plan { shape: shapes[0], flip, y: ground, amp: hop_amp, freq: bounce, bounces, delay: 0.0 };
                (format!("bounce x{} {}", bounces as u32, side_word(flip)), vec!["kind:bounce".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Peek => {
                // At least the whole sheet's own extent, with headroom - a
                // peek that only brought the *head* far enough in still left
                // the flared hem hanging off the edge (card 326 review).
                let peek_depth = super::cloth::extent(shapes[0]) * self.rng.range(1.15, 1.5);
                let dur = self.rng.range(2.2, 3.6) as f64 / f64::from(pace);
                let plan = Plan { shape: shapes[0], flip, y, amp: peek_depth, freq: 0.0, bounces: 0.0, delay: 0.0 };
                (format!("peek, {}", side_word(flip)), vec!["kind:peek".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Swoop => {
                // A top-corner entrance: the shoulder starts just clear of the
                // top and dips down by `arc`, so the dip has to fit between
                // there and the bottom, hem included.
                let top = shapes[0].head_r + 1.5;
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
            Kind::Boo => {
                let dur = self.rng.range(4.5, 7.0) as f64 / f64::from(pace);
                // Near the panel's own horizontal centre, with a little
                // jitter - `boo_close_depth` (see its own doc) already keeps
                // the *apparent*, zoomed-in extent clear of the panel at
                // whatever depth the hold reaches, so a small offset here is
                // safe rather than guessed.
                let close_x = crate::frame::W as f32 * 0.5 + self.rng.range(-4.0, 4.0);
                let plan = Plan { shape: shapes[0], flip, y, amp: close_x, freq: 0.0, bounces: 0.0, delay: 0.0 };
                (format!("boo, {}", side_word(flip)), vec!["kind:boo".into(), side_tag(flip)], vec![plan], dur, dur)
            }
            Kind::Materialize => {
                let dur = self.rng.range(5.0, 9.0) as f64 / f64::from(pace);
                // An on-panel spot, clear of the edges by the sheet's own
                // flat extent - it never moves there, so this is the only
                // clearance it ever needs (no perspective growth: `depth`
                // stays at the ambient breathe, never `Boo`'s close zoom).
                let clear = super::cloth::extent(shapes[0]) + 1.0;
                let hi = (crate::frame::W as f32 - clear).max(clear + 1.0);
                let x = self.rng.range(clear, hi);
                let plan = Plan { shape: shapes[0], flip, y, amp: x, freq: 0.0, bounces: 0.0, delay: 0.0 };
                ("materialize".to_string(), vec!["kind:materialize".into()], vec![plan], dur, dur)
            }
        };

        Act { kind, start, duration, travel_dur, name, tags, plans, id }
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
            // `Materialize` is the one deliberate, named exception the
            // acceptance criteria carve out: it never moves off panel at
            // all, it fades - checked separately, on alpha rather than
            // position, by `a_materialize_fades_rather_than_moves` below.
            if act.kind == Kind::Materialize {
                continue;
            }
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

    /// The one named exception to "always enters/leaves by crossing an edge"
    /// (card 339's own acceptance criterion): a `Materialize` fades its own
    /// alpha from `0` up and back down instead, and never moves.
    #[test]
    fn a_materialize_fades_rather_than_moves() {
        let p = params(&[("ghosts", 4.0)]);
        let mut d = Director::new(42);
        let mut t = 0.0;
        let mut found = false;
        // A deliberately rare surprise (`choose_kind`'s own penalty) needs a
        // long window to be sure of landing at all for a fixed seed - the
        // same twenty minutes `a_long_run_does_not_settle_into_a_loop` uses
        // for `bounce`/`cross`/`chase`, and the same coarse `0.25s` step
        // (Director-only, no GPU - cheap even over twenty minutes).
        while t < 1200.0 {
            d.advance(t, &ctx(t, &p));
            t += 0.25;
        }
        for act in &d.acts {
            if act.kind != Kind::Materialize {
                continue;
            }
            found = true;
            let start = act.poses_at(act.start + 1e-3);
            let mid = act.poses_at(act.start + act.duration * 0.5);
            let end = act.poses_at((act.start + act.duration - 1e-3).max(act.start));
            assert_eq!(start.len(), 1);
            assert_eq!(mid.len(), 1);
            assert_eq!(end.len(), 1);
            assert!(start[0].alpha < 0.05, "not near-invisible at the start: {}", start[0].alpha);
            assert!(end[0].alpha < 0.05, "not near-invisible at the end: {}", end[0].alpha);
            assert!(mid[0].alpha > start[0].shape.alpha * 0.9, "not near full alpha mid-hold: {}", mid[0].alpha);
            // Never moves.
            assert!((start[0].cx - mid[0].cx).abs() < 1e-3 && (start[0].cy - mid[0].cy).abs() < 1e-3, "a materialize moved");
        }
        assert!(found, "no `materialize` act landed in 300s at seed 42 - not a meaningful run of this test");
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

    /// Card 340's own real bug, found by instrumenting a render (see
    /// `mod.rs`'s Log, not repeated here): `spawn_one`'s "ensemble" beat
    /// (`slots > 1`) can build several *separate, solo* acts that all share
    /// the exact same `start` (the beat's own start, unchanged through the
    /// `while remaining > 0` loop) - and `poses_at`'s key used to be derived
    /// from `start` alone, so every solo act in the same beat produced the
    /// *identical* key. Two simultaneously visible ghosts then collided on
    /// the same `HashMap<u64, Cloth>` slot in `mod.rs`, each correctly
    /// posed but fighting over one shared cloth - the owner's own "crazy
    /// artifacts" (card 339) and the "dark mark on the lower body" a review
    /// later saw (card 340). Direct regression test: a busy, `ghosts`-at-cap
    /// run over ten real minutes (long enough for several ensemble beats at
    /// `ENSEMBLE_CHANCE`) never has two simultaneously-posed ghosts share a
    /// key.
    #[test]
    fn simultaneous_ghosts_never_share_a_key() {
        let p = params(&[("ghosts", 4.0)]);
        let mut d = Director::new(11);
        let mut t = 0.0;
        let mut saw_more_than_one = false;
        while t < 600.0 {
            d.advance(t, &ctx(t, &p));
            let poses = d.poses_at(t);
            saw_more_than_one |= poses.len() > 1;
            let mut keys: Vec<u64> = poses.iter().map(|p| p.key).collect();
            keys.sort_unstable();
            let before = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), before, "two ghosts shared a key at t={t}: {poses:?}");
            t += 0.2;
        }
        assert!(saw_more_than_one, "never saw more than one ghost at once in ten minutes - not a meaningful run of this test");
    }

    /// A long run keeps finding new combinations rather than settling into a
    /// loop: many distinct move kinds get used, not just the freshest one
    /// over and over, and every kind gets a real share of a long run.
    ///
    /// Twenty minutes, not the ten a pre-336 version of this test used: card
    /// 336's full body (arms held out) reaches a lot further than a bare
    /// head-and-sheet did, so `Shape::margin` grew - a slower, longer act,
    /// fewer of them fit in a fixed window, and a rare accent (`cross`,
    /// `chase`, needing two free slots at once) needs more real time to be
    /// sure of landing at all for this seed. Checked directly (30 real
    /// minutes) rather than assumed: the mechanism itself is unaffected, the
    /// two-slot coincidence is just rarer per minute now.
    #[test]
    fn a_long_run_does_not_settle_into_a_loop() {
        let p = params(&[]);
        let mut d = Director::new(2026);
        let mut t = 0.0;
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        while t < 1200.0 {
            d.advance(t, &ctx(t, &p));
            t += 0.25;
        }
        for act in &d.acts {
            *counts.entry(act.kind.name()).or_default() += 1;
        }
        eprintln!("20 minutes: {counts:?}");
        assert!(counts.len() >= 7, "only {} of {KINDS} kinds used in twenty minutes: {counts:?}", counts.len());
        let total: usize = counts.values().sum();
        assert!(total > 20, "only {total} acts in twenty minutes");
        // Card 326 recalibrated `bounce` (and `chase`/`cross`, already rare
        // by needing two free slots at once) down to a deliberate rare
        // accent rather than an equal member of the rotation, so
        // "neglected" now means "never happens at all" for those three, and
        // "at least a real share" - the original, stricter bar - for the
        // two common solo kinds it never touched. Card 339's `boo`/
        // `materialize` are deliberate rare surprises too (`choose_kind`'s
        // own penalty) - "never happened at all" is the bar for them.
        for kind in ["drift", "peek", "swoop"] {
            let share = *counts.get(kind).unwrap_or(&0) as f32 / total as f32;
            assert!(share > 0.03, "`{kind}` is being neglected: {counts:?}");
        }
        for kind in ["bounce", "cross", "chase", "boo", "materialize"] {
            assert!(counts.contains_key(kind), "`{kind}` never happened at all in twenty minutes: {counts:?}");
        }
        // No exact repeat back to back too often: the freshness scoring
        // should keep the same move from following itself most of the time.
        let seq: Vec<&str> = d.acts.iter().map(|a| a.kind.name()).collect();
        let repeats = seq.windows(2).filter(|w| w[0] == w[1]).count();
        assert!((repeats as f32 / seq.len() as f32) < 0.35, "{repeats} of {} acts repeated the last kind", seq.len());
    }

    /// Card 338: "the default hold should be near-symmetric... asymmetry
    /// only in gestures". Outside a wave (`wave_rate`/`wave_phase` both at a
    /// zero crossing) and at the gesture cycle's own rest point (`gesture_
    /// rate`/`gesture_phase` likewise), both arms get the identical `pitch`/
    /// `elbow` by construction (`arm_gesture` only ever writes a *different*
    /// pose into whichever side `wave_side` picked) - checked directly, for
    /// several shapes, rather than trusting that reading of the source: a
    /// held-out ghost that is not mid-wave is exactly symmetric, matching
    /// the reference. (336's own "one wrist higher" was a mid-wave/mid-sway
    /// snapshot, not a static bug in the held-out pose itself.)
    #[test]
    fn the_default_hold_is_symmetric_outside_a_wave() {
        let mut rng = Rng::new(41);
        for _ in 0..8 {
            let shape = Shape::new(&mut rng, 28.0, -0.15);
            // The instant, in the first 200s, closest to both cycles' own
            // zero crossing - "a real held-out moment", not mid-gesture.
            let mut best_el = 0.0_f32;
            let mut best_score = f32::MAX;
            let mut el = 0.0_f32;
            while el < 200.0 {
                let g = (shape.gesture_rate * el + shape.gesture_phase).sin();
                let wg = (shape.wave_rate * el + shape.wave_phase).sin();
                let score = g.abs() + wg.abs();
                if score < best_score {
                    best_score = score;
                    best_el = el;
                }
                el += 0.05;
            }
            let arms = arm_gesture(shape, best_el);
            assert!(
                (arms.left.pitch - arms.right.pitch).abs() < 1e-4 && (arms.left.elbow - arms.right.elbow).abs() < 1e-4,
                "asymmetric at a held-out moment: {arms:?}"
            );
        }
    }
}
