//! `GET /healthz` and `GET /api/v1/status`: is the server well, and what is
//! each panel doing?
//!
//! # What 503 means
//!
//! **`/healthz` is about the server, not about the panels.** A panel that is
//! unplugged, switched off, rebooting or on the wrong side of a dead access
//! point is normal life for a thing that runs for months, and restarting the
//! container is never the right answer to it. So a missing panel, a link that
//! is `connecting`, a device that has not been heard from and an empty device
//! list are all **200**.
//!
//! 503 is reserved for the three ways the *process* can be broken, each of
//! which a restart genuinely does fix:
//!
//! 1. **The state file cannot be written.** A studio that cannot save will not
//!    come back as itself, which is the whole promise of card 106.
//! 2. **A player has given up**: [`crate::player::MAX_FAULTS`] panics or stalls
//!    in a row, so it is no longer trying. A restart loop is worse than a
//!    stopped player, but a stopped player is not healthy either.
//! 3. **A player is not running**, and has not been for longer than
//!    [`crate::START_GRACE`] - a render thread that died and was not replaced.
//!
//! A **missing graphics adapter is not one of them** (card 145). A studio with
//! no GPU plays every CPU piece perfectly well, no restart conjures an adapter,
//! and the answer a person needs is a sentence on the page rather than a 503 at
//! three in the morning. It is reported on `/api/v1/status` as `gpu` and
//! nowhere else.
//!
//! Card 106 had a fourth - "the preview engine is wedged or dead" - and card
//! 170 deleted the thing it was about. There is one engine now, the player for
//! the attached panel, and a player that wedges is *recovered* by its own
//! watchdog within [`WATCHDOG`] rather than waiting for somebody to restart
//! the container.
//!
//! The body says which, in words, so a `docker logs` after a restart is not
//! the only evidence.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use screeny_art::output::PanelStatus;
use serde::Serialize;

use crate::devices::{DeviceFacts, DiscoveryHealth, HttpHealth, Telem};

use crate::player::{PlayerStatus, WATCHDOG};
use crate::state::{unix_now, StoreHealth};
use crate::{AppState, START_GRACE};

/// Everything `/api/v1/status` answers.
#[derive(Serialize)]
pub struct Status {
    /// The same judgement `/healthz` makes.
    pub ok: bool,
    /// Why not, in words. Empty when `ok`.
    pub problems: Vec<String>,
    pub uptime_s: f64,
    pub version: &'static str,
    pub state: StoreHealth,
    pub discovery: DiscoveryHealth,
    /// Card 145: the graphics adapter, or why there is none. **Never a
    /// problem**: a studio with no GPU plays the CPU pieces perfectly well,
    /// and a restart does not conjure an adapter. It is here so that a black
    /// `overland` has a reason a person can read.
    pub gpu: screeny_art::GpuStatus,
    /// What the page is showing - which, since card 170, is what the attached
    /// panel is showing. The key is still `preview` so that scripts written
    /// against card 106's shape keep working.
    pub preview: PreviewStatus,
    /// Card 120: what the preview sockets are costing. A studio meant to be
    /// forgotten in a container should be able to say how much of its traffic
    /// is browsers, without anybody having to read `docker stats`.
    pub sockets: crate::page::PreviewCost,
    /// One entry per device, whether or not it has a player or is reachable.
    pub devices: Vec<DeviceStatus>,
}

/// The player the page is a window onto.
///
/// Card 106 called this the preview engine and read it from a cached view,
/// because a wedged piece held the engine's lock and this route had to answer
/// anyway. It is read straight from the player now: a player's core is owned
/// by its own render thread and is behind no shared lock, so there is nothing
/// left that a wedged piece could hold.
#[derive(Serialize)]
pub struct PreviewStatus {
    pub piece: String,
    pub seed: u32,
    pub fps: f64,
    pub paused: bool,
    pub ticks: u64,
    pub panics: u64,
    /// True while its render thread is alive and ticking.
    pub alive: bool,
    pub last_tick_ago: f64,
    /// True when it has not produced a frame for longer than the watchdog. Its
    /// own supervisor is already replacing it when this is true.
    pub wedged: bool,
    pub gave_up: Option<String>,
    pub fell_back_from: Option<String>,
    /// Panel output: whether the attached panel is being driven.
    pub panel_on: bool,
    /// How the attached panel is named - an address, an mDNS instance name or
    /// its id. Empty when no panel is attached.
    pub panel_to: String,
    /// Which panel this is, by id. Empty when none is attached (card 170).
    pub device: String,
    pub panel: Option<PanelStatus>,
}

/// One device, its player and everything the device itself says.
#[derive(Serialize)]
pub struct DeviceStatus {
    pub id: String,
    /// What to call it: the name set here, else the device's own, else its
    /// instance name, else the id.
    pub label: String,
    pub name: String,
    pub instance: String,
    pub address: String,
    pub manual: bool,
    /// True once the studio knows its exact ports.
    pub resolved: bool,
    pub frame_addr: Option<String>,
    pub control_addr: Option<String>,
    /// Seconds since anything was heard from it. `None` means never.
    pub last_seen_ago: Option<f64>,
    pub firmware: Option<String>,
    pub panel_size: Option<String>,
    /// The device's own telemetry: uptime, RSSI, drops by cause, brightness.
    pub telemetry: Option<Telem>,
    pub telemetry_ago: Option<f64>,
    /// Card 180: what the device's own HTTP API says about itself - heap, free
    /// stack, firmware slot, reset reason, WiFi. `None` on firmware that does
    /// not serve one, which is normal and is why this is additive: everything
    /// above it still comes from UDP.
    ///
    /// **It carries the network's SSID**, which is why it is on the owner's
    /// page and in this reply and nowhere else: never a log line, never the
    /// state file (`CLAUDE.md`).
    pub facts: Option<DeviceFacts>,
    pub facts_ago: Option<f64>,
    /// How reading it is going. Never a reason for a 503.
    pub http: HttpHealth,
    pub last_error: Option<String>,
    /// What it plays, and how that is going. `None` when no player is
    /// configured for this device.
    pub player: Option<PlayerStatus>,
    /// True when this is the panel the page is a window onto.
    pub attached: bool,
}

/// Everything that is wrong with the *server*, in words. Empty is healthy.
#[must_use]
pub fn problems(st: &AppState) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(e) = st.store.health().last_error {
        out.push(format!("the state file cannot be written: {e}"));
    }

    let young = st.started.elapsed() < START_GRACE;
    for p in st.players.all() {
        let s = p.status();
        let who = if s.device.is_empty() { "the page".to_string() } else { format!("player `{}`", s.device) };
        if let Some(why) = &s.health.gave_up {
            out.push(format!("{who} has given up: {why}"));
        } else if !s.running && !young {
            out.push(format!("{who} should be playing `{}` and is not running", s.piece));
        } else if s.health.last_tick_ago.is_some_and(|a| a > WATCHDOG.as_secs_f64() * 2.0) && !young {
            out.push(format!("{who} has not produced a frame for {:.0} s", s.health.last_tick_ago.unwrap_or(0.0)));
        }
    }
    out
}

/// The container healthcheck. 200 `ok`, or 503 and the reasons.
pub async fn healthz(State(st): State<AppState>) -> impl IntoResponse {
    let problems = problems(&st);
    if problems.is_empty() {
        (StatusCode::OK, "ok\n".to_string())
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, format!("unhealthy\n{}\n", problems.join("\n")))
    }
}

/// The whole picture, as the page reads it every couple of seconds.
pub async fn status(State(st): State<AppState>) -> Json<Status> {
    Json(collect(&st))
}

/// The same thing, for a test that would rather not parse JSON.
#[must_use]
pub fn collect(st: &AppState) -> Status {
    let problems = problems(st);
    let page = st.page().status();
    let now = unix_now();

    let attached = page.device.clone();
    let panel_to = st
        .devices
        .get(&attached)
        .map(|d| {
            if !d.stored.address.is_empty() {
                d.stored.address.clone()
            } else if !d.stored.instance.is_empty() {
                d.stored.instance.clone()
            } else {
                d.stored.id.clone()
            }
        })
        .unwrap_or_default();

    let preview = PreviewStatus {
        piece: page.piece.clone(),
        seed: page.seed,
        fps: page.fps,
        paused: page.paused,
        ticks: page.health.ticks,
        panics: page.health.panics,
        alive: page.running,
        last_tick_ago: page.health.last_tick_ago.unwrap_or(0.0),
        wedged: page.health.last_tick_ago.is_some_and(|a| a > WATCHDOG.as_secs_f64()),
        gave_up: page.health.gave_up.clone(),
        fell_back_from: page.health.fell_back_from.clone(),
        panel_on: page.on,
        panel_to,
        device: attached.clone(),
        panel: page.panel.clone(),
    };

    let devices = st
        .devices
        .list()
        .into_iter()
        .map(|d| {
            let info = d.resolved.as_ref().and_then(|r| r.info.as_ref());
            DeviceStatus {
                label: d.label(),
                attached: d.stored.id == attached,
                id: d.stored.id.clone(),
                name: d.stored.name.clone(),
                instance: d.stored.instance.clone(),
                address: d.stored.address.clone(),
                manual: d.stored.manual,
                resolved: d.resolved.is_some(),
                frame_addr: d.resolved.as_ref().map(|r| r.frame.to_string()),
                control_addr: d.resolved.as_ref().map(|r| r.control.to_string()),
                last_seen_ago: d.seen_unix.map(|s| now.saturating_sub(s) as f64),
                firmware: info.map(|i| i.fw.clone()),
                panel_size: info.map(|i| format!("{}x{}", i.w, i.h)),
                telemetry_ago: d.telemetry.as_ref().map(|t| now.saturating_sub(t.heard_unix) as f64),
                telemetry: d.telemetry.clone(),
                facts_ago: d.facts.as_ref().map(|f| now.saturating_sub(f.heard_unix) as f64),
                facts: d.facts.clone(),
                http: d.http.clone(),
                last_error: d.last_error.clone(),
                player: st.players.get(&d.stored.id).map(|p| p.status()),
            }
        })
        .collect();

    Status {
        ok: problems.is_empty(),
        problems,
        uptime_s: st.started.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION"),
        state: st.store.health(),
        discovery: st.devices.discovery_health(),
        gpu: screeny_art::gpu_status(),
        preview,
        sockets: st.screen.cost(),
        devices,
    }
}
