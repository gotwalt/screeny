//! What the device answers.
//!
//! Every type here carries a `MAX_JSON_LEN`: an upper bound on its serialised
//! length, proved by a worst-case test in `tests/sizes.rs`. See the crate docs
//! for what that number is and is not good for.
//!
//! **No reply type has a field for the PSK**, on any route, in any state.
//! That is spec section 8.4, and `tests/no_psk.rs` serialises a worst-case
//! value of every one of these and greps the bytes.

use heapless::Vec;
use screeny_proto::control::Telemetry;
use screeny_provision::machine::{Trial, TrialOutcome};
use serde::{Deserialize, Serialize};

use crate::enums::{
    Accepted, FailReason, FirmwareError, FwSlot, FwState, IdleMode, ResetReason, RevertReason,
    StreamState, UpdateOutcome, WifiState,
};
use crate::text::{
    ipv4_text, ssid_text, FwText, IdText, IpText, NameText, PanicFileText, SsidText, MAX_FW_LEN,
    MAX_ID_LEN, MAX_IP_LEN, MAX_NAME_LEN, MAX_PANIC_FILE_LEN, MAX_SSID_LEN,
};
use crate::{ESCAPE_MAX, MAX_U32_LEN, MAX_U8_LEN, MIN_I8_LEN};

/// At most this many networks are reported by `GET /api/v1/networks`.
///
/// Why 16, and not "all of them": the list is a `heapless::Vec` of
/// [`Network`], which is 36 bytes each, so the cap is what the reply costs in
/// main RAM - the pool `docs/design/device-web.md` says breaks first. Sixteen
/// is 576 bytes of state and about 3.7 KB of worst-case JSON. It is also more
/// than anybody scrolls on a phone in a captive-portal sheet, and the list is
/// strongest-first, so the sixteen that survive are the sixteen worth joining.
/// A scan in a block of flats really does find forty.
pub const MAX_NETWORKS: usize = 16;

// ---------------------------------------------------------------------------
// GET /api/v1/status
// ---------------------------------------------------------------------------

/// What the device's panic breadcrumb says about the last panic (card 243).
///
/// The device keeps this in RTC memory, which survives a reset and is cleared
/// only by losing power - so `last_panic` is "since this device was last
/// unplugged", not "since this boot". A panic prints a full backtrace on the
/// serial port and then reboots; this is the part of that event a reader who
/// was not attached to the UART can still see, over HTTP, minutes or days
/// later.
///
/// It is deliberately small. `file` is a base name and `line` a line number,
/// which is enough to say *which* bug; the backtrace is where the exact answer
/// lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanicRecord {
    /// Uptime in milliseconds when it panicked.
    pub uptime_ms: u32,
    /// Which boot panicked. `1` is the first boot after power-on, and
    /// [`PanicReply::boot_count`] is the boot answering now, so
    /// `boot_count - boot` is how many boots ago it was.
    pub boot: u32,
    /// The base name of the source file, e.g. `"net.rs"`. Never empty: a panic
    /// with no location on record reads `"?"`.
    pub file: PanicFileText,
    /// The line within it, or `0` when the panic carried no location.
    pub line: u32,
    /// How many panics in a row, each within a minute of a boot, this one
    /// made. `1` is an isolated panic; a device that reaches the firmware's
    /// crash-loop limit stops rebooting and says so on the panel.
    pub consecutive: u32,
}

impl PanicRecord {
    pub(crate) const MAX_JSON_LEN: usize = 1
        + field("uptime_ms", MAX_U32_LEN)
        + field("boot", MAX_U32_LEN)
        + field("file", 2 + MAX_PANIC_FILE_LEN * ESCAPE_MAX)
        + field("line", MAX_U32_LEN)
        + field("consecutive", MAX_U32_LEN);
}

/// `GET /api/v1/status`: everything the status page and the Studio's device
/// view show, in one request.
///
/// This is research 007 section 7's list plus card 226's additions: `api`,
/// `fw_slot`, `fw_state`, `reset_reason`, `stack_free` and `store_errors`.
/// `slot` from 007 is `fw_slot` here, so that all four firmware-health fields
/// sort together and none of them is just "slot".
///
/// **Card 243's RTC breadcrumb is deliberately not in here**; it is
/// [`PanicReply`] on `GET /api/v1/panic`. The card's first shape put it in
/// this struct and the bench measured what that cost: **44 bytes added to this
/// type cost 3,488 bytes of core 0's stack**, because the firmware moves one of
/// these through picoserve's response chain many times over, in one inlined
/// async frame (the card's Log has the numbers and the ablations).
///
/// So this struct is **full**, and the rule that follows is worth more than the
/// fields it is about: *nothing goes in here that is not needed on every poll.*
/// The Studio reads it every 10 s and the page every 4; a fact that is constant
/// for the life of a boot - which is exactly what a panic breadcrumb is -
/// belongs on a route of its own, where it is read once per `boot_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReply {
    /// The API version, always [`crate::API_VERSION`]. First field, so a
    /// reader that does not recognise it can stop.
    pub api: u8,
    /// The stable short device id (the MAC suffix), `"c0ffee"`.
    pub id: IdText,
    /// The friendly name. Empty means "the device is called `screeny-<id>`".
    pub name: NameText,
    /// The firmware version.
    pub fw: FwText,
    /// A random number drawn once at boot. Same id as last time: the link
    /// flapped. Different id: the device rebooted. Random rather than a
    /// persisted counter so that booting costs no flash write.
    pub boot_id: u32,
    /// Milliseconds since boot. Wraps at 49.7 days, like telemetry's.
    pub uptime_ms: u32,
    /// Heap bytes in use.
    pub heap_used: u32,
    /// Heap bytes total.
    pub heap_size: u32,
    /// Bytes of core 0 stack never touched. The one number that says whether
    /// the next feature fits (`docs/design/device-web.md`, the RAM table).
    pub stack_free: u32,
    /// Last beacon RSSI, dBm.
    pub rssi_dbm: i8,
    /// Brightness currently applied, after the firmware cap.
    pub brightness: u8,
    /// What the panel does when nothing is streaming.
    pub idle_mode: IdleMode,
    /// The station's join state.
    pub wifi_state: WifiState,
    /// The SSID the station is configured for, or `null` when there is none -
    /// or when it is not UTF-8, see [`crate::text::ssid_text`]. **Never the
    /// PSK** (spec 8.4).
    pub ssid: Option<SsidText>,
    /// The station's address, or `null`.
    pub ip: Option<IpText>,
    /// The stream state.
    pub state: StreamState,
    /// True while the setup AP is up.
    pub portal: bool,
    /// Which app slot is running.
    pub fw_slot: FwSlot,
    /// The running slot's `otadata` state.
    pub fw_state: FwState,
    /// Why the chip last restarted.
    pub reset_reason: ResetReason,
    /// Settings-store errors since boot. Non-zero means the `screeny`
    /// partition is unhappy and the status page should say so.
    pub store_errors: u32,
}

impl StatusReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("api", MAX_U8_LEN)
        + field("id", 2 + MAX_ID_LEN * ESCAPE_MAX)
        + field("name", 2 + MAX_NAME_LEN * ESCAPE_MAX)
        + field("fw", 2 + MAX_FW_LEN * ESCAPE_MAX)
        + field("boot_id", MAX_U32_LEN)
        + field("uptime_ms", MAX_U32_LEN)
        + field("heap_used", MAX_U32_LEN)
        + field("heap_size", MAX_U32_LEN)
        + field("stack_free", MAX_U32_LEN)
        + field("rssi_dbm", MIN_I8_LEN)
        + field("brightness", MAX_U8_LEN)
        + field("idle_mode", IdleMode::MAX_JSON_LEN)
        + field("wifi_state", WifiState::MAX_JSON_LEN)
        + field("ssid", 2 + MAX_SSID_LEN * ESCAPE_MAX)
        + field("ip", 2 + MAX_IP_LEN * ESCAPE_MAX)
        + field("state", StreamState::MAX_JSON_LEN)
        + field("portal", "false".len())
        + field("fw_slot", FwSlot::MAX_JSON_LEN)
        + field("fw_state", FwState::MAX_JSON_LEN)
        + field("reset_reason", ResetReason::MAX_JSON_LEN)
        + field("store_errors", MAX_U32_LEN)

;
}

// ---------------------------------------------------------------------------
// GET /api/v1/panic
// ---------------------------------------------------------------------------

/// `GET /api/v1/panic`: the device's RTC breadcrumb in full.
///
/// Its own route rather than three more fields on [`StatusReply`], and the
/// reason is measured rather than tidy: see [`StatusReply`]'s docs. It is also
/// the right shape for what this is - the answer **cannot change while the
/// device is running**, because a panic reboots it, so a reader asks once per
/// `boot_id` instead of every poll.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PanicReply {
    /// Boots since the device last lost power, including this one. Same value
    /// as [`StatusReply::boot_count`].
    pub boot_count: u32,
    /// Panics recorded since the device last lost power.
    pub panic_count: u32,
    /// The last one, or `null` when there has been none - the normal answer.
    pub last_panic: Option<PanicRecord>,
    /// What became of the last firmware update (card 241), or `null` when this
    /// device has not activated one since it was last flashed over serial.
    ///
    /// It shares this route rather than [`StatusReply`] for the same two
    /// reasons the breadcrumb does: `status` is polled every few seconds and is
    /// full, and this is a fact that **cannot change while the device runs**
    /// except once, when an image on trial confirms itself. A reader asks once
    /// per `boot_id`, and again if it saw `"trial"`.
    ///
    /// `#[serde(default)]`, so a Studio built from this commit can still read
    /// firmware 0.6.x, which does not send it.
    #[serde(default)]
    pub update: Option<UpdateRecord>,
    /// Why **this** boot started, the same value as
    /// [`StatusReply::reset_reason`] (card 241b).
    ///
    /// It is here as well as there because this is the route for "why am I
    /// running", and a reader that has just seen `boot_count` go up should not
    /// have to poll a second route to learn whether the device chose to
    /// restart or was made to. `wdt` here means the firmware's liveness
    /// watchdog fired: core 0 stopped scheduling tasks and the chip reset
    /// itself rather than going quiet until somebody unplugged it.
    ///
    /// `null` on a device that does not keep the record.
    #[serde(default)]
    pub last_reset: Option<ResetReason>,
}

impl PanicReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("boot_count", MAX_U32_LEN)
        + field("panic_count", MAX_U32_LEN)
        + field("last_panic", PanicRecord::MAX_JSON_LEN)
        + field("update", UpdateRecord::MAX_JSON_LEN)
        + field("last_reset", ResetReason::MAX_JSON_LEN);
}

/// What became of the last firmware update this device activated (card 241).
///
/// Every field is read back out of flash - `otadata` for the first two, the
/// slot's own `esp_app_desc` for the last - so the answer survives a power
/// cycle and is still true days later. There is nothing here that a device
/// which was reverted and then unplugged would have forgotten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRecord {
    /// Trial, confirmed, or rolled back.
    pub outcome: UpdateOutcome,
    /// Why it was rolled back; `null` unless `outcome` is `reverted`.
    pub reason: Option<RevertReason>,
    /// The slot the update was installed into - the one now running for a
    /// `trial` or a `confirmed` one, and the one **not** running for a
    /// `reverted` one.
    pub slot: FwSlot,
    /// `esp_app_desc.version` of the image in that slot: the version this
    /// update was trying to install. `null` when the slot holds nothing
    /// readable, which is what an update reverted before its image finished
    /// staging looks like.
    pub version: Option<FwText>,
}

impl UpdateRecord {
    pub(crate) const MAX_JSON_LEN: usize = 1
        + field("outcome", UpdateOutcome::MAX_JSON_LEN)
        + field("reason", RevertReason::MAX_JSON_LEN)
        + field("slot", FwSlot::MAX_JSON_LEN)
        + field("version", 2 + MAX_FW_LEN * ESCAPE_MAX);
}

// ---------------------------------------------------------------------------
// GET /api/v1/telemetry
// ---------------------------------------------------------------------------

/// `GET /api/v1/telemetry`: the 48 bytes of spec section 6.7, as named fields.
///
/// **The numbers, not an interpretation of them.** Research 007 section 7 asks
/// for this route so "a browser and a UDP sender see the same numbers", so
/// `state` and `last_codec` are the raw bytes here, even though
/// [`StatusReply::state`] spells the same byte as a word. Telemetry is the
/// projection of the wire struct; status is the human view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TelemetryReply {
    /// Milliseconds since boot.
    pub uptime_ms: u32,
    /// `FRAME` datagrams accepted from the active source.
    pub frames_rx: u32,
    /// Frames actually pushed to the panel.
    pub frames_shown: u32,
    /// `seq` not newer than `last_seq`.
    pub frames_dropped_stale: u32,
    /// A newer frame arrived before this one was displayed.
    pub frames_dropped_superseded: u32,
    /// Decode failed, unknown codec, or awaiting a `KEY` frame.
    pub frames_dropped_decode: u32,
    /// Bad magic/version/length, or not the active source.
    pub frames_rejected: u32,
    /// Total count of sequence numbers never seen.
    pub seq_gaps: u32,
    /// EWMA of per-frame inter-arrival time.
    pub interarrival_us: u16,
    /// EWMA of `|d_i - interarrival_us|`.
    pub jitter_us: u16,
    /// Max inter-arrival since reset.
    pub interarrival_max_us: u16,
    /// EWMA of decode time.
    pub decode_us: u16,
    /// Max decode time since reset.
    pub decode_us_max: u16,
    /// Max time to push a decoded frame to the panel buffer.
    pub render_us_max: u16,
    /// Last beacon RSSI, dBm.
    pub rssi_dbm: i8,
    /// Brightness currently applied.
    pub brightness: u8,
    /// Stream state; the raw byte, see the type docs.
    pub state: u8,
    /// Codec id of the last frame shown.
    pub last_codec: u8,
}

impl TelemetryReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("uptime_ms", MAX_U32_LEN)
        + field("frames_rx", MAX_U32_LEN)
        + field("frames_shown", MAX_U32_LEN)
        + field("frames_dropped_stale", MAX_U32_LEN)
        + field("frames_dropped_superseded", MAX_U32_LEN)
        + field("frames_dropped_decode", MAX_U32_LEN)
        + field("frames_rejected", MAX_U32_LEN)
        + field("seq_gaps", MAX_U32_LEN)
        + field("interarrival_us", crate::MAX_U16_LEN)
        + field("jitter_us", crate::MAX_U16_LEN)
        + field("interarrival_max_us", crate::MAX_U16_LEN)
        + field("decode_us", crate::MAX_U16_LEN)
        + field("decode_us_max", crate::MAX_U16_LEN)
        + field("render_us_max", crate::MAX_U16_LEN)
        + field("rssi_dbm", MIN_I8_LEN)
        + field("brightness", MAX_U8_LEN)
        + field("state", MAX_U8_LEN)
        + field("last_codec", MAX_U8_LEN);
}

/// "A browser and a UDP sender see the same numbers" as code, not a promise.
impl From<&Telemetry> for TelemetryReply {
    fn from(t: &Telemetry) -> Self {
        Self {
            uptime_ms: t.uptime_ms,
            frames_rx: t.frames_rx,
            frames_shown: t.frames_shown,
            frames_dropped_stale: t.frames_dropped_stale,
            frames_dropped_superseded: t.frames_dropped_superseded,
            frames_dropped_decode: t.frames_dropped_decode,
            frames_rejected: t.frames_rejected,
            seq_gaps: t.seq_gaps,
            interarrival_us: t.interarrival_us,
            jitter_us: t.jitter_us,
            interarrival_max_us: t.interarrival_max_us,
            decode_us: t.decode_us,
            decode_us_max: t.decode_us_max,
            render_us_max: t.render_us_max,
            rssi_dbm: t.rssi_dbm,
            brightness: t.brightness,
            state: t.state,
            last_codec: t.last_codec,
        }
    }
}

impl From<Telemetry> for TelemetryReply {
    fn from(t: Telemetry) -> Self {
        Self::from(&t)
    }
}

impl From<&TelemetryReply> for Telemetry {
    fn from(r: &TelemetryReply) -> Self {
        Telemetry {
            uptime_ms: r.uptime_ms,
            frames_rx: r.frames_rx,
            frames_shown: r.frames_shown,
            frames_dropped_stale: r.frames_dropped_stale,
            frames_dropped_superseded: r.frames_dropped_superseded,
            frames_dropped_decode: r.frames_dropped_decode,
            frames_rejected: r.frames_rejected,
            seq_gaps: r.seq_gaps,
            interarrival_us: r.interarrival_us,
            jitter_us: r.jitter_us,
            interarrival_max_us: r.interarrival_max_us,
            decode_us: r.decode_us,
            decode_us_max: r.decode_us_max,
            render_us_max: r.render_us_max,
            rssi_dbm: r.rssi_dbm,
            brightness: r.brightness,
            state: r.state,
            last_codec: r.last_codec,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /api/v1/networks
// ---------------------------------------------------------------------------

/// One scan result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    /// The network name. Always present and always UTF-8: a scan result whose
    /// SSID is not UTF-8 is left out of the list rather than shown wrongly.
    pub ssid: SsidText,
    /// Beacon RSSI, dBm.
    pub rssi: i8,
    /// True unless the network is open. The portal page needs this to know
    /// whether to show a password box.
    pub secure: bool,
}

impl Network {
    pub(crate) const MAX_JSON_LEN: usize = 1
        + field("ssid", 2 + MAX_SSID_LEN * ESCAPE_MAX)
        + field("rssi", MIN_I8_LEN)
        + field("secure", "false".len());
}

/// `GET /api/v1/networks`: what a scan found, strongest first, at most
/// [`MAX_NETWORKS`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NetworksReply {
    /// Strongest first. Never longer than [`MAX_NETWORKS`].
    pub networks: Vec<Network, MAX_NETWORKS>,
}

impl NetworksReply {
    /// An upper bound on the serialised length: about 3.7 KB.
    pub const MAX_JSON_LEN: usize = 1
        + "\"networks\":".len()
        + 2 // []
        + MAX_NETWORKS * Network::MAX_JSON_LEN
        + (MAX_NETWORKS - 1) // the commas between entries
        + 1; // }

    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a scan result to the list.
    ///
    /// Keeps the list sorted strongest first and never longer than
    /// [`MAX_NETWORKS`]; when it is full, the weakest entry is dropped to make
    /// room for a stronger one. Returns `false` when the entry was not kept,
    /// which is either "not UTF-8" or "weaker than the sixteen already here".
    ///
    /// Doing the cap, the order and the UTF-8 rule in one place is the point:
    /// the firmware and the simulator both call this, so their lists cannot
    /// differ in which forty-first network they dropped.
    pub fn offer(&mut self, ssid: &[u8], rssi_dbm: i8, secure: bool) -> bool {
        let Some(ssid) = ssid_text(ssid) else {
            return false;
        };
        let at = self
            .networks
            .iter()
            .position(|n| n.rssi < rssi_dbm)
            .unwrap_or(self.networks.len());
        if at >= MAX_NETWORKS {
            return false;
        }
        if self.networks.is_full() {
            self.networks.pop();
        }
        // `at` is <= len and len < MAX_NETWORKS now, so this cannot fail.
        self.networks
            .insert(
                at,
                Network {
                    ssid,
                    rssi: rssi_dbm,
                    secure,
                },
            )
            .is_ok()
    }
}

// ---------------------------------------------------------------------------
// GET /api/v1/wifi
// ---------------------------------------------------------------------------

/// `GET /api/v1/wifi`: what the portal page's full-page reload reads.
///
/// Research 007 section 4.4: the iOS captive mini-browser only re-probes on a
/// full-page navigation, so the portal never polls this with `fetch()`. It is
/// here for a real browser, for `curl` and for the Studio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiReply {
    /// The station's join state.
    pub state: WifiState,
    /// The SSID being tried or joined, or `null`. **Never the PSK.**
    pub ssid: Option<SsidText>,
    /// The address acquired, or `null`.
    pub ip: Option<IpText>,
    /// Why the last attempt failed; `null` unless `state` is `failed`.
    /// Explicitly `null` rather than absent, because 007 section 7 writes it
    /// that way and the page tests for it.
    pub reason: Option<FailReason>,
}

impl WifiReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("state", WifiState::MAX_JSON_LEN)
        + field("ssid", 2 + MAX_SSID_LEN * ESCAPE_MAX)
        + field("ip", 2 + MAX_IP_LEN * ESCAPE_MAX)
        + field("reason", FailReason::MAX_JSON_LEN);

    /// Nothing configured, nothing tried.
    #[must_use]
    pub fn disconnected() -> Self {
        Self {
            state: WifiState::Disconnected,
            ssid: None,
            ip: None,
            reason: None,
        }
    }
}

/// The portal's trial join, as the page reads it.
///
/// [`TrialOutcome::Trying`] is reported as `connecting`, not as a state of its
/// own: from the page's point of view a trial is the station connecting, and
/// card 221 already decided the telemetry state for both is `PROVISIONING`.
impl From<&Trial> for WifiReply {
    fn from(t: &Trial) -> Self {
        let (state, reason) = match t.outcome {
            TrialOutcome::Trying => (WifiState::Connecting, None),
            TrialOutcome::Connected => (WifiState::Connected, None),
            TrialOutcome::Failed(r) => (WifiState::Failed, Some(r.into())),
        };
        Self {
            state,
            ssid: crate::text::text(t.ssid.as_str()),
            ip: t.ip.map(ipv4_text),
            reason,
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/v1/settings
// ---------------------------------------------------------------------------

/// `POST /api/v1/settings`: the values in effect after the request, with the
/// same clamping `SET_BRIGHTNESS` and `SET_NAME` apply.
///
/// The reply is the whole settings state, not an echo of what was sent: a
/// caller that set only `brightness` still learns the name and the idle mode,
/// and a caller whose brightness was capped learns the capped value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsReply {
    /// The friendly name now in effect; empty means `screeny-<id>`.
    pub name: NameText,
    /// Brightness now applied, after the firmware cap.
    pub brightness: u8,
    /// The idle mode now in effect.
    pub idle_mode: IdleMode,
}

impl SettingsReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("name", 2 + MAX_NAME_LEN * ESCAPE_MAX)
        + field("brightness", MAX_U8_LEN)
        + field("idle_mode", IdleMode::MAX_JSON_LEN);
}

// ---------------------------------------------------------------------------
// POST /api/v1/firmware
// ---------------------------------------------------------------------------

/// `POST /api/v1/firmware`: how the upload went.
///
/// `written` is how many bytes reached the sink, so a truncated upload can say
/// where it stopped. `error` is absent on success.
///
/// # What `ok: true` promises
///
/// **Every check the implementation claims to run, ran and passed** - not
/// "some of them". Research 006 names five: the `0xE9` image magic, the chip
/// in the header, the app descriptor's project name, the segment checksum and
/// the appended SHA-256, and [`FirmwareError`] has an arm for each. There is
/// deliberately no `checks_run` field and no partial `ok`: a caller that has
/// to ask which checks ran cannot act on the answer, and an `ok` that means
/// different things on different builds is worse than no `ok` at all.
///
/// So a build that cannot run all five - card 224's simulator, and the
/// firmware before card 240 - must **not** answer `ok: true` for an image it
/// only partly checked. It answers with this crate's
/// [`ErrorCode::Unavailable`](crate::ErrorCode::Unavailable) instead, and says
/// in its `detail` which checks it is missing. Refusing an upload it cannot
/// vouch for costs a developer one clear sentence; accepting one costs a
/// bricked panel on a bench nobody can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareReply {
    /// True only if the whole image arrived and **every check this build runs
    /// at all** passed. See the type's docs: a build that cannot run all five
    /// of research 006's checks answers
    /// [`ErrorCode::Unavailable`](crate::ErrorCode::Unavailable) rather than
    /// claiming a qualified success here.
    pub ok: bool,
    /// Bytes written to the inactive slot.
    pub written: u32,
    /// Why it failed. Absent when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FirmwareError>,
    /// The device has accepted the image **and will reboot into it** (card
    /// 241): expect the connection to go away within a couple of seconds and
    /// the device to be back in about twenty.
    ///
    /// False for a refused upload, and false for an accepted one that was sent
    /// with `?activate=0`, which stages the image and changes nothing about
    /// what boots.
    ///
    /// It is `true` when the device has *decided* to activate, not when it
    /// has: the `otadata` write happens after this reply is on the wire, on
    /// purpose (a flash write inside an HTTP handler is what card 227 found
    /// costing 4 KB of core 0's stack). A device that loses power in between
    /// comes back running what it was running before, and the upload is simply
    /// repeated.
    ///
    /// `#[serde(default)]`, so a reader built from this commit can still parse
    /// firmware 0.6.x's reply, which never activates anything.
    #[serde(default)]
    pub activating: bool,
}

impl FirmwareReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("ok", "false".len())
        + field("written", MAX_U32_LEN)
        + field("error", FirmwareError::MAX_JSON_LEN)
        + field("activating", "false".len());

    /// A successful upload of `written` bytes that **changed nothing about
    /// what boots**: the image is staged in the inactive slot and that is all.
    #[must_use]
    pub const fn ok(written: u32) -> Self {
        Self {
            ok: true,
            written,
            error: None,
            activating: false,
        }
    }

    /// A successful upload the device is about to reboot into.
    #[must_use]
    pub const fn activating(written: u32) -> Self {
        Self {
            ok: true,
            written,
            error: None,
            activating: true,
        }
    }

    /// A failed upload, having written `written` bytes.
    #[must_use]
    pub const fn failed(written: u32, error: FirmwareError) -> Self {
        Self {
            ok: false,
            written,
            error: Some(error),
            activating: false,
        }
    }
}

// ---------------------------------------------------------------------------
// The mutating routes that just say "yes"
// ---------------------------------------------------------------------------

/// `{"result":"trying"}` and its two siblings: the reply a mutating route
/// sends **before** doing the thing, because doing the thing may drop the
/// connection that would have carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedReply {
    /// What is about to happen.
    pub result: Accepted,
}

impl AcceptedReply {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1 + field("result", Accepted::MAX_JSON_LEN);

    /// `{"result":"trying"}`, for `POST /api/v1/wifi`.
    pub const TRYING: Self = Self {
        result: Accepted::Trying,
    };
    /// `{"result":"rebooting"}`, for `POST /api/v1/reboot`.
    pub const REBOOTING: Self = Self {
        result: Accepted::Rebooting,
    };
    /// `{"result":"identifying"}`, for `POST /api/v1/identify`.
    pub const IDENTIFYING: Self = Self {
        result: Accepted::Identifying,
    };
}

/// `"key":value` plus the comma or brace in front of it.
///
/// Every `MAX_JSON_LEN` in this crate is a sum of these, so adding a field to
/// a struct without extending its bound is a compile-time-visible omission and
/// a test failure, not a silently truncated reply.
const fn field(key: &str, value_max: usize) -> usize {
    1 + key.len() + 2 + 1 + value_max // , " key " : value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_round_trips_through_the_wire_struct() {
        let t = Telemetry {
            uptime_ms: 1,
            frames_rx: 2,
            frames_shown: 3,
            frames_dropped_stale: 4,
            frames_dropped_superseded: 5,
            frames_dropped_decode: 6,
            frames_rejected: 7,
            seq_gaps: 8,
            interarrival_us: 9,
            jitter_us: 10,
            interarrival_max_us: 11,
            decode_us: 12,
            decode_us_max: 13,
            render_us_max: 14,
            rssi_dbm: -54,
            brightness: 96,
            state: 1,
            last_codec: 3,
        };
        assert_eq!(Telemetry::from(&TelemetryReply::from(&t)), t);
    }

    #[test]
    fn networks_are_strongest_first_and_capped() {
        let mut r = NetworksReply::new();
        // Offer 40, weakest first, so every one of them displaces something.
        for i in 0..40i8 {
            let rssi = -90 + i;
            assert!(r.offer(b"Example-Wifi1", rssi, true));
        }
        assert_eq!(r.networks.len(), MAX_NETWORKS);
        assert_eq!(r.networks[0].rssi, -51);
        assert_eq!(r.networks[MAX_NETWORKS - 1].rssi, -66);
        for w in r.networks.windows(2) {
            assert!(w[0].rssi >= w[1].rssi);
        }
        // A weaker one, once full, is simply not kept.
        assert!(!r.offer(b"Example-Wifi1", -100, false));
        assert_eq!(r.networks.len(), MAX_NETWORKS);
    }

    #[test]
    fn a_non_utf8_scan_result_is_left_out() {
        let mut r = NetworksReply::new();
        assert!(!r.offer(&[0xff, 0xfe], -40, true));
        assert!(r.networks.is_empty());
    }

    #[test]
    fn a_trial_becomes_the_wifi_reply_the_portal_page_reads() {
        use screeny_provision::machine::FailReason as P;
        let mut t = Trial {
            ssid: crate::text::text("Example-Wifi1").unwrap(),
            outcome: TrialOutcome::Trying,
            ip: None,
            // The reply is the same shape whichever door the post came in by:
            // `origin` decides what the *machine* does next, not what the
            // page is told. Card 232.
            origin: screeny_provision::TrialOrigin::Portal,
        };
        let r = WifiReply::from(&t);
        assert_eq!(r.state, WifiState::Connecting);
        assert_eq!(r.reason, None);
        assert_eq!(r.ssid.as_deref(), Some("Example-Wifi1"));

        t.outcome = TrialOutcome::Connected;
        t.ip = Some([192, 168, 7, 221]);
        let r = WifiReply::from(&t);
        assert_eq!(r.state, WifiState::Connected);
        assert_eq!(r.ip.as_deref(), Some("192.168.7.221"));

        t.outcome = TrialOutcome::Failed(P::AuthError);
        t.ip = None;
        let r = WifiReply::from(&t);
        assert_eq!(r.state, WifiState::Failed);
        assert_eq!(r.reason, Some(FailReason::Auth));
    }
}
