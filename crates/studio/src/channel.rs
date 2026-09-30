//! Channels: **the only thing in the studio that renders** (cards 350-352).
//!
//! Until card 350 a *player* was a renderer welded to one panel link. It split
//! along the line that was already inside it - the cross-fade blends in linear
//! light *before* the limiter - into two things
//! (`docs/design/studio-vision.md`, "Several panels"):
//!
//! - a **channel** (this file) is a running picture: a patch, the named setting
//!   it came from, its working copy (seed and parameters) and its [`Deck`]. It
//!   renders **linear** frames once per tick, and hands each one to every panel
//!   that follows it;
//! - a **panel** ([`crate::panel`]) is a device and everything about the
//!   device: its link, on/off, brightness, its output settings and its own
//!   `Pipeline`. So two panels on one channel move in lock-step and can still
//!   differ in dither or limiter.
//!
//! A channel is the thing that is meant to be forgotten. It renders on its own
//! OS thread and survives its own patch:
//!
//! - **a patch that panics** is caught by `catch_unwind`, logged once, and the
//!   channel restarts on a safe fallback patch;
//! - **a patch that stalls** - a frame that never comes back - is noticed by a
//!   watchdog. The wedged thread is told to stop and **abandoned**, because no
//!   thread can be killed in Rust, and a *new* core is started on the fallback.
//!   Panel links and pipelines belong to the panels, not to the core, so
//!   abandoning a core leaks a patch's render state and one thread and never a
//!   socket, a link thread or the device itself (card 143, closed by
//!   construction);
//! - **a patch that does either repeatedly** is refused: after [`MAX_FAULTS`]
//!   the channel stops trying, says so once, and `/healthz` goes 503.
//!
//! **Changes are drained, not thrown at a new thread.** A slider is sixty
//! changes a second; a change goes into a one-slot [`Pending`] that the render
//! loop applies between frames.
//!
//! A channel renders at [`screeny_art::FPS`] while any panel on it is connected
//! **or** being watched (or is fading away from it), and at [`IDLE_FPS`]
//! otherwise: a panel that is unplugged for a month, with nobody looking,
//! should not cost a core for a month. A channel with no panel does not exist:
//! [`crate::panels::Panels`] makes one when a panel needs it and drops it when
//! the last panel has left.

use screeny_art::crossfade::{blend, Crossfade};
use screeny_art::patch::{local_now, Ctx, Params, Patch, PatchDef, Playing};
use screeny_art::Frame;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::panel::Panel;
use crate::state::{StoredChannel, Working, DEFAULT_SEED};

/// A channel's id: stable across restarts, never reused within a file.
pub type ChannelId = u32;

/// How long one frame may take before the channel is treated as wedged.
/// Generous: a cold GPU patch's first frame is not a fault.
pub const WATCHDOG: Duration = Duration::from_secs(5);
/// Faults - panics or stalls - before a channel gives up rather than looping.
pub const MAX_FAULTS: u32 = 3;
/// The rate a channel renders at when none of its panels is connected **and**
/// nobody is watching any of them. Enough to be ready the instant either of
/// those changes.
///
/// Card 161 left this alone on purpose: it is not a rate anybody chooses or
/// sees, it is what a forgotten panel costs. Everything else is
/// [`screeny_art::FPS`].
pub const IDLE_FPS: f64 = 5.0;

/// Seconds a picture change **made by hand** cross-fades over (card 304): a
/// patch change, a named setting loaded, a new seed, a restart - and, since
/// card 350, a panel moving from one channel to another. The owner's number,
/// 2026-09-26. Parameter edits happen in place and never fade.
pub const FADE_MANUAL: f32 = 2.0;
/// Longest fade a caller may ask for. Anything longer is this.
pub const FADE_MAX: f32 = 60.0;

/// A caller's fade length, in seconds, as the render loop will use it:
/// `None` (or anything not a number) is [`FADE_MANUAL`], zero or less is a
/// cut, and nothing is longer than [`FADE_MAX`].
#[must_use]
pub fn fade_len(asked: Option<f32>) -> f64 {
    match asked {
        Some(s) if s.is_finite() => f64::from(s.clamp(0.0, FADE_MAX)),
        _ => f64::from(FADE_MANUAL),
    }
}

// ------------------------------------------------------------ the patches ---

/// How long `fault-stall` stops returning for. Comfortably past the watchdog
/// and past `/healthz`'s patience, and then over.
pub const STALL_FOR: Duration = Duration::from_secs(30);

/// Patches that misbehave on purpose, for the containment tests.
///
/// **Not in `screeny_art::patches::ALL`** and not offered by `/api/v1/bootstrap`
/// unless the studio was started with fault patches enabled, which only a test
/// and `SCREENY_STUDIO_FAULTS=1` do. They exist so that "a patch that panics is
/// contained" can be a test rather than a claim.
pub static FAULT_PATCHES: &[PatchDef] = &[
    PatchDef {
        id: "fault-panic",
        name: "Fault: panic",
        blurb: "Panics on its fourth frame. For the containment tests only.",
        params: &[],
        make: |_| Box::new(FaultPatch { frames: 0, kind: Fault::Panic }),
        seeded: false,
    },
    PatchDef {
        id: "fault-stall",
        name: "Fault: stall",
        blurb: "Stops returning frames on its fourth. For the containment tests only.",
        params: &[],
        make: |_| Box::new(FaultPatch { frames: 0, kind: Fault::Stall }),
        seeded: false,
    },
];

enum Fault {
    Panic,
    Stall,
}

struct FaultPatch {
    frames: u32,
    kind: Fault,
}

impl Patch for FaultPatch {
    fn render(&mut self, _ctx: &Ctx) -> screeny_art::Frame {
        self.frames += 1;
        if self.frames >= 4 {
            match self.kind {
                Fault::Panic => panic!("fault-panic: this patch panics on purpose"),
                // Long enough to be a stall by any measure (the watchdog is
                // 5 s), short enough that an abandoned thread eventually goes
                // away even in a test that is itself stuck.
                Fault::Stall => std::thread::sleep(STALL_FOR),
            }
        }
        screeny_art::Frame::black()
    }
}

/// Find a patch by id, including the fault patches when they are enabled.
#[must_use]
pub fn find_patch(id: &str, faults: bool) -> Option<&'static PatchDef> {
    screeny_art::patch::find(id).or_else(|| faults.then(|| FAULT_PATCHES.iter().find(|d| d.id == id)).flatten())
}

/// A patch to fall back to when the chosen one cannot be run.
///
/// CPU only and gentle on the panel, in a fixed order so the choice is
/// predictable in a log. `avoid` is the patch that just failed.
///
/// `plasma` led this list until card 178 removed it. This is about a patch
/// that **broke while running**, not about one this build has not got: a file
/// naming a patch that no longer exists is repaired on the way in
/// (`state::repair_unknown_players`) and never reaches here.
#[must_use]
pub fn fallback_patch(avoid: &str) -> &'static PatchDef {
    for id in ["metaballs", "clocks-numerals", "clocks-dials"] {
        if id != avoid {
            if let Some(d) = screeny_art::patch::find(id) {
                return d;
            }
        }
    }
    screeny_art::patches::ALL.iter().find(|d| d.id != avoid).unwrap_or(&screeny_art::patches::ALL[0])
}

// ----------------------------------------------------------------- a core ---

/// The render state of one patch. Thrown away whole when it misbehaves.
///
/// Card 304 took the pipeline out of here and gave it to the render loop: a
/// cross-fade is two cores feeding **one** limiter, and the limiter has to see
/// the picture continuously across a change to own its brightness safety.
struct Core {
    def: &'static PatchDef,
    patch: Box<dyn Patch>,
    params: Params,
    t: f64,
}

impl Core {
    /// One frame of the patch, `wall` seconds after the last one.
    ///
    /// `paused` and `speed` are card 105's, moved here with the rest of the
    /// design view: the patch's clock is scaled, the pipeline's is not,
    /// because the limiter measures wall-clock rise.
    fn render(&mut self, wall: f64, paused: bool, speed: f64) -> screeny_art::Frame {
        let dt = if paused { 0.0 } else { wall * speed };
        self.t += dt;
        self.patch.render(&Ctx { t: self.t, dt, now: local_now(), params: &self.params })
    }

    /// The patch's parameters, as the configuration now says.
    fn reload_params(&mut self, params: &BTreeMap<String, f32>) {
        self.params = Params::defaults(self.def.params);
        for (id, v) in params {
            self.params.set(self.def.params, id, *v);
        }
    }
}

// -------------------------------------------------------- the cross-fade ---

/// What a fade is fading *from* (card 304).
enum Outgoing {
    /// The previous core, still running on its own clock - a clock's hands
    /// keep moving and a flock keeps flying while it goes.
    Live(Box<Core>),
    /// A still: the last blended frame, when the picture changed again
    /// mid-fade (fades never chain), or black at start-up, or whatever was
    /// on the panel when a live outgoing core failed.
    Held(screeny_art::Frame),
}

struct Fade {
    from: Outgoing,
    clock: Crossfade,
}

/// The picture a player renders: the current core and, during a fade, the one
/// on its way out. Everything the render loop does between "a change arrived"
/// and "a frame for the pipeline" is here, so the tests can drive it with a
/// fake clock and no thread.
struct Deck {
    core: Core,
    fade: Option<Fade>,
    /// The last frame this deck put out, kept only while a fade runs: it is
    /// the still a change mid-fade fades from.
    last: Option<screeny_art::Frame>,
    /// Said once per outgoing core that fails, rather than per frame.
    warned: bool,
}

impl Deck {
    /// A deck on `core` that fades in from black over `fade_in` seconds.
    fn new(core: Core, fade_in: f64) -> Deck {
        let mut deck = Deck { core, fade: None, last: None, warned: false };
        let clock = Crossfade::new(fade_in);
        if !clock.done() {
            deck.fade = Some(Fade { from: Outgoing::Held(screeny_art::Frame::black()), clock });
        }
        deck
    }

    /// Put `next` on, fading from what is showing over `len` seconds. Zero is
    /// a cut.
    ///
    /// A change mid-fade does not start a second fade on top of the first:
    /// the frame on the panel right now is held as a still and becomes what
    /// the new picture fades from, and both cores that were fading are
    /// dropped.
    fn switch(&mut self, next: Core, len: f64) {
        let old = std::mem::replace(&mut self.core, next);
        let clock = Crossfade::new(len);
        if clock.done() {
            self.fade = None;
            self.last = None;
            return;
        }
        let from = match self.fade.take() {
            Some(_) => Outgoing::Held(self.last.take().unwrap_or_else(screeny_art::Frame::black)),
            None => Outgoing::Live(Box::new(old)),
        };
        self.warned = false;
        self.fade = Some(Fade { from, clock });
    }

    /// True while two pictures are being mixed.
    #[cfg(test)]
    fn fading(&self) -> bool {
        self.fade.is_some()
    }

    /// The next frame for the pipeline, `wall` seconds after the last.
    ///
    /// The current core renders outside any `catch_unwind` of this deck's: a
    /// panic there is the player's fault path, as it always was. The
    /// *outgoing* core's render is caught here, because a patch that is on
    /// its way out must not take the one coming in with it - a failing
    /// outgoing core becomes a still of the last frame shown, and the fade
    /// carries on.
    fn render(&mut self, wall: f64, paused: bool, speed: f64) -> screeny_art::Frame {
        let incoming = self.core.render(wall, paused, speed);
        let Some(fade) = self.fade.as_mut() else {
            return incoming;
        };
        fade.clock.advance(wall);
        if fade.clock.done() {
            // Over: the incoming frame goes through untouched, so an indexed
            // patch is exact again from this frame on.
            self.fade = None;
            self.last = None;
            return incoming;
        }
        let w = fade.clock.weight();
        let out = match &mut fade.from {
            Outgoing::Held(still) => blend(still, &incoming, w),
            Outgoing::Live(core) => {
                let from = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.render(wall, paused, speed)));
                match from {
                    Ok(from) => blend(&from, &incoming, w),
                    Err(_) => {
                        if !self.warned {
                            eprintln!("studio: `{}` failed on its way out; fading from a still", core.def.id);
                            self.warned = true;
                        }
                        let still = self.last.take().unwrap_or_else(screeny_art::Frame::black);
                        let out = blend(&still, &incoming, w);
                        fade.from = Outgoing::Held(still);
                        out
                    }
                }
            }
        };
        self.last = Some(out.clone());
        out
    }
}

/// The handle the channel and the watchdog share with a running core.
struct CoreHandle {
    gen: u64,
    /// Set to stop *this* core. A wedged core reads it when its frame returns.
    stop: AtomicBool,
    /// Cleared by the thread on its way out, however it goes out.
    alive: AtomicBool,
    /// Milliseconds since the epoch, stamped before every frame.
    beat: AtomicU64,
    fps: Mutex<f32>,
    /// What is being performed, **and by which patch**.
    ///
    /// The pair matters: a change is applied by the render loop before its
    /// *next* frame, so for up to one frame period the configuration says one
    /// patch and the core is still running another. "What is it performing"
    /// is only true of the patch performing it.
    playing: Mutex<(&'static str, Option<Playing>)>,
}

/// What a core is performing, if it is running the patch that was asked for.
fn performing(handle: &CoreHandle, want: &str) -> Option<Playing> {
    let slot = handle.playing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (slot.0 == want).then(|| slot.1.clone()).flatten()
}

/// What the render loop must do before its next frame.
///
/// One slot, newest wins, like every other mailbox in this server. A slider
/// being dragged sets `params` sixty times a second and costs one re-read of
/// the parameter map per frame rather than sixty thread restarts.
#[derive(Default)]
struct Pending {
    /// The patch, the setting or the seed changed: a fresh core.
    rebuild: bool,
    /// Re-read the parameters into the running core.
    params: bool,
    /// Rebuild the patch from its seed, keeping everything else.
    restart: bool,
    /// One action a composing patch offered (card 140).
    action: Option<String>,
    /// How long the next rebuild or restart fades over, in seconds, as
    /// [`fade_len`] reads it. Newest wins, like everything else here.
    fade: Option<f32>,
}

impl Pending {
    fn anything(&self) -> bool {
        self.rebuild || self.params || self.restart || self.action.is_some()
    }
}

/// What a channel has been through. On `/api/v1/status`, merged into each of
/// its panels' `health`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ChannelHealth {
    /// Patches that panicked mid-render.
    pub panics: u64,
    /// Frames that never came back, caught by the watchdog.
    pub stalls: u64,
    /// Core threads started after the first: the count of recoveries. A
    /// patch change does *not* start one, so this counts faults.
    pub restarts: u64,
    /// Wedged threads still out there. Each one is a patch's render state,
    /// never a socket or a link.
    pub abandoned: u64,
    /// The patch the channel was asked for but cannot run, if it fell back.
    pub fell_back_from: Option<String>,
    /// Patches this channel refuses to load again until a human says
    /// otherwise.
    pub refused: Vec<String>,
    /// Set when the channel has given up: [`MAX_FAULTS`] faults in a row. The
    /// one condition of a picture that makes the server unhealthy.
    pub gave_up: Option<String>,
    /// The last thing that went wrong with the picture, if anything has.
    pub last_error: Option<String>,
}

/// A channel as `/api/v1/status` and the panels' views see it.
#[derive(Clone, Debug)]
pub struct ChannelStatus {
    pub id: ChannelId,
    pub stored: StoredChannel,
    /// True when a core thread is alive and ticking.
    pub running: bool,
    /// Frames rendered since the channel was made, across cores.
    pub ticks: u64,
    /// The rate the render loop is actually achieving.
    pub fps_measured: f32,
    /// Seconds since the render loop last completed a frame.
    pub last_tick_ago: Option<f64>,
    pub playing: Option<Playing>,
    pub health: ChannelHealth,
}

/// An edit to a channel's picture, as opposed to a pick of one (which goes
/// through [`crate::panels::Panels::pick`] and its three rules). Everything
/// optional; `None` leaves it alone.
#[derive(Clone, Debug, Default)]
pub struct Edit {
    pub seed: Option<u32>,
    pub param: Option<(String, f32)>,
    /// "Reset": back to the patch's defaults. Since card 350 there is no
    /// per-patch memory to forget as well: the working copy is the channel's.
    pub reset_params: bool,
    /// Start the patch again from its seed.
    pub restart: bool,
    /// An action a composing patch offered (card 140).
    pub act: Option<String>,
    /// Retired from every route (card 302) and kept for one reason: a test
    /// that needs a still picture holds the channel here, in process.
    pub paused: Option<bool>,
}

impl Edit {
    /// Whether this edit changes the picture itself - which, for a request that
    /// also picks one, means the panel must not simply *join* another channel
    /// (card 350's rule 1): the edit would land on everybody on it.
    #[must_use]
    pub fn changes_the_picture(&self) -> bool {
        self.seed.is_some() || self.param.is_some() || self.reset_params
    }
}

/// One running picture, and the panels that show it.
pub struct Channel {
    id: ChannelId,
    cfg: Mutex<StoredChannel>,
    paused: AtomicBool,
    core: Mutex<Option<Arc<CoreHandle>>>,
    health: Mutex<ChannelHealth>,
    pending: Mutex<Pending>,
    stop: AtomicBool,
    faults: bool,
    gen: AtomicU64,
    /// Frames rendered, across every core this channel has had: a core that
    /// is replaced must not reset the count, or a restart would look like a
    /// render loop that had stopped.
    ticks: Arc<AtomicU64>,
    /// Faults since the last good run: the brake on a restart loop.
    consecutive: AtomicU64,
    /// The panels this channel renders for, in the order they joined. The
    /// render loop hands every frame to each of them.
    followers: Mutex<Vec<Arc<Panel>>>,
    /// Panels still fading **away** from this channel (card 350): they read
    /// [`Channel::latest`] for up to [`FADE_MANUAL`], so this keeps rendering
    /// at the full rate, and is not dropped, until they have finished.
    leaving: AtomicUsize,
    /// The newest linear frame, for a panel fading away from this channel.
    latest: Mutex<Option<Arc<Frame>>>,
}

impl Channel {
    #[must_use]
    pub fn new(stored: StoredChannel, faults: bool) -> Arc<Channel> {
        Arc::new(Channel {
            id: stored.id,
            cfg: Mutex::new(stored),
            paused: AtomicBool::new(false),
            core: Mutex::new(None),
            health: Mutex::new(ChannelHealth::default()),
            pending: Mutex::new(Pending::default()),
            stop: AtomicBool::new(false),
            faults,
            gen: AtomicU64::new(0),
            ticks: Arc::new(AtomicU64::new(0)),
            consecutive: AtomicU64::new(0),
            followers: Mutex::new(Vec::new()),
            leaving: AtomicUsize::new(0),
            latest: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn id(&self) -> ChannelId {
        self.id
    }

    fn cfg(&self) -> MutexGuard<'_, StoredChannel> {
        self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn health_mut(&self) -> MutexGuard<'_, ChannelHealth> {
        self.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn pending_mut(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn take_pending(&self) -> Pending {
        std::mem::take(&mut *self.pending_mut())
    }

    fn followers_mut(&self) -> MutexGuard<'_, Vec<Arc<Panel>>> {
        self.followers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What it shows, as persisted.
    #[must_use]
    pub fn stored(&self) -> StoredChannel {
        self.cfg().clone()
    }

    /// The patch it is on.
    #[must_use]
    pub fn patch(&self) -> String {
        self.cfg().patch.clone()
    }

    /// The name of the setting its working copy came from; empty is Default.
    #[must_use]
    pub fn setting(&self) -> String {
        self.cfg().setting.clone()
    }

    /// **The working copy**: what a setting holds, as it is set right now
    /// (card 151). `None` for a patch this build has not got - there is no
    /// spec to measure the values against, so there is nothing honest to save.
    #[must_use]
    pub fn working(&self) -> Option<(&'static PatchDef, Working)> {
        let cfg = self.cfg();
        let def = find_patch(&cfg.patch, self.faults)?;
        Some((def, Working { params: crate::state::sparse(def, &cfg.params), seed: cfg.seed, speed: 1.0 }))
    }

    /// Whether the working copy has moved away from the setting it says it
    /// came from. Computed, never stored (card 151).
    #[must_use]
    pub fn modified(&self, memory: &crate::state::SharedMemory) -> bool {
        let setting = self.setting();
        self.working().is_some_and(|(def, work)| memory.modified(def, &setting, &work))
    }

    /// Does this channel show exactly `patch` on `setting`, with no unsaved
    /// tweaks? Card 350's rule 1: a panel asking for that picture joins it.
    #[must_use]
    pub fn shows(&self, patch: &str, setting: &str, memory: &crate::state::SharedMemory) -> bool {
        let (on_patch, on_setting) = {
            let cfg = self.cfg();
            (cfg.patch == patch, crate::state::same_setting(&cfg.setting, setting))
        };
        on_patch && on_setting && !self.modified(memory)
    }

    /// The panels on it, in the order they joined.
    #[must_use]
    pub fn followers(&self) -> Vec<Arc<Panel>> {
        self.followers_mut().clone()
    }

    /// Their device ids.
    #[must_use]
    pub fn follower_ids(&self) -> Vec<String> {
        self.followers_mut().iter().map(|p| p.device()).collect()
    }

    pub(crate) fn add_follower(&self, panel: &Arc<Panel>) {
        let mut f = self.followers_mut();
        if !f.iter().any(|p| Arc::ptr_eq(p, panel)) {
            f.push(Arc::clone(panel));
        }
    }

    pub(crate) fn remove_follower(&self, panel: &Arc<Panel>) {
        self.followers_mut().retain(|p| !Arc::ptr_eq(p, panel));
    }

    /// True when nothing needs this channel any more: no panel follows it and
    /// none is still fading away from it.
    #[must_use]
    pub fn unused(&self) -> bool {
        self.followers_mut().is_empty() && self.leaving.load(Ordering::Relaxed) == 0
    }

    /// A panel has started fading away from this channel.
    pub(crate) fn leaving_started(&self) {
        self.leaving.fetch_add(1, Ordering::Relaxed);
    }

    /// ...and has finished.
    pub(crate) fn leaving_done(&self) {
        self.leaving.fetch_sub(1, Ordering::Relaxed);
    }

    /// The newest linear frame this channel rendered, for a panel fading away
    /// from it.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Frames rendered since it was made. Card 350's measure of "one render
    /// for every panel on it".
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    /// **Show this picture**: `def`, on the setting called `setting` (empty or
    /// `Default` is Default), with `work` as its working copy - one change, so
    /// the panels follow in one step rather than through a burst of
    /// half-loaded pictures. Cross-fades over [`fade_len`]`(fade)`.
    pub fn show(&self, def: &'static PatchDef, setting: &str, work: Working, fade: Option<f32>) {
        {
            let mut cfg = self.cfg();
            cfg.patch = def.id.to_string();
            cfg.setting = crate::state::setting_key(setting);
            cfg.seed = work.seed;
            cfg.params = work.params;
        }
        // Asking for a patch again clears its refusal: a human saying "try it"
        // outranks the brake.
        {
            let mut h = self.health_mut();
            h.refused.retain(|r| r != def.id);
            h.gave_up = None;
            h.fell_back_from = None;
        }
        self.consecutive.store(0, Ordering::Relaxed);
        let mut p = self.pending_mut();
        p.rebuild = true;
        p.fade = fade;
    }

    /// Say which named setting the working copy came from, without touching
    /// the working copy: a save, a rename or a delete of a setting.
    pub fn set_setting_name(&self, name: &str) {
        self.cfg().setting = crate::state::setting_key(name);
    }

    /// Change the picture in place. Everything `None` is left alone.
    ///
    /// Nothing here stops a thread: what has to change is written into the
    /// one-slot [`Pending`] and applied by the render loop before its next
    /// frame. A new seed or a restart cross-fades over [`FADE_MANUAL`];
    /// parameter edits never fade.
    ///
    /// # Errors
    ///
    /// If the patch has no such parameter.
    pub fn edit(&self, edit: &Edit) -> Result<(), String> {
        let mut want = Pending::default();
        {
            let mut cfg = self.cfg();
            if let Some(seed) = edit.seed {
                cfg.seed = seed;
                want.rebuild = true;
            }
            if let Some((id, value)) = &edit.param {
                let def = find_patch(&cfg.patch, self.faults);
                let Some(spec) = def.and_then(|d| d.params.iter().find(|p| p.id == id)) else {
                    return Err(format!("{} has no parameter `{id}`", cfg.patch));
                };
                cfg.params.insert(id.clone(), spec.sanitise(*value));
                want.params = true;
            }
            if edit.reset_params {
                cfg.params.clear();
                want.params = true;
            }
        }
        if let Some(paused) = edit.paused {
            self.paused.store(paused, Ordering::Relaxed);
        }
        want.restart = edit.restart;
        want.action.clone_from(&edit.act);
        if want.anything() {
            let mut p = self.pending_mut();
            p.rebuild |= want.rebuild;
            p.params |= want.params;
            p.restart |= want.restart;
            if want.rebuild || want.restart {
                p.fade = None;
            }
            if want.action.is_some() {
                // One slot: a burst of button presses is the newest one.
                p.action = want.action;
            }
        }
        Ok(())
    }

    /// Start the render loop if it should be running and is not.
    pub fn ensure_running(self: &Arc<Self>) {
        if self.stop.load(Ordering::Relaxed) || self.health_mut().gave_up.is_some() {
            return;
        }
        let alive = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let running = alive.as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed));
        drop(alive);
        if !running {
            self.start_core();
        }
    }

    /// The watchdog, called about once a second.
    pub fn supervise(self: &Arc<Self>) {
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        // A wedged core: the frame never came back.
        let wedged = {
            let core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            core.as_ref().and_then(|h| {
                let since = unix_millis().saturating_sub(h.beat.load(Ordering::Relaxed));
                (h.alive.load(Ordering::Relaxed) && since > WATCHDOG.as_millis() as u64).then_some(Arc::clone(h))
            })
        };
        if let Some(h) = wedged {
            let patch = self.patch();
            h.stop.store(true, Ordering::Relaxed);
            {
                let mut hl = self.health_mut();
                hl.stalls += 1;
                hl.abandoned += 1;
            }
            self.fault(&patch, &format!("`{patch}` has not produced a frame for {WATCHDOG:?}"));
            // The wedged thread is on its own now; a fresh core takes over.
            *self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        self.ensure_running();
    }

    /// Everything the status views need.
    #[must_use]
    pub fn status(&self) -> ChannelStatus {
        let stored = self.stored();
        let (running, fps_measured, playing, last_tick_ago) = {
            let core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match core.as_ref() {
                Some(h) => (
                    h.alive.load(Ordering::Relaxed),
                    *h.fps.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
                    performing(h, &stored.patch),
                    Some(unix_millis().saturating_sub(h.beat.load(Ordering::Relaxed)) as f64 / 1000.0),
                ),
                None => (false, 0.0, None, None),
            }
        };
        ChannelStatus {
            id: self.id,
            stored,
            running,
            ticks: self.ticks(),
            fps_measured,
            last_tick_ago,
            playing,
            health: self.health_mut().clone(),
        }
    }

    /// What a composing patch says it is performing, from the last frame.
    ///
    /// `None` while the render loop has yet to pick up a patch change: what
    /// the *previous* patch was performing is not an answer to this question.
    #[must_use]
    pub fn playing(&self) -> Option<Playing> {
        let want = self.patch();
        let core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        core.as_ref().and_then(|h| performing(h, &want))
    }

    /// Stop for good: the core ends. The panels' links are theirs.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(h) = core.take() {
            h.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Who this channel is, for a log line: the panels on it.
    fn who(&self) -> String {
        let ids = self.follower_ids();
        if ids.is_empty() {
            format!("channel {}", self.id)
        } else {
            let names: Vec<&str> = ids.iter().map(|d| if d.is_empty() { "(no panel yet)" } else { d.as_str() }).collect();
            format!("channel {} ({})", self.id, names.join(", "))
        }
    }

    // ---- internals ----

    /// One fault: log it once, count it, and either fall back or give up.
    fn fault(self: &Arc<Self>, patch: &str, what: &str) {
        let n = self.consecutive.fetch_add(1, Ordering::Relaxed) + 1;
        let fallback = fallback_patch(patch);
        let who = self.who();
        let mut h = self.health_mut();
        h.last_error = Some(what.to_string());
        if !h.refused.iter().any(|r| r == patch) {
            h.refused.push(patch.to_string());
        }
        if n >= u64::from(MAX_FAULTS) {
            let msg = format!("{what}; giving up after {n} faults");
            eprintln!("studio: {who}: {msg}");
            // The configuration is left alone: the channel stays configured,
            // just silent, so that a human can see what it was meant to show.
            h.gave_up = Some(msg);
            return;
        }
        eprintln!("studio: {who}: {what}; falling back to `{}`", fallback.id);
        h.fell_back_from = Some(patch.to_string());
        drop(h);
        // The fallback arrives on its Default, like any patch picked afresh
        // since card 350. The failing patch's named settings are untouched.
        let mut cfg = self.cfg();
        cfg.patch = fallback.id.to_string();
        cfg.setting = String::new();
        cfg.seed = DEFAULT_SEED;
        cfg.params = BTreeMap::new();
    }

    /// A core on whatever the configuration says, or on the fallback when that
    /// cannot be run. Never fails: a channel always has something to show.
    fn make_core(&self) -> Core {
        let (def, seed, params_map) = {
            let cfg = self.cfg();
            (cfg.patch.clone(), cfg.seed, cfg.params.clone())
        };
        let refused = self.health_mut().refused.clone();
        let chosen = match find_patch(&def, self.faults) {
            Some(d) if !refused.contains(&def) => d,
            _ => {
                let f = fallback_patch(&def);
                if find_patch(&def, self.faults).is_none() {
                    let mut h = self.health_mut();
                    if h.fell_back_from.as_deref() != Some(def.as_str()) {
                        eprintln!("studio: channel {}: no patch called `{def}`; playing `{}`", self.id, f.id);
                    }
                    h.fell_back_from = Some(def.clone());
                }
                f
            }
        };
        let mut params = Params::defaults(chosen.params);
        for (id, v) in &params_map {
            params.set(chosen.params, id, *v);
        }
        Core { def: chosen, patch: (chosen.make)(u64::from(seed)), params, t: 0.0 }
    }

    fn start_core(self: &Arc<Self>) {
        let core = self.make_core();
        let gen = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = Arc::new(CoreHandle {
            gen,
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            beat: AtomicU64::new(unix_millis()),
            fps: Mutex::new(0.0),
            playing: Mutex::new((core.def.id, None)),
        });
        {
            let mut slot = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(old) = slot.replace(Arc::clone(&handle)) {
                old.stop.store(true, Ordering::Relaxed);
            }
        }
        if gen > 1 {
            self.health_mut().restarts += 1;
        }
        let me = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name(format!("channel-{}", self.id))
            .spawn(move || run_core(&me, &handle, core));
        if let Err(e) = started {
            let mut h = self.health_mut();
            h.gave_up = Some(format!("could not start a render thread: {e}"));
            eprintln!("studio: channel {}: could not start a render thread: {e}", self.id);
        }
    }

    /// Put a frame where a panel fading away from this channel can find it.
    fn publish_latest(&self, frame: &Frame) {
        // Only kept while somebody is leaving: nobody else reads it, and a
        // 64x32 frame a tick is not worth copying for nobody.
        let keep = self.leaving.load(Ordering::Relaxed) > 0;
        let mut slot = self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = keep.then(|| Arc::new(frame.clone()));
    }

    /// Test only: what a channel that is not running would have shown.
    #[cfg(test)]
    pub(crate) fn set_latest(&self, frame: Frame) {
        *self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(frame));
    }
}

/// The render loop of one core.
///
/// Everything that can change about the picture arrives through the
/// channel's one-slot [`Pending`] and is applied here, between frames. Each
/// frame is rendered **once** and handed, linear, to every panel on the
/// channel, which puts it through its own pipeline, its own preview cell and
/// its own link (card 350).
///
/// The first core of a channel starts on its picture at once: a panel that
/// arrives on a channel fades in its own output stage, so a fade here would be
/// a second one on top. A core that replaces one that failed fades in from
/// black, as every render thread did before card 350.
fn run_core(channel: &Arc<Channel>, handle: &Arc<CoreHandle>, core: Core) {
    let fade_in = if handle.gen > 1 { f64::from(FADE_MANUAL) } else { 0.0 };
    let mut deck = Deck::new(core, fade_in);
    let mut patch_id = deck.core.def.id.to_string();
    let mut next = Instant::now();
    let mut last = Instant::now();
    let mut fps = 0.0_f32;
    let mut panicked = false;

    while !handle.stop.load(Ordering::Relaxed) && !channel.stop.load(Ordering::Relaxed) {
        handle.beat.store(unix_millis(), Ordering::Relaxed);
        let paused = channel.paused.load(Ordering::Relaxed);

        // Apply whatever has been asked for since the last frame, then render.
        // The patch is the only code in here that can panic - `act` as much as
        // `render` - so both are inside the same `catch_unwind`.
        let want = channel.take_pending();
        let rebuilt = want.rebuild || want.restart;
        if rebuilt {
            deck.switch(channel.make_core(), fade_len(want.fade));
            patch_id = deck.core.def.id.to_string();
        }
        let reconf = (want.params && !rebuilt).then(|| channel.stored());
        let now = Instant::now();
        let wall = now.duration_since(last).as_secs_f64();
        last = now;
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(cfg) = &reconf {
                deck.core.reload_params(&cfg.params);
            }
            if let Some(action) = &want.action {
                deck.core.patch.act(action);
            }
            deck.render(wall, paused, 1.0)
        }));
        let Ok(frame) = rendered else {
            panicked = true;
            break;
        };
        if wall > 0.0 {
            fps += (1.0 / wall as f32 - fps) * 0.1;
        }
        channel.ticks.fetch_add(1, Ordering::Relaxed);
        *handle.fps.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = fps;
        if let Ok(mut p) = handle.playing.lock() {
            *p = (deck.core.def.id, deck.core.patch.playing());
        }
        channel.publish_latest(&frame);

        // Every panel on the channel gets this one frame. Each present is a
        // 64x32 pipeline pass and a non-blocking send, and never waits for a
        // browser or the network.
        let mut busy = channel.leaving.load(Ordering::Relaxed) > 0;
        for panel in channel.followers() {
            busy |= panel.present(&frame, wall, deck.core.t, fps);
        }

        // A run of good frames clears the fault brake.
        if channel.ticks().is_multiple_of(300) {
            channel.consecutive.store(0, Ordering::Relaxed);
        }

        // Full rate while a panel on it is connected or watched (or still
        // fading away from it). Otherwise there is nothing to be fast for.
        let rate = if busy { screeny_art::FPS } else { IDLE_FPS };
        next += Duration::from_secs_f64(1.0 / rate);
        let now = Instant::now();
        if next > now {
            std::thread::sleep((next - now).min(Duration::from_secs(1)));
        } else {
            next = now;
        }
    }

    handle.alive.store(false, Ordering::Relaxed);
    *handle.fps.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = 0.0;

    if panicked {
        channel.health_mut().panics += 1;
        channel.fault(&patch_id, &format!("`{patch_id}` panicked while rendering"));
        // Only the generation that panicked may start the replacement: a core
        // that was stopped on purpose must not resurrect itself.
        let mine = {
            let core = channel.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            core.as_ref().is_some_and(|h| h.gen == handle.gen)
        };
        if mine && !channel.stop.load(Ordering::Relaxed) {
            *channel.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            channel.ensure_running();
        }
    }
}

#[must_use]
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fault_patches_are_not_in_the_normal_list() {
        assert!(screeny_art::patch::find("fault-panic").is_none());
        assert!(screeny_art::patch::find("fault-stall").is_none());
        assert!(find_patch("fault-panic", false).is_none());
        assert!(find_patch("fault-panic", true).is_some());
        assert!(find_patch("metaballs", false).is_some());
    }

    /// Every id the list itself names, so that the day one of them is removed
    /// (as `plasma` was, card 178) this says so rather than quietly handing
    /// back the patch that just failed.
    #[test]
    fn the_fallback_is_never_the_patch_that_just_failed() {
        assert_ne!(fallback_patch("metaballs").id, "metaballs");
        assert_ne!(fallback_patch("clocks-numerals").id, "clocks-numerals");
        assert_ne!(fallback_patch("clocks-dials").id, "clocks-dials");
        assert_ne!(fallback_patch("fault-panic").id, "fault-panic");
        assert!(
            screeny_art::patch::find(fallback_patch("metaballs").id).is_some(),
            "the fallback has to be a patch this build really has"
        );
    }

    fn idle_channel() -> Arc<Channel> {
        Channel::new(StoredChannel { id: 1, patch: "metaballs".into(), ..StoredChannel::default() }, true)
    }

    #[test]
    fn an_edit_keeps_what_it_was_given_and_a_bad_one_changes_nothing() {
        let c = idle_channel();
        c.edit(&Edit { seed: Some(9), param: Some(("size".into(), 2.5)), ..Edit::default() }).expect("a real parameter");
        let s = c.stored();
        assert_eq!(s.seed, 9);
        assert_eq!(s.params["size"], 2.5);
        assert!(c.edit(&Edit { param: Some(("nope".into(), 1.0)), ..Edit::default() }).is_err());
        assert_eq!(c.stored(), s, "a rejected change must change nothing");
    }

    /// Changing a parameter must not rebuild the patch: the page's sliders are
    /// sixty changes a second, and a rebuild would restart the animation on
    /// every one of them. A seed does rebuild it.
    #[test]
    fn a_parameter_change_does_not_rebuild_the_patch() {
        let c = idle_channel();
        c.edit(&Edit { param: Some(("size".into(), 2.5)), ..Edit::default() }).expect("size");
        let want = c.take_pending();
        assert!(want.params, "the running core re-reads its parameters");
        assert!(!want.rebuild, "and the patch is not built again");
        c.edit(&Edit { seed: Some(3), ..Edit::default() }).expect("a seed");
        assert!(c.take_pending().rebuild);
    }

    /// An action a composing patch offers goes into a one-slot mailbox rather
    /// than reaching into a running patch from another thread (card 140).
    #[test]
    fn an_action_is_a_one_slot_mailbox() {
        let c = idle_channel();
        c.edit(&Edit { act: Some("next".into()), ..Edit::default() }).expect("an action");
        c.edit(&Edit { act: Some("again".into()), ..Edit::default() }).expect("another");
        let want = c.take_pending();
        assert_eq!(want.action.as_deref(), Some("again"), "newest wins; a burst costs one slot");
        assert!(c.take_pending().action.is_none(), "and it is taken, not repeated");
    }

    /// A picture shown carries its fade to the render loop; a parameter edit
    /// never fades.
    #[test]
    fn a_picture_carries_its_fade_and_a_parameter_edit_has_none() {
        let c = idle_channel();
        let def = find_patch("clocks-dials", false).expect("dials");
        c.show(def, "", Working { params: BTreeMap::new(), seed: 1, speed: 1.0 }, Some(5.0));
        let want = c.take_pending();
        assert!(want.rebuild);
        assert_eq!(want.fade, Some(5.0));
        c.edit(&Edit { param: Some(("dwell".into(), 90.0)), ..Edit::default() }).expect("dwell");
        let want = c.take_pending();
        assert!(!want.rebuild && !want.restart);
        assert_eq!(want.fade, None, "nothing to fade");
        c.edit(&Edit { restart: true, ..Edit::default() }).expect("restart");
        let want = c.take_pending();
        assert!(want.restart);
        assert_eq!(fade_len(want.fade), f64::from(FADE_MANUAL), "by hand is the manual length");
    }

    /// "Modified" is the working copy against the setting it came from, on the
    /// channel now rather than per patch (card 350).
    #[test]
    fn modified_is_the_channels_working_copy_against_its_setting() {
        let memory = crate::state::SharedMemory::default();
        let c = idle_channel();
        assert!(!c.modified(&memory), "Default, untouched");
        assert!(c.shows("metaballs", "Default", &memory));
        assert!(c.shows("metaballs", "", &memory), "empty is Default too");
        c.edit(&Edit { param: Some(("size".into(), 2.5)), ..Edit::default() }).expect("size");
        assert!(c.modified(&memory));
        assert!(!c.shows("metaballs", "Default", &memory), "a tweaked picture is not the picture");
        let def = find_patch("metaballs", false).expect("metaballs");
        let (_, work) = c.working().expect("a working copy");
        memory.save_setting(def, "Lava", &work).expect("save");
        c.set_setting_name("Lava");
        assert!(!c.modified(&memory));
        assert!(c.shows("metaballs", "Lava", &memory));
    }

    // ------------------------------ the cross-fade (card 304) ------------------------------

    use screeny_art::crossfade::ease;
    use screeny_art::{Output, Pipeline};
    use screeny_art::{Frame, Rgb, N};

    /// A patch that is one colour, as an indexed frame, so "the output after
    /// the fade is the incoming frame exactly" can be checked byte for byte.
    struct Flat(Rgb);

    impl Patch for Flat {
        fn render(&mut self, _ctx: &Ctx) -> Frame {
            Frame::Indexed { palette: vec![self.0], indices: vec![0; N] }
        }
    }

    const RED: Rgb = Rgb::new(0.30, 0.02, 0.01);
    const BLUE: Rgb = Rgb::new(0.01, 0.03, 0.25);
    const GREEN: Rgb = Rgb::new(0.02, 0.20, 0.03);

    static FLAT_PATCHES: &[PatchDef] = &[
        PatchDef { id: "test-red", name: "red", blurb: "", params: &[], make: |_| Box::new(Flat(RED)), seeded: false },
        PatchDef { id: "test-blue", name: "blue", blurb: "", params: &[], make: |_| Box::new(Flat(BLUE)), seeded: false },
        PatchDef { id: "test-green", name: "green", blurb: "", params: &[], make: |_| Box::new(Flat(GREEN)), seeded: false },
        PatchDef { id: "test-white", name: "white", blurb: "", params: &[], make: |_| Box::new(Flat(Rgb::splat(1.0))), seeded: false },
    ];

    fn core_of(def: &'static PatchDef) -> Core {
        Core { def, patch: (def.make)(0), params: Params::defaults(def.params), t: 0.0 }
    }

    fn flat(id: &str) -> Core {
        core_of(FLAT_PATCHES.iter().find(|d| d.id == id).expect("a test patch"))
    }

    const DT: f64 = 1.0 / 30.0;

    fn close(a: Rgb, b: Rgb) -> bool {
        (a.r - b.r).abs() < 1e-5 && (a.g - b.g).abs() < 1e-5 && (a.b - b.b).abs() < 1e-5
    }

    /// A patch change fades over the manual length with smoothstep weights,
    /// and the first frame after the fade is the incoming patch's own frame,
    /// indexed and exact - not a blend that happens to land on it.
    #[test]
    fn a_patch_change_fades_over_the_manual_length_and_lands_exactly() {
        let mut deck = Deck::new(flat("test-red"), 0.0);
        assert!(!deck.fading(), "a zero fade-in is no fade at all");
        assert!(matches!(deck.render(DT, false, 1.0), Frame::Indexed { .. }));

        deck.switch(flat("test-blue"), fade_len(None));
        let mut frames = 0;
        loop {
            let f = deck.render(DT, false, 1.0);
            frames += 1;
            if !deck.fading() {
                match f {
                    Frame::Indexed { palette, indices } => {
                        assert_eq!(palette, vec![BLUE]);
                        assert_eq!(indices, vec![0; N]);
                    }
                    Frame::Linear(_) => panic!("after the fade the incoming frame goes through untouched"),
                }
                break;
            }
            let w = ease((frames as f64 * DT / f64::from(FADE_MANUAL)) as f32);
            assert!(close(f.pixel(100), RED.lerp(BLUE, w)), "frame {frames}: {:?} is not {w} of the way", f.pixel(100));
            assert!(frames < 100, "the fade never ended");
        }
        assert!((59..=61).contains(&frames), "two seconds at 30 fps, not {frames} frames");
    }

    /// The outgoing core keeps its own clock during the fade: it is ticked,
    /// not frozen.
    #[test]
    fn the_outgoing_core_keeps_running() {
        let mut deck = Deck::new(flat("test-red"), 0.0);
        for _ in 0..30 {
            deck.render(DT, false, 1.0);
        }
        deck.switch(flat("test-blue"), 2.0);
        for _ in 0..15 {
            deck.render(DT, false, 1.0);
        }
        let Some(Fade { from: Outgoing::Live(old), .. }) = &deck.fade else { panic!("a live outgoing core") };
        assert!((old.t - 45.0 * DT).abs() < 1e-9, "the old clock ran on: {}", old.t);
        assert!((deck.core.t - 15.0 * DT).abs() < 1e-9, "the new one started at zero");
    }

    /// A change mid-fade holds the frame that was showing as a still and
    /// fades from that - it does not chain a second fade onto the first, and
    /// both cores that were fading are gone.
    #[test]
    fn a_change_mid_fade_fades_from_a_held_still() {
        let mut deck = Deck::new(flat("test-red"), 0.0);
        deck.render(DT, false, 1.0);
        deck.switch(flat("test-blue"), 2.0);
        let mut shown = Frame::black();
        for _ in 0..30 {
            shown = deck.render(DT, false, 1.0);
        }
        let held = shown.pixel(0);
        assert!(close(held, RED.lerp(BLUE, 0.5)), "one second in is an even mix: {held:?}");

        deck.switch(flat("test-green"), 2.0);
        match &deck.fade {
            Some(Fade { from: Outgoing::Held(still), .. }) => assert!(close(still.pixel(0), held), "the still is the frame that was shown"),
            _ => panic!("a change mid-fade must hold a still, not a live core"),
        }
        for k in 1..=45 {
            let f = deck.render(DT, false, 1.0);
            let w = ease((k as f64 * DT / 2.0) as f32);
            assert!(close(f.pixel(0), held.lerp(GREEN, w)), "frame {k}: from the still, not from red or blue");
        }
    }

    /// `Some(0.0)` is a cut: the very next frame is the new patch, exactly.
    #[test]
    fn a_zero_fade_is_a_cut() {
        let mut deck = Deck::new(flat("test-red"), 0.0);
        deck.render(DT, false, 1.0);
        deck.switch(flat("test-blue"), fade_len(Some(0.0)));
        assert!(!deck.fading());
        let f = deck.render(DT, false, 1.0);
        assert!(matches!(&f, Frame::Indexed { palette, .. } if palette == &vec![BLUE]));

        // And a cut mid-fade is a cut too, not a fade from a still.
        deck.switch(flat("test-green"), 2.0);
        deck.render(DT, false, 1.0);
        deck.switch(flat("test-red"), 0.0);
        assert!(!deck.fading());
        assert!(matches!(deck.render(DT, false, 1.0), Frame::Indexed { palette, .. } if palette == vec![RED]));
    }

    /// The first picture a render thread shows fades in from black.
    #[test]
    fn a_new_player_fades_in_from_black() {
        let mut deck = Deck::new(flat("test-blue"), f64::from(FADE_MANUAL));
        let first = deck.render(DT, false, 1.0).pixel(0);
        assert!(first.b < BLUE.b * 0.01, "the first frame is all but black: {first:?}");
        let mut frames = 1;
        while deck.fading() {
            deck.render(DT, false, 1.0);
            frames += 1;
        }
        assert!((59..=61).contains(&frames), "over the manual length, not {frames} frames");
    }

    /// An outgoing patch that fails on its way out becomes a still of the last
    /// frame shown; the incoming one carries on and nothing is faulted.
    #[test]
    fn an_outgoing_core_that_panics_becomes_a_still() {
        let panicker = FAULT_PATCHES.iter().find(|d| d.id == "fault-panic").expect("fault-panic");
        let mut deck = Deck::new(core_of(panicker), 0.0);
        deck.render(DT, false, 1.0);
        deck.render(DT, false, 1.0);
        deck.switch(flat("test-blue"), 2.0);
        for _ in 0..10 {
            deck.render(DT, false, 1.0); // its fourth frame panics, inside the deck
        }
        assert!(matches!(&deck.fade, Some(Fade { from: Outgoing::Held(_), .. })));
        while deck.fading() {
            deck.render(DT, false, 1.0);
        }
        assert!(matches!(deck.render(DT, false, 1.0), Frame::Indexed { palette, .. } if palette == vec![BLUE]));
    }

    /// The limiter sees the blended frame: a fade into full white rises no
    /// faster than the rise cap and settles at the APL cap, like any frame.
    #[test]
    fn the_limiter_runs_on_the_blended_frame() {
        let mut deck = Deck::new(flat("test-red"), 0.0);
        let mut pipe = Pipeline::new(Output::default());
        let cap = Output::default().limiter;
        let mut prev = pipe.process(deck.render(DT, false, 1.0), DT).stats.luma;
        deck.switch(flat("test-white"), 2.0);
        for _ in 0..120 {
            let s = pipe.process(deck.render(DT, false, 1.0), DT).stats;
            assert!(s.luma - prev <= cap.max_rise_per_s * DT as f32 + 1e-4, "rose {} in a frame", s.luma - prev);
            assert!(s.apl <= cap.apl_cap + 1e-4, "{} is over the cap", s.apl);
            prev = s.luma;
        }
        assert!((pipe.process(deck.render(DT, false, 1.0), DT).stats.apl - cap.apl_cap).abs() < 0.01);
    }

    /// What a caller asks for, as the render loop reads it.
    #[test]
    fn fade_lengths_as_asked() {
        assert_eq!(fade_len(None), 2.0);
        assert_eq!(fade_len(Some(5.0)), 5.0);
        assert_eq!(fade_len(Some(0.0)), 0.0);
        assert_eq!(fade_len(Some(-3.0)), 0.0);
        assert_eq!(fade_len(Some(f32::NAN)), 2.0);
        assert_eq!(fade_len(Some(1e9)), f64::from(FADE_MAX));
    }
}
