//! The 8-byte header, and `FRAME` / `CONTROL` parse and build.
//!
//! Spec: `docs/design/protocol-v1.md` sections 2, 3 and 6.1.
//!
//! ```text
//!  offset  size  frame packet        control packet
//!  ------  ----  ------------------  ---------------------
//!  0       1     magic = 0x53 's'    magic = 0x53 's'
//!  1       1     ver_type            ver_type
//!  2       1     codec               op
//!  3       1     flags               flags
//!  4       2     seq        (u16le)  req_id       (u16le)
//!  6       2     len        (u16le)  len          (u16le)
//!  8       ...   pixel payload       body
//! ```
//!
//! Parsing borrows from the caller's datagram buffer: no copies, no
//! allocation. Every accessor is bounds-checked at parse time, so a
//! [`FramePacket`] that exists is a packet whose `len` agreed with the
//! datagram it came from.

use crate::{C_ERROR, C_REPLY, F_HAS_TS, HEADER_LEN, MAGIC, MAX_PIXEL_PAYLOAD, TS_LEN, VERSION};

/// Packet type nibble for `FRAME`.
pub const TYPE_FRAME: u8 = 0x0;
/// Packet type nibble for `CONTROL`.
pub const TYPE_CONTROL: u8 = 0x2;

/// Byte 1 of a v1 `FRAME`.
pub const VER_TYPE_FRAME: u8 = (VERSION << 4) | TYPE_FRAME;
/// Byte 1 of a v1 `CONTROL`.
pub const VER_TYPE_CONTROL: u8 = (VERSION << 4) | TYPE_CONTROL;

/// Why a datagram was not a packet we will act on.
///
/// The variants map onto the telemetry counters: everything here is
/// `frames_rejected` on the device side, but the distinction is what a log
/// line needs to be useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reject {
    /// Fewer than 8 bytes: there is not even a header.
    Short,
    /// Byte 0 was not [`MAGIC`]. Discard without further parsing (section 2.1).
    BadMagic,
    /// Version nibble of byte 1 was not [`VERSION`] (section 2.2).
    BadVersion,
    /// Packet type nibble is reserved, or is not the type asked for.
    BadType,
    /// `8 + len` exceeded the datagram length (section 2.3).
    BadLength,
    /// A `FRAME` whose `len` exceeded [`MAX_PIXEL_PAYLOAD`] (section 2.3).
    TooLong,
    /// `HAS_TS` was set but `len` left no room for the 4-byte timestamp
    /// (section 3).
    ShortTimestamp,
}

/// Why a packet could not be written into the caller's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildError {
    /// The output slice is shorter than the packet.
    BufferTooSmall,
    /// The body would push the datagram past the protocol's limits.
    TooLong,
}

/// Either kind of v1 packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    /// A pixel frame.
    Frame(FramePacket<'a>),
    /// A control request, reply, or unsolicited device packet.
    Control(ControlPacket<'a>),
}

impl<'a> Packet<'a> {
    /// Parse any v1 packet out of one UDP datagram.
    ///
    /// Callers that know which port the datagram arrived on should normally
    /// use [`FramePacket::parse`] or [`ControlPacket::parse`], which enforce
    /// section 2.2's "discard a `FRAME` on the control port" rule for free.
    /// The exception is the frame socket of a *sender*, which must also accept
    /// the `TELEMETRY` reply described in section 6.4 - that one wants this
    /// function.
    ///
    /// # Errors
    ///
    /// [`Reject`] describes which of section 2's MUSTs the datagram failed.
    pub fn parse(datagram: &'a [u8]) -> Result<Self, Reject> {
        match header(datagram)?.ty {
            TYPE_FRAME => FramePacket::parse(datagram).map(Packet::Frame),
            TYPE_CONTROL => ControlPacket::parse(datagram).map(Packet::Control),
            _ => Err(Reject::BadType),
        }
    }

    /// The frame, if this is one.
    #[must_use]
    pub fn as_frame(self) -> Option<FramePacket<'a>> {
        match self {
            Packet::Frame(f) => Some(f),
            Packet::Control(_) => None,
        }
    }

    /// The control packet, if this is one.
    #[must_use]
    pub fn as_control(self) -> Option<ControlPacket<'a>> {
        match self {
            Packet::Control(c) => Some(c),
            Packet::Frame(_) => None,
        }
    }
}

/// The parts of the header that are common to both types.
struct Header {
    ty: u8,
    b2: u8,
    flags: u8,
    id: u16,
    len: usize,
}

/// Validate magic, version and `len` against the datagram. Section 2.
fn header(d: &[u8]) -> Result<Header, Reject> {
    if d.len() < HEADER_LEN {
        return Err(Reject::Short);
    }
    if d[0] != MAGIC {
        return Err(Reject::BadMagic);
    }
    if d[1] >> 4 != VERSION {
        return Err(Reject::BadVersion);
    }
    let len = u16::from_le_bytes([d[6], d[7]]) as usize;
    // `HEADER_LEN + len` cannot overflow: len is at most 65535.
    if HEADER_LEN + len > d.len() {
        return Err(Reject::BadLength);
    }
    Ok(Header {
        ty: d[1] & 0x0f,
        b2: d[2],
        flags: d[3],
        id: u16::from_le_bytes([d[4], d[5]]),
        len,
    })
}

/// Reserve a `CONTROL` datagram of `body_len` bytes in `out`, write its
/// header, and hand back the body slice for the caller to fill.
pub(crate) fn reserve_control(
    out: &mut [u8],
    op: u8,
    flags: u8,
    req_id: u16,
    body_len: usize,
) -> Result<&mut [u8], BuildError> {
    if body_len > MAX_PIXEL_PAYLOAD {
        return Err(BuildError::TooLong);
    }
    let total = HEADER_LEN + body_len;
    if out.len() < total {
        return Err(BuildError::BufferTooSmall);
    }
    put_header(out, VER_TYPE_CONTROL, op, flags, req_id, body_len);
    Ok(&mut out[HEADER_LEN..total])
}

/// Write the common header. `out` is known to be at least [`HEADER_LEN`] long.
fn put_header(out: &mut [u8], ver_type: u8, b2: u8, flags: u8, id: u16, len: usize) {
    out[0] = MAGIC;
    out[1] = ver_type;
    out[2] = b2;
    out[3] = flags;
    out[4..6].copy_from_slice(&id.to_le_bytes());
    // `len` is checked by the caller to fit in a u16.
    out[6..8].copy_from_slice(&(len as u16).to_le_bytes());
}

// ---------------------------------------------------------------------------
// FRAME
// ---------------------------------------------------------------------------

/// A `FRAME` packet: section 3.
///
/// The same type parses and builds. `timestamp_us` is the single source of
/// truth for the `HAS_TS` flag - [`FramePacket::write`] sets or clears the bit
/// to match, so an inconsistent packet cannot be built - and `payload` is
/// always the *pixel* payload with any timestamp prefix already removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePacket<'a> {
    /// Codec id, byte 2. See [`crate::dec::codec`].
    pub codec: u8,
    /// Flag bits, byte 3. `HAS_TS` is derived on write and informational on read.
    pub flags: u8,
    /// Sequence number, wraps mod 2^16.
    pub seq: u16,
    /// Sender timestamp in microseconds, present iff `HAS_TS` was set.
    pub timestamp_us: Option<u32>,
    /// The codec payload, `HAS_TS` prefix stripped.
    pub payload: &'a [u8],
}

impl<'a> FramePacket<'a> {
    /// Parse a `FRAME` out of one UDP datagram.
    ///
    /// A `CONTROL` packet is rejected with [`Reject::BadType`], which is
    /// exactly section 2.2's rule for the device's frame socket.
    ///
    /// # Errors
    ///
    /// See [`Reject`].
    pub fn parse(datagram: &'a [u8]) -> Result<Self, Reject> {
        let h = header(datagram)?;
        if h.ty != TYPE_FRAME {
            return Err(Reject::BadType);
        }
        if h.len > MAX_PIXEL_PAYLOAD {
            return Err(Reject::TooLong);
        }
        let body = &datagram[HEADER_LEN..HEADER_LEN + h.len];
        let (timestamp_us, payload) = if h.flags & F_HAS_TS != 0 {
            if body.len() < TS_LEN {
                return Err(Reject::ShortTimestamp);
            }
            let (ts, rest) = body.split_at(TS_LEN);
            (Some(u32::from_le_bytes([ts[0], ts[1], ts[2], ts[3]])), rest)
        } else {
            (None, body)
        };
        Ok(FramePacket {
            codec: h.b2,
            flags: h.flags,
            seq: h.id,
            timestamp_us,
            payload,
        })
    }

    /// Total datagram length this packet will occupy.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER_LEN + self.body_len()
    }

    fn body_len(&self) -> usize {
        // payload.len() is bounded by the caller's buffer; saturating keeps
        // `encoded_len` honest for an over-long payload that `write` rejects.
        self.payload
            .len()
            .saturating_add(if self.timestamp_us.is_some() {
                TS_LEN
            } else {
                0
            })
    }

    /// Serialise into `out`, returning the datagram length.
    ///
    /// The `HAS_TS` bit of `flags` is ignored and recomputed from
    /// `timestamp_us`.
    ///
    /// # Errors
    ///
    /// [`BuildError::TooLong`] if the body would exceed
    /// [`MAX_PIXEL_PAYLOAD`], [`BuildError::BufferTooSmall`] if `out` is
    /// shorter than [`FramePacket::encoded_len`].
    pub fn write(&self, out: &mut [u8]) -> Result<usize, BuildError> {
        let body = self.body_len();
        if body > MAX_PIXEL_PAYLOAD {
            return Err(BuildError::TooLong);
        }
        let total = HEADER_LEN + body;
        if out.len() < total {
            return Err(BuildError::BufferTooSmall);
        }
        let flags = match self.timestamp_us {
            Some(_) => self.flags | F_HAS_TS,
            None => self.flags & !F_HAS_TS,
        };
        put_header(out, VER_TYPE_FRAME, self.codec, flags, self.seq, body);
        let mut at = HEADER_LEN;
        if let Some(ts) = self.timestamp_us {
            out[at..at + TS_LEN].copy_from_slice(&ts.to_le_bytes());
            at += TS_LEN;
        }
        out[at..at + self.payload.len()].copy_from_slice(self.payload);
        Ok(total)
    }

    /// `flags.KEY` - this frame decodes standalone.
    #[must_use]
    pub fn is_key(&self) -> bool {
        self.flags & crate::F_KEY != 0
    }
    /// `flags.STATS_REQ` - reply with one `TELEMETRY` after processing.
    #[must_use]
    pub fn wants_stats(&self) -> bool {
        self.flags & crate::F_STATS_REQ != 0
    }
    /// `flags.FINAL` - last frame of this stream; release the lock.
    #[must_use]
    pub fn is_final(&self) -> bool {
        self.flags & crate::F_FINAL != 0
    }
    /// `flags.HAS_TS` - equivalent to `timestamp_us.is_some()`.
    #[must_use]
    pub fn has_ts(&self) -> bool {
        self.flags & F_HAS_TS != 0
    }
}

// ---------------------------------------------------------------------------
// CONTROL
// ---------------------------------------------------------------------------

/// A `CONTROL` packet: section 6.1.
///
/// This is the envelope only. [`crate::control::Request`] and
/// [`crate::control::Reply`] give `op` and `body` their meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPacket<'a> {
    /// Opcode, byte 2. See [`crate::control::op`].
    pub op: u8,
    /// Flag bits, byte 3: `REPLY` and `ERROR`.
    pub flags: u8,
    /// Chosen by the requester and echoed in the reply. 0 means "no reply wanted".
    pub req_id: u16,
    /// Opcode-specific body.
    pub body: &'a [u8],
}

impl<'a> ControlPacket<'a> {
    /// Parse a `CONTROL` out of one UDP datagram.
    ///
    /// A `FRAME` is rejected with [`Reject::BadType`], which is section 2.2's
    /// rule for the control socket.
    ///
    /// # Errors
    ///
    /// See [`Reject`].
    pub fn parse(datagram: &'a [u8]) -> Result<Self, Reject> {
        let h = header(datagram)?;
        if h.ty != TYPE_CONTROL {
            return Err(Reject::BadType);
        }
        Ok(ControlPacket {
            op: h.b2,
            flags: h.flags,
            req_id: h.id,
            body: &datagram[HEADER_LEN..HEADER_LEN + h.len],
        })
    }

    /// Total datagram length this packet will occupy.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER_LEN.saturating_add(self.body.len())
    }

    /// Serialise into `out`, returning the datagram length.
    ///
    /// # Errors
    ///
    /// [`BuildError::TooLong`] if the body does not fit a UDP datagram we are
    /// willing to send, [`BuildError::BufferTooSmall`] if `out` is too short.
    pub fn write(&self, out: &mut [u8]) -> Result<usize, BuildError> {
        let total = self.encoded_len();
        if self.body.len() > MAX_PIXEL_PAYLOAD {
            return Err(BuildError::TooLong);
        }
        if out.len() < total {
            return Err(BuildError::BufferTooSmall);
        }
        put_header(
            out,
            VER_TYPE_CONTROL,
            self.op,
            self.flags,
            self.req_id,
            self.body.len(),
        );
        out[HEADER_LEN..total].copy_from_slice(self.body);
        Ok(total)
    }

    /// `flags.REPLY`.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        self.flags & C_REPLY != 0
    }
    /// `flags.ERROR`. Only meaningful together with `REPLY`.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.flags & C_ERROR != 0
    }
}

// ---------------------------------------------------------------------------
// Sequence numbers (section 3.2)
// ---------------------------------------------------------------------------

/// True if `a` is newer than `b` under mod-2^16 wrapping (RFC 1982).
///
/// Equal sequence numbers are *not* newer, so a duplicate is rejected.
#[inline]
#[must_use]
pub fn newer(a: u16, b: u16) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 0x8000
}

/// How many sequence numbers were skipped between `last` and `new`.
///
/// This is the value section 3.2 says to add to `seq_gaps`. It is 0 unless
/// `new` is strictly newer than `last`, so feeding it a duplicate or a
/// reordered frame cannot manufacture a 65534-frame gap.
#[inline]
#[must_use]
pub fn gap(new: u16, last: u16) -> u16 {
    if newer(new, last) {
        new.wrapping_sub(last).wrapping_sub(1)
    } else {
        0
    }
}
