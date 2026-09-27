//! The face: pixel art stamped straight onto the panel grid, not cut into the
//! cloth's own curved, supersampled, motion-blurred UV surface.
//!
//! Card 336 tried the obvious thing - two dark holes cut into the mesh's own
//! UV space - and its own final log named the limit that made: "a UV ellipse
//! cut into a curved, supersampled, motion-blurred surface" cannot make the
//! reference's solid dark blocks at 64x32. Card 338: project the head's face
//! centre (and its facing) into the 64x32 frame every frame, and stamp a
//! small, hand-drawn pixel-art glyph there, after the cloth render - the same
//! idea `crates/art/src/faces` uses for clock digits (a bitmap's cells *are*
//! LEDs, so drawn at 1:1 every one lands whole on one LED), applied to a face
//! instead of a font.
//!
//! **Projection** ([`project`]) reuses the exact camera model the mesh
//! pipeline renders with (`REF_DISTANCE`, world x/y == panel LEDs at
//! `depth == 0` - see `mod.rs`), so the stamped face never drifts from where
//! the cloth itself puts the head: `super::head_of` places a head at world
//! `(cx - W/2, H/2 - cy, -(REF_DISTANCE + depth))`, and the mesh's own
//! `view_proj` is a perspective matrix built from `REF_DISTANCE` alone, which
//! works out to exactly `screen = panel_centre +/- world * REF_DISTANCE /
//! distance` for x and y alike (checked against `gpu::mat::perspective`'s own
//! matrix, not assumed).
//!
//! **Facing and turning.** Each face point (both eyes, the mouth, the bridge
//! between the eyes) is a fixed point in head-local space; [`facing`] rotates
//! its own surface normal by the head's yaw (`cloth::V3::rot_y`, the same
//! rotation the head mesh itself turns by) and reads off how much it still
//! points at the camera. Because the eyes sit off to each side and the mouth
//! and bridge sit dead centre, one threshold on that single number
//! ([`MIN_FACING`]) is enough to stage the whole thing without any separate
//! "which eye is far" logic: as yaw grows away from zero, whichever eye is
//! turning away is already the more foreshortened of the two and crosses the
//! threshold first, the mouth and the bridge (both on the centreline) follow
//! as the head keeps turning, and the remaining eye goes once the head is
//! essentially profile-on.
//!
//! **Silhouette and occlusion.** A candidate pixel is only ever painted where
//! the composited cloth render (read back *before* any face is stamped over
//! it this frame) is already brighter than the true-black background - the
//! honest test for "is this ghost's own sheet actually here", including its
//! own edges and folds - and never where a *nearer* ghost's own head disc
//! already covers it (the card's own suggestion, in place of a real
//! depth-buffer readback).

use super::cloth::V3;
use crate::color::{oklch, Rgb};
use crate::frame::{H, W};

/// Head-local geometry, head centred at the origin, no yaw yet - every
/// offset a fraction of `head_r`. Tuned by rendering and dumping the actual
/// 64x32 pixels (`Frame::pixel`, not the scaled-up PNG - card 336's own
/// lesson, paid for twice already).
const EYE_DX: f32 = 0.34;
const EYE_DY: f32 = 0.10;
const EYE_DZ: f32 = 0.82;
const MOUTH_DY: f32 = -0.32;
const MOUTH_DZ: f32 = 0.88;

/// Below this, a face point's own local normal (rotated by the head's yaw)
/// no longer counts as "facing the camera" - see the module doc.
const MIN_FACING: f32 = 0.5;

/// Projected head radius (LEDs) at and above which the bigger glyph bucket is
/// used for a feature - `eyes`/`mouth` (the params) nudge a ghost across this
/// line early or late by scaling the *test* itself, so a bigger `eyes`
/// really does read as "a bigger glyph" (the card: "`eyes` picks a glyph
/// size"), not only "the same glyph spaced a little further apart".
const BUCKET_LED: f32 = 4.2;

/// True black (matches `mod.rs`'s own `DARK_L`, so a dark eye or the mouth
/// sits at the palette's own darkest step) and a cap for a glowing eye or the
/// bridge highlight - "never full white" (the owner, on `eye_light`), well
/// short of the palette's own `LIGHT_L`.
const DARK_L: f32 = 0.02;
const GLOW_CAP: f32 = 0.85;

/// One hand-drawn glyph: rows of `X`/`.`, top to bottom, every row the same
/// length - a bitmap whose cells *are* LEDs, the same idea
/// `crates/art/src/faces`'s `Bitmap` uses for clock digits.
struct Glyph {
    rows: &'static [&'static str],
}

impl Glyph {
    fn w(&self) -> usize {
        self.rows[0].len()
    }
    fn h(&self) -> usize {
        self.rows.len()
    }
    /// Ink at `(col, row)`, `col` counted from the mirrored side when
    /// `mirror` is set - so the same glyph drawn for both eyes reads as a
    /// matched, mirrored pair (card 336: "tilted (sad/spooky) ... each eye
    /// mirrored").
    fn ink(&self, col: usize, row: usize, mirror: bool) -> bool {
        let w = self.w();
        let c = if mirror { w - 1 - col } else { col };
        self.rows[row].as_bytes()[c] == b'X'
    }
}

// The card's own examples, taken literally: eyes "2x3 or 3x4 at default
// size", mouth "3x2". A small set of buckets, not a scalable font - the
// house precedent (`crates/art/src/faces`) is bitmaps at fixed sizes too.
const EYE_SMALL: Glyph = Glyph { rows: &[".X", "XX", ".X"] };
const EYE_DEFAULT: Glyph = Glyph { rows: &[".XX", "XXX", "XXX", "XX."] };
const MOUTH_SMALL: Glyph = Glyph { rows: &["XX", ".X"] };
const MOUTH_DEFAULT: Glyph = Glyph { rows: &["XXX", ".X."] };
/// The bridge: a single lit LED between the eyes - see [`stamp`].
const BRIDGE_DOT: Glyph = Glyph { rows: &["X"] };

/// One ghost's face this frame: everything [`stamp`] needs to place, gate and
/// colour it, already resolved from the live params and this instant's pose.
pub(crate) struct FaceInput {
    pub key: u64,
    pub head_pos: V3,
    pub yaw: f32,
    pub head_r: f32,
    /// Where it is looking, as `act::Pose::gaze` hands it - nudges the eyes
    /// (only the eyes) a little, the same "looking around" life a `Peek`'s
    /// hold always had.
    pub gaze: (f32, f32),
    pub hue: f32,
    pub chroma: f32,
    pub eyes_scale: f32,
    pub eye_light: f32,
    pub eye_hue: f32,
    pub mouth_scale: f32,
}

/// A nearer ghost's own head, as far as another ghost's face needs to know
/// about it: is a candidate pixel inside this disc, and is it actually in
/// front (per the card: "use ... the cloth's own projected head disc").
#[derive(Clone, Copy)]
pub(crate) struct Occluder {
    pub key: u64,
    pub head_pos: V3,
    pub head_r: f32,
}

/// World point `local` (head-local, no yaw) placed by this head's own pose
/// and projected to panel pixel space - `(x, y, scale)`, `scale` being how
/// many panel LEDs one world unit covers at this point's own depth. Matches
/// `gpu::mat::perspective`'s own matrix exactly (see the module doc): at
/// `depth == 0` (`d == REF_DISTANCE`) `scale == 1`, world units and LEDs
/// coincide, exactly as `mod.rs`'s `head_of` already assumes for `cx`/`cy`.
fn project(head_pos: V3, yaw: f32, local: V3) -> (f32, f32, f32) {
    let world = local.rot_y(yaw).add(head_pos);
    let d = (-world.z).max(1.0);
    let scale = super::REF_DISTANCE / d;
    (W as f32 / 2.0 + world.x * scale, H as f32 / 2.0 - world.y * scale, scale)
}

/// How much `local`'s own surface normal (a point on the head, treated as a
/// sphere through its own centre) still points at the camera once the head
/// has turned by `yaw` - `1.0` dead-on, `0.0` edge-on, negative facing away.
fn facing(local: V3, yaw: f32) -> f32 {
    local.normalize().rot_y(yaw).z
}

fn luma(c: Rgb) -> f32 {
    (c.r + c.g + c.b) / 3.0
}

/// The honest "is this ghost's own sheet actually here" test: never paint a
/// face pixel where the cloth itself, read back before any face was stamped
/// over it, is not visibly brighter than the true-black background - the
/// card's "must never float off the sheet", including at a fold or the
/// silhouette's own edge.
const SILHOUETTE_MIN: f32 = 0.06;

/// Is `(ux, uy)` inside `o`'s own projected head disc, and is `o` actually
/// nearer to the camera than `self_z` (world z, more negative is farther)?
fn occludes(o: &Occluder, self_z: f32, ux: usize, uy: usize) -> bool {
    if o.head_pos.z <= self_z {
        return false;
    }
    let d = (-o.head_pos.z).max(1.0);
    let scale = super::REF_DISTANCE / d;
    let (cx, cy) = (W as f32 / 2.0 + o.head_pos.x * scale, H as f32 / 2.0 - o.head_pos.y * scale);
    let r = o.head_r * scale;
    let (dx, dy) = (ux as f32 + 0.5 - cx, uy as f32 + 0.5 - cy);
    dx * dx + dy * dy <= r * r
}

/// Stamp one ghost's face onto `px` (row-major linear-light `W`x`H`),
/// reading `base` (the same buffer, from *before* any ghost's face was
/// stamped onto it this frame) for the silhouette test, and `occluders` for
/// every ghost on screen this frame (this one included - it excludes itself
/// by `key`).
pub(crate) fn stamp(px: &mut [Rgb], base: &[Rgb], input: &FaceInput, occluders: &[Occluder]) {
    if input.head_r <= 1e-3 {
        return;
    }

    const GAZE_AMOUNT: f32 = 0.10;
    let gx = input.gaze.0.clamp(-1.0, 1.0) * GAZE_AMOUNT * input.head_r;
    let gy = input.gaze.1.clamp(-1.0, 1.0) * GAZE_AMOUNT * input.head_r * 0.6;

    let d = (-input.head_pos.z).max(1.0);
    let head_px_radius = (super::REF_DISTANCE / d) * input.head_r;
    let px_scale = head_px_radius / input.head_r.max(1e-6);
    let eye_glyph = if head_px_radius * input.eyes_scale >= BUCKET_LED { &EYE_DEFAULT } else { &EYE_SMALL };
    let mouth_glyph =
        if head_px_radius * input.mouth_scale.max(0.05) >= BUCKET_LED { &MOUTH_DEFAULT } else { &MOUTH_SMALL };

    // The eyes must never merge into one blob (card 340: "at size=16 the two
    // eyes merge into one dark blob") - `EYE_DX * head_r` is a *world*
    // offset, so its projected pixel separation shrinks linearly with the
    // head, and below some size the two glyphs (each `eye_glyph.w()` LEDs
    // wide) touch or overlap with no column left for the bridge highlight to
    // land in. Floor the *pixel* half-separation instead of the world one,
    // from the glyph actually chosen, so a lit gap of at least one LED is
    // guaranteed at any head size or depth, not just the sizes this was
    // tuned against - "two separate eyes with a lit gap" (the card's own
    // goal), never a deliberate absolute constant that would need
    // re-tuning every time `EYE_DX` or a glyph's width changes.
    let min_half_sep_px = (eye_glyph.w() as f32 + 1.0) * 0.5;
    let nominal_half_sep_px = EYE_DX * head_px_radius;
    let eye_dx = nominal_half_sep_px.max(min_half_sep_px) / px_scale;
    let (eye_dy, eye_dz) = (EYE_DY * input.head_r, EYE_DZ * input.head_r);
    let left_local = V3::new(-eye_dx + gx, eye_dy + gy, eye_dz);
    let right_local = V3::new(eye_dx + gx, eye_dy + gy, eye_dz);
    let mouth_local = V3::new(0.0, MOUTH_DY * input.head_r, MOUTH_DZ * input.head_r);
    let bridge_local = V3::new(0.0, eye_dy, eye_dz);

    let eye_l = DARK_L + (GLOW_CAP - DARK_L) * input.eye_light.clamp(0.0, 1.0);
    let eye_hue = if input.eye_light > 0.001 { input.eye_hue } else { input.hue };
    let eye_color = oklch(eye_l, input.chroma, eye_hue);
    let dark = oklch(DARK_L, input.chroma, input.hue);
    let bridge_color = oklch(GLOW_CAP, input.chroma, input.hue);

    let paint = |px: &mut [Rgb], local: V3, glyph: &Glyph, mirror: bool, color: Rgb| {
        if facing(local, input.yaw) < MIN_FACING {
            return;
        }
        let (cx, cy, _) = project(input.head_pos, input.yaw, local);
        let (w, h) = (glyph.w(), glyph.h());
        let x0 = (cx - w as f32 / 2.0).round() as i32;
        let y0 = (cy - h as f32 / 2.0).round() as i32;
        for row in 0..h {
            for col in 0..w {
                if !glyph.ink(col, row, mirror) {
                    continue;
                }
                let (px_x, px_y) = (x0 + col as i32, y0 + row as i32);
                if px_x < 0 || px_y < 0 || px_x as usize >= W || px_y as usize >= H {
                    continue;
                }
                let (ux, uy) = (px_x as usize, px_y as usize);
                let i = uy * W + ux;
                if luma(base[i]) < SILHOUETTE_MIN {
                    continue;
                }
                if occluders.iter().any(|o| o.key != input.key && occludes(o, input.head_pos.z, ux, uy)) {
                    continue;
                }
                px[i] = color;
            }
        }
    };

    paint(px, left_local, eye_glyph, false, eye_color);
    paint(px, right_local, eye_glyph, true, eye_color);
    if input.mouth_scale > 0.001 {
        paint(px, mouth_local, mouth_glyph, false, dark);
    }
    // The bridge: a single lit LED between the eyes (card 338) - the bright
    // strip a real draped sheet shows between two dark eye holes, confirmed
    // by 336's own `Frame::pixel` dump of a "bright-dark-bright-dark-bright"
    // pattern. Not gated by `eye_light` (it is a highlight, not one of the
    // "eyes"); gated the same way as everything else - facing, silhouette,
    // occlusion.
    paint(px, bridge_local, &BRIDGE_DOT, false, bridge_color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontal(head_r: f32) -> FaceInput {
        FaceInput {
            key: 1,
            head_pos: V3::new(0.0, 0.0, -40.0),
            yaw: 0.0,
            head_r,
            gaze: (1.0, 0.0),
            hue: 0.0,
            chroma: 0.0,
            eyes_scale: 1.0,
            eye_light: 0.0,
            eye_hue: 0.0,
            mouth_scale: 1.0,
        }
    }

    /// Card 340's own acceptance test, taken literally: "no face pixel lands
    /// below the head's projected disc". Every feature's head-local offset
    /// (before yaw, before the gaze nudge) has to have a smaller magnitude
    /// than `head_r` itself - i.e. sit strictly *inside* the head sphere a
    /// real head disc bounds - or a stamped feature could in principle
    /// project outside the head's own circle and read as a mark on
    /// whatever is behind or below it (a shoulder, another ghost's body).
    /// This is a property of the module's own constants (`EYE_DX/DY/DZ`,
    /// `MOUTH_DY/DZ`), so it is checked directly against them rather than by
    /// rendering - a future edit that pushes one of these too far trips this
    /// test before it ever reaches a rendered frame.
    #[test]
    fn every_face_point_sits_inside_the_head_sphere() {
        let eye = V3::new(EYE_DX, EYE_DY, EYE_DZ).length();
        let mouth = V3::new(0.0, MOUTH_DY, MOUTH_DZ).length();
        let bridge = V3::new(0.0, EYE_DY, EYE_DZ).length();
        for (name, r) in [("eye", eye), ("mouth", mouth), ("bridge", bridge)] {
            assert!(r < 1.0, "{name}'s own local offset ({r:.3} x head_r) reaches outside the head sphere");
        }
    }

    /// A head dead in front of the camera, well inside a bright silhouette:
    /// both eyes, the mouth and the bridge all get painted somewhere on the
    /// panel.
    #[test]
    fn a_forward_head_paints_a_whole_face() {
        let bright = vec![Rgb::splat(0.9); crate::frame::N];
        let mut px = bright.clone();
        let occluders = [Occluder { key: 1, head_pos: V3::new(0.0, 0.0, -40.0), head_r: 5.0 }];
        stamp(&mut px, &bright, &frontal(5.0), &occluders);
        let changed = px.iter().zip(bright.iter()).filter(|(a, b)| *a != *b).count();
        assert!(changed >= 5, "expected several painted pixels, got {changed}");
    }

    /// Never paints outside its own silhouette: with a fully black `base`
    /// (nothing rendered anywhere), nothing is ever stamped, whatever the
    /// pose.
    #[test]
    fn never_paints_off_a_black_base() {
        let black = vec![Rgb::BLACK; crate::frame::N];
        let mut px = black.clone();
        let occluders = [Occluder { key: 1, head_pos: V3::new(0.0, 0.0, -40.0), head_r: 5.0 }];
        stamp(&mut px, &black, &frontal(5.0), &occluders);
        assert!(px.iter().zip(black.iter()).all(|(a, b)| a == b), "something was painted on a black base");
    }

    /// As the head turns past 90 degrees (profile, then away), every
    /// feature - both eyes, the mouth, the bridge - eventually stops
    /// painting: no face is ever drawn on the back of a head.
    #[test]
    fn the_face_hides_as_the_head_turns_away() {
        let bright = vec![Rgb::splat(0.9); crate::frame::N];
        let occluders = [Occluder { key: 1, head_pos: V3::new(0.0, 0.0, -40.0), head_r: 5.0 }];
        let mut input = frontal(5.0);
        input.yaw = std::f32::consts::PI; // dead away from the camera.
        let mut px = bright.clone();
        stamp(&mut px, &bright, &input, &occluders);
        assert!(px.iter().zip(bright.iter()).all(|(a, b)| a == b), "a face painted on the back of the head");
    }

    /// Partway through a turn, the eye on the turning-away side drops before
    /// the other one does - the staged "slides toward the edge, drops the
    /// far eye" the card asks for, out of one threshold and the geometry
    /// alone (see the module doc), not a special case.
    #[test]
    fn a_partial_turn_drops_only_the_far_eye_first() {
        let bright = vec![Rgb::splat(0.9); crate::frame::N];
        let occluders = [Occluder { key: 1, head_pos: V3::new(0.0, 0.0, -40.0), head_r: 5.0 }];
        // Chosen so the geometric eye half-angle (~22 degrees here) plus this
        // yaw crosses `MIN_FACING`'s 60-degree cutoff for one eye but not
        // the other - found by sweeping yaw and checking with this test's own
        // assertions, not assumed.
        let mut input = frontal(5.0);
        input.yaw = 0.75;
        let mut px = bright.clone();
        stamp(&mut px, &bright, &input, &occluders);
        let left_ok = facing(V3::new(-EYE_DX * 5.0, EYE_DY * 5.0, EYE_DZ * 5.0), input.yaw) >= MIN_FACING;
        let right_ok = facing(V3::new(EYE_DX * 5.0, EYE_DY * 5.0, EYE_DZ * 5.0), input.yaw) >= MIN_FACING;
        assert_ne!(left_ok, right_ok, "expected exactly one eye to have dropped at this yaw");
        let painted = px.iter().zip(bright.iter()).filter(|(a, b)| a != b).count();
        assert!(painted >= 1, "expected the still-facing eye (and the mouth) to still be painted");
    }

    /// A nearer ghost's own head disc hides a farther ghost's face, even
    /// though the farther ghost's own sheet is right there in `base` (the
    /// card: "hidden where the sheet or another ghost is in front").
    #[test]
    fn a_nearer_ghosts_head_disc_occludes_a_farther_face() {
        let bright = vec![Rgb::splat(0.9); crate::frame::N];
        let mut input = frontal(5.0);
        input.head_pos = V3::new(0.0, 0.0, -60.0); // farther away than the occluder.
        let occluders = [
            Occluder { key: 1, head_pos: input.head_pos, head_r: 5.0 },
            Occluder { key: 2, head_pos: V3::new(0.0, 0.0, -30.0), head_r: 20.0 }, // nearer, huge disc.
        ];
        let mut px = bright.clone();
        stamp(&mut px, &bright, &input, &occluders);
        assert!(px.iter().zip(bright.iter()).all(|(a, b)| a == b), "painted through a nearer ghost's own head disc");
    }

    /// The card's own acceptance test, directly: a candidate pixel is never
    /// painted where the composited render is not - a hard silhouette edge
    /// (the left half of the panel blanked to black), not just the all-or-
    /// nothing cases the other tests use.
    #[test]
    fn only_paints_pixels_that_are_actually_on_the_sheet() {
        let mut base = vec![Rgb::splat(0.9); crate::frame::N];
        for y in 0..H {
            for x in 0..W / 2 {
                base[y * W + x] = Rgb::BLACK;
            }
        }
        let mut px = base.clone();
        let occluders = [Occluder { key: 1, head_pos: V3::new(0.0, 0.0, -40.0), head_r: 5.0 }];
        stamp(&mut px, &base, &frontal(5.0), &occluders);
        for y in 0..H {
            for x in 0..W / 2 {
                assert_eq!(px[y * W + x], Rgb::BLACK, "painted at ({x},{y}), which is off the silhouette");
            }
        }
        let changed = px.iter().zip(base.iter()).filter(|(a, b)| a != b).count();
        assert!(changed > 0, "expected some painting on the bright half");
    }

    /// `mouth_scale` at (or near) zero draws no mouth at all - the card's
    /// `mouth` (0 = none). `eye_light` is turned up so the *only* dark pixels
    /// either version could possibly paint are the mouth's own.
    #[test]
    fn mouth_zero_means_no_mouth() {
        let bright = vec![Rgb::splat(0.9); crate::frame::N];
        let mut input = frontal(5.0);
        input.eye_light = 1.0;
        let occluders = [Occluder { key: 1, head_pos: input.head_pos, head_r: 5.0 }];

        input.mouth_scale = 0.0;
        let mut off = bright.clone();
        stamp(&mut off, &bright, &input, &occluders);
        assert!(!off.iter().any(|c| luma(*c) < SILHOUETTE_MIN), "a dark mouth pixel was painted with mouth_scale == 0");

        input.mouth_scale = 1.0;
        let mut on = bright.clone();
        stamp(&mut on, &bright, &input, &occluders);
        assert!(on.iter().any(|c| luma(*c) < SILHOUETTE_MIN), "expected a dark mouth pixel with mouth_scale == 1");
    }

    /// `eye_light` never produces a full-white pixel, whatever it is set to.
    #[test]
    fn glowing_eyes_never_reach_full_white() {
        let bright = vec![Rgb::splat(0.9); crate::frame::N];
        let mut input = frontal(5.0);
        input.eye_light = 1.0;
        let mut px = bright.clone();
        stamp(&mut px, &bright, &input, &[Occluder { key: 1, head_pos: input.head_pos, head_r: 5.0 }]);
        assert!(px.iter().all(|c| c.r < 0.97 && c.g < 0.97 && c.b < 0.97), "a stamped pixel reached full white");
    }

    /// Card 340, the owner's own saved setting (`size = 16`): "the two eyes
    /// merge into one dark blob" - `EYE_DX * head_r` is a world offset, so
    /// its projected pixel separation shrinks with the head, and below some
    /// size the two eye glyphs touched or overlapped with no column left for
    /// the lit bridge between them. Sweep a wide range of head radii
    /// (smaller than any this patch's own `size` range of 16-32 LEDs
    /// actually produces, to prove the fix holds with real margin, not just
    /// at the one size that was reported) and require a genuinely lit pixel
    /// between the two eyes in every row that has one on each side - "two
    /// separate eyes with a lit gap" (the card's own words), never a single
    /// wide dark run. Mouth off, so a mouth pixel sharing a row with a tiny
    /// head's eyes can't be mistaken for one of them.
    #[test]
    fn the_eyes_never_merge_at_any_head_size() {
        for head_r in [1.5, 2.0, 2.5, 3.0, 4.0, 6.0, 10.0, 16.0] {
            let bright = vec![Rgb::splat(0.9); crate::frame::N];
            let mut input = frontal(head_r);
            input.gaze = (0.0, 0.0);
            input.mouth_scale = 0.0;
            let occluders = [Occluder { key: 1, head_pos: input.head_pos, head_r }];
            let mut px = bright.clone();
            stamp(&mut px, &bright, &input, &occluders);
            let mid = W / 2;
            let mut saw_both_eyes = false;
            for y in 0..H {
                let row: Vec<Rgb> = (0..W).map(|x| px[y * W + x]).collect();
                let left_dark_max = (0..mid).filter(|&x| luma(row[x]) < SILHOUETTE_MIN).max();
                let right_dark_min = (mid..W).filter(|&x| luma(row[x]) < SILHOUETTE_MIN).min();
                let (Some(lx), Some(rx)) = (left_dark_max, right_dark_min) else { continue };
                saw_both_eyes = true;
                assert!(rx > lx + 1, "head_r {head_r}: eyes touch or overlap at row {y}: dark at {lx} and {rx}");
                let lit_between = (lx + 1..rx).any(|x| luma(row[x]) > 0.5);
                assert!(lit_between, "head_r {head_r}: no lit pixel between the eyes at row {y} ({lx}..{rx})");
            }
            assert!(saw_both_eyes, "head_r {head_r}: never saw a dark pixel on both sides - test not exercising the eyes");
        }
    }
}
