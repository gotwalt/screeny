//! What the device says it did.
//!
//! Every interesting transition produces an [`Event`]. The binary logs them;
//! a test waits for them. They are the simulator's substitute for a serial
//! console, and they are how an integration test asserts on something that
//! leaves no trace in the counters - `SET_WIFI` arriving, say, which the
//! simulator accepts and deliberately does not act on.
//!
//! [`State`], [`DropCause`] and [`ReleaseReason`] come from the shared
//! receiver core (card 016); [`Event`] is the owned, `String`-carrying form of
//! [`screeny_receiver::Event`], which a `no_std` device cannot afford.

use std::net::SocketAddr;

use screeny_proto::Reject;
use screeny_receiver as rx;

pub use screeny_receiver::{DropCause, ReleaseReason, State};

/// Something the device did.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A frame was decoded and swapped onto the panel.
    Shown {
        /// Who sent it.
        from: SocketAddr,
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
        from: SocketAddr,
        /// Its sequence number.
        seq: u16,
        /// Why it did not reach the panel.
        cause: DropCause,
    },
    /// A datagram on the frame port was not a frame we would act on.
    Rejected {
        /// Who sent it.
        from: SocketAddr,
        /// Which of section 2's rules it broke, or `None` when it was a
        /// well-formed frame from a source that does not hold the lock.
        reason: Option<Reject>,
    },
    /// A frame arrived while another source held the lock.
    Busy {
        /// The locked-out source.
        to: SocketAddr,
        /// Whether a `BUSY` packet was actually sent, or suppressed by
        /// `BUSY_MIN_INTERVAL_MS`.
        sent: bool,
    },
    /// A `TELEMETRY` packet was sent back down the frame port (section 6.4).
    TelemetrySent {
        /// Where to.
        to: SocketAddr,
    },
    /// A piggybacked telemetry request was suppressed by the 100 ms limit.
    TelemetryRateLimited {
        /// Who asked.
        to: SocketAddr,
    },
    /// A control request arrived and was answered (or deliberately not).
    Control {
        /// Who sent it.
        from: SocketAddr,
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
        from: SocketAddr,
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
        source: SocketAddr,
        /// The source it displaced, if any.
        displaced: Option<SocketAddr>,
    },
    /// The panel is free again.
    LockReleased {
        /// The source that had it.
        source: SocketAddr,
        /// Why.
        reason: ReleaseReason,
    },
    /// Brightness changed.
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
    /// `SET_IDLE` changed the idle behaviour.
    IdleMode {
        /// The new mode byte.
        mode: u8,
    },
    /// `SET_NAME` changed the friendly name.
    Renamed {
        /// The new name.
        name: String,
    },
    /// `RESET_STATS` zeroed the counters.
    StatsReset,
    /// `SET_WIFI` arrived. The simulator logs it and does nothing, which is
    /// the whole point of a simulator with no radio.
    SetWifi {
        /// The SSID asked for. The PSK is deliberately not carried here: spec
        /// section 8.4's invariant is that it never appears in a log.
        ssid: String,
        /// Whether persistence was asked for.
        persist: bool,
    },
    /// `REBOOT` arrived, with the right magic. Logged, not acted on.
    Reboot,

    // --- card 224: provisioning and HTTP ------------------------------------
    /// The provisioning machine moved (research 007 section 5.2).
    WifiPhaseChanged {
        /// Where it was.
        from: WifiPhase,
        /// Where it is now.
        to: WifiPhase,
    },
    /// The provisioning machine asked the caller to do something. Reported
    /// rather than interpreted: `screeny_provision::Action` is the vocabulary,
    /// and the simulator does not invent a second one.
    WifiAction {
        /// What it asked for.
        action: WifiAction,
    },
    /// A test took the WiFi link down (spec section 7.3).
    LinkDown,
    /// A test brought the link back.
    LinkUp,
    /// An HTTP request was answered.
    ///
    /// The **query string and the body are deliberately not here.** A PSK
    /// arrives in the body of `POST /api/v1/wifi`, and spec section 8.4 says
    /// it never reaches a log line; the only way to be sure of that is for the
    /// log to have no field it could occupy.
    Http {
        /// The method, uppercased.
        method: String,
        /// The path, without its query string.
        path: String,
        /// The status the simulator answered with.
        status: u16,
    },
}

pub use screeny_provision::machine::Action as WifiAction;
pub use screeny_provision::State as WifiPhase;

impl Event {
    /// The owned form of what the shared receiver reported, or `None` for the
    /// things only a device acts on.
    ///
    /// `SET_WIFI` is not here: the shared core leaves the whole decision to
    /// the host, so [`crate::core`] raises that one itself.
    #[must_use]
    pub(crate) fn from_shared(e: &rx::Event<'_, SocketAddr>) -> Option<Event> {
        Some(match *e {
            rx::Event::Shown {
                from,
                seq,
                codec,
                bytes,
                timestamp_us,
            } => Event::Shown {
                from,
                seq,
                codec,
                bytes,
                timestamp_us,
            },
            rx::Event::Dropped { from, seq, cause } => Event::Dropped { from, seq, cause },
            rx::Event::Rejected { from, reason } => Event::Rejected { from, reason },
            rx::Event::Busy { to, sent } => Event::Busy { to, sent },
            rx::Event::TelemetrySent { to } => Event::TelemetrySent { to },
            rx::Event::TelemetryRateLimited { to } => Event::TelemetryRateLimited { to },
            rx::Event::Control {
                from,
                op,
                req_id,
                error,
                replied,
            } => Event::Control {
                from,
                op,
                req_id,
                error,
                replied,
            },
            rx::Event::ControlRejected { from, reason } => Event::ControlRejected { from, reason },
            rx::Event::StateChanged { from, to } => Event::StateChanged { from, to },
            rx::Event::LockTaken { source, displaced } => Event::LockTaken { source, displaced },
            rx::Event::LockReleased { source, reason } => Event::LockReleased { source, reason },
            rx::Event::Brightness { requested, applied } => {
                Event::Brightness { requested, applied }
            }
            rx::Event::Identify { duration_ms } => Event::Identify { duration_ms },
            rx::Event::IdleMode { mode } => Event::IdleMode { mode },
            rx::Event::Renamed { name } => Event::Renamed {
                name: name.to_string(),
            },
            rx::Event::StatsReset => Event::StatsReset,
            rx::Event::Reboot => Event::Reboot,
            // The overlay timing out is a repaint on a device and nothing at
            // all to a test: the state byte and `identify_until_us` already
            // say so, and card 006's event stream never had it.
            rx::Event::IdentifyExpired => return None,
            _ => return None,
        })
    }
}
