//! The screeny receive state machine, with no I/O and no wall clock in it.
//!
//! Everything spec sections 3.3, 4.7, 6 and 7 require of a *receiver* lives
//! here, and every method takes the current time as an argument. That makes
//! the whole of a device's behaviour - the source lock, `HOLD_MS`, the three
//! rate limiters - testable in microseconds instead of in real seconds.
//!
//! There is exactly one copy of this logic (card 016). Card 006 wrote it for
//! `crates/sim` and card 008 hand-ported it to `firmware/src/receiver.rs`,
//! with the comment "the two files should be diffed if either changes"; this
//! crate is that sentence made unnecessary. The firmware's shape won the
//! argument, so:
//!
//! * **No allocation anywhere, and no `std`.** An accepted frame's datagram
//!   stays with the caller: [`Receiver::offer_frame`] answers [`Offer::Keep`]
//!   and the caller holds the bytes until [`Receiver::flush_frames`] hands
//!   them back, so nothing is copied and the receiver stores only a flag.
//! * **Fixed-size rate limiters.** Three maps become three four-slot arrays
//!   with oldest-entry eviction ([`Limiter`]). Four is two more than the
//!   protocol needs: only the lock holder and one hopeful can be sending.
//! * **Outgoing datagrams are built on the stack** and handed to
//!   [`Host::send_from_frame_sock`], which is a socket on one side and a
//!   `Vec` on the other.
//!
//! Everything a receiver cannot decide for itself - what the clock reads,
//! where a datagram goes, what happens to a decoded frame, whether this
//! device has a radio - is a method on [`Host`].
//!
//! The one thing this module does *not* decide is when a drain ends. Spec
//! section 3.3 says to drain the socket until it would block and keep the
//! newest acceptable frame, so the caller feeds datagrams with
//! [`Receiver::offer_frame`] and then closes the batch with
//! [`Receiver::flush_frames`], which is the point at which exactly one frame
//! is decoded.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod stats;

pub use stats::{Arrival, Counters, Stats, Timings};

use screeny_proto::control::{
    busy_reason, op, state as tstate, ErrorCode, IdleMode, Reply, Request, SetWifi, Telemetry,
};
use screeny_proto::txt::DeviceInfo;
use screeny_proto::{dec, ControlPacket, DecodeError, FramePacket, Reject, Rgb888Frame};
use screeny_proto::{C_REPLY, MAX_UDP_PAYLOAD, TYPE_CONTROL, VERSION};

/// Longest `GET_INFO` / TXT record this crate will build. The v1 key set with
/// a 32-byte name is about 130 bytes.
pub const INFO_MAX: usize = 224;

/// Largest unsolicited datagram: a 48-byte `TELEMETRY` body plus the header.
pub const OUT_MAX: usize = 64;

/// Longest friendly name. Section 6.3's `SET_NAME` cannot carry more than
/// this, so `SET_NAME` never overflows the buffer; only a startup name from a
/// configuration file can, and that one is truncated.
pub const NAME_MAX: usize = screeny_proto::control::MAX_NAME_LEN;

const _: () = assert!(NAME_MAX == 32);

/// Slots in each rate limiter. See [`Limiter`].
const LIMIT_SLOTS: usize = 4;

/// Sources whose `STATS_REQ` can be pending at once. Only the lock holder can
/// have a frame accepted, so the second slot exists for the one drain in which
/// a takeover happens.
const STATS_REQ_SLOTS: usize = 2;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// The spec section 7.2 and 5.5 constants, overridable so a test does not have
/// to wait ten real seconds to watch `HOLD` expire.
///
/// [`Timing::SPEC`] is a device's behaviour and is the default. Anything else
/// is a test fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// After the last accepted frame, the active source keeps exclusivity
    /// this long.
    pub lock_ms: u32,
    /// No frames for this long and the stream is considered stopped.
    pub stream_timeout_ms: u32,
    /// How long the last frame stays lit after the stream stops.
    pub hold_ms: u32,
    /// Cross-fade duration into the idle screen.
    pub fade_ms: u32,
    /// Minimum gap between `BUSY` packets to one source.
    pub busy_min_interval_ms: u32,
    /// Minimum gap between piggybacked `TELEMETRY` packets to one source.
    pub telemetry_min_interval_ms: u32,
    /// Minimum gap between *new* `GET_INFO` replies to one source address
    /// (spec section 5.5). A retry of a `req_id` already answered inside the
    /// window is exempt.
    pub info_min_interval_ms: u32,
}

impl Timing {
    /// The constants exactly as spec sections 7.2 and 5.5 give them.
    pub const SPEC: Timing = Timing {
        lock_ms: screeny_proto::LOCK_MS,
        stream_timeout_ms: screeny_proto::STREAM_TIMEOUT_MS,
        hold_ms: screeny_proto::HOLD_MS,
        fade_ms: screeny_proto::FADE_MS,
        busy_min_interval_ms: screeny_proto::BUSY_MIN_INTERVAL_MS,
        telemetry_min_interval_ms: screeny_proto::TELEMETRY_MIN_INTERVAL_MS,
        info_min_interval_ms: 1_000,
    };
}

impl Default for Timing {
    fn default() -> Self {
        Timing::SPEC
    }
}

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// The stream state of spec section 7.3, ignoring overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Showing the idle screen. No active source.
    Idle,
    /// Streaming. A source holds the lock.
    Live,
    /// The last frame is still lit, but the lock has been released.
    Hold,
}

impl State {
    /// The telemetry `state` byte, ignoring overlays.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            State::Idle => tstate::IDLE,
            State::Live => tstate::LIVE,
            State::Hold => tstate::HOLD,
        }
    }

    /// The name a log line uses.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "IDLE",
            State::Live => "LIVE",
            State::Hold => "HOLD",
        }
    }
}

/// Why a frame the device did accept never reached the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropCause {
    /// `seq` was not newer than `last_seq`: a duplicate or a reordered frame.
    Stale,
    /// A newer frame arrived in the same drain.
    Superseded,
    /// The payload did not decode, or the codec is not one of the five.
    Decode(DecodeError),
}

/// Why the source lock was released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    /// A frame with `FINAL` set was displayed.
    Final,
    /// The active source sent `RELEASE`.
    Released,
    /// `STREAM_TIMEOUT_MS` passed with no accepted frame.
    Timeout,
    /// Another source took over after `LOCK_MS`.
    TakenOver,
    /// The Wi-Fi link went down under a live stream (section 7.3).
    LinkDown,
}

impl ReleaseReason {
    /// The word a log line uses.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ReleaseReason::Final => "FINAL",
            ReleaseReason::Released => "RELEASE",
            ReleaseReason::Timeout => "stream timeout",
            ReleaseReason::TakenOver => "taken over",
            ReleaseReason::LinkDown => "wifi down",
        }
    }
}

/// What [`Receiver::offer_frame`] wants the caller to do with the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Keep it: it is the newest acceptable frame of this drain so far, and
    /// [`Receiver::flush_frames`] will want it back.
    Keep,
    /// Forget it. It was rejected, stale, or malformed, and counted.
    Drop,
}

/// What the panel is showing, apart from any overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta<A> {
    /// Sequence number of the displayed frame.
    pub seq: u16,
    /// Its codec id.
    pub codec: u8,
    /// Its pixel payload size in bytes, `HAS_TS` prefix excluded.
    pub bytes: usize,
    /// Its `HAS_TS` timestamp, if any.
    pub timestamp_us: Option<u32>,
    /// Who sent it.
    pub from: A,
}

/// What the caller should compose, given the state machine and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Leave the streamed frame alone; the panel already shows it.
    Stream,
    /// Draw the `IDENTIFY` overlay.
    Identify,
    /// Cross-fade from the last streamed frame to the idle target, section
    /// 7.5. `t` is 0..=256.
    Fade {
        /// How far through the fade, 0..=256.
        t: u16,
    },
    /// The idle screen for the mode in force, fully faded in.
    Idle,
}

/// Something the receiver did.
///
/// Borrowed rather than owned so that a `no_std` device pays nothing for the
/// two variants that carry a string; a host that wants to keep them clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a, A> {
    /// A frame was decoded and became the front buffer.
    Shown {
        /// Who sent it.
        from: A,
        /// Its sequence number.
        seq: u16,
        /// Its codec id.
        codec: u8,
        /// Pixel payload bytes, timestamp prefix excluded.
        bytes: usize,
        /// Its `HAS_TS` timestamp, if it carried one.
        timestamp_us: Option<u32>,
    },
    /// A frame was accepted but never displayed.
    Dropped {
        /// Who sent it.
        from: A,
        /// Its sequence number.
        seq: u16,
        /// Why it did not reach the panel.
        cause: DropCause,
    },
    /// A datagram on the frame port was not a frame we would act on.
    Rejected {
        /// Who sent it.
        from: A,
        /// Which of section 2's rules it broke, or `None` when it was a
        /// well-formed frame from a source that does not hold the lock.
        reason: Option<Reject>,
    },
    /// A frame arrived while another source held the lock.
    Busy {
        /// The locked-out source.
        to: A,
        /// Whether a `BUSY` packet was actually sent, or suppressed by
        /// `BUSY_MIN_INTERVAL_MS`.
        sent: bool,
    },
    /// A `TELEMETRY` packet was sent back down the frame port (section 6.4).
    TelemetrySent {
        /// Where to.
        to: A,
    },
    /// A piggybacked telemetry request was suppressed by the 100 ms limit.
    TelemetryRateLimited {
        /// Who asked.
        to: A,
    },
    /// A control request arrived and was answered (or deliberately not).
    Control {
        /// Who sent it.
        from: A,
        /// Its opcode.
        op: u8,
        /// Its `req_id`; 0 means no reply was wanted.
        req_id: u16,
        /// The error code replied with, if the reply was an error.
        error: Option<u8>,
        /// Whether a reply datagram went out.
        replied: bool,
    },
    /// A datagram on the control port was discarded.
    ControlRejected {
        /// Who sent it.
        from: A,
        /// Which of section 2's rules it broke.
        reason: Reject,
    },
    /// The stream state machine moved.
    StateChanged {
        /// Where it was.
        from: State,
        /// Where it is now.
        to: State,
    },
    /// A source took the panel.
    LockTaken {
        /// The new active source.
        source: A,
        /// The source it displaced, if any.
        displaced: Option<A>,
    },
    /// The panel is free again.
    LockReleased {
        /// The source that had it.
        source: A,
        /// Why.
        reason: ReleaseReason,
    },
    /// Brightness was set. Fires even when the value did not change; a host
    /// that only cares about changes compares `applied` with what it has.
    Brightness {
        /// Requested value.
        requested: u8,
        /// Value after the cap.
        applied: u8,
    },
    /// `IDENTIFY` started (or, with 0, stopped).
    Identify {
        /// Overlay duration; 0 stops one in progress.
        duration_ms: u16,
    },
    /// The `IDENTIFY` overlay's time ran out.
    IdentifyExpired,
    /// `SET_IDLE` changed the idle behaviour.
    IdleMode {
        /// The new mode byte.
        mode: u8,
    },
    /// `SET_NAME` changed the friendly name.
    Renamed {
        /// The new name.
        name: &'a str,
    },
    /// `RESET_STATS` zeroed the counters.
    StatsReset,
    /// `REBOOT` arrived, with the right magic.
    Reboot,
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

/// Everything a receiver cannot decide for itself.
///
/// The simulator's implementation pushes into `Vec`s; the firmware's writes to
/// sockets, atomics and the log. Every method but the three required ones has
/// a do-nothing default, because a receiver that ignores all of them is still
/// a conforming receiver.
pub trait Host {
    /// How a source is addressed: a `SocketAddr` on a host, an `IpEndpoint` on
    /// the device.
    type Addr: Copy + PartialEq + Eq;
    /// The address without the port. Spec section 7.4 matches `RELEASE` and
    /// section 5.5 rate-limits `GET_INFO` on this, not on the endpoint.
    type Ip: Copy + PartialEq + Eq;

    /// Drop the port.
    fn ip_of(a: Self::Addr) -> Self::Ip;

    /// A monotonic microsecond clock. Only ever read twice in a row and
    /// subtracted, so its epoch does not matter.
    fn micros(&self) -> u64;

    /// Send one unsolicited datagram **from the frame socket**. Spec section
    /// 6.4: a piggybacked `TELEMETRY`, and by the same rule a `BUSY`, leaves
    /// by the port the frame came in on.
    fn send_from_frame_sock(&mut self, to: Self::Addr, bytes: &[u8]);

    /// Take a freshly decoded frame and put it on the panel. Returns how long
    /// that took in microseconds, which becomes telemetry byte 42.
    ///
    /// The default does nothing and reports nothing, which is what a caller
    /// that publishes the buffer itself wants.
    fn present(&mut self, _frame: &Rgb888Frame) -> u32 {
        0
    }

    /// Something happened. The simulator records it; the firmware logs the
    /// handful of them a serial console should show.
    fn event(&mut self, _e: Event<'_, Self::Addr>) {}

    /// The answer to `GET_WIFI`: `(ssid, state byte)`. Spec section 8.4
    /// forbids the reply carrying a PSK, so this cannot return one.
    fn wifi(&self) -> (&'static str, u8);

    /// Carry out `SET_WIFI`, or refuse it with an error code from section 6.5.
    ///
    /// The default refuses with `ERR_NOT_PERMITTED`, which is the honest
    /// answer for a build that cannot change its credentials.
    fn set_wifi(&mut self, _w: &SetWifi<'_>) -> Result<(), u8> {
        Err(ErrorCode::NotPermitted.as_u8())
    }

    /// Last word on an outgoing `TELEMETRY` body, for counters this crate
    /// cannot see - the firmware measures byte 42 on the other core.
    fn adjust_telemetry(&self, _t: &mut Telemetry) {}
}

// ---------------------------------------------------------------------------
// Rate limiters
// ---------------------------------------------------------------------------

/// "At most one packet per `min_us` per source", with a fixed table.
///
/// When the table is full the oldest entry is evicted, which can only ever
/// make the device *more* generous to a source it has forgotten. That is the
/// right direction to fail: the alternative, refusing to track a new source,
/// would silence a `BUSY` that tells a sender why its frames are vanishing.
#[derive(Debug)]
pub struct Limiter<K, const N: usize> {
    key: [Option<K>; N],
    at: [u64; N],
    /// The `req_id` last answered for this key. Only section 5.5's `GET_INFO`
    /// limiter uses it; the other two pass 0 and ignore it.
    req: [u16; N],
}

impl<K: Copy + PartialEq, const N: usize> Limiter<K, N> {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Limiter {
            key: [None; N],
            at: [0; N],
            req: [0; N],
        }
    }

    /// True if a packet may go to `k` now, recording it if so.
    ///
    /// `eq` compares two keys: the `BUSY` and `TELEMETRY` limiters are keyed
    /// by endpoint, section 5.5's `GET_INFO` limiter by address alone.
    /// `retry_id` is section 6.1's retransmission exemption: a repeat of a
    /// `req_id` already answered inside the window is the same request asked
    /// twice, not a second one, so it is answered again. Pass 0 to disable it.
    pub fn allow(
        &mut self,
        k: K,
        now_us: u64,
        min_us: u64,
        retry_id: u16,
        eq: impl Fn(K, K) -> bool,
    ) -> bool {
        let mut free: Option<usize> = None;
        let mut oldest = 0usize;
        for i in 0..N {
            match self.key[i] {
                Some(e) if eq(e, k) => {
                    let repeat = retry_id != 0 && retry_id == self.req[i];
                    if now_us.saturating_sub(self.at[i]) < min_us && !repeat {
                        return false;
                    }
                    self.at[i] = now_us;
                    self.req[i] = retry_id;
                    return true;
                }
                None => {
                    free.get_or_insert(i);
                }
                Some(_) => {
                    if self.at[i] < self.at[oldest] {
                        oldest = i;
                    }
                }
            }
        }
        let i = free.unwrap_or(oldest);
        self.key[i] = Some(k);
        self.at[i] = now_us;
        self.req[i] = retry_id;
        true
    }
}

impl<K: Copy + PartialEq, const N: usize> Default for Limiter<K, N> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// What a [`Receiver`] is built with. Everything here is settled before the
/// first datagram arrives.
#[derive(Debug, Clone, Copy)]
pub struct Params<'a> {
    /// Stable short device id, the `id=` TXT key: section 5.1's lowercase MAC
    /// suffix on the device, anything short on a simulator.
    pub id: &'a str,
    /// Firmware version string, the `fw=` TXT key.
    pub fw: &'a str,
    /// Friendly name, the `name=` TXT key. `SET_NAME` changes it. Longer than
    /// [`screeny_proto::control::MAX_NAME_LEN`] is truncated.
    pub name: &'a str,
    /// The control port to advertise. [`Receiver::set_control_port`] changes
    /// it once an ephemeral bind is known.
    pub control_port: u16,
    /// Brightness at startup.
    pub brightness: u8,
    /// The ceiling `SET_BRIGHTNESS` clamps to.
    pub brightness_cap: u8,
    /// The RSSI to report in telemetry.
    pub rssi_dbm: i8,
    /// Idle behaviour at startup.
    pub idle_mode: IdleMode,
    /// Spec timing constants, or a test's compressed versions of them.
    pub timing: Timing,
}

impl Default for Params<'_> {
    fn default() -> Self {
        Params {
            id: "000000",
            fw: "0.1.0",
            name: "screeny",
            control_port: screeny_proto::DEFAULT_CONTROL_PORT,
            brightness: 255,
            brightness_cap: 255,
            rssi_dbm: 0,
            idle_mode: IdleMode::Status,
            timing: Timing::SPEC,
        }
    }
}

// ---------------------------------------------------------------------------
// The receiver
// ---------------------------------------------------------------------------

/// The receiver, minus its sockets, its clock and its panel.
#[derive(Debug)]
pub struct Receiver<A> {
    // --- identity and settings ------------------------------------------
    name: heapless::String<NAME_MAX>,
    id: heapless::String<16>,
    fw: heapless::String<24>,
    /// `GET_INFO` body and mDNS TXT record: the same bytes (section 6.6).
    info: heapless::Vec<u8, INFO_MAX>,
    ctrl_port: u16,
    brightness_cap: u8,
    brightness: u8,
    idle_mode: IdleMode,
    rssi_dbm: i8,
    timing: Timing,
    /// Extra microseconds to attribute to every decode, for fault injection.
    extra_decode_us: u32,

    // --- stream state ----------------------------------------------------
    state: State,
    active_source: Option<A>,
    last_frame_at_us: u64,
    /// When the device entered `HOLD`.
    hold_since_us: u64,
    /// When the device entered `IDLE`.
    idle_since_us: u64,
    last_seq: u16,
    have_last_seq: bool,

    // --- overlays --------------------------------------------------------
    identify_until_us: Option<u64>,

    // --- counters --------------------------------------------------------
    stats: Stats,
    /// Codec of the last frame shown, telemetry byte 47.
    last_codec: u8,
    /// Metadata of the frame on the panel, `None` until one is displayed.
    shown: Option<FrameMeta<A>>,

    // --- rate limiters ---------------------------------------------------
    busy_sent_at: Limiter<A, LIMIT_SLOTS>,
    telemetry_sent_at: Limiter<A, LIMIT_SLOTS>,
    info_sent_at: Limiter<A, LIMIT_SLOTS>,

    // --- the current drain -----------------------------------------------
    /// The survivor datagram the caller is holding for us: who sent it and
    /// its sequence number, which is all we need until it is handed back.
    pending: Option<(A, u16)>,
    stats_req: heapless::Vec<A, STATS_REQ_SLOTS>,

    // --- things the caller watches ---------------------------------------
    /// Bumped whenever the panel content must be recomposed.
    pub redraw: u32,
    /// Set by `REBOOT`; the caller sends the reply, then resets.
    pub reboot_pending: bool,
    /// Set when `SET_NAME` changed the TXT record, so mDNS re-announces.
    pub info_changed: bool,
}

impl<A: Copy + PartialEq + Eq> Receiver<A> {
    /// Build a receiver.
    #[must_use]
    pub fn new(p: &Params<'_>) -> Self {
        let mut c = Receiver {
            name: heapless::String::new(),
            id: heapless::String::new(),
            fw: heapless::String::new(),
            info: heapless::Vec::new(),
            ctrl_port: p.control_port,
            brightness_cap: p.brightness_cap,
            brightness: p.brightness.min(p.brightness_cap),
            idle_mode: p.idle_mode,
            rssi_dbm: p.rssi_dbm,
            timing: p.timing,
            extra_decode_us: 0,
            state: State::Idle,
            active_source: None,
            last_frame_at_us: 0,
            hold_since_us: 0,
            idle_since_us: 0,
            last_seq: 0,
            have_last_seq: false,
            identify_until_us: None,
            stats: Stats::default(),
            last_codec: 0,
            shown: None,
            busy_sent_at: Limiter::new(),
            telemetry_sent_at: Limiter::new(),
            info_sent_at: Limiter::new(),
            pending: None,
            stats_req: heapless::Vec::new(),
            redraw: 1,
            reboot_pending: false,
            info_changed: false,
        };
        push_truncated(&mut c.name, p.name);
        push_truncated(&mut c.id, p.id);
        push_truncated(&mut c.fw, p.fw);
        c.rebuild_info();
        c
    }

    // -----------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------

    /// The stream state, ignoring overlays.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The telemetry `state` byte. Section 7.3: `IDENTIFY` is an overlay, not
    /// a stream state, and it is what the byte reports while it is up.
    #[must_use]
    pub fn state_byte(&self) -> u8 {
        if self.identify_until_us.is_some() {
            tstate::IDENTIFY
        } else {
            self.state.as_u8()
        }
    }

    /// The source that holds the lock, if any.
    #[must_use]
    pub fn active_source(&self) -> Option<A> {
        self.active_source
    }

    /// Metadata of the displayed frame.
    #[must_use]
    pub fn shown(&self) -> Option<FrameMeta<A>> {
        self.shown
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Brightness currently applied.
    #[must_use]
    pub fn brightness(&self) -> u8 {
        self.brightness
    }

    /// The idle behaviour in force.
    #[must_use]
    pub fn idle_mode(&self) -> IdleMode {
        self.idle_mode
    }

    /// The friendly name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Microseconds left of the `IDENTIFY` overlay, if it is up.
    #[must_use]
    pub fn identify_until_us(&self) -> Option<u64> {
        self.identify_until_us
    }

    /// When the device entered `HOLD`, for the cross-fade.
    #[must_use]
    pub fn hold_since_us(&self) -> u64 {
        self.hold_since_us
    }

    /// When the device entered `IDLE`.
    #[must_use]
    pub fn idle_since_us(&self) -> u64 {
        self.idle_since_us
    }

    /// The timing constants in force.
    #[must_use]
    pub fn timing(&self) -> Timing {
        self.timing
    }

    /// Fresh `GET_INFO` / TXT bytes (section 6.6).
    #[must_use]
    pub fn info_bytes(&self) -> &[u8] {
        &self.info
    }

    /// Tell the receiver which control port it ended up on, so `ctrl=` in the
    /// TXT record is the port a sender can actually reach (spec section 1: the
    /// `ctrl=` key is authoritative). Tests bind port 0.
    pub fn set_control_port(&mut self, port: u16) {
        self.ctrl_port = port;
        self.rebuild_info();
    }

    /// Set the RSSI telemetry reports.
    pub fn set_rssi_dbm(&mut self, rssi: i8) {
        self.rssi_dbm = rssi;
    }

    /// Change the injected decode cost at runtime.
    pub fn set_extra_decode_us(&mut self, us: u32) {
        self.extra_decode_us = us;
    }

    /// Take the "the TXT record changed" flag, for the mDNS re-announce.
    pub fn take_info_changed(&mut self) -> bool {
        core::mem::take(&mut self.info_changed)
    }

    /// Assemble the telemetry struct of section 6.7.
    ///
    /// A caller whose [`Host`] overrides [`Host::adjust_telemetry`] must apply
    /// it to the result; the receiver does so for the bodies it sends itself.
    #[must_use]
    pub fn telemetry(&self, now_us: u64) -> Telemetry {
        self.stats.telemetry(
            (now_us / 1_000) as u32,
            self.rssi_dbm,
            self.brightness,
            self.state_byte(),
            self.last_codec,
        )
    }

    /// Count a datagram too long to have been read whole (section 1).
    pub fn count_oversize(&mut self) {
        Counters::bump(&mut self.stats.counters.frames_rejected);
    }

    fn rebuild_info(&mut self) {
        let info = DeviceInfo {
            codecs: dec::CODECS_TXT,
            ctrl: self.ctrl_port,
            fw: &self.fw,
            id: &self.id,
            name: &self.name,
            ..DeviceInfo::DEFAULT
        };
        let mut buf = [0u8; INFO_MAX];
        let n = info.write(&mut buf).unwrap_or(0);
        self.info.clear();
        let _ = self.info.extend_from_slice(&buf[..n]);
    }

    // -----------------------------------------------------------------
    // Frame port
    // -----------------------------------------------------------------

    /// Offer one datagram drained from the frame socket.
    ///
    /// Applies section 2's validation, section 7.4's admission rule and
    /// section 3.2's sequence test. [`Offer::Keep`] means this datagram is the
    /// drain's survivor so far and the caller must hold on to it; an earlier
    /// survivor it thereby throws away has already been counted in
    /// `frames_dropped_superseded` here. Nothing is decoded or displayed until
    /// [`Receiver::flush_frames`].
    pub fn offer_frame<H: Host<Addr = A>>(
        &mut self,
        h: &mut H,
        now_us: u64,
        from: A,
        data: &[u8],
    ) -> Offer {
        // Section 1 (and card 006 item 29): "A sender MUST NOT emit a datagram
        // whose UDP payload exceeds 1472 bytes." One that does is a protocol
        // violation before it is anything else, and a receive buffer that size
        // means a longer one arrived truncated, so the bytes we have are not
        // the bytes that were sent. Discard and count; do not parse the prefix.
        if data.len() > MAX_UDP_PAYLOAD {
            Counters::bump(&mut self.stats.counters.frames_rejected);
            h.event(Event::Rejected {
                from,
                reason: Some(Reject::TooLong),
            });
            return Offer::Drop;
        }
        let pkt = match FramePacket::parse(data) {
            Ok(p) => p,
            Err(reason) => {
                // Section 2: bad magic, version, type or length - and a
                // CONTROL that arrived on the frame port. Section 6.7 counts
                // all of it in frames_rejected, which is frame-port only.
                Counters::bump(&mut self.stats.counters.frames_rejected);
                h.event(Event::Rejected {
                    from,
                    reason: Some(reason),
                });
                return Offer::Drop;
            }
        };

        // --- section 7.4, frame admission -------------------------------
        match self.active_source {
            None => self.adopt(h, now_us, from),
            Some(s) if s == from => {}
            Some(s) => {
                let since = now_us.saturating_sub(self.last_frame_at_us) / 1_000;
                if since >= self.timing.lock_ms as u64 {
                    h.event(Event::LockReleased {
                        source: s,
                        reason: ReleaseReason::TakenOver,
                    });
                    self.adopt(h, now_us, from);
                } else {
                    Counters::bump(&mut self.stats.counters.frames_rejected);
                    h.event(Event::Rejected { from, reason: None });
                    let remaining = self.timing.lock_ms as u64 - since;
                    self.send_busy(h, now_us, from, remaining as u32);
                    return Offer::Drop;
                }
            }
        }

        // The source is allowed to drive the panel, so a STATS_REQ on this
        // frame gets answered whatever becomes of the pixels. See the note in
        // spec section 6.4: the one frame a second a sender marks is exactly
        // the frame it must not lose track of when things go wrong.
        if pkt.wants_stats() && !self.stats_req.contains(&from) {
            let _ = self.stats_req.push(from);
        }

        // --- section 3.2, sequence numbers ------------------------------
        let n = if self.have_last_seq {
            if !screeny_proto::newer(pkt.seq, self.last_seq) {
                Counters::bump(&mut self.stats.counters.frames_dropped_stale);
                h.event(Event::Dropped {
                    from,
                    seq: pkt.seq,
                    cause: DropCause::Stale,
                });
                return Offer::Drop;
            }
            let g = screeny_proto::gap(pkt.seq, self.last_seq);
            self.stats.counters.seq_gaps = self.stats.counters.seq_gaps.wrapping_add(g as u32);
            g as u32 + 1
        } else {
            1
        };
        self.last_seq = pkt.seq;
        self.have_last_seq = true;

        // --- accepted (section 7.4) -------------------------------------
        self.last_frame_at_us = now_us;
        Counters::bump(&mut self.stats.counters.frames_rx);
        self.stats.arrival.record(now_us, n);

        // --- section 3.3, newest wins -----------------------------------
        if let Some((old_from, old_seq)) = self.pending.take() {
            Counters::bump(&mut self.stats.counters.frames_dropped_superseded);
            h.event(Event::Dropped {
                from: old_from,
                seq: old_seq,
                cause: DropCause::Superseded,
            });
        }
        self.pending = Some((from, pkt.seq));
        Offer::Keep
    }

    /// Close the drain: decode the survivor into `back` and present it, then
    /// answer any `STATS_REQ` seen during the drain.
    ///
    /// `survivor` must be the datagram of the last [`Offer::Keep`], or `None`
    /// if there was none. Returns `true` if `back` now holds a new frame the
    /// caller should publish (section 4.7: swap only on success).
    pub fn flush_frames<H: Host<Addr = A>>(
        &mut self,
        h: &mut H,
        now_us: u64,
        survivor: Option<&[u8]>,
        back: &mut Rgb888Frame,
    ) -> bool {
        let mut shown = false;
        if let Some((from, _)) = self.pending.take() {
            // Re-parsing costs eight byte loads and keeps the payload out of
            // the receiver, which is what lets this crate allocate nothing.
            if let Some(pkt) = survivor.and_then(|d| FramePacket::parse(d).ok()) {
                let t0 = h.micros();
                let result = dec::decode(pkt.codec, pkt.payload, back);
                let decode_us = (h.micros() - t0).min(u32::MAX as u64) as u32;
                self.stats
                    .timings
                    .decode(decode_us.saturating_add(self.extra_decode_us));

                match result {
                    Ok(()) => {
                        Counters::bump(&mut self.stats.counters.frames_shown);
                        self.last_codec = pkt.codec;
                        self.shown = Some(FrameMeta {
                            seq: pkt.seq,
                            codec: pkt.codec,
                            bytes: pkt.payload.len(),
                            timestamp_us: pkt.timestamp_us,
                            from,
                        });
                        // Section 3.3 step 3: swap at a refresh boundary, so a
                        // partially decoded frame is never scanned out.
                        let render_us = h.present(back);
                        self.stats.timings.render(render_us);
                        shown = true;
                        h.event(Event::Shown {
                            from,
                            seq: pkt.seq,
                            codec: pkt.codec,
                            bytes: pkt.payload.len(),
                            timestamp_us: pkt.timestamp_us,
                        });
                        // Section 7.4: the lock goes when a FINAL frame is
                        // *displayed*. A FINAL frame that failed to decode was
                        // never displayed, so it does not release anything;
                        // the stream timeout will, a second later. One damaged
                        // packet cannot hand the panel to a stranger.
                        if pkt.is_final() {
                            self.release(h, now_us, ReleaseReason::Final);
                        }
                    }
                    Err(e) => {
                        // Section 4.7: discard, count it, leave the previous
                        // frame lit.
                        Counters::bump(&mut self.stats.counters.frames_dropped_decode);
                        h.event(Event::Dropped {
                            from,
                            seq: pkt.seq,
                            cause: DropCause::Decode(e),
                        });
                    }
                }
            }
        }

        // Section 3.1: "after processing this frame, send one TELEMETRY
        // reply". After, so the counters the sender reads already include it.
        while !self.stats_req.is_empty() {
            let to = self.stats_req.remove(0);
            self.send_telemetry(h, now_us, to);
        }
        shown
    }

    /// Advance the timers: `LIVE -> HOLD`, `HOLD -> IDLE`, `IDENTIFY` expiry.
    pub fn tick<H: Host<Addr = A>>(&mut self, h: &mut H, now_us: u64) {
        if let Some(until) = self.identify_until_us {
            if now_us >= until {
                self.identify_until_us = None;
                self.redraw = self.redraw.wrapping_add(1);
                h.event(Event::IdentifyExpired);
            }
        }
        match self.state {
            State::Live => {
                let idle_for = now_us.saturating_sub(self.last_frame_at_us) / 1_000;
                if idle_for >= self.timing.stream_timeout_ms as u64 {
                    self.release(h, now_us, ReleaseReason::Timeout);
                }
            }
            State::Hold => {
                // Section 7.5, mode 1: stay in HOLD, never fade.
                if self.idle_mode == IdleMode::HoldForever {
                    return;
                }
                let held = now_us.saturating_sub(self.hold_since_us) / 1_000;
                if held >= self.timing.hold_ms as u64 {
                    self.set_state(h, State::Idle, now_us);
                    self.idle_since_us = now_us;
                }
            }
            State::Idle => {}
        }
    }

    /// Section 7.3's "any -> Wi-Fi link down -> HOLD".
    pub fn link_down<H: Host<Addr = A>>(&mut self, h: &mut H, now_us: u64) {
        if self.state == State::Live {
            self.release(h, now_us, ReleaseReason::LinkDown);
        }
    }

    /// What the panel should show now.
    #[must_use]
    pub fn intent(&self, now_us: u64) -> Intent {
        if self.identify_until_us.is_some() {
            return Intent::Identify;
        }
        match self.state {
            State::Live | State::Hold => Intent::Stream,
            State::Idle => {
                let since = now_us.saturating_sub(self.idle_since_us) / 1_000;
                let fade = self.timing.fade_ms as u64;
                if fade > 0 && since < fade {
                    Intent::Fade {
                        t: ((since * 256) / fade) as u16,
                    }
                } else {
                    Intent::Idle
                }
            }
        }
    }

    fn adopt<H: Host<Addr = A>>(&mut self, h: &mut H, now_us: u64, from: A) {
        let displaced = self.active_source;
        self.active_source = Some(from);
        // Sections 7.3 and 6.8: reset last_seq and the jitter EWMAs, not the
        // counters.
        self.have_last_seq = false;
        self.stats.arrival.on_source_change();
        h.event(Event::LockTaken {
            source: from,
            displaced,
        });
        self.set_state(h, State::Live, now_us);
    }

    fn release<H: Host<Addr = A>>(&mut self, h: &mut H, now_us: u64, reason: ReleaseReason) {
        if let Some(s) = self.active_source.take() {
            h.event(Event::LockReleased { source: s, reason });
        }
        self.have_last_seq = false;
        self.stats.arrival.on_source_change();
        if self.state != State::Idle {
            self.set_state(h, State::Hold, now_us);
        }
    }

    fn set_state<H: Host<Addr = A>>(&mut self, h: &mut H, to: State, now_us: u64) {
        if self.state == to {
            return;
        }
        let from = self.state;
        self.state = to;
        if to == State::Hold {
            self.hold_since_us = now_us;
        }
        if to == State::Idle {
            self.idle_since_us = now_us;
        }
        self.redraw = self.redraw.wrapping_add(1);
        h.event(Event::StateChanged { from, to });
    }

    // -----------------------------------------------------------------
    // Unsolicited device packets (section 6.2)
    // -----------------------------------------------------------------

    fn send_busy<H: Host<Addr = A>>(&mut self, h: &mut H, now_us: u64, to: A, remaining_ms: u32) {
        let min = self.timing.busy_min_interval_ms as u64 * 1_000;
        let ok = self
            .busy_sent_at
            .allow(to, now_us, min, 0, |a, b| a == b);
        if ok {
            let reply = Reply::Busy {
                reason: busy_reason::LOCKED,
                lock_holder_ms_remaining: remaining_ms,
            };
            send(h, to, &reply, op::BUSY);
        }
        h.event(Event::Busy { to, sent: ok });
    }

    fn send_telemetry<H: Host<Addr = A>>(&mut self, h: &mut H, now_us: u64, to: A) {
        let min = self.timing.telemetry_min_interval_ms as u64 * 1_000;
        if !self
            .telemetry_sent_at
            .allow(to, now_us, min, 0, |a, b| a == b)
        {
            h.event(Event::TelemetryRateLimited { to });
            return;
        }
        let mut t = self.telemetry(now_us);
        h.adjust_telemetry(&mut t);
        send(h, to, &Reply::Telemetry(t), op::TELEMETRY);
        h.event(Event::TelemetrySent { to });
    }

    // -----------------------------------------------------------------
    // Control port (section 6)
    // -----------------------------------------------------------------

    /// Handle one datagram from the control socket.
    ///
    /// Returns the length of a reply written into `out`, or `None` when the
    /// spec says to answer nothing.
    pub fn control<H: Host<Addr = A>>(
        &mut self,
        h: &mut H,
        now_us: u64,
        from: A,
        data: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let pkt = match ControlPacket::parse(data) {
            Ok(p) => p,
            Err(reason) => return self.control_reject(h, from, data, reason, out),
        };

        // Section 6.1 tells requests and replies apart by the REPLY bit and by
        // nothing else, so a device that answered replies would answer its own
        // and two of them on one LAN would talk to each other indefinitely. A
        // reply arriving at a device is somebody else's packet.
        if pkt.flags & C_REPLY != 0 {
            h.event(Event::ControlRejected {
                from,
                reason: Reject::BadType,
            });
            return None;
        }

        let req_id = pkt.req_id;

        // Section 5.5's rate limit, applied before the request is carried out
        // rather than after, because a suppressed GET_INFO must leave no
        // trace. GET_INFO has an empty body, so this does not pre-empt the
        // ERR_BAD_LENGTH a malformed one still deserves.
        if pkt.op == op::GET_INFO && pkt.body.is_empty() {
            let min = self.timing.info_min_interval_ms as u64 * 1_000;
            let ok = self.info_sent_at.allow(from, now_us, min, req_id, |a, b| {
                H::ip_of(a) == H::ip_of(b)
            });
            if !ok {
                h.event(Event::Control {
                    from,
                    op: pkt.op,
                    req_id,
                    error: None,
                    replied: false,
                });
                return None;
            }
            return emit(h, from, op::GET_INFO, req_id, &Reply::Info(&self.info), out);
        }

        let reply = match Request::decode(pkt.op, pkt.body) {
            Ok(req) => self.apply(h, now_us, from, req),
            Err(code) => Reply::Err {
                code: code.as_u8(),
            },
        };
        emit(h, from, pkt.op, req_id, &reply, out)
    }

    /// A datagram the control socket could not parse.
    ///
    /// Two of section 2's rejects have an answer rather than silence, and both
    /// are built out of header bytes that are certainly present: section 6.5's
    /// `ERR_BAD_LENGTH` for a `len` the datagram does not back up, and section
    /// 2.2's optional `ERR_VERSION` for a `CONTROL` of a version we do not
    /// speak. Everything else is discarded.
    fn control_reject<H: Host<Addr = A>>(
        &mut self,
        h: &mut H,
        from: A,
        data: &[u8],
        reason: Reject,
        out: &mut [u8],
    ) -> Option<usize> {
        h.event(Event::ControlRejected { from, reason });
        let code = match reason {
            Reject::BadLength => ErrorCode::BadLength,
            Reject::BadVersion => ErrorCode::Version,
            _ => return None,
        };
        let hdr = screeny_proto::peek(data)?;
        if hdr.ty != TYPE_CONTROL || hdr.id == 0 {
            return None;
        }
        if reason == Reject::BadVersion && hdr.version == VERSION {
            return None;
        }
        // A reply to a reply would be a loop; a device never starts one.
        if hdr.flags & C_REPLY != 0 {
            return None;
        }
        let reply = Reply::Err {
            code: code.as_u8(),
        };
        emit(h, from, hdr.b2, hdr.id, &reply, out)
    }

    /// Carry out a decoded request and say what to reply with.
    fn apply<H: Host<Addr = A>>(
        &mut self,
        h: &mut H,
        now_us: u64,
        from: A,
        req: Request<'_>,
    ) -> Reply<'static> {
        match req {
            Request::Ping => Reply::Ping {
                uptime_ms: (now_us / 1_000) as u32,
            },
            // Handled in `control`, where the rate limit lives; a GET_INFO
            // that reaches here has a body, which section 6.5 calls a length
            // error.
            Request::GetInfo => Reply::Err {
                code: ErrorCode::BadLength.as_u8(),
            },
            // A TELEMETRY request on the control port is solicited, so section
            // 6.2's 100 ms limit - which governs the *unsolicited* reply to a
            // STATS_REQ frame - does not apply to it.
            Request::Telemetry => {
                let mut t = self.telemetry(now_us);
                h.adjust_telemetry(&mut t);
                Reply::Telemetry(t)
            }
            Request::SetBrightness(level) => {
                let applied = level.min(self.brightness_cap);
                self.brightness = applied;
                h.event(Event::Brightness {
                    requested: level,
                    applied,
                });
                Reply::Brightness { applied }
            }
            Request::Identify { duration_ms } => {
                self.identify_until_us = if duration_ms == 0 {
                    None
                } else {
                    Some(now_us + duration_ms as u64 * 1_000)
                };
                self.redraw = self.redraw.wrapping_add(1);
                h.event(Event::Identify { duration_ms });
                Reply::Identify
            }
            Request::SetIdle(mode) => {
                self.idle_mode = mode;
                self.redraw = self.redraw.wrapping_add(1);
                h.event(Event::IdleMode {
                    mode: mode.as_u8(),
                });
                Reply::Idle {
                    mode: mode.as_u8(),
                }
            }
            Request::ResetStats => {
                self.stats.reset();
                h.event(Event::StatsReset);
                Reply::ResetStats
            }
            Request::Release => {
                // Section 7.4 matches on the IP alone, and it has to: RELEASE
                // arrives on the control port, so its source *port* is never
                // the frame stream's. A RELEASE from anyone else is an
                // ordinary acknowledgement that changes nothing.
                if self.active_source.map(H::ip_of) == Some(H::ip_of(from)) {
                    self.release(h, now_us, ReleaseReason::Released);
                }
                Reply::Release
            }
            Request::SetName(name) => {
                let previous = self.name.clone();
                self.name.clear();
                if self.name.push_str(name).is_err() {
                    self.name = previous;
                    return Reply::Err {
                        code: ErrorCode::BadArg.as_u8(),
                    };
                }
                self.rebuild_info();
                self.info_changed = true;
                self.redraw = self.redraw.wrapping_add(1);
                h.event(Event::Renamed { name: &self.name });
                Reply::SetName
            }
            Request::GetWifi => {
                let (ssid, state) = h.wifi();
                Reply::Wifi { ssid, state }
            }
            Request::SetWifi(w) => match h.set_wifi(&w) {
                Ok(()) => Reply::SetWifi,
                Err(code) => Reply::Err { code },
            },
            Request::Reboot => {
                self.reboot_pending = true;
                h.event(Event::Reboot);
                Reply::Reboot
            }
        }
    }
}

/// Truncate `s` to what `dst` can hold, on a character boundary.
fn push_truncated<const N: usize>(dst: &mut heapless::String<N>, s: &str) {
    let mut end = s.len().min(N);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let _ = dst.push_str(&s[..end]);
}

/// Build one unsolicited datagram on the stack and hand it to the host.
fn send<H: Host>(h: &mut H, to: H::Addr, reply: &Reply<'_>, opcode: u8) {
    let mut buf = [0u8; OUT_MAX];
    // req_id 0: both packets of section 6.2 are unsolicited.
    if let Ok(n) = reply.write(opcode, 0, &mut buf) {
        h.send_from_frame_sock(to, &buf[..n]);
    }
}

/// Write a reply into the caller's buffer, or record that we deliberately sent
/// none.
///
/// Section 6.1: `req_id` 0 means "no reply wanted", and that covers an error
/// reply too - a sender that did not ask to be told cannot be told. The
/// request itself has still been carried out.
fn emit<H: Host>(
    h: &mut H,
    from: H::Addr,
    op: u8,
    req_id: u16,
    reply: &Reply<'_>,
    out: &mut [u8],
) -> Option<usize> {
    let error = match reply {
        Reply::Err { code } => Some(*code),
        _ => None,
    };
    let written = if req_id == 0 {
        None
    } else {
        reply.write(op, req_id, out).ok()
    };
    h.event(Event::Control {
        from,
        op,
        req_id,
        error,
        replied: written.is_some(),
    });
    written
}
