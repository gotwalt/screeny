//! `GET /api/v1/ws`: preview frames out, state changes out, a word from the
//! browser about how much of it it can use.
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
//! panel shows are the same bytes by construction. That is not negotiable and
//! nothing here re-renders or re-encodes anything for a browser; the only
//! question card 120 lets a browser answer is **which** of those frames it is
//! sent.
//!
//! # What a browser may ask for (card 120)
//!
//! One frame packet is 6196 bytes. At 60 fps that is 372 KB/s per tab, sent
//! whether or not the tab is visible and whether or not it can draw that fast,
//! which over a tailnet or to a phone is a lot for a 64x32 picture. So a socket
//! carries a **pace**, set by the query string when it opens
//! (`?fps=10&repeat=false`) and changed at any time with
//!
//! ```json
//! {"type":"preview","fps":30,"repeat":false}
//! ```
//!
//! - `fps`: the most frame packets a second this socket wants. **`0` means
//!   none** - what the page asks for when its tab is hidden. The status
//!   heartbeat and state changes carry on either way, so a hidden tab stays up
//!   to date for about a kilobyte a second and is right the moment it is
//!   looked at again.
//! - `repeat`: whether to send a picture that is pixel-for-pixel the one this
//!   socket was last sent. `false` skips it - a held clock face is identical
//!   for fifteen seconds at a time - except once every [`STILL`], so the
//!   header's counters and the elapsed-time readout keep moving.
//!
//! Both default to what a socket got before this card existed: [`DEFAULT_FPS`]
//! and `repeat` on. Pacing is done by **dropping**, never by holding: a frame
//! that is not due is thrown away where it stands and the socket waits for the
//! next one, so this can no more slow the render loop down than a stalled
//! browser can.
//!
//! **A hidden tab is not a watcher.** `fps: 0` gives up this socket's claim on
//! [`crate::page::Screen::watchers`], which is what a player reads to decide
//! whether anybody is looking - so a studio whose panel is away and whose only
//! tabs are hidden idles at `player::IDLE_FPS` instead of rendering 60 fps for
//! nobody.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

use crate::page::{Viewer, HEADER};
use crate::AppState;

/// How long one message may take to reach a browser before the connection is
/// treated as dead. A client that cannot take 6 KB in this long is not
/// watching a 60 fps preview.
const STALL: Duration = Duration::from_secs(3);

/// Frame packets a second for a socket that does not say what it wants.
///
/// The panel's target is 30 fps (`CLAUDE.md`), a 64x32 picture at 30 is smooth
/// by any measure, and a client that really does want every frame of a 60 fps
/// player only has to say `?fps=60`.
pub const DEFAULT_FPS: f64 = 30.0;
/// The most a socket may ask for: the player's own ceiling, because there is
/// nothing faster to send.
const MAX_FPS: f64 = crate::player::MAX_FPS;
/// And the slowest that is still a rate rather than "stop": one frame every
/// ten seconds. Only a guard - `Duration::from_secs_f64` panics on an absurd
/// one - since `0` is how a browser says "none".
const MIN_FPS: f64 = 0.1;
/// With `repeat` off, how long a picture that has not changed may go unsent.
/// The page draws its meters out of the frame header, so an unchanging picture
/// still has something to say; once a second is enough for it.
const STILL: Duration = Duration::from_secs(1);

#[derive(Deserialize)]
pub struct WsQuery {
    /// The browser's own id, matching the `X-Studio-Client` header it sends
    /// with changes: its own changes are not echoed back to it.
    #[serde(default)]
    client: Option<String>,
    /// The opening pace, so a tab that is already hidden when the page loads
    /// never costs a frame. Changed later with a `preview` message.
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    repeat: Option<bool>,
}

/// What a browser may say. Anything else - including a message from a page
/// newer than this server - is ignored rather than fatal.
#[derive(Deserialize)]
struct ClientMessage {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    repeat: Option<bool>,
}

/// One socket's pace: how often it wants a frame, and whether it wants one it
/// has already been sent.
struct Pace {
    /// `None` when this socket wants no frames at all.
    gap: Option<Duration>,
    repeat: bool,
    /// When a frame was last sent on this socket.
    last_at: Option<Instant>,
    /// And what its picture was, kept only while `repeat` is off.
    last_sent: Option<Arc<Vec<u8>>>,
    /// When a frame last *arrived*, which is how long the next one is likely
    /// to be: see [`Pace::take`].
    last_arrival: Option<Instant>,
}

impl Pace {
    fn new(fps: f64, repeat: bool) -> Pace {
        let mut pace = Pace { gap: None, repeat, last_at: None, last_sent: None, last_arrival: None };
        pace.set_fps(fps);
        pace
    }

    /// A rate that is not a number is ignored; `0` or less means no frames.
    fn set_fps(&mut self, fps: f64) {
        if !fps.is_finite() {
            return;
        }
        self.gap = (fps > 0.0).then(|| Duration::from_secs_f64(1.0 / fps.clamp(MIN_FPS, MAX_FPS)));
    }

    fn wants_frames(&self) -> bool {
        self.gap.is_some()
    }

    /// Whether this frame goes out - and, if it does, remember it.
    ///
    /// Called with the newest frame in hand; a frame that is not due is simply
    /// dropped, which is the same thing that happens to a browser that is too
    /// slow to read one.
    ///
    /// The frame **nearest** the deadline is the one that goes, not the first
    /// one past it: a 30 fps cap on a 60 fps player whose frames land a
    /// fraction early would otherwise take every third frame and deliver 20.
    /// So a frame counts as due once the next one would be further from the
    /// deadline than this one is - half a source frame early, in other words,
    /// where "a source frame" is however long it has been since the last one
    /// arrived.
    fn take(&mut self, packet: &Arc<Vec<u8>>) -> bool {
        let Some(gap) = self.gap else { return false };
        let now = Instant::now();
        let since = self.last_at.map(|t| now.duration_since(t));
        let step = self.last_arrival.map_or(Duration::ZERO, |t| now.duration_since(t));
        self.last_arrival = Some(now);
        if since.is_some_and(|d| d + step / 2 < gap) {
            return false;
        }
        // The same picture as last time, and not yet time to say so again.
        if !self.repeat
            && since.is_some_and(|d| d < STILL)
            && self.last_sent.as_ref().is_some_and(|prev| same_picture(prev, packet))
        {
            return false;
        }
        self.last_at = Some(now);
        self.last_sent = (!self.repeat).then(|| Arc::clone(packet));
        true
    }
}

/// Whether two frame packets draw the same thing. Only the pixels: the header
/// carries a sequence number and a clock, which always differ.
fn same_picture(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.len() >= HEADER && a[HEADER..] == b[HEADER..]
}

pub async fn upgrade(ws: WebSocketUpgrade, Query(q): Query<WsQuery>, State(st): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| run(socket, st, q))
}

async fn run(mut socket: WebSocket, st: AppState, q: WsQuery) {
    let mut frames = st.screen.watch();
    let mut states = st.states.subscribe();
    let mut status = st.status.subscribe();
    let mut stop = st.stop.clone();
    let me = q.client;

    // Opening the socket is *not* what makes a player render at full rate;
    // asking for frames is. See the module docs.
    let mut viewer = st.screen.viewer();
    let mut pace = Pace::new(q.fps.unwrap_or(DEFAULT_FPS), q.repeat.unwrap_or(true));
    viewer.set_wants_frames(pace.wants_frames());

    // What the browser would otherwise have to ask for on connecting.
    let hello = st.state_event(None, st.page_state());
    match as_text(&hello) {
        Some(msg) => {
            if !send(&mut socket, msg, &viewer, false).await {
                return;
            }
        }
        None => return,
    }

    loop {
        // Each arm decides what to send; the sending happens below, outside the
        // `select!`, because one of the arms is reading from the same socket.
        let out: Option<Message> = tokio::select! {
            // Frames first when several are ready: the preview is the point.
            biased;
            r = frames.changed(), if pace.wants_frames() => {
                if r.is_err() { break }
                let packet = frames.borrow_and_update().clone();
                pace.take(&packet).then(|| Message::Binary(packet.as_slice().to_vec().into()))
            }
            r = states.recv() => match r {
                // Skip the change this very browser made.
                Ok(ev) if ev.from.is_some() && ev.from == me => None,
                Ok(ev) => as_text(&ev),
                // Too far behind to know what it missed: give it the truth.
                Err(RecvError::Lagged(_)) => as_text(&st.state_event(None, st.page_state())),
                Err(RecvError::Closed) => break,
            },
            r = status.changed() => {
                if r.is_err() { break }
                let msg = status.borrow_and_update().clone();
                as_text(&*msg)
            }
            // The browser talking back: the pace, or the close it sends on its
            // way out. Reading it is also how a tab being switched away from
            // stops costing anything.
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    apply(&text, &mut pace, &mut viewer);
                    None
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => None,
                Some(Err(_)) => break,
            },
            // The studio is stopping. A browser that is keeping up perfectly
            // must not be what holds the shutdown open.
            // The borrow `wait_for` hands back is not `Send`, and this future
            // has to be: discard it inside the block rather than in the arm.
            () = async { drop(stop.wait_for(|s| *s).await) } => break,
        };
        if let Some(msg) = out {
            let frame = matches!(msg, Message::Binary(_));
            if !send(&mut socket, msg, &viewer, frame).await {
                break;
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

/// A `preview` message from the browser. Anything else is ignored: a page from
/// a newer build must not be able to break this one, and a page from an older
/// build never sends any.
fn apply(text: &str, pace: &mut Pace, viewer: &mut Viewer) {
    let Ok(msg) = serde_json::from_str::<ClientMessage>(text) else { return };
    if msg.kind != "preview" {
        return;
    }
    if let Some(fps) = msg.fps {
        pace.set_fps(fps);
        viewer.set_wants_frames(pace.wants_frames());
    }
    if let Some(repeat) = msg.repeat {
        pace.repeat = repeat;
        if repeat {
            pace.last_sent = None;
        }
    }
}

fn as_text<T: serde::Serialize>(value: &T) -> Option<Message> {
    match serde_json::to_string(value) {
        Ok(text) => Some(Message::Text(text.into())),
        Err(e) => {
            eprintln!("studio: serialising a websocket message: {e}");
            None
        }
    }
}

/// False when the browser is gone, or so far behind that it may as well be.
async fn send(socket: &mut WebSocket, msg: Message, viewer: &Viewer, frame: bool) -> bool {
    let bytes = match &msg {
        Message::Text(t) => t.len(),
        Message::Binary(b) => b.len(),
        _ => 0,
    };
    match tokio::time::timeout(STALL, socket.send(msg)).await {
        Ok(Ok(())) => {
            viewer.sent(bytes, frame);
            true
        }
        Ok(Err(_)) => false,
        Err(_) => {
            eprintln!("studio: a browser stopped reading for {STALL:?}; closing its preview socket");
            false
        }
    }
}
