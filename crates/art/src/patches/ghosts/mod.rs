//! `ghosts`: classic sheet ghosts, bouncing in and out of the frame.
//!
//! The owner, 2026-09-26: "ghosts would be a classic - i imagine them just
//! sort of bouncing in and out of the frame, left to right and so on."
//! Playful, not horror.
//!
//! **The picture** is one shape drawn many ways: a rounded dome head, sides
//! that fall to a wavy, scalloped hem, two dark eyes that look where the
//! ghost is going. [`body_sdf`] is the whole silhouette, in the ghost's own
//! LEDs-from-its-shoulder coordinates - no supersampled buffer of its own,
//! because at one to four ghosts a plain per-sample evaluation is cheap
//! enough that nothing needs sharing across pixels (contrast `flock`'s
//! [`crate::patches::flock::Coverage`], which exists because fifty birds'
//! strokes do).
//!
//! **The motion** - drift, bounce, peek, swoop, cross, chase - is
//! [`act`]'s: a small vocabulary of closed-form trajectories, scheduled by a
//! [`act::Director`] that keeps a long run from settling into a loop.
//!
//! **The colour** is one ray, exactly as `vesta` draws its numerals: a hue
//! and a lightness, with `color` (0 = grayscale) scaling only the chroma, so
//! the whole scene - ghost, glow, ground line, stars - is one scalar "how far
//! up the ray" field, painted by index into a palette built from that same
//! ray (`GUARANTEED_PALETTE` and below, so every frame goes out exact).

pub(crate) mod act;

use crate::color::{oklch, smoothstep, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, W};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use act::{Director, Pose, Shape};
use std::f32::consts::PI;

pub const DEF: PatchDef = PatchDef {
    id: "ghosts",
    name: "Ghosts",
    blurb: "Classic sheet ghosts, bouncing in and out of the frame: drifting, bouncing, peeking, swooping, crossing, chasing.",
    params: PARAMS,
    make,
    // The schedule, every shape's proportions, and the stars are all drawn
    // from the seed: a new one is a different night of ghosts (card 151).
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("ghosts", "How many at once", 1.0, 4.0, 1.0, 2.0),
    param("pace", "How fast they move (lower is slower, dreamier)", 0.3, 2.2, 0.05, 1.0),
    param("bounce", "Bouncy vs floaty", 0.0, 1.0, 0.01, 0.5),
    param("size", "How big they are (LEDs tall)", 5.0, 20.0, 0.5, 11.0),
    param("glow", "Soft edge glow", 0.0, 1.0, 0.01, 0.22),
    param("color", "Colour (0 = grayscale)", 0.0, 1.0, 0.01, 0.32),
    param("hue", "Tint, when colour is on (0 = red, the cool default is ~205)", 0.0, 360.0, 1.0, 205.0),
];

// ---------------------------------------------------------------- geometry

/// A ghost's own coordinates: `u` across from its centreline, `v` down from
/// its shoulder (negative is up into the dome).
///
/// The dome (`v <= 0`) is a circle of radius `r`; the sides (`0 < v <=
/// hem_base`) are straight, `r` either way of the centreline; below that the
/// hem's edge is `hem_base + hem_amp * (0.5 + 0.5*cos(...))` - a row of
/// smooth points that never rises above the straight sides' own bottom, so
/// the silhouette is a dome and shoulders with a fringe hanging off it, not a
/// shape that grows past its own frame. All three pieces agree exactly at
/// their seam (`v = 0`, `v = hem_base` both give `r - |u|` from both sides of
/// the join), so this is continuous everywhere a pixel might sample it.
///
/// Not an exact signed distance - the corners where dome meets side and hem
/// wave meets side are a `min` of two half-plane-ish terms, which under-
/// estimates distance right at a corner - but at 6-20 LEDs tall the error is
/// a fraction of a pixel and disappears under anti-aliasing.
fn body_sdf(u: f32, v: f32, s: Shape, ripple: f32) -> f32 {
    if v <= 0.0 {
        s.r - (u * u + v * v).sqrt()
    } else if v <= s.hem_base {
        s.r - u.abs()
    } else {
        let hump = 0.5 + 0.5 * (s.humps * PI * u / s.r + ripple).cos();
        (s.r - u.abs()).min(s.hem_base + s.hem_amp * hump - v)
    }
}

/// The dome's own half of the shape, for placing eyes and a mouth: `v` such
/// that `(u, v)` sits at `frac` of the way from the shoulder (0) to the apex
/// (-1), scaled by `r`.
fn face_at(r: f32, frac: f32) -> f32 {
    -r * frac
}

const AA: f32 = 0.55;
const GLOW_REACH: f32 = 1.7;

/// A ghost's own eyes and, above a size worth drawing one, a mouth: dark
/// holes in the body, not separate strokes over it - so an eye occludes
/// whatever this ghost's own hem or another ghost's translucent body would
/// otherwise show through it.
struct Face {
    eye_r: f32,
    eye_y: f32,
    eye_dx: f32,
    gaze_shift: f32,
    mouth: Option<(f32, f32)>,
}

/// Below this total height there is no room for a mouth: the brief's own
/// words, "an 'O' mouth if there is room."
const MOUTH_MIN_HEIGHT: f32 = 11.0;

impl Face {
    fn of(s: Shape) -> Face {
        Face {
            eye_r: (s.r * 0.24).clamp(0.5, 1.4),
            eye_y: face_at(s.r, 0.32),
            eye_dx: s.r * 0.4,
            gaze_shift: s.r * 0.16,
            mouth: (s.total_height() >= MOUTH_MIN_HEIGHT).then(|| (face_at(s.r, 0.02), (s.r * 0.14).clamp(0.35, 0.9))),
        }
    }

    /// How dark this point should push the body: 1 for the centre of an eye,
    /// fading to 0 a pixel out; the mouth the same but weaker, per
    /// `MOUTH_DARK`.
    fn darken(&self, u: f32, v: f32, gaze: (f32, f32)) -> f32 {
        // A tighter edge than the body's own: an eye this small would nearly
        // dissolve under the body's softer anti-aliasing.
        const EYE_AA: f32 = 0.32;
        let shift = (gaze.0 * self.gaze_shift, gaze.1 * self.gaze_shift * 0.6);
        let eye = |ex: f32| {
            let (dx, dy) = (u - (ex + shift.0), v - (self.eye_y + shift.1));
            smoothstep(self.eye_r + EYE_AA, self.eye_r - EYE_AA, (dx * dx + dy * dy).sqrt())
        };
        let mut d = eye(-self.eye_dx).max(eye(self.eye_dx));
        if let Some((my, mr)) = self.mouth {
            let (dx, dy) = (u, v - my);
            let m = smoothstep(mr + EYE_AA, mr - EYE_AA, (dx * dx + dy * dy).sqrt());
            d = d.max(m * MOUTH_DARK);
        }
        d
    }
}

const MOUTH_DARK: f32 = 0.55;

// ---------------------------------------------------------------- colour

/// The scene's one hue, as a ray in linear light with its brightest channel
/// at 1 - `vesta::ray`'s trick, so blending along it (anti-aliasing,
/// translucency, the ramp itself) never drifts hue. `color` scales chroma
/// only, per the common rule: at 0 this is a pure grey ray.
fn ray(hue: f32, color: f32) -> Rgb {
    let c = oklch(0.86, 0.09 * color.clamp(0.0, 1.0), hue);
    let peak = c.r.max(c.g).max(c.b).max(1e-6);
    c.scale(1.0 / peak)
}

/// The brightest thing in the picture, a ghost's own body - well below full
/// white, per the brief's brightness rule.
const BODY: f32 = 0.74;
/// The faintest ramp step the palette bothers with, as a share of `BODY`.
const RAMP_DARK: f32 = 0.05;
const STEPS: usize = 27;

const GLOW_MAX: f32 = 0.24;
/// A background level, expressed the same way the palette's own ramp is
/// (`l` on the 0..1 perceptual scale, cubed into raw linear light) so "how
/// bright does this look" is chosen directly instead of guessed at in raw
/// terms and re-checked against a render.
fn level(l: f32) -> f32 {
    BODY * l * l * l
}
const GROUND_L: f32 = 0.14;
const STAR_L: f32 = 0.24;

fn palette(hue: f32, color: f32) -> Palette {
    let tint = ray(hue, color);
    let mut colours = vec![Rgb::BLACK];
    colours.extend((0..STEPS).map(|k| {
        // The cube, as `vesta` does it: OKLCH lightness goes roughly as the
        // cube root of linear light, so evenly spaced `l` here lands as
        // evenly spaced *perceived* steps, spending most of the 27 on the
        // dark range this scene actually lives in.
        let l = RAMP_DARK + (1.0 - RAMP_DARK) * k as f32 / (STEPS - 1) as f32;
        tint.scale(BODY * l * l * l)
    }));
    Palette::new(colours, 0.03)
}

// ---------------------------------------------------------------- the patch

/// A few dim points, fixed for the run: "a quiet ground line or a few dim
/// stars at most, as a bearing" (card 315).
const STARS: usize = 3;

struct Ghosts {
    director: Director,
    stars: [(f32, f32); STARS],
    /// The ground line's height, once, from the seed - not the panel's exact
    /// bottom, so it reads as a horizon rather than a frame edge.
    ground_y: f32,
}

fn make(seed: u64) -> Box<dyn Patch> {
    let mut rng = crate::rng::Rng::new(seed ^ 0x67_68_73_74 ^ 0x73_74_61_72);
    let stars = std::array::from_fn(|_| (rng.range(2.0, W as f32 - 2.0), rng.range(1.0, H as f32 * 0.4)));
    Box::new(Ghosts { director: Director::new(seed), stars, ground_y: rng.range(H as f32 - 3.0, H as f32 - 1.2) })
}

impl Patch for Ghosts {
    fn playing(&self) -> Option<Playing> {
        let act = self.director.last()?;
        Some(Playing { title: "Ghosts".to_string(), detail: act.name.clone(), actions: Vec::new(), notes: Vec::new() })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.director.advance(ctx.t, ctx);
        let poses = self.director.poses_at(ctx.t);
        let glow = ctx.get("glow");

        let tint = ray(ctx.get("hue"), ctx.get("color"));
        let frame = Frame::supersample(6, |x, y| tint.scale(sample(x, y, &poses, glow, self.ground_y, &self.stars)));
        palette(ctx.get("hue"), ctx.get("color")).map(&frame, Dither::BlueNoise, 0.5)
    }
}

/// The scene's brightness share at one point, before the ray's hue is
/// applied: 0..1, where [`BODY`] is the brightest a ghost's own core reaches.
fn sample(x: f32, y: f32, poses: &[Pose], glow: f32, ground_y: f32, stars: &[(f32, f32); STARS]) -> f32 {
    let mut v = background(x, y, ground_y, stars);
    for pose in poses {
        let (u, w) = ((x - pose.cx) / pose.stretch_x, (y - pose.cy) / pose.stretch_y);
        let d = body_sdf(u, w, pose.shape, pose.ripple);
        let body_cov = smoothstep(-AA, AA, d);

        // A soft halo just outside the crisp silhouette - "a faint glow ...
        // if it reads" - lightens only, so it never fights the eyes' dark
        // holes or another ghost's edge sitting on top of it.
        if glow > 0.0 && body_cov < 1.0 {
            let halo = smoothstep(-GLOW_REACH, 0.0, d) * (1.0 - body_cov);
            v = v.max(GLOW_MAX * glow * halo);
        }
        if body_cov <= 0.0 {
            continue;
        }

        let dark = Face::of(pose.shape).darken(u, w, pose.gaze);
        let level = BODY * (1.0 - dark);
        // Eyes (and the mouth) are true dark holes: they occlude fully
        // whatever is behind them, even where the body itself is
        // translucent - `dark` pushes the effective opacity to 1 exactly
        // where it pushes the level to 0.
        let opacity = (pose.shape.alpha + (1.0 - pose.shape.alpha) * dark).clamp(0.0, 1.0);
        v += (level - v) * body_cov * opacity;
    }
    v.clamp(0.0, BODY)
}

/// The ground line and the stars: what's behind the ghosts, kept to the
/// brief's "at most, as a bearing".
fn background(x: f32, y: f32, ground_y: f32, stars: &[(f32, f32); STARS]) -> f32 {
    let mut v = smoothstep(0.65, 0.0, (y - ground_y).abs()) * level(GROUND_L);
    for (sx, sy) in stars {
        let (dx, dy) = (x - sx, y - sy);
        let cov = smoothstep(0.9, 0.1, (dx * dx + dy * dy).sqrt());
        v = v.max(cov * level(STAR_L));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Params;

    /// Render the patch itself (no pipeline, matching `vesta`'s and
    /// `flock`'s own tests): stepped at the snapshot tool's 30 fps from
    /// engine time zero up to `at`.
    fn frame_at(seed: u64, at: f64, set: &[(&str, f32)]) -> Frame {
        let mut p = Params::defaults(DEF.params);
        for (k, v) in set {
            assert!(p.set(DEF.params, k, *v), "no parameter `{k}`");
        }
        let mut patch = (DEF.make)(seed);
        let dt = 1.0 / crate::snapshot::FPS;
        let steps = (at / dt).round() as usize;
        let mut frame = Frame::black();
        for i in 0..=steps {
            let t = i as f64 * dt;
            frame = patch.render(&Ctx { t, dt, now: t, params: &p });
        }
        frame
    }

    fn colours(frame: &Frame) -> Vec<[f32; 3]> {
        (0..crate::frame::N).map(|i| { let c = frame.pixel(i); [c.r, c.g, c.b] }).collect()
    }

    /// The same seed and the same moment draw the same frame, exactly -
    /// `screeny-art snapshot --seed N --at S` is a promise.
    #[test]
    fn a_seed_and_a_moment_are_deterministic() {
        for at in [3.0, 41.5, 122.25] {
            assert_eq!(colours(&frame_at(7, at, &[])), colours(&frame_at(7, at, &[])), "at {at}s");
        }
    }

    /// At `color` 0 the frame is grayscale: every pixel's three channels are
    /// equal (the card's grayscale rule, checked rather than assumed).
    #[test]
    fn color_zero_is_grayscale() {
        for at in [2.0, 30.0, 90.0] {
            let frame = frame_at(3, at, &[("color", 0.0)]);
            for c in colours(&frame) {
                assert!((c[0] - c[1]).abs() < 1e-4 && (c[1] - c[2]).abs() < 1e-4, "at {at}s: {c:?} is not grey");
            }
        }
    }

    /// Never a full-white frame, and the average level stays low - this runs
    /// off laptop USB (the common brightness rule).
    #[test]
    fn never_bright_and_never_a_lot_of_it() {
        for at in [5.0, 60.0, 200.0] {
            let frame = frame_at(1, at, &[("ghosts", 4.0), ("glow", 1.0)]);
            let px = colours(&frame);
            let apl = px.iter().map(|c| (c[0] + c[1] + c[2]) / 3.0).sum::<f32>() / px.len() as f32;
            assert!(apl < 0.12, "at {at}s: average picture level {apl}");
            assert!(px.iter().all(|c| c[0] < 0.95 && c[1] < 0.95 && c[2] < 0.95), "at {at}s: something is at full white");
        }
    }

    /// Every frame is at most `STEPS + 1` colours - well inside
    /// `GUARANTEED_PALETTE` - so it goes out exact whatever the sender's
    /// palette-size ladder does with it.
    #[test]
    fn every_frame_is_a_small_exact_palette() {
        let frame = frame_at(5, 45.0, &[("ghosts", 4.0)]);
        let Frame::Indexed { palette, .. } = frame else { panic!("ghosts must render indexed") };
        assert!(palette.len() <= crate::frame::GUARANTEED_PALETTE, "{} colours", palette.len());
    }
}
