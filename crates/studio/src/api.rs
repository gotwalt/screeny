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
}

/// Publish the engine's new state to every browser but the one that caused it.
fn publish(st: &AppState, headers: &HeaderMap) -> StudioState {
    let state = lock(&st.engine).snapshot();
    let from = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);
    st.publish_state(from, state.clone());
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

async fn reset_params(State(st): State<AppState>, headers: HeaderMap) -> Json<StudioState> {
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

async fn restart(State(st): State<AppState>, headers: HeaderMap) -> Json<StudioState> {
    lock(&st.engine).restart();
    Json(publish(&st, &headers))
}

#[derive(Deserialize)]
struct SetPanel {
    on: bool,
    #[serde(default)]
    to: String,
}

async fn set_panel(State(st): State<AppState>, Json(req): Json<SetPanel>) -> Json<Option<PanelStatus>> {
    Json(lock(&st.engine).set_panel(req.on, &req.to))
}
