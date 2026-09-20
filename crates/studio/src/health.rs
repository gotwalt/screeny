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
//! 503 is reserved for the four ways the *process* can be broken, each of
//! which a restart genuinely does fix:
//!
//! 1. **The state file cannot be written.** A studio that cannot save will not
//!    come back as itself, which is the whole promise of the card.
//! 2. **A player has given up**: [`crate::player::MAX_FAULTS`] panics or stalls
//!    in a row, so it is no longer trying. A restart loop is worse than a
//!    stopped player, but a stopped player is not healthy either.
//! 3. **A player that should be running is not**, and has not been for longer
//!    than [`crate::START_GRACE`] - a render thread that died and was not
//!    replaced.
//! 4. **The preview engine is wedged or dead**: no frame for longer than the
//!    watchdog, or its thread is gone.
//!
//! The body says which, in words, so a `docker logs` after a restart is not
//! the only evidence.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use screeny_art::output::PanelStatus;
use serde::Serialize;
use std::sync::atomic::Ordering;

use crate::devices::{DiscoveryHealth, Telem};

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
    pub preview: PreviewStatus,
    /// One entry per device, whether or not it has a player or is reachable.
    pub devices: Vec<DeviceStatus>,
}

/// The design view's player.
///
/// Read from the last view the status heartbeat managed to take, never from
/// the engine itself: this route has to answer when the engine is wedged,
/// because that is when somebody is looking.
#[derive(Serialize)]
pub struct PreviewStatus {
    pub piece: String,
    pub seed: u32,
    pub fps: f64,
    pub paused: bool,
    pub ticks: u64,
    pub panics: u64,
    pub alive: bool,
    pub last_tick_ago: f64,
    /// True when the engine has not produced a frame for longer than the
    /// watchdog: a piece that has stopped returning.
    pub wedged: bool,
    pub gave_up: Option<String>,
    pub fell_back_from: Option<String>,
    /// Whether the preview is being sent to a panel, and to which.
    pub panel_on: bool,
    pub panel_to: String,
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
    pub last_error: Option<String>,
    /// What it plays, and how that is going. `None` when no player is
    /// configured for this device.
    pub player: Option<PlayerStatus>,
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
        if let Some(why) = &s.health.gave_up {
            out.push(format!("player `{}` has given up: {why}", s.device));
        } else if s.on && !s.running && !young {
            out.push(format!("player `{}` should be playing `{}` and is not running", s.device, s.piece));
        } else if s.on && s.health.last_tick_ago.is_some_and(|a| a > WATCHDOG.as_secs_f64() * 2.0) {
            out.push(format!("player `{}` has not produced a frame for {:.0} s", s.device, s.health.last_tick_ago.unwrap_or(0.0)));
        }
    }

    if let Ok(g) = st.preview.gave_up.lock() {
        if let Some(why) = g.as_ref() {
            out.push(format!("the preview has given up: {why}"));
        }
    }
    if !st.preview.alive.load(Ordering::Relaxed) && !young {
        out.push("the preview engine thread is gone".into());
    } else if !young {
        let ago = (crate::player::unix_millis().saturating_sub(st.preview.beat.load(Ordering::Relaxed))) as f64 / 1000.0;
        if ago > WATCHDOG.as_secs_f64() * 2.0 {
            out.push(format!("the preview engine has not produced a frame for {ago:.0} s"));
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

/// The whole picture, as the dashboard reads it twice a second.
pub async fn status(State(st): State<AppState>) -> Json<Status> {
    Json(collect(&st))
}

/// The same thing, for a test that would rather not parse JSON.
#[must_use]
pub fn collect(st: &AppState) -> Status {
    let problems = problems(st);
    let seen = st.preview.seen.lock().ok().and_then(|s| s.clone());
    let last_tick_ago = (crate::player::unix_millis().saturating_sub(st.preview.beat.load(Ordering::Relaxed))) as f64 / 1000.0;
    let preview = PreviewStatus {
        piece: seen.as_ref().map_or_else(String::new, |v| v.state.piece.to_string()),
        seed: seen.as_ref().map_or(0, |v| v.state.seed),
        fps: seen.as_ref().map_or(0.0, |v| v.state.fps),
        paused: seen.as_ref().is_some_and(|v| v.state.paused),
        ticks: st.preview.ticks.load(Ordering::Relaxed),
        panics: st.preview.panics.load(Ordering::Relaxed),
        alive: st.preview.alive.load(Ordering::Relaxed),
        last_tick_ago,
        wedged: last_tick_ago > WATCHDOG.as_secs_f64(),
        gave_up: st.preview.gave_up.lock().ok().and_then(|g| g.clone()),
        fell_back_from: st.preview.fell_back_from.lock().ok().and_then(|g| g.clone()),
        panel_on: seen.as_ref().is_some_and(|v| v.panel_on),
        panel_to: seen.as_ref().map_or_else(String::new, |v| v.panel_to.clone()),
        panel: seen.and_then(|v| v.panel),
    };

    let now = unix_now();
    let devices = st
        .devices
        .list()
        .into_iter()
        .map(|d| {
            let info = d.resolved.as_ref().and_then(|r| r.info.as_ref());
            DeviceStatus {
                label: d.label(),
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
        preview,
        devices,
    }
}
