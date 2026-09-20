//! `GET /api/v1/ws`: preview frames out, state changes out.
//!
//! Three things reach a browser here, and all three are **newest-wins**:
//!
//! - binary: one frame packet, the same 52-byte header + 64x32 sRGB the
//!   desktop app used to hand over (`engine::HEADER`);
//! - `{"type":"state",...}`: somebody changed something;
//! - `{"type":"status",...}`: twice a second, what the piece is performing and
//!   what the panel link is doing.
//!
//! Nothing is queued for a browser. Frames and status live in
//! [`tokio::sync::watch`] cells - one slot, overwritten in place - and state
//! changes in a broadcast channel of fixed capacity whose overflow is handled
//! by sending the current state instead of the missed ones. A browser that
//! stops reading therefore cannot make the server hold anything on its
//! behalf, and cannot slow a player or the panel link down: the render loop
//! writes into the cell and never waits for a reader. If a single send
//! cannot complete within [`STALL`] the socket is closed and forgotten.
//!
//! Since card 170 the frames are the **attached panel's**: the same decoded
//! datagrams the panel is being sent, so what the browser draws and what the
//! panel shows are the same bytes by construction.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;

/// How long one message may take to reach a browser before the connection is
/// treated as dead. A client that cannot take 6 KB in this long is not
/// watching a 60 fps preview.
const STALL: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
pub struct WsQuery {
    /// The browser's own id, matching the `X-Studio-Client` header it sends
    /// with changes: its own changes are not echoed back to it.
    #[serde(default)]
    client: Option<String>,
}

pub async fn upgrade(ws: WebSocketUpgrade, Query(q): Query<WsQuery>, State(st): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| run(socket, st, q.client))
}

async fn run(mut socket: WebSocket, st: AppState, me: Option<String>) {
    // Subscribing is also how a player knows somebody is watching: a panel
    // that is away with no browser open drops to `player::IDLE_FPS`.
    let mut frames = st.screen.watch();
    let mut states = st.states.subscribe();
    let mut status = st.status.subscribe();
    let mut stop = st.stop.clone();

    // What the browser would otherwise have to ask for on connecting.
    let hello = st.state_event(None, st.page_state());
    if !send_json(&mut socket, &hello).await {
        return;
    }

    loop {
        tokio::select! {
            // Frames first when several are ready: the preview is the point.
            biased;
            r = frames.changed() => {
                if r.is_err() { break }
                let packet = frames.borrow_and_update().clone();
                if !send(&mut socket, Message::Binary(packet.as_slice().to_vec().into())).await { break }
            }
            r = states.recv() => match r {
                Ok(ev) => {
                    // Skip the change this very browser made.
                    if ev.from.is_some() && ev.from == me { continue }
                    if !send_json(&mut socket, &ev).await { break }
                }
                // Too far behind to know what it missed: give it the truth.
                Err(RecvError::Lagged(_)) => {
                    let now = st.state_event(None, st.page_state());
                    if !send_json(&mut socket, &now).await { break }
                }
                Err(RecvError::Closed) => break,
            },
            r = status.changed() => {
                if r.is_err() { break }
                let msg = status.borrow_and_update().clone();
                if !send_json(&mut socket, &*msg).await { break }
            }
            // The studio is stopping. A browser that is keeping up perfectly
            // must not be what holds the shutdown open.
            // The borrow `wait_for` hands back is not `Send`, and this future
            // has to be: discard it inside the block rather than in the arm.
            () = async { drop(stop.wait_for(|s| *s).await) } => break,
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn send_json<T: serde::Serialize>(socket: &mut WebSocket, value: &T) -> bool {
    match serde_json::to_string(value) {
        Ok(text) => send(socket, Message::Text(text.into())).await,
        Err(e) => {
            eprintln!("studio: serialising a websocket message: {e}");
            true
        }
    }
}

/// False when the browser is gone, or so far behind that it may as well be.
async fn send(socket: &mut WebSocket, msg: Message) -> bool {
    match tokio::time::timeout(STALL, socket.send(msg)).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            eprintln!("studio: a browser stopped reading for {STALL:?}; closing its preview socket");
            false
        }
    }
}
