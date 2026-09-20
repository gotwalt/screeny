//! Screeny Studio: the `screeny-art` pipeline behind an HTTP + WebSocket
//! server, with the UI compiled in.
//!
//! One program, two ways of running it (`docs/design/studio-vision.md`): on a
//! laptop with a browser tab open on `127.0.0.1:8787`, or in a container on
//! the network. There is no desktop window.
//!
//! **One panel, one picture** (card 170). A Studio is set up once against a
//! panel and is then almost always connected to it, and the web page is a
//! window onto what that panel is doing - for when the panel is not within
//! eyesight. So there is exactly one thing that renders, the player for the
//! attached panel, and the frames the browser draws are the same decoded
//! datagrams the panel is being sent. Changing a piece, a slider or a seed in
//! the browser changes the panel, at once.
//!
//! ```text
//!   state file (atomic, versioned) ──> device registry ──> one player per panel
//!                                            ^                     │
//!   mDNS browse + manual addresses ──────────┘                     ├── screeny::Link ──UDP──> panel
//!   supervisor (1 Hz): watchdog, fallback, reconnect, brightness   │
//!                                                                  └── the page's frame cell
//!                                                                          │ (one slot, newest wins)
//!   HTTP /api/v1/* ──> the focused player   ──state broadcast──> /api/v1/ws ──> browsers
//! ```
//!
//! Nothing in that picture can grow without bound, and nothing a browser does
//! (or stops doing) can slow a player or the panel link down.
//!
//! A studio always has a player, because the page always has a picture: with
//! no panel found yet it is *unbound* and has no link, and the first panel
//! found is adopted into that same player without the picture restarting.

pub mod api;
pub mod devhttp;
pub mod devices;
pub mod fleet;
pub mod health;
pub mod page;
pub mod player;
pub mod state;
pub mod ui;
pub mod ws;

use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

use crate::api::{StateEvent, StatusEvent};
use crate::devices::Registry;
use crate::page::{Screen, StudioState};
use crate::player::{Player, Players};
use crate::state::{SharedMemory, Store};

/// State changes a browser may be behind by before it is resynced instead.
/// Small on purpose: the newest state is always the right answer, so there is
/// no reason to keep a history.
const STATE_BACKLOG: usize = 32;
/// How often the "now playing" and panel-link heartbeat goes out.
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// How long after starting a player is allowed to not be running yet before
/// that counts against `/healthz`.
pub const START_GRACE: Duration = Duration::from_secs(15);
/// **The fastest a device's own HTTP status API may be read** (card 180).
///
/// Not a preference: the device has one connection worker and no listen
/// backlog (firmware card 222), so every request costs it a socket nothing
/// else can have while it is open. The firmware session measured 200 requests
/// in 60 s during a 30 fps stream costing no frame, and asked for ten seconds.
/// [`Config::default`] carries this and `main` clamps to it, so nothing that
/// can reach a real panel can go faster; a test against the simulator on
/// loopback sets its own.
pub const MIN_DEVICE_HTTP_EVERY: Duration = Duration::from_secs(10);

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
    /// How often to browse - and, card 141, how often a panel that has been
    /// unheard past [`Config::stale_after`] is probed for.
    pub discover_every: Duration,
    /// Where the "has it moved?" probe asks (card 141, spec 5.5). **Empty -
    /// the default - means the subnet broadcast address of every interface**,
    /// and the probe then follows `discover`: it is a LAN packet, so it is off
    /// wherever the browse is, including under `--no-discover`. A non-empty
    /// list is control addresses to ask instead, which is what a container
    /// with no broadcast route needs and what the loopback tests use; naming
    /// them turns the probe on by itself, because nothing it sends can then
    /// leave the addresses it was pointed at.
    pub probe_to: Vec<SocketAddr>,
    /// How often to ask each known device for its telemetry.
    pub telemetry_every: Duration,
    /// Read each device's own HTTP status API at all (card 180). On by
    /// default: a device that does not serve one costs one refused connection
    /// and is then left alone.
    pub device_http: bool,
    /// How often to read it. Never below [`MIN_DEVICE_HTTP_EVERY`] on anything
    /// that can reach a real panel; see that constant.
    pub device_http_every: Duration,
    /// Which port to read it on, for every device that does not say otherwise
    /// ([`devices::DeviceRecord::http_port`]). The device serves 80; a
    /// simulator cannot bind 80 without root, hence the override.
    pub device_http_port: u16,
    /// How often the supervisor looks at the players.
    pub supervise_every: Duration,
    /// How long a device may go unheard before the studio throws away where
    /// it thought the device was and finds out again. Long on purpose: a
    /// panel rebooting is not a panel that has moved, and re-resolving means
    /// a new link and a lost session.
    pub stale_after: Duration,
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
            probe_to: Vec::new(),
            telemetry_every: Duration::from_secs(5),
            device_http: true,
            device_http_every: MIN_DEVICE_HTTP_EVERY,
            device_http_port: devhttp::DEFAULT_PORT,
            supervise_every: Duration::from_secs(1),
            stale_after: Duration::from_secs(120),
            fault_pieces: false,
        }
    }
}

/// Everything a request handler can reach. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// The page's frame cell: one slot, filled by the focused player.
    pub screen: Arc<Screen>,
    /// State changes, `STATE_BACKLOG` deep. Overflow is not an error here: a
    /// listener that falls behind is sent the current state instead.
    pub states: broadcast::Sender<StateEvent>,
    /// The half-second heartbeat, in one slot for the same reason as frames.
    pub status: watch::Sender<Arc<StatusEvent>>,
    pub ui: Arc<ui::Ui>,
    /// True once the studio has been asked to stop. Everything that would
    /// otherwise run for ever - every player, the status task, every open
    /// preview socket - watches this, so a shutdown is not held up by a
    /// browser that is perfectly happy.
    pub stop: watch::Receiver<bool>,
    /// Every panel this studio knows about, keyed by its own stable id.
    pub devices: Arc<Registry>,
    /// One player per panel, plus the unbound one when there is no panel yet.
    pub players: Arc<Players>,
    /// The state file.
    pub store: Arc<Store>,
    /// The studio's one per-piece settings memory (card 165): what each piece
    /// was last left set to, wherever it was left. Every panel shares it, and
    /// it is written to `state.json` and nowhere else.
    pub memory: SharedMemory,
    pub cfg: Arc<Config>,
    /// When the process started, for `/api/v1/status` and for the grace period
    /// `/healthz` gives a player that has not started yet.
    pub started: Instant,
    rev: Arc<AtomicU64>,
}

impl AppState {
    fn new(
        ui: ui::Ui,
        stop: watch::Receiver<bool>,
        cfg: Arc<Config>,
        store: Arc<Store>,
        devices: Arc<Registry>,
        players: Arc<Players>,
    ) -> Self {
        let memory = players.memory();
        let screen = players.screen();
        AppState {
            screen,
            states: broadcast::Sender::new(STATE_BACKLOG),
            status: watch::Sender::new(Arc::new(StatusEvent { kind: "status", playing: None, panel: None })),
            ui: Arc::new(ui),
            stop,
            devices,
            players,
            store,
            memory,
            cfg,
            started: Instant::now(),
            rev: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The player the page is a window onto, made if the studio has none yet.
    #[must_use]
    pub fn page(&self) -> Arc<Player> {
        self.players.ensure_page(self.cfg.fault_pieces)
    }

    /// What the page is showing, which is what the panel is showing.
    #[must_use]
    pub fn page_state(&self) -> StudioState {
        self.page().state()
    }

    /// Write everything worth keeping to the state file.
    ///
    /// Called after every change. It never blocks: the store's writer thread
    /// has a one-slot mailbox and the newest state wins, so a slider being
    /// dragged costs one write rather than sixty.
    pub fn persist(&self) {
        self.store.save(state::Persisted {
            version: state::SCHEMA_VERSION,
            devices: self.devices.stored(),
            players: self.players.stored(),
            focus: self.players.focus(),
            // Card 165. It lives here and nowhere else: the state file in
            // `SCREENY_STATE_DIR` is the volume the container keeps across a
            // rebuild, so this is what makes the memory survive a deploy.
            pieces: self.memory.snapshot(),
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

/// Stops the studio when dropped: every player ends, the panel link sends
/// `FINAL`, and the server stops accepting. Tests hold one of these so that a
/// finished test leaves nothing running.
pub struct Running {
    pub addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Stop, and wait until the server has finished stopping - the panels
    /// released and the state file written.
    ///
    /// A restart test needs this rather than a sleep: "kill the server and
    /// start it again" is only a fair test if the first one really did finish.
    pub async fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Studio {
    /// Bind the port and start the players.
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
        // One settings memory for the whole studio: every panel reads and
        // writes the same map (card 165).
        let memory = SharedMemory::new(saved.pieces);
        let players = Arc::new(Players::new(memory.clone(), Screen::new()));
        for p in saved.players {
            players.load(p, cfg.fault_pieces);
        }
        if !saved.focus.is_empty() {
            players.set_focus(&saved.focus);
        }
        // There is always a picture, even before there is a panel.
        players.ensure_page(cfg.fault_pieces);

        let (stop, _) = watch::channel(false);
        let cfg = Arc::new(cfg);
        let state = AppState::new(
            ui::Ui { dir: cfg.ui_dir.clone() },
            stop.subscribe(),
            Arc::clone(&cfg),
            store,
            devices,
            Arc::clone(&players),
        );
        let listener = TcpListener::bind(cfg.listen).await?;
        let addr = listener.local_addr()?;

        // Start rendering at once rather than on the supervisor's first tick,
        // so a browser that opens immediately is not shown a black panel.
        for p in players.all() {
            p.ensure_running();
        }

        spawn_status(state.clone());
        fleet::spawn_supervisor(state.clone());
        fleet::spawn_discovery(state.clone());
        fleet::spawn_telemetry(state.clone());
        fleet::spawn_device_http(state.clone());

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
        st.players.shutdown();
        st.persist();
        st.store.flush();
        Ok(())
    }

    /// Serve in the background, and stop when the returned handle is dropped.
    #[must_use]
    pub fn spawn(self) -> Running {
        let addr = self.addr;
        let stop = self.stop.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = self.serve().await {
                eprintln!("studio: serving: {e}");
            }
        });
        Running { addr, stop, task: Some(task) }
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

/// The heartbeat: read once here however many browsers are watching.
///
/// Since card 170 this cannot be held up by a wedged piece. A player's core is
/// owned by its own render thread and is behind no shared lock, so "what is it
/// performing and what is the link doing" is always answerable - which is what
/// card 106's `try_lock` dance around the design view's engine was for, and
/// what card 143 was going to fix.
fn spawn_status(st: AppState) {
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(STATUS_EVERY);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let page = st.page();
                    let status = page.status();
                    st.status.send_replace(Arc::new(StatusEvent {
                        kind: "status",
                        playing: status.playing.clone(),
                        panel: status.panel.clone(),
                    }));
                }
                // Discarded inside the block: the borrow `wait_for` hands back
                // is not `Send` and this future has to be.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}
