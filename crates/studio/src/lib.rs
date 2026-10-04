//! Screeny Studio: the `screeny-art` pipeline behind an HTTP + WebSocket
//! server, with the UI compiled in.
//!
//! One program, two ways of running it (`docs/design/studio-vision.md`): on a
//! laptop with a browser tab open on `127.0.0.1:8787`, or in a container on
//! the network. There is no desktop window.
//!
//! **Channels own the picture** (cards 350-355). A *channel* renders, limits,
//! quantises and encodes a picture **once** per tick; every *panel* on it is
//! sent that one encoded frame byte for byte, so panels on one channel are
//! frame-for-frame identical and none of them is the master. Several channels
//! can play at once (`docs/design/studio-vision.md`, "Several panels: channels
//! own the picture"). The web page is a window onto a channel: the frames a
//! browser draws are that channel's encoded frames, decoded - what every panel
//! on it shows.
//!
//! ```text
//!   state file (atomic, versioned) ──> device registry ──> a panel per device (joins Channel 1)
//!                                            ^                     │ is on one channel
//!   mDNS browse + manual addresses ──────────┘                     v
//!   supervisor (1 Hz): watchdog, fallback,     channels: render ─> pipeline ─> encode, once per tick
//!   reconnect, brightness, adoption                  │ the same payload ─> each panel's screeny::Link ──UDP──> panel
//!                                                    └─> the channel's frame cell (one slot, newest wins)
//!   HTTP /api/v1/*?channel= ──> a channel (absent: Channel 1)    │
//!                              ──state broadcast──────────> /api/v1/ws?channel= ──> browsers
//! ```
//!
//! Nothing in that picture can grow without bound, and nothing a browser does
//! (or stops doing) can slow a channel or a panel link down.
//!
//! A studio always has a picture: Channel 1 exists before any panel does, and
//! renders while somebody watches it.

pub mod api;
pub mod app;
pub mod channel;
pub mod devhttp;
pub mod devices;
pub mod firmware;
pub mod fleet;
pub mod ha;
pub mod health;
pub mod page;
pub mod panel;
pub mod panels;
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

use crate::api::Changed;
use crate::channel::{Channel, ChannelId};
use crate::devices::Registry;
use crate::page::StudioState;
use crate::panel::Panel;
use crate::panels::{ChannelSummary, PanelSummary, Panels};
use crate::state::{SharedMemory, Store};
use screeny_art::output::PanelStatus;
use screeny_art::patch::Playing;

/// State changes a browser may be behind by before it is resynced instead.
/// Small on purpose: the newest state is always the right answer, so there is
/// no reason to keep a history.
const STATE_BACKLOG: usize = 32;
/// How often the "now playing" and panel-link heartbeat goes out.
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// How long after starting a channel is allowed to not be running yet before
/// that counts against `/healthz`.
pub const START_GRACE: Duration = Duration::from_secs(15);
/// **The fastest a device's own HTTP status API may be read** (card 180).
///
/// Not a preference: the device has one connection worker and no listen
/// backlog (firmware card 222), so every request costs it a socket nothing
/// else can have while it is open. Testing on the real device measured 200 requests
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
    /// Offer the deliberately broken patches (`fault-panic`, `fault-stall`).
    /// The containment tests, and `SCREENY_STUDIO_FAULTS=1` for a human who
    /// wants to watch it happen. Never in the normal patch list.
    pub fault_patches: bool,
    /// Card 308: a Home Assistant connection to start with **when the state
    /// file has no settings of its own** - which is how a test starts a
    /// connected studio. The product never sets it: the owner sets the
    /// integration up on the Settings screen (card 311), and that is kept in
    /// `state.json`. `None` - the default - talks to no broker.
    pub mqtt: Option<ha::MqttConfig>,
    /// Card 359, app mode only (`app::plan`): when set, `listen` serves only
    /// these peers - everyone else gets 403. `None` is every plain deployment.
    pub peers: Option<Vec<std::net::IpAddr>>,
    /// Card 359: a second, unchecked listener (the app's `direct_access`).
    pub direct: Option<SocketAddr>,
    /// Card 359: the Home Assistant Supervisor, whose MQTT broker is used
    /// when the owner has set none.
    pub supervisor: Option<ha::supervisor::Supervisor>,
    /// Card 364: the firmware image this Studio offers to panels (the app
    /// image, `screeny-fw-<v>.bin`). `None`, or a path with nothing at it,
    /// offers nothing; everything else works.
    pub firmware: Option<PathBuf>,
    /// Card 364: how patient an update is. Tests make it quick.
    pub firmware_timing: firmware::Timing,
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
            fault_patches: false,
            mqtt: None,
            peers: None,
            direct: None,
            supervisor: None,
            firmware: None,
            firmware_timing: firmware::Timing::default(),
        }
    }
}

/// The half-second heartbeat for every channel and every panel at once: one
/// read per tick however many browsers are watching, and each socket picks out
/// its own channel's and panel's.
#[derive(Default)]
pub struct StatusBoard {
    /// What each channel's patch is performing.
    pub channels: std::collections::BTreeMap<ChannelId, Option<Playing>>,
    /// Each panel's link, by device id.
    pub panels: std::collections::BTreeMap<String, Option<PanelStatus>>,
}

/// Everything the page's rows are drawn from (cards 350, 353): every panel and
/// every channel, in one slot, replaced only when it differs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overview {
    pub panels: Vec<PanelSummary>,
    pub channels: Vec<ChannelSummary>,
}

/// Everything a request handler can reach. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// "Something changed", `STATE_BACKLOG` deep. Each socket turns it into
    /// its own panel's state when it sends it (card 350). Overflow is not an
    /// error here: a listener that falls behind is sent the current state.
    pub states: broadcast::Sender<Changed>,
    /// The half-second heartbeat, in one slot for the same reason as frames.
    pub status: watch::Sender<Arc<StatusBoard>>,
    /// Cards 350, 353: every panel and every channel, in one slot, replaced
    /// only when it differs.
    pub overview: watch::Sender<Arc<Overview>>,
    pub ui: Arc<ui::Ui>,
    /// True once the studio has been asked to stop. Everything that would
    /// otherwise run for ever - every channel, the status task, every open
    /// preview socket - watches this, so a shutdown is not held up by a
    /// browser that is perfectly happy.
    pub stop: watch::Receiver<bool>,
    /// Every panel this studio knows about, keyed by its own stable id.
    pub devices: Arc<Registry>,
    /// Every panel and every channel: one panel per device, each on a
    /// channel, and always Channel 1.
    pub panels: Arc<Panels>,
    /// The state file.
    pub store: Arc<Store>,
    /// The studio's one library of named settings (card 151), shared by
    /// every channel. Written to `state.json` and nowhere else.
    pub memory: SharedMemory,
    pub cfg: Arc<Config>,
    /// When the process started, for `/api/v1/status` and for the grace period
    /// `/healthz` gives a channel that has not started yet.
    pub started: Instant,
    /// Card 311: Home Assistant - its settings, and how the connection is
    /// doing.
    pub ha: Arc<ha::Ha>,
    /// Card 364: the firmware this Studio offers, and every panel's update.
    pub firmware: Arc<firmware::Firmware>,
    rev: Arc<AtomicU64>,
}

impl AppState {
    fn new(
        ui: ui::Ui,
        stop: watch::Receiver<bool>,
        cfg: Arc<Config>,
        store: Arc<Store>,
        devices: Arc<Registry>,
        panels: Arc<Panels>,
        ha: Arc<ha::Ha>,
    ) -> Self {
        let memory = panels.memory();
        let firmware = Arc::new(firmware::Firmware::load(cfg.firmware.as_deref(), cfg.firmware_timing));
        AppState {
            states: broadcast::Sender::new(STATE_BACKLOG),
            status: watch::Sender::new(Arc::new(StatusBoard::default())),
            overview: watch::Sender::new(Arc::new(Overview::default())),
            ui: Arc::new(ui),
            stop,
            devices,
            panels,
            store,
            memory,
            cfg,
            started: Instant::now(),
            ha,
            firmware,
            rev: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The first panel adopted, if there is one: what a panel route without
    /// `panel` means.
    #[must_use]
    pub fn first(&self) -> Option<Arc<Panel>> {
        self.panels.first()
    }

    /// The panel a request named, or the first one when it named none.
    ///
    /// # Errors
    ///
    /// When it names a panel this studio does not have, or names none and
    /// there is no panel at all.
    pub fn panel(&self, which: Option<&str>) -> Result<Arc<Panel>, String> {
        match which.map(str::trim).filter(|w| !w.is_empty()) {
            None => self.first().ok_or_else(|| "there is no panel yet".to_string()),
            Some(id) => self.panels.get(id).ok_or_else(|| format!("no panel `{id}`")),
        }
    }

    /// **The channel a request means** (card 353): the `channel` it named;
    /// else the channel of the `panel` it named - which is also handed back,
    /// as the panel the state is seen from; else Channel 1.
    ///
    /// # Errors
    ///
    /// When it names a channel or a panel this studio does not have.
    pub fn channel(&self, channel: Option<ChannelId>, panel: Option<&str>) -> Result<(Arc<Channel>, Option<Arc<Panel>>), String> {
        if let Some(id) = channel {
            let c = self.panels.channel(id).ok_or_else(|| format!("no channel {id}"))?;
            // A panel named as well is only the point of view, if it is on it.
            let from = panel.and_then(|p| self.panels.get(p.trim())).filter(|p| p.channel().is_some_and(|pc| Arc::ptr_eq(&pc, &c)));
            return Ok((c, from));
        }
        match panel.map(str::trim).filter(|w| !w.is_empty()) {
            Some(id) => {
                let p = self.panels.get(id).ok_or_else(|| format!("no panel `{id}`"))?;
                let c = p.channel().unwrap_or_else(|| self.panels.home());
                Ok((c, Some(p)))
            }
            None => Ok((self.panels.home(), None)),
        }
    }

    /// What one channel is showing, seen from `from` when a request named a
    /// panel on it.
    #[must_use]
    pub fn state_of(&self, channel: &Arc<Channel>, from: Option<&Arc<Panel>>) -> StudioState {
        self.panels.state_of(channel, from)
    }

    /// What Channel 1 is showing.
    #[must_use]
    pub fn page_state(&self) -> StudioState {
        self.state_of(&self.panels.home(), None)
    }

    /// Every panel, now.
    #[must_use]
    pub fn summaries(&self) -> Vec<PanelSummary> {
        self.panels.summaries(&self.devices)
    }

    /// Every panel and every channel, now.
    #[must_use]
    pub fn overview(&self) -> Overview {
        Overview { panels: self.summaries(), channels: self.panels.channel_summaries() }
    }

    /// Put the overview in its cell, if it has changed.
    pub fn refresh_overview(&self) {
        let now = self.overview();
        self.overview.send_if_modified(|was| {
            if **was == now {
                return false;
            }
            *was = Arc::new(now);
            true
        });
    }

    /// Write everything worth keeping to the state file.
    ///
    /// Called after every change. It never blocks: the store's writer thread
    /// has a one-slot mailbox and the newest state wins, so a slider being
    /// dragged costs one write rather than sixty.
    pub fn persist(&self) {
        let (panels, channels) = self.panels.stored();
        self.store.save(state::Persisted {
            version: state::SCHEMA_VERSION,
            devices: self.devices.stored(),
            panels,
            channels,
            output: self.panels.output(),
            players: Vec::new(),
            focus: String::new(),
            // Card 151's named settings. They live here and nowhere else: the
            // state file in `SCREENY_STATE_DIR` is the volume the container
            // keeps across a rebuild.
            patches: self.memory.snapshot(),
            home_assistant: Some(self.ha.settings()).filter(|s| *s != ha::HaSettings::default()),
        });
    }

    /// Stamp a change, with who made it.
    #[must_use]
    pub fn stamp(&self, from: Option<String>) -> Changed {
        Changed { rev: self.rev.fetch_add(1, Ordering::Relaxed) + 1, from }
    }

    /// **Something changed**: every socket sends its own panel's state, and
    /// the overview is looked at again. `from` is the browser that made the
    /// change, which is not echoed its own.
    pub fn changed(&self, from: Option<String>) {
        // `send` fails only when nobody is listening, which is the normal case
        // for a server with no browser open.
        let _ = self.states.send(self.stamp(from));
        self.refresh_overview();
    }
}

/// A studio that is listening but not yet serving.
pub struct Studio {
    /// What it actually bound to. With port 0 in the config, this is the port
    /// the OS chose - which is how the tests get an ephemeral server.
    pub addr: SocketAddr,
    /// Card 359: where the unchecked `direct` listener bound, if there is one.
    pub direct_addr: Option<SocketAddr>,
    listener: TcpListener,
    state: AppState,
    stop: watch::Sender<bool>,
    /// Card 308, when there is a broker to talk to.
    ha: ha::client::Handle,
    direct: Option<TcpListener>,
}

/// Stops the studio when dropped: every channel ends, every panel link sends
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
        // One library of named settings for the whole studio.
        let memory = SharedMemory::new(saved.patches);
        let panels = Arc::new(Panels::new(memory, cfg.fault_patches));
        panels.set_output(saved.output);
        panels.load(saved.panels, saved.channels);
        // Every device the registry knows is a panel - on Channel 1 if the
        // file did not already have it (card 353).
        for id in devices.ids() {
            panels.adopt(&id);
        }

        let (stop, _) = watch::channel(false);
        let cfg = Arc::new(cfg);
        let state = AppState::new(
            ui::Ui { dir: cfg.ui_dir.clone() },
            stop.subscribe(),
            Arc::clone(&cfg),
            store,
            devices,
            Arc::clone(&panels),
            // Card 311: the state file's, else the config's (tests), else off.
            Arc::new(ha::Ha::new(
                saved
                    .home_assistant
                    .or_else(|| cfg.mqtt.as_ref().map(ha::HaSettings::connecting_as))
                    .unwrap_or_default(),
            )),
        );
        let listener = TcpListener::bind(cfg.listen).await?;
        let addr = listener.local_addr()?;
        let direct = match cfg.direct {
            Some(a) => Some(TcpListener::bind(a).await?),
            None => None,
        };
        let direct_addr = direct.as_ref().and_then(|l| l.local_addr().ok());

        // Start rendering at once rather than on the supervisor's first tick,
        // so a browser that opens immediately is not shown a black panel.
        panels.start();
        state.refresh_overview();

        spawn_status(state.clone());
        fleet::spawn_supervisor(state.clone());
        fleet::spawn_discovery(state.clone());
        fleet::spawn_telemetry(state.clone());
        fleet::spawn_device_http(state.clone());
        let ha = ha::bridge::start(&state);
        if let Some(sup) = cfg.supervisor.clone() {
            ha::supervisor::spawn(&state, sup, ha::supervisor::RECHECK, ha::supervisor::RETRY);
        }

        Ok(Studio { addr, direct_addr, listener, state, stop, ha, direct })
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
        let ha = self.ha;
        // Card 359: in app mode the main listener serves only the peers the
        // plan names; a plain deployment installs no check at all.
        let mut app = router(self.state.clone());
        if let Some(peers) = self.state.cfg.peers.clone() {
            app = app.layer(axum::middleware::from_fn_with_state(Arc::new(peers), app::only_peers));
        }
        let direct = self.direct.map(|l| {
            let mut stop = self.stop.subscribe();
            let app = router(self.state.clone());
            tokio::spawn(async move {
                let _ = axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>())
                    .with_graceful_shutdown(async move {
                        let _ = stop.wait_for(|s| *s).await;
                    })
                    .await;
            })
        });
        axum::serve(self.listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move {
                let _ = stopped.wait_for(|s| *s).await;
            })
            .await?;
        if let Some(d) = direct {
            let _ = d.await;
        }
        // Let every panel go at once rather than after its stream timeout, and
        // make sure the last thing that changed is on disk before we go.
        st.panels.shutdown();
        st.persist();
        st.store.flush();
        // Say `offline` on the way out, rather than leaving it to the broker
        // to notice in a keep-alive or two.
        ha.finish(ha::client::GOODBYE).await;
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
/// Since card 170 this cannot be held up by a wedged patch. A channel's core is
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
                    let board = StatusBoard {
                        channels: st.panels.channels().iter().map(|c| (c.id(), c.playing())).collect(),
                        panels: st.panels.all().iter().map(|p| (p.device(), p.link_status())).collect(),
                    };
                    st.status.send_replace(Arc::new(board));
                    // Links come and go without a state change; the overview
                    // says so within a heartbeat.
                    st.refresh_overview();
                }
                // Discarded inside the block: the borrow `wait_for` hands back
                // is not `Send` and this future has to be.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}
