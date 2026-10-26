//! Flame clock: hearth's fire, with the time standing in it.
//!
//! The owner, 2026-10-26: "could we modify the flames patch to function as a
//! clock? ... put a 3d voxel numeric read of the time as a non-flammable
//! object interfering with the flames. On an individual frame you might not
//! be able to see it, but over 30fps the behavior of the flame simulation
//! should make it visible." Then, inverted: "numerals are the fuel". A copy
//! of `hearth` (0.1.6): the time is fuel held in the shape of its digits
//! ([`clock`]), shown once a minute around the minute turning
//! ([`FlameClock::tell_time`]); hearth's sliders it does not offer are held
//! at the owner's values ([`Fixed`]). Everything below is hearth's, as it
//! was.
//!
//! Hearth: a fire burning just below the bottom edge of the panel.
//!
//! The owner, 2026-10-20: "something akin to a fireplace ... it looks like
//! just below the bottom of the screen is a fire, and we're seeing
//! flames/smokes/embers with meaningful randomness/generative behavior to it."
//! And, on how: "i want maximum fire realism". Card 372.
//!
//! **What is simulated.** A real 3D Eulerian fluid fire, stepped in wgpu
//! compute shaders ([`gpu`], `flame_clock.wgsl`): fuel from a bed of a few seeded
//! logs, burning to heat and an expansion at the flame (Nguyen, Fedkiw and
//! Jensen), heat lifting the gas, vorticity confinement keeping the curl at
//! the flame tips, MacCormack advection keeping them sharp, a pressure
//! projection keeping it incompressible. It is seen by ray-marching the
//! volume, many rays per LED: emission is blackbody ([`ramp`]: Planck against
//! the CIE 1931 matching functions, worked out once on the CPU into a table),
//! and the smoke absorbs it and is lit only by the fire's own light, gathered
//! onto a coarse grid and spread. Embers are GPU particles in the same
//! velocity field.
//!
//! **The grid is shaped to the shot**: 192 x 128 x 48 cells, three across per
//! LED, the logs in the bottom rows below the frame, and a camera a little
//! low and a long way back, looking a touch up.
//!
//! **Framing.** The fuel bed is below the frame. The flames usually reach the
//! lower third, at a swell perhaps half; above that is black, with the odd
//! ember climbing and a faint warm wisp of smoke just over the flames.
//!
//! **Time.** Everything steps on a fixed [`STEP`] from `ctx.dt` - the fluid,
//! the eased `size`, the breathing, the dice for gusts, embers and settling
//! logs - so a snapshot is the same picture every time, and at any frame rate.
//! There is no warm-up: the fire lights on screen, from an empty firebox
//! (the owner, card 373: "it can just start with zero fire and literally
//! start while it's on screen"). A five-second warm-up inside the first
//! frame held it for ~9 s, and the Studio drops a patch after 5.
//!
//! **Colour** is painted by palette index ([`FlameClock::paint`]): black, a
//! 24-step blackbody ramp chosen at even perceptual lightness, and four smoke
//! tints. 29 colours, so every frame is exact whatever it looks like.
//!
//! **The stove** (card 373): with a `draft`, the panel is the inside of a
//! wood stove's firebox. Its top edge is a solid ceiling with a chimney flue
//! in the middle; flames that reach it spread under it and the gas is drawn
//! in toward the flue and up it, sparks with it. `draft` 0 is card 372's
//! open fire.
//!
//! Named settings to offer (card 373's scale: card 372's maximum is 0.6):
//!
//! - **Roaring**: `size` 0.85, `gusts` 0.8, `embers` 0.8, `smoke` 0.3,
//!   `draft` 0.85.
//! - **Stove** (the defaults): `size` 0.6, `gusts` 0.6, `embers` 0.6,
//!   `smoke` 0.3, `draft` 0.7, `flue` 0.4.
//! - **Open hearth**: the defaults with `draft` 0.
//! - **Embers**: `size` 0.1, `gusts` 0.3, `embers` 0.3, `smoke` 0.15,
//!   `draft` 0.5.
//!
//! Credit: the method is Stam, "Stable Fluids" (SIGGRAPH 1999); Fedkiw, Stam
//! and Jensen, "Visual Simulation of Smoke" (SIGGRAPH 2001); Nguyen, Fedkiw
//! and Jensen, "Physically Based Modeling and Animation of Fire" (SIGGRAPH
//! 2002); and Selle et al., "An Unconditionally Stable MacCormack Method"
//! (2008). The pipeline follows `olliep24/firesim`
//! (<https://github.com/olliep24/firesim>), the reference the owner pointed
//! at, studied but not copied - it has no licence.

mod clock;
mod gpu;
mod noise;
mod ramp;
#[cfg(test)]
mod tests;

use crate::color::{oklch, smoothstep, to_oklab, Rgb};
use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::gpu::Gpu;
use crate::patch::{param, toggle, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use gpu::{Params, Saved, Sim, EMBERS};
use ramp::Ramp;

pub const DEF: PatchDef = PatchDef {
    id: "flame-clock",
    name: "Flame clock",
    blurb: "GPU. Hearth's 3D fluid fire with the time standing in it as an unseen, unburnable solid: the flames part round the numerals, and over a few seconds they read.",
    params: PARAMS,
    make,
    // The seed lays the logs (how many, where, how big), and every wandering
    // strength, gust and spark follows from it: another seed is another fire.
    seeded: true,
};

// Card 373: the owner liked card 372's fire best with these sliders all the
// way up, so card 372's maximum is now about [`OLD_MAX`] of each range, the
// default sits there, and the rest of the range is headroom above it.
const PARAMS: &[ParamSpec] = &[
    // The time (2026-10-26). Once a minute, around the minute turning: a
    // jet lights it `before` seconds ahead; at the minute a jet under the
    // digits that change turns them into the new ones; `after` seconds on
    // it collapses into the fire (the owner: "I want it to show the time
    // change (from one minute to another) and then some number of seconds
    // before/after, probably default 5 each").
    toggle("hours24", "24-hour", false),
    // Defaults: the owner's on Office, 2026-10-26.
    param("before", "Seconds the time shows before the minute turns", 1.0, 25.0, 1.0, 1.0),
    param("after", "Seconds the new time shows after the minute turns", 4.0, 25.0, 1.0, 10.0),
    param("height", "Where the time stands, up from the bottom of the frame (0.5: the middle)", 0.0, 1.0, 0.01, 0.5),
    param("digits", "How tall the digits are, LEDs", 10.0, 20.0, 1.0, 20.0),
    // At most a fifth of the height: heavier fills the holes in 0, 4, 6, 8.
    param("weight", "How thick the digits' strokes are, LEDs (at most a fifth of their height)", 1.0, 4.0, 1.0, 3.0),
    param("depth", "How deep into the fire the digits reach (1: front to back)", 0.05, 1.0, 0.01, 0.43),
    // Below about 0.15 the digits do not catch; at 1 the frame floods.
    param("fuel", "How much fuel the digits give off", 0.0, 2.0, 0.01, 0.38),
    param("flames", "How tall the digits' flames are", 0.0, 2.0, 0.01, 1.19),
    // The fire: the Living Room panel's hearth, which the owner likes
    // (2026-10-26: "i like how it looks over there. this one is patchier"),
    // embers as on Office. Card 373's scale: card 372's maximum is 0.6.
    param("size", "How big the fire is between showings", 0.0, 2.0, 0.01, 1.13),
    param("gusts", "How often the draught surges between showings", 0.0, 2.0, 0.01, 0.75),
    param("embers", "How many sparks fly up", 0.0, 2.0, 0.01, 0.83),
    param("smoke", "How much smoke shows", 0.0, 1.0, 0.01, 0.41),
    param("draft", "How hard the chimney draws (0: an open fire, no stove)", 0.0, 2.0, 0.01, 0.73),
    // The gas and sparks run this many times faster than wall time (hearth,
    // exp/hearth-speed); 18 is about a real fire.
    param("speed", "How fast the gas moves (1: the slow, grand fire; 18: a real one)", 1.0, 18.0, 0.5, 5.0),
    param("color", "How much colour (0 is pure grayscale)", 0.0, 1.0, 0.01, 1.0),
];

/// What hearth has as sliders and this patch holds fixed (2026-10-26: "let's
/// look to simplify the control surface"): the values the owner had on the
/// panels, and, for hearth's tests copied here, hearth's own.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fixed {
    /// The flue's width, the bed's patchiness, the burn's expansion and the
    /// ceiling's rise above the frame: hearth's `flue`, `wood`, `expansion`
    /// and `headroom`.
    pub flue: f32,
    pub wood: f32,
    pub expansion: f32,
    pub headroom: f32,
    /// The dark ramp ([`DARK_CHOICES`]).
    pub dark: usize,
    /// How hard the woodpile burns while the time shows, and between.
    pub logs: f32,
    pub fire: f32,
    /// How often the draught surges while the time shows.
    pub gusts: f32,
}

impl Fixed {
    /// The Living Room's hearth; the woodpile out while the time shows;
    /// the owner's gusts on Office for the time.
    pub const PINNED: Fixed = Fixed { flue: 0.07, wood: 0.94, expansion: 0.06, headroom: 0.0, dark: DEFAULT_DARK, logs: 0.0, fire: 1.0, gusts: 1.72 };
    /// Hearth's defaults, the woodpile always burning.
    #[cfg(test)]
    pub const HEARTH: Fixed = Fixed { flue: 0.4, wood: 0.0, expansion: 1.0, headroom: 0.0, dark: DEFAULT_DARK, logs: 1.0, fire: 1.0, gusts: 0.6 };
}

/// The time's showing, seconds: the jet lights it over `EMERGE` (the jet
/// for the first `JET` of it, the numerals catching from `CATCH` on), and
/// it melts over `MELT`.
const EMERGE: f64 = 1.8;
const JET: f64 = 0.9;
const CATCH: f64 = 0.3;
const MELT: f64 = clock::COLLAPSE as f64;
/// The jet's fuel and lift, the lift in design units per second squared
/// (buoyancy at full heat is 95).
const JET_FUEL: f32 = 0.45;
const JET_LIFT: f32 = 260.0;
/// The surge as the time melts: how long it lasts, and how far into the
/// melt it may start, seconds.
const SURGE: f64 = 1.2;
const SURGE_LATEST: f32 = 1.2;
/// The surge's fuel, in jets.
const SURGE_FUEL: f32 = 0.5;

/// The morph at the minute: the jet under the changing digits, and when
/// the new ones start and finish catching, seconds after the minute.
const MORPH_JET: f64 = 1.2;
const MORPH_CATCH: (f64, f64) = (0.5, 1.9);

/// Where in its showing the time is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Showing {
    Emerging,
    Standing,
    Morphing,
    Melting,
    Burning,
}

/// Where card 372's maximum sits on the rescaled `size`, `gusts` and
/// `embers` (card 373): a slider at this value behaves as card 372's did at
/// 1, and the range runs on to 1 / `OLD_MAX` of it.
const OLD_MAX: f32 = 0.6;

/// A slider in card 372's units: 1 is its old maximum.
fn intensity(ctx: &Ctx, id: &str) -> f32 {
    ctx.get(id) / OLD_MAX
}

// Space is described in *design units*: card 372's cells, three to an LED
// across. The simulation grid ([`Grid`]) has `k` cells to a design unit, so
// the same firebox can be simulated finer or coarser (card 374).

/// The firebox in design units: across, up, into the hearth.
const WIDE: f32 = 3.0 * W as f32;
const TALL: f32 = 136.0;
/// The row the bottom edge of the frame sits on (at the middle depth): the
/// logs are below it. Experiment (exp/hearth-speed): 16, the bench panel's
/// 15 mm of black margin under the LEDs, so the woodpile sits in it.
const BELOW: f32 = 16.0;
/// The firebox ceiling (card 373): the row at the frame's top edge, at the
/// middle depth. Only with a draft.
const CEILING: f32 = BELOW + 3.0 * H as f32;
/// The woodpile's rows, design units: the logs and the gas just around them.
const BED_TOP: f32 = 22.0;
/// How far from a log's surface its gas comes off, design units (a grid
/// cell is 1.5): the shell the bed holds fuel in.
const SHELL: f32 = 1.6;
/// How far from the middle the woodpile may reach, a share of the width.
const SPREAD: f32 = 0.46;
/// How far back the camera stands from the middle of the hearth.
const DISTANCE: f32 = 340.0;
/// How far below the frame's middle the camera's eye is: a little low,
/// looking a touch up.
const EYE_DROP: f32 = 22.0;

/// How the fire is simulated and seen: the knobs card 374 swept for the
/// point where the picture stops improving.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Grid {
    /// Grid cells per design unit.
    pub k: f32,
    /// The firebox's depth, design units.
    pub depth: f32,
    /// Pressure sweeps per step.
    pub sweeps: usize,
    /// Rays per LED, each way.
    pub samples: u32,
    /// Ray-march step, grid cells.
    pub march: f32,
}

impl Grid {
    /// What ships: card 374's knee. Two cells per LED across (128 x 88 x 32)
    /// instead of three keeps the pointed tongues; at 1.5 they go to rounded
    /// blobs. Pressure sweeps past 16 change nothing a survey of the
    /// picture can find (16, 32 and 64 measure the same). Three rays per LED
    /// each way look the same as five on the panel. About a third of card
    /// 373's GPU time.
    ///
    /// For speed (docs/research/011): 8 sweeps measure and look as 16 do,
    /// since the pressure carries from step to step (4 flattens the fire).
    /// About 0.8 of the step's GPU time.
    pub const SHIPPED: Grid = Grid { k: 2.0 / 3.0, depth: 48.0, sweeps: 8, samples: 3, march: 0.5 };

    fn cells(design: f32, k: f32) -> usize {
        ((design * k / 4.0).ceil() as usize).max(2) * 4
    }
    pub fn gw(&self) -> usize {
        Grid::cells(WIDE, self.k)
    }
    pub fn gh(&self) -> usize {
        Grid::cells(TALL, self.k)
    }
    pub fn gd(&self) -> usize {
        Grid::cells(self.depth, self.k)
    }
    pub fn dims(&self) -> [u32; 3] {
        [self.gw() as u32, self.gh() as u32, self.gd() as u32]
    }
    /// How many grid rows the woodpile covers.
    pub fn bed_rows(&self) -> usize {
        (BED_TOP * self.k).ceil() as usize
    }
    /// The ceiling's grid row.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ceiling(&self) -> usize {
        (CEILING * self.k).round() as usize
    }
}
/// How far the frame's light may stray from its low-passed self, and that
/// low-pass's time constant, seconds ([`FlameClock::paint`]).
const STEADY: f32 = 0.05;
const STEADY_TAU: f32 = 0.3;

/// An ember slot at or below this heat is empty.
const EMBER_OUT: f32 = 0.30;

/// The fixed step, seconds.
pub(crate) const STEP: f64 = 1.0 / 30.0;
/// After a stall, at most this many steps are caught up: a paused Studio
/// resumes the fire, it does not fast-forward it.
const CATCHUP: i64 = 15;
/// The most fixed steps of gas one GPU substep may cover at `speed` above 1.
/// One: bigger substeps smooth the flames into jets (docs/research/011); a
/// fast fire is paid for with a coarser grid instead ([`GRIDS`]). Env
/// `HEARTH_BIG` overrides it, for experiments.
const BIG: f32 = 1.0;

/// At `speed`, how many GPU substeps each fixed step takes, and each one's dt.
fn substeps(speed: f32) -> (usize, f32) {
    let speed = speed.max(1.0);
    static BIG_ENV: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    let big = *BIG_ENV.get_or_init(|| std::env::var("HEARTH_BIG").ok().and_then(|v| v.parse().ok()).unwrap_or(BIG));
    let n = (speed / big).ceil().max(1.0);
    (n as usize, STEP as f32 * speed / n)
}

/// How long `size` takes to follow its parameter: the time constant, seconds.
const SIZE_EASE: f32 = 18.0;
/// The same, as the time comes and goes.
const TIME_EASE: f32 = 1.5;

const FIRE_STEPS: usize = 24;
const SMOKE_STEPS: usize = 4;

/// The `dark` choices (card 375). `"as before"` is card 374's computed
/// palette; each other is a [`DARK_RAMPS`] entry, the same index less one.
pub(crate) const DARK_CHOICES: &[&str] = &["as before", "steady red", "deeper red", "warmer"];
pub(crate) const DEFAULT_DARK: usize = 2;

/// The fire's ramp as panel level triples (brief 2.1.1), darkest first. Every
/// channel sits on one of the panel's 64 levels, so nothing in the fire is
/// made of a channel alternating between two levels near black - which is
/// what made card 374's cool reds band and drift in hue: its computed dark
/// entries had green at a fraction of level 1, and the `aligned_dark` output
/// snapped each channel its own way.
///
/// The bright half is the computed blackbody ramp at its nearest levels and
/// is the same in all three; the ramps differ in the dark half, where it
/// shows.
pub(crate) const DARK_RAMPS: [[[u32; 3]; FIRE_STEPS]; 3] = [
    // "steady red": card 374's ramp at its nearest levels, green held at 0
    // until it is past half a level - the same look, steadied.
    [
        [2, 0, 0], [3, 0, 0], [4, 0, 0], [5, 0, 0], [6, 0, 0], [7, 1, 0], [8, 1, 0], [10, 1, 0],
        [12, 2, 0], [14, 2, 0], [16, 2, 0], [18, 3, 0], [20, 4, 0], [23, 5, 0], [25, 6, 0], [28, 7, 0],
        [30, 8, 0], [32, 10, 1], [35, 11, 1], [37, 13, 2], [40, 15, 3], [43, 17, 4], [45, 19, 5], [48, 22, 6],
    ],
    // "deeper red": one level darker at the bottom and pure red for longer,
    // so the tips and the edges fade through deep red rather than orange.
    [
        [1, 0, 0], [2, 0, 0], [3, 0, 0], [4, 0, 0], [5, 0, 0], [6, 0, 0], [8, 0, 0], [10, 1, 0],
        [12, 1, 0], [14, 1, 0], [16, 2, 0], [18, 2, 0], [20, 3, 0], [23, 4, 0], [25, 5, 0], [28, 6, 0],
        [30, 8, 0], [32, 10, 1], [35, 11, 1], [37, 13, 2], [40, 15, 3], [43, 17, 4], [45, 19, 5], [48, 22, 6],
    ],
    // "warmer": green comes in early, so the cool end is a dim orange
    // rather than a red.
    [
        [2, 0, 0], [3, 1, 0], [4, 1, 0], [5, 1, 0], [6, 2, 0], [7, 2, 0], [8, 2, 0], [10, 3, 0],
        [12, 3, 0], [14, 4, 0], [16, 4, 0], [18, 5, 0], [20, 6, 0], [23, 7, 0], [25, 8, 0], [28, 9, 0],
        [30, 10, 1], [32, 11, 1], [35, 12, 1], [37, 13, 2], [40, 15, 3], [43, 17, 4], [45, 19, 5], [48, 22, 6],
    ],
];

/// The soot greys with a level ramp: level 1 and level 2 on every channel,
/// nothing between them and black. Card 374's darkest computed soot sat
/// below level 1 and the panel showed it *at* level 1 - brighter than meant.
/// Soot dimmer than level 1 is now dithered against black instead.
const SOOT_TRIPLES: [[u32; 3]; 2] = [[1, 1, 1], [2, 2, 2]];
/// Darkest fire entry's lightness, OKLab; below half of it, black.
const FIRE_L0: f32 = 0.2;
/// Soot that has taken more than this share of the fire behind it paints
/// that LED black.
const SOOT_BLACK: f32 = 0.6;
/// The soot greys, OKLab lightness, darkest first. Neutral: no chroma.
const SMOKE_L: [f32; SMOKE_STEPS] = [0.2, 0.25, 0.3, 0.35];
/// Lit smoke, as the march returns it, to a step on the smoke ramp at
/// `smoke` 1.
const SMOKE_GAIN: f32 = 20.0;

/// A log: a capped cylinder, design units, lying near level.
struct Log {
    /// The middle of its axis.
    x: f32,
    y: f32,
    z: f32,
    r: f32,
    /// Half its length, and the axis's turn from straight across toward
    /// the back, radians.
    half: f32,
    yaw: f32,
    salt: u32,
    /// Offset into its own strength noise.
    phase: f32,
}

impl Log {
    fn axis(&self) -> [f32; 3] {
        [self.yaw.cos(), 0.0, self.yaw.sin()]
    }

    /// How far `p` is from its surface (negative inside), and which way
    /// that surface faces up: 1 on top, -1 underneath, and 2 at an end (the
    /// cut end of a log, where the gas comes out hardest).
    fn surface(&self, p: [f32; 3]) -> (f32, f32) {
        let a = self.axis();
        let q = [p[0] - self.x, p[1] - self.y, p[2] - self.z];
        let along = q[0] * a[0] + q[2] * a[2];
        let radial = [q[0] - along * a[0], q[1], q[2] - along * a[2]];
        let rho = (radial[0] * radial[0] + radial[1] * radial[1] + radial[2] * radial[2]).sqrt();
        let (side, end) = (rho - self.r, along.abs() - self.half);
        let d = side.max(0.0).hypot(end.max(0.0)) + side.max(end).min(0.0);
        let facing = if end > side { 2.0 } else { radial[1] / rho.max(1e-3) };
        (d, facing)
    }

    /// Its reach across, from the middle of its axis.
    fn reach(&self) -> f32 {
        self.half * self.yaw.cos().abs() + self.r * self.yaw.sin().abs() + self.r
    }

    /// A point on its top, `f` in -1..1 along it, lifted `lift` off it.
    fn top(&self, f: f32, lift: f32) -> [f32; 3] {
        let a = self.axis();
        [self.x + f * self.half * a[0], self.y + self.r + lift, self.z + f * self.half * a[2]]
    }
}

/// A woodpile cell where something happens, worked out once from the logs:
/// inside one, or in the shell round one or more.
struct BedCell {
    /// Its index in the bed, and where it is, design units.
    at: usize,
    p: [f32; 3],
    inside: bool,
    /// Up to three logs whose gas reaches it, each with how much (before
    /// the log's own strength), and the hot-gap bonus.
    near: [(usize, f32); 3],
    gap: f32,
}

/// The woodpile's cells for `logs` on `grid`.
fn bed_cells(logs: &[Log], grid: &Grid) -> Vec<BedCell> {
    let (gw, rows, gd, k) = (grid.gw(), grid.bed_rows(), grid.gd(), grid.k);
    let mut cells = Vec::new();
    for z in 0..gd {
        for y in 0..rows {
            for x in 0..gw {
                let p = [x as f32 / k, y as f32 / k, z as f32 / k];
                let (mut d1, mut d2, mut inside) = (f32::MAX, f32::MAX, false);
                let mut near = [(0, 0.0f32); 3];
                for (i, log) in logs.iter().enumerate() {
                    let (d, facing) = log.surface(p);
                    inside |= d < 0.0;
                    if d < d1 {
                        (d1, d2) = (d, d1);
                    } else if d < d2 {
                        d2 = d;
                    }
                    if (0.0..3.0 * SHELL).contains(&d) {
                        let face = if facing > 1.5 { 1.3 } else { 0.2 + 0.8 * smoothstep(-0.4, 0.7, facing) };
                        let w = face * (-d / SHELL).exp();
                        // Keep the strongest three.
                        let slot = (0..3).min_by(|&a, &b| near[a].1.total_cmp(&near[b].1)).unwrap_or(0);
                        if w > near[slot].1 {
                            near[slot] = (i, w);
                        }
                    }
                }
                // A gap: two surfaces close on either side of the cell.
                let gap = if !inside && d2 < 4.0 * SHELL { 1.2 * (-(d1 + d2) / (2.0 * SHELL)).exp() } else { 0.0 };
                if inside || gap > 0.0 || near.iter().any(|n| n.1 > 0.0) {
                    cells.push(BedCell { at: (z * rows + y) * gw + x, p, inside, near, gap });
                }
            }
        }
    }
    cells
}

struct Gust {
    start: f64,
    length: f64,
    /// Signed peak, cells per second.
    peak: f32,
}

struct Settle {
    start: f64,
    log: usize,
    sparks: u32,
    next_spark: f64,
}

/// The camera, worked out once: where it is, its unit axes, and the
/// tangents of half the frame each way.
struct Camera {
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
    tan_w: f32,
    tan_h: f32,
}

impl Camera {
    /// Set up in design units, then scaled to the grid's.
    fn new(grid: &Grid) -> Camera {
        let centre = [WIDE * 0.5, BELOW + 1.5 * H as f32, grid.depth * 0.5];
        let eye = [centre[0], centre[1] - EYE_DROP, centre[2] - DISTANCE];
        let d = [centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let fwd = d.map(|v| v / len);
        // right = world up x fwd, normalised; up = fwd x right.
        let r = [fwd[2], 0.0, -fwd[0]];
        let rl = (r[0] * r[0] + r[2] * r[2]).sqrt();
        let right = r.map(|v| v / rl);
        let up = [
            fwd[1] * right[2] - fwd[2] * right[1],
            fwd[2] * right[0] - fwd[0] * right[2],
            fwd[0] * right[1] - fwd[1] * right[0],
        ];
        Camera { eye: eye.map(|v| v * grid.k), right, up, fwd, tan_w: 0.5 * WIDE / len, tan_h: 1.5 * H as f32 / len }
    }

    /// A grid position on the panel, in LED coordinates.
    fn project(&self, p: [f32; 3]) -> Option<(f32, f32)> {
        let d = [p[0] - self.eye[0], p[1] - self.eye[1], p[2] - self.eye[2]];
        let dot = |a: [f32; 3]| a[0] * d[0] + a[1] * d[1] + a[2] * d[2];
        let z = dot(self.fwd);
        if z < 1.0 {
            return None;
        }
        let u = dot(self.right) / (z * self.tan_w);
        let v = dot(self.up) / (z * self.tan_h);
        Some(((u + 1.0) * 0.5 * W as f32, (1.0 - v) * 0.5 * H as f32))
    }
}

pub(crate) struct FlameClock {
    pub(crate) grid: Grid,
    /// `None` until first render; `Some(None)` if there is no GPU.
    sim: Option<Option<Sim>>,
    /// Whether the grid follows `speed` ([`k_for`]), and the sims of grids
    /// it has left, kept for the way back.
    follow: bool,
    spare: Vec<Sim>,
    camera: Camera,
    /// The ember slots as last read back (three vec4s each), the slots
    /// filled since, and the sparks waiting to go in with the next step.
    sparks: Vec<[f32; 4]>,
    pending: Vec<usize>,
    spawns: Vec<(usize, [[f32; 4]; 4])>,
    /// Each slot's bounce counters as last read back, and the running
    /// totals (card 376).
    bounces_read: [[f32; 4]; EMBERS],
    pub(crate) bounces: Bounces,
    spark_dice: Rng,
    logs: Vec<Log>,
    salt: u32,
    dice: Rng,
    /// Sim seconds, a whole number of steps.
    t: f64,
    steps: i64,
    /// Steps since the patch began, for the shader's per-step dice.
    step_no: u64,
    clock: f64,
    warm: bool,
    /// The eased fire size the sim is burning at.
    size: f32,
    /// Soot absorption per cell per unit of smoke.
    soot: f32,
    /// The chimney's pull this step, surges included (0: an open fire), and
    /// the flue's radius in cells.
    draft: f32,
    flue: f32,
    /// The ceiling's rise above the frame's top edge, LEDs (`headroom`).
    headroom: f32,
    /// Gas time, seconds: wall time times `speed`. The bed's tongues and
    /// pockets follow it, the logs' own swells keep wall time.
    gas_t: f64,
    /// `wood`: how patchy the bed's release is (0: card 372's held sheet).
    wood: f32,
    /// `expansion`: the share of card 372's burn expansion kept.
    expansion: f32,
    gust: Option<Gust>,
    calm_until: f64,
    settle: Option<Settle>,
    bed: Vec<[f32; 4]>,
    bed_cells: Vec<BedCell>,
    wind: [[f32; 4]; 40],
    ramp: Ramp,
    /// The fire ramp's temperatures and lightnesses, and the palette built
    /// from them for the last `color`.
    fire_l: [f32; FIRE_STEPS],
    fire_theta: [f32; FIRE_STEPS],
    palette: Option<Painting>,
    /// The frame's light, low-passed: what [`FlameClock::paint`] holds the whole
    /// frame near.
    steady: Option<f32>,
    // Counters for tests and `playing`.
    pub(crate) gusts_seen: u32,
    pub(crate) settles_seen: u32,
    pub(crate) sparks_thrown: u32,
    /// What hearth has as sliders and this holds fixed.
    pub(crate) fixed: Fixed,
    /// The time: its shape on the panel, and `Params::clock`.
    glyph: clock::Mask,
    clock_slab: [f32; 4],
    /// `Params::numerals`, but for gas time, which is the step's.
    numerals: [f32; 4],
    /// `logs`: the share of the woodpile's gas it gives off.
    logs_burn: f32,
    /// The jet, as `Params::jet`.
    jet: [f32; 4],
    showing: Showing,
    /// How much the time is showing (1 standing, 0 the fire alone), and
    /// whether the time comes and goes at all.
    shown: f32,
    cycling: bool,
}

fn make(seed: u64) -> Box<dyn Patch> {
    // Experiment: env `HEARTH_K` holds the grid at that many cells per
    // design unit instead of following `speed` (docs/research/011).
    match std::env::var("HEARTH_K").ok().and_then(|v| v.parse().ok()) {
        Some(k) => Box::new(FlameClock::with_grid(seed, Grid { k, ..Grid::SHIPPED })),
        None => Box::new(FlameClock::new(seed)),
    }
}

/// The grid for each `speed`, up to it: the slow fire on card 374's grid,
/// a fast one coarser. A step costs about the cells, and a frame holds
/// `speed` steps, so each is the finest whose frame fits in ~28 ms on
/// workbench's Intel iGPU (docs/research/011: 19, 6.1, 3.5, 1.7 and ~0.6 ms
/// a frame per unit of speed). The owner on the panel: 1/3 at speed 11
/// "pretty good", 1/2's detail "noticeable" at 4. At 18 on 1/3 the GPU was
/// ~90% busy, so the top speeds step down to a quarter.
const GRIDS: &[(f32, f32)] = &[(1.5, 2.0 / 3.0), (4.5, 0.5), (8.0, 0.4), (12.0, 1.0 / 3.0), (f32::MAX, 0.25)];

fn k_for(speed: f32) -> f32 {
    GRIDS.iter().find(|(up_to, _)| speed <= *up_to).map_or(0.25, |g| g.1)
}

/// `saved`, a fire on a grid of `from` cells per design unit, on a grid of
/// `dims` at `to`: each field sampled where the new cell sits, and lengths
/// in cells scaled (velocity once, pressure twice, since it is velocity
/// times cells).
fn resample(saved: &Saved, from: f32, dims: [u32; 3], to: f32) -> Saved {
    let r = to / from;
    let [ow, oh, od] = saved.dims.map(|d| d as usize);
    let [nw, nh, nd] = dims.map(|d| d as usize);
    let mut out = Saved {
        dims,
        vel: Vec::with_capacity(nw * nh * nd),
        scal: Vec::with_capacity(nw * nh * nd),
        pres: Vec::with_capacity(nw * nh * nd),
        parts: saved.parts.clone(),
    };
    let axis = |n: usize, o: usize| -> (usize, usize, f32) {
        let p = (n as f32 / r).clamp(0.0, (o - 1) as f32);
        let i = (p as usize).min(o - 2);
        (i, i + 1, p - i as f32)
    };
    for z in 0..nd {
        let (z0, z1, fz) = axis(z, od);
        for y in 0..nh {
            let (y0, y1, fy) = axis(y, oh);
            for x in 0..nw {
                let (x0, x1, fx) = axis(x, ow);
                let at = |x, y, z| (z * oh + y) * ow + x;
                let corners = [
                    (at(x0, y0, z0), (1.0 - fx) * (1.0 - fy) * (1.0 - fz)),
                    (at(x1, y0, z0), fx * (1.0 - fy) * (1.0 - fz)),
                    (at(x0, y1, z0), (1.0 - fx) * fy * (1.0 - fz)),
                    (at(x1, y1, z0), fx * fy * (1.0 - fz)),
                    (at(x0, y0, z1), (1.0 - fx) * (1.0 - fy) * fz),
                    (at(x1, y0, z1), fx * (1.0 - fy) * fz),
                    (at(x0, y1, z1), (1.0 - fx) * fy * fz),
                    (at(x1, y1, z1), fx * fy * fz),
                ];
                let (mut v, mut s, mut p) = ([0.0f32; 4], [0.0f32; 4], 0.0f32);
                for (i, w) in corners {
                    for c in 0..4 {
                        v[c] += w * saved.vel[i][c];
                        s[c] += w * saved.scal[i][c];
                    }
                    p += w * saved.pres[i];
                }
                out.vel.push([v[0] * r, v[1] * r, v[2] * r, v[3]]);
                out.scal.push(s);
                out.pres.push(p * r * r);
            }
        }
    }
    // Ember slots: position, velocity and last position are in cells.
    for (k, slot) in out.parts.iter_mut().enumerate() {
        if k % 4 < 3 {
            for v in &mut slot[..3] {
                *v *= r;
            }
        }
    }
    out
}

impl FlameClock {
    /// The player's hearth: its grid follows `speed`.
    pub(crate) fn new(seed: u64) -> FlameClock {
        FlameClock { follow: true, ..FlameClock::with_grid(seed, Grid::SHIPPED) }
    }

    /// Everything worked out from the grid, worked out again for `grid`.
    fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
        self.bed_cells = bed_cells(&self.logs, &grid);
        self.bed = vec![[0.0; 4]; grid.gw() * grid.bed_rows() * grid.gd()];
        self.camera = Camera::new(&grid);
    }

    /// Move the fire to a grid of `k` cells per design unit, carrying it
    /// over: the fields resampled, the sparks rescaled. The old grid's sim
    /// is kept, so going back costs no shader compile.
    fn regrid(&mut self, k: f32, gpu: &'static Gpu) {
        let from = self.grid.k;
        self.set_grid(Grid { k, ..self.grid });
        let dims = self.grid.dims();
        let old = self.sim.take().flatten();
        let saved = old.as_ref().map(|sim| sim.save());
        let sim = match self.spare.iter().position(|s| s.dims() == dims) {
            Some(i) => self.spare.swap_remove(i),
            None => Sim::new(gpu, dims, self.grid.bed_rows() as u32, self.grid.sweeps, &self.ramp.table()),
        };
        if let Some(saved) = saved {
            sim.load(&resample(&saved, from, dims, k));
        }
        self.spare.extend(old);
        let r = k / from;
        for (slot, v) in self.sparks.iter_mut().enumerate() {
            if slot % 4 < 3 {
                v[..3].iter_mut().for_each(|c| *c *= r);
            }
        }
        for (_, spark) in &mut self.spawns {
            for v in &mut spark[..3] {
                v[..3].iter_mut().for_each(|c| *c *= r);
            }
        }
        self.sim = Some(Some(sim));
    }

    pub(crate) fn with_grid(seed: u64, grid: Grid) -> FlameClock {
        let mut rng = Rng::new(seed ^ 0x4ea7_7a11);
        let salt = rng.u64() as u32;
        // Three or four logs on the floor across the width, never evenly
        // spaced and never quite parallel, and one or two laid across the
        // gaps between them, resting on both.
        let count = 3 + (rng.u64() % 2) as usize;
        let mut logs = Vec::with_capacity(count + 2);
        let log = |rng: &mut Rng, x: f32, y: f32, z: f32, r: f32, half: f32, yaw: f32| Log {
            x,
            y: y.max(r),
            z,
            r,
            half,
            yaw,
            salt: rng.u64() as u32,
            phase: rng.range(0.0, 100.0),
        };
        for k in 0..count {
            let slot = (k as f32 + 0.5) / count as f32;
            let r = rng.range(4.0, 5.5);
            let (x, z) = (WIDE * (0.14 + 0.72 * slot) + rng.range(-8.0, 8.0), grid.depth * 0.5 + rng.range(-6.0, 6.0));
            let (half, yaw) = (rng.range(20.0, 28.0), rng.range(-0.35, 0.35));
            logs.push(log(&mut rng, x, r, z, r, half, yaw));
        }
        let gaps = 1 + (rng.u64() % (count as u64 - 1)).min(1) as usize;
        let mut pairs: Vec<usize> = (0..count - 1).collect();
        for _ in 0..gaps {
            let p = pairs.remove((rng.u64() % pairs.len() as u64) as usize);
            let (a, b) = (&logs[p], &logs[p + 1]);
            let r = rng.range(3.5, 4.5);
            // Sat down into the gap: its axis a little under the tops of
            // the two it rests on, plus its own radius.
            let y = 0.5 * (a.y + a.r + b.y + b.r) + 0.6 * r;
            let (x, z) = (0.5 * (a.x + a.half * a.yaw.cos() + b.x - b.half * b.yaw.cos()), 0.5 * (a.z + b.z) + rng.range(-3.0, 3.0));
            let (half, yaw) = (rng.range(16.0, 22.0), rng.range(-0.6, 0.6));
            logs.push(log(&mut rng, x, y, z, r, half, yaw));
        }
        // The pile centred and kept off the side walls: the fire is as
        // likely to reach out of frame on the left as on the right.
        let (lo, hi) = logs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), l| (lo.min(l.x - l.reach()), hi.max(l.x + l.reach())));
        let squeeze = (2.0 * SPREAD * WIDE / (hi - lo)).min(1.0);
        let mid = 0.5 * (lo + hi);
        for l in &mut logs {
            l.x = WIDE * 0.5 + (l.x - mid) * squeeze;
            let room = l.half * l.yaw.sin().abs() + l.r + 1.0;
            l.z = l.z.clamp(room.min(grid.depth * 0.5), (grid.depth - room).max(grid.depth * 0.5));
        }
        let bed_cells = bed_cells(&logs, &grid);
        let ramp = Ramp::new();
        let (fire_theta, fire_l) = fire_steps(&ramp);
        FlameClock {
            grid,
            sim: None,
            follow: false,
            spare: Vec::new(),
            camera: Camera::new(&grid),
            sparks: vec![[0.0; 4]; EMBERS * 4],
            bounces_read: [[0.0; 4]; EMBERS],
            bounces: Bounces::default(),
            pending: Vec::new(),
            spawns: Vec::new(),
            spark_dice: Rng::new(rng.u64()),
            logs,
            salt,
            dice: Rng::new(rng.u64()),
            t: 0.0,
            steps: 0,
            step_no: 0,
            clock: 0.0,
            warm: false,
            size: 0.5,
            soot: 0.28,
            draft: 0.0,
            flue: 13.0,
            headroom: 0.0,
            gas_t: 0.0,
            wood: 0.0,
            expansion: 1.0,
            gust: None,
            calm_until: 20.0,
            settle: None,
            bed: vec![[0.0; 4]; grid.gw() * grid.bed_rows() * grid.gd()],
            bed_cells,
            wind: [[0.0; 4]; 40],
            ramp,
            fire_l,
            fire_theta,
            palette: None,
            steady: None,
            gusts_seen: 0,
            settles_seen: 0,
            sparks_thrown: 0,
            fixed: Fixed::PINNED,
            glyph: [[0; 4]; 16],
            clock_slab: [0.0; 4],
            numerals: [2.0, 0.0, 0.0, 0.0],
            logs_burn: 1.0,
            jet: [0.0; 4],
            showing: Showing::Standing,
            // The fire alone until the clock has looked at the time.
            shown: 0.0,
            cycling: false,
        }
    }

    /// The uniform block for a step at the current eased size: the physical
    /// constants (cells and seconds, tuned by eye), the camera, the wind.
    fn params(&self) -> Params {
        let s = self.size / OLD_MAX;
        // Card 372's cooling curve up to its old maximum, and on down past
        // it for the headroom: a hotter, taller fire.
        let (cool4, cool1) = if s <= 1.0 {
            (0.9 + 1.5 * (1.0 - s) * (1.0 - s), 0.7 + 2.3 * (1.0 - s) * (1.0 - s))
        } else {
            // Floors keep the very top (card 375: sliders to 2) a fire, not a
            // furnace that never cools.
            ((0.9 - 0.3 * (s - 1.0)).max(0.25), (0.7 - 0.25 * (s - 1.0)).max(0.15))
        };
        let c = &self.camera;
        let scale = |v: [f32; 3], k: f32| [v[0] * k, v[1] * k, v[2] * k, 0.0];
        let g = &self.grid;
        // Lengths and speeds go to grid cells (times k); what happens per
        // grid cell along a ray (emission, soot, lit smoke) by 1/k.
        let k = g.k;
        let [gw, gh, gd] = g.dims();
        Params {
            dims: [gw, gh, gd, self.step_no as u32],
            coarse: [gw / 4, gh / 4, gd / 4, EMBERS as u32],
            // dt, buoyancy, smoke weight, drag
            a: [STEP as f32, 95.0 * k, 1.0 * k, 0.9],
            // wind grip, confinement, burn, fuel heat
            b: [1.4, 5.0 * k, (2.0 - 0.6 * s).max(0.5), 1.5],
            // Size is mostly how fast the gas cools: how far it rises still
            // glowing. cool4, cool1, expansion, smoke yield.
            c: [cool4, cool1, 1.5 * self.expansion, 0.5],
            // smoke fade, smoke absorption, smoke lighting, emission
            // (Lit smoke is not divided by k: a coarser grid already smears
            // smoke wider, and the wider wisps would show as more grey.)
            d: [0.55, self.soot / k, 1.0, 0.35 / k],
            eye: [c.eye[0], c.eye[1], c.eye[2], g.samples as f32],
            right: scale(c.right, c.tan_w),
            up: scale(c.up, c.tan_h),
            fwd: [c.fwd[0], c.fwd[1], c.fwd[2], g.march],
            stove: [self.draft, ((CEILING + 3.0 * self.headroom) * k).round(), self.flue * k, k],
            logs: [g.bed_rows() as f32, 0.0, 0.0, 0.0],
            wind: self.wind,
            clock: self.clock_slab,
            glyph: self.glyph,
            numerals: [self.numerals[0], self.numerals[1], self.gas_t as f32, self.numerals[3]],
            jet: self.jet,
        }
    }

    /// The time, from `ctx.now`, as the solid the next steps burn round.
    ///
    /// Once a minute, around the minute turning: a jet lights the time
    /// `before` seconds ahead; at the minute, a jet under the digits that
    /// change shoots up, the old ones collapse and the new ones catch in
    /// their place; `after` seconds on, it collapses into the fire (as a pile
    /// of fuel does, [`clock::Shape::melt`]) while a surge comes up through
    /// it and the woodpile takes over, and the fire burns alone until the
    /// next minute.
    fn tell_time(&mut self, ctx: &Ctx) {
        let before = f64::from(ctx.get("before"));
        let after = f64::from(ctx.get("after"));
        let hours24 = ctx.get("hours24") > 0.5;
        let middle_row = (1.0 - ctx.get("height")) * H as f32;
        let font = clock::Font { height: ctx.get("digits"), weight: ctx.get("weight") };
        let p = ctx.now.rem_euclid(60.0);
        let minute = ctx.now - p;
        let lead = before + EMERGE;
        let catch = |t: f64, from: f64, to: f64| smoothstep(0.0, 1.0, ((t - from) / (to - from)).clamp(0.0, 1.0) as f32);
        // Which collapse: a new one each minute.
        let salt = self.salt ^ ((minute / 60.0) as u32).wrapping_mul(0x9e37);
        let new = clock::text(minute, hours24);
        let old = clock::text(minute - 60.0, hours24);
        // The digits the minute changes; all of them if the time changes
        // length (12-hour, 9:59 to 10:00), since it moves.
        let same_len = old.chars().count() == new.chars().count();
        let changed: Vec<bool> = new.chars().zip(old.chars()).map(|(a, b)| !same_len || a != b).collect();
        let whole = clock::Shape::WHOLE;
        let (showing, glyph, at, phase) = if p >= 60.0 - lead {
            // Before the minute: the time as it is, lit by the jet.
            let q = p - (60.0 - lead);
            let shape = clock::Shape { reveal: catch(q, CATCH, EMERGE), ..whole };
            let (g, at) = clock::mask(&clock::text(minute, hours24), middle_row, font, &[shape]);
            (if q < EMERGE { Showing::Emerging } else { Showing::Standing }, g, at, q)
        } else if p < f64::from(clock::COLLAPSE) {
            // The minute turns: the old digits that change fall, the new
            // ones catch where they stood.
            let gone: Vec<_> = changed.iter().map(|&c| if c { clock::Shape { melt: p as f32 / clock::COLLAPSE, salt, ..whole } } else { whole }).collect();
            let coming: Vec<_> = changed.iter().map(|&c| clock::Shape { reveal: if c { catch(p, MORPH_CATCH.0, MORPH_CATCH.1) } else { 0.0 }, ..whole }).collect();
            let (a, _) = clock::mask(&old, middle_row, font, &gone);
            let (b, at) = clock::mask(&new, middle_row, font, &coming);
            (Showing::Morphing, clock::both(&a, &b), at, p)
        } else if p < after {
            let (g, at) = clock::mask(&new, middle_row, font, &[whole]);
            (Showing::Standing, g, at, p)
        } else if p < after + MELT {
            let m = ((p - after) / MELT) as f32;
            let (g, at) = clock::mask(&new, middle_row, font, &[clock::Shape { melt: m, salt, ..whole }]);
            (Showing::Melting, g, at, p - after)
        } else {
            let (g, at) = clock::mask(&new, middle_row, font, &[clock::Shape { reveal: 0.0, ..whole }]);
            (Showing::Burning, g, at, p)
        };
        self.showing = showing;
        self.glyph = glyph;
        let gd = self.grid.gd() as f32;
        let half = 0.5 * ctx.get("depth") * gd;
        self.clock_slab = [0.5 * gd - half, 0.5 * gd + half, 0.0, 0.0];
        let burn = ctx.get("fuel");
        // The woodpile: `logs` while the time shows, `fire` between.
        let shown = match showing {
            // The woodpile burns on while the numerals catch, so the fire
            // narrows into them rather than going out first.
            Showing::Emerging => catch(phase, CATCH, EMERGE),
            Showing::Standing | Showing::Morphing => 1.0,
            // The fuel goes into the woodpile as it falls out of the frame.
            Showing::Melting => 1.0 - at.fallen,
            Showing::Burning => 0.0,
        };
        self.logs_burn = self.fixed.logs * shown + self.fixed.fire * (1.0 - shown);
        self.shown = shown;
        self.cycling = true;
        // The fuel holds while it falls, and fails as it goes.
        let fuel_share = match showing {
            Showing::Melting => 1.0 - 0.7 * smoothstep(0.5, 1.0, (phase / MELT) as f32),
            Showing::Burning => 0.0,
            _ => 1.0,
        };
        // The jets: from the bed up, across what they light. Columns and
        // rows by the frame at the middle depth: 3 design units an LED.
        let k = self.grid.k;
        let foot = BELOW + 3.0 * (H as f32 - at.bottom as f32);
        let middle = BELOW + 3.0 * (H as f32 - 0.5 * (at.top + at.bottom) as f32);
        // A jet's rise and fall over `length` seconds from its start.
        let envelope = |t: f64, length: f64| {
            let t = t as f32;
            smoothstep(0.0, 0.25, t) * (1.0 - smoothstep(length as f32 - 0.4, length as f32, t))
        };
        // The surge as the time collapses (2026-10-26: "can we have a random
        // surge come up from below at the same time?"): each minute its own -
        // when in the collapse, how wide, where, how hard.
        let mut dice = Rng::new(u64::from(salt) ^ (minute as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let surge_at = f64::from(dice.range(0.0, SURGE_LATEST));
        let surge_width = dice.range(6.0, 14.0);
        let surge_mid = at.left as f32 + dice.f32() * (at.right - at.left) as f32;
        let surge_strength = dice.range(0.8, 1.6);
        // The digits that change, as columns, for the morph's jet.
        let spans = clock::spans(&new, font);
        let morph_cols = spans.iter().zip(&changed).filter(|(_, &c)| c).fold((W as f32, 0.0f32), |(l, r), (&(x, w), _)| (l.min(x as f32), r.max((x + w) as f32)));
        // The jets are timed on the wall clock, but the gas runs `speed`
        // times faster: at 5, the unscaled jet filled the whole frame.
        let fast = ctx.get("speed").max(1.0);
        let (fuel, lift, from, to, top) = match showing {
            Showing::Emerging if phase < JET => {
                let e = envelope(phase, JET);
                (e / fast.sqrt(), e / fast, at.left as f32, at.right as f32, foot)
            }
            // The morph's jet shoots up into the changing digits, to their
            // middle, as the old ones fall and the new ones catch.
            Showing::Morphing if phase < MORPH_JET && morph_cols.1 > morph_cols.0 => {
                let e = envelope(phase, MORPH_JET);
                (e / fast.sqrt(), e / fast, morph_cols.0, morph_cols.1, middle)
            }
            Showing::Melting if phase >= surge_at && phase < surge_at + SURGE => {
                let e = surge_strength * envelope(phase - surge_at, SURGE);
                let (from, to) = (surge_mid - surge_width / 2.0, surge_mid + surge_width / 2.0);
                (SURGE_FUEL * e, e / fast, from.max(0.0), to.min(W as f32), middle)
            }
            _ => (0.0, 0.0, 0.0, 0.0, foot),
        };
        self.jet = if fuel > 0.0 && burn > 0.0 {
            [JET_FUEL * fuel, JET_LIFT * k * lift, 3.0 * from * k, 3.0 * to * k]
        } else {
            [0.0; 4]
        };
        self.numerals = [0.0, burn * fuel_share, 0.0, top * k];
    }

    /// A fire knob that is one thing while the time shows and another
    /// between, handed over as it collapses.
    fn knob(&self, while_shown: f32, between: f32) -> f32 {
        while_shown * self.shown + between * (1.0 - self.shown)
    }

    /// The fire breathing by itself: slow swells and lulls over minutes.
    fn breath(&self) -> f32 {
        let t = self.t as f32;
        1.0 + 0.12 * noise::fbm1(t / 75.0, self.salt ^ 0xb4ea) + 0.06 * noise::fbm1(t / 23.0, self.salt ^ 0x77)
    }

    /// How strongly log `k` is burning now, before size and breath.
    fn log_strength(&self, k: usize) -> f32 {
        let log = &self.logs[k];
        let t = self.t as f32;
        let mut s = 0.85 + 0.3 * noise::noise1(t / 17.0 + log.phase, log.salt);
        if let Some(settle) = &self.settle {
            if settle.log == k {
                let age = (self.t - settle.start) as f32;
                s += 1.3 * smoothstep(0.0, 0.35, age) * (-(age / 2.2)).exp();
            }
        }
        s
    }

    /// One fixed step of everything.
    fn step(&mut self, ctx: &Ctx) {
        let dt = STEP as f32;
        // How strongly soot absorbs the flame behind it: the smoke slider.
        self.soot = 0.08 + 0.25 * ctx.get("smoke");
        let target = self.knob(ctx.get("flames"), ctx.get("size"));
        // A cycling clock changes size with its phases: in a second or two,
        // not hearth's tens of seconds.
        let ease = if self.cycling { TIME_EASE } else { SIZE_EASE };
        self.size += (target - self.size) * (1.0 - (-dt / ease).exp());
        self.events(ctx);

        // Fuel from the logs (exp/hearth-speed): the gas comes off their
        // surfaces - the tops most, the undersides least, the cut ends hard,
        // and hardest in the hot gaps where two logs nearly touch - so the
        // flames wrap the logs and climb out of the gaps between them. The
        // logs are solid: the gas goes round them. Noise drifting in gas
        // time breaks the release into tongues; `wood` into pockets. Where
        // gas comes off, the bed holds it at a fuel *level* (see card 372's
        // Log: a fuel rate is far too touchy). The logs glow whatever the
        // flames are doing.
        let t = self.t as f32;
        let gt = self.gas_t as f32;
        let wood = self.wood;
        let breath = self.breath();
        let strengths: Vec<f32> = (0..self.logs.len()).map(|k| self.log_strength(k)).collect();
        let lo = 0.55 - 0.45 * self.size / OLD_MAX;
        for c in &self.bed_cells {
            let [xf, yf, zf] = c.p;
            let shimmer = 0.85 + 0.3 * noise::noise3(xf / 5.0, zf / 5.0, t * 0.3, self.salt ^ 0xc0a1);
            if c.inside {
                self.bed[c.at] = [0.0, 0.7 * shimmer * self.logs_burn, 1.0, 0.0];
                continue;
            }
            let release = (c.near.iter().map(|&(i, w)| strengths[i] * w).fold(0.0, f32::max) + c.gap).min(1.6);
            if release < 0.02 {
                self.bed[c.at] = [0.0; 4];
                continue;
            }
            let tongues = (0.2 + 1.8 * noise::fbm3(xf / 7.0, zf / 7.0 + yf / 5.0, gt * 0.7, self.salt)).max(0.0).powf(1.2);
            // Wood (experiment): the gas comes out of the logs in pockets -
            // a crack, an end, a hot gap - each swelling, burning hard and
            // dying in a second or two of gas time, not as a burner's
            // steady sheet.
            let pocket = smoothstep(0.05, 0.45, noise::fbm3(xf / 9.0, zf / 6.0 + yf / 6.0, gt * 0.9, self.salt ^ 0x9e3d));
            let wood_release = 1.0 - wood + wood * 1.6 * pocket;
            // Held thinner toward the shell's edge: the surfaces and gaps
            // together give off far more than card 372's sheet otherwise.
            let level = 1.5 * smoothstep(lo, lo + 0.6, release * (0.35 + tongues) * breath * wood_release) * release.min(1.0);
            let level = level * self.logs_burn;
            self.bed[c.at] = [level, 0.6 * release.min(1.0) * shimmer * self.logs_burn, 0.0, 0.0];
        }
        let k = self.grid.k;

        // The air: a calm wandering drift, and a gust when one is blowing,
        // stronger higher up and weakest down among the logs.
        // In a stove the walls stop any wind: a gust is the chimney drawing
        // harder for a few seconds (the stove breathing), and the air only
        // wanders a little.
        let draft = ctx.get("draft");
        let stove = draft > 0.0;
        let gh = self.grid.gh();
        let drift = 3.0 * noise::fbm1(t / 21.0, self.salt ^ 0xd1f7) * if stove { 0.3 } else { 1.0 };
        let (gust, surge) = self.gust.as_ref().map_or((0.0, 0.0), |g| {
            let a = ((self.t - g.start) / g.length).clamp(0.0, 1.0) as f32;
            let env = (std::f32::consts::PI * a).sin().powi(2) * (1.0 + 0.25 * noise::fbm1(t * 0.9, self.salt ^ 0x6057));
            (g.peak * env, env * g.peak.abs() / 18.0)
        });
        let gust = if stove { 0.0 } else { gust };
        for y in 0..gh.min(160) {
            let yd = y as f32 / k;
            self.wind[y / 4][y % 4] = k * (drift + gust) * (0.1 + 0.9 * smoothstep(BELOW, TALL * 0.6, yd));
        }
        self.draft = if stove { draft * (1.0 + 0.9 * surge) } else { 0.0 };
        self.flue = 6.0 + 18.0 * self.fixed.flue;
        self.headroom = self.fixed.headroom;
        self.wood = self.fixed.wood;
        self.expansion = self.fixed.expansion;
        self.gas_t += STEP * f64::from(ctx.get("speed").max(1.0));

        let params = self.params();
        if let Some(Some(sim)) = &self.sim {
            for (slot, spark) in self.spawns.drain(..) {
                sim.spawn(slot, spark);
            }
            // `speed` seconds of gas per second: in substeps of at most
            // BIG fixed steps each (a 16x fire is 16 substeps at BIG 1).
            let (n, dt) = substeps(ctx.get("speed"));
            let mut params = params;
            params.a[0] = dt;
            sim.bed(&self.bed);
            for _ in 0..n {
                sim.step(&params);
                self.step_no += 1;
                params.dims[3] = self.step_no as u32;
            }
            self.step_no -= 1;
        }
        self.t += STEP;
        self.steps += 1;
        self.step_no += 1;
    }

    /// Throw a spark up from `(x, z)` on the bed into a free slot, if there
    /// is one. `vigour` in `0..1` is how hard its fire is burning.
    /// A settle's `burst` throws its sparks harder and wider: those are
    /// the ones that reach the firebox walls (card 376).
    fn spawn(&mut self, [x, y, z]: [f32; 3], vigour: f32, burst: bool) {
        self.sparks_thrown += 1;
        let free = (0..EMBERS).find(|k| self.sparks[4 * k][3] <= EMBER_OUT && !self.pending.contains(k));
        let Some(slot) = free else { return };
        let r = &mut self.spark_dice;
        let k = self.grid.k;
        let pos = [x + r.range(-3.0, 3.0), y, z + r.range(-2.0, 2.0)].map(|v| v * k);
        let heat = r.range(0.88, 1.0);
        let wide = if burst { 90.0 } else { 12.0 };
        let v = [r.range(-wide, wide), r.range(30.0, 55.0) * (0.7 + 0.5 * vigour), r.range(-6.0, 6.0)].map(|v| v * k);
        // A burst's sparks are flagged in the cooling slot (plus 1): the air
        // grips them less, so they carry their throw to the walls.
        let cooling = r.range(0.16, 0.30) + if burst { 1.0 } else { 0.0 };
        self.pending.push(slot);
        // Until the next read-back says otherwise, the slot is this spark.
        self.sparks[4 * slot] = [pos[0], pos[1], pos[2], heat];
        self.sparks[4 * slot + 2] = [pos[0], pos[1], pos[2], 0.0];
        self.sparks[4 * slot + 3] = [0.0; 4];
        self.bounces_read[slot] = [0.0; 4];
        self.spawns.push((slot, [[pos[0], pos[1], pos[2], heat], [v[0], v[1], v[2], cooling], [pos[0], pos[1], pos[2], 0.0], [0.0; 4]]));
    }

    /// The dice: gusts, log settles, and single sparks.
    fn events(&mut self, ctx: &Ctx) {
        let dt = STEP;
        let gusts = self.knob(self.fixed.gusts, ctx.get("gusts")) / OLD_MAX;
        if self.gust.as_ref().is_some_and(|g| self.t > g.start + g.length) {
            self.gust = None;
        }
        // A mean wait of four minutes near 0, one every half minute at card
        // 372's maximum (now the default), and more often above it; none at
        // all when it is turned right off.
        if gusts > 0.0 && self.gust.is_none() && self.t > self.calm_until {
            let mean = 240.0 * (30.0f64 / 240.0).powf(f64::from(gusts));
            if f64::from(self.dice.f32()) < dt / mean {
                let peak = self.dice.range(11.0, 18.0) * (0.6 + 0.8 * gusts) * self.dice.sign();
                let length = f64::from(self.dice.range(4.5, 9.0));
                self.gust = Some(Gust { start: self.t, length, peak });
                self.calm_until = self.t + length + 15.0 / f64::from(gusts.max(1.0));
                self.gusts_seen += 1;
            }
        }

        // A log settles now and then - a short flare and a handful of sparks.
        if self.settle.as_ref().is_some_and(|s| self.t > s.start + 8.0) {
            self.settle = None;
        }
        // A mean wait of 150 s at the default, about 100 s at the top.
        let embers = intensity(ctx, "embers");
        let settle_mean = 150.0 / (0.3 + 0.7 * f64::from(embers));
        if self.settle.is_none() && f64::from(self.dice.f32()) < dt / settle_mean && self.t > 4.0 {
            let log = (self.dice.u64() % self.logs.len() as u64) as usize;
            let sparks = 3 + (embers * 4.0).round() as u32;
            self.settle = Some(Settle { start: self.t, log, sparks, next_spark: self.t + 0.25 });
            self.settles_seen += 1;
        }
        let mut burst = None;
        if let Some(s) = &mut self.settle {
            if s.sparks > 0 && self.t >= s.next_spark {
                s.sparks -= 1;
                s.next_spark = self.t + f64::from(self.dice.range(0.05, 0.25));
                burst = Some(self.logs[s.log].top(self.dice.range(-0.5, 0.5), 2.0));
            }
        }
        if let Some(at) = burst {
            self.spawn(at, 1.0, true);
        }

        // Single sparks: on average one every six seconds or so at the
        // default, fewer from a small fire.
        let vigour = (0.25 + self.size / OLD_MAX).min(1.6) * self.breath();
        let rate = f64::from(embers * 0.42 * vigour);
        if f64::from(self.dice.f32()) < dt * rate {
            let weights: Vec<f32> = (0..self.logs.len()).map(|k| self.log_strength(k)).collect();
            let total: f32 = weights.iter().sum();
            let mut pick = self.dice.f32() * total;
            let mut which = 0;
            for (k, w) in weights.iter().enumerate() {
                which = k;
                if pick < *w {
                    break;
                }
                pick -= w;
            }
            let at = self.logs[which].top(self.dice.range(-0.6, 0.6), 2.0);
            self.spawn(at, vigour.min(1.0), false);
        }
    }

    fn advance(&mut self, ctx: &Ctx) {
        if !self.warm {
            // The fire starts at its size, not eased up to it from 0.5.
            self.warm = true;
            self.size = self.knob(ctx.get("flames"), ctx.get("size"));
        }
        self.clock += ctx.dt.clamp(0.0, 0.25);
        let target = (self.clock / STEP + 1e-6).floor() as i64;
        // A fast fire catches up less: each step is several on the GPU, and
        // catching up a stall must not cause the next one.
        let catchup = (CATCHUP / substeps(ctx.get("speed")).0 as i64).max(1);
        self.steps = self.steps.max(target - catchup);
        while self.steps < target {
            self.step(ctx);
        }
    }

    /// An ember as a soft, sub-pixel point, smeared along the last step's
    /// motion so it never twinkles as it crosses an LED boundary. `spark` is
    /// its three slot vec4s.
    fn draw_ember(&self, px: &mut [Rgb], spark: &[[f32; 4]]) {
        const SIGMA: f32 = 0.3;
        const TAPS: usize = 3;
        const SUB: usize = 3;
        let (now, prev) = (spark[0], spark[2]);
        // The peak LED reads about as bright as gas at the ember's heat.
        let colour = self.ramp.emit(now[3]).scale(1.4 / TAPS as f32);
        for k in 0..TAPS {
            let f = (k as f32 + 0.5) / TAPS as f32;
            let at = [0, 1, 2].map(|c| prev[c] + (now[c] - prev[c]) * f);
            let Some((cx, cy)) = self.camera.project(at) else { continue };
            let (x0, x1) = ((cx - 2.0).floor().max(0.0) as usize, ((cx + 2.0).ceil().max(0.0) as usize).min(W));
            let (y0, y1) = ((cy - 2.0).floor().max(0.0) as usize, ((cy + 2.0).ceil().max(0.0) as usize).min(H));
            for ly in y0..y1 {
                for lx in x0..x1 {
                    let mut cover = 0.0;
                    for sy in 0..SUB {
                        for sx in 0..SUB {
                            let dx = lx as f32 + (sx as f32 + 0.5) / SUB as f32 - cx;
                            let dy = ly as f32 + (sy as f32 + 0.5) / SUB as f32 - cy;
                            cover += (-(dx * dx + dy * dy) / (2.0 * SIGMA * SIGMA)).exp();
                        }
                    }
                    let c = &mut px[ly * W + lx];
                    *c = c.add(colour.scale(cover / (SUB * SUB) as f32));
                }
            }
        }
    }

    /// The palette for `color` and the `dark` choice, with the lightness of
    /// each fire entry (what the painter matches the fire's light against)
    /// and how many soot entries follow the fire's.
    pub(crate) fn palette(&mut self, color: f32, dark: usize) -> Painting {
        if let Some(p) = &self.palette {
            if p.color == color && p.dark == dark {
                return p.clone();
            }
        }
        let desaturate = |c: Rgb| {
            let lab = to_oklab(c);
            let (ch, h) = ((lab[1] * lab[1] + lab[2] * lab[2]).sqrt(), lab[2].atan2(lab[1]).to_degrees());
            oklch(lab[0], ch * color, h)
        };
        let mut colours = vec![Rgb::BLACK];
        let mut fire_l = self.fire_l;
        let soot = match dark.checked_sub(1).and_then(|d| DARK_RAMPS.get(d)) {
            None => {
                for theta in self.fire_theta {
                    colours.push(desaturate(self.ramp.emit(theta)));
                }
                // Soot: neutral greys, never the flame's hue (card 373).
                for l in SMOKE_L {
                    colours.push(oklch(l, 0.0, 0.0));
                }
                SMOKE_STEPS
            }
            Some(ramp) => {
                // Snapped last: a desaturated entry goes back onto levels.
                for (k, triple) in ramp.iter().enumerate() {
                    let c = on_levels(desaturate(crate::panel::level_triple(*triple)));
                    fire_l[k] = to_oklab(c)[0];
                    if k > 0 {
                        fire_l[k] = fire_l[k].max(fire_l[k - 1] + 1e-4);
                    }
                    colours.push(c);
                }
                for triple in SOOT_TRIPLES {
                    colours.push(crate::panel::level_triple(triple));
                }
                SOOT_TRIPLES.len()
            }
        };
        let p = Painting { color, dark, colours, fire_l, soot };
        self.palette = Some(p.clone());
        p
    }

    /// The frame, by palette index: fire where there is fire, smoke where
    /// there is lit smoke and no fire, black everywhere else. `leds` is the
    /// march's (fire light rgb, lit smoke) per LED.
    fn paint(&mut self, ctx: &Ctx, leds: &[[f32; 4]]) -> Frame {
        let (leds, bare) = leds.split_at(N);
        let mut light: Vec<Rgb> = leds.iter().map(|l| Rgb::new(l[0], l[1], l[2])).collect();
        // How much of each LED's fire the soot in front of it has taken.
        let soot: Vec<f32> =
            light.iter().zip(bare).map(|(c, b)| if b[0] > 1e-4 { (1.0 - c.luma() / b[0]).clamp(0.0, 1.0) } else { 0.0 }).collect();
        for k in 0..EMBERS {
            if self.sparks[4 * k][3] > EMBER_OUT {
                let spark = self.sparks[4 * k..4 * k + 3].to_vec();
                self.draw_ember(&mut light, &spark);
            }
        }
        let amount = ctx.get("smoke");
        let dark = self.fixed.dark.min(DARK_CHOICES.len() - 1);
        let Painting { colours: palette, fire_l, soot: soot_steps, .. } = self.palette(ctx.get("color"), dark);
        let dither = Dither::BlueNoise;
        let paint = |gain: f32| -> Vec<u8> {
            (0..N)
                .map(|i| {
                    let thr = dither.threshold(i % W, i / W);
                    let c = light[i];
                    let l = to_oklab(Rgb::new(c.r * gain, c.g * gain, c.b * gain))[0];
                    let p = fire_position(&fire_l, l);
                    // Flame seen through thick soot goes to black, not to the
                    // ramp's dark reds: smoke on this panel is a silhouette.
                    if soot[i] > SOOT_BLACK && p < 0.5 * FIRE_STEPS as f32 {
                        return 0;
                    }
                    if p >= 0.35 {
                        return (p + 0.5 + thr).floor().clamp(1.0, FIRE_STEPS as f32) as u8;
                    }
                    let q = amount * SMOKE_GAIN * leds[i][3] * SMOKE_STEPS as f32;
                    if q >= 0.6 && p < 0.25 {
                        // Spread over the soot entries this ramp has; below its
                        // first entry, dithered against black.
                        let q = q * soot_steps as f32 / SMOKE_STEPS as f32;
                        let k = (q + 0.5 + thr).floor().clamp(0.0, soot_steps as f32) as usize;
                        return if k == 0 { 0 } else { (FIRE_STEPS + k) as u8 };
                    }
                    0
                })
                .collect()
        };
        let level = |indices: &[u8]| indices.iter().map(|k| palette[*k as usize].luma()).sum::<f32>() / N as f32;
        // Brief: no full-field flashing above 3 Hz, built in as a limiter.
        // A fast fire's whole frame swings (35% of its mean at speed 18), so
        // the painted frame is held within STEADY of its own low-passed
        // level: the flames still flicker, the panel as a whole does not.
        // The level is not proportional to the light (the ramp is steeper)
        // but rises with it, so the gain is found by halving.
        let mut indices = paint(1.0);
        let now = level(&indices);
        let steady = self.steady.map_or(now, |s| s + (now - s) * (1.0 - (-ctx.dt as f32 / STEADY_TAU).exp()));
        self.steady = Some(steady);
        let (low, high) = (steady * (1.0 - STEADY), steady * (1.0 + STEADY));
        if now > high || now < low {
            let target = now.clamp(low, high);
            let (mut lo, mut hi) = if now > high { (0.0f32, 1.0f32) } else { (1.0, 4.0) };
            let mut best = (f32::MAX, indices);
            for _ in 0..8 {
                let g = 0.5 * (lo + hi);
                let next = paint(g);
                let l = level(&next);
                if l > target {
                    hi = g;
                } else {
                    lo = g;
                }
                if (l - target).abs() < best.0 {
                    best = ((l - target).abs(), next);
                }
            }
            indices = best.1;
        }
        Frame::Indexed { palette, indices }
    }

    /// For tests: how many embers are alive.
    #[cfg(test)]
    pub(crate) fn ember_count(&self) -> usize {
        (0..EMBERS).filter(|k| self.sparks[4 * k][3] > EMBER_OUT).count()
    }

    #[cfg(test)]
    pub(crate) fn eased_size(&self) -> f32 {
        self.size
    }
}

/// A colour moved onto the panel's levels, channel by channel: the nearest
/// level, and the lowest code that reaches it (brief 2.1.1).
fn on_levels(c: Rgb) -> Rgb {
    let snap = |v: f32| {
        let code = crate::color::linear_to_srgb8(v);
        let (level, _) = screeny_panel::model::nearest_level(screeny_panel::model::duty_16ths(code));
        crate::color::srgb8_to_linear(crate::panel::level_code(level))
    };
    Rgb::new(snap(c.r), snap(c.g), snap(c.b))
}

/// Sparks against the firebox (card 376), counted as the read-backs show
/// them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bounces {
    pub walls: u32,
    pub ceiling: u32,
    /// Sparks that went up the flue.
    pub flue: u32,
}

/// A palette as the painter uses it (card 375).
#[derive(Clone)]
pub(crate) struct Painting {
    color: f32,
    dark: usize,
    pub(crate) colours: Vec<Rgb>,
    /// Each fire entry's lightness, darkest first.
    fire_l: [f32; FIRE_STEPS],
    /// How many soot entries follow the fire's.
    pub(crate) soot: usize,
}

/// Where lightness `l` falls along the fire ramp: 0 is black, 1 the first
/// entry, `FIRE_STEPS` the last, fractional in between.
fn fire_position(fire_l: &[f32; FIRE_STEPS], l: f32) -> f32 {
    if l <= fire_l[0] {
        return l / fire_l[0];
    }
    for k in 1..FIRE_STEPS {
        if l < fire_l[k] {
            return k as f32 + (l - fire_l[k - 1]) / (fire_l[k] - fire_l[k - 1]);
        }
    }
    FIRE_STEPS as f32
}

/// The fire ramp's entries: temperatures at even OKLab lightness from
/// [`FIRE_L0`] up to the hottest gas.
fn fire_steps(ramp: &Ramp) -> ([f32; FIRE_STEPS], [f32; FIRE_STEPS]) {
    let l_of = |theta: f32| to_oklab(ramp.emit(theta))[0];
    let top = l_of(1.0);
    let mut thetas = [0.0; FIRE_STEPS];
    let mut ls = [0.0; FIRE_STEPS];
    for k in 0..FIRE_STEPS {
        let want = FIRE_L0 + (top - FIRE_L0) * k as f32 / (FIRE_STEPS - 1) as f32;
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..30 {
            let mid = 0.5 * (lo + hi);
            if l_of(mid) < want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        thetas[k] = hi;
        ls[k] = l_of(hi);
    }
    (thetas, ls)
}

impl Patch for FlameClock {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let k = if self.follow { k_for(ctx.get("speed")) } else { self.grid.k };
        if self.sim.is_none() && k != self.grid.k {
            // Nothing burning yet to carry over.
            self.set_grid(Grid { k, ..self.grid });
        }
        if self.sim.is_none() {
            self.sim = Some(match Gpu::shared() {
                Ok(gpu) => Some(Sim::new(gpu, self.grid.dims(), self.grid.bed_rows() as u32, self.grid.sweeps, &self.ramp.table())),
                Err(e) => {
                    eprintln!("screeny-art: flame-clock: {e}; rendering black");
                    None
                }
            });
        }
        if !matches!(self.sim, Some(Some(_))) {
            return Frame::black();
        }
        if k != self.grid.k {
            if let Ok(gpu) = Gpu::shared() {
                self.regrid(k, gpu);
            }
        }
        self.tell_time(ctx);
        self.advance(ctx);
        let params = self.params();
        let Some(Some(sim)) = &self.sim else { return Frame::black() };
        let Some((leds, sparks)) = sim.picture(&params) else { return Frame::black() };
        self.sparks = sparks;
        self.pending.clear();
        for k in 0..EMBERS {
            let now = self.sparks[4 * k + 3];
            let was = self.bounces_read[k];
            let more = |i: usize| (now[i] - was[i]).max(0.0) as u32;
            self.bounces.walls += more(1);
            self.bounces.ceiling += more(2);
            self.bounces.flue += more(3);
            self.bounces_read[k] = now;
        }
        self.paint(ctx, &leds)
    }

    fn playing(&self) -> Option<Playing> {
        let detail = if self.showing == Showing::Emerging {
            "the time flares up".to_string()
        } else if self.showing == Showing::Morphing {
            "the minute turns".to_string()
        } else if self.showing == Showing::Melting {
            "the time collapses".to_string()
        } else if self.settle.is_some() {
            "a log settles".to_string()
        } else if let Some(g) = &self.gust {
            if self.draft > 0.0 {
                "the chimney draws hard".to_string()
            } else {
                format!("a draught from the {}", if g.peak > 0.0 { "left" } else { "right" })
            }
        } else {
            "burning quietly".to_string()
        };
        Some(Playing { title: format!("a fire of {} logs", self.logs.len()), detail, actions: Vec::new(), notes: Vec::new() })
    }
}
