//! What the device says it did.
//!
//! Every interesting transition produces an [`Event`]. The binary logs them;
//! a test waits for them. They are the simulator's substitute for a serial
//! console, and they are how an integration test asserts on something that
//! leaves no trace in the counters - `SET_WIFI` arriving, say, which the
//! simulator accepts and deliberately does not act on.

use std::net::SocketAddr;

use screeny_proto::{DecodeError, Reject};

/// The stream state of spec section 7.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Showing the status screen. No active source.
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
            State::Idle => screeny_proto::control::state::IDLE,
            State::Live => screeny_proto::control::state::LIVE,
            State::Hold => screeny_proto::control::state::HOLD,
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
}
