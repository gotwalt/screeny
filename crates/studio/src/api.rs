//! The JSON API: everything the page can ask the studio to do.
//!
//! One route per command, the names unchanged since the desktop app's IPC, so
//! `invoke('set_patch', {id})` became `POST /api/v1/set_patch {"id": ...}` and
//! nothing else had to move. Reads are `GET`, changes are `POST`.
//!
//! **Card 350: every route that acts on "the picture" or "the panel" takes a
//! `panel`** - a device id - in the JSON body or as `?panel=` in the query
//! string (the body wins). Without one it means **the first panel**, the one
//! adopted first, so a page or a script written before there were several
//! panels keeps working and keeps changing the panel it always changed.
//! Channels are never addressed directly: a client always says which panel it
//! means, and the picture routes act on that panel's channel - so an edit on a
//! panel that shares its channel reaches every panel on it, which is what
//! *"Also on Kitchen"* on the page is for.
//!
//! Picking a picture (`set_patch`, `set_picture`, `settings/load`,
//! `player/set` with a patch or a setting) goes through card 350's three rules
//! ([`crate::panels::Panels::pick`]); `same_as` and `detach` are the two
//! explicit ways of joining and leaving a channel.
//!
//! Every change is announced on [`crate::AppState::states`] so that the other
//! browsers watching stay in step; the sender's own socket is skipped, which
//! is what the `X-Studio-Client` header is for.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use screeny_art::output::PanelStatus;
use screeny_art::patch::{PatchDef, Playing};
use screeny_art::Output;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::channel::{find_patch, Channel, Edit};
use crate::page::{self, Bootstrap, StudioState};
use crate::panel::Panel;
use crate::panels::{PanelSummary, PlayerStatus};
use crate::AppState;

/// The header a browser tags its own changes with, so the state it just made
/// is not echoed back to it over the WebSocket while a slider is moving.
pub const CLIENT_HEADER: &str = "x-studio-client";

/// "Something changed", as the broadcast carries it. Each socket turns it
/// into a [`StateEvent`] for its own panel when it sends it (card 350): one
/// change can move several panels' states at once - an edit to a shared
/// channel - and a socket only ever speaks for one panel.
#[derive(Clone, Debug)]
pub struct Changed {
    /// Increases by one per change.
    pub rev: u64,
    /// Which browser made the change, if it said.
    pub from: Option<String>,
}

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
    /// Card 350: which panel this is the state of - the socket's own.
    pub panel: String,
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

    /// No device or panel with that id. The page's list is stale; reload it.
    fn not_found(message: String) -> Self {
        ApiError(StatusCode::NOT_FOUND, message)
    }

    /// The device is known but cannot be reached right now - or, card 350,
    /// the panel is idle and has no picture to change. A fact about the panel
    /// and not a fault in the server.
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
        // ---- card 350: several panels ----
        .route("/panels", get(panels_list))
        .route("/set_picture", post(set_picture))
        .route("/same_as", post(same_as))
        .route("/detach", post(detach))
        // ---- card 151: a patch's named settings ----
        .route("/settings/load", post(settings_load))
        .route("/settings/save", post(settings_save))
        .route("/settings/rename", post(settings_rename))
        .route("/settings/delete", post(settings_delete))
        // ---- card 311: Home Assistant, set up on the Settings screen ----
        .route("/home_assistant", get(ha_get))
        .route("/home_assistant/set", post(ha_set))
        .route("/home_assistant/forget", post(ha_forget))
        // ---- card 150: the names a piece went by, still answering ----
        .route("/piece_playing", get(patch_playing))
        .route("/set_piece", post(set_patch))
        .route("/set_settings", post(set_output))
        .route("/piece_act", post(patch_act))
        // ---- card 106: devices, panels and health ----
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

// ---------------- which panel ----------------

/// `?panel=<device id>`, on any route.
#[derive(Default, Deserialize)]
pub struct Which {
    #[serde(default)]
    panel: Option<String>,
}

/// The panel a request means: the body's `panel`, else the query's, else the
/// first panel.
fn target(st: &AppState, body: Option<&str>, query: &Which) -> ApiResult<Arc<Panel>> {
    let which = body.filter(|b| !b.trim().is_empty()).or(query.panel.as_deref());
    st.panel(which).map_err(ApiError::not_found)
}

/// For the routes that take no arguments and read their body only so the
/// connection closes cleanly: the `panel` in it, if it has one.
fn body_panel(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Which>(body).ok().and_then(|w| w.panel)
}

/// The channel a panel follows, or the refusal for an idle one.
fn channel_of(panel: &Arc<Panel>) -> ApiResult<Arc<Channel>> {
    panel.channel().ok_or_else(|| {
        ApiError::unreachable(format!("`{}` is idle: it has no picture yet, so pick one for it first.", panel.device()))
    })
}

/// Tell the other browsers, write it down, and answer with the panel's state.
fn publish(st: &AppState, headers: &HeaderMap, panel: &Arc<Panel>) -> StudioState {
    let from = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    st.changed(from);
    st.persist();
    st.state_of(panel)
}

/// An edit to the picture a panel shows: its channel, so every panel on it.
fn edit(st: &AppState, headers: &HeaderMap, panel: &Arc<Panel>, change: &Edit) -> ApiResult<Json<StudioState>> {
    let channel = channel_of(panel)?;
    channel.edit(change).map_err(ApiError::bad_request)?;
    channel.ensure_running();
    Ok(Json(publish(st, headers, panel)))
}

/// A patch this build has, or the sentence a typo gets.
fn patch_def(st: &AppState, id: &str) -> ApiResult<&'static PatchDef> {
    find_patch(id, st.cfg.fault_patches).ok_or_else(|| ApiError::bad_request(format!("no patch called `{id}`")))
}

/// **Pick a picture for a panel** by card 350's rules, then make sure it is
/// being rendered and streamed. `setting` of `None` means "just the patch":
/// asking for the patch the panel is already on changes nothing (it keeps its
/// tweaks, as it always has), except that it is a human saying "try it" to a
/// patch its channel had given up on.
fn pick(st: &AppState, panel: &Arc<Panel>, def: &'static PatchDef, setting: Option<&str>, exclusive: bool) -> ApiResult<()> {
    if setting.is_none() {
        if let Some(c) = panel.channel().filter(|c| c.patch() == def.id) {
            if c.status().health.gave_up.is_some() {
                if let Some((def, work)) = c.working() {
                    c.show(def, &c.setting(), work, None);
                }
            }
            c.ensure_running();
            return Ok(());
        }
    }
    let channel = st.panels.pick(panel, def, setting.unwrap_or(""), exclusive, None).map_err(ApiError::bad_request)?;
    channel.ensure_running();
    crate::fleet::aim_at_device(st, panel);
    Ok(())
}

// ---------------- reads ----------------

async fn bootstrap(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<Json<Bootstrap>> {
    let panel = target(&st, None, &q)?;
    Ok(Json(Bootstrap {
        patches: page::patches(st.cfg.fault_patches),
        payload_bytes: screeny_art::meter::PAYLOAD_BYTES,
        state: st.state_of(&panel),
        gpu: screeny_art::gpu_status(),
        brightness_stops: page::brightness_stops(),
    }))
}

/// The newest frame packet a panel was sent, for a client that would rather
/// poll than open a WebSocket (and for tests, which then need no WebSocket).
async fn frame(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<impl IntoResponse> {
    let packet = target(&st, None, &q)?.screen().newest();
    Ok(([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], packet.to_vec()))
}

async fn patch_playing(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<Json<Option<Playing>>> {
    Ok(Json(target(&st, None, &q)?.channel().and_then(|c| c.playing())))
}

async fn panel_status(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<Json<Option<PanelStatus>>> {
    Ok(Json(target(&st, None, &q)?.link_status()))
}

/// Card 350: the overview - every panel, first first.
#[derive(Serialize)]
struct PanelsAnswer {
    panels: Vec<PanelSummary>,
}

async fn panels_list(State(st): State<AppState>) -> Json<PanelsAnswer> {
    let _ = st.first();
    Json(PanelsAnswer { panels: st.summaries() })
}

// ---------------- changes ----------------

#[derive(Deserialize)]
struct SetPatch {
    /// The patch id. `patch` and `piece` are taken as well, because
    /// `POST /set_piece {"piece": ...}` is what scripts written before card
    /// 150 send.
    #[serde(alias = "patch", alias = "piece")]
    id: String,
    #[serde(default)]
    panel: Option<String>,
}

/// A patch, on its Default - by the three rules.
async fn set_patch(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetPatch>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let def = patch_def(&st, &req.id)?;
    pick(&st, &panel, def, None, false)?;
    Ok(Json(publish(&st, &headers, &panel)))
}

#[derive(Deserialize)]
struct SetPicture {
    #[serde(alias = "id", alias = "piece")]
    patch: String,
    /// A named setting of that patch; absent or empty is Default.
    #[serde(default)]
    setting: String,
    #[serde(default)]
    panel: Option<String>,
}

/// Card 350: **a picture** - a patch on one of its named settings - for a
/// panel, in one step, by the three rules. What Home Assistant's picture
/// select does, as a route.
async fn set_picture(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetPicture>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let def = patch_def(&st, &req.patch)?;
    pick(&st, &panel, def, Some(&req.setting), false)?;
    Ok(Json(publish(&st, &headers, &panel)))
}

#[derive(Deserialize)]
struct SameAs {
    /// The panel whose channel to join.
    #[serde(rename = "as")]
    other: String,
    #[serde(default)]
    panel: Option<String>,
}

/// Card 350: **"Same as <panel>"** - put this panel on that one's channel,
/// tweaks and all, in sync.
async fn same_as(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SameAs>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let other = st.panels.get(req.other.trim()).ok_or_else(|| ApiError::not_found(format!("no panel `{}`", req.other.trim())))?;
    let channel = st.panels.same_as(&panel, &other).map_err(ApiError::unreachable)?;
    channel.ensure_running();
    crate::fleet::aim_at_device(&st, &panel);
    Ok(Json(publish(&st, &headers, &panel)))
}

/// Card 350: **Detach** - a copy of the channel for this panel alone, so an
/// edit to it stops reaching the others. Body: `{panel?}`.
async fn detach(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, body_panel(&body).as_deref(), &q)?;
    let channel = st.panels.detach(&panel).map_err(ApiError::unreachable)?;
    channel.ensure_running();
    Ok(Json(publish(&st, &headers, &panel)))
}

#[derive(Deserialize)]
struct SetParam {
    id: String,
    value: f32,
    #[serde(default)]
    panel: Option<String>,
}

async fn set_param(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetParam>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    edit(&st, &headers, &panel, &Edit { param: Some((req.id, req.value)), ..Edit::default() })
}

/// Takes no arguments but `panel` - and reads the body anyway, because the
/// UI sends `{}` and a server that closes a connection with a request body
/// still unread gets a TCP reset rather than a clean close.
async fn reset_params(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, body_panel(&body).as_deref(), &q)?;
    edit(&st, &headers, &panel, &Edit { reset_params: true, ..Edit::default() })
}

#[derive(Deserialize)]
struct SetSeed {
    /// Absent or null picks a new one.
    #[serde(default)]
    seed: Option<u32>,
    #[serde(default)]
    panel: Option<String>,
}

async fn set_seed(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetSeed>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let seed = req.seed.unwrap_or_else(page::fresh_seed);
    edit(&st, &headers, &panel, &Edit { seed: Some(seed), ..Edit::default() })
}

#[derive(Deserialize)]
struct SetOutput {
    /// `settings` up to card 150.
    #[serde(alias = "settings")]
    output: Output,
    #[serde(default)]
    panel: Option<String>,
}

/// The panel's own output stage (card 350: a panel's, not its channel's, so
/// two panels on one picture can differ in dither or limiter).
async fn set_output(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetOutput>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    panel.set_output(req.output);
    Ok(Json(publish(&st, &headers, &panel)))
}

/// What `set_playback` says instead of doing anything (card 302).
pub const PLAYBACK_RETIRED: &str = "speed and pause are not settings any more (card 302): \
    every patch plays at 1.00x and nothing pauses, so this changed nothing";

/// **Retired** (card 302; the owner took Speed and Pause off the page on
/// 2026-09-26). The route still answers 200 - a page or a script that still
/// sends `{paused, speed}` is not broken by it - but it **changes nothing**,
/// and says so: the answer is the whole state, as before, with one more key,
/// `ignored`, holding [`PLAYBACK_RETIRED`]. Any body at all is accepted.
async fn set_playback(State(st): State<AppState>, Query(q): Query<Which>, body: axum::body::Bytes) -> ApiResult<Json<serde_json::Value>> {
    let panel = target(&st, body_panel(&body).as_deref(), &q)?;
    let mut answer = serde_json::to_value(st.state_of(&panel)).unwrap_or_default();
    if let Some(o) = answer.as_object_mut() {
        o.insert("ignored".into(), PLAYBACK_RETIRED.into());
    }
    Ok(Json(answer))
}

#[derive(Deserialize)]
struct PatchAct {
    action: String,
    /// Which panel's patch to act on. `device` is card 140's name for it and
    /// is still taken; absent means the first panel.
    #[serde(default, alias = "device")]
    panel: Option<String>,
}

async fn patch_act(State(st): State<AppState>, Query(q): Query<Which>, Json(req): Json<PatchAct>) -> ApiResult<Json<Option<Playing>>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let channel = channel_of(&panel)?;
    channel.edit(&Edit { act: Some(req.action), ..Edit::default() }).map_err(ApiError::bad_request)?;
    // A patch's own action changes the patch, not the studio's state, so there
    // is nothing to publish: the half-second status push carries it. What is
    // returned is what it was performing *before* the action is drained.
    Ok(Json(channel.playing()))
}

/// Takes no arguments but `panel`; reads the body for the reason
/// `reset_params` does.
async fn restart(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, body_panel(&body).as_deref(), &q)?;
    edit(&st, &headers, &panel, &Edit { restart: true, ..Edit::default() })
}

// ---------------- card 151: a patch's named settings ----------------
//
// Four changes in the same shape as every other one on this page: act, tell the
// other browsers, write it down. The **list**, the current name and `modified`
// travel in `StudioState`, which every one of these answers with.
//
// Since card 350 the working copy is a **channel's**, and the library of named
// settings is still one per patch for the whole studio. So a save saves the
// panel's channel's working copy; a rename or a delete moves every channel that
// was on that setting; and a load is *picking a picture*, by the three rules.
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
    #[serde(default)]
    panel: Option<String>,
}

/// Put the panel on a setting of the patch it is showing - picking a picture,
/// by the three rules, in one change: the parameters and the seed move
/// together, one broadcast and one write.
async fn settings_load(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<LoadSetting>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let channel = channel_of(&panel)?;
    let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&channel)))?;
    let name = if req.name.trim().is_empty() { channel.setting() } else { req.name };
    pick(&st, &panel, def, Some(&name), false)?;
    Ok(Json(publish(&st, &headers, &panel)))
}

#[derive(Deserialize)]
struct SaveSetting {
    /// A name saves under it ("Save as..."); no name overwrites the one the
    /// working copy is on ("Save"), which `Default` never is.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    panel: Option<String>,
}

async fn settings_save(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SaveSetting>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let channel = channel_of(&panel)?;
    let (def, work) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&channel)))?;
    let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(asked) => asked.to_string(),
        None => crate::state::own_setting(None, &channel.setting(), true).map_err(ApiError::bad_request)?,
    };
    let saved = st.memory.save_setting(def, &name, &work).map_err(ApiError::bad_request)?;
    channel.set_setting_name(&saved);
    Ok(Json(publish(&st, &headers, &panel)))
}

#[derive(Deserialize)]
struct RenameSetting {
    /// Which one; absent or empty is the one the working copy is on.
    #[serde(default, alias = "name")]
    from: String,
    to: String,
    #[serde(default)]
    panel: Option<String>,
}

async fn settings_rename(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<RenameSetting>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let channel = channel_of(&panel)?;
    let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&channel)))?;
    let from = crate::state::own_setting(Some(&req.from), &channel.setting(), false).map_err(ApiError::bad_request)?;
    let to = st.memory.rename_setting(def, &from, &req.to).map_err(ApiError::bad_request)?;
    st.panels.setting_renamed(def.id, &from, &to);
    Ok(Json(publish(&st, &headers, &panel)))
}

#[derive(Deserialize)]
struct DeleteSetting {
    #[serde(default)]
    name: String,
    #[serde(default)]
    panel: Option<String>,
}

/// Delete a setting. **What is playing does not change**: the values stay, and
/// what goes is the name they came from - so every channel that was on it says
/// `Default`, and `modified`, which is the truth.
async fn settings_delete(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<DeleteSetting>) -> ApiResult<Json<StudioState>> {
    let panel = target(&st, req.panel.as_deref(), &q)?;
    let channel = channel_of(&panel)?;
    let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&channel)))?;
    let name = crate::state::own_setting(Some(&req.name), &channel.setting(), false).map_err(ApiError::bad_request)?;
    let gone = st.memory.delete_setting(def, &name).map_err(ApiError::bad_request)?;
    st.panels.setting_deleted(def.id, &gone);
    Ok(Json(publish(&st, &headers, &panel)))
}

/// The one refusal these four share: a channel on a patch this build has not
/// got. There is no spec to measure values against, so there is nothing
/// honest to save them as.
fn unknown_patch(channel: &Arc<Channel>) -> String {
    format!("`{}` is not a patch this build has, so it has no settings.", channel.patch())
}

// ------------------------- card 311: Home Assistant -------------------------

/// The settings without the password, how the connection is doing, and where
/// to look on the broker.
async fn ha_get(State(st): State<AppState>) -> Json<crate::ha::HaView> {
    Json(st.ha.view())
}

/// Every field optional: what is absent stays as it is. **`password` absent
/// keeps the saved one**, because the page never has it to send back; an empty
/// string clears it.
#[derive(Deserialize)]
struct SetHa {
    enabled: Option<bool>,
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    password: Option<String>,
    discovery_prefix: Option<String>,
    instance: Option<String>,
    name: Option<String>,
}

/// Change the settings; the connection follows at once - dropped, made, or
/// made again somewhere else.
async fn ha_set(State(st): State<AppState>, Json(req): Json<SetHa>) -> ApiResult<Json<crate::ha::HaView>> {
    let was = st.ha.settings();
    let next = crate::ha::HaSettings {
        enabled: req.enabled.unwrap_or(was.enabled),
        host: req.host.unwrap_or(was.host),
        port: req.port.unwrap_or(was.port),
        username: req.username.unwrap_or(was.username),
        password: req.password.unwrap_or(was.password),
        discovery_prefix: req.discovery_prefix.unwrap_or(was.discovery_prefix),
        instance: req.instance.unwrap_or(was.instance),
        name: req.name.unwrap_or(was.name),
    }
    .check()
    .map_err(ApiError::bad_request)?;
    st.ha.set(next);
    st.persist();
    Ok(Json(st.ha.view()))
}

/// **Remove this studio from Home Assistant**: switch the integration off,
/// then connect once more to clear everything it left retained, which makes
/// HA drop the device and its entities. The settings are kept, switched off,
/// so switching it back on brings the device back.
async fn ha_forget(State(st): State<AppState>, _body: axum::body::Bytes) -> ApiResult<Json<crate::ha::HaView>> {
    let was = st.ha.settings();
    let cfg = crate::ha::HaSettings { enabled: true, ..was.clone() }
        .connection()
        .ok_or_else(|| ApiError::bad_request("There is no broker set up, so there is nothing to remove.".into()))?;
    st.ha.set(crate::ha::HaSettings { enabled: false, ..was });
    st.persist();
    // The running client must be gone first: `forget` connects as the same
    // client, and the broker would throw one off for the other.
    st.ha.until_off(crate::ha::client::GOODBYE + std::time::Duration::from_secs(1)).await;
    crate::ha::client::forget(&cfg, std::time::Duration::from_secs(10)).await.map_err(|e| ApiError::unreachable(format!("removing from Home Assistant: {e}.")))?;
    Ok(Json(st.ha.view()))
}

// ------------------------------ panel output ------------------------------

#[derive(Deserialize)]
struct SetPanel {
    on: bool,
    /// A panel to find (or add) and drive: a device id, an mDNS instance name
    /// or an address.
    #[serde(default)]
    to: String,
    /// Card 350: the panel to switch, by device id. With neither this nor
    /// `to`, `on: true` means the first panel and `on: false` means **every**
    /// panel.
    #[serde(default)]
    panel: Option<String>,
}

/// What `set_panel` answers: what happened, in a form a script can read.
///
/// Card 106 answered `null` for "off", which was true and useless: another
/// session drove `{"on":false}` from a shell script to borrow the panel for
/// firmware tests, got a 200, and ran a conformance suite against a panel the
/// studio was still streaming to at 30 fps. The answer says whether the panel
/// is being driven, which panel, and what it is showing.
#[derive(Serialize)]
struct PanelOutcome {
    /// Panel output, after this change. **False means the panel has been let
    /// go**: `FINAL` has been sent and it is back on its own idle screen.
    on: bool,
    /// Which panel, by id. Empty for the unbound stand-in.
    device: String,
    /// What to call it.
    label: String,
    /// The link, or `null` when output is off.
    panel: Option<PanelStatus>,
    /// What the panel is showing, which carries on either way.
    state: StudioState,
}

/// **Panel output.**
///
/// The bodies that matter, kept exactly because a conformance script drives
/// them to borrow the panel for firmware tests:
///
/// - `{"on":false}` - **every** panel's link is released with `FINAL`, each
///   goes back to its own idle screen and **stops receiving frames**, and the
///   pictures carry on for the page. Off is off for every panel: a script that
///   says "let the panel go" means the panel, and a second panel quietly
///   holding the first one's lock would be exactly the surprise this is here
///   to prevent. `{"on":false,"panel":ID}` lets just that one go (card 350).
/// - `{"on":true,"to":"screeny-c0ffee"}` - find that panel (adding it, exactly
///   as `POST /devices/add` would, if it is new) and drive it. `to` may be a
///   device id, an mDNS instance name or an address. A panel that is idle is
///   given a picture: the unbound stand-in's, if the studio has not had a
///   panel before, else the default patch on Default by the three rules.
/// - `{"on":true}` - output back on, for `panel` or the first panel.
async fn set_panel(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetPanel>) -> ApiResult<Json<PanelOutcome>> {
    let panel = if req.on {
        match req.to.trim() {
            "" => {
                let panel = target(&st, req.panel.as_deref(), &q)?;
                crate::fleet::drive(&st, &panel.device())
            }
            to => {
                let device = find_or_add(&st, to)?;
                crate::fleet::drive(&st, &device)
            }
        }
    } else {
        let only = req.panel.as_deref().or(q.panel.as_deref()).filter(|p| !p.trim().is_empty());
        let answer = target(&st, only, &q)?;
        let off: Vec<Arc<Panel>> = if only.is_some() { vec![Arc::clone(&answer)] } else { st.panels.all() };
        for p in off {
            p.set_on(false);
            crate::fleet::aim_at_device(&st, &p);
        }
        answer
    };
    let state = publish(&st, &headers, &panel);
    let device = panel.device();
    Ok(Json(PanelOutcome {
        on: panel.cfg().on,
        label: st.devices.get(&device).map(|d| d.label()).unwrap_or_default(),
        device,
        panel: panel.link_status(),
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

// ------------------ card 106: devices, panels and health ------------------

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
    /// Drive it straight away. A studio that has not had a panel before hands
    /// it the picture the page is already showing, so nothing restarts; any
    /// other idle panel is given the default patch. Without `play` a new
    /// panel is adopted idle (card 350).
    #[serde(default)]
    play: bool,
    /// A device id: this panel has *moved*, rather than being a new one.
    /// It keeps its id, its panel and everything it was playing; only the
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
        crate::fleet::drive(&st, &id);
    } else {
        st.panels.adopt(&id);
    }
    st.persist();
    st.changed(None);
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
    st.panels.remove(&req.device);
    // There is always a first panel: with nothing left that is the unbound
    // stand-in, on a picture of its own and with no link.
    let _ = st.first();
    st.panels.start();
    st.persist();
    st.changed(None);
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
                    st.panels.rekey(&from, &new_id);
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
    /// "Reset": this picture back to its patch's defaults. The page's
    /// equivalent is `POST /reset_params`.
    #[serde(default)]
    reset_params: bool,
    // Card 161: `fps` was here, and card 302 took `paused` and `speed` the
    // same way. None is declared any more and serde ignores unknown keys, so a
    // script that still sends them is accepted and they do nothing.
    /// `settings` before card 150; both are taken.
    #[serde(default, alias = "settings")]
    output: Option<Output>,
    /// Absent leaves the policy alone; `null` clears it; a number sets it.
    #[serde(default, deserialize_with = "double_option")]
    brightness: Option<Option<u8>>,
    /// Start the patch again from its seed.
    #[serde(default)]
    restart: bool,
    /// Card 151: a named setting of the patch - the one given, or the one the
    /// panel is on. With `patch` or on its own, a picture: the three rules.
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

/// **One panel, everything at once** - card 106's route for a script naming a
/// device rather than going through the page. A patch or a setting is a
/// picture, picked by card 350's rules; a seed, a parameter, a reset or a
/// restart is an edit of the channel the panel is then on (so a request that
/// picks *and* edits never joins another panel's channel - its edit would land
/// on them); `on`, `output` and `brightness` are the panel's own.
async fn player_set(State(st): State<AppState>, Json(req): Json<SetPlayer>) -> ApiResult<Json<PlayerStatus>> {
    if st.devices.get(&req.device).is_none() {
        return Err(ApiError::not_found(format!("no device `{}`", req.device)));
    }
    let panel = st.panels.attach(&req.device);
    let change = Edit {
        seed: req.seed,
        param: req.param.map(|p| (p.id, p.value)),
        reset_params: req.reset_params,
        restart: req.restart,
        ..Edit::default()
    };
    let exclusive = change.changes_the_picture();
    match (req.patch.as_deref(), req.setting.as_deref()) {
        (Some(id), setting) => pick(&st, &panel, patch_def(&st, id)?, setting, exclusive)?,
        (None, Some(setting)) => {
            let channel = channel_of(&panel)?;
            let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&channel)))?;
            let name = if setting.trim().is_empty() { channel.setting() } else { setting.to_string() };
            pick(&st, &panel, def, Some(&name), exclusive)?;
        }
        (None, None) => {}
    }
    if exclusive || change.restart {
        // An edit on a shared channel is an edit for everybody on it: that is
        // what sharing a channel means (card 350, "editing is per channel").
        channel_of(&panel)?.edit(&change).map_err(ApiError::bad_request)?;
    }
    if let Some(on) = req.on {
        panel.set_on(on);
    }
    if let Some(output) = req.output {
        panel.set_output(output);
    }
    if let Some(b) = req.brightness {
        panel.set_brightness(b);
    }
    crate::fleet::aim_at_device(&st, &panel);
    if let Some(c) = panel.channel() {
        c.ensure_running();
    }
    st.changed(None);
    st.persist();
    Ok(Json(st.panels.status_of(&panel)))
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
    if let Some(p) = st.panels.get(&req.device) {
        p.set_brightness(Some(level));
        p.brightness_applied(level, applied);
    }
    st.persist();
    st.changed(None);
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
