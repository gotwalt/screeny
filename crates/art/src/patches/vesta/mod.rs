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
//! where we sit). The numerals are `crates/art/src/faces`, which is a choice
//! of faces - the house one drawn for this size, and real pixel fonts whose
//! cells land 1:1 on the LEDs (card 174).

pub(crate) mod flap;

use crate::color::{oklch, smoothstep, srgb8_to_linear, Rgb};
use crate::dither::Dither;
use crate::faces::{self, Face};
use crate::frame::{Frame, H, W};
use crate::palette::Palette;
use crate::patch::{choice, param, toggle, Ctx, ParamSpec, Patch, PatchDef, Playing};
use flap::Fall;

pub const DEF: PatchDef = PatchDef {
    id: "vesta",
    name: "Vesta",
    blurb: "A split-flap night clock: HH:MM on four flap modules, red on black, the cards falling under gravity as the minute turns. Built for a dark room - few pixels, moderate level, pure red.",
    params: PARAMS,
    // The picture is the time: there is no randomness in it at all, so a new
    // seed would change nothing a person could see (card 151).
    seeded: false,
    make,
};

const PARAMS: &[ParamSpec] = &[
    choice("font", "Numerals", faces::NAMES, faces::DEFAULT),
    param("light", "Numeral level (sRGB code)", 40.0, 255.0, 1.0, 120.0),
    param("hue", "Hue (0 = pure red)", 0.0, 360.0, 1.0, 0.0),
    param("size", "Size", 0.55, 1.0, 0.01, 1.0),
    param("weight", "Stroke weight, the Vesta face only (LEDs)", 1.2, 3.0, 0.1, 2.0),
    param("seam", "Seam (LEDs)", 0.0, 3.0, 0.5, 2.0),
    choice("flips", "On the minute", FLIPS, 0.0),
    param("spin", "A full rotation (seconds)", 0.7, 2.5, 0.05, 1.15),
    param("flip", "A card's fall, and a rotation's landing (seconds)", 0.08, 0.6, 0.01, 0.2),
    param("tilt", "Viewpoint above the board (deg)", 0.0, 40.0, 1.0, 16.0),
    param("fill", "Halftone (0 = solid)", 0.0, 0.6, 0.05, 0.0),
    param("pace", "Seconds per minute (60 = real clock)", 5.0, 60.0, 1.0, 60.0),
    toggle("blink", "Colon blinks", false),
    toggle("hours24", "24-hour", true),
    toggle("zero", "Leading zero (off = a blank card)", false),
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

/// The blank card.
///
/// A real board's hours-tens drum carries a blank where a leading zero would
/// be, and shows it for most of the day (the owner, 2026-09-20: "let's add a
/// blank card for leading zero, it's a thing the real thing does"). It is a
/// card like any other: it falls, it is fallen onto, and the module keeps its
/// seam and its black body while it is up.
const BLANK: u8 = 10;

/// The glyph a card carries. The blank has none, and a face draws nothing at
/// all for a glyph it has not got - which is exactly what a blank card is.
fn glyph(card: u8) -> char {
    char::from_digit(u32::from(card), 10).unwrap_or(' ')
}

/// **One drum, eleven cards, on every module** (card 184).
///
/// The owner, seeing the new faces land: "let's do an entire rotation of every
/// position on minute change. I think the fun of a flipboard is that it
/// flips." A real board's charm is that every module carries the *same* drum
/// and turns it at the same rate, so a refresh is one wave of identical
/// clatter rather than four little animations - and the positions resolve one
/// by one because each has a different distance left to go, not because each
/// runs at its own speed.
///
/// So: the blank first, then the ten numerals. The blank is a card like any
/// other and flashes past every module once a rotation, which is what a real
/// board does; on the hours' tens it is also where a leading zero would be,
/// and `zero` swaps it for the numeral on that module alone.
const DRUM: [u8; 11] = [BLANK, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
const DRUM_ZERO: [u8; 11] = [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

/// The drum a module turns. Only the hours' tens differs, and only in what
/// its first card carries.
fn drum(module: usize, zero: bool) -> &'static [u8] {
    if module == 0 && zero {
        &DRUM_ZERO
    } else {
        &DRUM
    }
}

/// What the minute does to every module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flips {
    /// The whole drum, everywhere: card 184's default.
    Rotation,
    /// Every card between the old numeral and the new one (`cascade` on).
    Between,
    /// One card, on the modules whose numeral changed (`cascade` off).
    Changed,
}

impl Flips {
    fn of(v: f32) -> Flips {
        match v.round() as i32 {
            1 => Flips::Between,
            2 => Flips::Changed,
            _ => Flips::Rotation,
        }
    }
}

const FLIPS: &[&str] = &["Full rotation", "Through the numerals between", "Changed cards only"];

/// A fast card's fall, in seconds. Three frames at 30 fps is the floor at
/// which a fall still reads as a fall rather than as a flicker, and a
/// rotation is eleven-plus cards: at the slow `flip` it would be two seconds
/// of slow motion every minute.
const AIR: f64 = 0.1;

/// How many cards at the end of a rotation ease back to the slow, readable
/// `flip`. The landing is the part worth watching - it is the one that says
/// what time it is - so the drum arrives at a walk instead of stopping dead.
const EASE: usize = 3;

/// Each module is its own mechanism: it lets go a moment after the one to its
/// left and its drum turns a per cent or two differently. Fixed, not random -
/// these are machined parts, and the same board does the same thing every
/// minute. Left to right, so the wave runs the way the time is read and the
/// minutes' units, the one numeral that changes every minute, is the last to
/// settle.
const SKEW: [f64; 4] = [0.0, 0.035, 0.075, 0.11];
const RATE: [f64; 4] = [1.0, 1.02, 1.05, 1.03];

/// How much of its lit edge a card keeps at the fastest it falls.
///
/// This is a night clock and the lit edge is the brightest thing the patch
/// draws (card 155). A rotation puts two or three cards in the air on four
/// modules at once, and at full `EDGE_BOOST` that is a flash. A fast card's
/// edge is also physically a thinner, shorter-lived line, so damping it with
/// speed is not a cheat - and it leaves the eased landing at full brightness,
/// which is where the eye should be anyway.
const EDGE_DAMP: f32 = 0.45;

/// One card of a run: what it brings down, when it is let go, how long it
/// falls.
#[derive(Clone, Copy, Debug)]
struct Card {
    /// The numeral on its back - the bottom half it brings down, and the top
    /// half that stands behind it afterwards.
    to: u8,
    /// Engine time it is released, and its fall in seconds.
    at: f64,
    fall: f64,
}

/// One flap module: the card it started from, and every card of the run it is
/// part way through.
///
/// A run is planned once, when the minute turns, and then only read. That is
/// what lets several cards be in the air at once - the old model advanced one
/// card and asked for the next when it landed, which can never overlap - and
/// it is what keeps a pinned time exactly reproducible: the picture at `t` is
/// a function of the plan, not of how many frames have been drawn.
#[derive(Clone, Debug, Default)]
struct Module {
    /// The card that was showing when this run began.
    from: u8,
    /// Every card of the run, in release order.
    run: Vec<Card>,
}

/// How a run is timed.
#[derive(Clone, Copy, Debug)]
struct Timing {
    /// Seconds between one card being let go and the next.
    period: f64,
    /// A card's fall while the drum is spinning.
    air: f64,
    /// A card's fall when it is the last of a run, and in the two slow modes.
    flip: f64,
    /// How many cards at the end ease back to `flip`.
    ease: usize,
}

impl Timing {
    /// `spin` is what a **full revolution** takes, first card let go to last
    /// card settled, so the parameter means what it says whatever a
    /// particular module has to turn. The extra cards a distant target needs
    /// are extra time, exactly as they are on a real board.
    fn rotation(spin: f64, flip: f64) -> Timing {
        let air = AIR.min(flip);
        let n = DRUM.len() as f64;
        // total(k) = (k - 1) * period + (flip - air) + flip * (1 + SETTLE)
        let fixed = (flip - air) + flip * (1.0 + SETTLE);
        Timing { period: ((spin - fixed) / (n - 1.0)).max(1.0 / 60.0), air, flip, ease: EASE }
    }

    /// The old behaviour: one card at a time, the next let go as the last
    /// lands, no overlap and no easing.
    fn slow(flip: f64) -> Timing {
        Timing { period: flip, air: flip, flip, ease: 0 }
    }

    /// Card `j` of `k`: the last `ease` of them stretch back to `flip`.
    fn fall_of(&self, j: usize, k: usize) -> f64 {
        let left = k - 1 - j;
        if self.ease == 0 || left >= self.ease {
            return self.air;
        }
        let e = (self.ease - left) as f64 / self.ease as f64;
        self.air + (self.flip - self.air) * e
    }

    /// Scaled for one module, so the four are never in lock-step.
    fn at_rate(&self, rate: f64) -> Timing {
        Timing { period: self.period * rate, air: self.air * rate, flip: self.flip * rate, ease: self.ease }
    }
}

/// The cards a change asks for, as numerals in release order.
///
/// A drum only ever advances one card, so "a full rotation ending on the new
/// numeral" is eleven cards **plus** the distance to the target - eleven to
/// twenty-one of them. There is no way to give every module the same card
/// count without letting one of them skip, and skipping is the one thing a
/// flap cannot do; a shared rate with different distances is exactly what
/// makes a real board resolve position by position.
fn route(from: u8, target: u8, drum: &[u8], flips: Flips) -> Vec<u8> {
    let at = |c: u8| drum.iter().position(|d| *d == c);
    let (i, t) = match (at(from), at(target)) {
        (Some(i), Some(t)) if flips != Flips::Changed => (i, t),
        // One card, straight there - which is also what a module showing a
        // card its drum has not got must do, the instant after `zero` moves.
        _ => return if from == target { Vec::new() } else { vec![target] },
    };
    let n = drum.len();
    let d = (t + n - i) % n;
    let k = if flips == Flips::Rotation { n + d } else { d };
    (1..=k).map(|j| drum[(i + j) % n]).collect()
}

impl Module {
    /// Plan a run from what is showing at `t` to `target`.
    ///
    /// Cards already let go are kept - a card in the air cannot be recalled -
    /// and the rest is replanned from the last of them. That only comes up
    /// when the minute turns mid-rotation, which needs a `pace` of a few
    /// seconds a minute.
    fn plan(&mut self, t: f64, target: u8, drum: &[u8], flips: Flips, tm: &Timing, skew: f64) {
        let gone: Vec<Card> = self.run.iter().copied().filter(|c| c.at <= t).collect();
        let (from, mut at) = match gone.last() {
            // A period after the last card that was let go, not when it lands:
            // the cards of a rotation overlap.
            Some(c) => (c.to, (c.at + tm.period + (c.fall - tm.air)).max(t)),
            None => (self.from, t + skew),
        };
        let route = route(from, target, drum, flips);
        let k = route.len();
        let mut run = gone;
        for (j, to) in route.into_iter().enumerate() {
            let fall = tm.fall_of(j, k);
            run.push(Card { to, at, fall });
            at += tm.period + (fall - tm.air);
        }
        self.from = from;
        self.run = run;
    }

    /// Settle the module on one card, showing it and nothing else.
    fn still(&mut self, card: u8) {
        self.from = card;
        self.run.clear();
    }

    /// The card on the plate at `t`: the last one to have landed.
    fn shown(&self, t: f64) -> u8 {
        self.run.iter().rev().find(|c| t >= c.at + c.fall).map_or(self.from, |c| c.to)
    }

    /// Where the run ends: the last card landed and its settle over.
    fn ends(&self) -> f64 {
        self.run.last().map_or(f64::MIN, |c| c.at + c.fall * (1.0 + SETTLE))
    }

    /// Cards let go and not yet landed.
    fn flying(&self, t: f64) -> usize {
        self.run.iter().filter(|c| c.at <= t && t < c.at + c.fall).count()
    }

    /// Everything the module is showing at `t`.
    fn pose(&self, t: f64, g: &Geom) -> Pose {
        let mut pose = Pose { plate: self.from, standing: self.from, air: [Flying::default(); MAX_AIR], n: 0 };
        let mut under = self.from;
        let mut settling: Option<(u8, f32, f64)> = None;
        for c in &self.run {
            if t < c.at {
                break;
            }
            pose.standing = c.to;
            if t < c.at + c.fall {
                let u = ((t - c.at) / c.fall) as f32;
                pose.push(under, c.to, flap::angle(u), c.fall, g);
            } else {
                pose.plate = c.to;
                let tau = ((t - c.at - c.fall) / (c.fall * SETTLE)) as f32;
                settling = (tau < 1.0).then_some((c.to, tau, c.fall));
            }
            under = c.to;
        }
        // The card that has just landed is lying on the stack below, in front
        // of anything still in the air and nearer the eye, so it goes first.
        if let Some((card, tau, fall)) = settling {
            pose.unshift(card, card, 180.0 - flap::settle(tau, SETTLE_DEG), fall, g);
        }
        pose
    }
}

/// At most this many cards of one module are drawn at once. Two is what the
/// default timing puts in the air and three is what the eased landing can
/// reach; the rest is headroom for a `spin` wound right down.
const MAX_AIR: usize = 6;

/// One card off the stack.
#[derive(Clone, Copy, Debug, Default)]
struct Flying {
    /// The top half it carries down on its front, and the bottom half it
    /// brings on its back.
    front: u8,
    back: u8,
    /// Where it is, and how much of its lit edge it keeps.
    fall: Fall,
    damp: f32,
}

/// Everything a module is showing at one instant.
#[derive(Clone, Copy, Debug)]
struct Pose {
    /// The bottom plate: the last card to have landed.
    plate: u8,
    /// Standing above the axle, behind everything in the air.
    standing: u8,
    /// Cards off the stack, front first. A card let go earlier is further
    /// round its swing and nearer the eye, and on the standing stack it was
    /// in front of the one behind it, so release order is depth order.
    air: [Flying; MAX_AIR],
    n: usize,
}

impl Pose {
    fn flying(&self) -> &[Flying] {
        &self.air[..self.n]
    }

    fn make(front: u8, back: u8, theta: f32, fall: f64, g: &Geom) -> Flying {
        let speed = if g.flip > 0.0 { (fall / g.flip).clamp(0.0, 1.0) as f32 } else { 1.0 };
        Flying { front, back, fall: Fall::new(theta, g.tilt, g.h), damp: EDGE_DAMP + (1.0 - EDGE_DAMP) * speed }
    }

    fn push(&mut self, front: u8, back: u8, theta: f32, fall: f64, g: &Geom) {
        if self.n < MAX_AIR {
            self.air[self.n] = Pose::make(front, back, theta, fall, g);
            self.n += 1;
        }
    }

    fn unshift(&mut self, front: u8, back: u8, theta: f32, fall: f64, g: &Geom) {
        let n = self.n.min(MAX_AIR - 1);
        self.air.copy_within(..n, 1);
        self.air[0] = Pose::make(front, back, theta, fall, g);
        self.n = n + 1;
    }
}


struct Vesta {
    /// The time of day on the first frame; a sped-up clock (`pace` < 60) runs
    /// on from here, exactly as the other clock patches do.
    born: Option<f64>,
    modules: [Module; 4],
    /// The cards the runs in hand were planned for, and the minute they were
    /// planned for. A run is replanned when the minute turns - **on every
    /// module, including the ones whose card did not change**, which is the
    /// whole of card 184 - or when a parameter moves a module's target
    /// between minutes.
    want: [u8; 4],
    minute: Option<i64>,
    /// What the four modules were showing on the last frame, for `playing`.
    face: [u8; 4],
    /// For the studio's "now playing".
    doing: String,
}

fn make(_seed: u64) -> Box<dyn Patch> {
    Box::new(Vesta { born: None, modules: std::array::from_fn(|_| Module::default()), want: [0; 4], minute: None, face: [0; 4], doing: String::new() })
}

/// The hours and minutes a minute-of-day shows.
fn digits(minute: i64, hours24: bool) -> (u32, u32) {
    let (h, m) = ((minute.div_euclid(60).rem_euclid(24)) as u32, minute.rem_euclid(60) as u32);
    (if hours24 { h } else { (h + 11) % 12 + 1 }, m)
}

/// The four cards a minute-of-day asks for.
///
/// Every position has a card - that is what a flap board is - but the hours'
/// tens carries a *blank* one where the leading zero would be, so `09:05` is
/// ` 9:05` and `00:00` is `12:00` on the 12-hour setting and ` 0:00` on the
/// 24-hour one. `zero` puts the numeral back.
fn cards(minute: i64, hours24: bool, zero: bool) -> [u8; 4] {
    let (h, m) = digits(minute, hours24);
    let tens = (h / 10) as u8;
    [if tens == 0 && !zero { BLANK } else { tens }, (h % 10) as u8, (m / 10) as u8, (m % 10) as u8]
}

impl Patch for Vesta {
    fn playing(&self) -> Option<Playing> {
        let card = |d: u8| if d == BLANK { " ".to_string() } else { d.to_string() };
        let face: String = self.face.iter().enumerate().map(|(i, c)| if i == 2 { format!(":{}", card(*c)) } else { card(*c) }).collect();
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
        let zero = ctx.get("zero") >= 0.5;
        let target = cards(minute, ctx.get("hours24") >= 0.5, zero);

        let flips = Flips::of(ctx.get("flips"));
        let flip = f64::from(ctx.get("flip"));
        // The stagger is the rotation's. The two older modes are left exactly
        // as card 155 drew them - one card at a time, all four modules in
        // step - so a person who picks one of them gets what they remember.
        let spinning = flips == Flips::Rotation;
        let base = if spinning { Timing::rotation(f64::from(ctx.get("spin")), flip) } else { Timing::slow(flip) };
        // **A run is planned once** - when the minute turns - and only read
        // after that. Replanning every frame would re-time the tail of a
        // rotation against the clock each time and the drum would never stop
        // turning; it is also what lets a plan hold several cards in the air
        // at once, which the old one-card-at-a-time model could not express.
        //
        // The trigger is **the minute**, not the module's own card: card 184
        // asks every position to rotate, "the ones whose numeral did not
        // change too". In the two older modes a module whose card did not
        // change plans an empty run, which is the same as standing still, so
        // one trigger serves all three.
        let turned = self.minute.is_some_and(|was| was != minute);
        self.minute = Some(minute);
        for (i, (m, want)) in self.modules.iter_mut().zip(target).enumerate() {
            if born_now {
                // Born reading the time, not flipping its way up to it.
                m.still(want);
            } else if turned || self.want[i] != want {
                let (tm, skew) = if spinning { (base.at_rate(RATE[i]), SKEW[i]) } else { (base, 0.0) };
                m.plan(ctx.t, want, drum(i, zero), flips, &tm, skew);
            } else if !m.run.is_empty() && ctx.t >= m.ends() {
                // Retire a finished run so the plan does not grow all night.
                m.still(want);
            }
        }
        self.want = target;
        self.face = std::array::from_fn(|i| self.modules[i].shown(ctx.t));
        let moving = self.modules.iter().filter(|m| m.flying(ctx.t) > 0).count();
        self.doing = match moving {
            0 => "settled".to_string(),
            n => format!("{n} of 4 flipping"),
        };

        let g = Geom::of(ctx);
        let poses: [Pose; 4] = std::array::from_fn(|i| self.modules[i].pose(ctx.t, &g));
        let colon_on = ctx.get("blink") < 0.5 || clock.rem_euclid(1.0) < 0.55;
        picture(&poses, &g, &Levels::of(ctx, colon_on), ctx.get("hue"), ctx.get("fill"))
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
    /// A stroke's width, in LEDs. The stroked face only.
    weight: f32,
    tilt: f32,
    /// A card's slow fall, in seconds: what the lit edge is damped against.
    flip: f64,
    /// The numerals' face.
    face: &'static Face,
}

impl Geom {
    fn of(ctx: &Ctx) -> Geom {
        let size = ctx.get("size");
        let (cx, cy) = (W as f32 * 0.5, H as f32 * 0.5);
        Geom {
            face: faces::face(ctx.get("font")),
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
            weight: ctx.get("weight"),
            tilt: ctx.get("tilt"),
            flip: f64::from(ctx.get("flip")),
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
    let on = |d: u8, gx: f32, gy: f32| g.face.ink(glyph(d), gx / g.size, gy / g.size, g.weight);

    // The cards off the stack are in front of everything else, so they are
    // asked first, front to back. Once one of them covers the point the
    // answer is its own, even when the answer is black: a card occludes.
    for f in pose.flying() {
        let fall = f.fall;
        let Some(r) = fall.along(v) else { continue };
        let across = u / fall.widen(r);
        if across.abs() > g.w * 0.5 {
            continue;
        }
        if r < g.seam {
            // The card's own root, at the axle: the seam again.
            return 0.0;
        }
        // The lit edge. Measured on the panel rather than on the card,
        // because as the card turns edge-on its whole face collapses into
        // this line and there is nothing else left to see. `damp` is how
        // much of it a card falling this fast keeps.
        if (v - fall.screen(fall.reach)).abs() <= EDGE_HALF {
            return levels.edge * fall.edge * f.damp;
        }
        // Front face: the numeral it is carrying down. Back face, once the
        // card has passed us: the one it is bringing.
        let front = fall.squash >= 0.0;
        let (digit, gy) = if front { (f.front, -r) } else { (f.back, r) };
        return if on(digit, across, gy) { levels.numeral * fall.shade } else { 0.0 };
    }

    if v.abs() < g.seam {
        return 0.0;
    }
    if v < 0.0 {
        // Above the axle: the top half of whatever will be showing when the
        // cards in the air have all landed, already standing there.
        return if on(pose.standing, u, v) { levels.numeral } else { 0.0 };
    }
    // Below it: the plate's bottom half, with the frontmost card's shadow
    // running down it ahead of the card.
    let shadow = pose.flying().first().map_or(0.0, |f| f.fall.shadow);
    let lit = if shadow > 0.0 {
        let k = smoothstep(shadow + PENUMBRA, shadow - PENUMBRA, v);
        1.0 - (1.0 - SHADOW) * k
    } else {
        1.0
    };
    if on(pose.plate, u, v) {
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

    /// Which card each module is showing, read back off the panel by matching
    /// the lit LEDs against every numeral drawn in the same place. A stronger
    /// check than asking the patch: it is the picture that has to be right.
    ///
    /// A module with nothing lit in it at all is the blank card, which is the
    /// only way a blank can be told from a numeral: it has no ink of its own.
    fn reads(frame: &Frame, g: &Geom) -> Option<[usize; 4]> {
        let lit: Vec<bool> = (0..N).map(|i| frame.pixel(i).luma() > 1e-4).collect();
        let mut out = [0; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            let inside = |x: usize, y: usize| {
                let (u, v) = (x as f32 + 0.5 - g.centres[i], y as f32 + 0.5 - g.axle);
                (u.abs() <= g.w * 0.5 && v.abs() <= g.h * 0.5).then_some((u, v))
            };
            if !(0..H).any(|y| (0..W).any(|x| inside(x, y).is_some() && lit[y * W + x])) {
                *slot = BLANK as usize;
                continue;
            }
            // How many LEDs a numeral would light here that are dark.
            let missing = |d: usize| {
                let mut wrong = 0;
                for y in 0..H {
                    for x in 0..W {
                        let Some((u, v)) = inside(x, y) else { continue };
                        // Well inside the ink and clear of the seam, so a
                        // pixel that is dark here is the numeral not being
                        // drawn rather than an anti-aliased edge.
                        if v.abs() >= g.seam + 0.5 && g.face.core(glyph(d as u8), u / g.size, v / g.size, g.weight) && !lit[y * W + x] {
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
        reads(frame, g).expect("the modules are not showing four cards")
    }

    const B: usize = BLANK as usize;

    fn geom(set: &[(&str, f32)]) -> Geom {
        let mut p = params();
        for (k, v) in set {
            assert!(p.set(DEF.params, k, *v));
        }
        Geom::of(&Ctx { t: 0.0, dt: 0.0, now: 0.0, params: &p })
    }

    /// The clock tells the time: the four modules are the four cards of
    /// `Ctx::now`, 24-hour by default, the hours' tens blank where a leading
    /// zero would be.
    #[test]
    fn the_modules_show_the_time_of_day() {
        for (when, want) in [("21:12", [2, 1, 1, 2]), ("00:00", [B, 0, 0, 0]), ("09:05", [B, 9, 0, 5]), ("14:47", [1, 4, 4, 7])] {
            assert_eq!(read_back(&run(when, 3.0, &[]), &geom(&[])), want, "{when}");
        }
        // And `zero` puts the numeral back on that one module.
        assert_eq!(read_back(&run("09:05", 3.0, &[("zero", 1.0)]), &geom(&[("zero", 1.0)])), [0, 9, 0, 5]);
        // 12-hour is the same four modules: 13:11 is 1:11, 00:00 is 12:00.
        assert_eq!(digits(13 * 60 + 11, false), (1, 11));
        assert_eq!(digits(0, false), (12, 0));
        assert_eq!(digits(0, true), (0, 0));
        assert_eq!(cards(0, false, false), [1, 2, 0, 0], "noon and midnight are 12, not a blank");
        assert_eq!(cards(0, true, false), [BLANK, 0, 0, 0]);
        assert_eq!(read_back(&run("13:11", 3.0, &[("hours24", 0.0)]), &geom(&[])), [B, 1, 1, 1]);
        assert_eq!(read_back(&run("12:34", 3.0, &[("hours24", 0.0)]), &geom(&[])), [1, 2, 3, 4]);
    }

    /// The blank is a card on the drum: it is turned past, landed on and
    /// fallen away from like any other, rather than being switched on and
    /// off. Every module carries it, so it flashes past all four once a
    /// rotation - which is what a real board does.
    #[test]
    fn the_blank_card_flips_like_any_other() {
        let g = geom(&[]);
        assert_eq!(drum(0, false), DRUM, "one drum on every module");
        assert_eq!(drum(3, false), DRUM);
        assert_eq!(DRUM[0], BLANK, "the blank is the drum's first card");
        assert_eq!(drum(0, true)[0], 0, "`zero` swaps it on the hours' tens alone");
        assert_eq!(drum(3, true), DRUM, "and on that module alone");
        // 09:59 -> 10:00 is the blank turning away and 1 arriving; 23:59 ->
        // 00:00 is 2 turning away and the blank arriving. Both are a whole
        // rotation now, so they are read before and well after it.
        assert_eq!(read_back(&run("09:59:59", 0.5, &[]), &g)[0], B);
        assert_eq!(read_back(&run("09:59:59", 3.0, &[]), &g)[0], 1);
        assert_eq!(read_back(&run("23:59:59", 0.5, &[]), &g)[0], 2);
        assert_eq!(read_back(&run("23:59:59", 3.0, &[]), &g)[0], B);
        // Every module turns *through* the blank on the way, and the one it
        // lands on is the target, blank or not.
        let tm = Timing::rotation(1.15, 0.2);
        for (from, target) in [(BLANK, 1), (2, BLANK), (9, 0), (4, 4)] {
            let mut m = Module::default();
            m.still(from);
            m.plan(0.0, target, &DRUM, Flips::Rotation, &tm, 0.0);
            let turned: Vec<u8> = m.run.iter().map(|c| c.to).collect();
            assert_eq!(*turned.last().unwrap(), target, "{from} -> {target} landed elsewhere");
            for card in DRUM {
                assert!(turned.contains(&card), "{from} -> {target} never turned past {card}");
            }
        }
        // With "changed cards only" it is still one card, straight there.
        let one = Timing::slow(0.2);
        let mut m = Module::default();
        m.still(2);
        m.plan(0.0, BLANK, &DRUM, Flips::Changed, &one, 0.0);
        assert_eq!(m.run.len(), 1);
        assert_eq!(m.run[0].to, BLANK);
    }

    /// Every minute of a day, in every mode: the run a module plans ends on
    /// the card the time asks for, and a rotation turns past every card of
    /// the drum at least once on the way. This is the whole correctness of
    /// card 184 in one test - the animation may be wrong, but the clock
    /// cannot be.
    #[test]
    fn every_module_lands_on_the_right_card_for_every_minute_of_a_day() {
        let modes = [(Flips::Rotation, Timing::rotation(1.15, 0.2)), (Flips::Between, Timing::slow(0.2)), (Flips::Changed, Timing::slow(0.2))];
        for (flips, tm) in modes {
            for zero in [false, true] {
                for hours24 in [true, false] {
                    let mut m: [Module; 4] = std::array::from_fn(|_| Module::default());
                    for (i, c) in cards(0, hours24, zero).into_iter().enumerate() {
                        m[i].still(c);
                    }
                    for minute in 1..=1440 {
                        let want = cards(minute, hours24, zero);
                        for (i, slot) in m.iter_mut().enumerate() {
                            let was = slot.shown(0.0);
                            slot.plan(0.0, want[i], drum(i, zero), flips, &tm.at_rate(RATE[i]), SKEW[i]);
                            let end = slot.ends().max(0.0) + 1.0;
                            assert_eq!(slot.shown(end), want[i], "{flips:?} zero={zero} h24={hours24} minute {minute} module {i}");
                            if flips == Flips::Rotation {
                                let turned: Vec<u8> = slot.run.iter().map(|c| c.to).collect();
                                for card in drum(i, zero) {
                                    assert!(turned.contains(card), "{flips:?} minute {minute} module {i}: {was} -> {} skipped {card}", want[i]);
                                }
                            }
                            // Retire the run so the next minute plans from a
                            // settled module, as the renderer does.
                            slot.still(want[i]);
                        }
                    }
                }
            }
        }
    }

    /// **The point of card 184.** On an ordinary minute only one numeral
    /// changes, and all four modules rotate anyway - the owner asked for "an
    /// entire rotation of every position on minute change". A module whose
    /// card did not change turns its whole drum and comes back to it.
    #[test]
    fn every_position_rotates_even_when_its_card_does_not_change() {
        let g = geom(&[]);
        // 21:12 -> 21:13: only the minutes' units is a different card.
        let film = film("21:12:59", 2.6, &[]);
        let still = film[0].2.clone();
        let moved = |i: usize| {
            film.iter().any(|(_, _, px)| {
                (0..H).any(|y| {
                    (0..W).any(|x| {
                        let u = x as f32 + 0.5 - g.centres[i];
                        u.abs() <= g.w * 0.5 && px[(y * W + x) * 3..][..3] != still[(y * W + x) * 3..][..3]
                    })
                })
            })
        };
        for i in 0..4 {
            assert!(moved(i), "module {i} never moved on an ordinary minute");
        }
        // And every one of them is back on the time at the end.
        assert_eq!(reads(&film.last().unwrap().1, &g), Some([2, 1, 1, 3]));
        // A module whose card does not change turns a whole revolution: as
        // many cards as the drum has, ending where it started.
        let tm = Timing::rotation(1.15, 0.2);
        let mut m = Module::default();
        m.still(1);
        m.plan(0.0, 1, &DRUM, Flips::Rotation, &tm, 0.0);
        assert_eq!(m.run.len(), DRUM.len(), "a standing module did not turn one whole revolution");
        assert_eq!(m.run.last().unwrap().to, 1);
        // The two older modes leave it alone, as they always did.
        for flips in [Flips::Between, Flips::Changed] {
            let mut m = Module::default();
            m.still(1);
            m.plan(0.0, 1, &DRUM, flips, &Timing::slow(0.2), 0.0);
            assert!(m.run.is_empty(), "{flips:?} moved a module whose card did not change");
        }
    }

    /// The four modules are never in lock-step: they let go a few tens of
    /// milliseconds apart and they stop at different moments, because each
    /// has a different distance left to turn. That is the wave, and it is
    /// most of the charm.
    #[test]
    fn the_four_modules_are_not_in_step() {
        let tm = Timing::rotation(1.15, 0.2);
        // 09:59 -> 10:00 moves all four: blank->1, 9->0, 5->0, 9->0.
        let from = cards(9 * 60 + 59, true, false);
        let want = cards(10 * 60, true, false);
        let mut starts = Vec::new();
        let mut stops = Vec::new();
        for i in 0..4 {
            let mut m = Module::default();
            m.still(from[i]);
            m.plan(0.0, want[i], drum(i, false), Flips::Rotation, &tm.at_rate(RATE[i]), SKEW[i]);
            starts.push(m.run[0].at);
            stops.push(m.ends());
        }
        for pair in starts.windows(2) {
            assert!(pair[1] - pair[0] >= 0.02, "two modules let go together: {starts:?}");
        }
        for (a, b) in stops.iter().enumerate().flat_map(|(i, a)| stops.iter().skip(i + 1).map(move |b| (*a, *b))) {
            assert!((a - b).abs() >= 0.02, "two modules stopped together: {stops:?}");
        }
        // And the whole thing is over inside the time the card asked for.
        let last = stops.iter().fold(f64::MIN, |a, b| a.max(*b));
        assert!((1.2..=1.9).contains(&last), "the rotation took {last:.2} s");
    }

    /// A setting written before card 184 carries `cascade` and no `flips`.
    /// Card 151 drops an unknown parameter quietly; what matters is that what
    /// is left is sensible, which here means the new default.
    #[test]
    fn a_setting_that_still_says_cascade_loads() {
        let mut p = params();
        assert!(!p.set(DEF.params, "cascade", 0.0), "`cascade` is gone, and is refused rather than stored");
        assert_eq!(p.get("flips"), 0.0, "and the patch is left on the new default");
        assert_eq!(Flips::of(p.get("flips")), Flips::Rotation);
        // The three stops are the three behaviours, in the order the page
        // lists them.
        assert_eq!(FLIPS.len(), 3);
        assert_eq!(Flips::of(0.0), Flips::Rotation);
        assert_eq!(Flips::of(1.0), Flips::Between);
        assert_eq!(Flips::of(2.0), Flips::Changed);
        assert_eq!(Flips::of(-5.0), Flips::Rotation, "out of range is the default, not a panic");
        assert_eq!(Flips::of(99.0), Flips::Rotation);
    }

    /// The picture at every 30 fps frame of a run pinned to `when`, as
    /// srgb8, with the rendered frames beside them.
    fn film(when: &str, secs: f64, set: &[(&str, f32)]) -> Vec<(f64, Frame, Vec<u8>)> {
        let mut p = params();
        for (k, v) in set {
            assert!(p.set(DEF.params, k, *v), "no parameter `{k}`");
        }
        let mut patch = (DEF.make)(1);
        let clock = at(when);
        let dt = 1.0 / crate::snapshot::FPS;
        (0..=(secs / dt) as usize)
            .map(|i| {
                let t = i as f64 * dt;
                let frame = patch.render(&Ctx { t, dt, now: clock.now(t), params: &p });
                let px: Vec<u8> = (0..N).flat_map(|k| frame.pixel(k).to_srgb8()).collect();
                (t, frame, px)
            })
            .collect()
    }

    /// Nothing moves before the minute; the rotation starts on it and is over
    /// inside the time `spin` promises. Pinned at 09:59:59, one second in is
    /// the turn of four modules at once - blank to 1, 9 to 0, 5 to 0 and 9 to
    /// 0 - which is the worst moment the clock has and the longest rotation
    /// it draws.
    #[test]
    fn a_rotation_begins_on_the_minute_and_ends_inside_its_time() {
        let g = geom(&[]);
        let film = film("09:59:59", 3.0, &[]);
        let still = film[0].2.clone();
        let began = film.iter().find(|(_, _, px)| *px != still).map(|(t, _, _)| *t).expect("the modules never moved");
        assert!((0.95..1.10).contains(&began), "the rotation began at t={began}, not on the minute");
        // Every frame up to the last unchanged one reads 9:59 exactly.
        for (t, frame, px) in &film {
            if *px != still {
                break;
            }
            assert_eq!(reads(frame, &g), Some([B, 9, 5, 9]), "at t={t} the clock is not holding 9:59");
        }
        // The drum turns *past* the new time on its way round, so "landed"
        // is the first frame that reads it and never moves again.
        let last_move = film.windows(2).rev().find(|w| w[0].2 != w[1].2).map(|w| w[1].0).expect("the modules never moved");
        let ended = film
            .iter()
            .find(|(t, frame, _)| *t >= last_move && reads(frame, &g) == Some([1, 0, 0, 0]))
            .map(|(t, _, _)| *t)
            .expect("the modules never settled on 10:00");
        for (t, frame, _) in film.iter().filter(|(t, _, _)| *t >= ended) {
            assert_eq!(reads(frame, &g), Some([1, 0, 0, 0]), "at t={t} the board moved again after landing");
        }
        // 09:59 -> 10:00 is the worst rotation the clock draws: the minutes'
        // tens goes 5 -> 0 the long way round, seventeen cards.
        assert!((1.2..=1.9).contains(&(ended - began)), "the rotation took {:.2} s", ended - began);
        // It is a rotation, not a flip: half a second in, the clock does not
        // yet read the new time anywhere near everywhere.
        let half = film.iter().find(|(t, _, _)| *t >= began + 0.5).expect("a frame half a second in");
        assert_ne!(reads(&half.1, &g), Some([1, 0, 0, 0]), "the whole board landed within half a second");
    }

    /// "Changed cards only" is the old `cascade`-off behaviour: a module goes
    /// straight to its numeral, so every module lands within one card's fall
    /// however far it had to go, and the ones that did not change never move.
    #[test]
    fn changed_cards_only_lands_every_module_together() {
        let g = geom(&[]);
        let fall = f64::from(params().get("flip"));
        let dt = 1.0 / crate::snapshot::FPS;
        let settled = film("09:59:59", 4.0, &[("flips", 2.0)])
            .into_iter()
            .find(|(t, frame, _)| *t > 1.05 && reads(frame, &g) == Some([1, 0, 0, 0]))
            .map(|(t, _, _)| t)
            .expect("the modules never landed");
        assert!(settled - 1.0 <= fall + 2.0 * dt, "one card took {:.2} s", settled - 1.0);
        // 21:12 -> 21:13 moves one module and leaves three alone.
        let film = film("21:12:59", 2.0, &[("flips", 2.0)]);
        let cx = geom(&[]).centres;
        let untouched = |px: &[u8], q: &[u8], i: usize| {
            (0..H).all(|y| {
                (0..W).all(|x| {
                    let u = x as f32 + 0.5 - cx[i];
                    u.abs() > g.w * 0.5 || px[(y * W + x) * 3..][..3] == q[(y * W + x) * 3..][..3]
                })
            })
        };
        let first = film[0].2.clone();
        for (t, _, px) in &film {
            for i in 0..3 {
                assert!(untouched(px, &first, i), "at t={t} module {i} moved and its card did not change");
            }
        }
    }

    /// The palette is one ramp plus black - 32 colours - so every frame is
    /// exact on the wire whatever the picture does, settled or mid-flip.
    #[test]
    fn the_palette_is_one_ramp_and_black() {
        let sets: [&[(&str, f32)]; 6] = [&[], &[("fill", 0.5)], &[("size", 0.6)], &[("hue", 40.0)], &[("light", 220.0)], &[("spin", 0.7)]];
        // Settled, and then right through a rotation - a dozen cards in the
        // air across the four modules, which is the busiest the picture gets.
        for at_s in [3.0, 1.03, 1.10, 1.17, 1.23, 1.30, 1.40, 1.55, 1.70, 1.85, 2.00, 2.20] {
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
        // Measured, at the defaults: 0.95% resting, peaking at 1.23% while
        // four modules are mid-flip. The card asked for a few percent; this
        // is under one, which is where a red-on-black clock lands when the
        // card bodies really are black.
        let plain = apl(&[]);
        assert!((0.007..0.012).contains(&plain), "the resting picture is {:.2}% of full", plain * 100.0);
        let peak = (30..=40)
            .map(|k| {
                let f = run("09:59:59", f64::from(k) / crate::snapshot::FPS, &[]);
                (0..N).map(|i| f.pixel(i).duty()).sum::<f32>() / N as f32
            })
            .fold(0.0_f32, f32::max);
        assert!(peak < 0.02, "four modules mid-flip reach {:.2}%", peak * 100.0);
        assert!(peak > plain, "a flip should put more light on the panel, not less");
        // The halftone is the cheap way to halve the light without shrinking
        // anything, which is the one thing it has to actually do.
        let half = apl(&[("fill", 0.5)]);
        assert!((half / plain - 0.5).abs() < 0.12, "halftone 0.5 gave {:.2} of the light", half / plain);
        // And `size` takes light out by taking area out.
        assert!(apl(&[("size", 0.6)]) < plain * 0.6);
    }

    /// **The night-clock budget.** A rotation lights more of the panel, more
    /// often, than a flip did, and the lit edges of a dozen fast cards are
    /// the brightest thing the patch draws (card 155's `EDGE_BOOST`). So the
    /// peak and the mean are pinned, over the rotation and over a whole
    /// minute, at the worst moment the clock has (09:59:59 -> 10:00:00, four
    /// modules turning at once).
    #[test]
    fn a_rotation_is_still_a_picture_for_a_dark_room() {
        let apl = |f: &Frame| (0..N).map(|i| f.pixel(i).duty()).sum::<f32>() / N as f32;
        let film = film("09:59:59", 3.0, &[]);
        let resting = apl(&film[0].1);
        let over: Vec<f32> = film.iter().filter(|(t, _, _)| (0.95..2.2).contains(t)).map(|(_, f, _)| apl(f)).collect();
        let peak = over.iter().copied().fold(0.0_f32, f32::max);
        let mean = over.iter().sum::<f32>() / over.len() as f32;
        // Over a whole minute: the rotation, then 58-odd seconds of the
        // resting picture.
        let rotation = 2.2 - 0.95;
        let minute = (mean * rotation + resting * (60.0 - rotation)) / 60.0;
        println!("APL resting {:.3}%  rotation mean {:.3}%  peak {:.3}%  whole minute {:.3}%", resting * 100.0, mean * 100.0, peak * 100.0, minute * 100.0);
        assert!(peak < 0.030, "a rotation peaks at {:.2}% of the panel", peak * 100.0);
        assert!(mean < 0.020, "a rotation averages {:.2}%", mean * 100.0);
        assert!(minute < 0.012, "a minute with a rotation in it averages {:.2}%", minute * 100.0);
        assert!(peak > resting, "a rotation should put more light on the panel, not less");
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

    /// **The crispness proof.** A pixel face at `size` 1 is *exactly* crisp:
    /// inside a module there are two colours and no others, full ink and true
    /// black, with nothing in between. That is the whole reason to embed a
    /// pixel font rather than draw one - module centres and the axle are pixel
    /// boundaries, so a face whose cells are whole LEDs has every cell
    /// boundary on an LED boundary and `Frame::supersample`'s 36 samples
    /// inside an LED all land in the same cell.
    ///
    /// The faces that are not crisp are checked too, the other way round: if
    /// they came out with two colours as well, this test would be measuring
    /// nothing.
    #[test]
    fn a_pixel_face_is_exactly_crisp_at_size_one() {
        for (i, face) in faces::FACES.iter().enumerate() {
            let set: &[(&str, f32)] = &[("font", i as f32), ("zero", 1.0)];
            let g = geom(set);
            let (mut colours, mut lit) = (std::collections::BTreeSet::new(), 0);
            // Every numeral, and the four times of the contact sheets.
            for when in ["01:23", "04:56", "07:08", "09:59"] {
                let frame = run(when, 3.0, set);
                for y in 0..H {
                    for x in 0..W {
                        let v = y as f32 + 0.5 - g.axle;
                        if !g.centres.iter().any(|c| (x as f32 + 0.5 - c).abs() <= g.w * 0.5) || v.abs() > g.h * 0.5 {
                            continue;
                        }
                        let p = frame.pixel(y * W + x);
                        colours.insert(p.to_srgb8());
                        lit += usize::from(p.luma() > 1e-4);
                    }
                }
            }
            // Sixteen numerals, each around a hundred LEDs minus the seam.
            assert!(lit > 1000, "{}: only {lit} lit LEDs over four times", face.name);
            if face.crisp() {
                assert_eq!(colours.len(), 2, "{}: a crisp face drew {:?}", face.name, colours);
                assert!(colours.contains(&[0, 0, 0]), "{}: no black card", face.name);
            } else {
                assert!(colours.len() > 4, "{}: {} colours - is this face crisp after all?", face.name, colours.len());
            }
        }
    }

    /// The seam is the signature, so it is really there: the row of LEDs on
    /// the axle is black all the way across every module.
    #[test]
    fn the_seam_cuts_every_module() {
        let g = geom(&[]);
        // 08:38 puts a numeral with ink right across the waist (8, 3) in
        // every module, so nothing but the seam can be making the gap - with
        // `zero` on, because the hours' tens is otherwise a blank card and a
        // blank module has no seam to see.
        let frame = run("08:38", 3.0, &[("zero", 1.0)]);
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

    /// Mid-rotation, the module really is showing two different numerals at
    /// once - the next one standing above the axle, the last one still below
    /// it - and a card in between with an edge brighter than either. If any
    /// of that stopped being true the rotation would be a wipe.
    ///
    /// The drum carries a blank, so the module *is* allowed to go dark for
    /// the frame or two that card is on it. That is a real card passing, not
    /// a hole in the animation, and it must not be more than that.
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
        assert!(all_dark <= 3, "the module went blank for {all_dark} frames - more than the blank card passing");
        assert!(lit_edge > 5, "the falling card's edge is never lit: {lit_edge} of 31 frames");
    }

    /// The angle a single-card module - the hours' tens, blank to 1, in
    /// "changed cards only" - is at on frame `frame` of a run pinned to
    /// 09:59:59, planned through the same `plan` and the same clock as the
    /// renderer. Nothing here interpolates the fall curve by hand, which is
    /// how the README's table came to be written down against the wrong frame
    /// the first time.
    fn theta_at(flip: f64, frame: i32) -> f32 {
        let clock = at("09:59:59");
        let dt = 1.0 / crate::snapshot::FPS;
        let tm = Timing::slow(flip);
        let mut m = Module { from: BLANK, run: Vec::new() };
        for i in 0..=frame {
            let t = f64::from(i) * dt;
            let minute = (clock.now(t) / 60.0).floor() as i64;
            let want = cards(minute, true, false)[0];
            if m.shown(t) != want {
                m.plan(t, want, drum(0, false), Flips::Changed, &tm, 0.0);
            }
        }
        // The renderer's own formula: the card in the air if there is one,
        // otherwise the small rebound of the one that has just landed.
        let t = f64::from(frame) * dt;
        if let Some(c) = m.run.iter().find(|c| c.at <= t && t < c.at + c.fall) {
            return flap::angle(((t - c.at) / c.fall) as f32);
        }
        match m.run.iter().rev().find(|c| t >= c.at + c.fall) {
            Some(c) if t - c.at - c.fall < c.fall * SETTLE => 180.0 - flap::settle(((t - c.at - c.fall) / (c.fall * SETTLE)) as f32, SETTLE_DEG),
            _ => 0.0,
        }
    }

    /// The README's **rotation** table, checked rather than believed: which
    /// frame of a run pinned to 09:59:59 is worth a snapshot, and why. A
    /// module is "turning" when its own pixels differ from the next frame's,
    /// which is the only definition that does not need the patch's insides.
    #[test]
    fn the_readmes_rotation_table_lands_where_it_says() {
        let g = geom(&[]);
        let film = film("09:59:59", 3.2, &[]);
        let frame = |n: i32| &film[(30 + n) as usize];
        let turning = |n: i32| {
            let (a, b) = (&frame(n).2, &frame(n + 1).2);
            (0..4)
                .filter(|i| {
                    (0..H).any(|y| {
                        (0..W).any(|x| {
                            let u = x as f32 + 0.5 - g.centres[*i];
                            u.abs() <= g.w * 0.5 && a[(y * W + x) * 3..][..3] != b[(y * W + x) * 3..][..3]
                        })
                    })
                })
                .count()
        };
        assert_eq!(reads(&frame(0).1, &g), Some([B, 9, 5, 9]), "+0 is not the last settled frame");
        assert_eq!(frame(0).2, film[0].2, "+0 has already moved");
        // The first card is let go on the minute but takes a frame or two to
        // uncover anything, so the board is *moving* from +2.
        assert_eq!(turning(0), 0, "+0 is already moving");
        assert!(turning(2) >= 2, "+2 is not the board starting to move");
        assert_eq!(turning(8), 4, "+8 is not four modules turning at once");
        assert_eq!(turning(48), 1, "+48 is not the minutes' tens turning alone");
        assert_eq!(turning(54), 0, "+54 is not settled");
        assert_eq!(reads(&frame(54).1, &g), Some([1, 0, 0, 0]), "+54 does not read 10:00");
    }

    /// The README's table of snapshot recipes, checked rather than believed.
    ///
    /// `--time 09:59:59` starts the run there and the snapshot steps at 30
    /// fps, so 10:00:00 lands exactly on frame 30 and a card is `n` frames
    /// into its fall at `--at (30 + n) / 30`. With `--set flip=0.6` a fall is
    /// eighteen frames, which is what makes the five angles reachable at all.
    #[test]
    fn the_snapshot_recipes_in_the_readme_land_where_they_say() {
        assert_eq!(theta_at(0.6, 29), 0.0, "nothing moves before the minute");
        assert_eq!(theta_at(0.6, 30), 0.0, "the minute turns on frame 30 and the card lets go");
        assert!(theta_at(0.6, 31) > 0.0, "and it is moving on frame 31");
        for (secs, frame, want) in [(1.2, 36, 29.7), (1.3333, 40, 61.4), (1.4333, 43, 97.1), (1.5, 45, 127.4), (1.5333, 46, 144.3)] {
            assert_eq!((secs * crate::snapshot::FPS).round() as i32, frame, "--at {secs} is not frame {frame}");
            let got = theta_at(0.6, frame);
            assert!((got - want).abs() < 0.15, "--at {secs} is {got:.1} degrees, not {want}");
        }
        // And at the default fall these are the six frames the panel gets.
        // This is the sequence the owner actually sees; it is the one to
        // argue with.
        let real_time: Vec<f32> = (30..=36).map(|f| theta_at(0.2, f).round()).collect();
        assert_eq!(real_time, vec![0.0, 13.0, 30.0, 52.0, 84.0, 127.0, 180.0]);
        assert_eq!((f64::from(params().get("flip")) * crate::snapshot::FPS).round(), 6.0, "six frames a card");
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
            ("font", 0.0, (faces::NAMES.len() - 1) as f32, faces::DEFAULT),
            ("light", 40.0, 255.0, 120.0),
            ("hue", 0.0, 360.0, 0.0),
            ("size", 0.55, 1.0, 1.0),
            ("weight", 1.2, 3.0, 2.0),
            ("seam", 0.0, 3.0, 2.0),
            ("flips", 0.0, 2.0, 0.0),
            ("spin", 0.7, 2.5, 1.15),
            ("flip", 0.08, 0.6, 0.2),
            ("tilt", 0.0, 40.0, 16.0),
            ("fill", 0.0, 0.6, 0.0),
            ("pace", 5.0, 60.0, 60.0),
            ("blink", 0.0, 1.0, 0.0),
            ("hours24", 0.0, 1.0, 1.0),
            ("zero", 0.0, 1.0, 0.0),
            ("offset", 0.0, 1439.0, 0.0),
        ];
        assert_eq!(DEF.params.len(), want.len(), "a parameter came or went");
        for ((id, min, max, default), spec) in want.iter().zip(DEF.params) {
            assert_eq!(spec.id, *id);
            assert_eq!((spec.min, spec.max, spec.default), (*min, *max, *default), "{id}");
        }
    }
}

