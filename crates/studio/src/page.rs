//! What the page is told: the frame packet, the studio's state, and the patch
//! list a browser draws itself from.
//!
//! This was `engine.rs` until card 170. There is no design-view engine any
//! more - the player for the attached panel is the only thing that renders,
//! and the page is a window onto it - so what is left here is the *vocabulary*
//! the browser and the server share, and nothing that runs.
//!
//! The frame packet is unchanged from card 105, because the browser's
//! `ui/picture.js` reads it byte by byte: a fixed [`HEADER`] of counters and then
//! `N` sRGB triples, which are the **decoded datagram** - what the panel will
//! actually put up, not a copy of the framebuffer.

use screeny_art::pipeline::Stats;
use screeny_art::{patches, Output, N};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

// Card 105 offered two rates here, card 172 replaced them with a slider over
// the player's whole range, and **card 161 removed the control altogether**:
// there is one rate, `screeny_art::FPS`, the panel's own. Nothing on the page
// offers a frame rate and nothing accepts one. What is left is
// [`StudioState::fps`], which reports it.

/// Frame packet header size; see [`pack`] and `ui/picture.js`.
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
/// It is also how a player knows whether anybody is looking: [`Screen::watchers`]
/// is the number of preview sockets **being sent frames**, and a player whose
/// panel is away and whose page nobody is looking at drops to
/// `player::IDLE_FPS`.
///
/// Card 120: "open a socket" and "want frames" came apart, because a tab that
/// has been switched away from asks for none. A hidden tab is therefore *not* a
/// watcher - it must not hold a panel-less studio at the full rate for a phone
/// in a pocket - so the count is kept here by [`Viewer`] rather than read off the
/// `watch` channel's receiver count.
pub struct Screen {
    frames: watch::Sender<Arc<Vec<u8>>>,
    /// Preview sockets open, whether or not they want frames.
    sockets: AtomicUsize,
    /// Of those, the ones being sent frames right now.
    watching: AtomicUsize,
    frames_sent: AtomicU64,
    bytes_sent: AtomicU64,
}

/// What the preview has cost, on `/api/v1/status` as `sockets`.
///
/// The one number card 120 is about: with the studio running for months in a
/// container, `bytes_sent` over a minute is what `docker stats` would otherwise
/// be the only witness to.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct PreviewCost {
    /// Preview sockets open.
    pub open: usize,
    /// Of those, the ones being sent frames. A hidden tab is not one.
    pub watching: usize,
    /// Frame packets sent to browsers since the process started, across every
    /// socket - not frames rendered, which is `preview.ticks`.
    pub frames_sent: u64,
    /// What they weighed, payload only (WebSocket framing is 2-4 bytes more).
    /// Includes the JSON: state changes and the twice-a-second heartbeat.
    pub bytes_sent: u64,
}

impl Screen {
    #[must_use]
    pub fn new() -> Arc<Screen> {
        Arc::new(Screen {
            frames: watch::Sender::new(Arc::new(blank_packet())),
            sockets: AtomicUsize::new(0),
            watching: AtomicUsize::new(0),
            frames_sent: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
        })
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

    /// How many browsers are being sent frames. A tab that has said it is
    /// hidden is not one of them.
    #[must_use]
    pub fn watchers(&self) -> usize {
        self.watching.load(Ordering::Relaxed)
    }

    /// What the preview is costing.
    #[must_use]
    pub fn cost(&self) -> PreviewCost {
        PreviewCost {
            open: self.sockets.load(Ordering::Relaxed),
            watching: self.watching.load(Ordering::Relaxed),
            frames_sent: self.frames_sent.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
        }
    }

    /// Register one preview socket. It counts as a watcher only once it says it
    /// wants frames.
    #[must_use]
    pub fn viewer(self: &Arc<Self>) -> Viewer {
        self.sockets.fetch_add(1, Ordering::Relaxed);
        Viewer { screen: Arc::clone(self), wants: false }
    }
}

/// One preview socket's claim on the picture, and its share of the bill.
///
/// Dropping it gives the claim back, however the socket ended - so a browser
/// that vanishes cannot leave a studio rendering at full rate for ever.
pub struct Viewer {
    screen: Arc<Screen>,
    wants: bool,
}

impl Viewer {
    /// Whether this socket is being sent frames.
    #[must_use]
    pub fn wants_frames(&self) -> bool {
        self.wants
    }

    /// Say whether this socket wants frames. Idempotent.
    pub fn set_wants_frames(&mut self, wants: bool) {
        if wants == self.wants {
            return;
        }
        self.wants = wants;
        if wants {
            self.screen.watching.fetch_add(1, Ordering::Relaxed);
        } else {
            self.screen.watching.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// One message went out: `frame` distinguishes a frame packet from the
    /// JSON, because only the frames are worth pacing.
    pub fn sent(&self, bytes: usize, frame: bool) {
        self.screen.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        if frame {
            self.screen.frames_sent.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.set_wants_frames(false);
        self.screen.sockets.fetch_sub(1, Ordering::Relaxed);
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
    /// on the wire, in the state file and in the per-patch memory; this only
    /// changes which control is drawn and what it says.
    pub choices: &'static [&'static str],
    /// This one is off or on. Drawn as a switch.
    pub switch: bool,
}

#[derive(Clone, Serialize)]
pub struct PatchInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub params: Vec<ParamInfo>,
    /// Card 145: this patch cannot draw without a graphics adapter. With
    /// [`Bootstrap::gpu`] saying there is none, the page marks it unavailable
    /// rather than letting it be picked and render black.
    pub needs_gpu: bool,
    /// Card 151: whether "another one like this" means anything to this patch
    /// (`PatchDef::seeded`). The page shows its one quiet **Another** button
    /// only for a patch that says yes; the seed itself is not shown at all any
    /// more, being an opaque number.
    pub seeded: bool,
}

/// What the page is showing, which is what the panel is showing.
///
/// Unchanged in shape from card 105 apart from two additive fields, so the
/// browser's `sync()` and every script that reads `/api/v1/bootstrap` keep
/// working: `patch`, `seed`, `params`, `output`, `paused`, `speed`, `fps`.
///
/// `params` holds **every** parameter of the patch at its effective value -
/// the patch's defaults with whatever has been set over the top - because the
/// page draws one slider per parameter and reads its position from here.
#[derive(Clone, Debug, Serialize)]
pub struct StudioState {
    pub patch: String,
    pub seed: u32,
    pub params: std::collections::BTreeMap<String, f32>,
    pub output: Output,
    pub paused: bool,
    pub speed: f64,
    /// The rate everything runs at: `screeny_art::FPS`, always (card 161).
    ///
    /// **Reported, never set.** It is still here so the page and any script
    /// can read the rate rather than writing 30 down for themselves - the
    /// limiter meter's per-frame tick is drawn from it - and so that a reader
    /// of `/api/v1/bootstrap` written before this card still finds the field.
    pub fps: f64,
    /// Card 170: whether the panel is being driven. False means the link is
    /// released and the panel is on its own idle screen; the page carries on
    /// showing the patch.
    pub on: bool,
    /// Which panel this is, or empty while no panel is attached.
    pub device: String,
    /// Card 151: the setting the working copy was loaded from. Always a name;
    /// `"Default"` ([`crate::state::DEFAULT_SETTING`]) when it is on the
    /// patch's own.
    pub setting: String,
    /// This patch's saved settings, alphabetically. **Default is not in here**:
    /// it is not stored, it is always there, and the page lists it first of its
    /// own accord.
    pub settings: Vec<String>,
    /// Whether the working copy still *is* [`StudioState::setting`].
    ///
    /// Computed on every read from the values themselves, never stored, so it
    /// cannot be left set by a change that forgot to clear it.
    pub modified: bool,
}

/// Everything the UI needs to draw itself once.
#[derive(Serialize)]
pub struct Bootstrap {
    pub patches: Vec<PatchInfo>,
    pub payload_bytes: u32,
    pub state: StudioState,
    /// Card 145: whether this process has a graphics adapter, so the page can
    /// say why the GPU patches are not available instead of showing black.
    pub gpu: screeny_art::GpuStatus,
}

/// Every patch this build offers, with its parameters.
#[must_use]
pub fn patches(faults: bool) -> Vec<PatchInfo> {
    let extra = if faults { crate::player::FAULT_PATCHES } else { &[] };
    patches::ALL
        .iter()
        .chain(extra.iter())
        .map(|d| PatchInfo {
            id: d.id,
            name: d.name,
            blurb: d.blurb,
            needs_gpu: patches::needs_gpu(d.id),
            seeded: d.seeded,
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
