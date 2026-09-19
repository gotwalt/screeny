//! screeny wire protocol v1.
//!
//! The single shared definition of the wire format, used unchanged by the
//! firmware (`no_std`, no alloc, no float, Xtensa) and by the host tools.
//! `docs/design/protocol-v1.md` is normative; this crate is its executable
//! form, and where the two disagree the spec wins and this crate is a bug.
//!
//! Four independent pieces:
//!
//! * [`packet`] - the 8-byte header, FRAME and CONTROL parse/build, sequence
//!   number arithmetic.
//! * [`control`] - control opcodes, request/reply bodies, the telemetry
//!   struct, error codes.
//! * [`txt`] - DNS-SD TXT wire format, shared by the mDNS responder and the
//!   `GET_INFO` reply body.
//! * [`dec`] - the five v1 frame decoders, behind one
//!   [`decode`](dec::decode)`(codec, payload, dst)` entry point.
//!
//! # Totality
//!
//! Everything in this crate parses data that arrived from the network onto a
//! device with no MMU. No public function panics, indexes out of bounds,
//! allocates, or fails to terminate on *any* input. `tests/mutation.rs` is the
//! evidence: it throws several hundred thousand corrupt, truncated and random
//! byte strings at every parser and decoder.
//!
//! All multi-byte integers on the wire are little-endian.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::needless_range_loop)]

pub mod control;
pub mod dec;
pub mod packet;
pub mod txt;

pub use dec::{decode, DecodeError};
pub use packet::{
    gap, newer, BuildError, ControlPacket, FramePacket, Packet, Reject, TYPE_CONTROL, TYPE_FRAME,
};

// ---------------------------------------------------------------------------
// Geometry (architecture.md, "Frame types")
// ---------------------------------------------------------------------------

/// Panel width in pixels.
pub const W: usize = 64;
/// Panel height in pixels.
pub const H: usize = 32;
/// Pixels per frame.
pub const NPIX: usize = W * H;
/// Bytes per decoded frame.
pub const NBYTES: usize = NPIX * 3;

/// A decoded frame: 64x32 pixels, RGB888 **sRGB**, row-major, top-left origin.
///
/// Gamma to panel duty is the display driver's job and is shared by every
/// codec; nothing in this crate does it.
pub type Rgb888Frame = [u8; NBYTES];

/// A frame that is still in palette form, as a sender's source material.
///
/// Borrowed rather than owned so the firmware never has to size a palette
/// buffer it does not use. `palette` holds at most 256 entries; an index
/// outside it is a bug in the producer, and [`IndexedFrame::expand`] reports
/// it rather than panicking.
#[derive(Debug, Clone, Copy)]
pub struct IndexedFrame<'a> {
    /// Up to 256 RGB888 colours.
    pub palette: &'a [[u8; 3]],
    /// One palette index per pixel, raster order.
    pub indices: &'a [u8; NPIX],
}

impl IndexedFrame<'_> {
    /// Number of distinct palette slots. Always `<= 256` for a wire-legal frame.
    #[must_use]
    pub fn palette_len(&self) -> usize {
        self.palette.len()
    }

    /// Write this frame out as RGB888.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Corrupt`] if any index is outside `palette`.
    pub fn expand(&self, dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
        let n = self.palette.len();
        for p in 0..NPIX {
            let i = self.indices[p] as usize;
            if i >= n {
                return Err(DecodeError::Corrupt);
            }
            let c = self.palette[i];
            dst[p * 3] = c[0];
            dst[p * 3 + 1] = c[1];
            dst[p * 3 + 2] = c[2];
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Transport constants (protocol-v1.md sections 1, 2, 10)
// ---------------------------------------------------------------------------

/// First byte of every screeny packet, `'s'`.
pub const MAGIC: u8 = 0x53;
/// Protocol version carried in the high nibble of byte 1.
pub const VERSION: u8 = 1;

/// Fixed header length, bytes.
pub const HEADER_LEN: usize = 8;
/// Largest UDP payload a sender may emit: 1500 MTU - 20 IPv4 - 8 UDP.
pub const MAX_UDP_PAYLOAD: usize = 1472;
/// Largest `FRAME` pixel payload, i.e. `len`'s ceiling.
pub const MAX_PIXEL_PAYLOAD: usize = MAX_UDP_PAYLOAD - HEADER_LEN; // 1464

/// Default frame port (0xC0DE).
pub const DEFAULT_FRAME_PORT: u16 = 49374;
/// Default control port (0xC0DF).
pub const DEFAULT_CONTROL_PORT: u16 = 49375;

/// DNS-SD service type, fully qualified.
pub const SERVICE_TYPE: &str = "_screeny._udp.local.";

/// `FRAME` flag: this frame decodes standalone. All v1 codecs set it.
pub const F_KEY: u8 = 0x01;
/// `FRAME` flag: send one `TELEMETRY` reply after processing this frame.
pub const F_STATS_REQ: u8 = 0x02;
/// `FRAME` flag: last frame of this stream; release the source lock.
pub const F_FINAL: u8 = 0x04;
/// `FRAME` flag: a `u32le` sender timestamp (us) precedes the pixel payload.
pub const F_HAS_TS: u8 = 0x08;
/// The `FRAME` flag bits v1 defines. Everything else is reserved and ignored.
pub const F_KNOWN: u8 = F_KEY | F_STATS_REQ | F_FINAL | F_HAS_TS;

/// `CONTROL` flag: this packet is a reply, not a request.
pub const C_REPLY: u8 = 0x01;
/// `CONTROL` flag: this reply carries a 1-byte error code.
pub const C_ERROR: u8 = 0x02;
/// The `CONTROL` flag bits v1 defines.
pub const C_KNOWN: u8 = C_REPLY | C_ERROR;

/// Width of the `HAS_TS` timestamp prefix, bytes.
pub const TS_LEN: usize = 4;

// --- arbitration and idle timing (section 7.2) -----------------------------

/// After the last accepted frame, the active source keeps exclusivity this long.
pub const LOCK_MS: u32 = 500;
/// No frames for this long and the stream is considered stopped.
pub const STREAM_TIMEOUT_MS: u32 = 1_000;
/// How long the last frame stays lit after the stream stops.
pub const HOLD_MS: u32 = 10_000;
/// Cross-fade duration into the idle screen.
pub const FADE_MS: u32 = 500;
/// Minimum gap between `BUSY` packets to one source.
pub const BUSY_MIN_INTERVAL_MS: u32 = 1_000;
/// Minimum gap between `TELEMETRY` packets to one source.
pub const TELEMETRY_MIN_INTERVAL_MS: u32 = 100;

const _: () = assert!(MAX_PIXEL_PAYLOAD == 1464);
const _: () = assert!(NBYTES == 6144);
