//! The bat itself: where every corner of one is, in the world.
//!
//! A bat silhouette is not a bird's (card 313's picture): wide wings with a
//! **scalloped trailing edge** and a **pronounced wrist**, a small body, and
//! ears when it is big enough to show them. `flock/bird.rs` is the model for
//! *how* to draw one - level of detail by projected span, a leading-edge spar
//! and a chorded trailing edge - but the shape is different enough (no long
//! tail dart, a much wider wing held nearly flat rather than a gull's smooth
//! dihedral, a ragged rather than smooth trailing edge) that this is its own,
//! smaller model rather than a bent copy of `bird::pose`.

use super::sim::{Bat, V3};
use crate::color::smoothstep;

/// How the wing folds and beats. Fast and shallow - "fast shallow wingbeats,
/// no long glides" - so the amplitude is a third of a gull's and the rate is
/// set by the `beat` parameter (Hz), not tuned here.
const DIHEDRAL: f32 = 0.36;
const TIP_GAIN: f32 = 1.22;
/// The hand wing lags the shoulder, same idea as flock's wing - a wave
/// running out along the wing - but a shorter lag: a bat's wing is stiffer
/// and shorter than a gull's.
const LAG: f32 = 0.36;

/// Where the wings are hinged, and how the semi-span splits at the wrist. The
/// wrist sits further out than a bird's - a bat's "hand" (the long finger
/// bones the membrane is stretched from) is most of the wing.
const SHOULDER_FWD: f32 = 0.08;
const SHOULDER_OUT: f32 = 0.06;
const INNER: f32 = 0.34;
const HAND: f32 = 0.66;

/// Chord at the root and at the wrist. Nearly the same width all the way to
/// the wrist and *then* the taper - "wide wings ... a pronounced wrist" - is
/// the opposite emphasis from a gull's, whose taper starts at the shoulder.
const ROOT_CHORD: f32 = 0.20;
const WRIST_CHORD: f32 = 0.195;

/// The trailing edge's own ripple, as a fraction of the wing's local chord,
/// so it reads at any drawn size rather than being a fixed number of LEDs. A
/// bat's flight membrane is scalloped between each finger; two notches is as
/// much of that as this resolution can hold before it is just noise.
const SCALLOP: f32 = 0.30;

/// Ears: only drawn once the whole bat is big enough that a pair of two-LED
/// triangles reads as ears and not as noise on the head.
const EAR_AREA: f32 = 0.55;
const EAR_LEN: f32 = 0.085;
const EAR_SPREAD: f32 = 0.055;

/// The thumb claw: a real bat's wing has a small clawed digit at the wrist,
/// free of the membrane, used for climbing - card 319's "the wrist, the
/// thumb claw". Same level-of-detail gate as the ears: a hook this small is
/// only worth drawing once the wrist itself is legible.
const THUMB_AREA: f32 = 0.55;
const THUMB_LEN: f32 = 0.09;

pub(crate) struct Wing {
    /// The leading edge: shoulder, wrist, tip.
    pub spar: [V3; 3],
    /// The trailing edge from the root to the wrist: root, two ripple
    /// points, wrist. Fanned from the shoulder (see `draw_bats` in `mod.rs`)
    /// rather than filled as one polygon, so each little scallop
    /// anti-aliases the way a flock wing's edges do.
    pub trail: [V3; 4],
    pub normal: V3,
    /// A short hook forward and up off the wrist, present only once the
    /// bat's own [`Pose::ears`] would be too - `None` otherwise.
    pub thumb: Option<[V3; 2]>,
}

pub(crate) struct Pose {
    /// Nose, chest, tail - a short body, not a dart: a bat's body is small
    /// against its wings, and never the thing that says "flying" here.
    pub body: [V3; 3],
    /// Present only once `area` (the level-of-detail number) has grown past
    /// [`EAR_AREA`]; empty otherwise, so the caller need not re-check.
    pub ears: Option<[V3; 2]>,
    pub wings: [Wing; 2],
}

fn beat(phase: f32) -> (f32, f32) {
    let warp = |p: f32| (p + 0.28 * p.sin()).sin();
    let lagged = phase - LAG;
    let inner = DIHEDRAL * warp(phase);
    let hand = DIHEDRAL * TIP_GAIN * warp(lagged);
    (inner, hand)
}

/// Where every corner of this bat is, at a drawn wingspan of `span` metres.
///
/// `area` is the level of detail, 0..1, from the bat's projected span (see
/// [`super::AREA`]): at 0 it is the bare two- or three-LED skeleton the far
/// side of the colony needs, growing a surface, a scalloped edge and finally
/// ears as it nears the camera.
pub(crate) fn pose(b: Bat, span: f32, area: f32) -> Pose {
    let (right, up, fwd) = frame(b);
    let along = |k: f32| b.pos.add(fwd.scale(k * span));
    let (inner, hand) = beat(b.phase);

    let body = [along(0.16), along(0.02), along(-0.14 * area - 0.03)];

    let half = 0.5 * span;
    let wing = |sgn: f32| {
        let side = right.scale(sgn);
        let shoulder = b.pos.add(fwd.scale(SHOULDER_FWD * span)).add(side.scale(SHOULDER_OUT * span));
        let (si, ci) = inner.sin_cos();
        let out_in = side.scale(ci).add(up.scale(si));
        let wrist = shoulder.add(out_in.scale(INNER * half));

        let (sh, ch) = hand.sin_cos();
        let out_hand = side.scale(ch).add(up.scale(sh));
        let tip = wrist.add(out_hand.scale(HAND * half));

        // Two ripple points along the trailing edge, pulled a little forward
        // (towards the spar, i.e. a notch) and pushed back (a scallop),
        // scaled by the local chord so it stays proportionate at any drawn
        // size and vanishes cleanly at `area` 0.
        let chord_at = |t: f32| ROOT_CHORD + (WRIST_CHORD - ROOT_CHORD) * t;
        let root_trail = shoulder.sub(fwd.scale(chord_at(0.0) * span * area));
        let notch = shoulder
            .lerp(wrist, 0.42)
            .sub(fwd.scale(chord_at(0.42) * span * area * (1.0 - SCALLOP)));
        let scallop = shoulder
            .lerp(wrist, 0.74)
            .sub(fwd.scale(chord_at(0.74) * span * area * (1.0 + SCALLOP * 0.5)));
        let wrist_trail = wrist.sub(fwd.scale(WRIST_CHORD * span * area));

        let thumb = (area > THUMB_AREA).then(|| {
            let len = span * THUMB_LEN * smoothstep(THUMB_AREA, 1.0, area);
            [wrist, wrist.add(fwd.scale(len * 0.6)).add(out_in.scale(len * 0.5))]
        });

        Wing {
            spar: [shoulder, wrist, tip],
            trail: [root_trail, notch, scallop, wrist_trail],
            normal: up.scale(ci).sub(side.scale(si)),
            thumb,
        }
    };

    let ears = (area > EAR_AREA).then(|| {
        let head = body[0];
        let spread = span * EAR_SPREAD * smoothstep(EAR_AREA, 1.0, area);
        let len = span * EAR_LEN * smoothstep(EAR_AREA, 1.0, area);
        [
            head.add(right.scale(spread)).add(up.scale(len)),
            head.sub(right.scale(spread)).add(up.scale(len)),
        ]
    });

    Pose { body, ears, wings: [wing(-1.0), wing(1.0)] }
}

/// The bat's own right/up/forward, built from its heading and world up -
/// there is no roll model here (a bat does not hold a bank the way a gliding
/// bird does; it is already all sharp turns), so `up` is always world up
/// levelled against the heading.
fn frame(b: Bat) -> (V3, V3, V3) {
    let fwd = b.heading();
    let world_up = V3 { x: 0.0, y: 1.0, z: 0.0 };
    let right = fwd.cross(world_up).unit_or(V3 { x: 1.0, y: 0.0, z: 0.0 });
    let up = right.cross(fwd).unit_or(world_up);
    (right, up, fwd)
}
