//! Players: one per device, each rendering a piece onto one panel.
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
//!   never a socket, a link thread or the device itself;
//! - **a piece that does either repeatedly** is refused: after
//!   [`MAX_FAULTS`] the player stops trying, says so once, and `/healthz` goes
//!   503. A restart loop is worse than a stopped player.
//!
//! Nothing here grows without bound. One core thread per player, one link, one
//! control request in flight, a fixed fallback ladder, and every fault logged
//! once rather than per frame. While the panel is away the player renders at
//! [`IDLE_FPS`] instead of its configured rate: a panel that is unplugged for a
//! month should not cost a core for a month.

use screeny_art::frame::WireFrame;
use screeny_art::output::{Output, PanelStatus, SenderOutput};
use screeny_art::piece::{local_now, Ctx, Params, Piece, PieceDef, Playing};
use screeny_art::{Pipeline, Settings};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::devices::Reach;
use crate::state::{unix_now, StoredPlayer};

/// How long one frame may take before the player is treated as wedged.
/// Generous: a cold GPU piece's first frame is not a fault.
pub const WATCHDOG: Duration = Duration::from_secs(5);
/// Faults - panics or stalls - before a player gives up rather than looping.
pub const MAX_FAULTS: u32 = 3;
/// The rate a player renders at while its panel is not connected. Enough to
/// drive reconnection and to be ready the instant the panel comes back.
pub const IDLE_FPS: f64 = 5.0;
/// Frames per second a player may be asked for.
pub const MIN_FPS: f64 = 1.0;
pub const MAX_FPS: f64 = 60.0;

// ------------------------------------------------------------ the pieces ---

/// Pieces that misbehave on purpose, for the containment tests.
///
/// **Not in `screeny_art::pieces::ALL`** and not offered by `/api/v1/bootstrap`
/// unless the studio was started with fault pieces enabled, which only a test
/// and `SCREENY_STUDIO_FAULTS=1` do. They exist so that "a piece that panics is
/// contained" can be a test rather than a claim.
/// How long `fault-stall` stops returning for. Comfortably past the watchdog
/// and past `/healthz`'s patience, and then over.
pub const STALL_FOR: Duration = Duration::from_secs(30);

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
    /// Core threads started after the first: the count of recoveries.
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
    /// Sessions the link has opened: one at first connect, one per reconnect.
    pub sessions: u64,
    /// The brightness policy as actually applied by the device (its own cap
    /// may be lower than what was asked for).
    pub brightness_applied: Option<u8>,
    /// The last thing that went wrong here, if anything has.
    pub last_error: Option<String>,
}

/// A player, as the dashboard and `/api/v1/status` see it.
#[derive(Clone, Debug, Serialize)]
pub struct PlayerStatus {
    pub device: String,
    pub on: bool,
    pub piece: String,
    pub piece_name: String,
    pub seed: u32,
    pub params: BTreeMap<String, f32>,
    pub fps: f64,
    pub brightness: Option<u8>,
    pub settings: Settings,
    /// True when a core thread is alive and ticking.
    pub running: bool,
    /// The rate the render loop is actually achieving.
    pub fps_measured: f32,
    /// What a composing piece says it is performing.
    pub playing: Option<Playing>,
    pub health: PlayerHealth,
    /// The link, or `None` when the player is off.
    pub panel: Option<PanelStatus>,
}

// ----------------------------------------------------------------- a core ---

/// The render state of one piece. Thrown away whole when it misbehaves.
struct Core {
    def: &'static PieceDef,
    piece: Box<dyn Piece>,
    params: Params,
    pipeline: Pipeline,
    t: f64,
    last: Instant,
    fps: f32,
}

impl Core {
    fn tick(&mut self) -> WireFrame {
        let now = Instant::now();
        let wall = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let frame = self.piece.render(&Ctx { t: self.t, dt: wall, now: local_now(), params: &self.params });
        self.t += wall;
        if wall > 0.0 {
            self.fps += (1.0 / wall as f32 - self.fps) * 0.1;
        }
        self.pipeline.process(frame, wall).wire
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
    ticks: AtomicU64,
    fps: Mutex<f32>,
    playing: Mutex<Option<Playing>>,
}

/// A brightness the caller should apply, off the render thread.
pub struct BrightnessJob {
    pub device: String,
    pub level: u8,
}

// --------------------------------------------------------------- a player ---

/// One panel's player.
pub struct Player {
    /// The device id this plays to. Never an address.
    pub device: String,
    cfg: Mutex<StoredPlayer>,
    /// The panel link, outside the core on purpose: a core can be abandoned
    /// without losing the link, the device or the source lock.
    link: Mutex<LinkSlot>,
    core: Mutex<Option<Arc<CoreHandle>>>,
    health: Mutex<PlayerHealth>,
    stop: Arc<AtomicBool>,
    faults: bool,
    gen: AtomicU64,
    /// Faults since the last good run: the brake on a restart loop.
    consecutive: AtomicU64,
}

#[derive(Default)]
struct LinkSlot {
    out: Option<SenderOutput>,
    /// What the link was built for, so a changed address rebuilds it and an
    /// unchanged one does not.
    key: String,
    /// Sessions the last time we looked, for counting reconnects and for
    /// knowing when to re-apply the brightness policy.
    sessions: u32,
    /// When a frame last reached the wire.
    last_frame_unix: Option<u64>,
}

impl Player {
    #[must_use]
    pub fn new(cfg: StoredPlayer, faults: bool) -> Arc<Player> {
        Arc::new(Player {
            device: cfg.device.clone(),
            cfg: Mutex::new(cfg),
            link: Mutex::new(LinkSlot::default()),
            core: Mutex::new(None),
            health: Mutex::new(PlayerHealth::default()),
            stop: Arc::new(AtomicBool::new(false)),
            faults,
            gen: AtomicU64::new(0),
            consecutive: AtomicU64::new(0),
        })
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

    /// The desired configuration, as persisted.
    #[must_use]
    pub fn stored(&self) -> StoredPlayer {
        self.cfg().clone()
    }

    /// Change what it plays. Anything `None` is left alone.
    ///
    /// A piece change restarts the core, because a piece owns its own state
    /// and there is no meaningful way to carry it across.
    ///
    /// # Errors
    ///
    /// If no piece has that id.
    pub fn configure(self: &Arc<Self>, change: &PlayerChange) -> Result<(), String> {
        let mut restart = false;
        {
            let mut cfg = self.cfg();
            if let Some(id) = &change.piece {
                let def = find_piece(id, self.faults).ok_or_else(|| format!("no piece called `{id}`"))?;
                if cfg.piece != def.id {
                    cfg.piece = def.id.to_string();
                    cfg.params.clear();
                    restart = true;
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
                restart = true;
            }
            if let Some((id, value)) = &change.param {
                let def = find_piece(&cfg.piece, self.faults);
                let known = def.is_some_and(|d| d.params.iter().any(|p| p.id == id));
                if !known {
                    return Err(format!("{} has no parameter `{id}`", cfg.piece));
                }
                cfg.params.insert(id.clone(), *value);
                restart = true;
            }
            if let Some(fps) = change.fps {
                cfg.fps = fps.clamp(MIN_FPS, MAX_FPS);
            }
            if let Some(s) = change.settings {
                cfg.settings = s;
                restart = true;
            }
            if let Some(b) = change.brightness {
                cfg.brightness = b;
                // Re-apply on the next pass.
                self.slot().sessions = 0;
            }
            if let Some(on) = change.on {
                if cfg.on != on {
                    cfg.on = on;
                    restart = true;
                }
            }
            if change.replace.is_some() {
                let r = change.replace.clone().expect("checked");
                let def = find_piece(&r.piece, self.faults).ok_or_else(|| format!("no piece called `{}`", r.piece))?;
                cfg.piece = def.id.to_string();
                cfg.seed = r.seed;
                cfg.params = r.params;
                cfg.settings = r.settings;
                cfg.fps = r.fps.clamp(MIN_FPS, MAX_FPS);
                restart = true;
                let mut h = self.health_mut();
                h.refused.clear();
                h.gave_up = None;
                h.fell_back_from = None;
                self.consecutive.store(0, Ordering::Relaxed);
            }
        }
        if restart {
            self.restart_core("configured");
        }
        Ok(())
    }

    /// Point the link at a device, or at nothing. Rebuilds it only when what
    /// it is aimed at has actually changed.
    pub fn aim(self: &Arc<Self>, reach: &Reach) {
        let on = self.cfg().on;
        let key = reach_key(reach);
        let mut slot = self.slot();
        if !on || matches!(reach, Reach::Unknown) {
            if let Some(out) = slot.out.as_mut() {
                // FINAL: the panel is released now rather than after its
                // stream timeout.
                out.close();
            }
            slot.out = None;
            slot.key = String::new();
            return;
        }
        if slot.out.is_some() && slot.key == key {
            return;
        }
        if let Some(out) = slot.out.as_mut() {
            out.close();
        }
        slot.out = Some(open_link(reach));
        slot.key = key;
        slot.sessions = 0;
    }

    /// Start the render loop if it should be running and is not.
    pub fn ensure_running(self: &Arc<Self>) {
        if self.stop.load(Ordering::Relaxed) || !self.cfg().on {
            return;
        }
        if self.health_mut().gave_up.is_some() {
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
        let on = self.cfg().on;
        if !on {
            self.stop_core();
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
        let want = self.cfg().brightness;
        let mut slot = self.slot();
        if let Some(out) = slot.out.as_mut() {
            out.poll();
            let sessions = out.link().stats().sessions;
            let up = out.link().state().is_up();
            if up && sessions != slot.sessions {
                slot.sessions = sessions;
                if let Some(level) = want {
                    job = Some(BrightnessJob { device: self.device.clone(), level });
                }
            }
            self.health_mut().sessions = u64::from(sessions);
        }
        job
    }

    /// Record what the device actually applied, which may be below what was
    /// asked for: the firmware caps brightness and says so.
    pub fn brightness_applied(&self, applied: u8) {
        self.health_mut().brightness_applied = Some(applied);
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

    /// Everything the dashboard shows.
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
                    h.playing.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
                ),
                None => (false, 0, 0.0, None),
            }
        };
        let mut health = self.health_mut().clone();
        health.ticks = health.ticks.max(ticks);
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
            device: self.device.clone(),
            on: cfg.on,
            piece: cfg.piece,
            piece_name: name,
            seed: cfg.seed,
            params: cfg.params,
            fps: cfg.fps,
            brightness: cfg.brightness,
            settings: cfg.settings,
            running,
            fps_measured,
            playing,
            health,
            panel,
        }
    }

    /// An action offered by a composing piece.
    pub fn act(&self, _action: &str) {
        // Reaching into a running piece from another thread would need the
        // core's lock on the render path, which is exactly what card 105's
        // handover said to avoid. The design view is where pieces are played
        // with; a device player is left alone. (Card 142.)
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

    fn restart_core(self: &Arc<Self>, _why: &str) {
        self.stop_core();
        self.ensure_running();
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
            eprintln!("studio: player {}: {msg}", self.device);
            h.gave_up = Some(msg);
            if !h.refused.iter().any(|r| r == piece) {
                h.refused.push(piece.to_string());
            }
            // `on` is left alone: the player stays configured, just silent, so
            // that a human can see what it was meant to be playing.
            drop(cfg);
            return;
        }
        eprintln!("studio: player {}: {what}; falling back to `{}`", self.device, fallback.id);
        if !h.refused.iter().any(|r| r == piece) {
            h.refused.push(piece.to_string());
        }
        h.fell_back_from = Some(piece.to_string());
        cfg.piece = fallback.id.to_string();
        cfg.params.clear();
    }

    fn start_core(self: &Arc<Self>) {
        let (def, seed, params_map, settings, on) = {
            let cfg = self.cfg();
            (cfg.piece.clone(), cfg.seed, cfg.params.clone(), cfg.settings, cfg.on)
        };
        if !on {
            return;
        }
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
                        eprintln!("studio: player {}: no piece called `{def}`; playing `{}`", self.device, f.id);
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
        let core = Core {
            def: chosen,
            piece: (chosen.make)(u64::from(seed)),
            params,
            pipeline: Pipeline::new(settings),
            t: 0.0,
            last: Instant::now(),
            fps: 0.0,
        };

        let gen = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = Arc::new(CoreHandle {
            gen,
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            beat: AtomicU64::new(unix_millis()),
            ticks: AtomicU64::new(self.health_mut().ticks),
            fps: Mutex::new(0.0),
            playing: Mutex::new(None),
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
            .name(format!("play-{}", short(&self.device)))
            .spawn(move || run_core(&me, &handle, core));
        if let Err(e) = started {
            let mut h = self.health_mut();
            h.gave_up = Some(format!("could not start a render thread: {e}"));
            eprintln!("studio: player {}: could not start a render thread: {e}", self.device);
        }
    }
}

/// What a change asks for. Everything optional; `None` leaves it alone.
#[derive(Clone, Debug, Default)]
pub struct PlayerChange {
    pub on: Option<bool>,
    pub piece: Option<String>,
    pub seed: Option<u32>,
    pub param: Option<(String, f32)>,
    pub fps: Option<f64>,
    pub settings: Option<Settings>,
    /// `Some(None)` clears the brightness policy; `Some(Some(n))` sets it.
    pub brightness: Option<Option<u8>>,
    /// "Make what I am previewing what this panel plays": everything at once.
    pub replace: Option<Adopt>,
}

/// A whole configuration to adopt, as the preview hands it over.
#[derive(Clone, Debug)]
pub struct Adopt {
    pub piece: String,
    pub seed: u32,
    pub params: BTreeMap<String, f32>,
    pub settings: Settings,
    pub fps: f64,
}

/// The render loop of one core.
fn run_core(player: &Arc<Player>, handle: &Arc<CoreHandle>, mut core: Core) {
    let piece_id = core.def.id.to_string();
    let mut next = Instant::now();
    let mut limits_at = Instant::now() - Duration::from_secs(10);
    let mut panicked = false;

    while !handle.stop.load(Ordering::Relaxed) && !player.stop.load(Ordering::Relaxed) {
        handle.beat.store(unix_millis(), Ordering::Relaxed);

        // The piece is the only code here that can panic. Catching it is what
        // keeps one bad piece from taking the process - and the other panels -
        // down with it.
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.tick()));
        let Ok(wire) = rendered else {
            panicked = true;
            break;
        };
        handle.ticks.fetch_add(1, Ordering::Relaxed);
        *handle.fps.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = core.fps;
        if let Ok(mut p) = handle.playing.lock() {
            *p = core.piece.playing();
        }

        // Hand it to the panel. The link is the player's, not the core's.
        let connected = {
            let mut slot = player.slot();
            let mut landed = false;
            let up = match slot.out.as_mut() {
                Some(out) => {
                    if limits_at.elapsed() >= Duration::from_secs(1) {
                        limits_at = Instant::now();
                        let lim = out.limits();
                        if lim.connected {
                            core.pipeline.meter().set_limits(lim.budget, lim.codecs.clone());
                        }
                    }
                    let before = out.link().stats().frames_sent;
                    if let Err(e) = out.send(&wire) {
                        // Only our own errors can get here; the network cannot
                        // fail a send. Logged once.
                        let mut h = player.health_mut();
                        let msg = format!("sending to the panel: {e}");
                        if h.last_error.as_deref() != Some(msg.as_str()) {
                            eprintln!("studio: player {}: {msg}", player.device);
                        }
                        h.last_error = Some(msg);
                    }
                    landed = out.link().stats().frames_sent > before;
                    out.link().state().is_up()
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

        // While the panel is away there is nothing to be fast for.
        let rate = if connected { player.cfg().fps } else { IDLE_FPS };
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
    id.chars().take(8).collect()
}

#[must_use]
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ------------------------------------------------------------ the fleet ----

/// Every player, keyed by device id. A collection from day one.
#[derive(Default)]
pub struct Players {
    inner: Mutex<BTreeMap<String, Arc<Player>>>,
}

impl Players {
    #[must_use]
    pub fn new() -> Self {
        Players::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Player>>> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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

    /// The player for a device, made if it is not there yet.
    pub fn ensure(&self, device: &str, faults: bool, default: impl FnOnce() -> StoredPlayer) -> Arc<Player> {
        let mut all = self.lock();
        if let Some(p) = all.get(device) {
            return Arc::clone(p);
        }
        let mut cfg = default();
        cfg.device = device.to_string();
        let p = Player::new(cfg, faults);
        all.insert(device.to_string(), Arc::clone(&p));
        p
    }

    /// Adopt a player from the state file.
    pub fn load(&self, cfg: StoredPlayer, faults: bool) {
        if cfg.device.is_empty() {
            return;
        }
        let device = cfg.device.clone();
        self.lock().insert(device, Player::new(cfg, faults));
    }

    /// A device's id changed when it told us its real one. The player follows
    /// it: the same panel keeps playing the same thing.
    pub fn rekey(&self, from: &str, to: &str) {
        let mut all = self.lock();
        if from == to {
            return;
        }
        if let Some(old) = all.remove(from) {
            let mut cfg = old.stored();
            cfg.device = to.to_string();
            old.shutdown();
            // If the destination already had a player, the one that was
            // actually configured wins.
            all.insert(to.to_string(), Player::new(cfg, old.faults));
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
        let p = Player::new(StoredPlayer { device: "abc".into(), on: false, ..StoredPlayer::default() }, true);
        p.configure(&PlayerChange { piece: Some("plasma".into()), seed: Some(9), ..PlayerChange::default() })
            .expect("a real piece");
        let s = p.stored();
        assert_eq!(s.piece, "plasma");
        assert_eq!(s.seed, 9);
        assert!(!s.on, "configuring must not turn a stopped player on");

        assert!(p.configure(&PlayerChange { piece: Some("nope".into()), ..PlayerChange::default() }).is_err());
        assert!(p
            .configure(&PlayerChange { param: Some(("nope".into(), 1.0)), ..PlayerChange::default() })
            .is_err());
        assert_eq!(p.stored().piece, "plasma", "a rejected change must change nothing");
    }

    #[test]
    fn an_fps_outside_the_range_is_clamped_not_refused() {
        let p = Player::new(StoredPlayer { device: "abc".into(), on: false, ..StoredPlayer::default() }, false);
        p.configure(&PlayerChange { fps: Some(1000.0), ..PlayerChange::default() }).expect("clamped");
        assert_eq!(p.stored().fps, MAX_FPS);
        p.configure(&PlayerChange { fps: Some(0.0), ..PlayerChange::default() }).expect("clamped");
        assert_eq!(p.stored().fps, MIN_FPS);
    }

    #[test]
    fn a_fleet_is_a_collection_and_a_rekey_carries_the_player_over() {
        let players = Players::new();
        players.load(StoredPlayer { device: "pending:192.0.2.7".into(), piece: "plasma".into(), seed: 5, on: false, ..StoredPlayer::default() }, false);
        assert_eq!(players.ids(), vec!["pending:192.0.2.7".to_string()]);
        players.rekey("pending:192.0.2.7", "abc123");
        assert_eq!(players.ids(), vec!["abc123".to_string()]);
        let p = players.get("abc123").expect("moved");
        assert_eq!(p.stored().piece, "plasma");
        assert_eq!(p.stored().seed, 5);
        assert_eq!(p.stored().device, "abc123");
        players.remove("abc123");
        assert!(players.ids().is_empty());
    }
}
