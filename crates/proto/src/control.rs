//! Control opcodes, request and reply bodies, telemetry, error codes.
//!
//! Spec: `docs/design/protocol-v1.md` section 6 (and 8.2 for `SET_WIFI`).
//!
//! The device side is [`Request::decode`], which turns `(op, body)` into a
//! typed request or into exactly the [`ErrorCode`] the spec says to reply
//! with. The sender side is [`Request::write`] and [`Reply::decode`].
//!
//! Bodies are strict: a request whose body is not the opcode's exact size is
//! `ERR_BAD_LENGTH`, because section 6.5 defines that code as "`len`
//! inconsistent with ... the opcode's fixed body size". The one deliberately
//! loose case is [`Telemetry`], which is versioned by `len` and tolerates
//! fields appended by a future device.

use crate::packet::{reserve_control, BuildError};
use crate::{C_ERROR, C_REPLY};

/// Control opcodes, section 6.3.
pub mod op {
    /// Liveness. Reply: `u32le uptime_ms`.
    pub const PING: u8 = 0x01;
    /// Device metadata. Reply: DNS-SD TXT wire format ([`crate::txt`]).
    pub const GET_INFO: u8 = 0x02;
    /// Counters. Reply: [`super::Telemetry`].
    pub const TELEMETRY: u8 = 0x03;
    /// Set panel brightness, clamped by the firmware cap.
    pub const SET_BRIGHTNESS: u8 = 0x04;
    /// "Which one is this?" overlay.
    pub const IDENTIFY: u8 = 0x05;
    /// Set the idle behaviour, section 7.5.
    pub const SET_IDLE: u8 = 0x06;
    /// Zero the telemetry counters.
    pub const RESET_STATS: u8 = 0x07;
    /// Give up the source lock.
    pub const RELEASE: u8 = 0x08;
    /// Set the friendly name.
    pub const SET_NAME: u8 = 0x09;
    /// Read the stored SSID and join state. Never returns the PSK.
    pub const GET_WIFI: u8 = 0x0A;
    /// Store credentials and rejoin, section 8.2.
    pub const SET_WIFI: u8 = 0x0B;
    /// Device -> sender only: your frames are being rejected.
    pub const BUSY: u8 = 0x0C;
    /// Reboot, guarded by [`super::REBOOT_MAGIC`].
    pub const REBOOT: u8 = 0x0D;
}

/// The `u32le` body `REBOOT` requires, `"RBOO"` read little-endian.
pub const REBOOT_MAGIC: u32 = 0x4F4F_4252;

/// Longest `SET_NAME` name, bytes of UTF-8.
pub const MAX_NAME_LEN: usize = 32;
/// Longest Wi-Fi SSID, bytes.
pub const MAX_SSID_LEN: usize = 32;
/// Longest Wi-Fi PSK, bytes. 0 means an open network.
pub const MAX_PSK_LEN: usize = 64;

/// Body length of a `TELEMETRY` reply in v1. A future device may send more.
pub const TELEMETRY_LEN: usize = 48;

/// Error codes, section 6.5.
///
/// Doubles as this module's parse-failure type: `BadLength`, `UnknownOp` and
/// `BadArg` are precisely the ways a body can be wrong, and on the device side
/// the value is the byte to put in the error reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    /// `len` inconsistent with the datagram or with the opcode's body size.
    BadLength = 0x01,
    /// Opcode not implemented.
    UnknownOp = 0x02,
    /// Protocol version not supported.
    Version = 0x03,
    /// Another sender holds the lock.
    Busy = 0x04,
    /// Body parsed but the value is out of range.
    BadArg = 0x05,
    /// Persisting the setting failed.
    Storage = 0x06,
    /// Wi-Fi operation failed.
    Wifi = 0x07,
    /// Op disabled in this build or requires auth.
    NotPermitted = 0x08,
    /// Too many requests.
    RateLimited = 0x09,
}

impl ErrorCode {
    /// The wire byte.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Interpret a wire byte. `None` for a code this version does not know.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => ErrorCode::BadLength,
            0x02 => ErrorCode::UnknownOp,
            0x03 => ErrorCode::Version,
            0x04 => ErrorCode::Busy,
            0x05 => ErrorCode::BadArg,
            0x06 => ErrorCode::Storage,
            0x07 => ErrorCode::Wifi,
            0x08 => ErrorCode::NotPermitted,
            0x09 => ErrorCode::RateLimited,
            _ => return None,
        })
    }
}

/// Idle behaviour, section 7.5. Set with `SET_IDLE`, persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum IdleMode {
    /// Name, address, RSSI bars, slow ambient animation. The default.
    #[default]
    Status = 0,
    /// Stay in `HOLD` forever; never fade.
    HoldForever = 1,
    /// Fade the last frame to 10% brightness and hold it.
    Dim = 2,
    /// Fade to black.
    Black = 3,
}

impl IdleMode {
    /// The wire byte.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
    /// Interpret a wire byte. `None` for a mode this version does not know.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => IdleMode::Status,
            1 => IdleMode::HoldForever,
            2 => IdleMode::Dim,
            3 => IdleMode::Black,
            _ => return None,
        })
    }
}

/// Stream state, the `state` byte of [`Telemetry`] (section 6.7).
pub mod state {
    /// Showing the status screen; no active source.
    pub const IDLE: u8 = 0;
    /// Streaming; a source holds the lock.
    pub const LIVE: u8 = 1;
    /// Last frame still lit, lock released.
    pub const HOLD: u8 = 2;
    /// `IDENTIFY` overlay is up (frame handling continues underneath).
    pub const IDENTIFY: u8 = 3;
    /// Wi-Fi provisioning overlay is up.
    pub const PROVISIONING: u8 = 4;
}

/// Join state, the trailing byte of a `GET_WIFI` reply (section 6.3).
pub mod wifi_state {
    /// Not associated and not trying.
    pub const DISCONNECTED: u8 = 0;
    /// Association in progress.
    pub const CONNECTING: u8 = 1;
    /// Associated and addressed.
    pub const CONNECTED: u8 = 2;
    /// The last join attempt failed; this is what section 8.2 calls reporting
    /// `ERR_WIFI`.
    pub const FAILED: u8 = 3;
}

/// Reasons a device sends `BUSY` (section 6.3).
pub mod busy_reason {
    /// Another source holds the lock (section 7.4). The only v1 reason.
    pub const LOCKED: u8 = 0;
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// The body of a `SET_WIFI` request, section 8.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetWifi<'a> {
    /// 1..=32 bytes of UTF-8.
    pub ssid: &'a str,
    /// 0..=64 bytes of UTF-8. Empty means an open network.
    pub psk: &'a str,
    /// Store the credentials, rather than trying them only until reboot.
    pub persist: bool,
}

/// A control request: sender -> device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request<'a> {
    /// [`op::PING`].
    Ping,
    /// [`op::GET_INFO`].
    GetInfo,
    /// [`op::TELEMETRY`].
    Telemetry,
    /// [`op::SET_BRIGHTNESS`]. The device clamps to its compile-time cap.
    SetBrightness(u8),
    /// [`op::IDENTIFY`]. 0 stops an identify in progress.
    Identify {
        /// Overlay duration in milliseconds.
        duration_ms: u16,
    },
    /// [`op::SET_IDLE`].
    SetIdle(IdleMode),
    /// [`op::RESET_STATS`].
    ResetStats,
    /// [`op::RELEASE`].
    Release,
    /// [`op::SET_NAME`].
    SetName(&'a str),
    /// [`op::GET_WIFI`].
    GetWifi,
    /// [`op::SET_WIFI`].
    SetWifi(SetWifi<'a>),
    /// [`op::REBOOT`]. Encodes [`REBOOT_MAGIC`]; decoding rejects anything else.
    Reboot,
}

impl<'a> Request<'a> {
    /// The opcode this request carries.
    #[must_use]
    pub fn op(&self) -> u8 {
        match self {
            Request::Ping => op::PING,
            Request::GetInfo => op::GET_INFO,
            Request::Telemetry => op::TELEMETRY,
            Request::SetBrightness(_) => op::SET_BRIGHTNESS,
            Request::Identify { .. } => op::IDENTIFY,
            Request::SetIdle(_) => op::SET_IDLE,
            Request::ResetStats => op::RESET_STATS,
            Request::Release => op::RELEASE,
            Request::SetName(_) => op::SET_NAME,
            Request::GetWifi => op::GET_WIFI,
            Request::SetWifi(_) => op::SET_WIFI,
            Request::Reboot => op::REBOOT,
        }
    }

    /// Body length on the wire.
    #[must_use]
    pub fn body_len(&self) -> usize {
        match self {
            Request::Ping
            | Request::GetInfo
            | Request::Telemetry
            | Request::ResetStats
            | Request::Release
            | Request::GetWifi => 0,
            Request::SetBrightness(_) | Request::SetIdle(_) => 1,
            Request::Identify { .. } => 2,
            Request::Reboot => 4,
            Request::SetName(s) => 1 + s.len(),
            Request::SetWifi(w) => 3 + w.ssid.len() + w.psk.len(),
        }
    }

    /// Total datagram length.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        crate::HEADER_LEN + self.body_len()
    }

    /// Decode a request body the device just received.
    ///
    /// # Errors
    ///
    /// The [`ErrorCode`] to put in the error reply: [`ErrorCode::UnknownOp`]
    /// for an opcode we do not implement, [`ErrorCode::BadLength`] for a body
    /// that is not the opcode's size, [`ErrorCode::BadArg`] for a body that is
    /// the right size but holds a value out of range.
    pub fn decode(op: u8, body: &'a [u8]) -> Result<Self, ErrorCode> {
        let empty = |r: Request<'a>| {
            if body.is_empty() {
                Ok(r)
            } else {
                Err(ErrorCode::BadLength)
            }
        };
        match op {
            op::PING => empty(Request::Ping),
            op::GET_INFO => empty(Request::GetInfo),
            op::TELEMETRY => empty(Request::Telemetry),
            op::RESET_STATS => empty(Request::ResetStats),
            op::RELEASE => empty(Request::Release),
            op::GET_WIFI => empty(Request::GetWifi),
            op::SET_BRIGHTNESS => match body {
                [level] => Ok(Request::SetBrightness(*level)),
                _ => Err(ErrorCode::BadLength),
            },
            op::SET_IDLE => match body {
                [mode] => IdleMode::from_u8(*mode)
                    .map(Request::SetIdle)
                    .ok_or(ErrorCode::BadArg),
                _ => Err(ErrorCode::BadLength),
            },
            op::IDENTIFY => match body {
                [lo, hi] => Ok(Request::Identify {
                    duration_ms: u16::from_le_bytes([*lo, *hi]),
                }),
                _ => Err(ErrorCode::BadLength),
            },
            op::REBOOT => match body {
                [a, b, c, d] => {
                    if u32::from_le_bytes([*a, *b, *c, *d]) == REBOOT_MAGIC {
                        Ok(Request::Reboot)
                    } else {
                        Err(ErrorCode::BadArg)
                    }
                }
                _ => Err(ErrorCode::BadLength),
            },
            op::SET_NAME => {
                let (n, rest) = split_u8_prefixed(body).ok_or(ErrorCode::BadLength)?;
                if !rest.is_empty() {
                    return Err(ErrorCode::BadLength);
                }
                if n.len() > MAX_NAME_LEN {
                    return Err(ErrorCode::BadArg);
                }
                core::str::from_utf8(n)
                    .map(Request::SetName)
                    .map_err(|_| ErrorCode::BadArg)
            }
            op::SET_WIFI => {
                let (ssid, rest) = split_u8_prefixed(body).ok_or(ErrorCode::BadLength)?;
                let (psk, rest) = split_u8_prefixed(rest).ok_or(ErrorCode::BadLength)?;
                let flags = match rest {
                    [f] => *f,
                    _ => return Err(ErrorCode::BadLength),
                };
                if ssid.is_empty()
                    || ssid.len() > MAX_SSID_LEN
                    || psk.len() > MAX_PSK_LEN
                    || flags & !0x01 != 0
                {
                    return Err(ErrorCode::BadArg);
                }
                Ok(Request::SetWifi(SetWifi {
                    ssid: core::str::from_utf8(ssid).map_err(|_| ErrorCode::BadArg)?,
                    psk: core::str::from_utf8(psk).map_err(|_| ErrorCode::BadArg)?,
                    persist: flags & 0x01 != 0,
                }))
            }
            _ => Err(ErrorCode::UnknownOp),
        }
    }

    /// Serialise a complete request datagram into `out`.
    ///
    /// `req_id` 0 means "no reply wanted" (section 6.1).
    ///
    /// # Errors
    ///
    /// See [`BuildError`].
    pub fn write(&self, req_id: u16, out: &mut [u8]) -> Result<usize, BuildError> {
        let n = self.body_len();
        let body = reserve_control(out, self.op(), 0, req_id, n)?;
        match self {
            Request::Ping
            | Request::GetInfo
            | Request::Telemetry
            | Request::ResetStats
            | Request::Release
            | Request::GetWifi => {}
            Request::SetBrightness(level) => body[0] = *level,
            Request::SetIdle(m) => body[0] = m.as_u8(),
            Request::Identify { duration_ms } => {
                body[..2].copy_from_slice(&duration_ms.to_le_bytes());
            }
            Request::Reboot => body[..4].copy_from_slice(&REBOOT_MAGIC.to_le_bytes()),
            Request::SetName(s) => {
                if s.len() > MAX_NAME_LEN {
                    return Err(BuildError::TooLong);
                }
                body[0] = s.len() as u8;
                body[1..].copy_from_slice(s.as_bytes());
            }
            Request::SetWifi(w) => {
                if w.ssid.is_empty() || w.ssid.len() > MAX_SSID_LEN || w.psk.len() > MAX_PSK_LEN {
                    return Err(BuildError::TooLong);
                }
                let s = w.ssid.as_bytes();
                let p = w.psk.as_bytes();
                body[0] = s.len() as u8;
                body[1..1 + s.len()].copy_from_slice(s);
                body[1 + s.len()] = p.len() as u8;
                body[2 + s.len()..2 + s.len() + p.len()].copy_from_slice(p);
                body[2 + s.len() + p.len()] = u8::from(w.persist);
            }
        }
        Ok(crate::HEADER_LEN + n)
    }
}

/// Split `[u8 n][n bytes][rest]`. `None` if the length byte overruns.
fn split_u8_prefixed(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&n, rest) = b.split_first()?;
    let n = n as usize;
    if rest.len() < n {
        return None;
    }
    Some(rest.split_at(n))
}

// ---------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------

/// A control reply: device -> sender. Always written with `flags.REPLY` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply<'a> {
    /// [`op::PING`].
    Ping {
        /// Milliseconds since boot, wraps at 49.7 days.
        uptime_ms: u32,
    },
    /// [`op::GET_INFO`]. The body is DNS-SD TXT wire format; parse it with
    /// [`crate::txt::DeviceInfo::parse`].
    Info(&'a [u8]),
    /// [`op::TELEMETRY`].
    Telemetry(Telemetry),
    /// [`op::SET_BRIGHTNESS`]. `applied` is the value after the firmware cap.
    Brightness {
        /// Brightness actually in effect.
        applied: u8,
    },
    /// [`op::IDENTIFY`].
    Identify,
    /// [`op::SET_IDLE`]. Raw, because a future device may know modes we do not.
    Idle {
        /// The mode now in effect; interpret with [`IdleMode::from_u8`].
        mode: u8,
    },
    /// [`op::RESET_STATS`].
    ResetStats,
    /// [`op::RELEASE`].
    Release,
    /// [`op::SET_NAME`].
    SetName,
    /// [`op::GET_WIFI`]. Never carries the PSK - that invariant holds in every
    /// version of this protocol (section 8.4).
    Wifi {
        /// The stored SSID.
        ssid: &'a str,
        /// Join state; see [`wifi_state`].
        state: u8,
    },
    /// [`op::SET_WIFI`], sent *before* the device disconnects.
    SetWifi,
    /// [`op::BUSY`], unsolicited, `req_id == 0`.
    Busy {
        /// See [`busy_reason`].
        reason: u8,
        /// How much longer the current lock holder keeps the panel.
        lock_holder_ms_remaining: u32,
    },
    /// [`op::REBOOT`], sent before rebooting.
    Reboot,
    /// An error reply: `flags.REPLY|ERROR`, one body byte.
    Err {
        /// Raw code; interpret with [`ErrorCode::from_u8`].
        code: u8,
    },
}

impl<'a> Reply<'a> {
    /// The opcode this reply echoes.
    ///
    /// [`Reply::Err`] has no opcode of its own - an error reply echoes the
    /// request's - so it reports 0 and callers pass the opcode explicitly to
    /// [`Reply::write`].
    #[must_use]
    pub fn op(&self) -> u8 {
        match self {
            Reply::Ping { .. } => op::PING,
            Reply::Info(_) => op::GET_INFO,
            Reply::Telemetry(_) => op::TELEMETRY,
            Reply::Brightness { .. } => op::SET_BRIGHTNESS,
            Reply::Identify => op::IDENTIFY,
            Reply::Idle { .. } => op::SET_IDLE,
            Reply::ResetStats => op::RESET_STATS,
            Reply::Release => op::RELEASE,
            Reply::SetName => op::SET_NAME,
            Reply::Wifi { .. } => op::GET_WIFI,
            Reply::SetWifi => op::SET_WIFI,
            Reply::Busy { .. } => op::BUSY,
            Reply::Reboot => op::REBOOT,
            Reply::Err { .. } => 0,
        }
    }

    /// Body length on the wire.
    #[must_use]
    pub fn body_len(&self) -> usize {
        match self {
            Reply::Identify
            | Reply::ResetStats
            | Reply::Release
            | Reply::SetName
            | Reply::SetWifi
            | Reply::Reboot => 0,
            Reply::Brightness { .. } | Reply::Idle { .. } | Reply::Err { .. } => 1,
            Reply::Ping { .. } => 4,
            Reply::Busy { .. } => 5,
            Reply::Telemetry(_) => TELEMETRY_LEN,
            Reply::Info(b) => b.len(),
            Reply::Wifi { ssid, .. } => 2 + ssid.len(),
        }
    }

    /// Total datagram length.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        crate::HEADER_LEN.saturating_add(self.body_len())
    }

    /// Decode a reply a sender just received.
    ///
    /// `flags` comes from [`crate::ControlPacket::flags`]; the `ERROR` bit
    /// selects [`Reply::Err`] regardless of `op`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::BadLength`] if the body is not the opcode's size,
    /// [`ErrorCode::UnknownOp`] for an opcode this version does not know.
    pub fn decode(op: u8, flags: u8, body: &'a [u8]) -> Result<Self, ErrorCode> {
        if flags & C_ERROR != 0 {
            return match body {
                [code] => Ok(Reply::Err { code: *code }),
                _ => Err(ErrorCode::BadLength),
            };
        }
        let empty = |r: Reply<'a>| {
            if body.is_empty() {
                Ok(r)
            } else {
                Err(ErrorCode::BadLength)
            }
        };
        match op {
            op::PING => match body {
                [a, b, c, d] => Ok(Reply::Ping {
                    uptime_ms: u32::from_le_bytes([*a, *b, *c, *d]),
                }),
                _ => Err(ErrorCode::BadLength),
            },
            op::GET_INFO => Ok(Reply::Info(body)),
            op::TELEMETRY => Telemetry::decode(body).map(Reply::Telemetry),
            op::SET_BRIGHTNESS => match body {
                [applied] => Ok(Reply::Brightness { applied: *applied }),
                _ => Err(ErrorCode::BadLength),
            },
            op::IDENTIFY => empty(Reply::Identify),
            op::SET_IDLE => match body {
                [mode] => Ok(Reply::Idle { mode: *mode }),
                _ => Err(ErrorCode::BadLength),
            },
            op::RESET_STATS => empty(Reply::ResetStats),
            op::RELEASE => empty(Reply::Release),
            op::SET_NAME => empty(Reply::SetName),
            op::SET_WIFI => empty(Reply::SetWifi),
            op::REBOOT => empty(Reply::Reboot),
            op::GET_WIFI => {
                let (ssid, rest) = split_u8_prefixed(body).ok_or(ErrorCode::BadLength)?;
                let state = match rest {
                    [s] => *s,
                    _ => return Err(ErrorCode::BadLength),
                };
                Ok(Reply::Wifi {
                    ssid: core::str::from_utf8(ssid).map_err(|_| ErrorCode::BadArg)?,
                    state,
                })
            }
            op::BUSY => match body {
                [reason, a, b, c, d] => Ok(Reply::Busy {
                    reason: *reason,
                    lock_holder_ms_remaining: u32::from_le_bytes([*a, *b, *c, *d]),
                }),
                _ => Err(ErrorCode::BadLength),
            },
            _ => Err(ErrorCode::UnknownOp),
        }
    }

    /// Serialise a complete reply datagram into `out`.
    ///
    /// `op` is passed explicitly because an error reply echoes the request's
    /// opcode; for every other variant it must equal [`Reply::op`].
    ///
    /// # Errors
    ///
    /// See [`BuildError`].
    pub fn write(&self, op: u8, req_id: u16, out: &mut [u8]) -> Result<usize, BuildError> {
        let n = self.body_len();
        let flags = match self {
            Reply::Err { .. } => C_REPLY | C_ERROR,
            _ => C_REPLY,
        };
        let body = reserve_control(out, op, flags, req_id, n)?;
        match self {
            Reply::Identify
            | Reply::ResetStats
            | Reply::Release
            | Reply::SetName
            | Reply::SetWifi
            | Reply::Reboot => {}
            Reply::Brightness { applied } => body[0] = *applied,
            Reply::Idle { mode } => body[0] = *mode,
            Reply::Err { code } => body[0] = *code,
            Reply::Ping { uptime_ms } => body[..4].copy_from_slice(&uptime_ms.to_le_bytes()),
            Reply::Busy {
                reason,
                lock_holder_ms_remaining,
            } => {
                body[0] = *reason;
                body[1..5].copy_from_slice(&lock_holder_ms_remaining.to_le_bytes());
            }
            Reply::Telemetry(t) => {
                t.write(body)?;
            }
            Reply::Info(b) => body.copy_from_slice(b),
            Reply::Wifi { ssid, state } => {
                if ssid.len() > MAX_SSID_LEN {
                    return Err(BuildError::TooLong);
                }
                body[0] = ssid.len() as u8;
                body[1..1 + ssid.len()].copy_from_slice(ssid.as_bytes());
                body[1 + ssid.len()] = *state;
            }
        }
        Ok(crate::HEADER_LEN + n)
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// The 48-byte `TELEMETRY` body, section 6.7.
///
/// Counters are free-running since boot or the last `RESET_STATS` and wrap;
/// take deltas with `wrapping_sub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Telemetry {
    /// Milliseconds since boot; wraps at 49.7 days.
    pub uptime_ms: u32,
    /// `FRAME` datagrams accepted from the active source.
    pub frames_rx: u32,
    /// Frames actually pushed to the panel.
    pub frames_shown: u32,
    /// `seq` not newer than `last_seq` (duplicate or reordered).
    pub frames_dropped_stale: u32,
    /// A newer frame arrived before this one was displayed.
    pub frames_dropped_superseded: u32,
    /// Decode failed, unknown codec, or awaiting a `KEY` frame.
    pub frames_dropped_decode: u32,
    /// Bad magic/version/length, or not the active source.
    pub frames_rejected: u32,
    /// Total count of sequence numbers never seen.
    pub seq_gaps: u32,
    /// EWMA of per-frame inter-arrival time, saturating.
    pub interarrival_us: u16,
    /// EWMA of `|d_i - interarrival_us|`, section 6.8.
    pub jitter_us: u16,
    /// Max inter-arrival since reset, saturating.
    pub interarrival_max_us: u16,
    /// EWMA of decode time.
    pub decode_us: u16,
    /// Max decode time since reset.
    pub decode_us_max: u16,
    /// Max time to push a decoded frame to the panel buffer.
    pub render_us_max: u16,
    /// Last beacon RSSI, dBm.
    pub rssi_dbm: i8,
    /// Brightness currently applied, 0-255.
    pub brightness: u8,
    /// Stream state; see [`state`].
    pub state: u8,
    /// Codec id of the last frame shown.
    pub last_codec: u8,
}

impl Telemetry {
    /// Parse a telemetry body.
    ///
    /// Tolerates a body *longer* than 48 bytes - that is how section 6.7 says
    /// the struct grows - and rejects one shorter.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::BadLength`] if `body` is shorter than [`TELEMETRY_LEN`].
    pub fn decode(body: &[u8]) -> Result<Self, ErrorCode> {
        if body.len() < TELEMETRY_LEN {
            return Err(ErrorCode::BadLength);
        }
        let u32at = |o: usize| -> u32 {
            u32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]])
        };
        let u16at = |o: usize| -> u16 { u16::from_le_bytes([body[o], body[o + 1]]) };
        Ok(Telemetry {
            uptime_ms: u32at(0),
            frames_rx: u32at(4),
            frames_shown: u32at(8),
            frames_dropped_stale: u32at(12),
            frames_dropped_superseded: u32at(16),
            frames_dropped_decode: u32at(20),
            frames_rejected: u32at(24),
            seq_gaps: u32at(28),
            interarrival_us: u16at(32),
            jitter_us: u16at(34),
            interarrival_max_us: u16at(36),
            decode_us: u16at(38),
            decode_us_max: u16at(40),
            render_us_max: u16at(42),
            rssi_dbm: body[44] as i8,
            brightness: body[45],
            state: body[46],
            last_codec: body[47],
        })
    }

    /// Write the 48-byte body into the front of `out`.
    ///
    /// # Errors
    ///
    /// [`BuildError::BufferTooSmall`] if `out` is shorter than
    /// [`TELEMETRY_LEN`].
    pub fn write(&self, out: &mut [u8]) -> Result<usize, BuildError> {
        if out.len() < TELEMETRY_LEN {
            return Err(BuildError::BufferTooSmall);
        }
        out[0..4].copy_from_slice(&self.uptime_ms.to_le_bytes());
        out[4..8].copy_from_slice(&self.frames_rx.to_le_bytes());
        out[8..12].copy_from_slice(&self.frames_shown.to_le_bytes());
        out[12..16].copy_from_slice(&self.frames_dropped_stale.to_le_bytes());
        out[16..20].copy_from_slice(&self.frames_dropped_superseded.to_le_bytes());
        out[20..24].copy_from_slice(&self.frames_dropped_decode.to_le_bytes());
        out[24..28].copy_from_slice(&self.frames_rejected.to_le_bytes());
        out[28..32].copy_from_slice(&self.seq_gaps.to_le_bytes());
        out[32..34].copy_from_slice(&self.interarrival_us.to_le_bytes());
        out[34..36].copy_from_slice(&self.jitter_us.to_le_bytes());
        out[36..38].copy_from_slice(&self.interarrival_max_us.to_le_bytes());
        out[38..40].copy_from_slice(&self.decode_us.to_le_bytes());
        out[40..42].copy_from_slice(&self.decode_us_max.to_le_bytes());
        out[42..44].copy_from_slice(&self.render_us_max.to_le_bytes());
        out[44] = self.rssi_dbm as u8;
        out[45] = self.brightness;
        out[46] = self.state;
        out[47] = self.last_codec;
        Ok(TELEMETRY_LEN)
    }
}
