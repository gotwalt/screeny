//! What the page is told: the frame packet, the studio's state, and the patch
//! list a browser draws itself from.
//!
//! This was `engine.rs` until card 170. There is no design-view engine any
//! more - the channels are the only things that render (card 350),
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

/// A packet for a panel that has not been sent anything yet, so a browser
/// connecting during the first frame is handed a black panel rather than
/// nothing at all.
#[must_use]
pub fn blank_packet() -> Vec<u8> {
    vec![0; PACKET_BYTES]
}

/// One panel's frame cell: what the browsers watching that panel are looking
/// at. Card 350 gave every panel its own, filled by its channel's render
/// thread with the frames **that panel** is sent (after its own pipeline), so
/// "what is on screen is what the device is showing" holds per panel.
///
/// **One slot, newest wins.** The render loop writes into it and never waits
/// for a reader, so a browser that has stopped reading misses frames and costs
/// nothing - it cannot slow the render loop or the panel link down. That
/// property is card 105's and the whole server rests on it.
///
/// It is also how a channel knows whether anybody is looking:
/// [`Screen::watchers`] is the number of preview sockets **being sent
/// frames** from this panel, and a channel none of whose panels is connected
/// or watched drops to `channel::IDLE_FPS`. A hidden tab is not a watcher
/// (card 120).
pub struct Screen {
    frames: watch::Sender<Arc<Vec<u8>>>,
    /// Of the sockets on this panel, the ones being sent frames right now.
    watching: AtomicUsize,
    /// What every preview socket in the studio has cost, shared.
    meter: Arc<SocketMeter>,
}

/// What the preview sockets have cost, across every panel: `sockets` on
/// `/api/v1/status`. One of these per studio, shared by every panel's
/// [`Screen`].
#[derive(Default)]
pub struct SocketMeter {
    /// Preview sockets open, whether or not they want frames.
    sockets: AtomicUsize,
    /// Of those, the ones being sent frames.
    watching: AtomicUsize,
    frames_sent: AtomicU64,
    bytes_sent: AtomicU64,
}

impl SocketMeter {
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

    /// A socket that is not (yet) on any panel's frames - one scoped to a
    /// panel that does not exist - still costs its state messages.
    #[must_use]
    pub fn socket(self: &Arc<Self>) -> SocketClaim {
        self.sockets.fetch_add(1, Ordering::Relaxed);
        SocketClaim { meter: Arc::clone(self) }
    }

    /// One message went out: `frame` distinguishes a frame packet from the
    /// JSON.
    pub fn sent(&self, bytes: usize, frame: bool) {
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        if frame {
            self.frames_sent.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// One open preview socket, counted for as long as it is held.
pub struct SocketClaim {
    meter: Arc<SocketMeter>,
}

impl Drop for SocketClaim {
    fn drop(&mut self) {
        self.meter.sockets.fetch_sub(1, Ordering::Relaxed);
    }
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
    /// socket - not frames rendered.
    pub frames_sent: u64,
    /// What they weighed, payload only (WebSocket framing is 2-4 bytes more).
    /// Includes the JSON: state changes and the twice-a-second heartbeat.
    pub bytes_sent: u64,
}

impl Screen {
    #[must_use]
    pub fn new(meter: Arc<SocketMeter>) -> Arc<Screen> {
        Arc::new(Screen { frames: watch::Sender::new(Arc::new(blank_packet())), watching: AtomicUsize::new(0), meter })
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

    /// How many browsers are being sent this panel's frames. A tab that has
    /// said it is hidden is not one of them.
    #[must_use]
    pub fn watchers(&self) -> usize {
        self.watching.load(Ordering::Relaxed)
    }

    /// Register one preview socket on this panel. It counts as a watcher only
    /// once it says it wants frames.
    #[must_use]
    pub fn viewer(self: &Arc<Self>) -> Viewer {
        Viewer { screen: Arc::clone(self), wants: false }
    }
}

/// One preview socket's claim on a panel's picture.
///
/// Dropping it gives the claim back, however the socket ended - so a browser
/// that vanishes cannot leave a channel rendering at full rate for ever.
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

    /// Which panel's cell this is a claim on.
    #[must_use]
    pub fn screen(&self) -> &Arc<Screen> {
        &self.screen
    }

    /// Say whether this socket wants frames. Idempotent.
    pub fn set_wants_frames(&mut self, wants: bool) {
        if wants == self.wants {
            return;
        }
        self.wants = wants;
        if wants {
            self.screen.watching.fetch_add(1, Ordering::Relaxed);
            self.screen.meter.watching.fetch_add(1, Ordering::Relaxed);
        } else {
            self.screen.watching.fetch_sub(1, Ordering::Relaxed);
            self.screen.meter.watching.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.set_wants_frames(false);
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

/// What one panel is showing (card 350: every panel has one of these; a route
/// or a socket without `panel` is the first panel's).
///
/// Unchanged in shape from card 105 apart from additive fields, so the
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
    /// showing the patch. Since card 353 this is the panel the request named
    /// (`?panel=`), else the channel's first member - and false for a channel
    /// with no panel.
    pub on: bool,
    /// Which panel `on` is about: the one the request named, else the
    /// channel's first member, else empty.
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
    /// Card 353: **the channel this is the state of** - every state is a
    /// channel's now, and never `null`: there is no idle panel.
    pub channel: crate::channel::ChannelId,
    /// Its name ("Channel 1").
    pub channel_name: String,
    /// Every panel on it, by device id, in the order they joined: what an
    /// edit here reaches.
    pub panels: Vec<String>,
    /// `panels` without `device`: *"Also on Kitchen"*. Kept for the page
    /// card 351 built; card 354 reads `panels`.
    pub shared_with: Vec<String>,
}

/// Everything the UI needs to draw itself once.
#[derive(Serialize)]
pub struct Bootstrap {
    pub patches: Vec<PatchInfo>,
    pub payload_bytes: u32,
    /// The panel's state: the first panel's, or `?panel=`'s.
    pub state: StudioState,
    /// Card 145: whether this process has a graphics adapter, so the page can
    /// say why the GPU patches are not available instead of showing black.
    pub gpu: screeny_art::GpuStatus,
    /// Card 187: the brightness control's real stops - `0` for off, then one
    /// value per output-enable slot the panel can light. Built from
    /// [`brightness_stops`] so the page never writes the slot arithmetic
    /// itself; a device's own cap trims the top of this list client-side,
    /// since the stops here know nothing about any one panel.
    pub brightness_stops: Vec<u8>,
}

/// The brightness control's real stops (card 187): `0`, then the lowest
/// `u8` that lights each of [`screeny_panel::MAX_OE_SLOTS`] output-enable
/// slots. The device dims by shortening that window, not by scaling pixel
/// values (`screeny_receiver::clamp_brightness`, spec 6.3), so most of
/// `0..=255` is a value that lands on a picture a neighbour already showed -
/// `oe_slots(129) == oe_slots(130)`. A stepped control should only ever land
/// on a value that changes something, and this is the one place that
/// arithmetic is written for the host side (card 066 has the device's copy,
/// `screeny_panel::oe_slots`).
#[must_use]
pub fn brightness_stops() -> Vec<u8> {
    let mut stops = vec![0u8];
    let mut last_slots = 0;
    for level in 1..=u8::MAX {
        let slots = screeny_panel::model::oe_slots(level);
        if slots > last_slots {
            stops.push(level);
            last_slots = slots;
        }
    }
    stops
}

/// Every patch this build offers, with its parameters.
#[must_use]
pub fn patches(faults: bool) -> Vec<PatchInfo> {
    let extra = if faults { crate::channel::FAULT_PATCHES } else { &[] };
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

#[cfg(test)]
mod tests {
    use super::brightness_stops;

    /// One stop for off, one for each of `MAX_OE_SLOTS` (25) lit slots - the
    /// full real resolution of the control, never 256.
    #[test]
    fn there_is_one_stop_per_real_picture() {
        let stops = brightness_stops();
        assert_eq!(stops.len(), 1 + screeny_panel::model::MAX_OE_SLOTS as usize, "{stops:?}");
        assert_eq!(stops[0], 0, "off is always the first stop");
        assert!(stops.windows(2).all(|w| w[0] < w[1]), "strictly increasing: {stops:?}");
        // The top stop is not necessarily 255: it is the *lowest* level that
        // lights every slot, and 255 itself would only be one more value
        // landing on the same picture (card 187's whole point).
        let top = *stops.last().expect("at least one stop");
        assert_eq!(screeny_panel::model::oe_slots(top), screeny_panel::model::MAX_OE_SLOTS);
        assert!(screeny_panel::model::oe_slots(top - 1) < screeny_panel::model::MAX_OE_SLOTS, "not the lowest: {top}");
    }

    /// The lowest nonzero stop is the floor `screeny_receiver` raises a
    /// nonzero request to (card 136) - pinned against the device's own
    /// constant rather than written down again here.
    #[test]
    fn the_lowest_nonzero_stop_is_the_floor() {
        assert_eq!(brightness_stops()[1], screeny_receiver::BRIGHTNESS_FLOOR);
    }
}
