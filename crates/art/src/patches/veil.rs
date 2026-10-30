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
    param("steps", "Palette steps", 2.0, 31.0, 1.0, 16.0),
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

fn smooth(x: f32) -> f32 {
    crate::color::smoothstep(0.0, 1.0, x)
}

/// One visit, laid out for its slot.
struct Visit {
    kind: Kind,
    /// Seconds into the slot it begins, and how long each act lasts.
    start: f32,
    approach: f32,
    hold: f32,
    withdraw: f32,
    at: (f32, f32),
    /// Where the tip wanders to while it holds, relative to `at`.
    wander: (f32, f32),
    /// The direction, in the sheet, the hand is in from the tip; and how far
    /// the finger leans off pointing straight at us.
    toward: f32,
    lean: f32,
    first: f32,
    deeper: f32,
    /// The second finger, relative to the first (`Two` only).
    other: (f32, f32),
}

impl Visit {
    fn plan(slot: u64, seed: f32) -> Self {
        let mut rng = Rng::new(((seed * 1e9) as u64) ^ slot.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // The first slot always has a finger in it, soon: whoever starts the
        // patch should see what it is.
        let roll = if slot == 0 { 0.5 } else { rng.f32() };
        let kind = match roll {
            r if r < 0.15 => Kind::Nothing,
            r if r < 0.7 => Kind::Finger,
            r if r < 0.85 => Kind::Two,
            _ => Kind::Hand,
        };
        let hand = kind == Kind::Hand;
        let reach = if hand { 0.15 } else { 0.42 };
        let first = rng.range(0.32, 0.46);
        let mut v = Visit {
            kind,
            start: if slot == 0 { 1.5 } else { rng.range(0.0, 3.0) },
            approach: rng.range(5.0, 8.0),
            hold: rng.range(5.0, 10.0),
            withdraw: rng.range(0.9, 3.0),
            at: (rng.range(-reach, reach), rng.range(-reach, reach * 0.8)),
            wander: (0.0, 0.0),
            // Mostly from below, as an arm would be, sometimes from a side.
            toward: -PI / 2.0 + rng.range(-1.1, 1.1),
            lean: rng.range(0.35, 0.75),
            first,
            deeper: first + rng.range(0.12, 0.28),
            other: (0.0, 0.0),
        };
        let (a, d) = (rng.range(0.0, TAU), rng.range(0.0, if hand { 0.08 } else { 0.25 }));
        v.wander = (a.cos() * d, a.sin() * d);
        // Beside the first, across the way the hand lies.
        let side = v.toward + PI / 2.0 * rng.sign();
        v.other = (side.cos() * 0.34, side.sin() * 0.34);
        v
    }

    /// `k` is the finger size over [`DRAWN_AT`].
    fn extra(&self, local: f32, k: f32) -> [f32; MAX_EXTRA] {
        let mut x = [0.0; MAX_EXTRA];
        let tau = local - self.start;
        let (a, h, w) = (self.approach, self.hold, self.withdraw);
        if self.kind == Kind::Nothing || tau < 0.0 {
            return x;
        }
        // Push: in from behind, a pause at the first depth, further, then back.
        let held = self.first + (self.deeper - self.first) * smooth((tau - a - h * 0.3) / (h * 0.35));
        let push = if tau < a {
            AWAY + (self.first - AWAY) * smooth(tau / a)
        } else if tau < a + h {
            held + 0.015 * ((tau - a) * 1.3).sin()
        } else {
            // Snatched back: it accelerates away.
            let u = ((tau - a - h) / w).min(1.0);
            held + (AWAY - 0.1 - held) * u * u
        };
        let roam = smooth((tau - a - h * 0.45) / (h * 0.5));
        let tip = (self.at.0 + self.wander.0 * roam * k, self.at.1 + self.wander.1 * roam * k);
        // The glow gathers before it touches and goes with it.
        let presence = smooth(tau / (a * 0.7)) * (1.0 - smooth((tau - a - h) / (w * 0.8)));
        // The sheet shivers once it is let go.
        let age = tau - a - h - w * 0.4;
        let ripple = if age > 0.0 { 0.012 * (-age * 0.9).exp() } else { 0.0 };

        x[0] = self.kind as u8 as f32;
        x[1] = tip.0;
        x[2] = tip.1;
        x[3] = push * k;
        x[4] = self.toward;
        x[5] = self.lean;
        x[6] = presence;
        x[7] = ripple;
        x[8] = age.max(0.0);
        x[9] = self.at.0;
        x[10] = self.at.1;
        x[11] = self.other.0 * k;
        x[12] = self.other.1 * k;
        // The second finger arrives a little behind the first and stops shorter.
        x[13] = if self.kind == Kind::Two { (push - 0.08 - 0.12 * (1.0 - smooth((tau - 1.5) / a))) * k } else { 0.0 };
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
        palette: Some(Palette::ramps(&[ctx.get("hue")], ctx.get("steps") as usize, (0.28, 0.95), ctx.get("tint"))),
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
            let end = v.start + v.approach + v.hold + v.withdraw;
            assert!(end + 6.0 <= VISIT, "slot {slot}: ends at {end}");
            let x = v.extra(VISIT, 1.0);
            assert!(x[0] == 0.0 || x[3] <= AWAY, "slot {slot}: still pressing at the end");
        }
    }

    #[test]
    fn the_first_slot_shows_a_finger_early() {
        let v = Visit::plan(0, 0.9);
        assert_eq!(v.kind, Kind::Finger);
        assert!(v.extra(v.start + v.approach, 1.0)[3] > 0.2);
    }

    #[test]
    fn some_slots_are_empty_and_hands_are_rare() {
        let kinds: Vec<Kind> = (1..1000).map(|s| Visit::plan(s, 0.5).kind).collect();
        let count = |k| kinds.iter().filter(|&&x| x == k).count();
        assert!(count(Kind::Nothing) > 80, "{}", count(Kind::Nothing));
        assert!(count(Kind::Hand) < count(Kind::Finger) / 2);
    }
}
