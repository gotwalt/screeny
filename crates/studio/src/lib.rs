//! Screeny Studio: the `screeny-art` pipeline behind an HTTP + WebSocket
//! server, with the UI compiled in.
//!
//! One program, two ways of running it (`docs/design/studio-vision.md`): on a
//! laptop with a browser tab open on `127.0.0.1:8787`, or in a container on
//! the network. There is no desktop window. The engine thread renders whether
//! or not anything is watching - that is the property the whole plan rests on
//! - and a browser is a remote control and a preview, never the clock.
//!
//! ```text
//!  engine thread ──frame watch (one slot, newest wins)──┐
//!       │                                               ├── /api/v1/ws ── browsers
//!       ├── screeny::Link ──UDP──> panel                │
//!  HTTP /api/v1/* ──state broadcast (fixed capacity)────┘
//! ```
//!
//! Nothing in that picture can grow without bound, and nothing a browser does
//! (or stops doing) can slow the engine or the panel link down.
//!
//! Card 106 added the other half - the part that has to survive months with
//! nobody looking:
//!
//! ```text
//!   state file (atomic, versioned) ──> device registry ──> one player per device
//!                                            ^                     │
//!   mDNS browse + manual addresses ──────────┘                     └── screeny::Link ──> panel
//!   supervisor (1 Hz): watchdog, fallback, reconnect, brightness policy
//!   /healthz 200/503, /api/v1/status: what each panel is playing and whether it is well
//! ```
//!
//! The design view keeps its own *preview* player (the engine above); a device
//! player is a separate thing, and promoting what you are previewing to what a
//! panel plays is an explicit action.

pub mod api;
pub mod devices;
pub mod engine;
pub mod fleet;
pub mod health;
pub mod player;
pub mod state;
pub mod ui;
pub mod ws;

use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

use crate::api::{StateEvent, StatusEvent};
use crate::devices::Registry;
use crate::engine::{lock, Engine, Shared, StudioState};
use crate::player::Players;
use crate::state::Store;

/// State changes a browser may be behind by before it is resynced instead.
/// Small on purpose: the newest state is always the right answer, so there is
/// no reason to keep a history.
const STATE_BACKLOG: usize = 32;
/// How often the "now playing" and panel-link heartbeat goes out.
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// How long after starting a player is allowed to not be running yet before
/// that counts against `/healthz`.
pub const START_GRACE: Duration = Duration::from_secs(15);

/// How to run.
///
/// Two of these defaults are deliberately *not* what the product does, because
/// a worker's test must never reach the LAN: `discover` is off and `state_dir`
/// is `None` (nothing is written). `main` turns discovery on and picks a state
/// directory; a test that wants either says so.
#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    /// `None` serves the UI compiled into the binary.
    pub ui_dir: Option<PathBuf>,
    /// Where the state file lives. `None` keeps everything in memory.
    pub state_dir: Option<PathBuf>,
    /// Browse `_screeny._udp` for panels. Off here on purpose; see above.
    pub discover: bool,
    /// How often to browse.
    pub discover_every: Duration,
    /// How often to ask each known device for its telemetry.
    pub telemetry_every: Duration,
    /// How often the supervisor looks at the players.
    pub supervise_every: Duration,
    /// Offer the deliberately broken pieces (`fault-panic`, `fault-stall`).
    /// The containment tests, and `SCREENY_STUDIO_FAULTS=1` for a human who
    /// wants to watch it happen. Never in the normal piece list.
    pub fault_pieces: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 8787)),
            ui_dir: None,
            state_dir: None,
            discover: false,
            discover_every: Duration::from_secs(30),
            telemetry_every: Duration::from_secs(5),
            supervise_every: Duration::from_secs(1),
            fault_pieces: false,
        }
    }
}

/// How the design view's own player is doing.
///
/// The preview engine is card 105's, unchanged in substance: one thread, one
/// piece. What card 106 adds is a heartbeat, a panic count and a fallback, so
/// that "is the server well" has an answer that includes it.
#[derive(Default)]
pub struct PreviewHealth {
    /// Milliseconds since the epoch, stamped before every frame.
    pub beat: AtomicU64,
    pub ticks: AtomicU64,
    pub panics: AtomicU64,
    pub alive: AtomicBool,
    /// Set when the preview has panicked [`player::MAX_FAULTS`] times in a
    /// row, fallback included. The thread stays up and idle; `/healthz` says
    /// the server is not well.
    pub gave_up: Mutex<Option<String>>,
    /// The piece it fell back from, if it did.
    pub fell_back_from: Mutex<Option<String>>,
}

/// Everything a request handler can reach. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub engine: Shared,
    /// The newest frame packet. One slot: a frame nobody read is overwritten.
    pub frames: watch::Sender<Arc<Vec<u8>>>,
    /// State changes, `STATE_BACKLOG` deep. Overflow is not an error here: a
    /// listener that falls behind is sent the current state instead.
    pub states: broadcast::Sender<StateEvent>,
    /// The half-second heartbeat, in one slot for the same reason as frames.
    pub status: watch::Sender<Arc<StatusEvent>>,
    pub ui: Arc<ui::Ui>,
    /// True once the studio has been asked to stop. Everything that would
    /// otherwise run for ever - the engine thread, the status task, every
    /// open preview socket - watches this, so a shutdown is not held up by a
    /// browser that is perfectly happy.
    pub stop: watch::Receiver<bool>,
    /// Every panel this studio knows about, keyed by its own stable id.
    pub devices: Arc<Registry>,
    /// One player per device.
    pub players: Arc<Players>,
    /// The state file.
    pub store: Arc<Store>,
    pub cfg: Arc<Config>,
    /// How the preview engine is doing.
    pub preview: Arc<PreviewHealth>,
    /// When the process started, for `/api/v1/status` and for the grace period
    /// `/healthz` gives a player that has not started yet.
    pub started: Instant,
    rev: Arc<AtomicU64>,
}

impl AppState {
    fn new(
        engine: Shared,
        ui: ui::Ui,
        stop: watch::Receiver<bool>,
        cfg: Arc<Config>,
        store: Arc<Store>,
        devices: Arc<Registry>,
        players: Arc<Players>,
    ) -> Self {
        let packet = lock(&engine).packet();
        AppState {
            frames: watch::Sender::new(packet),
            states: broadcast::Sender::new(STATE_BACKLOG),
            status: watch::Sender::new(Arc::new(StatusEvent { kind: "status", playing: None, panel: None })),
            engine,
            ui: Arc::new(ui),
            stop,
            devices,
            players,
            store,
            cfg,
            preview: Arc::new(PreviewHealth::default()),
            started: Instant::now(),
            rev: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Write everything worth keeping to the state file.
    ///
    /// Called after every change. It never blocks: the store's writer thread
    /// has a one-slot mailbox and the newest state wins, so a slider being
    /// dragged costs one write rather than sixty.
    pub fn persist(&self) {
        let preview = {
            let e = lock(&self.engine);
            let snap = e.snapshot();
            let (panel_on, panel_to) = e.panel_aim();
            state::StoredPreview {
                piece: snap.piece.to_string(),
                seed: snap.seed,
                params: snap.params.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
                settings: snap.settings,
                paused: snap.paused,
                speed: snap.speed,
                fps: snap.fps,
                panel_on,
                panel_to,
            }
        };
        self.store.save(state::Persisted {
            version: state::SCHEMA_VERSION,
            devices: self.devices.stored(),
            players: self.players.stored(),
            preview,
        });
    }

    /// Stamp a state change and hand it to everyone watching.
    #[must_use]
    pub fn state_event(&self, from: Option<String>, state: StudioState) -> StateEvent {
        StateEvent { kind: "state", rev: self.rev.fetch_add(1, Ordering::Relaxed) + 1, from, state }
    }

    pub fn publish_state(&self, from: Option<String>, state: StudioState) {
        // `send` fails only when nobody is listening, which is the normal case
        // for a server with no browser open.
        let _ = self.states.send(self.state_event(from, state));
    }
}

/// A studio that is listening but not yet serving.
pub struct Studio {
    /// What it actually bound to. With port 0 in the config, this is the port
    /// the OS chose - which is how the tests get an ephemeral server.
    pub addr: SocketAddr,
    listener: TcpListener,
    state: AppState,
    stop: watch::Sender<bool>,
}

/// Stops the studio when dropped: the engine thread ends, the panel link
/// sends `FINAL`, and the server stops accepting. Tests hold one of these so
/// that a finished test leaves nothing running.
pub struct Running {
    pub addr: SocketAddr,
    stop: watch::Sender<bool>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Studio {
    /// Bind the port and start the engine.
    ///
    /// # Errors
    ///
    /// If the listen address cannot be bound.
    pub async fn bind(cfg: Config) -> std::io::Result<Self> {
        // The state file first: everything else is built from what it says.
        let (store, saved) = Store::open(cfg.state_dir.as_deref());
        let devices = Arc::new(Registry::new());
        devices.load(saved.devices);
        devices.set_discovery_enabled(cfg.discover);
        let players = Arc::new(Players::new());
        for p in saved.players {
            players.load(p, cfg.fault_pieces);
        }

        let engine: Shared = Arc::new(Mutex::new(Engine::new()));
        restore_preview(&engine, &saved.preview, cfg.fault_pieces);

        let (stop, _) = watch::channel(false);
        let cfg = Arc::new(cfg);
        let state = AppState::new(
            engine,
            ui::Ui { dir: cfg.ui_dir.clone() },
            stop.subscribe(),
            Arc::clone(&cfg),
            store,
            devices,
            players,
        );
        let listener = TcpListener::bind(cfg.listen).await?;
        let addr = listener.local_addr()?;

        spawn_engine(state.clone());
        spawn_status(state.clone());
        fleet::spawn_supervisor(state.clone());
        fleet::spawn_discovery(state.clone());
        fleet::spawn_telemetry(state.clone());

        Ok(Studio { addr, listener, state, stop })
    }

    /// Everything a handler can reach, for a test that would rather poke the
    /// server in process than over HTTP.
    #[must_use]
    pub fn state(&self) -> AppState {
        self.state.clone()
    }

    /// Stop on Ctrl-C and on `SIGTERM` - which is what `docker stop` sends.
    ///
    /// Worth the twenty lines: `Drop` does not run on a signal (the same edge
    /// card 101 hit in `screeny-art play`), so without this the panel would be
    /// left holding the last frame until its stream timeout every time the
    /// container was restarted.
    pub fn stop_on_signal(&self) {
        let stop = self.stop.clone();
        tokio::spawn(async move {
            signalled().await;
            eprintln!("studio: stopping; releasing the panel");
            let _ = stop.send(true);
        });
    }

    /// The router, for a test that would rather call it in process.
    pub fn router(&self) -> Router {
        router(self.state.clone())
    }

    /// Serve until the process is asked to stop.
    ///
    /// # Errors
    ///
    /// If the server cannot accept connections.
    pub async fn serve(self) -> std::io::Result<()> {
        let mut stopped = self.stop.subscribe();
        let st = self.state.clone();
        let app = router(self.state);
        axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                let _ = stopped.wait_for(|s| *s).await;
            })
            .await?;
        // Let every panel go at once rather than after its stream timeout, and
        // make sure the last thing that changed is on disk before we go.
        lock(&st.engine).close_panel();
        st.players.shutdown();
        st.persist();
        st.store.flush();
        Ok(())
    }

    /// Serve in the background, and stop when the returned handle is dropped.
    #[must_use]
    pub fn spawn(self) -> Running {
        let running = Running { addr: self.addr, stop: self.stop.clone() };
        tokio::spawn(async move {
            if let Err(e) = self.serve().await {
                eprintln!("studio: serving: {e}");
            }
        });
        running
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .nest("/api/v1", api::routes())
        .route("/api/v1/ws", axum::routing::any(ws::upgrade))
        // The container healthcheck (card 107). 200 while the server itself is
        // doing its job; 503 only when it is not. A panel being unplugged is
        // normal life and never appears here: restarting the container is not
        // the answer to a missing panel. See `health::healthz`.
        .route("/healthz", axum::routing::get(health::healthz))
        .fallback(ui::serve)
        .with_state(state)
}

/// Put the design view back the way it was before the restart.
///
/// Deliberately forgiving: a piece that no longer exists, a parameter that has
/// been renamed and an fps that is no longer offered are each ignored rather
/// than fatal. A state file from an older build must never stop the server.
fn restore_preview(engine: &Shared, saved: &state::StoredPreview, faults: bool) {
    let mut e = lock(engine);
    if player::find_piece(&saved.piece, faults).is_some() && e.set_piece(&saved.piece).is_err() {
        eprintln!("studio: the saved piece `{}` could not be loaded; starting on the default", saved.piece);
    }
    e.set_seed(Some(saved.seed));
    for (id, v) in &saved.params {
        let _ = e.set_param(id, *v);
    }
    e.set_settings(saved.settings);
    e.set_playback(saved.paused, saved.speed, saved.fps);
    if saved.panel_on {
        e.set_panel(true, &saved.panel_to);
    }
}

/// Ctrl-C, or `SIGTERM`.
async fn signalled() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// The clock. A plain OS thread, not a tokio task: a frame is several
/// milliseconds of arithmetic and has no business on a runtime worker.
fn spawn_engine(st: AppState) {
    let stop = st.stop.clone();
    st.preview.alive.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let mut next = std::time::Instant::now();
            let mut consecutive = 0u32;
            while !*stop.borrow() {
                st.preview.beat.store(player::unix_millis(), Ordering::Relaxed);
                if st.preview.gave_up.lock().is_ok_and(|g| g.is_some()) {
                    // Idle rather than loop: the server and the device players
                    // carry on, and `/healthz` says the preview is broken.
                    std::thread::sleep(Duration::from_millis(500));
                    next = std::time::Instant::now();
                    continue;
                }
                // A piece that panics must not take the process - or the other
                // panels - with it. Caught, counted, and replaced by a safe
                // fallback; said once, not once a frame.
                let ticked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock(&st.engine).tick()));
                if ticked.is_err() {
                    consecutive += 1;
                    st.preview.panics.fetch_add(1, Ordering::Relaxed);
                    let failed = lock(&st.engine).snapshot().piece;
                    if consecutive >= player::MAX_FAULTS {
                        let msg = format!("`{failed}` panicked {consecutive} times in a row; the preview has stopped");
                        eprintln!("studio: preview: {msg}");
                        if let Ok(mut g) = st.preview.gave_up.lock() {
                            *g = Some(msg);
                        }
                        continue;
                    }
                    let fallback = player::fallback_piece(failed);
                    eprintln!("studio: preview: `{failed}` panicked while rendering; falling back to `{}`", fallback.id);
                    if let Ok(mut f) = st.preview.fell_back_from.lock() {
                        *f = Some(failed.to_string());
                    }
                    let _ = lock(&st.engine).set_piece(fallback.id);
                    st.publish_state(None, lock(&st.engine).snapshot());
                    st.persist();
                } else {
                    st.preview.ticks.fetch_add(1, Ordering::Relaxed);
                    if st.preview.ticks.load(Ordering::Relaxed) % 300 == 0 {
                        consecutive = 0;
                    }
                }
                // One slot, replaced in place: a browser that is not keeping
                // up misses frames and costs nothing. This never blocks, so a
                // browser cannot hold the engine or the panel link back.
                st.frames.send_replace(lock(&st.engine).packet());
                next += Duration::from_secs_f64(1.0 / lock(&st.engine).rate());
                let now = std::time::Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
            lock(&st.engine).close_panel();
            st.preview.alive.store(false, Ordering::Relaxed);
        })
        .expect("spawn engine thread");
}

/// The heartbeat: polled once here however many browsers are watching.
fn spawn_status(st: AppState) {
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(STATUS_EVERY);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let (playing, panel) = {
                        let mut e = lock(&st.engine);
                        (e.playing(), e.panel_status())
                    };
                    st.status.send_replace(Arc::new(StatusEvent { kind: "status", playing, panel }));
                }
                _ = stop.wait_for(|s| *s) => break,
            }
        }
    });
}
