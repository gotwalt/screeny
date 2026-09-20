//! What the page is told: the frame packet, the studio's state, and the piece
//! list a browser draws itself from.
//!
//! This was `engine.rs` until card 170. There is no design-view engine any
//! more - the player for the attached panel is the only thing that renders,
//! and the page is a window onto it - so what is left here is the *vocabulary*
//! the browser and the server share, and nothing that runs.
//!
//! The frame packet is unchanged from card 105, because the browser's
//! `ui/main.js` reads it byte by byte: a fixed [`HEADER`] of counters and then
//! `N` sRGB triples, which are the **decoded datagram** - what the panel will
//! actually put up, not a copy of the framebuffer.

use screeny_art::pipeline::Stats;
use screeny_art::{pieces, Settings, N};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::watch;

// Card 172 removed `RATES`, the two rates the page used to offer. A player may
// be at any rate in `player::MIN_FPS..=player::MAX_FPS` - the soak uses 10, 15
// and 24 - and a control that can only say 30 or 60 cannot show where a panel
// actually is. The page's control is now a slider over the player's whole
// range, with detents at the rates worth reaching for, and `set_playback`
// clamps instead of ignoring.

/// Frame packet header size; see [`pack`] and `ui/main.js`.
pub const HEADER: usize = 52;
/// Bytes in one frame packet: the header and then `N` sRGB triples.
pub const PACKET_BYTES: usize = HEADER + N * 3;

/// One frame packet: the counters the meters are drawn from, then the picture.
///
/// `preview` is `N * 3` sRGB bytes straight out of the pipeline, so what the
/// browser draws and what the panel shows are the same bytes by construction.
#[must_use]
pub fn pack(seq: u32, t: f64, stats: &Stats, fps: f32, preview: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(PACKET_BYTES);
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&(t as f32).to_le_bytes());
    p.extend_from_slice(&stats.distinct_colours.to_le_bytes());
    p.extend_from_slice(&stats.encoded_bytes.to_le_bytes());
    p.extend_from_slice(&u32::from(stats.codec).to_le_bytes());
    p.extend_from_slice(&u32::from(stats.exact).to_le_bytes());
    for v in [stats.apl, stats.apl_in, stats.luma, stats.dluma, stats.dluma_peak, stats.limiter_gain, fps] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    debug_assert_eq!(p.len(), HEADER);
    p.extend_from_slice(preview);
    p
}

/// A packet for a player that has not rendered anything yet, so a browser
/// connecting during the first frame is handed a black panel rather than
/// nothing at all.
#[must_use]
pub fn blank_packet() -> Vec<u8> {
    vec![0; PACKET_BYTES]
}

/// The page's one frame cell: what the browsers are looking at.
///
/// **One slot, newest wins.** The focused player writes into it and never
/// waits for a reader, so a browser that has stopped reading misses frames and
/// costs nothing - it cannot slow the render loop or the panel link down.
/// That property is card 105's and the whole server rests on it.
///
/// It is also how a player knows whether anybody is looking: `watchers` is the
/// number of open preview sockets, and a player whose panel is away and whose
/// page nobody has open drops to `player::IDLE_FPS`.
pub struct Screen {
    frames: watch::Sender<Arc<Vec<u8>>>,
}

impl Screen {
    #[must_use]
    pub fn new() -> Arc<Screen> {
        Arc::new(Screen { frames: watch::Sender::new(Arc::new(blank_packet())) })
    }

    /// Replace the newest frame. Never blocks, never fails.
    pub fn show(&self, packet: Vec<u8>) {
        self.frames.send_replace(Arc::new(packet));
    }

    /// The newest frame, for `GET /api/v1/frame` and for a socket's first send.
    #[must_use]
    pub fn newest(&self) -> Arc<Vec<u8>> {
        self.frames.borrow().clone()
    }

    #[must_use]
    pub fn watch(&self) -> watch::Receiver<Arc<Vec<u8>>> {
        self.frames.subscribe()
    }

    /// How many browsers are reading.
    #[must_use]
    pub fn watchers(&self) -> usize {
        self.frames.receiver_count()
    }
}

/// A seed nobody chose. Small enough to read out loud and type back in.
#[must_use]
pub fn fresh_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.subsec_nanos() ^ d.as_secs() as u32)
        % 1_000_000
}

#[derive(Clone, Serialize)]
pub struct ParamInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
    /// Card 163: the name of each stop, for a parameter whose values are a
    /// list rather than a range. Empty for an ordinary number, and then the
    /// page draws the slider it always did. **The value is still an `f32`**
    /// on the wire, in the state file and in the per-piece memory; this only
    /// changes which control is drawn and what it says.
    pub choices: &'static [&'static str],
    /// This one is off or on. Drawn as a switch.
    pub switch: bool,
}

#[derive(Clone, Serialize)]
pub struct PieceInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub params: Vec<ParamInfo>,
    /// Card 145: this piece cannot draw without a graphics adapter. With
    /// [`Bootstrap::gpu`] saying there is none, the page marks it unavailable
    /// rather than letting it be picked and render black.
    pub needs_gpu: bool,
}

/// What the page is showing, which is what the panel is showing.
///
/// Unchanged in shape from card 105 apart from two additive fields, so the
/// browser's `sync()` and every script that reads `/api/v1/bootstrap` keep
/// working: `piece`, `seed`, `params`, `settings`, `paused`, `speed`, `fps`.
///
/// `params` holds **every** parameter of the piece at its effective value -
/// the piece's defaults with whatever has been set over the top - because the
/// page draws one slider per parameter and reads its position from here.
#[derive(Clone, Debug, Serialize)]
pub struct StudioState {
    pub piece: String,
    pub seed: u32,
    pub params: std::collections::BTreeMap<String, f32>,
    pub settings: Settings,
    pub paused: bool,
    pub speed: f64,
    pub fps: f64,
    /// Card 170: whether the panel is being driven. False means the link is
    /// released and the panel is on its own idle screen; the page carries on
    /// showing the piece.
    pub on: bool,
    /// Which panel this is, or empty while no panel is attached.
    pub device: String,
}

/// Everything the UI needs to draw itself once.
#[derive(Serialize)]
pub struct Bootstrap {
    pub pieces: Vec<PieceInfo>,
    pub payload_bytes: u32,
    pub state: StudioState,
    /// Card 145: whether this process has a graphics adapter, so the page can
    /// say why the GPU pieces are not available instead of showing black.
    pub gpu: screeny_art::GpuStatus,
}

/// Every piece this build offers, with its parameters.
#[must_use]
pub fn pieces(faults: bool) -> Vec<PieceInfo> {
    let extra = if faults { crate::player::FAULT_PIECES } else { &[] };
    pieces::ALL
        .iter()
        .chain(extra.iter())
        .map(|d| PieceInfo {
            id: d.id,
            name: d.name,
            blurb: d.blurb,
            needs_gpu: pieces::needs_gpu(d.id),
            params: d
                .params
                .iter()
                .map(|p| ParamInfo {
                    id: p.id,
                    label: p.label,
                    min: p.min,
                    max: p.max,
                    step: p.step,
                    default: p.default,
                    choices: p.choices,
                    switch: p.switch,
                })
                .collect(),
        })
        .collect()
}
