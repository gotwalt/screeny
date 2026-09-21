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

/// The spine, as fractions of the full wingspan measured forward from the
/// bird's position: nose, head, chest, hip, tail tip. **A dart when the bird
/// is small and a bird when it is big**, blended on the same level-of-detail
/// number the wing's chord uses.
///
/// Card 168's bird is 0.70 of a wingspan from beak to tail. A real gull is
/// nearer 0.46, and at this resolution that difference is the whole
/// silhouette. At two or three LEDs the long dart is what says *flying*: it is
/// the only thing left once the wings are a single pixel each, and the owner
/// said of that picture "this is great". At sixteen it is what made the bird
/// read as a paper dart instead of a bird - the commonest view in this patch
/// is from behind or ahead, where the body carries the whole shape and a body
/// two thirds of a wingspan long simply is not a bird's.
///
/// So the far side of the flock keeps card 168's proportions exactly, and a
/// bird near enough for its wings to have any shape gets a body short enough
/// to belong to them. Nothing is switched: the spine slides.
///                        nose  head  chest    hip    tail
const DART: [f32; 5] = [0.30, 0.22, 0.05, -0.16, -0.40];
const BIRD: [f32; 5] = [0.21, 0.15, 0.04, -0.12, -0.27];

/// Where the wings are hinged: a little forward of the middle and barely off
/// the centre line, because at this size the two shoulders are the same pixel.
const SHOULDER_FWD: f32 = 0.12;
const SHOULDER_OUT: f32 = 0.05;

/// How the semi-span is divided at the wrist. The hand wing is the longer
/// half, which is what makes folding it shorten the span as much as it does.
const INNER: f32 = 0.45;
const HAND: f32 = 0.55;

/// Chord at the root and at the wrist, as fractions of the span; the tip has
/// none, so the wing tapers to a point. The spar the chord hangs behind is the
/// leading edge, which is where a wing's bones really are.
///
/// **Nearly constant from the body out to the wrist, and then all of the
/// taper is in the hand.** The first version made the wing widest where it met
/// the body, which is what turned a bird seen from below into a cross: the
/// root chord reached back past the hip and the wing, the body and the tail
/// were one mass with no waist anywhere in it. Held to 0.145 the root's
/// trailing edge lands well forward of the hip, so there is a **notch**
/// between the back of the wing and the tail - which is the outline that says
/// "bird" from below and from behind, and the one thing the old silhouette
/// never had.
const ROOT_CHORD: f32 = 0.145;
const WRIST_CHORD: f32 = 0.135;

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

/// How far the hand wing bends *away* from the way it is travelling, at full
/// speed, in radians. A wing is not a rod: the air loads it and the tip
/// trails - up through the downstroke, down through the upstroke.
///
/// It earns its place at one moment in particular. Half way through either
/// stroke the inner wing is level and the lag alone leaves the hand nearly
/// level with it, so seen head-on - which, with the camera flying inside the
/// flock, is half of what you ever see - the bird is a straight bar, and a
/// straight bar is the one thing a flying bird never looks like. This is
/// largest exactly there and vanishes at the top and bottom of the stroke,
/// where the beat's own angles are already doing the work, so it deepens the
/// flat moments without touching the extremes.
const CAMBER: f32 = 0.40;

/// A glide: wings held slightly raised with the hand wing dropped a touch
/// below them - the gull "M" - and the wrist half folded.
const GLIDE_INNER: f32 = 0.24;
const GLIDE_HAND: f32 = -0.30;
const GLIDE_FOLD: f32 = 0.34;

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
    /// The spine, nose first: nose, head, chest, hip, tail tip. The chest is
    /// the fattest point and sits just behind the shoulder; ahead of it the
    /// neck and the head are short and thin, which is the difference between
    /// a bird and a dart with a long nose.
    pub spine: [V3; 5],
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
    let travel = rate(lagged);
    let inner = amp * warp(b.phase) + GLIDE_INNER * b.glide;
    let hand = amp * TIP_GAIN * warp(lagged) - CAMBER * travel * (1.0 - b.glide)
        + GLIDE_HAND * b.glide;
    // Folded through the upstroke, spread through the downstroke, and the
    // change between them is quick: a wing does not ease into its own fold.
    let fold = smoothstep(-0.35, 0.75, travel);
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

    // The spine slides from the dart to the bird with the same number that
    // grows the wings' surface, so a bird never has one without the other.
    let mut spine = [b.pos; 5];
    for (i, p) in spine.iter_mut().enumerate() {
        *p = along(DART[i] + (BIRD[i] - DART[i]) * area);
    }

    // The tail fans in a glide and through a hard turn, and is folded shut the
    // rest of the time. Both of those the bird already carries; neither needs
    // anything new in `sim`.
    let spread = b.glide.max((b.roll.abs() / 0.45).min(1.0));
    let fan = span * (0.090 + 0.060 * spread) * area;
    let tail_tip = spine[4];

    let half = 0.5 * span;
    let wing = |sgn: f32| {
        let side = right.scale(sgn);
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
        spine,
        tail: [tail_tip.sub(right.scale(fan)), tail_tip.add(right.scale(fan))],
        wings: [wing(-1.0), wing(1.0)],
    }
}
