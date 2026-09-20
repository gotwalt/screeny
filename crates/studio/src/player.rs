//! Players: **the only thing in the studio that renders.**
//!
//! Card 106 built one player per panel and left the design view its own
//! engine. Card 170 deleted that engine: there is one picture, the panel shows
//! it, and the page is a window onto it. So a player now does everything the
//! design view used to do as well - pause, speed, restart, a piece's own
//! actions - and the frames the browser draws are the same decoded datagrams
//! the panel is being sent.
//!
//! A player is the thing that is meant to be forgotten. It renders on its own
//! OS thread, sends through [`screeny::Link`] (which reconnects by itself), and
//! survives its own piece:
//!
//! - **a piece that panics** is caught by `catch_unwind`, logged once, and the
//!   player restarts on a safe fallback piece;
//! - **a piece that stalls** - a frame that never comes back - is noticed by a
//!   watchdog. The wedged thread is told to stop and **abandoned**, because no
//!   thread can be killed in Rust, and a *new* core is started on the fallback.
//!   The panel link is deliberately owned by the player rather than by the
//!   core, so abandoning a core leaks a piece's render state and one thread and
//!   never a socket, a link thread or the device itself. The core is behind no
//!   shared lock at all, which is why a wedged piece cannot take `/healthz` or
//!   `/api/v1/status` with it (card 143, closed by construction);
//! - **a piece that does either repeatedly** is refused: after
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
//! at its configured rate while its panel is connected **or** a browser is
//! watching, and at [`IDLE_FPS`] otherwise: a panel that is unplugged for a
//! month, with nobody looking, should not cost a core for a month.

use screeny_art::output::{Output, PanelStatus, SenderOutput};
use screeny_art::piece::{local_now, Ctx, Params, Piece, PieceDef, Playing};
use screeny_art::{Pipeline, Settings};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::devices::Reach;
use crate::page::{self, Screen, StudioState};
use crate::state::{unix_now, StoredPlayer, UNBOUND};

/// Write down what this player's current piece is set to, in the studio's one
/// settings memory. An unknown piece has no specs to compare against, so its
/// entry is left exactly as the file had it (card 165: "unknown piece id ->
/// entry ignored, kept in the file").
fn remember_current(cfg: &StoredPlayer, memory: &crate::state::SharedMemory, faults: bool) {
    if let Some(def) = find_piece(&cfg.piece, faults) {
        memory.remember(def, &cfg.params, cfg.seed);
    }
}

/// Put `cfg` on `def`, restoring whatever that piece was last left set to -
/// here, or on another panel. One memory, one answer.
fn recall_into(cfg: &mut StoredPlayer, def: &'static PieceDef, memory: &crate::state::SharedMemory, device: &str) {
    cfg.piece = def.id.to_string();
    let (params, seed) = memory.recall(def, &format!("panel {}", label(device)));
    cfg.params = params;
    if let Some(seed) = seed {
        cfg.seed = seed;
    }
}

/// How long one frame may take before the player is treated as wedged.
/// Generous: a cold GPU piece's first frame is not a fault.
pub const WATCHDOG: Duration = Duration::from_secs(5);
/// Faults - panics or stalls - before a player gives up rather than looping.
pub const MAX_FAULTS: u32 = 3;
/// The rate a player renders at when its panel is not connected **and** no
/// browser is watching. Enough to drive reconnection and to be ready the
/// instant either of those changes.
pub const IDLE_FPS: f64 = 5.0;
/// Frames per second a player may be asked for.
pub const MIN_FPS: f64 = 1.0;
pub const MAX_FPS: f64 = 60.0;
/// How fast a piece may be played. Card 105's clamp, unchanged.
pub const MAX_SPEED: f64 = 8.0;

// ------------------------------------------------------------ the pieces ---

/// How long `fault-stall` stops returning for. Comfortably past the watchdog
/// and past `/healthz`'s patience, and then over.
pub const STALL_FOR: Duration = Duration::from_secs(30);

/// Pieces that misbehave on purpose, for the containment tests.
///
/// **Not in `screeny_art::pieces::ALL`** and not offered by `/api/v1/bootstrap`
/// unless the studio was started with fault pieces enabled, which only a test
/// and `SCREENY_STUDIO_FAULTS=1` do. They exist so that "a piece that panics is
/// contained" can be a test rather than a claim.
pub static FAULT_PIECES: &[PieceDef] = &[
    PieceDef {
        id: "fault-panic",
        name: "Fault: panic",
        blurb: "Panics on its fourth frame. For the containment tests only.",
        params: &[],
        make: |_| Box::new(FaultPiece { frames: 0, kind: Fault::Panic }),
    },
    PieceDef {
        id: "fault-stall",
        name: "Fault: stall",
        blurb: "Stops returning frames on its fourth. For the containment tests only.",
        params: &[],
        make: |_| Box::new(FaultPiece { frames: 0, kind: Fault::Stall }),
    },
];

enum Fault {
    Panic,
    Stall,
}

struct FaultPiece {
    frames: u32,
    kind: Fault,
}

impl Piece for FaultPiece {
    fn render(&mut self, _ctx: &Ctx) -> screeny_art::Frame {
        self.frames += 1;
        if self.frames >= 4 {
            match self.kind {
                Fault::Panic => panic!("fault-panic: this piece panics on purpose"),
                // Long enough to be a stall by any measure (the watchdog is
                // 5 s), short enough that an abandoned thread eventually goes
                // away even in a test that is itself stuck.
                Fault::Stall => std::thread::sleep(STALL_FOR),
            }
        }
        screeny_art::Frame::black()
    }
}

/// Find a piece by id, including the fault pieces when they are enabled.
#[must_use]
pub fn find_piece(id: &str, faults: bool) -> Option<&'static PieceDef> {
    screeny_art::piece::find(id).or_else(|| faults.then(|| FAULT_PIECES.iter().find(|d| d.id == id)).flatten())
}

/// A piece to fall back to when the chosen one cannot be run.
///
/// CPU only and gentle on the panel, in a fixed order so the choice is
/// predictable in a log. `avoid` is the piece that just failed.
#[must_use]
pub fn fallback_piece(avoid: &str) -> &'static PieceDef {
    for id in ["plasma", "metaballs", "clocks-numerals"] {
        if id != avoid {
            if let Some(d) = screeny_art::piece::find(id) {
                return d;
            }
        }
    }
    screeny_art::pieces::ALL.iter().find(|d| d.id != avoid).unwrap_or(&screeny_art::pieces::ALL[0])
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
    /// Pieces that panicked mid-render.
    pub panics: u64,
    /// Frames that never came back, caught by the watchdog.
    pub stalls: u64,
    /// Core threads started after the first: the count of recoveries. Since
    /// card 170 a piece change does *not* start one, so this counts faults.
    pub restarts: u64,
    /// Wedged threads still out there. They go away when (if) their frame
    /// returns; each one is a piece's render state, never a socket or a link.
    pub abandoned: u64,
    /// The piece the player was asked for but cannot run, if it fell back.
    pub fell_back_from: Option<String>,
    /// Pieces this player refuses to load again until a human says otherwise.
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
    pub piece: String,
    pub piece_name: String,
    pub seed: u32,
    /// Only what has been set away from the piece's defaults.
    pub params: BTreeMap<String, f32>,
    pub fps: f64,
    pub paused: bool,
    pub speed: f64,
    pub brightness: Option<u8>,
    pub settings: Settings,
    /// True when a core thread is alive and ticking.
    pub running: bool,
    /// True when this is the player the page is a window onto.
    pub focused: bool,
    /// The rate the render loop is actually achieving.
    pub fps_measured: f32,
    /// What a composing piece says it is performing.
    pub playing: Option<Playing>,
    pub health: PlayerHealth,
    /// The link, or `None` when panel output is off.
    pub panel: Option<PanelStatus>,
}

// ----------------------------------------------------------------- a core ---

/// The render state of one piece. Thrown away whole when it misbehaves.
struct Core {
    def: &'static PieceDef,
    piece: Box<dyn Piece>,
    params: Params,
    pipeline: Pipeline,
    seed: u32,
    t: f64,
    last: Instant,
    fps: f32,
}

impl Core {
    /// One frame: the piece, the pipeline, and what the page is shown.
    ///
    /// `paused` and `speed` are card 105's, moved here with the rest of the
    /// design view: the piece's clock is scaled, the pipeline's is not,
    /// because the limiter measures wall-clock rise.
    fn tick(&mut self, paused: bool, speed: f64) -> screeny_art::pipeline::Output {
        let now = Instant::now();
        let wall = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let dt = if paused { 0.0 } else { wall * speed };
        self.t += dt;
        let frame = self.piece.render(&Ctx { t: self.t, dt, now: local_now(), params: &self.params });
        if wall > 0.0 {
            self.fps += (1.0 / wall as f32 - self.fps) * 0.1;
        }
        self.pipeline.process(frame, wall)
    }

    /// The piece's parameters, as the configuration now says.
    fn reload_params(&mut self, cfg: &StoredPlayer) {
        self.params = Params::defaults(self.def.params);
        for (id, v) in &cfg.params {
            self.params.set(self.def.params, id, *v);
        }
    }

    /// Build the piece again from its seed and start its clock over.
    fn restart(&mut self) {
        self.piece = (self.def.make)(u64::from(self.seed));
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
    /// What is being performed, **and by which piece**.
    ///
    /// The pair matters: a change is applied by the render loop before its
    /// *next* frame, so for up to one frame period the configuration says one
    /// piece and the core is still running another. "What is it performing"
    /// is only true of the piece performing it, and answering with the old
    /// piece's answer would put the wrong thing on the page.
    playing: Mutex<(&'static str, Option<Playing>)>,
}

/// What a core is performing, if it is running the piece that was asked for.
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
    /// The piece or the seed changed: a fresh core.
    rebuild: bool,
    /// Re-read the parameters into the running core.
    params: bool,
    /// Re-read the pipeline settings.
    settings: bool,
    /// Rebuild the piece from its seed, keeping everything else.
    restart: bool,
    /// One action a composing piece offered (card 140).
    action: Option<String>,
}

impl Pending {
    fn anything(&self) -> bool {
        self.rebuild || self.params || self.settings || self.restart || self.action.is_some()
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
    /// The studio's one settings memory (card 165), shared with every other
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

    /// What the page draws itself from: the piece, the seed, **every**
    /// parameter at its effective value, the pipeline settings and the
    /// playback state.
    #[must_use]
    pub fn state(&self) -> StudioState {
        let cfg = self.cfg().clone();
        let params = match find_piece(&cfg.piece, self.faults) {
            Some(def) => {
                let mut p = Params::defaults(def.params);
                for (id, v) in &cfg.params {
                    p.set(def.params, id, *v);
                }
                p.iter().map(|(k, v)| (k.to_string(), v)).collect()
            }
            // A piece this build has not got: the page shows no sliders for
            // it rather than inventing some.
            None => BTreeMap::new(),
        };
        StudioState {
            piece: cfg.piece,
            seed: cfg.seed,
            params,
            settings: cfg.settings,
            paused: cfg.paused,
            speed: cfg.speed,
            fps: cfg.fps,
            on: cfg.on,
            device: cfg.device,
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
    /// If no piece has that id, or the current piece has no such parameter.
    pub fn configure(self: &Arc<Self>, change: &PlayerChange) -> Result<(), String> {
        let mut want = Pending::default();
        {
            let mut cfg = self.cfg();
            if let Some(id) = &change.piece {
                let def = find_piece(id, self.faults).ok_or_else(|| format!("no piece called `{id}`"))?;
                if cfg.piece != def.id {
                    // Arrive at the new piece set up the way it was left, by
                    // whoever last touched it (card 165). Nothing to write down
                    // on the way out: every change went into the memory when it
                    // was made.
                    let device = cfg.device.clone();
                    recall_into(&mut cfg, def, &self.memory, &device);
                    want.rebuild = true;
                }
                // Asking for a piece again clears its refusal: a human saying
                // "try it" outranks the brake.
                let mut h = self.health_mut();
                h.refused.retain(|r| r != def.id);
                h.gave_up = None;
                h.fell_back_from = None;
                self.consecutive.store(0, Ordering::Relaxed);
            }
            if let Some(seed) = change.seed {
                cfg.seed = seed;
                remember_current(&cfg, &self.memory, self.faults);
                want.rebuild = true;
            }
            if let Some((id, value)) = &change.param {
                let def = find_piece(&cfg.piece, self.faults);
                let Some(spec) = def.and_then(|d| d.params.iter().find(|p| p.id == id)) else {
                    return Err(format!("{} has no parameter `{id}`", cfg.piece));
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
                self.memory.forget_params(&cfg.piece);
                want.params = true;
            }
            if let Some(fps) = change.fps {
                // A rate that is not a number at all is refused rather than
                // clamped: `f64::clamp` hands a NaN straight back, and
                // `Duration::from_secs_f64(NaN)` in the render loop panics.
                // Card 172 made this reachable - `set_playback` used to drop
                // anything that was not 30 or 60, NaN included.
                if fps.is_finite() {
                    cfg.fps = fps.clamp(MIN_FPS, MAX_FPS);
                }
            }
            if let Some(paused) = change.paused {
                cfg.paused = paused;
            }
            if let Some(speed) = change.speed {
                cfg.speed = speed.clamp(0.0, MAX_SPEED);
            }
            if let Some(s) = change.settings {
                cfg.settings = s;
                want.settings = true;
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
            p.settings |= want.settings;
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
            let piece = self.cfg().piece.clone();
            h.stop.store(true, Ordering::Relaxed);
            {
                let mut hl = self.health_mut();
                hl.stalls += 1;
                hl.abandoned += 1;
            }
            self.fault(&piece, &format!("`{piece}` has not produced a frame for {WATCHDOG:?}"));
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
                    performing(h, &cfg.piece),
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
        let name = find_piece(&cfg.piece, self.faults).map_or("", |d| d.name).to_string();
        PlayerStatus {
            device: cfg.device,
            on: cfg.on,
            piece: cfg.piece,
            piece_name: name,
            seed: cfg.seed,
            params: cfg.params,
            fps: cfg.fps,
            paused: cfg.paused,
            speed: cfg.speed,
            brightness: cfg.brightness,
            settings: cfg.settings,
            running,
            focused: self.is_focused(),
            fps_measured,
            playing,
            health,
            panel,
        }
    }

    /// What a composing piece says it is performing, from the last frame.
    ///
    /// `None` while the render loop has yet to pick up a piece change: what
    /// the *previous* piece was performing is not an answer to this question.
    #[must_use]
    pub fn playing(&self) -> Option<Playing> {
        let want = self.cfg().piece.clone();
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
    fn fault(self: &Arc<Self>, piece: &str, what: &str) {
        let n = self.consecutive.fetch_add(1, Ordering::Relaxed) + 1;
        let fallback = fallback_piece(piece);
        let mut cfg = self.cfg();
        let mut h = self.health_mut();
        h.last_error = Some(what.to_string());
        if n >= u64::from(MAX_FAULTS) {
            let msg = format!("{what}; giving up after {n} faults");
            eprintln!("studio: player {}: {msg}", label(&cfg.device));
            h.gave_up = Some(msg);
            if !h.refused.iter().any(|r| r == piece) {
                h.refused.push(piece.to_string());
            }
            // `on` is left alone: the player stays configured, just silent, so
            // that a human can see what it was meant to be playing.
            drop(cfg);
            return;
        }
        eprintln!("studio: player {}: {what}; falling back to `{}`", label(&cfg.device), fallback.id);
        if !h.refused.iter().any(|r| r == piece) {
            h.refused.push(piece.to_string());
        }
        h.fell_back_from = Some(piece.to_string());
        // The fallback arrives set up the way it was last left, like any other
        // piece change. The failing piece's memory is untouched: a fault is not
        // a reason to forget how somebody had it set.
        let device = cfg.device.clone();
        recall_into(&mut cfg, fallback, &self.memory, &device);
    }

    /// A core on whatever the configuration says, or on the fallback when that
    /// cannot be run. Never fails: a player always has something to show.
    fn make_core(&self) -> Core {
        let (def, seed, params_map, settings, device) = {
            let cfg = self.cfg();
            (cfg.piece.clone(), cfg.seed, cfg.params.clone(), cfg.settings, cfg.device.clone())
        };
        // A piece that is unknown, or one this player has refused, becomes the
        // fallback rather than a reason not to run.
        let refused = self.health_mut().refused.clone();
        let chosen = match find_piece(&def, self.faults) {
            Some(d) if !refused.contains(&def) => d,
            _ => {
                let f = fallback_piece(&def);
                if find_piece(&def, self.faults).is_none() {
                    let mut h = self.health_mut();
                    if h.fell_back_from.as_deref() != Some(def.as_str()) {
                        eprintln!("studio: player {}: no piece called `{def}`; playing `{}`", label(&device), f.id);
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
            piece: (chosen.make)(u64::from(seed)),
            params,
            pipeline: Pipeline::new(settings),
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
    pub piece: Option<String>,
    pub seed: Option<u32>,
    pub param: Option<(String, f32)>,
    /// "Reset": back to the piece's defaults, and forget what was remembered
    /// for it (card 165).
    pub reset_params: bool,
    pub fps: Option<f64>,
    pub paused: Option<bool>,
    pub speed: Option<f64>,
    pub settings: Option<Settings>,
    /// `Some(None)` clears the brightness policy; `Some(Some(n))` sets it.
    pub brightness: Option<Option<u8>>,
    /// Start the piece again from its seed.
    pub restart: bool,
    /// An action a composing piece offered (card 140).
    pub act: Option<String>,
}

/// The render loop of one core.
///
/// Everything that can change about what is being played arrives through the
/// player's one-slot [`Pending`] and is applied here, between frames, so
/// nothing a browser does costs a thread.
fn run_core(player: &Arc<Player>, handle: &Arc<CoreHandle>, core: Core) {
    let mut core = core;
    let mut piece_id = core.def.id.to_string();
    let mut next = Instant::now();
    let mut limits_at = Instant::now() - Duration::from_secs(10);
    let mut panicked = false;

    while !handle.stop.load(Ordering::Relaxed) && !player.stop.load(Ordering::Relaxed) {
        handle.beat.store(unix_millis(), Ordering::Relaxed);

        let (fps, paused, speed) = {
            let cfg = player.cfg();
            (cfg.fps, cfg.paused, cfg.speed)
        };

        // Apply whatever has been asked for since the last frame, then render.
        // The piece is the only code in here that can panic - `act` as much as
        // `render` - so both are inside the same `catch_unwind`, which is what
        // keeps one bad piece from taking the process, and the panel, with it.
        let want = player.take_pending();
        if want.rebuild {
            core = player.make_core();
            piece_id = core.def.id.to_string();
        }
        // Read the configuration once, and only when something needs it: this
        // runs sixty times a second.
        let reconf = (want.params || want.settings).then(|| player.stored());
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !want.rebuild {
                if let Some(cfg) = &reconf {
                    if want.params {
                        core.reload_params(cfg);
                    }
                    if want.settings {
                        core.pipeline.settings = cfg.settings;
                    }
                }
                if want.restart {
                    core.restart();
                }
            }
            if let Some(action) = &want.action {
                core.piece.act(action);
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
            *p = (core.def.id, core.piece.playing());
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
        let watched = player.is_focused() && player.screen.watchers() > 0;
        let rate = if connected || watched { fps } else { IDLE_FPS };
        next += Duration::from_secs_f64(1.0 / rate.clamp(MIN_FPS, MAX_FPS));
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
        player.fault(&piece_id, &format!("`{piece_id}` panicked while rendering"));
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
    /// The studio's one settings memory, handed to every player made here.
    memory: crate::state::SharedMemory,
    /// The page's one frame cell, likewise.
    screen: Arc<Screen>,
}

impl Players {
    #[must_use]
    pub fn new(memory: crate::state::SharedMemory, screen: Arc<Screen>) -> Self {
        Players { inner: Mutex::new(BTreeMap::new()), focus: Mutex::new(UNBOUND.to_string()), memory, screen }
    }

    /// The studio's one settings memory, as handed to every player here.
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
    /// **Renamed in place**, not replaced. The thread, the core and the piece
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
    fn the_fault_pieces_are_not_in_the_normal_list() {
        assert!(screeny_art::piece::find("fault-panic").is_none());
        assert!(screeny_art::piece::find("fault-stall").is_none());
        assert!(find_piece("fault-panic", false).is_none());
        assert!(find_piece("fault-panic", true).is_some());
        assert!(find_piece("plasma", false).is_some());
    }

    #[test]
    fn the_fallback_is_never_the_piece_that_just_failed() {
        assert_ne!(fallback_piece("plasma").id, "plasma");
        assert_ne!(fallback_piece("metaballs").id, "metaballs");
        assert_ne!(fallback_piece("clocks-numerals").id, "clocks-numerals");
        assert_ne!(fallback_piece("fault-panic").id, "fault-panic");
    }

    #[test]
    fn a_player_keeps_what_it_was_configured_with() {
        let p = idle_player();
        p.configure(&PlayerChange { piece: Some("plasma".into()), seed: Some(9), ..PlayerChange::default() })
            .expect("a real piece");
        let s = p.stored();
        assert_eq!(s.piece, "plasma");
        assert_eq!(s.seed, 9);
        assert!(!s.on, "configuring must not turn panel output on");

        assert!(p.configure(&PlayerChange { piece: Some("nope".into()), ..PlayerChange::default() }).is_err());
        assert!(p
            .configure(&PlayerChange { param: Some(("nope".into(), 1.0)), ..PlayerChange::default() })
            .is_err());
        assert_eq!(p.stored().piece, "plasma", "a rejected change must change nothing");
    }

    #[test]
    fn an_fps_outside_the_range_is_clamped_not_refused() {
        let p = idle_player();
        p.configure(&PlayerChange { fps: Some(1000.0), ..PlayerChange::default() }).expect("clamped");
        assert_eq!(p.stored().fps, MAX_FPS);
        p.configure(&PlayerChange { fps: Some(0.0), ..PlayerChange::default() }).expect("clamped");
        assert_eq!(p.stored().fps, MIN_FPS);
        p.configure(&PlayerChange { speed: Some(99.0), ..PlayerChange::default() }).expect("clamped");
        assert_eq!(p.stored().speed, MAX_SPEED, "card 105's speed clamp, now the player's");
    }

    /// Card 170: the page draws its sliders from `state()`, so it must carry
    /// **every** parameter at its effective value - not just the sparse set
    /// that has been moved away from the defaults.
    #[test]
    fn the_page_state_carries_every_parameter() {
        let p = idle_player();
        p.configure(&PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() }).expect("plasma");
        p.configure(&PlayerChange { param: Some(("scale".into(), 2.5)), ..PlayerChange::default() }).expect("scale");
        let def = find_piece("plasma", false).expect("plasma");
        let state = p.state();
        assert_eq!(state.params.len(), def.params.len(), "one entry per parameter of the piece");
        assert_eq!(state.params["scale"], 2.5, "the one that was moved");
        for spec in def.params {
            if spec.id != "scale" {
                assert_eq!(state.params[spec.id], spec.default, "`{}` should still be its default", spec.id);
            }
        }
        assert_eq!(p.stored().params.len(), 1, "and the file still keeps only what was moved");
    }

    /// Changing a parameter must not rebuild the piece: the page's sliders are
    /// sixty changes a second, and a rebuild would restart the animation on
    /// every one of them.
    #[test]
    fn a_parameter_change_does_not_rebuild_the_piece() {
        let p = idle_player();
        p.configure(&PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() }).expect("plasma");
        p.take_pending();
        p.configure(&PlayerChange { param: Some(("scale".into(), 2.5)), ..PlayerChange::default() }).expect("scale");
        let want = p.take_pending();
        assert!(want.params, "the running core re-reads its parameters");
        assert!(!want.rebuild, "and the piece is not built again");

        // A seed or a piece is a different matter: those are what a piece is
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

    /// An action a composing piece offers goes into a one-slot mailbox rather
    /// than reaching into a running piece from another thread (card 140).
    #[test]
    fn an_action_is_a_one_slot_mailbox() {
        let p = idle_player();
        p.configure(&PlayerChange { act: Some("next".into()), ..PlayerChange::default() }).expect("an action");
        p.configure(&PlayerChange { act: Some("again".into()), ..PlayerChange::default() }).expect("another");
        let want = p.take_pending();
        assert_eq!(want.action.as_deref(), Some("again"), "newest wins; a burst costs one slot");
        assert!(p.take_pending().action.is_none(), "and it is taken, not repeated");
    }

    // ------------------------- the per-piece memory (card 165) -------------------------

    /// A settings memory of this test's own.
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

    /// The card, for a panel: tune one piece, go and tune another, come back.
    #[test]
    fn a_panel_comes_back_to_a_piece_as_it_left_it() {
        let p = idle_player();
        p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("plasma");
        p.configure(&change(PlayerChange { seed: Some(11), ..PlayerChange::default() })).expect("a seed");
        p.configure(&change(PlayerChange { param: Some(("scale".into(), 2.5)), ..PlayerChange::default() })).expect("scale");

        p.configure(&change(PlayerChange { piece: Some("metaballs".into()), ..PlayerChange::default() })).expect("metaballs");
        p.configure(&change(PlayerChange { seed: Some(22), ..PlayerChange::default() })).expect("a seed");
        p.configure(&change(PlayerChange { param: Some(("count".into(), 8.0)), ..PlayerChange::default() })).expect("count");
        assert_eq!(p.stored().params.get("scale"), None, "the other piece's parameters do not follow it over");

        p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("back");
        let s = p.stored();
        assert_eq!(s.piece, "plasma");
        assert_eq!(s.seed, 11, "and on the seed it was left on");
        assert_eq!(s.params["scale"], 2.5);

        // And the other one is still where it was left, too.
        p.configure(&change(PlayerChange { piece: Some("metaballs".into()), ..PlayerChange::default() })).expect("and back");
        let s = p.stored();
        assert_eq!(s.seed, 22);
        assert_eq!(s.params["count"], 8.0);
    }

    /// Reset means "back to the defaults and stay there".
    #[test]
    fn reset_makes_a_panel_forget_that_piece() {
        let p = idle_player();
        p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("plasma");
        p.configure(&change(PlayerChange { param: Some(("scale".into(), 2.5)), ..PlayerChange::default() })).expect("scale");
        p.configure(&change(PlayerChange { reset_params: true, ..PlayerChange::default() })).expect("reset");
        assert!(p.stored().params.is_empty());

        p.configure(&change(PlayerChange { piece: Some("metaballs".into()), ..PlayerChange::default() })).expect("away");
        p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("back");
        assert!(p.stored().params.is_empty(), "Reset means the old value does not come back on the next switch");
    }

    /// There is **one** memory, not one per panel: "how plasma is set" is one
    /// fact about the studio. With card 170 the page is one of the panels
    /// rather than a context of its own, so this is now simply about panels.
    #[test]
    fn every_panel_shares_the_one_memory() {
        let memory = mem();
        let a = player_on("a", memory.clone());
        let b = player_on("b", memory);
        for p in [&a, &b] {
            p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("plasma");
        }
        a.configure(&change(PlayerChange { param: Some(("scale".into(), 2.5)), ..PlayerChange::default() })).expect("scale");

        // b is already on plasma, so it does not move until it is asked for a
        // piece again - but when it is, it gets what a set.
        b.configure(&change(PlayerChange { piece: Some("metaballs".into()), ..PlayerChange::default() })).expect("away");
        b.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("back");
        assert_eq!(b.stored().params["scale"], 2.5, "one memory, one answer");
    }

    /// A value a later build cannot use is corrected rather than obeyed, and it
    /// never costs the rest of the entry. Card 160 adding a `rest` parameter to
    /// `clocks-numerals` is the live version of the middle row.
    #[test]
    fn a_remembered_value_this_build_cannot_use_is_corrected() {
        let cfg = StoredPlayer { device: "abc".into(), on: false, piece: "metaballs".into(), ..StoredPlayer::default() };
        let memory = crate::state::SharedMemory::new(crate::state::Memory::from([(
            "plasma".to_string(),
            crate::state::PieceMemory {
                seed: Some(5),
                params: BTreeMap::from([
                    ("scale".to_string(), 2.5),   // fine
                    ("gone".to_string(), 1.0),    // a parameter this build does not have
                    ("drift".to_string(), 999.0), // out of the range this build allows
                ]),
            },
        )]));
        let p = Player::new(cfg, false, memory, Screen::new());
        p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("plasma");
        let s = p.stored();
        assert_eq!(s.seed, 5);
        assert_eq!(s.params["scale"], 2.5, "the good value survived the bad ones");
        assert!(!s.params.contains_key("gone"));
        assert_eq!(s.params["drift"], 2.0, "clamped to this build's range, as the slider would");
    }

    /// An entry for a piece this build has never heard of is kept, not thrown
    /// away: a piece that comes back in a later release gets its settings back.
    #[test]
    fn a_memory_for_a_piece_that_is_not_here_is_kept() {
        let cfg = StoredPlayer { device: "abc".into(), on: false, ..StoredPlayer::default() };
        let memory = crate::state::SharedMemory::new(crate::state::Memory::from([(
            "from-the-future".to_string(),
            crate::state::PieceMemory { seed: Some(3), params: BTreeMap::new() },
        )]));
        let p = Player::new(cfg, false, memory.clone(), Screen::new());
        p.configure(&change(PlayerChange { piece: Some("plasma".into()), ..PlayerChange::default() })).expect("plasma");
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
            StoredPlayer { device: "pending:192.0.2.7".into(), piece: "plasma".into(), seed: 5, on: false, ..StoredPlayer::default() },
            false,
        );
        assert_eq!(players.ids(), vec!["pending:192.0.2.7".to_string()]);
        let before = players.get("pending:192.0.2.7").expect("there");
        players.rekey("pending:192.0.2.7", "abc123");
        assert_eq!(players.ids(), vec!["abc123".to_string()]);
        let p = players.get("abc123").expect("moved");
        assert!(Arc::ptr_eq(&before, &p), "the same player, renamed - not a replacement, or the picture would restart");
        assert_eq!(p.stored().piece, "plasma");
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
        page.configure(&change(PlayerChange { piece: Some("plasma".into()), seed: Some(42), ..PlayerChange::default() }))
            .expect("plasma");

        players.rekey(UNBOUND, "4a00a4");
        let now = players.page().expect("still a page");
        assert!(Arc::ptr_eq(&page, &now));
        assert_eq!(now.device(), "4a00a4");
        assert_eq!(now.stored().piece, "plasma", "the picture did not restart");
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
