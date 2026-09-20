//! The JSON API: everything the page can ask the studio to do.
//!
//! One route per command, the names unchanged since the desktop app's IPC, so
//! `invoke('set_piece', {id})` became `POST /api/v1/set_piece {"id": ...}` and
//! nothing else had to move. Reads are `GET`, changes are `POST`.
//!
//! **Card 170 changed what these act on, not what they are.** There is no
//! design-view engine any more: `set_piece`, `set_param`, `set_seed`,
//! `set_settings`, `set_playback`, `piece_act` and `restart` act on the player
//! for the attached panel, which is what the page is a window onto. So a
//! script that spoke to the design view still works, and now it changes the
//! panel - which is the whole point of the card.
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

use crate::page::{self, Bootstrap, StudioState, RATES};
use crate::player::{Player, PlayerChange};
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
/// link is doing. Pushed rather than polled, so N browsers cost one read.
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

    /// No device with that id. The page's list is stale; reload it.
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
        .route("/device/brightness", post(device_brightness))
        .route("/device/identify", post(device_identify))
        .route("/device/name", post(device_name))
        .route("/device/reboot", post(device_reboot))
        .route("/device/stats", post(device_stats))
}

/// Apply a change to the player the page is a window onto, tell the other
/// browsers, and write it down.
fn on_page(st: &AppState, headers: &HeaderMap, change: &PlayerChange) -> ApiResult<Json<StudioState>> {
    let player = st.page();
    player.configure(change).map_err(ApiError::bad_request)?;
    player.ensure_running();
    Ok(Json(publish(st, headers, &player)))
}

/// Publish the page's new state to every browser but the one that caused it,
/// and write it down.
fn publish(st: &AppState, headers: &HeaderMap, player: &Arc<Player>) -> StudioState {
    let state = player.state();
    let from = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    st.publish_state(from, state.clone());
    st.persist();
    state
}

// ---------------- reads ----------------

async fn bootstrap(State(st): State<AppState>) -> Json<Bootstrap> {
    Json(Bootstrap {
        pieces: page::pieces(st.cfg.fault_pieces),
        payload_bytes: screeny_art::meter::PAYLOAD_BYTES,
        state: st.page_state(),
    })
}

/// The newest frame packet, for a client that would rather poll than open a
/// WebSocket (and for tests, which then need no WebSocket at all).
async fn frame(State(st): State<AppState>) -> impl IntoResponse {
    let packet: Arc<Vec<u8>> = st.screen.newest();
    ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], packet.to_vec())
}

async fn piece_playing(State(st): State<AppState>) -> Json<Option<Playing>> {
    Json(st.page().playing())
}

async fn panel_status(State(st): State<AppState>) -> Json<Option<PanelStatus>> {
    Json(st.page().status().panel)
}

// ---------------- changes ----------------

#[derive(Deserialize)]
struct SetPiece {
    id: String,
}

async fn set_piece(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPiece>) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { piece: Some(req.id), ..PlayerChange::default() })
}

#[derive(Deserialize)]
struct SetParam {
    id: String,
    value: f32,
}

async fn set_param(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetParam>) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { param: Some((req.id, req.value)), ..PlayerChange::default() })
}

/// Takes no arguments - but reads the body anyway, because the UI sends `{}`
/// and a server that closes a connection with a request body still unread
/// gets a TCP reset rather than a clean close.
async fn reset_params(State(st): State<AppState>, headers: HeaderMap, _body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { reset_params: true, ..PlayerChange::default() })
}

#[derive(Deserialize)]
struct SetSeed {
    /// Absent or null picks a new one.
    #[serde(default)]
    seed: Option<u32>,
}

async fn set_seed(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetSeed>) -> ApiResult<Json<StudioState>> {
    let seed = req.seed.unwrap_or_else(page::fresh_seed);
    on_page(&st, &headers, &PlayerChange { seed: Some(seed), ..PlayerChange::default() })
}

#[derive(Deserialize)]
struct SetSettings {
    settings: Settings,
}

async fn set_settings(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetSettings>) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { settings: Some(req.settings), ..PlayerChange::default() })
}

#[derive(Deserialize)]
struct SetPlayback {
    paused: bool,
    speed: f64,
    fps: f64,
}

/// The page's rate control offers [`RATES`] and nothing else, so a rate that
/// is not one of them leaves the rate where it is rather than being an error.
/// (`POST /player/set {fps}` takes any rate in the player's range; this is the
/// page's control, and card 105's behaviour is kept exactly.)
async fn set_playback(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPlayback>) -> ApiResult<Json<StudioState>> {
    on_page(
        &st,
        &headers,
        &PlayerChange {
            paused: Some(req.paused),
            speed: Some(req.speed),
            fps: RATES.contains(&req.fps).then_some(req.fps),
            ..PlayerChange::default()
        },
    )
}

#[derive(Deserialize)]
struct PieceAct {
    action: String,
    /// Which panel's piece to act on. Absent means the one on the page, which
    /// is what a browser sends. (Card 140: a panel's composing piece can be
    /// acted on, through a one-slot mailbox its render loop drains - never by
    /// reaching into a running piece from another thread.)
    #[serde(default)]
    device: String,
}

async fn piece_act(State(st): State<AppState>, Json(req): Json<PieceAct>) -> ApiResult<Json<Option<Playing>>> {
    let player = if req.device.is_empty() {
        st.page()
    } else {
        st.players.get(&req.device).ok_or_else(|| ApiError::not_found(format!("no player for `{}`", req.device)))?
    };
    player
        .configure(&PlayerChange { act: Some(req.action), ..PlayerChange::default() })
        .map_err(ApiError::bad_request)?;
    // A piece's own action changes the piece, not the studio's state, so there
    // is nothing to publish: the half-second status push carries it. What is
    // returned is what it was performing *before* the action is drained - the
    // page redraws from the next heartbeat.
    Ok(Json(player.playing()))
}

/// Takes no arguments; reads the body for the reason `reset_params` does.
async fn restart(State(st): State<AppState>, headers: HeaderMap, _body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { restart: true, ..PlayerChange::default() })
}

#[derive(Deserialize)]
struct SetPanel {
    on: bool,
    #[serde(default)]
    to: String,
}

/// What `set_panel` answers: what happened, in a form a script can read.
///
/// Card 106 answered `null` for "off", which was true and useless: another
/// session drove `{"on":false}` from a shell script to borrow the panel for
/// firmware tests, got a 200, and ran a conformance suite against a panel the
/// studio was still streaming to at 30 fps - because back then this route only
/// switched the *preview's* link and the device's own player carried on. The
/// answer now says whether the panel is being driven, which panel, and what
/// the page is showing, like every other change route.
#[derive(Serialize)]
struct PanelOutcome {
    /// Panel output, after this change. **False means the panel has been let
    /// go**: `FINAL` has been sent and it is back on its own idle screen.
    on: bool,
    /// Which panel, by id. Empty when none is attached.
    device: String,
    /// What to call it.
    label: String,
    /// The link, or `null` when output is off.
    panel: Option<PanelStatus>,
    /// What the page is showing, which carries on either way.
    state: StudioState,
}

/// **Panel output**, and which panel the page is attached to.
///
/// Two bodies matter and are kept exactly, because another session drives them
/// from a script to borrow the panel for firmware tests:
///
/// - `{"on":false}` - the link is released with `FINAL`, the panel goes back to
///   its own idle screen and **stops receiving frames**, and the page carries
///   on showing the piece.
/// - `{"on":true,"to":"screeny-4a00a4"}` - attach to that panel and drive it.
///   `to` may be a device id from the registry, an mDNS instance name or an
///   address; one that is not known yet is added, exactly as
///   `POST /devices/add` would. An empty `to` means "the panel already
///   attached", so `{"on":true}` simply turns output back on.
///
/// Off is off for **every** player, not only the one on the page: a script
/// that says "let the panel go" means the panel, and a second panel's player
/// quietly holding the first one's lock would be exactly the surprise this is
/// here to prevent.
async fn set_panel(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPanel>) -> ApiResult<Json<PanelOutcome>> {
    let player = if req.on {
        let device = match req.to.trim() {
            "" => st.page().device(),
            to => find_or_add(&st, to)?,
        };
        crate::fleet::attach(&st, &device)
    } else {
        // Everything stops driving a panel, and the page's player is the one
        // whose answer comes back.
        for p in st.players.all() {
            let _ = p.configure(&PlayerChange { on: Some(false), ..PlayerChange::default() });
            crate::fleet::aim_at_device(&st, &p);
        }
        st.page()
    };
    player
        .configure(&PlayerChange { on: Some(req.on), ..PlayerChange::default() })
        .map_err(ApiError::bad_request)?;
    crate::fleet::aim_at_device(&st, &player);
    player.ensure_running();
    let state = publish(&st, &headers, &player);
    let status = player.status();
    Ok(Json(PanelOutcome {
        on: status.on,
        device: status.device.clone(),
        label: st.devices.get(&status.device).map(|d| d.label()).unwrap_or_default(),
        panel: status.panel,
        state,
    }))
}

/// Turn "whatever the human meant" into a device id, adding the device if this
/// studio has never heard of it.
///
/// A registry id, an mDNS instance name and a typed address are all accepted,
/// because the page has ids and a human has names.
fn find_or_add(st: &AppState, to: &str) -> ApiResult<String> {
    if st.devices.get(to).is_some() {
        return Ok(to.to_string());
    }
    if let Some(d) = st
        .devices
        .list()
        .into_iter()
        .find(|d| d.stored.instance == to || d.stored.address == to)
    {
        return Ok(d.stored.id);
    }
    st.devices.add_manual(to, "").map_err(ApiError::bad_request)
}

// ------------------ card 106: devices, players and health ------------------

/// The device list on its own, for a client that only wants that much.
async fn devices_list(State(st): State<AppState>) -> Json<Vec<crate::health::DeviceStatus>> {
    Json(crate::health::collect(&st).devices)
}

#[derive(Deserialize)]
struct AddDevice {
    /// A name (`screeny-4a00a4`) or an address (`192.168.7.221`, `127.0.0.1:49374`).
    to: String,
    #[serde(default)]
    name: String,
    /// Attach to it and start playing straight away. With no panel attached
    /// yet this adopts the player the page is already showing, so the picture
    /// does not restart - it simply starts reaching the panel.
    #[serde(default)]
    play: bool,
    /// A device id: this panel has *moved*, rather than being a new one.
    /// It keeps its id, its player and everything it was playing; only the
    /// way there changes.
    #[serde(default)]
    device: String,
}

async fn devices_add(State(st): State<AppState>, Json(req): Json<AddDevice>) -> ApiResult<Json<serde_json::Value>> {
    if !req.device.is_empty() {
        if !st.devices.set_address(&req.device, &req.to) {
            return Err(ApiError::not_found(format!("no device `{}`", req.device)));
        }
        st.persist();
        return Ok(Json(serde_json::json!({ "id": req.device, "moved": req.to.trim() })));
    }
    let id = st.devices.add_manual(&req.to, &req.name).map_err(ApiError::bad_request)?;
    if req.play {
        let player = crate::fleet::attach(&st, &id);
        let _ = player.configure(&PlayerChange { on: Some(true), ..PlayerChange::default() });
        crate::fleet::aim_at_device(&st, &player);
        player.ensure_running();
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
    // The page never goes blank: whatever is left becomes what it shows, and
    // with nothing left that is an unbound player with no link.
    st.page().ensure_running();
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
    /// "Reset": this piece back to its defaults on this panel, and forget what
    /// was remembered for it (card 165). The page's equivalent is
    /// `POST /reset_params`.
    #[serde(default)]
    reset_params: bool,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    paused: Option<bool>,
    #[serde(default)]
    speed: Option<f64>,
    #[serde(default)]
    settings: Option<Settings>,
    /// Absent leaves the policy alone; `null` clears it; a number sets it.
    #[serde(default, deserialize_with = "double_option")]
    brightness: Option<Option<u8>>,
    /// Start the piece again from its seed.
    #[serde(default)]
    restart: bool,
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
    let player = crate::fleet::player_for(&st, &req.device);
    player
        .configure(&PlayerChange {
            on: req.on,
            piece: req.piece,
            seed: req.seed,
            param: req.param.map(|p| (p.id, p.value)),
            reset_params: req.reset_params,
            fps: req.fps,
            paused: req.paused,
            speed: req.speed,
            settings: req.settings,
            brightness: req.brightness,
            restart: req.restart,
            act: None,
        })
        .map_err(ApiError::bad_request)?;
    crate::fleet::aim_at_device(&st, &player);
    player.ensure_running();
    // The page is showing this player if it is the focused one, and the other
    // browsers have to hear about it either way.
    if player.is_focused() {
        st.publish_state(None, player.state());
    }
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
        p.configure(&PlayerChange { brightness: Some(Some(level)), ..PlayerChange::default() })
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
/// either way. The studio's own label is what the page shows, so renaming a
/// panel that is currently off still works.
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
