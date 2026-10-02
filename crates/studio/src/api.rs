//! The JSON API: everything the page can ask the studio to do.
//!
//! One route per command, the names unchanged since the desktop app's IPC, so
//! `invoke('set_patch', {id})` became `POST /api/v1/set_patch {"id": ...}` and
//! nothing else had to move. Reads are `GET`, changes are `POST`.
//!
//! **Card 353: the picture routes take a `channel`** - an id, in the JSON body
//! or as `?channel=` - and without one they mean **Channel 1**. For
//! compatibility a `panel` on those routes means *that panel's channel*. An
//! edit reaches every panel on the channel, which is the point: panels on one
//! channel are frame-for-frame identical. The channels themselves are made,
//! renamed and deleted with `/channels/*`, and a panel is moved between them
//! with `/panel/channel`. Card 350's implicit rules, `same_as` and `detach` are
//! retired. Panel routes (`/device/*`, `set_panel`, `panel_status`) keep
//! `panel`/`device`.
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

use crate::channel::{find_patch, Channel, ChannelId, Edit};
use crate::page::{self, Bootstrap, StudioState};
use crate::panel::Panel;
use crate::panels::{ChannelSummary, PanelSummary, PlayerStatus};
use crate::AppState;

/// The header a browser tags its own changes with, so the state it just made
/// is not echoed back to it over the WebSocket while a slider is moving.
pub const CLIENT_HEADER: &str = "x-studio-client";

/// "Something changed", as the broadcast carries it. Each socket turns it
/// into a [`StateEvent`] for its own channel when it sends it.
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
    /// Card 353: the channel this is the state of - the socket's own.
    pub channel: ChannelId,
    /// The panel the socket named (`?panel=`), else empty.
    pub panel: String,
    pub state: StudioState,
}

/// The half-second heartbeat: what the patch is performing and what the panel
/// link is doing. Pushed rather than polled, so N browsers cost one read.
#[derive(Clone, Serialize)]
pub struct StatusEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Card 353: the socket's channel.
    pub channel: ChannelId,
    pub playing: Option<Playing>,
    /// The link of the socket's panel - the one it named, else its channel's
    /// first member - or `null`.
    pub panel: Option<PanelStatus>,
}

/// An error the UI can put on the notice line.
pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad_request(message: String) -> Self {
        ApiError(StatusCode::BAD_REQUEST, message)
    }

    /// No device, panel or channel with that id. The page's list is stale;
    /// reload it.
    fn not_found(message: String) -> Self {
        ApiError(StatusCode::NOT_FOUND, message)
    }

    /// The device is known but cannot be reached right now. A fact about the
    /// panel and not a fault in the server.
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
        .route("/panels", get(panels_list))
        .route("/set_picture", post(set_picture))
        // ---- card 353: channels own the picture ----
        .route("/channels", get(channels_list))
        .route("/channels/new", post(channels_new))
        .route("/channels/rename", post(channels_rename))
        .route("/channels/delete", post(channels_delete))
        .route("/panel/channel", post(panel_channel))
        // ---- card 350's, retired by card 353 ----
        .route("/same_as", post(retired))
        .route("/detach", post(retired))
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

// ---------------- which channel ----------------

/// `?channel=<id>` and `?panel=<device id>`, on any route.
#[derive(Default, Deserialize)]
pub struct Which {
    #[serde(default)]
    channel: Option<ChannelId>,
    #[serde(default)]
    panel: Option<String>,
}

/// The two fields every picture route's body may carry.
#[derive(Default, Deserialize)]
struct Target {
    #[serde(default)]
    channel: Option<ChannelId>,
    #[serde(default)]
    panel: Option<String>,
}

/// What a picture request is about: the channel, and the panel it named (the
/// point of view for `on` and `device` in the answer).
struct Aim {
    channel: Arc<Channel>,
    from: Option<Arc<Panel>>,
}

/// **The channel a request means**: the body's `channel`, else the query's;
/// else the channel of the body's or the query's `panel`; else Channel 1.
fn target(st: &AppState, body: &Target, query: &Which) -> ApiResult<Aim> {
    let channel = body.channel.or(query.channel);
    let panel = body.panel.as_deref().filter(|b| !b.trim().is_empty()).or(query.panel.as_deref());
    let (channel, from) = st.channel(channel, panel).map_err(ApiError::not_found)?;
    Ok(Aim { channel, from })
}

/// For the routes that take no arguments but the target, and read their body
/// only so the connection closes cleanly.
fn body_target(body: &[u8]) -> Target {
    serde_json::from_slice::<Target>(body).unwrap_or_default()
}

/// Tell the other browsers, write it down, and answer with the channel's
/// state.
fn publish(st: &AppState, headers: &HeaderMap, aim: &Aim) -> StudioState {
    let from = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    st.changed(from);
    st.persist();
    st.state_of(&aim.channel, aim.from.as_ref())
}

/// An edit to a channel's picture: every panel on it.
fn edit(st: &AppState, headers: &HeaderMap, aim: &Aim, change: &Edit) -> ApiResult<Json<StudioState>> {
    aim.channel.edit(change).map_err(ApiError::bad_request)?;
    aim.channel.ensure_running();
    Ok(Json(publish(st, headers, aim)))
}

/// A patch this build has, or the sentence a typo gets.
fn patch_def(st: &AppState, id: &str) -> ApiResult<&'static PatchDef> {
    find_patch(id, st.cfg.fault_patches).ok_or_else(|| ApiError::bad_request(format!("no patch called `{id}`")))
}

/// **Pick a picture for a channel** - every panel on it changes, which is the
/// point (card 353). `setting` of `None` means "just the patch": asking for
/// the patch the channel is already on changes nothing (it keeps its tweaks,
/// as it always has), except that it is a human saying "try it" to a patch
/// the channel had given up on.
fn pick(st: &AppState, channel: &Arc<Channel>, def: &'static PatchDef, setting: Option<&str>) -> ApiResult<()> {
    if setting.is_none() && channel.patch() == def.id {
        if channel.status().health.gave_up.is_some() {
            if let Some((def, work)) = channel.working() {
                channel.show(def, &channel.setting(), work, None);
            }
        }
        channel.ensure_running();
        return Ok(());
    }
    st.panels.pick(channel, def, setting.unwrap_or(""), None).map_err(ApiError::bad_request)?;
    channel.ensure_running();
    Ok(())
}

// ---------------- reads ----------------

async fn bootstrap(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<Json<Bootstrap>> {
    let aim = target(&st, &Target::default(), &q)?;
    let gpu = screeny_art::gpu_status();
    Ok(Json(Bootstrap {
        patches: page::patches(st.cfg.fault_patches, &gpu),
        payload_bytes: screeny_art::meter::PAYLOAD_BYTES,
        state: st.state_of(&aim.channel, aim.from.as_ref()),
        gpu,
        brightness_stops: page::brightness_stops(),
    }))
}

/// The newest frame packet of a channel - its encoded frame, decoded: what
/// every panel on it is showing - for a client that would rather poll than
/// open a WebSocket (and for tests, which then need no WebSocket).
async fn frame(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<impl IntoResponse> {
    let packet = target(&st, &Target::default(), &q)?.channel.screen().newest();
    Ok(([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], packet.to_vec()))
}

async fn patch_playing(State(st): State<AppState>, Query(q): Query<Which>) -> ApiResult<Json<Option<Playing>>> {
    Ok(Json(target(&st, &Target::default(), &q)?.channel.playing()))
}

/// A panel's link: `?panel=`, else the first panel. `null` when there is no
/// panel, or its output is off.
async fn panel_status(State(st): State<AppState>, Query(q): Query<Which>) -> Json<Option<PanelStatus>> {
    Json(st.panel(q.panel.as_deref()).ok().and_then(|p| p.link_status()))
}

/// Every panel, first adopted first.
#[derive(Serialize)]
struct PanelsAnswer {
    panels: Vec<PanelSummary>,
}

async fn panels_list(State(st): State<AppState>) -> Json<PanelsAnswer> {
    Json(PanelsAnswer { panels: st.summaries() })
}

/// Card 353: every channel, Channel 1 first.
#[derive(Serialize)]
struct ChannelsAnswer {
    channels: Vec<ChannelSummary>,
}

async fn channels_list(State(st): State<AppState>) -> Json<ChannelsAnswer> {
    Json(ChannelsAnswer { channels: st.panels.channel_summaries() })
}

// ---------------- card 353: channels ----------------

#[derive(Deserialize)]
struct NewChannel {
    /// Absent or empty is "Channel <id>".
    #[serde(default)]
    name: Option<String>,
    /// The channel to copy - picture and working copy. Absent is
    /// Channel 1.
    #[serde(default)]
    from: Option<ChannelId>,
}

/// **New channel**: a copy of `from` (Channel 1 by default), with no panels.
/// Answers with the new channel's state.
async fn channels_new(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<NewChannel>) -> ApiResult<Json<StudioState>> {
    let from = match req.from {
        Some(id) => Some(st.panels.channel(id).ok_or_else(|| ApiError::not_found(format!("no channel {id}")))?),
        None => None,
    };
    let channel = st.panels.new_channel(req.name.as_deref(), from.as_ref()).map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers, &Aim { channel, from: None })))
}

#[derive(Deserialize)]
struct RenameChannel {
    channel: ChannelId,
    name: String,
}

async fn channels_rename(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<RenameChannel>) -> ApiResult<Json<StudioState>> {
    let channel = st.panels.channel(req.channel).ok_or_else(|| ApiError::not_found(format!("no channel {}", req.channel)))?;
    st.panels.rename_channel(&channel, &req.name).map_err(ApiError::bad_request)?;
    Ok(Json(publish(&st, &headers, &Aim { channel, from: None })))
}

#[derive(Deserialize)]
struct DeleteChannel {
    channel: ChannelId,
}

/// **Delete a channel**: its panels move to Channel 1. Channel 1 is a 400.
/// Answers with Channel 1's state.
async fn channels_delete(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<DeleteChannel>) -> ApiResult<Json<StudioState>> {
    let channel = st.panels.channel(req.channel).ok_or_else(|| ApiError::not_found(format!("no channel {}", req.channel)))?;
    let moved = channel.followers();
    st.panels.delete_channel(&channel).map_err(ApiError::bad_request)?;
    for p in &moved {
        crate::fleet::aim_at_device(&st, p);
    }
    Ok(Json(publish(&st, &headers, &Aim { channel: st.panels.home(), from: None })))
}

#[derive(Deserialize)]
struct PanelChannel {
    panel: String,
    channel: ChannelId,
}

/// **Move a panel to a channel**: it fades there over 2 s, alone, and is then
/// on that channel's shared frames. Answers with the channel's state, seen
/// from the panel.
async fn panel_channel(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<PanelChannel>) -> ApiResult<Json<StudioState>> {
    let panel = st.panels.get(req.panel.trim()).ok_or_else(|| ApiError::not_found(format!("no panel `{}`", req.panel.trim())))?;
    let channel = st.panels.channel(req.channel).ok_or_else(|| ApiError::not_found(format!("no channel {}", req.channel)))?;
    crate::fleet::move_to_channel(&st, &panel, &channel);
    Ok(Json(publish(&st, &headers, &Aim { channel, from: Some(panel) })))
}

/// What `same_as` and `detach` say now (card 353).
pub const PICK_RULES_RETIRED: &str = "`same_as` and `detach` are retired (card 353): panels are on explicit channels now. \
    Make one with POST /api/v1/channels/new and move a panel with POST /api/v1/panel/channel";

/// Card 350's `same_as` and `detach`, **retired** by card 353: a 410 that
/// says what to do instead.
async fn retired(_body: axum::body::Bytes) -> ApiError {
    ApiError(StatusCode::GONE, PICK_RULES_RETIRED.to_string())
}

// ---------------- changes ----------------

#[derive(Deserialize)]
struct SetPatch {
    /// The patch id. `patch` and `piece` are taken as well, because
    /// `POST /set_piece {"piece": ...}` is what scripts written before card
    /// 150 send.
    #[serde(alias = "patch", alias = "piece")]
    id: String,
    #[serde(flatten)]
    at: Target,
}

/// A patch, on its Default, for a channel - every panel on it.
async fn set_patch(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetPatch>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let def = patch_def(&st, &req.id)?;
    pick(&st, &aim.channel, def, None)?;
    Ok(Json(publish(&st, &headers, &aim)))
}

#[derive(Deserialize)]
struct SetPicture {
    #[serde(alias = "id", alias = "piece")]
    patch: String,
    /// A named setting of that patch; absent or empty is Default.
    #[serde(default)]
    setting: String,
    #[serde(flatten)]
    at: Target,
}

/// **A picture** - a patch on one of its named settings - for a channel, in
/// one step. What Home Assistant's picture select does, as a route.
async fn set_picture(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetPicture>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let def = patch_def(&st, &req.patch)?;
    pick(&st, &aim.channel, def, Some(&req.setting))?;
    Ok(Json(publish(&st, &headers, &aim)))
}

#[derive(Deserialize)]
struct SetParam {
    id: String,
    value: f32,
    #[serde(flatten)]
    at: Target,
}

async fn set_param(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetParam>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    edit(&st, &headers, &aim, &Edit { param: Some((req.id, req.value)), ..Edit::default() })
}

/// Takes no arguments but the target - and reads the body anyway, because
/// the UI sends `{}` and a server that closes a connection with a request
/// body still unread gets a TCP reset rather than a clean close.
async fn reset_params(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &body_target(&body), &q)?;
    edit(&st, &headers, &aim, &Edit { reset_params: true, ..Edit::default() })
}

#[derive(Deserialize)]
struct SetSeed {
    /// Absent or null picks a new one.
    #[serde(default)]
    seed: Option<u32>,
    #[serde(flatten)]
    at: Target,
}

async fn set_seed(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetSeed>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let seed = req.seed.unwrap_or_else(page::fresh_seed);
    edit(&st, &headers, &aim, &Edit { seed: Some(seed), ..Edit::default() })
}

#[derive(Deserialize)]
struct SetOutput {
    /// `settings` up to card 150.
    #[serde(alias = "settings")]
    output: Output,
    #[serde(flatten)]
    at: Target,
}

/// **The studio's output stage** (card 356): one setting for every channel.
/// A `channel` or `panel` (an older client's) is accepted, and only chooses
/// which state the answer describes - falling back to Channel 1 if it is gone.
async fn set_output(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetOutput>) -> ApiResult<Json<StudioState>> {
    st.panels.set_output(req.output);
    let aim = target(&st, &req.at, &q).unwrap_or_else(|_| Aim { channel: st.panels.home(), from: None });
    Ok(Json(publish(&st, &headers, &aim)))
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
    let aim = target(&st, &body_target(&body), &q)?;
    let mut answer = serde_json::to_value(st.state_of(&aim.channel, aim.from.as_ref())).unwrap_or_default();
    if let Some(o) = answer.as_object_mut() {
        o.insert("ignored".into(), PLAYBACK_RETIRED.into());
    }
    Ok(Json(answer))
}

#[derive(Deserialize)]
struct PatchAct {
    action: String,
    #[serde(default)]
    channel: Option<ChannelId>,
    /// Which panel's channel to act on. `device` is card 140's name for it
    /// and is still taken.
    #[serde(default, alias = "device")]
    panel: Option<String>,
}

async fn patch_act(State(st): State<AppState>, Query(q): Query<Which>, Json(req): Json<PatchAct>) -> ApiResult<Json<Option<Playing>>> {
    let aim = target(&st, &Target { channel: req.channel, panel: req.panel }, &q)?;
    aim.channel.edit(&Edit { act: Some(req.action), ..Edit::default() }).map_err(ApiError::bad_request)?;
    // A patch's own action changes the patch, not the studio's state, so there
    // is nothing to publish: the half-second status push carries it. What is
    // returned is what it was performing *before* the action is drained.
    Ok(Json(aim.channel.playing()))
}

/// Takes no arguments but the target; reads the body for the reason
/// `reset_params` does.
async fn restart(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &body_target(&body), &q)?;
    edit(&st, &headers, &aim, &Edit { restart: true, ..Edit::default() })
}

// ---------------- card 151: a patch's named settings ----------------
//
// Four changes in the same shape as every other one on this page: act, tell the
// other browsers, write it down. The **list**, the current name and `modified`
// travel in `StudioState`, which every one of these answers with.
//
// The working copy is a **channel's**, and the library of named settings is one
// per patch for the whole studio. So a save saves the channel's working copy; a
// rename or a delete moves every channel that was on that setting; and a load
// is picking a picture for the channel.
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
    #[serde(flatten)]
    at: Target,
}

/// Put the channel on a setting of the patch it is showing, in one change:
/// the parameters and the seed move together, one broadcast and one write.
async fn settings_load(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<LoadSetting>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let channel = &aim.channel;
    let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(channel)))?;
    let name = if req.name.trim().is_empty() { channel.setting() } else { req.name };
    // The one on it already, untouched, changes nothing; moved, it is Revert.
    pick(&st, channel, def, Some(&name))?;
    Ok(Json(publish(&st, &headers, &aim)))
}

#[derive(Deserialize)]
struct SaveSetting {
    /// A name saves under it ("Save as..."); no name overwrites the one the
    /// working copy is on ("Save"), which `Default` never is.
    #[serde(default)]
    name: Option<String>,
    #[serde(flatten)]
    at: Target,
}

async fn settings_save(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SaveSetting>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let channel = &aim.channel;
    let (def, work) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(channel)))?;
    let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(asked) => asked.to_string(),
        None => crate::state::own_setting(None, &channel.setting(), true).map_err(ApiError::bad_request)?,
    };
    let saved = st.memory.save_setting(def, &name, &work).map_err(ApiError::bad_request)?;
    channel.set_setting_name(&saved);
    Ok(Json(publish(&st, &headers, &aim)))
}

#[derive(Deserialize)]
struct RenameSetting {
    /// Which one; absent or empty is the one the working copy is on.
    #[serde(default, alias = "name")]
    from: String,
    to: String,
    #[serde(flatten)]
    at: Target,
}

async fn settings_rename(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<RenameSetting>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let channel = &aim.channel;
    let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(channel)))?;
    let from = crate::state::own_setting(Some(&req.from), &channel.setting(), false).map_err(ApiError::bad_request)?;
    let to = st.memory.rename_setting(def, &from, &req.to).map_err(ApiError::bad_request)?;
    st.panels.setting_renamed(def.id, &from, &to);
    Ok(Json(publish(&st, &headers, &aim)))
}

#[derive(Deserialize)]
struct DeleteSetting {
    #[serde(default)]
    name: String,
    #[serde(flatten)]
    at: Target,
}

/// Delete a setting. **What is playing does not change**: the values stay, and
/// what goes is the name they came from - so every channel that was on it says
/// `Default`, and `modified`, which is the truth.
async fn settings_delete(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<DeleteSetting>) -> ApiResult<Json<StudioState>> {
    let aim = target(&st, &req.at, &q)?;
    let channel = &aim.channel;
    let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(channel)))?;
    let name = crate::state::own_setting(Some(&req.name), &channel.setting(), false).map_err(ApiError::bad_request)?;
    let gone = st.memory.delete_setting(def, &name).map_err(ApiError::bad_request)?;
    st.panels.setting_deleted(def.id, &gone);
    Ok(Json(publish(&st, &headers, &aim)))
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
    crate::ha::client::forget(&cfg, &crate::ha::bridge::panel_keys(&st), &crate::ha::bridge::channel_ids(&st), std::time::Duration::from_secs(10)).await.map_err(|e| ApiError::unreachable(format!("removing from Home Assistant: {e}.")))?;
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
    /// The panel to switch, by device id. With neither this nor `to`,
    /// `on: true` means the first panel and `on: false` means **every** panel.
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
    /// Which panel, by id. Empty when there is no panel at all.
    device: String,
    /// What to call it.
    label: String,
    /// The link, or `null` when output is off.
    panel: Option<PanelStatus>,
    /// What its channel is showing, which carries on either way.
    state: StudioState,
}

/// **Panel output.**
///
/// The bodies that matter, kept exactly because a conformance script drives
/// them to borrow the panel for firmware tests:
///
/// - `{"on":false}` - **every** panel's link is released with `FINAL`, each
///   goes back to its own idle screen and **stops receiving frames**, and the
///   pictures carry on for the page. `{"on":false,"panel":ID}` lets just that
///   one go.
/// - `{"on":true,"to":"screeny-c0ffee"}` - find that panel (adding it, exactly
///   as `POST /devices/add` would, if it is new - onto Channel 1) and drive
///   it. `to` may be a device id, an mDNS instance name or an address.
/// - `{"on":true}` - output back on, for `panel` or the first panel.
async fn set_panel(State(st): State<AppState>, Query(q): Query<Which>, headers: HeaderMap, Json(req): Json<SetPanel>) -> ApiResult<Json<PanelOutcome>> {
    let named = req.panel.as_deref().or(q.panel.as_deref()).filter(|p| !p.trim().is_empty());
    let panel: Option<Arc<Panel>> = if req.on {
        match req.to.trim() {
            "" => {
                let panel = match named {
                    Some(id) => Some(st.panel(Some(id)).map_err(ApiError::not_found)?),
                    None => st.first(),
                };
                panel.map(|p| crate::fleet::drive(&st, &p.device()))
            }
            to => {
                let device = find_or_add(&st, to)?;
                Some(crate::fleet::drive(&st, &device))
            }
        }
    } else {
        let answer = match named {
            Some(id) => Some(st.panel(Some(id)).map_err(ApiError::not_found)?),
            None => st.first(),
        };
        let off: Vec<Arc<Panel>> = match (named, &answer) {
            (Some(_), Some(p)) => vec![Arc::clone(p)],
            _ => st.panels.all(),
        };
        for p in off {
            p.set_on(false);
            crate::fleet::aim_at_device(&st, &p);
        }
        answer
    };
    let channel = panel.as_ref().and_then(|p| p.channel()).unwrap_or_else(|| st.panels.home());
    let state = publish(&st, &headers, &Aim { channel, from: panel.clone() });
    let device = panel.as_ref().map(|p| p.device()).unwrap_or_default();
    Ok(Json(PanelOutcome {
        on: panel.as_ref().is_some_and(|p| p.cfg().on),
        label: st.devices.get(&device).map(|d| d.label()).unwrap_or_default(),
        device,
        panel: panel.as_ref().and_then(|p| p.link_status()),
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
    /// Drive it straight away: output on and its link aimed now. Since card
    /// 353 a new panel joins Channel 1 and lights up either way; without
    /// `play` the supervisor aims it within a second.
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
/// device rather than going through the page. Since card 353 everything about
/// the picture - a patch, a setting, a seed, a parameter, a reset, a restart,
/// the output settings - is **its channel's**, and so reaches every panel on
/// it; `on` and `brightness` are the panel's own.
async fn player_set(State(st): State<AppState>, Json(req): Json<SetPlayer>) -> ApiResult<Json<PlayerStatus>> {
    if st.devices.get(&req.device).is_none() {
        return Err(ApiError::not_found(format!("no device `{}`", req.device)));
    }
    let panel = st.panels.attach(&req.device);
    let channel = panel.channel().unwrap_or_else(|| st.panels.home());
    let change = Edit {
        seed: req.seed,
        param: req.param.map(|p| (p.id, p.value)),
        reset_params: req.reset_params,
        restart: req.restart,
        ..Edit::default()
    };
    match (req.patch.as_deref(), req.setting.as_deref()) {
        (Some(id), setting) => pick(&st, &channel, patch_def(&st, id)?, setting)?,
        (None, Some(setting)) => {
            let (def, _) = channel.working().ok_or_else(|| ApiError::bad_request(unknown_patch(&channel)))?;
            let name = if setting.trim().is_empty() { channel.setting() } else { setting.to_string() };
            pick(&st, &channel, def, Some(&name))?;
        }
        (None, None) => {}
    }
    if change.changes_the_picture() || change.restart {
        channel.edit(&change).map_err(ApiError::bad_request)?;
    }
    if let Some(on) = req.on {
        panel.set_on(on);
    }
    if let Some(output) = req.output {
        st.panels.set_output(output);
    }
    if let Some(b) = req.brightness {
        panel.set_brightness(b);
    }
    crate::fleet::aim_at_device(&st, &panel);
    channel.ensure_running();
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
