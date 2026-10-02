//! Improv Wi-Fi over serial (card 362): the device half of the protocol that
//! ESP Web Tools speaks to a freshly flashed board, as a pure parser,
//! encoders and a session that translates [`crate::Provisioner`]'s progress.
//!
//! Sources, read rather than recalled: the spec at
//! <https://www.improv-wifi.com/serial/> and the reference implementation
//! `improv-wifi/sdk-cpp` (`improv.h`, `improv.cpp`). ESPHome's
//! `improv_serial_component.cpp` is the model for the device side's habits
//! (state replies on request, a newline after every frame, an empty scan
//! terminator).
//!
//! # Wire format
//!
//! ```text
//! "IMPROV" | version=1 | type | len | data[len] | checksum
//! checksum = sum of every preceding byte of the frame, mod 256
//! type: 1 current state, 2 error state, 3 RPC command, 4 RPC result
//! RPC command data : cmd | n | n bytes          (n = bytes after these two)
//! RPC result data  : cmd | n | (len | bytes)*   (strings, one-byte lengths)
//! WIFI_SETTINGS    : cmd 1, n, ssid_len, ssid, pwd_len, pwd
//! ```
//!
//! The wire is shared with the device's log text, so the [`Parser`] hunts for
//! the header in a stream of arbitrary bytes and resynchronises on anything
//! that does not fit.
//!
//! # The password
//!
//! It exists in exactly two places: the [`Parser`]'s frame buffer, which is
//! zeroed as soon as the next byte arrives after the frame (or on
//! [`Parser::wipe`]), and the [`Submit`] the session hands the caller, which
//! borrows from it. The [`Session`] never stores it, no encoder takes one
//! (except the client-side [`encode_wifi_settings`]), and [`Rpc`] and
//! [`Submit`] print lengths only.

use core::fmt;

/// The six header bytes.
pub const HEADER: &[u8; 6] = b"IMPROV";
/// The protocol version this implements.
pub const VERSION: u8 = 1;

/// Largest RPC data (`cmd | n | payload`) the parser keeps: a `WIFI_SETTINGS`
/// with a 32-byte SSID and a 64-byte password is 2 + 1 + 32 + 1 + 64 = 100.
/// Longer frames are skipped (see [`Parsed::Invalid`]).
pub const MAX_DATA: usize = 100;
/// The parser's frame buffer: header, version, type, length, data, checksum.
pub const MAX_FRAME: usize = 9 + MAX_DATA + 1;

/// Frame types.
pub mod ty {
    /// Device to client: current state.
    pub const CURRENT_STATE: u8 = 0x01;
    /// Device to client: error state.
    pub const ERROR_STATE: u8 = 0x02;
    /// Client to device: RPC command.
    pub const RPC_COMMAND: u8 = 0x03;
    /// Device to client: RPC result.
    pub const RPC_RESULT: u8 = 0x04;
}

/// RPC command ids.
pub mod cmd {
    /// Send Wi-Fi settings.
    pub const WIFI_SETTINGS: u8 = 0x01;
    /// Request current state.
    pub const GET_CURRENT_STATE: u8 = 0x02;
    /// Request device information.
    pub const GET_DEVICE_INFO: u8 = 0x03;
    /// Request scanned Wi-Fi networks.
    pub const GET_WIFI_NETWORKS: u8 = 0x04;
}

/// The device's provisioning state (frame type 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Provisioning is unavailable.
    Stopped = 0x00,
    /// Waiting for a physical authorization. This firmware never reports it:
    /// holding the USB cable is the authorization.
    AwaitingAuthorization = 0x01,
    /// Ready to take credentials.
    Authorized = 0x02,
    /// A join is in progress.
    Provisioning = 0x03,
    /// Joined.
    Provisioned = 0x04,
}

impl State {
    /// From the wire byte.
    #[must_use]
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => State::Stopped,
            1 => State::AwaitingAuthorization,
            2 => State::Authorized,
            3 => State::Provisioning,
            4 => State::Provisioned,
            _ => return None,
        })
    }
}

/// The error state (frame type 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    /// Clears the error.
    None = 0x00,
    /// Malformed RPC packet, or bad checksum.
    InvalidRpc = 0x01,
    /// Unknown RPC command id.
    UnknownRpc = 0x02,
    /// The join failed.
    UnableToConnect = 0x03,
    /// Not authorized.
    NotAuthorized = 0x04,
    /// Bad hostname.
    BadHostname = 0x05,
    /// Anything else.
    Unknown = 0xFF,
}

/// The checksum of `bytes`: their sum mod 256.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// A decoded RPC command, borrowing the parser's buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Rpc<'a> {
    /// Command 1. The password is the borrowed secret: see the module docs.
    WifiSettings {
        /// The network name, 0 to 255 bytes as sent (the session range-checks).
        ssid: &'a [u8],
        /// The password, or empty for an open network.
        password: &'a [u8],
    },
    /// Command 2.
    GetCurrentState,
    /// Command 3.
    GetDeviceInfo,
    /// Command 4.
    GetWifiNetworks,
    /// Any other command id (hostname, device name, network state, ...).
    Unknown(u8),
}

impl fmt::Debug for Rpc<'_> {
    // Lengths only: a derived Debug would print the password bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rpc::WifiSettings { ssid, password } => f
                .debug_struct("WifiSettings")
                .field("ssid_len", &ssid.len())
                .field("psk_len", &password.len())
                .finish(),
            Rpc::GetCurrentState => f.write_str("GetCurrentState"),
            Rpc::GetDeviceInfo => f.write_str("GetDeviceInfo"),
            Rpc::GetWifiNetworks => f.write_str("GetWifiNetworks"),
            Rpc::Unknown(c) => f.debug_tuple("Unknown").field(c).finish(),
        }
    }
}

/// What a complete frame turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// A well-formed RPC command.
    Rpc(Rpc<'a>),
    /// A frame that must be answered with [`ErrorCode::InvalidRpc`]: bad
    /// checksum, an RPC whose inner lengths disagree, or a frame longer than
    /// [`MAX_DATA`] (refused at its length byte, and its tail skipped without
    /// being interpreted).
    Invalid,
    /// A well-formed frame of a type a device does not act on (a stray
    /// state, error or result echoed back). No reply.
    Ignored(u8),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Collecting `buf[..len]`.
    Collect,
    /// A frame completed; its bytes are still in `buf` and are zeroed on the
    /// next push.
    Done,
    /// Discarding the tail of an oversized frame.
    Skip(u16),
}

/// A byte-at-a-time Improv frame parser that resynchronises on garbage.
///
/// ```
/// use screeny_provision::improv::{Parser, Parsed, Rpc};
/// let mut p = Parser::new();
/// let mut got = false;
/// for &b in b"log text IMPROV\x01\x03\x02\x02\x00\xe5 after" {
///     if let Some(Parsed::Rpc(Rpc::GetCurrentState)) = p.push(b) { got = true; }
/// }
/// assert!(got);
/// ```
#[derive(Clone)]
pub struct Parser {
    buf: [u8; MAX_FRAME],
    len: usize,
    mode: Mode,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Parser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parser").field("len", &self.len).finish()
    }
}

impl Parser {
    /// An empty parser.
    #[must_use]
    pub const fn new() -> Self {
        Parser { buf: [0; MAX_FRAME], len: 0, mode: Mode::Collect }
    }

    /// Zero the buffer and forget any partial frame.
    pub fn wipe(&mut self) {
        self.buf.fill(0);
        self.len = 0;
        self.mode = Mode::Collect;
    }

    /// Whether the buffer holds nothing (so there is nothing to wipe).
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.len == 0 && self.mode == Mode::Collect
    }

    /// Feed one byte. Returns `Some` when it completed a frame (or when an
    /// oversized frame began and was refused).
    pub fn push(&mut self, b: u8) -> Option<Parsed<'_>> {
        match self.mode {
            Mode::Done => self.wipe(),
            Mode::Skip(n) => {
                self.mode = if n <= 1 { Mode::Collect } else { Mode::Skip(n - 1) };
                return None;
            }
            Mode::Collect => {}
        }

        let pos = self.len;
        // Header, then version.
        let expected = if pos < 6 {
            Some(HEADER[pos])
        } else if pos == 6 {
            Some(VERSION)
        } else {
            None
        };
        if let Some(want) = expected {
            if b == want {
                self.buf[pos] = b;
                self.len += 1;
            } else {
                // Resync. 'I' occurs once in the header, so "restart at the
                // byte that broke the match if it is 'I'" is exact.
                self.wipe();
                if b == HEADER[0] {
                    self.buf[0] = b;
                    self.len = 1;
                }
            }
            return None;
        }

        self.buf[pos] = b;
        self.len += 1;
        if pos == 8 {
            // The length byte.
            if b as usize > MAX_DATA {
                self.wipe();
                // `b` data bytes and the checksum are still to come.
                self.mode = Mode::Skip(u16::from(b) + 1);
                return Some(Parsed::Invalid);
            }
            return None;
        }
        if pos < 8 {
            return None;
        }
        let total = 9 + self.buf[8] as usize + 1;
        if self.len < total {
            return None;
        }
        self.mode = Mode::Done;
        Some(self.finish(total))
    }

    fn finish(&self, total: usize) -> Parsed<'_> {
        let frame = &self.buf[..total];
        if checksum(&frame[..total - 1]) != frame[total - 1] {
            return Parsed::Invalid;
        }
        let ty = frame[7];
        if ty != ty::RPC_COMMAND {
            return Parsed::Ignored(ty);
        }
        let data = &frame[9..total - 1];
        if data.len() < 2 || data[1] as usize != data.len() - 2 {
            return Parsed::Invalid;
        }
        let payload = &data[2..];
        match data[0] {
            cmd::WIFI_SETTINGS => {
                let Some((&sl, rest)) = payload.split_first() else {
                    return Parsed::Invalid;
                };
                let sl = sl as usize;
                if rest.len() < sl + 1 {
                    return Parsed::Invalid;
                }
                let (ssid, rest) = rest.split_at(sl);
                let (&pl, pw) = rest.split_first().unwrap_or((&0, &[]));
                if pw.len() != pl as usize {
                    return Parsed::Invalid;
                }
                Parsed::Rpc(Rpc::WifiSettings { ssid, password: pw })
            }
            cmd::GET_CURRENT_STATE => Parsed::Rpc(Rpc::GetCurrentState),
            cmd::GET_DEVICE_INFO => Parsed::Rpc(Rpc::GetDeviceInfo),
            cmd::GET_WIFI_NETWORKS => Parsed::Rpc(Rpc::GetWifiNetworks),
            other => Parsed::Rpc(Rpc::Unknown(other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Client-side scanning
// ---------------------------------------------------------------------------

/// What [`scan_frame`] found at the front of a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan<'a> {
    /// A complete, checksummed frame. Drop `used` bytes from the buffer.
    Frame {
        /// Frame type ([`ty`]).
        ty: u8,
        /// The frame's data, without header or checksum.
        data: &'a [u8],
        /// Bytes to drop from the front of the buffer.
        used: usize,
    },
    /// Nothing usable yet: drop `skip` bytes (garbage, log text, a corrupt
    /// frame) and wait for more. `skip` never covers a possible frame start.
    Need {
        /// Bytes safe to drop.
        skip: usize,
    },
}

/// Find the first Improv frame in `buf`, for a *client* reading a device's
/// stream (which carries frames of every type, unlike the device-side
/// [`Parser`] that only keeps commands). A frame that fails its checksum is
/// skipped one byte at a time, so a real frame that began inside it is still
/// found.
#[must_use]
pub fn scan_frame(buf: &[u8]) -> Scan<'_> {
    let Some(i) = buf.windows(6).position(|w| w == HEADER) else {
        // Keep a tail that could be the start of a header.
        let keep = buf
            .iter()
            .rposition(|&b| b == HEADER[0])
            .map_or(0, |p| buf.len() - p);
        return Scan::Need { skip: buf.len() - keep };
    };
    let f = &buf[i..];
    if f.len() < 9 {
        return Scan::Need { skip: i };
    }
    if f[6] != VERSION {
        return Scan::Need { skip: i + 1 };
    }
    let total = 9 + f[8] as usize + 1;
    if f.len() < total {
        return Scan::Need { skip: i };
    }
    if checksum(&f[..total - 1]) != f[total - 1] {
        return Scan::Need { skip: i + 1 };
    }
    Scan::Frame { ty: f[7], data: &f[9..total - 1], used: i + total }
}

// ---------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------

/// An encoder ran out of room or was given an oversized string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeError;

fn begin(out: &mut [u8], ty: u8, data_len: usize) -> Result<usize, EncodeError> {
    if data_len > 255 || out.len() < 9 + data_len + 1 {
        return Err(EncodeError);
    }
    out[..6].copy_from_slice(HEADER);
    out[6] = VERSION;
    out[7] = ty;
    out[8] = data_len as u8;
    Ok(9)
}

fn seal(out: &mut [u8], end: usize) -> usize {
    out[end] = checksum(&out[..end]);
    end + 1
}

/// Encode a current-state frame into `out` (10 bytes needed); returns its length.
pub fn encode_state(out: &mut [u8], s: State) -> Result<usize, EncodeError> {
    let at = begin(out, ty::CURRENT_STATE, 1)?;
    out[at] = s as u8;
    Ok(seal(out, at + 1))
}

/// Encode an error-state frame into `out` (10 bytes needed); returns its length.
pub fn encode_error(out: &mut [u8], e: ErrorCode) -> Result<usize, EncodeError> {
    let at = begin(out, ty::ERROR_STATE, 1)?;
    out[at] = e as u8;
    Ok(seal(out, at + 1))
}

/// Encode an RPC result for command `command` carrying `strings`, each with a
/// one-byte length prefix. No strings is the scan terminator.
pub fn encode_rpc_result(
    out: &mut [u8],
    command: u8,
    strings: &[&[u8]],
) -> Result<usize, EncodeError> {
    let mut inner = 0usize;
    for s in strings {
        if s.len() > 255 {
            return Err(EncodeError);
        }
        inner += 1 + s.len();
    }
    if inner > 255 {
        return Err(EncodeError);
    }
    let mut at = begin(out, ty::RPC_RESULT, 2 + inner)?;
    out[at] = command;
    out[at + 1] = inner as u8;
    at += 2;
    for s in strings {
        out[at] = s.len() as u8;
        out[at + 1..at + 1 + s.len()].copy_from_slice(s);
        at += 1 + s.len();
    }
    Ok(seal(out, at))
}

/// Encode a client-to-device RPC command, for host tools and tests. `payload`
/// is the command's own bytes (empty for the three queries).
pub fn encode_rpc_command(
    out: &mut [u8],
    command: u8,
    payload: &[u8],
) -> Result<usize, EncodeError> {
    if payload.len() > 253 {
        return Err(EncodeError);
    }
    let mut at = begin(out, ty::RPC_COMMAND, 2 + payload.len())?;
    out[at] = command;
    out[at + 1] = payload.len() as u8;
    at += 2;
    out[at..at + payload.len()].copy_from_slice(payload);
    Ok(seal(out, at + payload.len()))
}

/// Encode a `WIFI_SETTINGS` command, for tests and for a host client that
/// reads the password from a prompt.
pub fn encode_wifi_settings(
    out: &mut [u8],
    ssid: &[u8],
    password: &[u8],
) -> Result<usize, EncodeError> {
    if 2 + ssid.len() + password.len() > 253 {
        return Err(EncodeError);
    }
    let mut payload = [0u8; 253];
    payload[0] = ssid.len() as u8;
    payload[1..=ssid.len()].copy_from_slice(ssid);
    payload[1 + ssid.len()] = password.len() as u8;
    let end = 2 + ssid.len() + password.len();
    payload[2 + ssid.len()..end].copy_from_slice(password);
    let r = encode_rpc_command(out, cmd::WIFI_SETTINGS, &payload[..end]);
    payload.fill(0);
    r
}

/// Split the data of an RPC result (`cmd | n | strings`) for a client: calls
/// `f` per string and returns the command id; `None` when the lengths disagree.
pub fn for_each_result_string<'a>(data: &'a [u8], mut f: impl FnMut(&'a [u8])) -> Option<u8> {
    if data.len() < 2 || data[1] as usize != data.len() - 2 {
        return None;
    }
    let mut rest = &data[2..];
    while let Some((&l, tail)) = rest.split_first() {
        if tail.len() < l as usize {
            return None;
        }
        let (s, t) = tail.split_at(l as usize);
        f(s);
        rest = t;
    }
    Some(data[0])
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Who this device says it is, for command 3.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo<'a> {
    /// Firmware name, e.g. `screeny-fw`.
    pub firmware: &'a str,
    /// Firmware version.
    pub version: &'a str,
    /// Chip, e.g. `ESP32`.
    pub chip: &'a str,
    /// Device name: the mDNS instance.
    pub name: &'a str,
}

/// What the join machine looks like from here: the few facts the session needs,
/// copied out of the [`crate::Provisioner`] (see [`Observed::from_machine`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    /// The machine's state.
    pub state: crate::State,
    /// The last trial's outcome, if there has been one.
    pub trial: Option<crate::TrialOutcome>,
    /// The address, if the machine holds one.
    pub ip: Option<[u8; 4]>,
}

impl Observed {
    /// Read the facts off a machine.
    #[must_use]
    pub fn from_machine(p: &crate::Provisioner) -> Self {
        Observed {
            state: p.state(),
            trial: p.trial().map(|t| t.outcome),
            ip: p.ip(),
        }
    }
}

/// One thing for the session's owner to put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// A state frame.
    State(State),
    /// An error frame.
    Error(ErrorCode),
    /// Command 3's result.
    DeviceInfo,
    /// Command 4's terminator: no networks listed (the firmware's `improv.rs`
    /// says why).
    ScanEnd,
    /// An RPC result carrying the URL of the device's page, `http://a.b.c.d/`,
    /// for the command id given: 1 after a join, 2 for a state request made
    /// once provisioned.
    Url([u8; 4], u8),
}

impl Reply {
    /// Encode into `out` (needs [`REPLY_MAX`] bytes); returns the length.
    pub fn encode(&self, info: &DeviceInfo<'_>, out: &mut [u8]) -> Result<usize, EncodeError> {
        match *self {
            Reply::State(s) => encode_state(out, s),
            Reply::Error(e) => encode_error(out, e),
            Reply::DeviceInfo => encode_rpc_result(
                out,
                cmd::GET_DEVICE_INFO,
                &[
                    info.firmware.as_bytes(),
                    info.version.as_bytes(),
                    info.chip.as_bytes(),
                    info.name.as_bytes(),
                ],
            ),
            Reply::ScanEnd => encode_rpc_result(out, cmd::GET_WIFI_NETWORKS, &[]),
            Reply::Url(ip, c) => {
                let mut url = [0u8; URL_MAX];
                let n = url_text(ip, &mut url);
                encode_rpc_result(out, c, &[&url[..n]])
            }
        }
    }
}

/// Room [`Reply::encode`] may need: a device-info result with four strings of
/// up to 40 bytes each, plus framing.
pub const REPLY_MAX: usize = 9 + 2 + 4 * 41 + 1;

/// Longest [`url_text`].
pub const URL_MAX: usize = 23;

/// `http://a.b.c.d/` into `out`; returns its length.
pub fn url_text(ip: [u8; 4], out: &mut [u8; URL_MAX]) -> usize {
    let mut n = 0;
    for &b in b"http://" {
        out[n] = b;
        n += 1;
    }
    for (i, &o) in ip.iter().enumerate() {
        if i > 0 {
            out[n] = b'.';
            n += 1;
        }
        let digits = [o / 100, o / 10 % 10, o % 10];
        let skip = if o >= 100 {
            0
        } else if o >= 10 {
            1
        } else {
            2
        };
        for &d in &digits[skip..] {
            out[n] = b'0' + d;
            n += 1;
        }
    }
    out[n] = b'/';
    n + 1
}

/// Credentials the session wants joined. Borrowed from the [`Parser`]; the
/// caller copies them into the provisioning path (`NEW_WIFI` in the
/// firmware, `post_credentials` in the simulator) and drops the borrow.
/// `Debug` prints lengths only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Submit<'a> {
    /// The network name, 1 to 32 bytes.
    pub ssid: &'a [u8],
    /// The password, 0 to 64 bytes.
    pub password: &'a [u8],
}

impl fmt::Debug for Submit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Submit")
            .field("ssid_len", &self.ssid.len())
            .field("psk_len", &self.password.len())
            .finish()
    }
}

/// Replies one call produces.
pub type Replies = heapless::Vec<Reply, 4>;

/// How long after a submit the machine may take to show it is trying (the
/// firmware defers the post by a few hundred ms).
pub const ENTER_TRIAL_MS: u32 = 5_000;
/// How long a submit may take in all before the session gives up. The machine's
/// own bound is three 15 s attempts.
pub const GIVE_UP_MS: u32 = 90_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Submitted; waiting for the machine to enter `Trial`.
    Submitted { at: u32 },
    /// The machine is in `Trial`.
    Trying { since: u32 },
}

/// The Improv conversation: RPC commands in, replies out, one submit at a time.
///
/// The session never holds credentials; it knows only that a submit is in
/// flight. The *outcome* is the machine's: the session reads [`Observed`] and
/// translates, so a failed join falls back to the previous network exactly as
/// it does for the portal and `SET_WIFI`.
#[derive(Debug, Clone)]
pub struct Session {
    phase: Phase,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// An idle session.
    #[must_use]
    pub const fn new() -> Self {
        Session { phase: Phase::Idle }
    }

    /// Whether a submit is awaiting its outcome.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.phase != Phase::Idle
    }

    fn state_for(&self, o: &Observed) -> State {
        if self.busy() {
            return State::Provisioning;
        }
        match o.state {
            crate::State::Online if o.ip.is_some() => State::Provisioned,
            crate::State::Joining | crate::State::Trial => State::Provisioning,
            _ => State::Authorized,
        }
    }

    /// Answer one parsed frame. A `WIFI_SETTINGS` that passes its range checks
    /// returns the [`Submit`] for the caller to route into the provisioning path.
    pub fn handle<'a>(
        &mut self,
        parsed: Parsed<'a>,
        o: &Observed,
        now_ms: u32,
        out: &mut Replies,
    ) -> Option<Submit<'a>> {
        let rpc = match parsed {
            Parsed::Ignored(_) => return None,
            Parsed::Invalid => {
                let _ = out.push(Reply::Error(ErrorCode::InvalidRpc));
                return None;
            }
            Parsed::Rpc(r) => r,
        };
        // The spec: the device clears the error state on receiving a command.
        let _ = out.push(Reply::Error(ErrorCode::None));
        match rpc {
            Rpc::GetCurrentState => {
                let s = self.state_for(o);
                let _ = out.push(Reply::State(s));
                if let (State::Provisioned, Some(ip)) = (s, o.ip) {
                    let _ = out.push(Reply::Url(ip, cmd::GET_CURRENT_STATE));
                }
                None
            }
            Rpc::GetDeviceInfo => {
                let _ = out.push(Reply::DeviceInfo);
                None
            }
            Rpc::GetWifiNetworks => {
                let _ = out.push(Reply::ScanEnd);
                None
            }
            Rpc::Unknown(_) => {
                let _ = out.push(Reply::Error(ErrorCode::UnknownRpc));
                None
            }
            Rpc::WifiSettings { ssid, password } => {
                if ssid.is_empty()
                    || ssid.len() > crate::SSID_MAX
                    || password.len() > screeny_proto::control::MAX_PSK_LEN
                {
                    let _ = out.push(Reply::Error(ErrorCode::InvalidRpc));
                    return None;
                }
                if self.busy() {
                    // One at a time: the first answer is still owed.
                    let _ = out.push(Reply::Error(ErrorCode::Unknown));
                    return None;
                }
                self.phase = Phase::Submitted { at: now_ms };
                let _ = out.push(Reply::State(State::Provisioning));
                Some(Submit { ssid, password })
            }
        }
    }

    /// Call every few hundred milliseconds: turns the machine's progress into
    /// the frames the client is waiting for.
    pub fn poll(&mut self, o: &Observed, now_ms: u32, out: &mut Replies) {
        match self.phase {
            Phase::Idle => {}
            Phase::Submitted { at } => {
                if o.state == crate::State::Trial {
                    self.phase = Phase::Trying { since: now_ms };
                } else if now_ms.wrapping_sub(at) > ENTER_TRIAL_MS {
                    // The post never reached the machine, or it was refused.
                    self.fail(ErrorCode::Unknown, out);
                }
            }
            Phase::Trying { since } => match o.trial {
                Some(crate::TrialOutcome::Connected) => {
                    self.phase = Phase::Idle;
                    let _ = out.push(Reply::State(State::Provisioned));
                    if let Some(ip) = o.ip {
                        let _ = out.push(Reply::Url(ip, cmd::WIFI_SETTINGS));
                    }
                }
                Some(crate::TrialOutcome::Failed(_)) => self.fail(ErrorCode::UnableToConnect, out),
                _ => {
                    if now_ms.wrapping_sub(since) > GIVE_UP_MS {
                        self.fail(ErrorCode::UnableToConnect, out);
                    }
                }
            },
        }
    }

    fn fail(&mut self, e: ErrorCode, out: &mut Replies) {
        self.phase = Phase::Idle;
        let _ = out.push(Reply::Error(e));
        let _ = out.push(Reply::State(State::Authorized));
    }
}
