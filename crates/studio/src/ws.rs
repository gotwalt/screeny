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
//!
//! # And the state is paced too (card 196)
//!
//! Dragging a slider is about sixty `set_param` a second, and each one used to
//! put a full `StudioState` - measured at 416 bytes - on every *other* socket.
//! That is 25 KB/s of JSON for one control moving, and on a hidden tab, which
//! asks for no pictures at all, it is everything that tab costs.
//!
//! The newest state is the only one worth having, exactly as the newest frame
//! is, so [`Gate`] does for state what [`Pace`] does for frames - with one
//! difference that matters. A frame that is not due is **dropped**; a state
//! change that is not due is **held**, because a state message is also how a
//! deliberate change (a piece picked, a switch flipped) reaches the other
//! browser and the last one must never be lost. So:
//!
//! - **leading edge**: a change after a quiet moment goes out at once. A single
//!   change is as immediate as it ever was; only a burst is thinned.
//! - **coalesced**: while a socket is inside [`STATE_GAP`] of its last state
//!   message the newest state (by `rev`) replaces whatever was held.
//! - **trailing edge**: the held one goes out when the gap is up, so the value
//!   a drag *ended* on always arrives and no browser rests on a stale one.
//!
//! At [`STATE_GAP`] that is 20 messages a second - invisible to a human, and
//! a third of what a drag used to cost.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

use crate::api::StateEvent;
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
/// The shortest time between two state messages on one socket (card 196):
/// 20 a second.
///
/// A human cannot see a slider redrawn faster than this, and a drag - the only
/// thing that produces state changes in bursts - is the one case being thinned.
/// It is a *gap*, not a budget: nothing is held longer than this, so the cost
/// of the cap is at most `STATE_GAP` of latency on the second, third and
/// following changes of a burst, and none at all on the first.
const STATE_GAP: Duration = Duration::from_millis(50);
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

/// One socket's state pacing: at most one state message every [`STATE_GAP`],
/// and the one that goes is always the newest (card 196).
///
/// Unlike [`Pace`] this **holds** rather than drops, because the last change of
/// a burst is the value the other browser is left resting on. See the module
/// docs for the three edges.
#[derive(Default)]
struct Gate {
    /// When a state message was last sent on this socket. `None` until the
    /// first one, so the first change of all is immediate.
    last_at: Option<Instant>,
    /// The newest state this socket has not been told about yet.
    held: Option<StateEvent>,
}

impl Gate {
    /// A change to send now, or `None` when it is held for the trailing edge.
    fn offer(&mut self, ev: StateEvent, now: Instant) -> Option<StateEvent> {
        if self.last_at.is_none_or(|t| now.duration_since(t) >= STATE_GAP) {
            self.last_at = Some(now);
            self.held = None;
            return Some(ev);
        }
        // Newest wins, by `rev`: the broadcast delivers in order, but a resync
        // after a lag is made on the spot and may not be.
        if self.held.as_ref().is_none_or(|h| h.rev <= ev.rev) {
            self.held = Some(ev);
        }
        None
    }

    /// This socket's **own** change, which it is never told about - but which
    /// does make anything held for it stale, since the held message carries a
    /// whole state and would put this browser's own control back where it was.
    /// So the state goes on, and whose change it was does not.
    fn skip_own(&mut self, ev: StateEvent) {
        if let Some(held) = &mut self.held {
            if held.rev <= ev.rev {
                held.rev = ev.rev;
                held.state = ev.state;
            }
        }
    }

    /// When the held change is due, if one is held.
    fn due(&self) -> Option<Instant> {
        self.held.as_ref()?;
        Some(self.last_at.map_or_else(Instant::now, |t| t + STATE_GAP))
    }

    /// The held change, once it is due.
    fn release(&mut self, now: Instant) -> Option<StateEvent> {
        let ev = self.held.take()?;
        self.last_at = Some(now);
        Some(ev)
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
    let mut gate = Gate::default();
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
                Ok(ev) if ev.from.is_some() && ev.from == me => {
                    gate.skip_own(ev);
                    None
                }
                Ok(ev) => gate.offer(ev, Instant::now()).as_ref().and_then(as_text),
                // Too far behind to know what it missed: give it the truth.
                Err(RecvError::Lagged(_)) => {
                    let ev = st.state_event(None, st.page_state());
                    gate.offer(ev, Instant::now()).as_ref().and_then(as_text)
                }
                Err(RecvError::Closed) => break,
            },
            // The trailing edge: what a burst of changes ended on, once this
            // socket's gap is up. Nothing is scheduled when nothing is held.
            () = async {
                match gate.due() {
                    Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                    None => std::future::pending().await,
                }
            } => gate.release(Instant::now()).as_ref().and_then(as_text),
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
