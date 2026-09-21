//! How a flap falls, and what a falling flap looks like from where we sit.
//!
//! One flap is a rigid card hinged on the module's horizontal axle. It is the
//! only thing in this patch that leaves the plane of the panel, so it is the
//! only thing that has to be projected - and at 30 fps, over about six frames,
//! it is the whole reason the picture reads as a machine rather than a wipe.

use std::sync::OnceLock;

/// How hard the card is pushed off the pin, as a share of the speed gravity
/// will add over the whole fall (it leaves at `sqrt(PUSH)` and lands at
/// `sqrt(PUSH + 2)`, so this is the only number that sets how much of the fall
/// is acceleration).
///
/// A card released from dead balance never starts, and one released a few
/// degrees past it spends half the fall in the first 30 degrees - which at six
/// frames is three frames of nothing and then a slam. A real card does not do
/// that either: the drum carries it past the retaining pin at the motor's own
/// speed and it falls away from there. 0.12 is a card that leaves at a quarter
/// of the speed it lands at, chosen by looking at the five-angle strip.
const PUSH: f32 = 0.12;

/// Samples of the fall curve. Even in *time*, so a lookup is a lerp.
const CURVE: usize = 256;

/// Where the flap is, `u` of the way through its fall: degrees, 0 standing on
/// the axle and 180 flat on the stack below.
///
/// This is a rigid card under gravity, not an easing curve: it leaves the pin
/// at the drum's speed and everything after that is `theta'' = k sin theta`,
/// which has no elementary solution, so the time to reach each angle is
/// integrated once and inverted into a table. What comes out starts at a
/// walk, accelerates the whole way and arrives at full speed - which is what a
/// falling card does and what no ease-in-out reads as.
#[must_use]
pub fn angle(u: f32) -> f32 {
    let table = curve();
    let x = u.clamp(0.0, 1.0) * (CURVE - 1) as f32;
    let i = (x as usize).min(CURVE - 2);
    let f = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

fn curve() -> &'static [f32; CURVE] {
    static TABLE: OnceLock<[f32; CURVE]> = OnceLock::new();
    TABLE.get_or_init(|| {
        // Conservation of energy, with the drum's push as the starting speed:
        // `d(theta)/dt` goes as `sqrt(PUSH + 1 - cos theta)`, up to a constant
        // that normalising the fall to 0..1 removes. Nothing diverges, so a
        // plain midpoint rule over the half turn is enough.
        const M: usize = 8192;
        let step = std::f32::consts::PI / M as f32;
        let mut time = [0.0_f32; M];
        let mut t = 0.0_f32;
        for (j, slot) in time.iter_mut().enumerate() {
            let th = (j as f32 + 0.5) * step;
            t += step / (PUSH + 1.0 - th.cos()).max(1e-9).sqrt();
            *slot = t;
        }
        let total = t;
        // Invert it: the angle at each of `CURVE` equal slices of the fall.
        let mut out = [0.0_f32; CURVE];
        let mut j = 0;
        for (k, o) in out.iter_mut().enumerate() {
            let want = total * k as f32 / (CURVE - 1) as f32;
            while j + 1 < M && time[j + 1] < want {
                j += 1;
            }
            let (t0, t1) = (time[j], time[j + 1].max(time[j]));
            let f = if t1 > t0 { ((want - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { 0.0 };
            *o = ((j as f32 + 0.5 + f) * step).to_degrees();
        }
        // The first and last samples are the ends of the fall exactly, so a
        // module that is not flipping is `angle(0.0)` and a landed one is
        // `angle(1.0)`, with no sliver of a step at either end.
        out[0] = 0.0;
        out[CURVE - 1] = 180.0;
        out
    })
}

/// The hint of a settle after a flap lands: how far it lifts back off the
/// stack, in degrees, `tau` of the way through the bounce.
///
/// One small hump, not a decaying oscillation. A card landing on a stack of
/// cards does not ring.
#[must_use]
pub fn settle(tau: f32, amplitude: f32) -> f32 {
    let tau = tau.clamp(0.0, 1.0);
    amplitude * (tau * std::f32::consts::PI).sin() * (1.0 - tau)
}

/// Where the viewer is, and how far away, in module heights.
///
/// `DEPTH` is a chosen viewing distance for the *depicted* module, not for the
/// panel: the picture is a split-flap board seen from a couple of its own
/// heights away, so its free edge comes towards the viewer and grows by about
/// a quarter as it passes edge-on. At the panel's own viewing distance there
/// would be no perspective at all and nothing would read.
pub const DEPTH: f32 = 2.6;

/// The geometry of one module mid-flip, for a given flap angle.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fall {
    /// Half the module's height: how far the flap reaches from the axle.
    pub reach: f32,
    /// Sine of the flap angle: how far out of the panel the free edge is.
    pub out: f32,
    /// The flap's foreshortening, signed. Positive is the flap standing above
    /// the axle with its front face towards us; negative is the same card
    /// below the axle showing its back; zero is edge-on.
    pub squash: f32,
    /// How much of the light the face is catching, 0..1.
    pub shade: f32,
    /// How brightly the free edge is catching it.
    pub edge: f32,
    /// How far below the axle the flap's shadow reaches, in LEDs.
    pub shadow: f32,
}

/// Elevation of the light, in degrees. Below the viewer on purpose: with the
/// light more frontal than the eye, the shadow of a falling flap runs *ahead*
/// of the flap down the plate below it, which is the cue that says the card is
/// off the surface. Put the light above the eye and the shadow hides under the
/// card and is worth nothing.
///
/// It does not move with `tilt`, which has a consequence worth knowing: the
/// card goes edge-on to the *light* at `90 + LIGHT` but edge-on to the
/// *viewer* at `90 + tilt`, so a large tilt leaves a few frames where the card
/// still faces us and is already unlit. At `tilt` 16 the face is 3 LEDs tall
/// by then and it does not show; above about 28 it does. Card 155 left that
/// with the owner rather than guessing at a light that follows the viewpoint.
const LIGHT: f32 = 6.0;

impl Fall {
    /// `theta` and `tilt` in degrees; `height` is the module's, in LEDs.
    #[must_use]
    pub fn new(theta: f32, tilt: f32, height: f32) -> Fall {
        let (t, phi, lam) = (theta.to_radians(), tilt.to_radians(), LIGHT.to_radians());
        let reach = height * 0.5;
        // The eye is `tilt` above the board, so a point `r` along the flap is
        // seen `r cos(theta - tilt) / cos(tilt)` from the axle - one formula
        // for both faces, worth more than it looks: it is 1 at rest (the flap
        // covers its half exactly), it crosses zero at `90 + tilt` rather than
        // at 90 (a raised viewer sees the front of the card for longer), and
        // its sign is which face is towards us, because the flap's normal dotted
        // with the view direction is the same cosine.
        let squash = (t - phi).cos() / phi.cos();
        // Lambert on the same two vectors, with the light where LIGHT says.
        let shade = ((t - lam).cos() / lam.cos()).abs().min(1.0);
        // The free edge is the card's thickness: its normal lies along the
        // card, so it is brightest as the card passes edge-on to us - which is
        // exactly when the face has nothing left to show. The second factor is
        // that a card lying flat has no visible edge at all: its thickness is
        // against the stack. Without it a landed card leaves a lit line along
        // the bottom of its module, and the settle ends with that line jumping
        // to the top.
        let edge = (t + lam).sin().abs().min(1.0) * t.sin().max(0.0);
        // The card's shadow, cast onto the plate below by a light at LIGHT.
        let shadow = (-(t - lam).cos() / lam.cos() * reach).max(0.0);
        Fall { reach, out: t.sin().max(0.0), squash, shade, edge, shadow }
    }

    /// How far along the flap, from the axle, the point at `v` LEDs below the
    /// axle is - or `None` if the flap does not reach it.
    ///
    /// The inverse of `v = -r squash / (1 - r out / D)`, which is the flap's
    /// foreshortening and its perspective in one: solving for `r` is one
    /// division, so every sample of `Frame::supersample` can ask.
    #[must_use]
    pub fn along(&self, v: f32) -> Option<f32> {
        let d = flat_depth(self.reach);
        let denom = v * self.out / d - self.squash;
        if denom.abs() < 1e-6 {
            return None;
        }
        let r = v / denom;
        (r >= 0.0 && r <= self.reach).then_some(r)
    }

    /// How much wider the card is at `r` than it is at the axle.
    #[must_use]
    pub fn widen(&self, r: f32) -> f32 {
        1.0 / (1.0 - r * self.out / flat_depth(self.reach)).max(0.2)
    }

    /// Where the point `r` along the card lands on the panel: LEDs below the
    /// axle, negative above it. The forward of [`Fall::along`].
    #[must_use]
    pub fn screen(&self, r: f32) -> f32 {
        -r * self.squash * self.widen(r)
    }
}

fn flat_depth(reach: f32) -> f32 {
    DEPTH * reach * 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fall starts at the top, ends flat, and never goes backwards.
    #[test]
    fn the_fall_runs_from_nought_to_a_half_turn() {
        assert_eq!(angle(0.0), 0.0);
        assert_eq!(angle(1.0), 180.0);
        assert_eq!(angle(-3.0), 0.0, "clamped, not wrapped");
        assert_eq!(angle(7.0), 180.0);
        let mut prev = -1.0;
        for k in 0..=200 {
            let a = angle(k as f32 / 200.0);
            assert!(a >= prev, "the flap went back up at u={}", k as f32 / 200.0);
            prev = a;
        }
    }

    /// And it is gravity, not an easing curve: the second half of the fall
    /// covers far more than the first, and the flap is still speeding up when
    /// it lands. An ease-in-out would fail both.
    #[test]
    fn a_flap_accelerates_all_the_way_down() {
        assert!(angle(0.5) < 70.0, "half way through the fall it is already at {}", angle(0.5));
        let early = angle(0.1) - angle(0.0);
        let late = angle(1.0) - angle(0.9);
        assert!(late > early * 4.0, "it is not accelerating: {early} then {late} degrees a tenth");
        // No deceleration anywhere: each tenth is at least as big as the last.
        let steps: Vec<f32> = (0..10).map(|k| angle((k + 1) as f32 / 10.0) - angle(k as f32 / 10.0)).collect();
        for w in steps.windows(2) {
            assert!(w[1] >= w[0] * 0.98, "it slows down: {steps:?}");
        }
    }

    /// The settle is a hint and then nothing.
    #[test]
    fn the_settle_is_one_small_hump() {
        assert_eq!(settle(0.0, 7.0), 0.0);
        assert_eq!(settle(1.0, 7.0), 0.0);
        let peak = (0..=100).map(|k| settle(k as f32 / 100.0, 7.0)).fold(0.0_f32, f32::max);
        assert!((3.0..7.0).contains(&peak), "the bounce peaks at {peak} degrees");
    }

    /// At rest the flap covers its half of the module exactly, whatever the
    /// viewpoint - otherwise a still clock would not be a rectangle.
    #[test]
    fn a_resting_flap_covers_its_half_exactly() {
        for tilt in [0.0, 8.0, 16.0, 30.0, 40.0] {
            let rest = Fall::new(0.0, tilt, 30.0);
            assert!((rest.squash - 1.0).abs() < 1e-5, "tilt {tilt}: squash {}", rest.squash);
            assert_eq!(rest.out, 0.0, "tilt {tilt}: a resting flap is in the panel");
            assert!((rest.widen(rest.reach) - 1.0).abs() < 1e-6);
            let tip = rest.along(-14.99).expect("the top of the module is the tip");
            assert!((tip - 14.99).abs() < 1e-3, "tilt {tilt}: {tip}");
            assert_eq!(rest.along(1.0), None, "and it reaches nothing below the axle");
            // Landed: the same card, the same coverage, below the axle.
            let down = Fall::new(180.0, tilt, 30.0);
            assert!((down.squash + 1.0).abs() < 1e-5, "tilt {tilt}: landed squash {}", down.squash);
            let landed = down.along(14.99).expect("the bottom of the module is the tip");
            assert!((landed - 14.99).abs() < 1e-3, "tilt {tilt}: {landed}");
        }
    }

    /// A raised viewer sees the front of the card past 90 degrees, and the
    /// card is never quite edge-on to them until then. This is the whole of
    /// what `tilt` does.
    #[test]
    fn tilt_is_how_long_the_front_of_the_card_is_visible() {
        let flat = Fall::new(90.0, 0.0, 30.0);
        assert!(flat.squash.abs() < 1e-6, "flat on, 90 degrees is edge-on");
        let raised = Fall::new(90.0, 16.0, 30.0);
        assert!(raised.squash > 0.2, "raised, 90 degrees still shows the face: {}", raised.squash);
        assert!(Fall::new(106.0, 16.0, 30.0).squash.abs() < 1e-5, "edge-on at 90 + tilt");
        assert!(Fall::new(120.0, 16.0, 30.0).squash < 0.0, "past it, the back of the card");
    }

    /// The shadow leads the card down the plate: between the moment the card
    /// passes the axle and the moment it lands, the shadow reaches further
    /// down the panel than the card does, which is what makes the card look
    /// lifted off it. It leads by most around 115 degrees and tucks back
    /// under the card as it lands.
    #[test]
    fn the_shadow_runs_ahead_of_the_card() {
        let lead = |theta: f32| {
            let f = Fall::new(theta, 16.0, 30.0);
            f.shadow - f.screen(f.reach).max(0.0)
        };
        for theta in [100.0, 115.0, 130.0, 150.0] {
            assert!(lead(theta) > 0.1, "at {theta} deg the shadow hides under the card: {}", lead(theta));
        }
        assert!(lead(115.0) > 1.5, "the shadow is never far enough ahead to see: {}", lead(115.0));
        assert!(lead(115.0) > lead(170.0), "it should tuck under the card as it lands");
        // Above the axle there is no shadow on the plate below at all.
        assert_eq!(Fall::new(40.0, 16.0, 30.0).shadow, 0.0);
    }

    /// The face dims as it turns out of the light and the edge lights up as it
    /// passes us, so there is always something to see.
    #[test]
    fn the_face_and_the_edge_trade_places() {
        let up = Fall::new(0.0, 16.0, 30.0);
        let side = Fall::new(90.0, 16.0, 30.0);
        let down = Fall::new(180.0, 16.0, 30.0);
        assert!(up.shade > 0.98 && down.shade > 0.98, "a flat card is fully lit");
        assert!(side.shade < 0.2, "edge-on, the face has nothing: {}", side.shade);
        assert!(side.edge > 0.98, "and the edge has everything: {}", side.edge);
        assert!(up.edge < 0.2 && down.edge < 0.2, "flat, the edge is not in the picture");
    }

    /// The free edge comes towards the viewer, so the card is widest as it
    /// passes edge-on and exactly the module's width at both ends.
    #[test]
    fn the_free_edge_comes_towards_the_viewer() {
        let side = Fall::new(90.0, 16.0, 30.0);
        let w = side.widen(side.reach);
        assert!((1.15..1.35).contains(&w), "the tip widens by {w}");
        assert!(side.widen(0.0) == 1.0, "the axle does not move");
    }
}

