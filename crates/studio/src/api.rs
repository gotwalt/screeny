//! The JSON API: the thirteen things the UI can ask the engine to do.
//!
//! One route per command, the names unchanged from the desktop app's IPC, so
//! `invoke('set_piece', {id})` became `POST /api/v1/set_piece {"id": ...}` and
//! nothing else had to move. Reads are `GET`, changes are `POST`.
//!
//! Every change is published to [`crate::AppState::states`] so that the other
//! browsers watching stay in step; the sender's own socket is skipped, which
//! is what the `X-Studio-Client` header is for.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use screeny_art::output::PanelStatus;
use screeny_art::piece::Playing;
use screeny_art::Settings;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::engine::{self, lock, Bootstrap, StudioState};
use crate::AppState;

/// The header a browser tags its own changes with, so the state it just made
/// is not echoed back to it over the WebSocket while a slider is moving.
pub const CLIENT_HEADER: &str = "x-studio-client";

/// A state change, as the WebSocket delivers it.
#[derive(Clone, Serialize)]
pub struct StateEvent {
    /// Always `state`; the UI switches on it.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Increases by one per change. A client that sees a gap has missed one
    /// and should trust the state it is holding, which is the newest anyway.
    pub rev: u64,
    /// Which browser made the change, if it said. `None` means "everyone
    /// should adopt this", which is what a resync sends.
    pub from: Option<String>,
    pub state: StudioState,
}

/// The half-second heartbeat: what the piece is performing and what the panel
/// link is doing. Pushed rather than polled, so N browsers cost one poll.
#[derive(Clone, Serialize)]
pub struct StatusEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub playing: Option<Playing>,
    pub panel: Option<PanelStatus>,
}

/// An error the UI can put on the notice line.
pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad_request(message: String) -> Self {
        ApiError(StatusCode::BAD_REQUEST, message)
    }

    /// No device with that id. The dashboard's list is stale; reload it.
    fn not_found(message: String) -> Self {
        ApiError(StatusCode::NOT_FOUND, message)
    }

    /// The device is known but cannot be reached right now, which is a normal
    /// state for a panel and not an error in the server.
    fn unreachable(message: String) -> Self {
        ApiError(StatusCode::CONFLICT, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/bootstrap", get(bootstrap))
        .route("/frame", get(frame))
        .route("/piece_playing", get(piece_playing))
        .route("/panel_status", get(panel_status))
        .route("/set_piece", post(set_piece))
        .route("/set_param", post(set_param))
        .route("/reset_params", post(reset_params))
        .route("/set_seed", post(set_seed))
        .route("/set_settings", post(set_settings))
        .route("/set_playback", post(set_playback))
        .route("/piece_act", post(piece_act))
        .route("/restart", post(restart))
        .route("/set_panel", post(set_panel))
        // ---- card 106: devices, players and health ----
        .route("/status", get(crate::health::status))
        .route("/devices", get(devices_list))
        .route("/devices/add", post(devices_add))
        .route("/devices/forget", post(devices_forget))
        .route("/devices/refresh", post(devices_refresh))
        .route("/player/set", post(player_set))
        .route("/player/adopt_preview", post(player_adopt_preview))
        .route("/device/brightness", post(device_brightness))
        .route("/device/identify", post(device_identify))
        .route("/device/name", post(device_name))
        .route("/device/reboot", post(device_reboot))
        .route("/device/stats", post(device_stats))
}

/// Publish the engine's new state to every browser but the one that caused it,
/// and write it down: since card 106 the design view survives a restart too.
fn publish(st: &AppState, headers: &HeaderMap) -> StudioState {
    let state = lock(&st.engine).snapshot();
    let from = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    st.publish_state(from, state.clone());
    st.persist();
    state
}

// ---------------- reads ----------------

async fn bootstrap(State(st): State<AppState>) -> Json<Bootstrap> {
    Json(engine::bootstrap(&st.engine))
}

/// The newest frame packet, for a client that would rather poll than open a
/// WebSocket (and for tests, which then need no WebSocket at all).
async fn frame(State(st): State<AppState>) -> impl IntoResponse {
    let packet: Arc<Vec<u8>> = lock(&st.engine).packet();
    ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], packet.to_vec())
}

async fn piece_playing(State(st): State<AppState>) -> Json<Option<Playing>> {
    Json(lock(&st.engine).playing())
}

async fn panel_status(State(st): State<AppState>) -> Json<Option<PanelStatus>> {
    Json(lock(&st.engine).panel_status())
}

// ---------------- changes ----------------

#[derive(Deserialize)]
struct SetPiece {
    id: String,
}

async fn set_piece(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPiece>) -> ApiResult<Json<StudioState>> {
    lock(&st.engine).set_piece(&req.id).map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers)))
}

#[derive(Deserialize)]
struct SetParam {
    id: String,
    value: f32,
}

async fn set_param(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetParam>) -> ApiResult<Json<StudioState>> {
    lock(&st.engine).set_param(&req.id, req.value).map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers)))
}

/// Takes no arguments - but reads the body anyway, because the UI sends `{}`
/// and a server that closes a connection with a request body still unread
/// gets a TCP reset rather than a clean close.
async fn reset_params(State(st): State<AppState>, headers: HeaderMap, _body: axum::body::Bytes) -> Json<StudioState> {
    lock(&st.engine).reset_params();
    Json(publish(&st, &headers))
}

#[derive(Deserialize)]
struct SetSeed {
    /// Absent or null picks a new one.
    #[serde(default)]
    seed: Option<u32>,
}

async fn set_seed(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetSeed>) -> Json<StudioState> {
    lock(&st.engine).set_seed(req.seed);
    Json(publish(&st, &headers))
}

#[derive(Deserialize)]
struct SetSettings {
    settings: Settings,
}

async fn set_settings(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetSettings>) -> Json<StudioState> {
    lock(&st.engine).set_settings(req.settings);
    Json(publish(&st, &headers))
}

#[derive(Deserialize)]
struct SetPlayback {
    paused: bool,
    speed: f64,
    fps: f64,
}

async fn set_playback(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPlayback>) -> Json<StudioState> {
    lock(&st.engine).set_playback(req.paused, req.speed, req.fps);
    Json(publish(&st, &headers))
}

#[derive(Deserialize)]
struct PieceAct {
    action: String,
}

async fn piece_act(State(st): State<AppState>, Json(req): Json<PieceAct>) -> Json<Option<Playing>> {
    // A piece's own action changes the piece, not the studio's state, so
    // there is nothing to publish: the half-second status push carries it.
    Json(lock(&st.engine).act(&req.action))
}

/// Takes no arguments; reads the body for the reason `reset_params` does.
async fn restart(State(st): State<AppState>, headers: HeaderMap, _body: axum::body::Bytes) -> Json<StudioState> {
    lock(&st.engine).restart();
    Json(publish(&st, &headers))
}

#[derive(Deserialize)]
struct SetPanel {
    on: bool,
    #[serde(default)]
    to: String,
}

/// Point the *preview* at a panel, or take it off one.
///
/// `to` may be a device id from the registry as well as a name or an address -
/// the dashboard has ids and a human has names. A registry id is turned into
/// the best way of reaching that device: its instance name when there is one,
/// so the link follows a DHCP lease, and its address otherwise.
async fn set_panel(State(st): State<AppState>, Json(req): Json<SetPanel>) -> Json<Option<PanelStatus>> {
    let to = resolve_aim(&st, &req.to);
    let out = lock(&st.engine).set_panel(req.on, &to);
    // Which panel the design view is pointed at is state now, not a secret in
    // one browser's localStorage (card 105's handover note).
    st.persist();
    Json(out)
}

/// Turn "whatever the human meant" into something `target_for` understands.
fn resolve_aim(st: &AppState, to: &str) -> String {
    let to = to.trim();
    match st.devices.get(to) {
        Some(d) => {
            if !d.stored.instance.is_empty() {
                d.stored.instance
            } else if !d.stored.address.is_empty() {
                d.stored.address
            } else {
                d.resolved.map_or_else(String::new, |r| r.frame.to_string())
            }
        }
        None => to.to_string(),
    }
}

// ------------------ card 106: devices, players and health ------------------

/// The device list on its own, for a dashboard that only wants that much.
async fn devices_list(State(st): State<AppState>) -> Json<Vec<crate::health::DeviceStatus>> {
    Json(crate::health::collect(&st).devices)
}

#[derive(Deserialize)]
struct AddDevice {
    /// A name (`screeny-4a00a4`) or an address (`192.168.7.221`, `127.0.0.1:49374`).
    to: String,
    #[serde(default)]
    name: String,
    /// Start playing on it straight away. The dashboard's "add and play".
    #[serde(default)]
    play: bool,
}

async fn devices_add(State(st): State<AppState>, Json(req): Json<AddDevice>) -> ApiResult<Json<serde_json::Value>> {
    let id = st.devices.add_manual(&req.to, &req.name).map_err(ApiError::bad_request)?;
    if req.play {
        st.players.ensure(&id, st.cfg.fault_pieces, crate::state::StoredPlayer::default);
    }
    st.persist();
    Ok(Json(serde_json::json!({ "id": id })))
}

#[derive(Deserialize)]
struct DeviceRef {
    device: String,
}

async fn devices_forget(State(st): State<AppState>, Json(req): Json<DeviceRef>) -> ApiResult<Json<serde_json::Value>> {
    if !st.devices.forget(&req.device) {
        return Err(ApiError::not_found(format!("no device `{}`", req.device)));
    }
    st.players.remove(&req.device);
    st.persist();
    Ok(Json(serde_json::json!({ "forgotten": req.device })))
}

/// Ask the unresolved devices who they are, now rather than on the next pass.
async fn devices_refresh(State(st): State<AppState>, _body: axum::body::Bytes) -> Json<Vec<crate::health::DeviceStatus>> {
    for d in st.devices.list() {
        if d.resolved.is_some() {
            continue;
        }
        let Some(addr) = crate::devices::parse_addr(&d.stored.address) else { continue };
        let id = d.stored.id.clone();
        match tokio::task::spawn_blocking(move || crate::devices::identify_at(addr)).await {
            Ok(Ok(dev)) => {
                let (new_id, renamed) = st.devices.resolved(&dev);
                if let Some(from) = renamed {
                    st.players.rekey(&from, &new_id);
                }
            }
            Ok(Err(e)) => st.devices.control_failed(&id, e),
            Err(e) => st.devices.control_failed(&id, e.to_string()),
        }
    }
    st.persist();
    Json(crate::health::collect(&st).devices)
}

#[derive(Deserialize)]
struct SetPlayer {
    device: String,
    #[serde(default)]
    on: Option<bool>,
    #[serde(default)]
    piece: Option<String>,
    #[serde(default)]
    seed: Option<u32>,
    #[serde(default)]
    param: Option<ParamChange>,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    settings: Option<Settings>,
    /// Absent leaves the policy alone; `null` clears it; a number sets it.
    #[serde(default, deserialize_with = "double_option")]
    brightness: Option<Option<u8>>,
}

#[derive(Deserialize)]
struct ParamChange {
    id: String,
    value: f32,
}

/// Distinguish "the field was absent" from "the field was null".
fn double_option<'de, D>(d: D) -> Result<Option<Option<u8>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u8>::deserialize(d).map(Some)
}

async fn player_set(State(st): State<AppState>, Json(req): Json<SetPlayer>) -> ApiResult<Json<crate::player::PlayerStatus>> {
    if st.devices.get(&req.device).is_none() {
        return Err(ApiError::not_found(format!("no device `{}`", req.device)));
    }
    let player = st.players.ensure(&req.device, st.cfg.fault_pieces, crate::state::StoredPlayer::default);
    player
        .configure(&crate::player::PlayerChange {
            on: req.on,
            piece: req.piece,
            seed: req.seed,
            param: req.param.map(|p| (p.id, p.value)),
            fps: req.fps,
            settings: req.settings,
            brightness: req.brightness,
            replace: None,
        })
        .map_err(ApiError::bad_request)?;
    if let Some(record) = st.devices.get(&req.device) {
        player.aim(&record.reach());
    }
    player.ensure_running();
    st.persist();
    Ok(Json(player.status()))
}

/// "Make what I am previewing what panel X plays."
///
/// The explicit promotion the vision asks for: the design view is for playing
/// with, and nothing it does reaches a panel's player until this is called.
async fn player_adopt_preview(State(st): State<AppState>, Json(req): Json<DeviceRef>) -> ApiResult<Json<crate::player::PlayerStatus>> {
    if st.devices.get(&req.device).is_none() {
        return Err(ApiError::not_found(format!("no device `{}`", req.device)));
    }
    let snap = lock(&st.engine).snapshot();
    let player = st.players.ensure(&req.device, st.cfg.fault_pieces, crate::state::StoredPlayer::default);
    player
        .configure(&crate::player::PlayerChange {
            on: Some(true),
            replace: Some(crate::player::Adopt {
                piece: snap.piece.to_string(),
                seed: snap.seed,
                params: snap.params.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
                settings: snap.settings,
                fps: snap.fps,
            }),
            ..crate::player::PlayerChange::default()
        })
        .map_err(ApiError::bad_request)?;
    if let Some(record) = st.devices.get(&req.device) {
        player.aim(&record.reach());
    }
    player.ensure_running();
    st.persist();
    Ok(Json(player.status()))
}

// ---- the control port, proxied: brightness, identify, name, stats, reboot ----

/// The control address of a device, or the right refusal.
fn control_addr(st: &AppState, device: &str) -> ApiResult<std::net::SocketAddr> {
    let record = st.devices.get(device).ok_or_else(|| ApiError::not_found(format!("no device `{device}`")))?;
    record
        .control_addr()
        .ok_or_else(|| ApiError::unreachable(format!("`{}` has not been reached yet, so there is no control port to talk to", record.label())))
}

/// Run one control request on a blocking thread, and turn a silent device into
/// a 409 rather than a 500: a panel that is off is not a server fault.
async fn on_device<T, F>(st: &AppState, device: &str, what: &'static str, f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce(&mut screeny::ControlClient) -> Result<T, String> + Send + 'static,
{
    let addr = control_addr(st, device)?;
    let done = tokio::task::spawn_blocking(move || crate::devices::control(addr).and_then(|mut c| f(&mut c))).await;
    match done {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            st.devices.control_failed(device, format!("{what}: {e}"));
            Err(ApiError::unreachable(format!("{what}: {e}")))
        }
        Err(e) => Err(ApiError::unreachable(format!("{what}: {e}"))),
    }
}

#[derive(Deserialize)]
struct SetBrightness {
    device: String,
    level: u8,
}

/// Set brightness now, and remember it as the policy so a reconnect re-applies
/// it. The answer is what the device *applied*, which its own cap may make
/// lower than what was asked for.
async fn device_brightness(State(st): State<AppState>, Json(req): Json<SetBrightness>) -> ApiResult<Json<serde_json::Value>> {
    let level = req.level;
    let applied = on_device(&st, &req.device, "setting brightness", move |c| c.set_brightness(level).map_err(|e| e.to_string())).await?;
    if let Some(p) = st.players.get(&req.device) {
        p.configure(&crate::player::PlayerChange { brightness: Some(Some(level)), ..crate::player::PlayerChange::default() })
            .map_err(ApiError::bad_request)?;
        p.brightness_applied(level, applied);
    }
    st.persist();
    Ok(Json(serde_json::json!({ "asked": level, "applied": applied })))
}

#[derive(Deserialize)]
struct Identify {
    device: String,
    #[serde(default = "default_identify_ms")]
    ms: u16,
}

fn default_identify_ms() -> u16 {
    3000
}

async fn device_identify(State(st): State<AppState>, Json(req): Json<Identify>) -> ApiResult<Json<serde_json::Value>> {
    let ms = req.ms.min(10_000);
    on_device(&st, &req.device, "identifying", move |c| c.identify(ms).map_err(|e| e.to_string())).await?;
    Ok(Json(serde_json::json!({ "identifying_ms": ms })))
}

#[derive(Deserialize)]
struct SetName {
    device: String,
    name: String,
}

/// Rename a device: on the device itself when it can be reached, and here
/// either way. The studio's own label is what the dashboard shows, so renaming
/// a panel that is currently off still works.
async fn device_name(State(st): State<AppState>, Json(req): Json<SetName>) -> ApiResult<Json<serde_json::Value>> {
    if !st.devices.rename(&req.device, &req.name) {
        return Err(ApiError::not_found(format!("no device `{}`", req.device)));
    }
    st.persist();
    let name = req.name.trim().to_string();
    let on_the_device = if name.is_empty() {
        Ok(())
    } else {
        on_device(&st, &req.device, "renaming", move |c| c.set_name(&name).map_err(|e| e.to_string())).await
    };
    Ok(Json(serde_json::json!({
        "name": req.name.trim(),
        "on_the_device": on_the_device.is_ok(),
    })))
}

#[derive(Deserialize)]
struct Reboot {
    device: String,
    /// Required, and must be true. The UI asks first; this is the second lock.
    #[serde(default)]
    confirm: bool,
}

async fn device_reboot(State(st): State<AppState>, Json(req): Json<Reboot>) -> ApiResult<Json<serde_json::Value>> {
    if !req.confirm {
        return Err(ApiError::bad_request("rebooting a panel needs `confirm: true`".into()));
    }
    on_device(&st, &req.device, "rebooting", |c| c.reboot().map_err(|e| e.to_string())).await?;
    Ok(Json(serde_json::json!({ "rebooting": req.device })))
}

/// Telemetry, read now rather than from the five-second poll.
async fn device_stats(State(st): State<AppState>, Json(req): Json<DeviceRef>) -> ApiResult<Json<crate::devices::Telem>> {
    let t = on_device(&st, &req.device, "asking for telemetry", |c| c.telemetry().map_err(|e| e.to_string())).await?;
    st.devices.heard(&req.device, &t);
    Ok(Json(crate::devices::Telem::of(&t)))
}
