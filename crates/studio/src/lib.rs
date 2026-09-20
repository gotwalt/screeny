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

pub mod api;
pub mod engine;
pub mod ui;
pub mod ws;

use axum::routing::any;
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

use crate::api::{StateEvent, StatusEvent};
use crate::engine::{lock, Engine, Shared, StudioState};

/// State changes a browser may be behind by before it is resynced instead.
/// Small on purpose: the newest state is always the right answer, so there is
/// no reason to keep a history.
const STATE_BACKLOG: usize = 32;
/// How often the "now playing" and panel-link heartbeat goes out.
const STATUS_EVERY: Duration = Duration::from_millis(500);

/// How to run: what to listen on, and where the UI comes from.
#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    /// `None` serves the UI compiled into the binary.
    pub ui_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config { listen: SocketAddr::from(([127, 0, 0, 1], 8787)), ui_dir: None }
    }
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
    rev: Arc<AtomicU64>,
}

impl AppState {
    fn new(engine: Shared, ui: ui::Ui, stop: watch::Receiver<bool>) -> Self {
        let packet = lock(&engine).packet();
        AppState {
            frames: watch::Sender::new(packet),
            states: broadcast::Sender::new(STATE_BACKLOG),
            status: watch::Sender::new(Arc::new(StatusEvent { kind: "status", playing: None, panel: None })),
            engine,
            ui: Arc::new(ui),
            stop,
            rev: Arc::new(AtomicU64::new(0)),
        }
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
        let engine: Shared = Arc::new(Mutex::new(Engine::new()));
        let (stop, _) = watch::channel(false);
        let state = AppState::new(engine, ui::Ui { dir: cfg.ui_dir.clone() }, stop.subscribe());
        let listener = TcpListener::bind(cfg.listen).await?;
        let addr = listener.local_addr()?;

        spawn_engine(state.clone());
        spawn_status(state.clone());

        Ok(Studio { addr, listener, state, stop })
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
        let engine = self.state.engine.clone();
        let app = router(self.state);
        axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                let _ = stopped.wait_for(|s| *s).await;
            })
            .await?;
        // Let the panel go at once rather than after its stream timeout.
        lock(&engine).close_panel();
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
        .route("/api/v1/ws", any(ws::upgrade))
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

/// The clock. A plain OS thread, not a tokio task: a frame is several
/// milliseconds of arithmetic and has no business on a runtime worker.
fn spawn_engine(st: AppState) {
    let stop = st.stop.clone();
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let mut next = std::time::Instant::now();
            while !*stop.borrow() {
                // A panicking piece reports itself on stderr; keep the clock running.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock(&st.engine).tick()));
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
