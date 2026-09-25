//! The JSON API: everything the page can ask the studio to do.
//!
//! One route per command, the names unchanged since the desktop app's IPC, so
//! `invoke('set_patch', {id})` became `POST /api/v1/set_patch {"id": ...}` and
//! nothing else had to move. Reads are `GET`, changes are `POST`.
//!
//! **Card 170 changed what these act on, not what they are.** There is no
//! design-view engine any more: `set_patch`, `set_param`, `set_seed`,
//! `set_output`, `set_playback`, `patch_act` and `restart` act on the player
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
use screeny_art::patch::Playing;
use screeny_art::Output;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::page::{self, Bootstrap, StudioState};
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

/// The half-second heartbeat: what the patch is performing and what the panel
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
        .route("/patch_playing", get(patch_playing))
        .route("/panel_status", get(panel_status))
        .route("/set_patch", post(set_patch))
        .route("/set_param", post(set_param))
        .route("/reset_params", post(reset_params))
        .route("/set_seed", post(set_seed))
        .route("/set_output", post(set_output))
        .route("/set_playback", post(set_playback))
        .route("/patch_act", post(patch_act))
        .route("/restart", post(restart))
        .route("/set_panel", post(set_panel))
        // ---- card 151: a patch's named settings ----
        .route("/settings/load", post(settings_load))
        .route("/settings/save", post(settings_save))
        .route("/settings/rename", post(settings_rename))
        .route("/settings/delete", post(settings_delete))
        // ---- card 150: the names a piece went by, still answering ----
        .route("/piece_playing", get(patch_playing))
        .route("/set_piece", post(set_patch))
        .route("/set_settings", post(set_output))
        .route("/piece_act", post(patch_act))
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
        patches: page::patches(st.cfg.fault_patches),
        payload_bytes: screeny_art::meter::PAYLOAD_BYTES,
        state: st.page_state(),
        gpu: screeny_art::gpu_status(),
        brightness_stops: page::brightness_stops(),
    })
}

/// The newest frame packet, for a client that would rather poll than open a
/// WebSocket (and for tests, which then need no WebSocket at all).
async fn frame(State(st): State<AppState>) -> impl IntoResponse {
    let packet: Arc<Vec<u8>> = st.screen.newest();
    ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], packet.to_vec())
}

async fn patch_playing(State(st): State<AppState>) -> Json<Option<Playing>> {
    Json(st.page().playing())
}

async fn panel_status(State(st): State<AppState>) -> Json<Option<PanelStatus>> {
    Json(st.page().status().panel)
}

// ---------------- changes ----------------

#[derive(Deserialize)]
struct SetPatch {
    /// The patch id. `patch` and `piece` are taken as well, because
    /// `POST /set_piece {"piece": ...}` is what scripts written before card
    /// 150 send.
    #[serde(alias = "patch", alias = "piece")]
    id: String,
}

async fn set_patch(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPatch>) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { patch: Some(req.id), ..PlayerChange::default() })
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
struct SetOutput {
    /// `settings` up to card 150.
    #[serde(alias = "settings")]
    output: Output,
}

async fn set_output(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetOutput>) -> ApiResult<Json<StudioState>> {
    on_page(&st, &headers, &PlayerChange { output: Some(req.output), ..PlayerChange::default() })
}

/// **`fps` is gone from this body** (card 161). It is not declared here and
/// serde ignores what it does not know, so a page or a script that still sends
/// `{paused, speed, fps}` is accepted in full and the rate is simply not a
/// thing any more. Card 172's slider over `MIN_FPS..=MAX_FPS` went with it:
/// there is one rate, `screeny_art::FPS`, and `StudioState::fps` reports it.
#[derive(Deserialize)]
struct SetPlayback {
    paused: bool,
    speed: f64,
}

async fn set_playback(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SetPlayback>) -> ApiResult<Json<StudioState>> {
    on_page(
        &st,
        &headers,
        &PlayerChange {
            paused: Some(req.paused),
            speed: Some(req.speed),
            ..PlayerChange::default()
        },
    )
}

#[derive(Deserialize)]
struct PatchAct {
    action: String,
    /// Which panel's patch to act on. Absent means the one on the page, which
    /// is what a browser sends. (Card 140: a panel's composing patch can be
    /// acted on, through a one-slot mailbox its render loop drains - never by
    /// reaching into a running patch from another thread.)
    #[serde(default)]
    device: String,
}

async fn patch_act(State(st): State<AppState>, Json(req): Json<PatchAct>) -> ApiResult<Json<Option<Playing>>> {
    let player = if req.device.is_empty() {
        st.page()
    } else {
        st.players.get(&req.device).ok_or_else(|| ApiError::not_found(format!("no player for `{}`", req.device)))?
    };
    player
        .configure(&PlayerChange { act: Some(req.action), ..PlayerChange::default() })
        .map_err(ApiError::bad_request)?;
    // A patch's own action changes the patch, not the studio's state, so there
    // is nothing to publish: the half-second status push carries it. What is
    // returned is what it was performing *before* the action is drained - the
    // page redraws from the next heartbeat.
    Ok(Json(player.playing()))
}

// ---------------- card 151: a patch's named settings ----------------
//
// Four changes in the same shape as every other one on this page: act, tell the
// other browsers, write it down. The **list**, the current name and `modified`
// travel in `StudioState`, which every one of these answers with, so a browser
// needs no extra `GET` and a second browser sees a save the moment it happens.
//
// Note for anyone reading both APIs: the **panel's own** firmware serves a
// `POST /api/v1/settings` (`docs/design/device-web.md`) for its WiFi and name.
// That one is on the device, on port 80; these are the studio's, and they are
// about a patch. Nothing is shared but the word.

#[derive(Deserialize)]
struct LoadSetting {
    /// The setting's name, or `Default`. Empty means the one it is already on,
    /// which is what "Revert" sends.
    #[serde(default)]
    name: String,
}

/// Put the working copy back on a setting.
///
/// **One change**: the parameters, the seed and the speed move together, so
/// there is one broadcast and one write, and the panel follows in one step
/// rather than through a burst of half-loaded pictures (card 196's pacing is
/// about a burst; a load must not be one).
async fn settings_load(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<LoadSetting>) -> ApiResult<Json<StudioState>> {
    let name = if req.name.trim().is_empty() {
        st.page().state().setting
    } else {
        req.name
    };
    on_page(&st, &headers, &PlayerChange { load_setting: Some(name), ..PlayerChange::default() })
}

#[derive(Deserialize)]
struct SaveSetting {
    /// A name saves under it ("Save as..."); no name overwrites the one the
    /// working copy is on ("Save"), which `Default` never is.
    #[serde(default)]
    name: Option<String>,
}

async fn settings_save(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<SaveSetting>) -> ApiResult<Json<StudioState>> {
    let player = st.page();
    let (def, work) = player.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&player)))?;
    st.memory
        .save_setting(def, req.name.as_deref(), &work)
        .map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers, &player)))
}

#[derive(Deserialize)]
struct RenameSetting {
    /// Which one; absent or empty is the one the working copy is on.
    #[serde(default, alias = "name")]
    from: String,
    to: String,
}

async fn settings_rename(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<RenameSetting>) -> ApiResult<Json<StudioState>> {
    let player = st.page();
    let (def, _) = player.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&player)))?;
    st.memory
        .rename_setting(def, Some(req.from.as_str()).filter(|f| !f.trim().is_empty()), &req.to)
        .map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers, &player)))
}

#[derive(Deserialize)]
struct DeleteSetting {
    #[serde(default)]
    name: String,
}

/// Delete a setting. **What is playing does not change**: the values stay, and
/// what goes is the name they came from - so the answer says `Default`, and
/// `modified`, which is the truth about values nothing is holding any more.
async fn settings_delete(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<DeleteSetting>) -> ApiResult<Json<StudioState>> {
    let player = st.page();
    let (def, _) = player.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&player)))?;
    st.memory
        .delete_setting(def, Some(req.name.as_str()).filter(|n| !n.trim().is_empty()))
        .map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers, &player)))
}

/// The one refusal these four share: a state file naming a patch this build
/// has not got. There is no spec to measure values against, so there is
/// nothing honest to save them as.
fn unknown_patch(player: &Arc<Player>) -> String {
    format!("`{}` is not a patch this build has, so it has no settings.", player.stored().patch)
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
/// Two bodies matter and are kept exactly, because a conformance script drives
/// them to borrow the panel for firmware tests:
///
/// - `{"on":false}` - the link is released with `FINAL`, the panel goes back to
///   its own idle screen and **stops receiving frames**, and the page carries
///   on showing the patch.
/// - `{"on":true,"to":"screeny-c0ffee"}` - attach to that panel and drive it.
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
    /// A name (`screeny-c0ffee`) or an address (`192.168.1.50`, `127.0.0.1:49374`).
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
        let asked = tokio::task::spawn_blocking(move || crate::devices::identify_at_counted(addr)).await;
        // Card 164: counted against the panel whether or not it answered.
        let asked = match asked {
            Ok((cost, out)) => {
                st.devices.metered_control(&id, cost);
                Ok(out)
            }
            Err(e) => Err(e),
        };
        match asked {
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
    /// `piece` before card 150; both are taken.
    #[serde(default, alias = "piece")]
    patch: Option<String>,
    #[serde(default)]
    seed: Option<u32>,
    #[serde(default)]
    param: Option<ParamChange>,
    /// "Reset": this patch back to its defaults on this panel, and forget what
    /// was remembered for it (card 165). The page's equivalent is
    /// `POST /reset_params`.
    #[serde(default)]
    reset_params: bool,
    // Card 161: `fps` was here. It is not declared any more and serde ignores
    // unknown keys, so a script that still sends it is accepted and the field
    // does nothing - which is the whole of what it can mean now.
    #[serde(default)]
    paused: Option<bool>,
    #[serde(default)]
    speed: Option<f64>,
    /// `settings` before card 150; both are taken.
    #[serde(default, alias = "settings")]
    output: Option<Output>,
    /// Absent leaves the policy alone; `null` clears it; a number sets it.
    #[serde(default, deserialize_with = "double_option")]
    brightness: Option<Option<u8>>,
    /// Start the patch again from its seed.
    #[serde(default)]
    restart: bool,
    /// Card 151: put this panel's patch on one of its named settings, or on
    /// `Default`. The page uses `POST /settings/load`, which is the same change
    /// on the panel the page is a window onto.
    #[serde(default)]
    setting: Option<String>,
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
            patch: req.patch,
            seed: req.seed,
            param: req.param.map(|p| (p.id, p.value)),
            reset_params: req.reset_params,
            paused: req.paused,
            speed: req.speed,
            output: req.output,
            brightness: req.brightness,
            restart: req.restart,
            act: None,
            load_setting: req.setting,
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
    let done = tokio::task::spawn_blocking(move || crate::devices::control_call(addr, f)).await;
    // Card 164: every control conversation with a panel is counted against it,
    // however it went.
    let done = match done {
        Ok((cost, out)) => {
            st.devices.metered_control(device, cost);
            out
        }
        Err(e) => return Err(ApiError::unreachable(format!("{what}: {e}"))),
    };
    match done {
        Ok(v) => Ok(v),
        Err(e) => {
            st.devices.control_failed(device, format!("{what}: {e}"));
            Err(ApiError::unreachable(format!("{what}: {e}")))
        }
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

/// **The only reboot this studio asks for.** Card 195: the ask is written down
/// here, so that when the panel comes back with a new `boot_id` the studio can
/// tell its own reboot from one it knows nothing about - which, on firmware
/// that cannot report a panic, is the nearest thing to "it may have crashed"
/// there is (`devices::DeviceFacts::unasked_reboots`).
///
/// Written down *before* the request goes out: see `Registry::asked_to_reboot`.
async fn device_reboot(State(st): State<AppState>, Json(req): Json<Reboot>) -> ApiResult<Json<serde_json::Value>> {
    if !req.confirm {
        return Err(ApiError::bad_request("rebooting a panel needs `confirm: true`".into()));
    }
    st.devices.asked_to_reboot(&req.device);
    on_device(&st, &req.device, "rebooting", |c| c.reboot().map_err(|e| e.to_string())).await?;
    Ok(Json(serde_json::json!({ "rebooting": req.device })))
}

/// Telemetry, read now rather than from the five-second poll.
async fn device_stats(State(st): State<AppState>, Json(req): Json<DeviceRef>) -> ApiResult<Json<crate::devices::Telem>> {
    let t = on_device(&st, &req.device, "asking for telemetry", |c| c.telemetry().map_err(|e| e.to_string())).await?;
    st.devices.heard(&req.device, &t);
    Ok(Json(crate::devices::Telem::of(&t)))
}
