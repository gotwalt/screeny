//! Leaves: a few real 3D leaves tumbling through space under a low sun,
//! falling slowly - card 321's second pass, after the owner saw card 314's
//! flat, round blobs on the panel and said "the leaves really need to be 3d
//! rendered. It looks like cheap animation."
//!
//! **The flight** is [`leaf`]: gravity, drag and lift from each leaf's own
//! 3D attitude against the air it is actually flying through (`velocity -
//! wind`, from [`wind::Wind::at3`]), stepped on a fixed timestep exactly as
//! `flock` and card 314's 2D model both were, so a snapshot at any render
//! rate is the same picture. **The shape** is [`mesh`]: a leaf-shaped,
//! triangulated, gently cupped and curled surface, not a flat card. **The
//! light** is the GPU ([`gpu`]): one key light (the `sun` parameter), a real
//! depth buffer, heavy supersampling, and the two faces of the leaf reading
//! differently as it turns - because, per the owner's later direction on this
//! card, compute is not the constraint here, the picture is, and a GPU is
//! sitting there for exactly this.
//!
//! **The picture is calm on purpose.** A handful of leaves in the air at
//! once (`leaves`, default 3), true black behind them - the owner, 2026-09-26:
//! "we'll prefer foreground animations against a black backdrop... The
//! leaves are the only light in the frame" - so there is no sky, no ground
//! plane and no heightfield pile any more (card 314's dropped `scheme` and
//! `pile`): a leaf that lands simply stops and lies still, lit exactly as it
//! was falling, until either the `rest` cap bumps it back into the air to
//! make room for the next one to land, or a strong gust plucks it up in a
//! swirl (both reusing card 314's respawn-through-a-pending-queue shape,
//! `pending`/`respawn` below).
//!
//! **The frame is still indexed and exact** (brief section 2.3), the same
//! promise every patch here makes - but the route there changed with the
//! rendering: the GPU shades continuously (real Lambertian light, a
//! translucent backlit glow, a small specular glint - see `leaves.wgsl`),
//! and [`crate::palette::Palette`] snaps that down to a small, designed,
//! dithered palette afterwards, exactly as `knot` (the house example for a
//! GPU patch that must still go out exact) does it.

mod geom;
mod gpu;
mod landing;
mod leaf;
mod mesh;
mod shell;
mod wind;

use crate::color::smoothstep;
use crate::dither::Dither;
use crate::frame::Frame;
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use crate::rng::Rng;
use geom::{v3, Quat, V3};
use landing::{Landing, Litter, Touchdown};
use leaf::{Build, Leaf3D};
use std::collections::VecDeque;
use wind::Wind;

pub const DEF: PatchDef = PatchDef {
    id: "leaves",
    name: "Leaves",
    blurb: "A few real 3D leaves - a lofted, cupped, lit surface, not a flat blob - tumbling slowly through a low sun and coming to rest.",
    params: PARAMS,
    make,
    // The wind field and every leaf's build (shape, size, hue, cup, curl,
    // spin) and spawn moment come from the seed, so "another one like this"
    // is a different gust pattern and a different cast of leaves.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("leaves", "How many leaves are aloft at once", 1.0, 8.0, 1.0, 4.0),
    param("wind", "Mean wind: the steady push downwind", 0.0, 2.5, 0.05, 0.5),
    param("gusts", "How strong the gusts are, and how often they hit", 0.0, 1.5, 0.05, 0.5),
    param("flutter", "How much a falling leaf rocks, glides or tumbles", 0.0, 2.0, 0.05, 1.0),
    param(
        "rest",
        "How many spent leaves may lie still before the next landing bumps the oldest back into the air",
        0.0,
        6.0,
        1.0,
        2.0,
    ),
    param("sun", "Which way the light comes from, degrees around the scene", 0.0, 360.0, 1.0, 35.0),
    param("color", "Colour; 0 is grayscale", 0.0, 1.0, 0.01, 1.0),
];

/// Safety clamp matching the `leaves` param's own max.
const MAX_LEAVES: usize = 8;
/// Safety clamp matching the `rest` param's own max.
const MAX_REST: usize = 6;

const HUES: usize = 4;

/// A named autumn shade: hue, and how light the top (adaxial, "fresh") and
/// underside ("aged", paler and duller - card 321: "the underside paler and
/// duller") faces read before the light gets to them. Maple red, amber,
/// ochre, brown - card 314's own words, kept.
struct HueFamily {
    hue: f32,
    top_l: f32,
    bot_l: f32,
    chroma: f32,
}

const FAMILIES: [HueFamily; HUES] = [
    HueFamily { hue: 18.0, top_l: 0.80, bot_l: 0.58, chroma: 0.19 },
    HueFamily { hue: 64.0, top_l: 0.86, bot_l: 0.64, chroma: 0.16 },
    HueFamily { hue: 84.0, top_l: 0.82, bot_l: 0.60, chroma: 0.11 },
    HueFamily { hue: 42.0, top_l: 0.72, bot_l: 0.50, chroma: 0.10 },
];

/// The exact palette's shared lightness range and step count, and the
/// blue-noise dither strength - `knot`'s own pattern for bringing a
/// continuous GPU render inside the panel's colour budget (see
/// `crate::palette::Palette`'s doc). One shared range across all four hues:
/// the *shader* is what gives each family its own bright/dull anchors and
/// its lighting; this only has to cover the range that lighting reaches.
const PALETTE_STEPS: usize = 10;
const PALETTE_RANGE: (f32, f32) = (0.08, 0.88);
const PALETTE_CHROMA: f32 = 0.15;
const PALETTE_DITHER: f32 = 0.75;

// --------------------------------------------------------------------------
// The world: a fixed camera, a fall height, and per-depth framing so a leaf
// entering "at the top of the frame" and coming to rest "at the bottom" means
// that at any depth, near or far, rather than at one literal world height.
// --------------------------------------------------------------------------

/// Camera eye height, world metres. The fall range is framed symmetrically
/// around it (see [`top_y`]/[`ground_y`]).
const EYE_Y: f32 = 2.3;
/// Vertical field of view, degrees - picked by eye (see the Log) so a leaf's
/// fall reads as unhurried and the near/far size range is dramatic without
/// losing far leaves to a single pixel.
const VFOV_DEG: f32 = 30.0;
/// The panel's aspect ratio (64:32), the same number `gpu::mat::perspective`'s
/// own doc names.
const ASPECT: f32 = 2.0;
/// How much of the visible half-height/half-width a leaf's spawn and rest
/// planes use, short of the true edge, so nothing spawns or lands exactly on
/// the frame's boundary.
const TOP_MARGIN: f32 = 0.92;
/// How close to the visible frame's own bottom edge, at [`Z_NEAR`], the real
/// ground plane ([`ground_plane_y`]) sits - short of the true edge, so the
/// nearest litter never touches the very last row.
const GROUND_MARGIN: f32 = 0.88;
const X_MARGIN: f32 = 0.85;

pub(crate) const Z_NEAR: f32 = 3.0;
pub(crate) const Z_FAR: f32 = 15.0;

fn tan_half_vfov() -> f32 {
    (VFOV_DEG.to_radians() * 0.5).tan()
}

/// The world y a leaf falling at depth `z` enters from - just above the
/// visible frame at that depth.
fn top_y(z: f32) -> f32 {
    EYE_Y + z.max(0.5) * tan_half_vfov() * TOP_MARGIN
}

/// **The real ground: card 323.** A single, constant world height - not a
/// line that used to slide with a leaf's own depth (321/322's `ground_y(z)`,
/// which was really "the bottom of the visible frame at this leaf's depth",
/// never a floor at all - the owner's "the leaves motion really dies at the
/// bottom of the screen" was partly this: every leaf just stopped wherever
/// the frame's own edge happened to be for it). A level camera looking at a
/// real, constant-height plane already draws correct perspective on its own
/// (the classic "floor's horizon sits at eye height, the near edge of the
/// visible floor is at the bottom of frame, it recedes upward toward the
/// horizon with depth" picture) - no camera tilt needed, only a real plane.
///
/// The constant is chosen so that the near edge of the *visible* floor lands
/// almost exactly at [`Z_NEAR`] (short of it by [`GROUND_MARGIN`], the same
/// margin `ground_y` used to keep a leaf off the very last row): solving
/// "the ground plane's own screen position at `Z_NEAR` is the old formula's
/// value there" gives back exactly the old `ground_y(Z_NEAR)` expression.
/// Every leaf's own depth range (`Z_NEAR..Z_FAR`) then sits entirely beyond
/// that near edge, so the whole floor - near and far - stays inside the
/// frame's own bottom band, receding toward the horizon (screen-centre, eye
/// height) as depth grows, exactly the card's own picture.
pub(crate) fn ground_plane_y() -> f32 {
    EYE_Y - Z_NEAR * tan_half_vfov() * GROUND_MARGIN
}

/// Half the visible width at depth `z`.
pub(crate) fn half_width(z: f32) -> f32 {
    z.max(0.5) * tan_half_vfov() * ASPECT * X_MARGIN
}

// --------------------------------------------------------------------------

/// Longest a leaf may go without landing before it is recycled outright -
/// the "no leaf stuck aloft forever" backstop. Generous: this patch's leaves
/// fall slowly on purpose.
const MAX_ALOFT: f32 = 90.0;

/// How often the gust-relaunch condition is checked, and how long after one
/// relaunch before another may fire.
const GUST_CHECK_INTERVAL: f32 = 1.0;
const GUST_RELAUNCH_COOLDOWN: f32 = 7.0;
/// How strong the sampled wind has to be, near the ground, to count as the
/// "strong gust now and then" that plucks a resting leaf back up.
const GUST_RELAUNCH_THRESHOLD: f32 = 2.4;

/// How far above [`ground_plane_y`], in leaf half-lengths, a falling leaf's
/// landing physics ([`landing::Landing`]) takes over from its full
/// aerodynamic flight - one leaf-length of real fall-and-contact left for
/// the settle itself to resolve a tip, a skid or a flip, rather than handing
/// off already touching (which would have no room to look like anything)
/// or well above the ground (a visible, ownerless gap of plain falling
/// before the "landing" begins).
const LANDING_TRIGGER_LENGTHS: f32 = 1.0;

/// How long a leaf bumped off the `rest` cap takes to fade away once it
/// starts - the card's own "the oldest fade out slowly (over several
/// seconds)... nothing disappears instantly", replacing 321/322's instant
/// pop-and-relaunch on overflow (the "bump" the owner's wobble complaint
/// named directly).
const FADE_SECONDS: f32 = 3.0;

/// A litter leaf bumped off the `rest` cap, fading out rather than
/// disappearing: still drawn (blended, weight = its own remaining alpha)
/// until `vanish_at`.
struct Fading {
    litter: Litter,
    vanish_at: f32,
}

/// A landed leaf bumped out by the `rest` cap, or plucked up by a gust,
/// waiting for the next falling slot to become it - reusing the ordinary
/// respawn point rather than growing the flock keeps the number of leaves
/// actually falling exactly `leaves` at all times (card 314's `Relaunch`/
/// `pending` shape, kept).
type Pending = Leaf3D;

struct LeavesPatch {
    rng: Rng,
    wind: Wind,
    /// This patch instance's own GPU pipeline and render target - not a
    /// shared/global one, so two players running `leaves` at once (the
    /// studio may do that) never fight over the same buffers. Outer
    /// `Option`: "have we tried to open it yet"; inner: "did it work" -
    /// `knot`'s exact pattern, since it is a mesh-based GPU patch with the
    /// same question to answer.
    gpu: Option<Option<gpu::Live>>,
    /// Simulated seconds asked for so far, and steps actually taken - see
    /// [`LeavesPatch::advance`] and `flock`'s identical pattern.
    warped: f64,
    steps: i64,
    falling: Vec<Leaf3D>,
    /// Card 322: each falling leaf's own soft-body shell
    /// ([`shell::LeafShell`]) - index-parallel with `falling`. Every place
    /// that mutates one mutates the other in lockstep (`resize`, `respawn`,
    /// the landed and gust-pluck paths in `step_once`), so `falling[i]`'s
    /// rigid attachment and `shells[i]`'s flexing blade are always the same
    /// leaf.
    shells: Vec<shell::LeafShell>,
    /// Card 323: leaves whose shell has reached [`ground_plane_y`] and are
    /// mid-settle in [`landing::Landing`]'s own small rigid-body world -
    /// skidding, tipping or flipping toward a real rest, not yet frozen.
    landing: Vec<Landing>,
    /// Leaves that have actually finished settling: an immutable, baked
    /// [`Litter`] each - no physics ever runs on one again, so it cannot
    /// jitter even in principle.
    resting: VecDeque<Litter>,
    /// Litter bumped off the `rest` cap, fading rather than popping (see
    /// [`Fading`]'s own doc) - almost always at most one at a time, at this
    /// patch's calm landing rate; a small `VecDeque` rather than an
    /// `Option` so an unlucky cluster of landings is still handled correctly
    /// rather than silently dropping one's fade.
    fading: VecDeque<Fading>,
    pending: VecDeque<Pending>,
    gust_clock: f32,
    last_relaunch: f32,
    /// True until the first resize: the one moment leaves are scattered
    /// through the whole fall (the card: "warm up invisibly") rather than
    /// spawned only at the top, so frame one already looks mid-scene.
    warm: bool,
}

/// The physics step, re-exported from [`leaf`] so the tests share one
/// definition of "a fixed step" with the model itself.
use leaf::STEP;

/// Longest catch-up after a stall, in fixed steps: about ten seconds.
const CATCHUP: i64 = (10.0 / STEP) as i64;

/// How many of the most recent physics steps this frame's render keeps as a
/// fading motion-blur trail (see [`gpu::render`]) - the newest is drawn
/// solid, the rest blended in behind it. Three is what a 30 fps frame at this
/// patch's `STEP` normally contains; kept as a cap rather than "all of it" so
/// a stall's catch-up does not draw a smear across the whole fall.
const BLUR_TRAIL: usize = 3;

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(new_patch(seed))
}

/// The concrete constructor `make` wraps. Split out so the tests can drive a
/// `LeavesPatch` directly - reading its resting list and its falling leaves
/// to check the acceptance criteria - without downcasting a `Box<dyn Patch>`.
fn new_patch(seed: u64) -> LeavesPatch {
    LeavesPatch {
        rng: Rng::new(seed ^ 0x6c_65_61_76),
        wind: Wind::new(seed),
        gpu: None,
        warped: 0.0,
        steps: 0,
        falling: Vec::new(),
        shells: Vec::new(),
        landing: Vec::new(),
        resting: VecDeque::new(),
        fading: VecDeque::new(),
        pending: VecDeque::new(),
        gust_clock: 0.0,
        last_relaunch: -GUST_RELAUNCH_COOLDOWN,
        warm: true,
    }
}

fn random_quat(rng: &mut Rng) -> Quat {
    let axis = v3(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0));
    Quat::from_axis_angle(axis, rng.range(0.0, std::f32::consts::TAU))
}

fn random_build(rng: &mut Rng) -> Build {
    Build {
        size: rng.range(0.75, 1.35),
        inertia: rng.range(0.6, 1.6),
        shape: (rng.f32() * mesh::SHAPES as f32) as u8,
        hue: (rng.f32() * HUES as f32) as u8,
        cup: rng.range(0.06, 0.30),
        curl: rng.range(-0.22, 0.30),
        // Most leaves barely autorotate; a minority get enough of it to
        // visibly spiral (the module doc's "a few get enough of it").
        spin_bias: if rng.f32() < 0.35 { rng.range(-1.0, 1.0) } else { rng.range(-0.15, 0.15) },
    }
}

/// A fresh soft-body shell for a just-(re)spawned leaf, over its own build
/// at its own scale ([`gpu::HALF_LEN_M`] times [`leaf::Build::size`] - the
/// same number [`gpu::push_shell`] draws it at, not a second copy of it),
/// settling briefly under its own weight before anyone can see it
/// ([`shell::LeafShell::spawn`]'s own doc).
fn fresh_shell(leaf: &Leaf3D) -> shell::LeafShell {
    let scale = gpu::HALF_LEN_M * leaf.build.size;
    shell::LeafShell::spawn(leaf.build, scale, shell::Attachment::of(leaf))
}

/// A leaf entering from the top of the frame at its own depth, per
/// [`top_y`], drifting in from whichever side the wind blows from a little
/// more often than not.
fn spawn_entering(rng: &mut Rng, mean_wind: f32) -> Leaf3D {
    let z = rng.range(Z_NEAR, Z_FAR);
    let hw = half_width(z);
    let x = rng.range(-hw, hw);
    let y = top_y(z) + rng.range(0.0, 0.6);
    let vel = v3(mean_wind * 0.25 + rng.range(-0.15, 0.15), rng.range(-0.4, 0.0), rng.range(-0.15, 0.15));
    Leaf3D {
        pos: v3(x, y, z),
        vel,
        orient: random_quat(rng),
        omega: v3(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)),
        build: random_build(rng),
        aloft: 0.0,
        broadside: 0.0,
        resting: false,
    }
}

/// A leaf scattered anywhere mid-fall, for the initial, invisible warm-up.
fn spawn_scattered(rng: &mut Rng, mean_wind: f32) -> Leaf3D {
    let z = rng.range(Z_NEAR, Z_FAR);
    let hw = half_width(z);
    let x = rng.range(-hw, hw);
    let y = rng.range(ground_plane_y(), top_y(z));
    let vel = v3(mean_wind * 0.4 + rng.range(-0.3, 0.3), rng.range(-0.7, -0.1), rng.range(-0.2, 0.2));
    Leaf3D {
        pos: v3(x, y, z),
        vel,
        orient: random_quat(rng),
        omega: v3(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)),
        build: random_build(rng),
        aloft: rng.range(0.0, 3.0),
        broadside: 0.0,
        resting: false,
    }
}

impl LeavesPatch {
    fn resize(&mut self, n: usize, wind_mean: f32) {
        let n = n.clamp(1, MAX_LEAVES);
        if self.warm {
            self.warm = false;
            self.falling = (0..n).map(|_| spawn_scattered(&mut self.rng, wind_mean)).collect();
            self.shells = self.falling.iter().map(fresh_shell).collect();
            return;
        }
        while self.falling.len() < n {
            let fresh = spawn_entering(&mut self.rng, wind_mean);
            self.shells.push(fresh_shell(&fresh));
            self.falling.push(fresh);
        }
        self.falling.truncate(n);
        self.shells.truncate(n);
    }

    fn advance(&mut self, ctx: &Ctx, wind_mean: f32, gusts: f32, flutter: f32, rest_cap: usize) -> Vec<Vec<Leaf3D>> {
        self.warped += ctx.dt.clamp(0.0, 0.25);
        let target = (self.warped / f64::from(STEP) + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        let mut history = Vec::new();
        while self.steps < target {
            let t = self.steps as f32 * STEP;
            self.step_once(t, wind_mean, gusts, flutter, rest_cap);
            history.push(self.falling.clone());
            self.steps += 1;
        }
        if history.is_empty() {
            history.push(self.falling.clone());
        }
        history
    }

    fn step_once(&mut self, t: f32, wind_mean: f32, gusts: f32, flutter: f32, rest_cap: usize) {
        self.gust_clock += STEP;
        if self.gust_clock >= GUST_CHECK_INTERVAL {
            self.gust_clock = 0.0;
            let (gx, _gy, gz) = {
                let w = self.wind.at3(0.0, 0.0, (Z_NEAR + Z_FAR) * 0.5, t, wind_mean, gusts);
                (w.x, w.y, w.z)
            };
            let strength = (gx * gx + gz * gz).sqrt();
            if strength > GUST_RELAUNCH_THRESHOLD && t - self.last_relaunch > GUST_RELAUNCH_COOLDOWN && !self.resting.is_empty() {
                self.last_relaunch = t;
                if let Some(plucked) = self.resting.pop_back() {
                    // Rising from wherever it actually lay (`plucked.pos`),
                    // in its own shape and hue (`plucked.build`) - a gust
                    // that snatches a settled leaf back into the air, not a
                    // fresh leaf appearing elsewhere. `next_falling_leaf`
                    // gives it real relaunch velocity/spin once it reaches
                    // the front of `pending`, and a fresh shell once it
                    // re-enters `falling`, the same as any other respawn.
                    self.pending.push_back(Leaf3D {
                        pos: plucked.pos,
                        vel: V3::ZERO,
                        orient: plucked.orient,
                        omega: V3::ZERO,
                        build: plucked.build,
                        aloft: 0.0,
                        broadside: 0.0,
                        resting: false,
                    });
                }
            }
        }

        for i in 0..self.falling.len() {
            self.falling[i].step(&self.wind, t, wind_mean, gusts, flutter);
            let attach = shell::Attachment::of(&self.falling[i]);
            self.shells[i].step(attach, &self.wind, t, wind_mean, gusts);
            let leaf = &self.falling[i];
            let touch = ground_plane_y() + LANDING_TRIGGER_LENGTHS * leaf.build.size * gpu::HALF_LEN_M;
            let approaching = leaf.pos.y <= touch && leaf.aloft > 0.2;
            let off_side = {
                let hw = half_width(leaf.pos.z) + 1.0;
                leaf.pos.x < -hw || leaf.pos.x > hw
            };
            let stuck = leaf.aloft > MAX_ALOFT;

            if approaching {
                // Hand off to a real physical settle (card 323,
                // `landing::Landing`): this leaf's shell, exactly as it flexes
                // right now, becomes a rigid body carrying the rigid
                // velocity/spin it was already flying with - no snap, no
                // instant freeze. Refill this slot immediately, exactly as
                // any other respawn (card 321's own rule: `next_falling_leaf`
                // draws from `self.rng` in the same place every other exit
                // path does).
                let touchdown = Touchdown {
                    verts: self.shells[i].render_vertices(),
                    faces: self.shells[i].faces().to_vec(),
                    build: leaf.build,
                    vel: leaf.vel,
                    omega: leaf.omega,
                    orient: leaf.orient,
                };
                let floor = self.resting.iter().chain(self.fading.iter().map(|f| &f.litter));
                self.landing.push(Landing::begin(touchdown, floor));
                let fresh_leaf = self.next_falling_leaf(wind_mean);
                self.shells[i] = fresh_shell(&fresh_leaf);
                self.falling[i] = fresh_leaf;
            } else if off_side || stuck {
                self.respawn(i, wind_mean);
            }
        }

        // Step every settle in progress, and freeze any that finished.
        let mut i = 0;
        while i < self.landing.len() {
            self.landing[i].step();
            if self.landing[i].settled() {
                let litter = self.landing.swap_remove(i).bake();
                self.resting.push_back(litter);
                // The cap: the oldest leaf overflowing it fades rather than
                // popping straight back into `pending` (321/322's own
                // instant bump, named directly by the owner's "wobble"
                // complaint on this card - see `review/322-leaves-flex-
                // rapier.md`'s Log). Only a gust, above, ever relaunches a
                // resting leaf now.
                if self.resting.len() > rest_cap {
                    if let Some(old) = self.resting.pop_front() {
                        self.fading.push_back(Fading { litter: old, vanish_at: t + FADE_SECONDS });
                    }
                }
            } else {
                i += 1;
            }
        }
        self.fading.retain(|f| t < f.vanish_at);
    }

    /// The next leaf to occupy a falling slot: a relaunched (bumped or
    /// gust-plucked) one if any are waiting, re-thrown back into the air,
    /// else a brand new one entering at the top - shared by [`respawn`] and
    /// the landed path in [`step_once`] above, so both ways a slot gets
    /// refilled agree on where the leaf itself comes from.
    fn next_falling_leaf(&mut self, wind_mean: f32) -> Leaf3D {
        match self.pending.pop_front() {
            Some(mut relaunched) => {
                relaunched.resting = false;
                relaunched.aloft = 0.0;
                relaunched.vel = v3(self.rng.range(-1.0, 1.0), self.rng.range(1.6, 3.4), self.rng.range(-1.0, 1.0));
                relaunched.omega = v3(self.rng.range(-4.0, 4.0), self.rng.range(-4.0, 4.0), self.rng.range(-4.0, 4.0));
                relaunched
            }
            None => spawn_entering(&mut self.rng, wind_mean),
        }
    }

    fn respawn(&mut self, i: usize, wind_mean: f32) {
        let fresh = self.next_falling_leaf(wind_mean);
        self.shells[i] = fresh_shell(&fresh);
        self.falling[i] = fresh;
    }
}

impl Patch for LeavesPatch {
    fn playing(&self) -> Option<Playing> {
        Some(Playing {
            title: format!("{} leaves aloft", self.falling.len()),
            detail: format!("{} resting", self.resting.len()),
            actions: Vec::new(),
            notes: Vec::new(),
        })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        let wind_mean = ctx.get("wind");
        let gusts = ctx.get("gusts");
        let flutter = ctx.get("flutter");
        let rest_cap = (ctx.get("rest") as usize).min(MAX_REST);
        let color = ctx.get("color");
        let sun_deg = ctx.get("sun");

        self.resize(ctx.get("leaves") as usize, wind_mean);
        let history = self.advance(ctx, wind_mean, gusts, flutter, rest_cap);

        if self.gpu.is_none() {
            self.gpu = Some(gpu::open());
        }
        let Some(Some(live)) = self.gpu.as_ref() else { return Frame::black() };
        let falling = gpu::Falling { history: &history, shells: &self.shells };
        // Alpha computed here, not inside `gpu` (which has no clock of its
        // own): 1 while not yet fading, ramping down to 0 by `vanish_at` -
        // the card's own "fade out slowly... nothing disappears instantly".
        let now = self.steps as f32 * STEP;
        let fading: Vec<(&Litter, f32)> =
            self.fading.iter().map(|f| (&f.litter, ((f.vanish_at - now) / FADE_SECONDS).clamp(0.0, 1.0))).collect();
        let ground = gpu::Ground { resting: &self.resting, landing: &self.landing, fading: &fading };
        let frame = live.render(falling, ground, sun_deg, color, &FAMILIES, BLUR_TRAIL);

        let hues: Vec<f32> = FAMILIES.iter().map(|f| f.hue).collect();
        Palette::ramps(&hues, PALETTE_STEPS, PALETTE_RANGE, PALETTE_CHROMA * color.max(0.0))
            .map(&frame, Dither::BlueNoise, PALETTE_DITHER)
    }
}

/// Used by [`gpu::render`] to fade a distant leaf a little into the black
/// behind it, exactly as `flock`'s haze does for its birds - depth read as
/// contrast, not only as size.
#[must_use]
pub(crate) fn depth_fade(z: f32) -> f32 {
    let t = ((z - Z_NEAR) / (Z_FAR - Z_NEAR)).clamp(0.0, 1.0);
    1.0 - 0.55 * smoothstep(0.0, 1.0, t)
}

#[cfg(test)]
mod tests;
