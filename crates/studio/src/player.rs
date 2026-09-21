//! Players: **the only thing in the studio that renders.**
//!
//! Card 106 built one player per panel and left the design view its own
//! engine. Card 170 deleted that engine: there is one picture, the panel shows
//! it, and the page is a window onto it. So a player now does everything the
//! design view used to do as well - pause, speed, restart, a patch's own
//! actions - and the frames the browser draws are the same decoded datagrams
//! the panel is being sent.
//!
//! A player is the thing that is meant to be forgotten. It renders on its own
//! OS thread, sends through [`screeny::Link`] (which reconnects by itself), and
//! survives its own patch:
//!
//! - **a patch that panics** is caught by `catch_unwind`, logged once, and the
//!   player restarts on a safe fallback patch;
//! - **a patch that stalls** - a frame that never comes back - is noticed by a
//!   watchdog. The wedged thread is told to stop and **abandoned**, because no
//!   thread can be killed in Rust, and a *new* core is started on the fallback.
//!   The panel link is deliberately owned by the player rather than by the
//!   core, so abandoning a core leaks a patch's render state and one thread and
//!   never a socket, a link thread or the device itself. The core is behind no
//!   shared lock at all, which is why a wedged patch cannot take `/healthz` or
//!   `/api/v1/status` with it (card 143, closed by construction);
//! - **a patch that does either repeatedly** is refused: after
//!   [`MAX_FAULTS`] the player stops trying, says so once, and `/healthz` goes
//!   503. A restart loop is worse than a stopped player.
//!
//! **Changes are drained, not thrown at a new thread.** The page's sliders act
//! on the player, and dragging one is sixty changes a second; a change goes
//! into a one-slot [`Pending`] that the render loop applies between frames.
//! Only a panic or a stall replaces the thread, so `health.restarts` counts
//! faults rather than how often somebody changed their mind.
//!
//! Nothing here grows without bound. One core thread per player, one link, one
//! control request in flight, a one-slot pending mailbox, a fixed fallback
//! ladder, and every fault logged once rather than per frame. A player renders
//! at [`screeny_art::FPS`] - **the** rate, card 161; there is nothing to
//! configure - while its panel is connected **or** a browser is watching, and at
//! [`IDLE_FPS`] otherwise: a panel that is unplugged for a month, with nobody
//! looking, should not cost a core for a month.

// The frame-sink trait, in scope only so `SenderOutput::send` resolves. It is
// renamed here because card 150 gave `Output` to the settings block below.
use screeny_art::output::{Output as FrameSink, PanelStatus, SenderOutput};
use screeny_art::patch::{local_now, Ctx, Params, Patch, PatchDef, Playing};
use screeny_art::{Output, Pipeline};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::devices::Reach;
use crate::page::{self, Screen, StudioState};
use crate::state::{unix_now, StoredPlayer, Working, DEFAULT_SETTING, UNBOUND};

/// Write down what this player's current patch is set to, in the studio's one
/// patch memory. An unknown patch has no specs to compare against, so its
/// entry is left exactly as the file had it (card 165: "unknown patch id ->
/// entry ignored, kept in the file").
fn remember_current(cfg: &StoredPlayer, memory: &crate::state::SharedMemory, faults: bool) {
    if let Some(def) = find_patch(&cfg.patch, faults) {
        memory.remember(def, &cfg.params, cfg.seed, cfg.speed);
    }
}

/// Put `cfg` on `def`, restoring whatever that patch was last left set to -
/// here, or on another panel. One memory, one answer.
///
/// Card 151: **speed comes back with the parameters and the seed**, because it
/// is part of a setting and therefore part of the working copy.
///
/// And a patch the studio has never been on **arrives on Default** - its
/// declared defaults, `DEFAULT_SEED` and 1.00x - rather than inheriting the
/// seed and the speed of the patch that was playing a moment ago. That was
/// harmless while nothing compared them with anything; now that "modified" is
/// the working copy against Default, inheriting would mark a patch nobody has
/// ever touched as modified, which is not so.
fn recall_into(cfg: &mut StoredPlayer, def: &'static PatchDef, memory: &crate::state::SharedMemory, device: &str) {
    cfg.patch = def.id.to_string();
    let was = memory.recall(def, &format!("panel {}", label(device)));
    cfg.params = was.params;
    cfg.seed = was.seed.unwrap_or(crate::state::DEFAULT_SEED);
    cfg.speed = was.speed.unwrap_or(1.0).clamp(0.0, MAX_SPEED);
}

/// How long one frame may take before the player is treated as wedged.
/// Generous: a cold GPU patch's first frame is not a fault.
pub const WATCHDOG: Duration = Duration::from_secs(5);
/// Faults - panics or stalls - before a player gives up rather than looping.
pub const MAX_FAULTS: u32 = 3;
/// The rate a player renders at when its panel is not connected **and** no
/// browser is watching. Enough to drive reconnection and to be ready the
/// instant either of those changes.
///
/// Card 161 left this alone on purpose: it is not a rate anybody chooses or
/// sees, it is what a forgotten panel costs. Everything else is
/// [`screeny_art::FPS`].
pub const IDLE_FPS: f64 = 5.0;
/// How fast a patch may be played. Card 105's clamp, unchanged.
pub const MAX_SPEED: f64 = 8.0;

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

/// A device id for a log line. The unbound player has none.
fn label(device: &str) -> &str {
    if device.is_empty() {
        "(no panel yet)"
    } else {
        device
    }
}

// ----------------------------------------------------------------- health ---

/// What a player has been through. Everything on `/api/v1/status`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PlayerHealth {
    /// Frames rendered since the process started, across cores.
    pub ticks: u64,
    /// Patches that panicked mid-render.
    pub panics: u64,
    /// Frames that never came back, caught by the watchdog.
    pub stalls: u64,
    /// Core threads started after the first: the count of recoveries. Since
    /// card 170 a patch change does *not* start one, so this counts faults.
    pub restarts: u64,
    /// Wedged threads still out there. They go away when (if) their frame
    /// returns; each one is a patch's render state, never a socket or a link.
    pub abandoned: u64,
    /// The patch the player was asked for but cannot run, if it fell back.
    pub fell_back_from: Option<String>,
    /// Patches this player refuses to load again until a human says otherwise.
    pub refused: Vec<String>,
    /// Set when the player has given up: [`MAX_FAULTS`] faults in a row. The
    /// one player condition that makes the server unhealthy.
    pub gave_up: Option<String>,
    /// Seconds since the render loop last completed a frame.
    pub last_tick_ago: Option<f64>,
    /// Seconds since a frame last reached the wire.
    pub last_frame_ago: Option<f64>,
    /// Sessions the **current link object** has opened. Per link, so it goes
    /// back to zero whenever the link is rebuilt: useful for "is this link
    /// flapping", useless as "has this panel dropped". Read
    /// [`PlayerHealth::reconnects`] for that (card 171).
    pub sessions: u64,
    /// Every time this panel's stream has come up **since the studio
    /// started**, carried across link rebuilds the way `ticks` is carried
    /// across core restarts. One at the first connect.
    pub link_ups: u64,
    /// How many times the panel has come *back*: [`PlayerHealth::link_ups`]
    /// less the first connect, and less the times the studio itself let the
    /// panel go and picked it up again (output switched off and on).
    ///
    /// This is the one a human reads, and it is the number card 171 was
    /// written about: the old readout was `sessions - 1`, which the studio
    /// reset every time it rebuilt the link, so a panel that had really
    /// reconnected three times could show `0`.
    pub reconnects: u64,
    /// The brightness policy as actually applied by the device (its own cap
    /// may be lower than what was asked for).
    pub brightness_applied: Option<u8>,
    /// The device's own ceiling, learned the only way there is: ask for more
    /// than it will give and see what comes back. The page's slider never
    /// goes above this.
    pub brightness_cap: Option<u8>,
    /// The last thing that went wrong here, if anything has.
    pub last_error: Option<String>,
}

/// A player, as the page and `/api/v1/status` see it.
#[derive(Clone, Debug, Serialize)]
pub struct PlayerStatus {
    /// The device id, or empty for the player that has no panel yet.
    pub device: String,
    /// Panel output: false means the link is released and the panel is on its
    /// own idle screen. The player keeps rendering either way.
    pub on: bool,
    pub patch: String,
    pub patch_name: String,
    pub seed: u32,
    /// Only what has been set away from the patch's defaults.
    pub params: BTreeMap<String, f32>,
    /// The rate this is rendered at. Always [`screeny_art::FPS`] since card
    /// 161: reported, never chosen. Kept on the status so a script need not
    /// write 30 down itself.
    pub fps: f64,
    pub paused: bool,
    pub speed: f64,
    pub brightness: Option<u8>,
    pub output: Output,
    /// True when a core thread is alive and ticking.
    pub running: bool,
    /// True when this is the player the page is a window onto.
    pub focused: bool,
    /// The rate the render loop is actually achieving.
    pub fps_measured: f32,
    /// What a composing patch says it is performing.
    pub playing: Option<Playing>,
    pub health: PlayerHealth,
    /// The link, or `None` when panel output is off.
    pub panel: Option<PanelStatus>,
}

// ----------------------------------------------------------------- a core ---

/// The render state of one patch. Thrown away whole when it misbehaves.
struct Core {
    def: &'static PatchDef,
    patch: Box<dyn Patch>,
    params: Params,
    pipeline: Pipeline,
    seed: u32,
    t: f64,
    last: Instant,
    fps: f32,
}

impl Core {
    /// One frame: the patch, the pipeline, and what the page is shown.
    ///
    /// `paused` and `speed` are card 105's, moved here with the rest of the
    /// design view: the patch's clock is scaled, the pipeline's is not,
    /// because the limiter measures wall-clock rise.
    fn tick(&mut self, paused: bool, speed: f64) -> screeny_art::pipeline::Processed {
        let now = Instant::now();
        let wall = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let dt = if paused { 0.0 } else { wall * speed };
        self.t += dt;
        let frame = self.patch.render(&Ctx { t: self.t, dt, now: local_now(), params: &self.params });
        if wall > 0.0 {
            self.fps += (1.0 / wall as f32 - self.fps) * 0.1;
        }
        self.pipeline.process(frame, wall)
    }

    /// The patch's parameters, as the configuration now says.
    fn reload_params(&mut self, cfg: &StoredPlayer) {
        self.params = Params::defaults(self.def.params);
        for (id, v) in &cfg.params {
            self.params.set(self.def.params, id, *v);
        }
    }

    /// Build the patch again from its seed and start its clock over.
    fn restart(&mut self) {
        self.patch = (self.def.make)(u64::from(self.seed));
        self.pipeline.reset();
        self.t = 0.0;
    }
}

/// The handle the player and the watchdog share with a running core.
struct CoreHandle {
    gen: u64,
    /// Set to stop *this* core. A wedged core reads it when its frame returns.
    stop: AtomicBool,
    /// Cleared by the thread on its way out, however it goes out.
    alive: AtomicBool,
    /// Milliseconds since the epoch, stamped before every frame.
    beat: AtomicU64,
    /// Frames this player has rendered, **shared with the player**: a core
    /// that is replaced must not reset the count, or a restart would look
    /// like a render loop that had stopped.
    ticks: Arc<AtomicU64>,
    fps: Mutex<f32>,
    /// What is being performed, **and by which patch**.
    ///
    /// The pair matters: a change is applied by the render loop before its
    /// *next* frame, so for up to one frame period the configuration says one
    /// patch and the core is still running another. "What is it performing"
    /// is only true of the patch performing it, and answering with the old
    /// patch's answer would put the wrong thing on the page.
    playing: Mutex<(&'static str, Option<Playing>)>,
}

/// What a core is performing, if it is running the patch that was asked for.
fn performing(handle: &CoreHandle, want: &str) -> Option<Playing> {
    let slot = handle.playing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (slot.0 == want).then(|| slot.1.clone()).flatten()
}

/// A brightness the caller should apply, off the render thread.
pub struct BrightnessJob {
    pub device: String,
    pub level: u8,
}

/// What the render loop must do before its next frame.
///
/// One slot, newest wins, like every other mailbox in this server. A slider
/// being dragged sets `params` sixty times a second and costs one re-read of
/// the parameter map per frame rather than sixty thread restarts.
#[derive(Default)]
struct Pending {
    /// The patch or the seed changed: a fresh core.
    rebuild: bool,
    /// Re-read the parameters into the running core.
    params: bool,
    /// Re-read the pipeline output.
    output: bool,
    /// Rebuild the patch from its seed, keeping everything else.
    restart: bool,
    /// One action a composing patch offered (card 140).
    action: Option<String>,
}

impl Pending {
    fn anything(&self) -> bool {
        self.rebuild || self.params || self.output || self.restart || self.action.is_some()
    }
}

// --------------------------------------------------------------- a player ---

/// One panel's player - and, when it is the focused one, the page's picture.
pub struct Player {
    cfg: Mutex<StoredPlayer>,
    /// The panel link, outside the core on purpose: a core can be abandoned
    /// without losing the link, the device or the source lock.
    link: Mutex<LinkSlot>,
    core: Mutex<Option<Arc<CoreHandle>>>,
    health: Mutex<PlayerHealth>,
    pending: Mutex<Pending>,
    stop: Arc<AtomicBool>,
    faults: bool,
    gen: AtomicU64,
    /// Frames rendered, across every core this player has had.
    ticks: Arc<AtomicU64>,
    /// The frame packet's sequence number, across cores for the same reason.
    seq: Arc<AtomicU32>,
    /// True when this is the player the page is showing. Only the focused
    /// player fills the page's frame cell.
    focused: AtomicBool,
    /// The page's one frame cell, and how many browsers are reading it.
    screen: Arc<Screen>,
    /// The studio's one patch memory (card 165), shared with every other
    /// player.
    memory: crate::state::SharedMemory,
    /// Faults since the last good run: the brake on a restart loop.
    consecutive: AtomicU64,
}

#[derive(Default)]
struct LinkSlot {
    out: Option<SenderOutput>,
    /// What the link was built for, so a changed address rebuilds it and an
    /// unchanged one does not.
    key: String,
    /// Sessions the **current** link had the last time we looked.
    sessions: u32,
    /// Sessions banked from every link this player has already closed.
    ///
    /// Card 171: the studio builds a new link whenever what it is aiming at
    /// changes, and the link's own counter starts again at zero. Banking it
    /// here is what makes "how many times has this panel's stream come up" a
    /// *player* lifetime number rather than a per-link one.
    closed_ups: u64,
    /// Of those, the ones the **studio** caused rather than the panel.
    /// Subtracted from the reconnect count, because "the panel dropped" and
    /// "the studio re-aimed" are not the same thing and only the first is
    /// worth a person's attention. There are exactly two:
    ///
    /// - output was switched off and then on again;
    /// - the studio **learned where the device really is** - a panel typed in
    ///   as an address is re-aimed at the resolved device within seconds of
    ///   being added, and that rebuild must not read as the panel having gone
    ///   away and come back, or every freshly attached panel would say 1.
    ///
    /// A link rebuilt from one *resolved* device to another - a panel that
    /// moved, or that was re-resolved after a stale period - is **not**
    /// counted here: the panel really was away.
    studio_ups: u64,
    /// Whether the current link was built from a resolved device, for the
    /// second case above.
    from_resolved: bool,

    /// Ask the device for the brightness policy again on the next pass, even
    /// though no new session has opened - because the policy itself changed.
    reapply_brightness: bool,
    /// When a frame last reached the wire.
    last_frame_unix: Option<u64>,
}

impl LinkSlot {
    /// Drop the current link and bank what it reached (card 171).
    ///
    /// `studio_took_a_live_stream` says the studio is replacing a link that is
    /// **up right now** - so the connect that follows is this studio getting
    /// back what it just let go of, not the panel coming back, and it does not
    /// count as a reconnect.
    ///
    /// "Up right now" is the whole condition, and both halves of it were paid
    /// for:
    ///
    /// - a link that never connected costs no extra session when it is
    ///   replaced, so discounting one hid the very first reconnect on a panel
    ///   whose typed address resolved before the link had finished connecting;
    /// - a link that is **down** is down because the panel is away, so the
    ///   connect that follows replacing it is the panel returning. Discounting
    ///   that swallowed a real reconnect whenever a re-aim happened to land in
    ///   the window while the panel was unplugged.
    ///
    /// `reached` is the closing link's **own** session count, read from it
    /// here rather than from `self.sessions`: that field is the supervisor's
    /// once-a-second copy, and a link built and torn down between two ticks
    /// would bank a zero it had not earned.
    fn close_link(&mut self, reached: u32, studio_took_a_live_stream: bool) {
        self.closed_ups += u64::from(reached);
        if studio_took_a_live_stream {
            self.studio_ups += 1;
        }
        self.out = None;
        self.sessions = 0;
    }
}

impl Player {
    #[must_use]
    pub fn new(cfg: StoredPlayer, faults: bool, memory: crate::state::SharedMemory, screen: Arc<Screen>) -> Arc<Player> {
        Arc::new(Player {
            memory,
            cfg: Mutex::new(cfg),
            link: Mutex::new(LinkSlot::default()),
            core: Mutex::new(None),
            health: Mutex::new(PlayerHealth::default()),
            pending: Mutex::new(Pending::default()),
            stop: Arc::new(AtomicBool::new(false)),
            faults,
            gen: AtomicU64::new(0),
            ticks: Arc::new(AtomicU64::new(0)),
            seq: Arc::new(AtomicU32::new(0)),
            focused: AtomicBool::new(false),
            screen,
            consecutive: AtomicU64::new(0),
        })
    }

    /// The device this plays to. Empty while no panel is attached.
    #[must_use]
    pub fn device(&self) -> String {
        self.cfg().device.clone()
    }

    /// True when this is the player the page is a window onto.
    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    fn cfg(&self) -> MutexGuard<'_, StoredPlayer> {
        self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn slot(&self) -> MutexGuard<'_, LinkSlot> {
        self.link.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn health_mut(&self) -> MutexGuard<'_, PlayerHealth> {
        self.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn pending_mut(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn take_pending(&self) -> Pending {
        std::mem::take(&mut *self.pending_mut())
    }

    /// The desired configuration, as persisted.
    #[must_use]
    pub fn stored(&self) -> StoredPlayer {
        self.cfg().clone()
    }

    /// **The working copy** of the patch this player is on: what a setting
    /// holds, as it is set right now (card 151).
    ///
    /// `None` for a patch this build has not got - there is no spec to measure
    /// the values against, so there is nothing honest to save.
    #[must_use]
    pub fn working(&self) -> Option<(&'static PatchDef, Working)> {
        let cfg = self.cfg();
        let def = find_patch(&cfg.patch, self.faults)?;
        Some((
            def,
            Working {
                params: crate::state::sparse(def, &cfg.params),
                seed: cfg.seed,
                speed: cfg.speed,
            },
        ))
    }

    /// What the page draws itself from: the patch, the seed, **every**
    /// parameter at its effective value, the pipeline output, the playback
    /// state and - card 151 - the patch's settings, which one is loaded and
    /// whether it has been moved since.
    #[must_use]
    pub fn state(&self) -> StudioState {
        let cfg = self.cfg().clone();
        let def = find_patch(&cfg.patch, self.faults);
        let params = match def {
            Some(def) => {
                let mut p = Params::defaults(def.params);
                for (id, v) in &cfg.params {
                    p.set(def.params, id, *v);
                }
                p.iter().map(|(k, v)| (k.to_string(), v)).collect()
            }
            // A patch this build has not got: the page shows no sliders for
            // it rather than inventing some.
            None => BTreeMap::new(),
        };
        // The list, the name and the mark travel with the state, so a browser
        // needs no second read and the other browsers see a save the moment it
        // happens (card 151).
        let (setting, settings, modified) = match def {
            Some(def) => {
                let work = Working { params: crate::state::sparse(def, &cfg.params), seed: cfg.seed, speed: cfg.speed };
                (
                    self.memory.current_setting(def.id),
                    self.memory.setting_names(def.id),
                    self.memory.modified(def, &work),
                )
            }
            None => (DEFAULT_SETTING.to_string(), Vec::new(), false),
        };
        StudioState {
            patch: cfg.patch,
            seed: cfg.seed,
            params,
            output: cfg.output,
            paused: cfg.paused,
            speed: cfg.speed,
            // Card 161: the one rate, reported so the page and any script can
            // read it instead of writing 30 down again.
            fps: screeny_art::FPS,
            on: cfg.on,
            device: cfg.device,
            setting,
            settings,
            modified,
        }
    }

    /// Change what it plays. Anything `None` is left alone.
    ///
    /// Nothing here stops a thread: what has to change is written into the
    /// one-slot [`Pending`] and applied by the render loop before its next
    /// frame, so the page's sliders cost one re-read per frame rather than a
    /// thread each.
    ///
    /// # Errors
    ///
    /// If no patch has that id, or the current patch has no such parameter.
    pub fn configure(self: &Arc<Self>, change: &PlayerChange) -> Result<(), String> {
        let mut want = Pending::default();
        {
            let mut cfg = self.cfg();
            if let Some(id) = &change.patch {
                let def = find_patch(id, self.faults).ok_or_else(|| format!("no patch called `{id}`"))?;
                if cfg.patch != def.id {
                    // Arrive at the new patch set up the way it was left, by
                    // whoever last touched it (card 165). Nothing to write down
                    // on the way out: every change went into the memory when it
                    // was made.
                    let device = cfg.device.clone();
                    recall_into(&mut cfg, def, &self.memory, &device);
                    want.rebuild = true;
                }
                // Asking for a patch again clears its refusal: a human saying
                // "try it" outranks the brake.
                let mut h = self.health_mut();
                h.refused.retain(|r| r != def.id);
                h.gave_up = None;
                h.fell_back_from = None;
                self.consecutive.store(0, Ordering::Relaxed);
            }
            // Card 151. **One change**, not one per parameter: the whole
            // working copy is replaced here and the caller publishes once and
            // writes once, so a panel follows a load at once rather than
            // walking through a burst of intermediate pictures.
            //
            // First, so that a change that loads a setting *and* moves
            // something - which is what the page does not do but a script may -
            // ends up with the movement on top rather than under.
            if let Some(name) = &change.load_setting {
                let def = find_patch(&cfg.patch, self.faults)
                    .ok_or_else(|| format!("`{}` is not a patch this build has, so it has no settings", cfg.patch))?;
                let (work, repaired) = self.memory.load_setting(def, name)?;
                if !repaired.is_empty() {
                    // The `repaired` voice, said once per load rather than per
                    // value or per frame: a setting older than the patch is
                    // exactly what this is for, not an error.
                    eprintln!("studio: player {}: `{}` loading `{name}`: {}", label(&cfg.device), def.id, repaired.join("; "));
                }
                cfg.params = work.params;
                cfg.seed = work.seed;
                cfg.speed = work.speed.clamp(0.0, MAX_SPEED);
                want.rebuild = true;
            }
            if let Some(seed) = change.seed {
                cfg.seed = seed;
                remember_current(&cfg, &self.memory, self.faults);
                want.rebuild = true;
            }
            if let Some((id, value)) = &change.param {
                let def = find_patch(&cfg.patch, self.faults);
                let Some(spec) = def.and_then(|d| d.params.iter().find(|p| p.id == id)) else {
                    return Err(format!("{} has no parameter `{id}`", cfg.patch));
                };
                cfg.params.insert(id.clone(), spec.sanitise(*value));
                remember_current(&cfg, &self.memory, self.faults);
                want.params = true;
            }
            // "Back to the defaults, and stay there": the remembered
            // parameters go with them, or the next switch back would hand the
            // old values straight over again.
            if change.reset_params {
                cfg.params.clear();
                self.memory.forget_params(&cfg.patch);
                want.params = true;
            }
            if let Some(paused) = change.paused {
                cfg.paused = paused;
            }
            if let Some(speed) = change.speed {
                cfg.speed = speed.clamp(0.0, MAX_SPEED);
                // Card 151: speed is part of a setting, so it is part of what
                // is remembered about the patch rather than only of the
                // player. `paused` is not - it is about playback, not about
                // the patch. (`fps` used to be listed here too; card 161
                // removed it as a setting of anything.)
                remember_current(&cfg, &self.memory, self.faults);
            }
            if let Some(s) = change.output {
                cfg.output = s;
                want.output = true;
            }
            if let Some(b) = change.brightness {
                cfg.brightness = b;
                // Re-apply on the next pass. Card 171: this used to be
                // `slot.sessions = 0`, which made the supervisor see a new
                // session that had not happened - and, once reconnects were
                // counted, would have invented one.
                self.slot().reapply_brightness = true;
            }
            if let Some(on) = change.on {
                cfg.on = on;
            }
            if change.restart {
                want.restart = true;
            }
        }
        if let Some(action) = &change.act {
            want.action = Some(action.clone());
        }
        if want.anything() {
            let mut p = self.pending_mut();
            p.rebuild |= want.rebuild;
            p.params |= want.params;
            p.output |= want.output;
            p.restart |= want.restart;
            if want.action.is_some() {
                // One slot: a burst of button presses is the newest one.
                p.action = want.action;
            }
        }
        // Deliberately *not* `ensure_running`: starting a thread is the
        // supervisor's job (and the API's, right after a change, so a browser
        // does not wait a second for a player that had given up). A change is
        // arithmetic on what would play, and stays that way in a test.
        Ok(())
    }

    /// Rename this player onto a device - the moment a panel is found and
    /// adopted, or a `pending:` id becomes the device's real one.
    ///
    /// **The core is not touched**, which is the whole point: the picture the
    /// page is showing carries straight on to the panel.
    pub fn rename(&self, device: &str) {
        self.cfg().device = device.to_string();
    }

    /// Point the link at a device, or at nothing. Rebuilds it only when what
    /// it is aimed at has actually changed.
    ///
    /// Card 171: this is also where the reconnect count is kept honest. A
    /// link the studio throws away takes its own session counter with it, so
    /// what it reached is banked in `closed_ups` - and when the *studio* is
    /// the reason the stream will come up again, that one is booked to
    /// `studio_ups` so it does not read as the panel having dropped.
    pub fn aim(self: &Arc<Self>, reach: &Reach) {
        let on = self.cfg().on;
        let key = reach_key(reach);
        let mut slot = self.slot();
        if !on || matches!(reach, Reach::Unknown) {
            if let Some(out) = slot.out.as_mut() {
                // Switching output off takes away a stream that was running,
                // so the connect that follows switching it back on is not the
                // panel coming back. Losing the device's address
                // (`Reach::Unknown`) is the opposite: the panel really is away.
                let took_a_live_stream = !on && out.link().state().is_up();
                let reached = out.link().stats().sessions;
                // FINAL: the panel is released now rather than after its
                // stream timeout, and goes back to its own idle screen.
                out.close();
                slot.close_link(reached, took_a_live_stream);
            }
            slot.key = String::new();
            slot.from_resolved = false;
            return;
        }
        if slot.out.is_some() && slot.key == key {
            return;
        }
        let resolved = matches!(reach, Reach::Resolved(_));
        if let Some(out) = slot.out.as_ref() {
            // The studio finding out where the device really is. A panel
            // typed in as an address is re-aimed at the resolved device
            // within seconds of being added, and the panel has not moved an
            // inch; without this every freshly attached panel would read
            // "Reconnects 1" before anyone had touched it. One resolved
            // device to *another* is not this: that panel was away.
            //
            // And only while the stream it is replacing is **up**: a link
            // that is down is down because the panel is away, so the connect
            // that follows is the panel returning and must be counted.
            let took_a_live_stream = resolved && !slot.from_resolved && out.link().state().is_up();
            let reached = out.link().stats().sessions;
            slot.close_link(reached, took_a_live_stream);
        }
        slot.out = Some(open_link(reach));
        slot.key = key;
        slot.sessions = 0;
        slot.from_resolved = resolved;
    }

    /// Start the render loop if it should be running and is not.
    ///
    /// Since card 170 this does not depend on `on`: panel output being off
    /// releases the link, it does not stop the picture.
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
    ///
    /// Returns a brightness to apply if the policy says so and the link has
    /// just come up - done by the caller, off this thread, because a control
    /// request can take a second and the render loop must never wait for one.
    pub fn supervise(self: &Arc<Self>) -> Option<BrightnessJob> {
        if self.stop.load(Ordering::Relaxed) {
            return None;
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
            let patch = self.cfg().patch.clone();
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

        // Drive reconnection and read the link even when frames are not
        // flowing, and notice a new session.
        let mut job = None;
        let (want, device) = {
            let cfg = self.cfg();
            (cfg.brightness, cfg.device.clone())
        };
        let mut slot = self.slot();
        let last = slot.sessions;
        let reapply = slot.reapply_brightness;
        let mut sessions = 0;
        let mut up = false;
        if let Some(out) = slot.out.as_mut() {
            out.poll();
            sessions = out.link().stats().sessions;
            up = out.link().state().is_up();
        }
        if up && (sessions != last || reapply) {
            slot.reapply_brightness = false;
            if let Some(level) = want {
                job = Some(BrightnessJob { device, level });
            }
        }
        slot.sessions = sessions;
        // Card 171: the count a person reads is a *player* lifetime one. The
        // link's own counter starts again at zero every time the link is
        // rebuilt, so what is banked from the closed links is added back, and
        // the first connect - plus any the studio itself caused - is taken off.
        let link_ups = slot.closed_ups + u64::from(sessions);
        let reconnects = link_ups.saturating_sub(1 + slot.studio_ups);
        drop(slot);
        let mut health = self.health_mut();
        health.sessions = u64::from(sessions);
        health.link_ups = link_ups;
        health.reconnects = reconnects;
        drop(health);
        job
    }

    /// Record what the device actually applied, which may be below what was
    /// asked for: the firmware caps brightness and says so, and that is the
    /// only way there is of learning where its ceiling is.
    pub fn brightness_applied(&self, asked: u8, applied: u8) {
        {
            let mut h = self.health_mut();
            h.brightness_applied = Some(applied);
            if applied < asked {
                h.brightness_cap = Some(applied);
            }
        }
        // Do not keep asking for something this panel will not give: the
        // policy becomes what it actually does, so the state file and the
        // page both say the true number.
        let mut cfg = self.cfg();
        if cfg.brightness == Some(asked) && applied < asked {
            cfg.brightness = Some(applied);
        }
    }

    /// Card 164: what this player's link has put on the wire and taken off it,
    /// over the life of the link object. Read once a second by the supervisor,
    /// which banks the difference per device; nothing on the frame path is
    /// involved.
    #[must_use]
    pub fn link_traffic(&self) -> screeny::LinkTraffic {
        self.slot().out.as_ref().map_or_else(Default::default, |o| o.link().stats().traffic)
    }

    /// The brightness policy, and what the device last said it applied.
    ///
    /// Compared against the device's own telemetry once a second, which is how
    /// a panel that came back at its own default gets put right again without
    /// waiting for the link to notice anything.
    #[must_use]
    pub fn brightness_policy(&self) -> (Option<u8>, Option<u8>) {
        (self.cfg().brightness, self.health_mut().brightness_applied)
    }

    /// Everything `/api/v1/status` shows.
    #[must_use]
    pub fn status(&self) -> PlayerStatus {
        let cfg = self.cfg().clone();
        let (running, ticks, fps_measured, playing) = {
            let core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match core.as_ref() {
                Some(h) => (
                    h.alive.load(Ordering::Relaxed),
                    h.ticks.load(Ordering::Relaxed),
                    *h.fps.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
                    performing(h, &cfg.patch),
                ),
                None => (false, 0, 0.0, None),
            }
        };
        let mut health = self.health_mut().clone();
        health.ticks = ticks;
        health.last_tick_ago = {
            let core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            core.as_ref().map(|h| (unix_millis().saturating_sub(h.beat.load(Ordering::Relaxed))) as f64 / 1000.0)
        };
        let panel = {
            let slot = self.slot();
            health.last_frame_ago = slot.last_frame_unix.map(|t| unix_now().saturating_sub(t) as f64);
            slot.out.as_ref().map(SenderOutput::status)
        };
        let name = find_patch(&cfg.patch, self.faults).map_or("", |d| d.name).to_string();
        PlayerStatus {
            device: cfg.device,
            on: cfg.on,
            patch: cfg.patch,
            patch_name: name,
            seed: cfg.seed,
            params: cfg.params,
            fps: screeny_art::FPS,
            paused: cfg.paused,
            speed: cfg.speed,
            brightness: cfg.brightness,
            output: cfg.output,
            running,
            focused: self.is_focused(),
            fps_measured,
            playing,
            health,
            panel,
        }
    }

    /// What a composing patch says it is performing, from the last frame.
    ///
    /// `None` while the render loop has yet to pick up a patch change: what
    /// the *previous* patch was performing is not an answer to this question.
    #[must_use]
    pub fn playing(&self) -> Option<Playing> {
        let want = self.cfg().patch.clone();
        let core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        core.as_ref().and_then(|h| performing(h, &want))
    }

    /// Stop for good: the core ends and the panel is released.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.stop_core();
        let mut slot = self.slot();
        if let Some(out) = slot.out.as_mut() {
            out.close();
        }
        slot.out = None;
    }

    // ---- internals ----

    fn stop_core(&self) {
        let mut core = self.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(h) = core.take() {
            h.stop.store(true, Ordering::Relaxed);
        }
    }

    /// One fault: log it once, count it, and either fall back or give up.
    fn fault(self: &Arc<Self>, patch: &str, what: &str) {
        let n = self.consecutive.fetch_add(1, Ordering::Relaxed) + 1;
        let fallback = fallback_patch(patch);
        let mut cfg = self.cfg();
        let mut h = self.health_mut();
        h.last_error = Some(what.to_string());
        if n >= u64::from(MAX_FAULTS) {
            let msg = format!("{what}; giving up after {n} faults");
            eprintln!("studio: player {}: {msg}", label(&cfg.device));
            h.gave_up = Some(msg);
            if !h.refused.iter().any(|r| r == patch) {
                h.refused.push(patch.to_string());
            }
            // `on` is left alone: the player stays configured, just silent, so
            // that a human can see what it was meant to be playing.
            drop(cfg);
            return;
        }
        eprintln!("studio: player {}: {what}; falling back to `{}`", label(&cfg.device), fallback.id);
        if !h.refused.iter().any(|r| r == patch) {
            h.refused.push(patch.to_string());
        }
        h.fell_back_from = Some(patch.to_string());
        // The fallback arrives set up the way it was last left, like any other
        // patch change. The failing patch's memory is untouched: a fault is not
        // a reason to forget how somebody had it set.
        let device = cfg.device.clone();
        recall_into(&mut cfg, fallback, &self.memory, &device);
    }

    /// A core on whatever the configuration says, or on the fallback when that
    /// cannot be run. Never fails: a player always has something to show.
    fn make_core(&self) -> Core {
        let (def, seed, params_map, output, device) = {
            let cfg = self.cfg();
            (cfg.patch.clone(), cfg.seed, cfg.params.clone(), cfg.output, cfg.device.clone())
        };
        // A patch that is unknown, or one this player has refused, becomes the
        // fallback rather than a reason not to run.
        let refused = self.health_mut().refused.clone();
        let chosen = match find_patch(&def, self.faults) {
            Some(d) if !refused.contains(&def) => d,
            _ => {
                let f = fallback_patch(&def);
                if find_patch(&def, self.faults).is_none() {
                    let mut h = self.health_mut();
                    if h.fell_back_from.as_deref() != Some(def.as_str()) {
                        eprintln!("studio: player {}: no patch called `{def}`; playing `{}`", label(&device), f.id);
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
        Core {
            def: chosen,
            patch: (chosen.make)(u64::from(seed)),
            params,
            pipeline: Pipeline::new(output),
            seed,
            t: 0.0,
            last: Instant::now(),
            fps: 0.0,
        }
    }

    fn start_core(self: &Arc<Self>) {
        let core = self.make_core();
        let gen = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = Arc::new(CoreHandle {
            gen,
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            beat: AtomicU64::new(unix_millis()),
            ticks: Arc::clone(&self.ticks),
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
            .name(format!("play-{}", short(&self.device())))
            .spawn(move || run_core(&me, &handle, core));
        if let Err(e) = started {
            let mut h = self.health_mut();
            h.gave_up = Some(format!("could not start a render thread: {e}"));
            eprintln!("studio: player: could not start a render thread: {e}");
        }
    }
}

/// What a change asks for. Everything optional; `None` leaves it alone.
#[derive(Clone, Debug, Default)]
pub struct PlayerChange {
    /// Panel output. False releases the link; the picture carries on.
    pub on: Option<bool>,
    pub patch: Option<String>,
    pub seed: Option<u32>,
    pub param: Option<(String, f32)>,
    /// "Reset": back to the patch's defaults, and forget what was remembered
    /// for it (card 165).
    pub reset_params: bool,
    /// Card 151: put the working copy on this named setting of the current
    /// patch - or on `Default`. One change, whatever it moves.
    pub load_setting: Option<String>,
    pub paused: Option<bool>,
    pub speed: Option<f64>,
    pub output: Option<Output>,
    /// `Some(None)` clears the brightness policy; `Some(Some(n))` sets it.
    pub brightness: Option<Option<u8>>,
    /// Start the patch again from its seed.
    pub restart: bool,
    /// An action a composing patch offered (card 140).
    pub act: Option<String>,
}

/// The render loop of one core.
///
/// Everything that can change about what is being played arrives through the
/// player's one-slot [`Pending`] and is applied here, between frames, so
/// nothing a browser does costs a thread.
fn run_core(player: &Arc<Player>, handle: &Arc<CoreHandle>, core: Core) {
    let mut core = core;
    let mut patch_id = core.def.id.to_string();
    let mut next = Instant::now();
    let mut limits_at = Instant::now() - Duration::from_secs(10);
    let mut panicked = false;

    while !handle.stop.load(Ordering::Relaxed) && !player.stop.load(Ordering::Relaxed) {
        handle.beat.store(unix_millis(), Ordering::Relaxed);

        let (paused, speed) = {
            let cfg = player.cfg();
            (cfg.paused, cfg.speed)
        };

        // Apply whatever has been asked for since the last frame, then render.
        // The patch is the only code in here that can panic - `act` as much as
        // `render` - so both are inside the same `catch_unwind`, which is what
        // keeps one bad patch from taking the process, and the panel, with it.
        let want = player.take_pending();
        if want.rebuild {
            core = player.make_core();
            patch_id = core.def.id.to_string();
        }
        // Read the configuration once, and only when something needs it: this
        // runs sixty times a second.
        let reconf = (want.params || want.output).then(|| player.stored());
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !want.rebuild {
                if let Some(cfg) = &reconf {
                    if want.params {
                        core.reload_params(cfg);
                    }
                    if want.output {
                        core.pipeline.output = cfg.output;
                    }
                }
                if want.restart {
                    core.restart();
                }
            }
            if let Some(action) = &want.action {
                core.patch.act(action);
            }
            core.tick(paused, speed)
        }));
        let Ok(out) = rendered else {
            panicked = true;
            break;
        };
        handle.ticks.fetch_add(1, Ordering::Relaxed);
        *handle.fps.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = core.fps;
        if let Ok(mut p) = handle.playing.lock() {
            *p = (core.def.id, core.patch.playing());
        }

        // The page, if this is the player it is a window onto. One slot,
        // replaced in place: a browser that is not keeping up misses frames
        // and costs nothing, and this never blocks, so a browser cannot hold
        // the render loop or the panel link back.
        if player.is_focused() {
            let seq = player.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            player.screen.show(page::pack(seq, core.t, &out.stats, core.fps, &out.preview));
        }

        // Hand it to the panel. The link is the player's, not the core's.
        let connected = {
            let mut slot = player.slot();
            let mut landed = false;
            let up = match slot.out.as_mut() {
                Some(link) => {
                    if limits_at.elapsed() >= Duration::from_secs(1) {
                        limits_at = Instant::now();
                        let lim = link.limits();
                        if lim.connected {
                            core.pipeline.meter().set_limits(lim.budget, lim.codecs.clone());
                        }
                    }
                    let before = link.link().stats().frames_sent;
                    if let Err(e) = link.send(&out.wire) {
                        // Only our own errors can get here; the network cannot
                        // fail a send. Logged once.
                        let mut h = player.health_mut();
                        let msg = format!("sending to the panel: {e}");
                        if h.last_error.as_deref() != Some(msg.as_str()) {
                            eprintln!("studio: player {}: {msg}", label(&player.device()));
                        }
                        h.last_error = Some(msg);
                    }
                    landed = link.link().stats().frames_sent > before;
                    link.link().state().is_up()
                }
                None => false,
            };
            if landed {
                slot.last_frame_unix = Some(unix_now());
            }
            up
        };

        // A run of good frames clears the fault brake.
        if handle.ticks.load(Ordering::Relaxed).is_multiple_of(300) {
            player.consecutive.store(0, Ordering::Relaxed);
        }

        // Full rate while the panel is connected or a browser is watching.
        // Otherwise there is nothing to be fast for: a panel unplugged for a
        // month, with nobody looking, must not cost a core for a month.
        //
        // Card 120: *watching*, not *connected*. A tab that has been switched
        // away from asks for no frames and gives up its claim, so a studio with
        // no panel and only hidden tabs open idles here too - and picks up
        // again within one idle frame when somebody looks.
        // Card 161: the full rate is `screeny_art::FPS` and nothing else. It
        // is the panel's own rate, so the link's cadence ceiling has nothing
        // to fold away - before this card the player rendered at 60 and half
        // of it was coalesced.
        let watched = player.is_focused() && player.screen.watchers() > 0;
        let rate = if connected || watched { screeny_art::FPS } else { IDLE_FPS };
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
        player.health_mut().panics += 1;
        player.fault(&patch_id, &format!("`{patch_id}` panicked while rendering"));
        // Only the generation that panicked may start the replacement: a core
        // that was stopped on purpose must not resurrect itself.
        let mine = {
            let core = player.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            core.as_ref().is_some_and(|h| h.gen == handle.gen)
        };
        if mine && !player.stop.load(Ordering::Relaxed) {
            *player.core.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            player.ensure_running();
        }
    }
}

/// Build the link for a way of reaching a device.
fn open_link(reach: &Reach) -> SenderOutput {
    match reach {
        // Resolved by the registry: attach to exactly these two ports and
        // never browse (card 111). It survives a reboot at the same address.
        Reach::Resolved(d) => SenderOutput::attach_deferred((**d).clone(), screeny::LinkConfig::default()),
        // A name a human typed: re-resolved on every reconnect, so it follows
        // the device across a DHCP lease.
        Reach::Name(n) => SenderOutput::deferred(screeny_art::output::target_for(n)),
        Reach::Addr(a) => SenderOutput::deferred(screeny_art::output::target_for(&a.to_string())),
        Reach::Unknown => SenderOutput::deferred(screeny_art::output::target_for("")),
    }
}

fn reach_key(reach: &Reach) -> String {
    match reach {
        Reach::Resolved(d) => format!("dev:{}|{}", d.frame, d.control),
        Reach::Name(n) => format!("name:{n}"),
        Reach::Addr(a) => format!("addr:{a}"),
        Reach::Unknown => String::new(),
    }
}

fn short(id: &str) -> String {
    if id.is_empty() {
        return "page".into();
    }
    id.chars().take(8).collect()
}

#[must_use]
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ------------------------------------------------------------ the fleet ----

/// Every player, keyed by device id. A collection from day one, with one panel
/// as the expected case: the page shows the **focused** one, and with a single
/// panel nobody ever has to choose.
pub struct Players {
    inner: Mutex<BTreeMap<String, Arc<Player>>>,
    /// Which player the page is a window onto.
    focus: Mutex<String>,
    /// The studio's one patch memory, handed to every player made here.
    memory: crate::state::SharedMemory,
    /// The page's one frame cell, likewise.
    screen: Arc<Screen>,
}

impl Players {
    #[must_use]
    pub fn new(memory: crate::state::SharedMemory, screen: Arc<Screen>) -> Self {
        Players { inner: Mutex::new(BTreeMap::new()), focus: Mutex::new(UNBOUND.to_string()), memory, screen }
    }

    /// The studio's one patch memory, as handed to every player here.
    #[must_use]
    pub fn memory(&self) -> crate::state::SharedMemory {
        self.memory.clone()
    }

    /// The page's frame cell.
    #[must_use]
    pub fn screen(&self) -> Arc<Screen> {
        Arc::clone(&self.screen)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Player>>> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The lock order everywhere in this type: `inner`, then `focus`. Never
    /// the other way round, so these two cannot be half of a deadlock.
    fn focus_mut(&self) -> MutexGuard<'_, String> {
        self.focus.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn get(&self, device: &str) -> Option<Arc<Player>> {
        self.lock().get(device).cloned()
    }

    #[must_use]
    pub fn all(&self) -> Vec<Arc<Player>> {
        self.lock().values().cloned().collect()
    }

    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// Device ids that actually name a panel: everything but the unbound
    /// player. "Has this studio been attached to anything yet?"
    #[must_use]
    pub fn bound(&self) -> Vec<String> {
        self.lock().keys().filter(|k| *k != UNBOUND).cloned().collect()
    }

    /// Which player the page is showing.
    #[must_use]
    pub fn focus(&self) -> String {
        self.focus_mut().clone()
    }

    /// The player the page is a window onto: the focused one, or whatever
    /// there is if the focus has gone stale.
    #[must_use]
    pub fn page(&self) -> Option<Arc<Player>> {
        let all = self.lock();
        let want = self.focus_mut().clone();
        all.get(&want).cloned().or_else(|| all.values().next().cloned())
    }

    /// Show this player on the page. False when there is no such player.
    pub fn set_focus(&self, device: &str) -> bool {
        let all = self.lock();
        if !all.contains_key(device) {
            return false;
        }
        for (id, p) in all.iter() {
            p.focused.store(id == device, Ordering::Relaxed);
        }
        *self.focus_mut() = device.to_string();
        true
    }

    /// Make sure there is a player for the page to show, and that exactly one
    /// of them is focused.
    ///
    /// A studio always has a picture: with no panel found yet that is the
    /// *unbound* player, which renders for the page and has no link.
    pub fn ensure_page(&self, faults: bool) -> Arc<Player> {
        if self.lock().is_empty() {
            self.ensure(UNBOUND, faults, StoredPlayer::default);
        }
        let want = {
            let all = self.lock();
            let focus = self.focus_mut().clone();
            if all.contains_key(&focus) {
                focus
            } else {
                all.keys().next().cloned().unwrap_or_else(|| UNBOUND.to_string())
            }
        };
        self.set_focus(&want);
        self.page().expect("a studio always has a player")
    }

    /// The player for a device, made if it is not there yet.
    pub fn ensure(&self, device: &str, faults: bool, default: impl FnOnce() -> StoredPlayer) -> Arc<Player> {
        let mut all = self.lock();
        if let Some(p) = all.get(device) {
            return Arc::clone(p);
        }
        let mut cfg = default();
        cfg.device = device.to_string();
        let p = Player::new(cfg, faults, self.memory.clone(), Arc::clone(&self.screen));
        all.insert(device.to_string(), Arc::clone(&p));
        p
    }

    /// Adopt a player from the state file.
    pub fn load(&self, cfg: StoredPlayer, faults: bool) {
        let device = cfg.device.clone();
        let p = Player::new(cfg, faults, self.memory.clone(), Arc::clone(&self.screen));
        self.lock().insert(device, p);
    }

    /// A player moves onto a device: a panel was found and adopted, or a
    /// device told us its real id.
    ///
    /// **Renamed in place**, not replaced. The thread, the core and the patch
    /// carry on, which is what "a panel is adopted without the picture
    /// restarting" means. The link is left to [`Player::aim`], which rebuilds
    /// it only if what it is pointed at has actually changed.
    pub fn rekey(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        {
            let mut all = self.lock();
            let Some(p) = all.remove(from) else { return };
            // If the destination already had a player, the one that was
            // actually configured wins, and the other is stopped rather than
            // left rendering into nothing.
            if let Some(old) = all.remove(to) {
                old.shutdown();
            }
            p.rename(to);
            all.insert(to.to_string(), p);
        }
        if self.focus() == from {
            self.set_focus(to);
        }
    }

    /// Stop and forget one player.
    pub fn remove(&self, device: &str) {
        if let Some(p) = self.lock().remove(device) {
            p.shutdown();
        }
    }

    /// What to write to the state file.
    #[must_use]
    pub fn stored(&self) -> Vec<StoredPlayer> {
        self.lock().values().map(|p| p.stored()).collect()
    }

    pub fn shutdown(&self) {
        for p in self.lock().values() {
            p.shutdown();
        }
    }
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

    #[test]
    fn a_player_keeps_what_it_was_configured_with() {
        let p = idle_player();
        p.configure(&PlayerChange { patch: Some("metaballs".into()), seed: Some(9), ..PlayerChange::default() })
            .expect("a real patch");
        let s = p.stored();
        assert_eq!(s.patch, "metaballs");
        assert_eq!(s.seed, 9);
        assert!(!s.on, "configuring must not turn panel output on");

        assert!(p.configure(&PlayerChange { patch: Some("nope".into()), ..PlayerChange::default() }).is_err());
        assert!(p
            .configure(&PlayerChange { param: Some(("nope".into(), 1.0)), ..PlayerChange::default() })
            .is_err());
        assert_eq!(p.stored().patch, "metaballs", "a rejected change must change nothing");
    }

    #[test]
    fn a_speed_outside_the_range_is_clamped_not_refused() {
        let p = idle_player();
        p.configure(&PlayerChange { speed: Some(99.0), ..PlayerChange::default() }).expect("clamped");
        assert_eq!(p.stored().speed, MAX_SPEED, "card 105's speed clamp, now the player's");
    }

    /// Card 161: there is one rate and no way to ask for another. What the
    /// page and `/api/v1/status` report is that rate, whatever the player has
    /// been told to do.
    #[test]
    fn the_only_rate_is_the_one_rate() {
        let p = idle_player();
        assert_eq!(p.state().fps, screeny_art::FPS);
        assert_eq!(p.status().fps, screeny_art::FPS);
        p.configure(&PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() }).expect("metaballs");
        assert_eq!(p.state().fps, screeny_art::FPS, "nothing a player is told moves the rate");
    }

    /// Card 170: the page draws its sliders from `state()`, so it must carry
    /// **every** parameter at its effective value - not just the sparse set
    /// that has been moved away from the defaults.
    #[test]
    fn the_page_state_carries_every_parameter() {
        let p = idle_player();
        p.configure(&PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() }).expect("metaballs");
        p.configure(&PlayerChange { param: Some(("size".into(), 2.5)), ..PlayerChange::default() }).expect("size");
        let def = find_patch("metaballs", false).expect("metaballs");
        let state = p.state();
        assert_eq!(state.params.len(), def.params.len(), "one entry per parameter of the patch");
        assert_eq!(state.params["size"], 2.5, "the one that was moved");
        for spec in def.params {
            if spec.id != "size" {
                assert_eq!(state.params[spec.id], spec.default, "`{}` should still be its default", spec.id);
            }
        }
        assert_eq!(p.stored().params.len(), 1, "and the file still keeps only what was moved");
    }

    /// Changing a parameter must not rebuild the patch: the page's sliders are
    /// sixty changes a second, and a rebuild would restart the animation on
    /// every one of them.
    #[test]
    fn a_parameter_change_does_not_rebuild_the_patch() {
        let p = idle_player();
        p.configure(&PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() }).expect("metaballs");
        p.take_pending();
        p.configure(&PlayerChange { param: Some(("size".into(), 2.5)), ..PlayerChange::default() }).expect("size");
        let want = p.take_pending();
        assert!(want.params, "the running core re-reads its parameters");
        assert!(!want.rebuild, "and the patch is not built again");

        // A seed or a patch is a different matter: those are what a patch is
        // made from, so they do rebuild it.
        p.configure(&PlayerChange { seed: Some(3), ..PlayerChange::default() }).expect("a seed");
        assert!(p.take_pending().rebuild);
    }

    /// Panel output off releases the link and leaves the picture running,
    /// because the page is still showing it.
    #[test]
    fn panel_output_off_does_not_stop_the_picture() {
        let p = idle_player();
        p.configure(&PlayerChange { on: Some(true), ..PlayerChange::default() }).expect("on");
        p.aim(&Reach::Addr("127.0.0.1:50999".parse().expect("an address")));
        assert!(p.status().panel.is_some(), "a link while output is on");

        p.configure(&PlayerChange { on: Some(false), ..PlayerChange::default() }).expect("off");
        p.aim(&Reach::Addr("127.0.0.1:50999".parse().expect("an address")));
        assert!(p.status().panel.is_none(), "the link is released");
        assert!(!p.stop.load(Ordering::Relaxed), "but the player is not stopped");
        p.shutdown();
    }

    /// An action a composing patch offers goes into a one-slot mailbox rather
    /// than reaching into a running patch from another thread (card 140).
    #[test]
    fn an_action_is_a_one_slot_mailbox() {
        let p = idle_player();
        p.configure(&PlayerChange { act: Some("next".into()), ..PlayerChange::default() }).expect("an action");
        p.configure(&PlayerChange { act: Some("again".into()), ..PlayerChange::default() }).expect("another");
        let want = p.take_pending();
        assert_eq!(want.action.as_deref(), Some("again"), "newest wins; a burst costs one slot");
        assert!(p.take_pending().action.is_none(), "and it is taken, not repeated");
    }

    // ------------------------- the per-patch memory (card 165) -------------------------

    /// A patch memory of this test's own.
    fn mem() -> crate::state::SharedMemory {
        crate::state::SharedMemory::default()
    }

    /// A player that is configured but not running: no thread, no socket, and
    /// every change is pure arithmetic on what it would play.
    fn idle_player() -> Arc<Player> {
        Player::new(
            StoredPlayer { device: "abc".into(), on: false, ..StoredPlayer::default() },
            true,
            mem(),
            Screen::new(),
        )
    }

    fn player_on(device: &str, memory: crate::state::SharedMemory) -> Arc<Player> {
        Player::new(
            StoredPlayer { device: device.into(), on: false, ..StoredPlayer::default() },
            false,
            memory,
            Screen::new(),
        )
    }

    fn change(c: PlayerChange) -> PlayerChange {
        c
    }

    /// The card, for a panel: tune one patch, go and tune another, come back.
    #[test]
    fn a_panel_comes_back_to_a_patch_as_it_left_it() {
        let p = idle_player();
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("metaballs");
        p.configure(&change(PlayerChange { seed: Some(11), ..PlayerChange::default() })).expect("a seed");
        p.configure(&change(PlayerChange { param: Some(("size".into(), 2.5)), ..PlayerChange::default() })).expect("size");

        // The other patch has to be one whose parameters do not overlap, or
        // "they do not follow it over" would not be a test. `dwell` is the
        // dials'; `size` is not.
        p.configure(&change(PlayerChange { patch: Some("clocks-dials".into()), ..PlayerChange::default() })).expect("dials");
        p.configure(&change(PlayerChange { seed: Some(22), ..PlayerChange::default() })).expect("a seed");
        p.configure(&change(PlayerChange { param: Some(("dwell".into(), 90.0)), ..PlayerChange::default() })).expect("dwell");
        assert_eq!(p.stored().params.get("size"), None, "the other patch's parameters do not follow it over");

        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("back");
        let s = p.stored();
        assert_eq!(s.patch, "metaballs");
        assert_eq!(s.seed, 11, "and on the seed it was left on");
        assert_eq!(s.params["size"], 2.5);

        // And the other one is still where it was left, too.
        p.configure(&change(PlayerChange { patch: Some("clocks-dials".into()), ..PlayerChange::default() })).expect("and back");
        let s = p.stored();
        assert_eq!(s.seed, 22);
        assert_eq!(s.params["dwell"], 90.0);
    }

    /// Reset means "back to the defaults and stay there".
    #[test]
    fn reset_makes_a_panel_forget_that_patch() {
        let p = idle_player();
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("metaballs");
        p.configure(&change(PlayerChange { param: Some(("size".into(), 2.5)), ..PlayerChange::default() })).expect("size");
        p.configure(&change(PlayerChange { reset_params: true, ..PlayerChange::default() })).expect("reset");
        assert!(p.stored().params.is_empty());

        p.configure(&change(PlayerChange { patch: Some("clocks-dials".into()), ..PlayerChange::default() })).expect("away");
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("back");
        assert!(p.stored().params.is_empty(), "Reset means the old value does not come back on the next switch");
    }

    /// There is **one** memory, not one per panel: "how metaballs is set" is
    /// one fact about the studio. With card 170 the page is one of the panels
    /// rather than a context of its own, so this is now simply about panels.
    #[test]
    fn every_panel_shares_the_one_memory() {
        let memory = mem();
        let a = player_on("a", memory.clone());
        let b = player_on("b", memory);
        for p in [&a, &b] {
            p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() }))
                .expect("metaballs");
        }
        a.configure(&change(PlayerChange { param: Some(("size".into(), 2.5)), ..PlayerChange::default() })).expect("size");

        // b is already on metaballs, so it does not move until it is asked for
        // a patch again - but when it is, it gets what a set.
        b.configure(&change(PlayerChange { patch: Some("clocks-dials".into()), ..PlayerChange::default() })).expect("away");
        b.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("back");
        assert_eq!(b.stored().params["size"], 2.5, "one memory, one answer");
    }

    /// A value a later build cannot use is corrected rather than obeyed, and it
    /// never costs the rest of the entry. Card 160 adding a `rest` parameter to
    /// `clocks-numerals` is the live version of the middle row.
    #[test]
    fn a_remembered_value_this_build_cannot_use_is_corrected() {
        let cfg = StoredPlayer { device: "abc".into(), on: false, patch: "clocks-dials".into(), ..StoredPlayer::default() };
        let memory = crate::state::SharedMemory::new(crate::state::Memory::from([(
            "metaballs".to_string(),
            crate::state::PatchMemory {
                seed: Some(5),
                params: BTreeMap::from([
                    ("size".to_string(), 2.5),    // fine
                    ("gone".to_string(), 1.0),    // a parameter this build does not have
                    ("speed".to_string(), 999.0), // out of the range this build allows
                ]),
                ..crate::state::PatchMemory::default()
            },
        )]));
        let p = Player::new(cfg, false, memory, Screen::new());
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("metaballs");
        let s = p.stored();
        assert_eq!(s.seed, 5);
        assert_eq!(s.params["size"], 2.5, "the good value survived the bad ones");
        assert!(!s.params.contains_key("gone"));
        assert_eq!(s.params["speed"], 3.0, "clamped to this build's range, as the slider would");
    }

    /// Card 151: a patch the studio has never been on arrives **on Default** -
    /// and therefore not modified - rather than inheriting the seed and the
    /// speed of whatever was playing a moment ago.
    #[test]
    fn an_untouched_patch_arrives_on_default() {
        let p = idle_player();
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("metaballs");
        p.configure(&change(PlayerChange { seed: Some(654_321), ..PlayerChange::default() })).expect("another");
        p.configure(&change(PlayerChange { speed: Some(0.25), ..PlayerChange::default() })).expect("slowly");

        p.configure(&change(PlayerChange { patch: Some("flock".into()), ..PlayerChange::default() })).expect("flock");
        let s = p.stored();
        assert_eq!(s.seed, crate::state::DEFAULT_SEED, "not the seed the last patch was on");
        assert_eq!(s.speed, 1.0, "nor its speed");
        let state = p.state();
        assert_eq!(state.setting, DEFAULT_SETTING);
        assert!(!state.modified, "a patch nobody has touched is not modified");

        // ...and the patch that *was* tuned still comes back as it was left.
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("back");
        let s = p.stored();
        assert_eq!(s.seed, 654_321);
        assert_eq!(s.speed, 0.25);
    }

    /// An entry for a patch this build has never heard of is kept, not thrown
    /// away: a patch that comes back in a later release gets its memory back.
    #[test]
    fn a_memory_for_a_patch_that_is_not_here_is_kept() {
        let cfg = StoredPlayer { device: "abc".into(), on: false, ..StoredPlayer::default() };
        let memory = crate::state::SharedMemory::new(crate::state::Memory::from([(
            "from-the-future".to_string(),
            crate::state::PatchMemory { seed: Some(3), ..crate::state::PatchMemory::default() },
        )]));
        let p = Player::new(cfg, false, memory.clone(), Screen::new());
        p.configure(&change(PlayerChange { patch: Some("metaballs".into()), ..PlayerChange::default() })).expect("metaballs");
        assert!(memory.knows("from-the-future"));
    }

    // ------------------------------- the fleet -------------------------------

    fn fleet() -> Players {
        Players::new(mem(), Screen::new())
    }

    #[test]
    fn a_fleet_is_a_collection_and_a_rekey_carries_the_player_over() {
        let players = fleet();
        players.load(
            StoredPlayer { device: "pending:192.0.2.7".into(), patch: "metaballs".into(), seed: 5, on: false, ..StoredPlayer::default() },
            false,
        );
        assert_eq!(players.ids(), vec!["pending:192.0.2.7".to_string()]);
        let before = players.get("pending:192.0.2.7").expect("there");
        players.rekey("pending:192.0.2.7", "abc123");
        assert_eq!(players.ids(), vec!["abc123".to_string()]);
        let p = players.get("abc123").expect("moved");
        assert!(Arc::ptr_eq(&before, &p), "the same player, renamed - not a replacement, or the picture would restart");
        assert_eq!(p.stored().patch, "metaballs");
        assert_eq!(p.stored().seed, 5);
        assert_eq!(p.stored().device, "abc123");
        players.remove("abc123");
        assert!(players.ids().is_empty());
    }

    /// A studio always has a picture to show: with no panel found yet, that is
    /// the unbound player.
    #[test]
    fn a_studio_with_no_panel_still_has_a_player() {
        let players = fleet();
        let page = players.ensure_page(false);
        assert_eq!(page.device(), UNBOUND);
        assert!(page.is_focused());
        assert!(players.bound().is_empty(), "and it is not pretending to be a panel");
        assert_eq!(players.focus(), UNBOUND);
        players.shutdown();
    }

    /// Adopting the first panel found renames that same player, so the picture
    /// the page is showing carries straight on to the panel.
    #[test]
    fn adopting_a_panel_keeps_the_same_player_and_the_focus() {
        let players = fleet();
        let page = players.ensure_page(false);
        page.configure(&change(PlayerChange { patch: Some("metaballs".into()), seed: Some(42), ..PlayerChange::default() }))
            .expect("metaballs");

        players.rekey(UNBOUND, "4a00a4");
        let now = players.page().expect("still a page");
        assert!(Arc::ptr_eq(&page, &now));
        assert_eq!(now.device(), "4a00a4");
        assert_eq!(now.stored().patch, "metaballs", "the picture did not restart");
        assert_eq!(now.stored().seed, 42);
        assert!(now.is_focused());
        assert_eq!(players.focus(), "4a00a4");
        players.shutdown();
    }

    /// With two panels, exactly one is on the page, and the chooser moves it.
    #[test]
    fn exactly_one_player_is_on_the_page() {
        let players = fleet();
        players.ensure("a", false, StoredPlayer::default);
        players.ensure("b", false, StoredPlayer::default);
        players.ensure_page(false);
        let focused: Vec<String> = players.all().iter().filter(|p| p.is_focused()).map(|p| p.device()).collect();
        assert_eq!(focused.len(), 1, "{focused:?}");

        assert!(players.set_focus("b"));
        assert_eq!(players.page().expect("a page").device(), "b");
        assert_eq!(players.all().iter().filter(|p| p.is_focused()).count(), 1);
        assert!(!players.set_focus("nope"), "a device with no player cannot be shown");
        assert_eq!(players.focus(), "b");
        players.shutdown();
    }

    /// The page never goes blank because the panel it was showing was
    /// forgotten: the focus falls back to whatever is left.
    #[test]
    fn forgetting_the_focused_panel_moves_the_page_on() {
        let players = fleet();
        players.ensure("a", false, StoredPlayer::default);
        players.ensure("b", false, StoredPlayer::default);
        players.ensure_page(false);
        players.set_focus("b");
        players.remove("b");
        let page = players.ensure_page(false);
        assert_eq!(page.device(), "a");
        assert!(page.is_focused());

        // And forgetting the last one leaves an unbound player behind.
        players.remove("a");
        let page = players.ensure_page(false);
        assert_eq!(page.device(), UNBOUND);
        players.shutdown();
    }
}
