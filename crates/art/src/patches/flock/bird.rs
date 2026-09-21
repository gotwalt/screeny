//! The bird itself: where every corner of one is, in the world.
//!
//! Card 168 drew a bird as three strokes - a body dash, and one straight
//! segment per wing raised and lowered by a single angle. At three panel
//! pixels that is a bird. At fifteen, which is what card 122's `size` made
//! possible, it is a stick figure, and the owner said so: "the expanded bird
//! shapes don't have natural looking wings."
//!
//! So a wing here is what a wing is. **Two segments with a wrist between
//! them**: the inner wing from the shoulder out, the hand wing from the wrist
//! to the tip. The hand wing beats further than the inner one and arrives
//! later - that lag is the wave that runs out along a real wing - and it folds
//! back on the upstroke, so the span visibly shortens on the quick half of the
//! beat and is at its widest through the slow, spread downstroke. **And a
//! surface**: each wing carries chord, broad at the root, tapering to nothing
//! at the tip, so it is an area and not a line.
//!
//! Everything is built in the bird's own frame and handed back in world
//! coordinates, so the camera does the foreshortening. Seen edge-on the wing
//! thins to almost nothing, seen from below in a bank it is at its broadest,
//! and neither is special-cased anywhere.

use super::sim::{Bird, V3};
use crate::color::smoothstep;

/// The body's landmarks, as fractions of the full wingspan, measured forward
/// from the bird's position. A bird is roughly as long as it is wide; these
/// are a generic passerine-to-gull silhouette and not any one species.
const NOSE: f32 = 0.30;
const CHEST: f32 = 0.06;
const HIP: f32 = -0.16;
const TAIL_TIP: f32 = -0.40;

/// Where the wings are hinged: a little forward of the middle and barely off
/// the centre line, because at this size the two shoulders are the same pixel.
const SHOULDER_FWD: f32 = 0.11;
const SHOULDER_OUT: f32 = 0.05;

/// How the semi-span is divided at the wrist. The hand wing is the longer
/// half, which is what makes folding it shorten the span as much as it does.
const INNER: f32 = 0.45;
const HAND: f32 = 0.55;

/// Chord at the root and at the wrist, as fractions of the span; the tip has
/// none, so the wing tapers to a point. The spar the chord hangs behind is the
/// leading edge, which is where a wing's bones really are.
const ROOT_CHORD: f32 = 0.19;
const WRIST_CHORD: f32 = 0.115;

/// How far the wrist sits behind the shoulder, spread and folded. Spread it
/// is very slightly *ahead* of it - a bird reaches forward into the
/// downstroke - and folded it is drawn back towards the body.
const INNER_SWEEP: (f32, f32) = (-0.02, 0.10);

/// How far back the hand wing is swept, spread and fully folded, in radians.
/// Spread it is nearly straight out; folded it trails behind the wrist.
const SWEEP: (f32, f32) = (0.34, 1.05);

/// How far the hand wing lags the shoulder, in radians of wingbeat phase.
const LAG: f32 = 0.55;

/// The dihedral the inner wing beats through, and how much further the tip
/// swings than the wrist does.
const DIHEDRAL: f32 = 0.62;
const TIP_GAIN: f32 = 1.30;

/// A glide: wings held slightly raised with the hand wing dropped a touch
/// below them - the gull "M" - and the wrist half folded.
const GLIDE_INNER: f32 = 0.24;
const GLIDE_HAND: f32 = -0.30;
const GLIDE_FOLD: f32 = 0.34;

/// How much further the **inside** wing of a turn is folded, and the lean at
/// which it is all the way there (radians). A banking bird draws its lower
/// wing in - it has less of a turn to fly round and the shorter wing is moving
/// slower through the air - and at this size that asymmetry is worth as much
/// as the lean itself: it is what stops a banked bird reading as a bird drawn
/// wonky (card 124).
const TUCK: f32 = 0.30;
const TUCK_AT: f32 = 0.50;

/// One wing, as the drawing needs it.
pub(crate) struct Wing {
    /// The leading edge: shoulder, wrist, tip.
    pub spar: [V3; 3],
    /// The trailing edge at the root and at the wrist. The tip has no chord,
    /// so the trailing edge ends there at the spar.
    pub trail: [V3; 2],
    /// Which way is "up" out of this wing's surface, at the wrist. Whether the
    /// camera is above or below that is the difference between a bird seen
    /// edge-on and one flashing its underside through a turn.
    pub normal: V3,
}

/// A whole bird, in world coordinates.
pub(crate) struct Pose {
    /// The spine, nose first: nose, chest, hip, tail tip.
    pub spine: [V3; 4],
    /// The two back corners of the tail fan.
    pub tail: [V3; 2],
    /// Left wing, right wing.
    pub wings: [Wing; 2],
}

/// The two angles and the fold that the wingbeat actually has.
struct Beat {
    inner: f32,
    hand: f32,
    fold: f32,
}

/// Card 168's shaped wingbeat, kept exactly: one half of the cycle is quicker
/// than the other, which is what tells the eye this is a wing and not an
/// oscillation. What is new is reading the *direction* of travel out of it as
/// well as the height, because the fold belongs to the quick half.
fn beat(b: Bird) -> Beat {
    let warp = |p: f32| (p + 0.35 * p.sin()).sin();
    // How fast the wing is climbing, normalised to roughly -1..1: the
    // derivative of the same warp. The hand wing folds on its own lagged
    // motion, not the shoulder's, so the fold travels out along the wing too.
    let rate = |p: f32| (p + 0.35 * p.sin()).cos() * (1.0 + 0.35 * p.cos()) / 1.35;
    let lagged = b.phase - LAG;

    let amp = DIHEDRAL * (1.0 - 0.75 * b.glide);
    let inner = amp * warp(b.phase) + GLIDE_INNER * b.glide;
    let hand = amp * TIP_GAIN * warp(lagged) + GLIDE_HAND * b.glide;
    // Folded through the upstroke, spread through the downstroke, and the
    // change between them is quick: a wing does not ease into its own fold.
    let fold = smoothstep(-0.35, 0.75, rate(lagged));
    Beat { inner, hand, fold: fold * (1.0 - b.glide) + GLIDE_FOLD * b.glide }
}

/// Where every corner of this bird is, at a drawn wingspan of `span` metres.
///
/// `area` is the level of detail, 0 to 1: at 0 the wings and the tail have no
/// chord at all and the bird is the bare skeleton card 168 drew, which is what
/// a bird three pixels across should be. The caller fades it in with the
/// bird's projected span, so the surface grows out of the line as one comes
/// near rather than appearing all at once.
pub(crate) fn pose(b: Bird, span: f32, area: f32) -> Pose {
    let (right, up, fwd) = b.frame();
    let along = |k: f32| b.pos.add(fwd.scale(k * span));
    let Beat { inner, hand, fold } = beat(b);

    // The tail fans in a glide and through a hard turn, and is folded shut the
    // rest of the time. Measured against the **drawn** lean rather than the
    // honest roll: the tail is part of the same picture as the bank, and a
    // bird that is visibly leaning with a shut tail looks like it is falling
    // over rather than turning.
    let spread = b.glide.max((b.lean.abs() / 0.55).min(1.0));
    let fan = span * (0.090 + 0.060 * spread) * area;
    let tail_tip = along(TAIL_TIP);

    let half = 0.5 * span;
    let wing = |sgn: f32| {
        let side = right.scale(sgn);
        // The inside wing of the turn - the low one - carries more fold than
        // the outside one, which stays reached out. Positive `lean` puts the
        // right wing up, so the left wing is the inside one there.
        let fold = (fold + TUCK * (-sgn * b.lean / TUCK_AT).clamp(0.0, 1.0)).min(1.0);
        let shoulder =
            b.pos.add(fwd.scale(SHOULDER_FWD * span)).add(side.scale(SHOULDER_OUT * span));
        // Out along the span at each segment's own dihedral.
        let (si, ci) = inner.sin_cos();
        let out_in = side.scale(ci).add(up.scale(si));
        let wrist = shoulder
            .add(out_in.scale(INNER * half))
            .sub(fwd.scale((INNER_SWEEP.0 + (INNER_SWEEP.1 - INNER_SWEEP.0) * fold) * span));

        let (sh, ch) = hand.sin_cos();
        let out_hand = side.scale(ch).add(up.scale(sh));
        let (ss, cs) = (SWEEP.0 + (SWEEP.1 - SWEEP.0) * fold).sin_cos();
        // Swept back inside the wing's own plane, and a little shorter with
        // it: a folded wing is drawn in as well as swung back.
        let out_tip = out_hand.scale(cs).sub(fwd.scale(ss));
        let tip = wrist.add(out_tip.scale(HAND * half * (1.0 - 0.10 * fold)));

        Wing {
            spar: [shoulder, wrist, tip],
            trail: [
                shoulder.sub(fwd.scale(ROOT_CHORD * span * area)),
                wrist.sub(fwd.scale(WRIST_CHORD * span * area)),
            ],
            normal: up.scale(ci).sub(side.scale(si)),
        }
    };

    Pose {
        spine: [along(NOSE), along(CHEST), along(HIP), tail_tip],
        tail: [tail_tip.sub(right.scale(fan)), tail_tip.add(right.scale(fan))],
        wings: [wing(-1.0), wing(1.0)],
    }
}
