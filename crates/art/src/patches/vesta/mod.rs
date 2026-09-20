//! `vesta`: a split-flap clock for a dark room.
//!
//! Four flap modules and a colon, `HH:MM`, red on black. The owner asked for
//! "a simple low light clock for night time ... render in 3d as if it were a
//! vestaboard, where the numerals animate vertically on change" (card 155).
//!
//! **What a split-flap is.** Each module is a stack of cards on a horizontal
//! axle through its middle. At rest the top half of the character is on the
//! card above the axle and the bottom half on the card below, with a thin dark
//! seam between them - the seam is the signature. On a change the upper card
//! falls forward about the axle: the *next* character's top half is already
//! standing behind it, the falling card carries the old top half on its front
//! until it is edge-on, and brings the new bottom half down on its back. A
//! real module gets from 3 to 7 by flipping through 4, 5 and 6, which is what
//! `cascade` does.
//!
//! **Low light, and its trap.** Card 102 measured that sRGB codes under about
//! 38 are made of very few panel refreshes, so large areas of them sparkle -
//! in a dark bedroom, exactly what you would see. So dim is reached by
//! lighting *few* pixels at a *moderate* level, not many at a very low one:
//! the card bodies are true black, the numerals are pure red (one LED die a
//! pixel, and kind to night vision), and the rest is the panel's own
//! brightness control, which costs no depth. Nothing here paints a grey card.
//! The 3D has to come from the numerals' own shading, the seam, the falling
//! card's lit edge and the shadow it throws on the plate below - and it does.
//!
//! **Size.** The owner withdrew his own "fill only maybe half the pixels": "I
//! think I'd rather spend the pixels than be artificially constrained". A
//! falling card only looks like one if it has rows to foreshorten through, so
//! the modules are as large as `HH:MM` allows - 14 x 30 LEDs each, a 4-LED
//! colon, one LED of true black between the modules of a pair. `size` shrinks
//! the whole clock for comparison; `fill` halves the light without shrinking
//! anything.
//!
//! Everything else is `flap.rs` (how a card falls and what it looks like from
//! where we sit) and `glyphs.rs` (ten numerals drawn for this size).

pub(crate) mod flap;
pub(crate) mod glyphs;

use crate::color::{oklch, smoothstep, srgb8_to_linear, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, W};
use crate::palette::Palette;
use crate::patch::{param, toggle, Ctx, ParamSpec, Patch, PatchDef, Playing};
use flap::Fall;

pub const DEF: PatchDef = PatchDef {
    id: "vesta",
    name: "Vesta",
    blurb: "A split-flap night clock: HH:MM on four flap modules, red on black, the cards falling under gravity as the minute turns. Built for a dark room - few pixels, moderate level, pure red.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[
    param("light", "Numeral level (sRGB code)", 40.0, 255.0, 1.0, 120.0),
    param("hue", "Hue (0 = pure red)", 0.0, 360.0, 1.0, 0.0),
    param("size", "Size", 0.55, 1.0, 0.01, 1.0),
    param("weight", "Stroke weight (LEDs)", 1.2, 3.0, 0.1, 2.0),
    param("seam", "Seam (LEDs)", 0.0, 3.0, 0.5, 2.0),
    param("flip", "A card's fall (seconds)", 0.08, 0.6, 0.01, 0.2),
    toggle("cascade", "Flip through the numerals between", true),
    param("tilt", "Viewpoint above the board (deg)", 0.0, 40.0, 1.0, 16.0),
    param("fill", "Halftone (0 = solid)", 0.0, 0.6, 0.05, 0.0),
    param("pace", "Seconds per minute (60 = real clock)", 5.0, 60.0, 1.0, 60.0),
    toggle("blink", "Colon blinks", false),
    toggle("hours24", "24-hour", true),
    param("offset", "Time offset (minutes)", 0.0, 1439.0, 1.0, 0.0),
];

// ---------------------------------------------------------------- layout

/// One module, in LEDs, at `size` 1. 14 x 30 is as large as four of them, a
/// colon and a black gap between each pair will go on 64 x 32, and 15 rows
/// either side of the axle is enough for a card to foreshorten through: a
/// half-card 15 rows tall passes 15, 14, 12, 9, 5, 1 rows on its way down,
/// where a 6-row one would be a blink.
const MODULE_W: f32 = 14.0;
const MODULE_H: f32 = 30.0;
/// True black between the two modules of a pair, so each reads as its own card.
const PAIR_GAP: f32 = 1.0;
const COLON_W: f32 = 4.0;

/// Module centres, at `size` 1, measured out from the middle of the colon:
/// columns `1..15  16..30  [colon]  34..48  49..63`, which is 62 of the 64.
const CENTRES: [f32; 4] = [
    W as f32 * 0.5 - COLON_W * 0.5 - MODULE_W * 1.5 - PAIR_GAP,
    W as f32 * 0.5 - COLON_W * 0.5 - MODULE_W * 0.5,
    W as f32 * 0.5 + COLON_W * 0.5 + MODULE_W * 0.5,
    W as f32 * 0.5 + COLON_W * 0.5 + MODULE_W * 1.5 + PAIR_GAP,
];

/// Half the colon dots' spacing either side of the axle, and their radius.
const COLON_AT: f32 = 6.0;
const COLON_R: f32 = 1.15;

/// The numeral in the shadow of a falling card, as a share of its own level.
/// Not black: a cast shadow that swallowed the numeral would read as the
/// numeral going out, not as something passing over it.
const SHADOW: f32 = 0.22;
/// Half the width of the shadow's penumbra, in LEDs.
const PENUMBRA: f32 = 0.6;
/// Half the width of the lit edge, in LEDs on the panel - a screen width, not
/// a card width, because as the card turns edge-on the edge is all there is.
/// Only the inward half of it falls on the card, so this is about one LED of
/// highlight, which is the least that reads.
const EDGE_HALF: f32 = 1.1;
/// How much brighter the lit edge is than the numeral.
const EDGE_BOOST: f32 = 3.0;
/// The colon, against the numerals. Punctuation, not a digit.
const COLON_LEVEL: f32 = 0.85;

/// The steps of the one ramp, plus black: 32 colours, so every frame goes on
/// the wire exactly whatever the picture does (`GUARANTEED_PALETTE`).
const STEPS: usize = 31;
/// The faintest step, as a share of the top of the ramp in OKLCH lightness.
/// Card 115's reasoning: any higher and the faintest edge a numeral can have
/// is a pixel it covers several percent of, and everything below that rounds
/// up to it, which is what makes a stroke look blunt.
const DARK: f32 = 0.16;

/// How long the hint of a settle lasts, as a share of a card's fall, and how
/// far the card lifts back off the stack.
const SETTLE: f64 = 0.45;
const SETTLE_DEG: f32 = 6.0;

/// sRGB red's own OKLCH hue. `hue` is an offset from it, so 0 is red.
const RED: f32 = 29.234;
/// A channel below this share of the brightest one is turned off.
///
/// OKLCH is the right place to pick a hue - it is the only way a hue control
/// behaves - but its gamut search stops just *inside* the boundary, so at red
/// it leaves a thousandth of green behind. That is a second LED die lit at
/// about sRGB 3 in every numeral pixel: invisible as colour, visible as the
/// dark end's sparkle, and exactly what a night clock must not do. 0.002 in
/// linear light is sRGB 7, about the darkest the panel can show at all.
const FLOOR: f32 = 0.002;

/// The colour a hue asks for, as a ray in linear light with its brightest
/// channel at 1. Everything in the picture is a point on this ray, which is
/// why 32 colours are enough.
fn ray(hue: f32) -> Rgb {
    // Below sRGB red's own lightness, so the gamut boundary at `RED` is the
    // red corner itself rather than a point on a face near it.
    let c = oklch(0.58, 0.4, RED + hue);
    let peak = c.r.max(c.g).max(c.b).max(1e-6);
    let c = c.scale(1.0 / peak);
    let cut = |v: f32| if v < FLOOR { 0.0 } else { v };
    Rgb::new(cut(c.r), cut(c.g), cut(c.b))
}

// ---------------------------------------------------------------- the patch

/// One flap module: what it shows, what it is on its way to, and where the
/// falling card is.
#[derive(Clone, Copy, Debug)]
struct Module {
    /// The numeral the module is showing, or falling away from.
    shown: u8,
    /// The numeral this fall lands on.
    to: u8,
    /// Engine time the fall - or the settle after it - began.
    began: f64,
    state: State,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Still,
    Falling,
    Settling,
}

impl Module {
    /// Advance to engine time `t`, heading for `target`.
    fn step(&mut self, t: f64, target: u8, fall: f64, cascade: bool) {
        if self.state == State::Falling && t - self.began >= fall {
            self.shown = self.to;
            self.state = if self.shown == target { State::Settling } else { State::Still };
            self.began = t;
        }
        if self.state == State::Settling && t - self.began >= fall * SETTLE {
            self.state = State::Still;
        }
        // A new target interrupts a settle but never a fall: a card that has
        // let go is falling whatever the clock does next.
        if self.state != State::Falling && self.shown != target {
            self.to = if cascade { (self.shown + 1) % 10 } else { target };
            self.began = t;
            self.state = State::Falling;
        }
    }

    /// What to draw: the numeral on the falling card's front, the one behind
    /// it, and where the card is.
    fn pose(&self, t: f64, fall: f64) -> Pose {
        match self.state {
            State::Still => Pose { old: self.shown, new: self.shown, theta: 0.0 },
            State::Falling => {
                let u = if fall > 0.0 { ((t - self.began) / fall) as f32 } else { 1.0 };
                Pose { old: self.shown, new: self.to, theta: flap::angle(u) }
            }
            // Landed. The card that fell is now the plate below, so both
            // halves are the same numeral and only the card's small rebound
            // is left - which is the one thing in the patch that is not
            // physics but a hint of one.
            State::Settling => {
                let tau = if fall > 0.0 { ((t - self.began) / (fall * SETTLE)) as f32 } else { 1.0 };
                Pose { old: self.shown, new: self.shown, theta: 180.0 - flap::settle(tau, SETTLE_DEG) }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Pose {
    old: u8,
    new: u8,
    theta: f32,
}

struct Vesta {
    /// The time of day on the first frame; a sped-up clock (`pace` < 60) runs
    /// on from here, exactly as the other clock patches do.
    born: Option<f64>,
    modules: [Module; 4],
    /// For the studio's "now playing".
    doing: String,
}

fn make(_seed: u64) -> Box<dyn Patch> {
    Box::new(Vesta { born: None, modules: [Module { shown: 0, to: 0, began: 0.0, state: State::Still }; 4], doing: String::new() })
}

/// The hours and minutes a minute-of-day shows. Leading zeros are kept: a flap
/// board has a card for every position and `09:05` is `0905`.
fn digits(minute: i64, hours24: bool) -> (u32, u32) {
    let (h, m) = ((minute.div_euclid(60).rem_euclid(24)) as u32, minute.rem_euclid(60) as u32);
    (if hours24 { h } else { (h + 11) % 12 + 1 }, m)
}

impl Patch for Vesta {
    fn playing(&self) -> Option<Playing> {
        let face: String = self.modules.iter().enumerate().map(|(i, m)| if i == 2 { format!(":{}", m.shown) } else { m.shown.to_string() }).collect();
        Some(Playing { title: face, detail: self.doing.clone(), actions: vec![], notes: vec![] })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        let pace = ctx.get("pace");
        // Display seconds per engine second. At pace 60 the wall clock is used
        // directly, so the time is right however long the patch has run.
        let rate = 60.0 / pace as f64;
        let born_now = self.born.is_none();
        let born = *self.born.get_or_insert(ctx.now - ctx.t);
        let clock = if pace >= 59.5 { ctx.now } else { born + ctx.t * rate } + f64::from(ctx.get("offset")) * 60.0;
        let minute = (clock / 60.0).floor() as i64;
        let (hh, mm) = digits(minute, ctx.get("hours24") >= 0.5);
        let target = [hh / 10, hh % 10, mm / 10, mm % 10].map(|d| d as u8);

        let fall = f64::from(ctx.get("flip"));
        let cascade = ctx.get("cascade") >= 0.5;
        for (m, want) in self.modules.iter_mut().zip(target) {
            if born_now {
                // Born reading the time, not flipping its way up to it.
                *m = Module { shown: want, to: want, began: ctx.t, state: State::Still };
            } else {
                m.step(ctx.t, want, fall, cascade);
            }
        }
        let moving = self.modules.iter().filter(|m| m.state == State::Falling).count();
        self.doing = match moving {
            0 => "settled".to_string(),
            n => format!("{n} of 4 flipping"),
        };

        let poses: [Pose; 4] = std::array::from_fn(|i| self.modules[i].pose(ctx.t, fall));
        let colon_on = ctx.get("blink") < 0.5 || clock.rem_euclid(1.0) < 0.55;
        picture(&poses, &Geom::of(ctx), &Levels::of(ctx, colon_on), ctx.get("hue"), ctx.get("fill"))
    }
}

// ---------------------------------------------------------------- drawing

/// Where everything is, in LEDs, once `size` has had its say.
struct Geom {
    w: f32,
    h: f32,
    centres: [f32; 4],
    colon: f32,
    axle: f32,
    size: f32,
    /// Half the seam's width, in LEDs.
    seam: f32,
    /// Half a stroke's width, in glyph units.
    half: f32,
    tilt: f32,
}

impl Geom {
    fn of(ctx: &Ctx) -> Geom {
        let size = ctx.get("size");
        let (cx, cy) = (W as f32 * 0.5, H as f32 * 0.5);
        Geom {
            w: MODULE_W * size,
            h: MODULE_H * size,
            centres: CENTRES.map(|c| cx + (c - cx) * size),
            colon: cx,
            axle: cy,
            size,
            // Not scaled by `size`: the seam, the lit edge and the shadow's
            // penumbra are lines on the panel, and a line has to stay at
            // least an LED wide to be a line. The axle sits on a pixel
            // boundary (row 16 of 32), so a seam of 2 is exactly two black
            // rows however large the clock is.
            seam: ctx.get("seam") * 0.5,
            half: ctx.get("weight") * 0.5,
            tilt: ctx.get("tilt"),
        }
    }
}

/// The three levels the picture is made of, in linear light on the hue's ray.
struct Levels {
    numeral: f32,
    edge: f32,
    colon: f32,
}

impl Levels {
    fn of(ctx: &Ctx, colon_on: bool) -> Levels {
        let numeral = srgb8_to_linear(ctx.get("light").round().clamp(0.0, 255.0) as u8);
        Levels {
            numeral,
            edge: (numeral * EDGE_BOOST).min(1.0),
            colon: if colon_on { numeral * COLON_LEVEL } else { 0.0 },
        }
    }

    /// The top of the ramp: everything in the picture is at or below it.
    fn top(&self) -> f32 {
        self.numeral.max(self.edge).max(self.colon)
    }
}

/// Draw the whole clock.
fn picture(poses: &[Pose; 4], g: &Geom, levels: &Levels, hue: f32, fill: f32) -> Frame {
    let tint = ray(hue);
    let top = levels.top();
    // One ramp on the hue's ray, even in OKLCH lightness (which goes as the
    // cube root of light), so a numeral's anti-aliased edge is the numeral
    // dimmed and costs no palette entry of its own - card 115's ramp, one hue
    // instead of two.
    let mut colours = vec![Rgb::BLACK];
    colours.extend((0..STEPS).map(|k| {
        let l = DARK + (1.0 - DARK) * k as f32 / (STEPS - 1) as f32;
        tint.scale(top * l * l * l)
    }));
    let palette = Palette::new(colours, 0.03);

    let mut frame = Frame::supersample(6, |x, y| tint.scale(sample(x, y, poses, g, levels)));
    // The halftone is applied here, in panel pixels, because that is what it
    // is: every other LED off in a fixed ordered pattern, the cheapest way to
    // take light out of the picture without taking size out of it. A pixel it
    // turns off goes to black, so it costs no colours either.
    if fill > 0.0 {
        if let Frame::Linear(px) = &mut frame {
            for (i, c) in px.iter_mut().enumerate() {
                if Dither::Bayer4.threshold(i % W, i / W) + 0.5 < fill {
                    *c = Rgb::BLACK;
                }
            }
        }
    }
    palette.map(&frame, Dither::None, 0.0)
}

/// Light at one point of the panel, as a share of the ray's full brightness.
fn sample(x: f32, y: f32, poses: &[Pose; 4], g: &Geom, levels: &Levels) -> f32 {
    let v = y - g.axle;
    if levels.colon > 0.0 {
        let (dx, dy) = (x - g.colon, v.abs() - COLON_AT * g.size);
        if dx * dx + dy * dy <= (COLON_R * g.size).powi(2) {
            return levels.colon;
        }
    }
    if v.abs() > g.h * 0.5 {
        return 0.0;
    }
    for (pose, cx) in poses.iter().zip(g.centres) {
        let u = x - cx;
        if u.abs() <= g.w * 0.5 {
            return module(u, v, pose, g, levels);
        }
    }
    0.0
}

/// Light at one point inside a module, `(u, v)` from its axle.
fn module(u: f32, v: f32, pose: &Pose, g: &Geom, levels: &Levels) -> f32 {
    let fall = Fall::new(pose.theta, g.tilt, g.h);
    let on = |d: u8, gx: f32, gy: f32| glyphs::distance(d as usize, gx / g.size, gy / g.size) <= g.half;

    // The falling card is in front of everything else, so it is asked first.
    if let Some(r) = fall.along(v) {
        let across = u / fall.widen(r);
        if across.abs() <= g.w * 0.5 {
            if r < g.seam {
                // The card's own root, at the axle: the seam again.
                return 0.0;
            }
            // The lit edge. Measured on the panel rather than on the card,
            // because as the card turns edge-on its whole face collapses into
            // this line and there is nothing else left to see.
            if (v - fall.screen(fall.reach)).abs() <= EDGE_HALF {
                return levels.edge * fall.edge;
            }
            // Front face: the old numeral's top half, carried down. Back
            // face, once the card has passed us: the new numeral's bottom
            // half, arriving.
            let front = fall.squash >= 0.0;
            let (digit, gy) = if front { (pose.old, -r) } else { (pose.new, r) };
            return if on(digit, across, gy) { levels.numeral * fall.shade } else { 0.0 };
        }
    }

    if v.abs() < g.seam {
        return 0.0;
    }
    if v < 0.0 {
        // Above the axle: the next numeral's top half, already standing there.
        return if on(pose.new, u, v) { levels.numeral } else { 0.0 };
    }
    // Below it: the old numeral's bottom half, with the falling card's shadow
    // running down it ahead of the card.
    let lit = if fall.shadow > 0.0 {
        let k = smoothstep(fall.shadow + PENUMBRA, fall.shadow - PENUMBRA, v);
        1.0 - (1.0 - SHADOW) * k
    } else {
        1.0
    };
    if on(pose.old, u, v) {
        levels.numeral * lit
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{GUARANTEED_PALETTE, N};
    use crate::patch::{Clock, Params};
    use crate::snapshot::{take, Shot};

    fn params() -> Params {
        Params::defaults(DEF.params)
    }

    fn at(hhmm: &str) -> Clock {
        Clock::parse(hhmm).unwrap_or_else(|e| panic!("{hhmm}: {e}"))
    }

    /// Render the patch itself (no pipeline), `at` seconds into a run pinned
    /// to `when`, stepped at the snapshot's own 30 fps.
    fn run(when: &str, secs: f64, set: &[(&str, f32)]) -> Frame {
        let mut p = params();
        for (k, v) in set {
            assert!(p.set(DEF.params, k, *v), "no parameter `{k}`");
        }
        let clock = at(when);
        let mut patch = (DEF.make)(1);
        let dt = 1.0 / crate::snapshot::FPS;
        let steps = (secs / dt).round() as usize;
        let mut frame = Frame::black();
        for i in 0..=steps {
            let t = i as f64 * dt;
            frame = patch.render(&Ctx { t, dt, now: clock.now(t), params: &p });
        }
        frame
    }

    /// Which numeral each module is showing, read back off the panel by
    /// matching the lit LEDs against every numeral drawn in the same place.
    /// A stronger check than asking the patch: it is the picture that has to
    /// be right.
    fn reads(frame: &Frame, g: &Geom) -> Option<[usize; 4]> {
        let lit: Vec<bool> = (0..N).map(|i| frame.pixel(i).luma() > 1e-4).collect();
        let mut out = [0; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            // How many LEDs a numeral would light here that are dark.
            let missing = |d: usize| {
                let mut wrong = 0;
                for y in 0..H {
                    for x in 0..W {
                        let (u, v) = (x as f32 + 0.5 - g.centres[i], y as f32 + 0.5 - g.axle);
                        if u.abs() > g.w * 0.5 || v.abs() > g.h * 0.5 {
                            continue;
                        }
                        // Well inside the stroke and clear of the seam, so a
                        // pixel that is dark here is the numeral not being
                        // drawn rather than an anti-aliased edge.
                        if v.abs() >= g.seam + 0.5 && glyphs::distance(d, u / g.size, v / g.size) <= g.half - 0.4 && !lit[y * W + x] {
                            wrong += 1;
                        }
                    }
                }
                wrong
            };
            *slot = (0..10).min_by_key(|d| missing(*d))?;
            if missing(*slot) > 0 {
                // Mid-flip: the module is not showing any whole numeral.
                return None;
            }
        }
        Some(out)
    }

    fn read_back(frame: &Frame, g: &Geom) -> [usize; 4] {
        reads(frame, g).expect("the modules are not showing four numerals")
    }

    fn geom(set: &[(&str, f32)]) -> Geom {
        let mut p = params();
        for (k, v) in set {
            assert!(p.set(DEF.params, k, *v));
        }
        Geom::of(&Ctx { t: 0.0, dt: 0.0, now: 0.0, params: &p })
    }

    /// The clock tells the time: the four modules are the four digits of
    /// `Ctx::now`, 24-hour by default, leading zeros kept.
    #[test]
    fn the_modules_show_the_time_of_day() {
        for (when, want) in [("21:12", [2, 1, 1, 2]), ("00:00", [0, 0, 0, 0]), ("09:05", [0, 9, 0, 5]), ("14:47", [1, 4, 4, 7])] {
            assert_eq!(read_back(&run(when, 3.0, &[]), &geom(&[])), want, "{when}");
        }
        // 12-hour is the same four modules: 13:11 is 01:11, 00:00 is 12:00.
        assert_eq!(digits(13 * 60 + 11, false), (1, 11));
        assert_eq!(digits(0, false), (12, 0));
        assert_eq!(digits(0, true), (0, 0));
        assert_eq!(read_back(&run("13:11", 3.0, &[("hours24", 0.0)]), &geom(&[])), [0, 1, 1, 1]);
    }

    /// A flip starts when the minute turns and is over within `flip` seconds
    /// times the number of numerals it has to pass through. Pinned at
    /// 09:59:59, one second in is the turn of four modules at once - 0 to 1,
    /// 9 to 0, 5 to 0 and 9 to 0 - which is the worst moment the clock has.
    #[test]
    fn a_flip_begins_on_the_minute_and_ends_when_it_should() {
        let mut patch = (DEF.make)(1);
        let p = params();
        let clock = at("09:59:59");
        let dt = 1.0 / crate::snapshot::FPS;
        let fall = f64::from(p.get("flip"));
        let (mut began, mut ended) = (None, None);
        let g = geom(&[]);
        let mut held: Option<Vec<u8>> = None;
        for i in 0..=(6.0 / dt) as usize {
            let t = i as f64 * dt;
            let frame = patch.render(&Ctx { t, dt, now: clock.now(t), params: &p });
            let pixels: Vec<u8> = (0..N).flat_map(|k| frame.pixel(k).to_srgb8()).collect();
            let still = held.get_or_insert_with(|| pixels.clone());
            if began.is_none() {
                // Before the minute turns the clock reads 09:59 and does not
                // move an LED - where a clock that flipped early is caught.
                assert_eq!(reads(&frame, &g), Some([0, 9, 5, 9]), "at t={t} the clock is not holding 09:59");
                if *still != pixels {
                    began = Some(t);
                }
            } else if ended.is_none() && reads(&frame, &g) == Some([1, 0, 0, 0]) {
                ended = Some(t);
            }
        }
        let began = began.expect("the modules never moved");
        let ended = ended.expect("the modules never landed");
        assert!((0.95..1.10).contains(&began), "the flip began at t={began}, not on the minute");
        // 5 -> 0 is the longest cascade here: five cards, then the settle.
        let longest = fall * 5.0 * (1.0 + SETTLE);
        assert!(ended - began <= longest + 3.0 * dt, "the flip took {:.2} s, longer than {longest:.2}", ended - began);
        assert!(ended - began > fall, "nothing fell: {:.2} s", ended - began);
    }

    /// With `cascade` off a module goes straight to its numeral, so every
    /// module lands within one card's fall however far it had to go.
    #[test]
    fn without_cascade_every_module_lands_together() {
        let mut p = params();
        assert!(p.set(DEF.params, "cascade", 0.0));
        let mut patch = (DEF.make)(1);
        let clock = at("09:59:59");
        let dt = 1.0 / crate::snapshot::FPS;
        let fall = f64::from(p.get("flip"));
        let mut settled_at = None;
        let g = geom(&[]);
        for i in 0..=(4.0 / dt) as usize {
            let t = i as f64 * dt;
            let frame = patch.render(&Ctx { t, dt, now: clock.now(t), params: &p });
            if t > 1.05 && reads(&frame, &g) == Some([1, 0, 0, 0]) {
                settled_at = Some(t);
                break;
            }
        }
        let settled = settled_at.expect("the modules never landed");
        assert!(settled - 1.0 <= fall + 2.0 * dt, "one card took {:.2} s", settled - 1.0);
    }

    /// The palette is one ramp plus black - 32 colours - so every frame is
    /// exact on the wire whatever the picture does, settled or mid-flip.
    #[test]
    fn the_palette_is_one_ramp_and_black() {
        let sets: [&[(&str, f32)]; 5] = [&[], &[("fill", 0.5)], &[("size", 0.6)], &[("hue", 40.0)], &[("light", 220.0)]];
        for at_s in [3.0, 1.03, 1.10, 1.17, 1.23, 1.30, 1.40] {
            for set in sets {
                let frame = run("09:59:59", at_s, set);
                let Frame::Indexed { palette, indices } = &frame else { panic!("not an indexed frame") };
                assert_eq!(palette.len(), STEPS + 1, "at t={at_s} {set:?}");
                assert!(palette.len() <= GUARANTEED_PALETTE, "and exact whatever the indices do");
                assert!(indices.iter().all(|i| (*i as usize) < palette.len()));
                assert_eq!(palette[0], Rgb::BLACK, "the card bodies are true black");
            }
        }
    }

    /// At the default hue nothing in the picture lights a green or a blue die.
    /// This is the whole of "pure red": one die a pixel, and nothing for the
    /// dark end to sparkle with.
    #[test]
    fn the_default_hue_is_red_and_nothing_else() {
        assert_eq!(ray(0.0), Rgb::new(1.0, 0.0, 0.0));
        for at_s in [3.0, 1.10, 1.23] {
            let frame = run("09:59:59", at_s, &[]);
            let Frame::Indexed { palette, .. } = &frame else { panic!("not indexed") };
            for c in palette {
                assert_eq!((c.g, c.b), (0.0, 0.0), "{c:?} lights more than the red die");
                assert_eq!(c.to_srgb8()[1..], [0, 0], "{c:?} reaches the panel with green or blue");
            }
        }
    }

    /// The resting picture is a few percent of the panel's light - which is
    /// what "low light" has to mean here, because the way to be dim on this
    /// panel is to light few pixels at a moderate level rather than many at a
    /// very low one (card 102).
    #[test]
    fn the_resting_picture_is_a_few_percent_of_the_panel() {
        let apl = |set: &[(&str, f32)]| {
            let f = run("21:12", 3.0, set);
            (0..N).map(|i| f.pixel(i).duty()).sum::<f32>() / N as f32
        };
        let plain = apl(&[]);
        assert!(plain < 0.03, "the resting picture is {:.1}% of full", plain * 100.0);
        assert!(plain > 0.002, "it went dark: {:.2}%", plain * 100.0);
        // The halftone is the cheap way to halve the light without shrinking
        // anything, which is the one thing it has to actually do.
        let half = apl(&[("fill", 0.5)]);
        assert!((half / plain - 0.5).abs() < 0.12, "halftone 0.5 gave {:.2} of the light", half / plain);
        // And `size` takes light out by taking area out.
        assert!(apl(&[("size", 0.6)]) < plain * 0.6);
    }

    /// Card 162's promise, for this patch: a pinned time draws the same
    /// picture whenever the command is typed, and two pinned times are two
    /// pictures. (`tests/pinned_time.rs` checks it through the whole
    /// pipeline; this is the patch itself, which is where a stray
    /// `SystemTime::now` would be.)
    #[test]
    fn a_pinned_time_is_the_same_picture_every_time() {
        let shot = |when: &str, at: f64| {
            let s = Shot { seed: 5, at, warmup: at, clock: at_clock(when), ..Shot::default() };
            take(&DEF, &params(), &s).preview
        };
        fn at_clock(s: &str) -> Clock {
            Clock::parse(s).expect("a time")
        }
        assert_eq!(shot("21:12", 3.0), shot("21:12", 3.0));
        assert_eq!(shot("09:59:59", 1.2), shot("09:59:59", 1.2), "mid-flip too");
        assert_ne!(shot("21:12", 3.0), shot("10:10", 3.0));
        // And a seed is not one of its inputs: there is no randomness in it.
        let other = Shot { seed: 99, at: 3.0, warmup: 3.0, clock: at_clock("21:12"), ..Shot::default() };
        assert_eq!(shot("21:12", 3.0), take(&DEF, &params(), &other).preview);
    }

    /// The seam is the signature, so it is really there: the row of LEDs on
    /// the axle is black all the way across every module.
    #[test]
    fn the_seam_cuts_every_module() {
        let g = geom(&[]);
        // 08:38 puts a numeral with ink right across the waist (8, 3) in
        // every module, so nothing but the seam can be making the gap.
        let frame = run("08:38", 3.0, &[]);
        for (i, cx) in g.centres.iter().enumerate() {
            for y in [15, 16] {
                let lit = (0..W).filter(|x| (*x as f32 + 0.5 - cx).abs() <= g.w * 0.5 && frame.pixel(y * W + x).luma() > 1e-4).count();
                assert_eq!(lit, 0, "module {i} row {y}: no seam");
            }
            // And the rows either side of it are not dark, or the "seam"
            // would just be a gap in the numerals.
            let around: usize = [13, 14, 17, 18]
                .iter()
                .map(|y| (0..W).filter(|x| (*x as f32 + 0.5 - cx).abs() <= g.w * 0.5 && frame.pixel(y * W + x).luma() > 1e-4).count())
                .sum();
            assert!(around > 4, "module {i} is dark either side of the seam too");
        }
    }

    /// Mid-flip, the module really is showing two different numerals at once -
    /// the next one standing above the axle, the old one still below it - and
    /// a card in between with an edge brighter than either. If any of that
    /// stopped being true the flip would be a wipe.
    #[test]
    fn mid_flip_there_are_two_numerals_and_a_card_between_them() {
        let g = geom(&[]);
        let numeral = srgb8_to_linear(params().get("light") as u8);
        let cx = g.centres[2];
        let inside = |x: usize| (x as f32 + 0.5 - cx).abs() <= g.w * 0.5;
        let (mut both_halves, mut lit_edge, mut all_dark) = (0, 0, 0);
        // From the minute turn to a second after it: the minutes' tens cascade
        // 5, 6, 7, 8, 9, 0.
        for k in 30..=60 {
            let frame = run("09:59:59", f64::from(k) / crate::snapshot::FPS, &[]);
            let rows = |ys: std::ops::Range<usize>| ys.map(|y| (0..W).filter(|x| inside(*x) && frame.pixel(y * W + x).luma() > 1e-4).count()).sum::<usize>();
            let (above, below) = (rows(2..14), rows(18..30));
            if above > 0 && below > 0 {
                both_halves += 1;
            }
            if above == 0 && below == 0 {
                all_dark += 1;
            }
            let brightest = (0..H).flat_map(|y| (0..W).filter(move |x| inside(*x)).map(move |x| (x, y))).map(|(x, y)| frame.pixel(y * W + x).r).fold(0.0_f32, f32::max);
            if brightest > numeral * 1.5 {
                lit_edge += 1;
            }
        }
        assert!(both_halves > 20, "the module rarely shows two numerals at once: {both_halves} of 31 frames");
        assert_eq!(all_dark, 0, "the module went blank for {all_dark} frames");
        assert!(lit_edge > 5, "the falling card's edge is never lit: {lit_edge} of 31 frames");
    }

    /// The four modules, the gaps and the colon fit 64 columns with a margin
    /// and never overlap, at full size and at the smallest.
    #[test]
    fn the_layout_fits_the_panel() {
        for size in [1.0, 0.8, 0.55] {
            let g = geom(&[("size", size)]);
            assert!(g.centres[0] - g.w * 0.5 >= 0.0, "size {size} runs off the left");
            assert!(g.centres[3] + g.w * 0.5 <= W as f32, "size {size} runs off the right");
            assert!(g.axle + g.h * 0.5 <= H as f32 && g.axle - g.h * 0.5 >= 0.0, "size {size} runs off the top or bottom");
            for pair in g.centres.windows(2) {
                assert!(pair[1] - pair[0] >= g.w + PAIR_GAP * size - 1e-4, "size {size}: modules touch");
            }
            // The colon has the middle to itself.
            assert!(g.centres[1] + g.w * 0.5 <= g.colon - COLON_R * size);
            assert!(g.centres[2] - g.w * 0.5 >= g.colon + COLON_R * size);
        }
        assert_eq!(CENTRES, [8.0, 23.0, 41.0, 56.0], "the layout the card was judged on");
    }

    /// Everything a person can move is on the page, with the defaults the card
    /// asked for. Pinned so a change to one of them is a decision.
    #[test]
    fn the_parameters_are_the_ones_the_card_asked_for() {
        let want: &[(&str, f32, f32, f32)] = &[
            ("light", 40.0, 255.0, 120.0),
            ("hue", 0.0, 360.0, 0.0),
            ("size", 0.55, 1.0, 1.0),
            ("weight", 1.2, 3.0, 2.0),
            ("seam", 0.0, 3.0, 2.0),
            ("flip", 0.08, 0.6, 0.2),
            ("cascade", 0.0, 1.0, 1.0),
            ("tilt", 0.0, 40.0, 16.0),
            ("fill", 0.0, 0.6, 0.0),
            ("pace", 5.0, 60.0, 60.0),
            ("blink", 0.0, 1.0, 0.0),
            ("hours24", 0.0, 1.0, 1.0),
            ("offset", 0.0, 1439.0, 0.0),
        ];
        assert_eq!(DEF.params.len(), want.len(), "a parameter came or went");
        for ((id, min, max, default), spec) in want.iter().zip(DEF.params) {
            assert_eq!(spec.id, *id);
            assert_eq!((spec.min, spec.max, spec.default), (*min, *max, *default), "{id}");
        }
    }
}
