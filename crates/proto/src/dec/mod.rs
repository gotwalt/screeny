//! The five v1 frame decoders.
//!
//! Spec: `docs/design/protocol-v1.md` section 4. Lifted from `lab/src/dec`
//! (card 002), which stays frozen as the reference the measurements were made
//! against, minus the lab-only codecs and with **one deliberate difference**:
//! on the wire the codec id lives in the header's `codec` byte and the payload
//! starts directly with the codec's own fields, so the entry point is
//! [`decode`]`(codec, payload, dst)` rather than the lab's
//! `decode(payload_with_mode_byte, dst)`. A lab payload is therefore exactly
//! one byte longer than the wire payload for the same frame, and
//! `tests/lab_vectors.rs` checks that dropping that byte gives identical
//! pixels.
//!
//! # Properties every decoder here holds
//!
//! * **Total.** No payload - truncated, corrupt, adversarial - can panic,
//!   index out of bounds, write outside `dst`, or fail to terminate. This code
//!   runs on a device with no MMU and parses whatever arrives on a UDP port.
//! * **Exact length.** A payload must be precisely the size its codec calls
//!   for. Short is [`DecodeError::Short`], long is [`DecodeError::Long`].
//!   Padding belongs beyond the header's `len`, not inside it (section 2.3).
//! * **Allocation-free, integer-only, no scratch buffers** beyond a 768-byte
//!   stack copy of the palette in [`decode_pal8_lz`], which the LZ output
//!   would otherwise overwrite.
//! * **All-or-nothing is the caller's job.** A failed decode may have written
//!   part of `dst`, so decode into the back buffer and swap only on success
//!   (section 4.7).
//!
//! Output is 64x32 RGB888 **sRGB**. Gamma to panel duty belongs to the display
//! driver and is shared by every codec.

use crate::Rgb888Frame;

pub mod block;
pub mod lz;
pub mod pal;

/// Codec ids, section 4.
pub mod codec {
    /// 32-colour adaptive palette, 1376 bytes fixed. The always-fits floor.
    pub const PAL5: u8 = 0x02;
    /// Up to 256 colours plus an LZ index plane. The workhorse.
    pub const PAL8_LZ: u8 = 0x10;
    /// 16 colours plus an LZ nibble plane. Text and UI, bit-exact.
    pub const PAL4_LZ: u8 = 0x11;
    /// 4x4 block codec, 1296 bytes fixed. Photographic content.
    pub const BC1_DUAL: u8 = 0x28;
    /// Whole frame one colour, 3 bytes.
    pub const SOLID: u8 = 0x7F;
}

/// Every codec a v1 device must decode, in the preference order the `codecs=`
/// TXT key advertises (most preferred first).
pub const SUPPORTED_CODECS: [u8; 5] = [
    codec::PAL8_LZ,
    codec::PAL4_LZ,
    codec::BC1_DUAL,
    codec::PAL5,
    codec::SOLID,
];

/// [`SUPPORTED_CODECS`] preformatted for the `codecs=` TXT key.
pub const CODECS_TXT: &str = "16,17,40,2,127";

/// Fixed payload size of [`codec::PAL5`]: 32x3 palette + 1024 nibbles + 256 bits.
pub const PAL5_LEN: usize = 32 * 3 + crate::NPIX / 2 + crate::NPIX / 8; // 1376
/// Fixed payload size of [`codec::BC1_DUAL`]: 16 flag bytes + 128 x 10 B blocks.
pub const BC1_DUAL_LEN: usize = 16 + 128 * 10; // 1296
/// Fixed payload size of [`codec::SOLID`].
pub const SOLID_LEN: usize = 3;

/// Why a payload did not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Payload shorter than this codec requires.
    Short,
    /// Payload longer than this codec requires: a fixed-size codec given
    /// extra bytes, or bytes trailing an LZ stream that already finished.
    Long,
    /// Codec id is reserved, lab-only, or otherwise not one of the five.
    UnsupportedCodec,
    /// Self-inconsistent payload: a palette index outside the palette, an LZ
    /// match reaching before the start of the output, or an LZ stream that
    /// produces the wrong number of bytes.
    Corrupt,
}

/// True if [`decode`] will attempt this codec id.
#[must_use]
pub fn is_supported(codec: u8) -> bool {
    matches!(
        codec,
        codec::PAL5 | codec::PAL8_LZ | codec::PAL4_LZ | codec::BC1_DUAL | codec::SOLID
    )
}

/// Decode one frame payload into `dst`.
///
/// `payload` is the pixel payload from [`crate::FramePacket::payload`]: the
/// `HAS_TS` prefix is already stripped and the codec id is *not* part of it.
///
/// # Errors
///
/// See [`DecodeError`]. On any error `dst` may have been partially written;
/// section 4.7 requires decoding into the back buffer and swapping only on
/// success.
pub fn decode(codec: u8, payload: &[u8], dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
    match codec {
        codec::PAL5 => pal::decode_pal5(payload, dst),
        codec::PAL8_LZ => pal::decode_pal8_lz(payload, dst),
        codec::PAL4_LZ => pal::decode_pal4_lz(payload, dst),
        codec::BC1_DUAL => block::decode_bc1_dual(payload, dst),
        codec::SOLID => decode_solid(payload, dst),
        _ => Err(DecodeError::UnsupportedCodec),
    }
}

/// [`codec::SOLID`]: `[R][G][B]`, the whole frame one colour.
///
/// # Errors
///
/// [`DecodeError::Short`] or [`DecodeError::Long`] if `src` is not 3 bytes.
pub fn decode_solid(src: &[u8], dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
    exact(src.len(), SOLID_LEN)?;
    let mut i = 0;
    while i < crate::NBYTES {
        dst[i] = src[0];
        dst[i + 1] = src[1];
        dst[i + 2] = src[2];
        i += 3;
    }
    Ok(())
}

/// The length check every fixed-size codec starts with.
#[inline]
pub(crate) fn exact(got: usize, want: usize) -> Result<(), DecodeError> {
    match got.cmp(&want) {
        core::cmp::Ordering::Less => Err(DecodeError::Short),
        core::cmp::Ordering::Greater => Err(DecodeError::Long),
        core::cmp::Ordering::Equal => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Shared integer primitives. The only "maths" the firmware needs.
// ---------------------------------------------------------------------------

/// Interpolation weights for 4 levels, scaled to 256 so `W4[k] + W4[3-k] == 256`.
pub const W4: [u32; 4] = [0, 85, 171, 256];
/// Interpolation weights for 8 levels; `W8[k] + W8[7-k] == 256`.
pub const W8: [u32; 8] = [0, 37, 73, 110, 146, 183, 219, 256];

/// Blend `a` and `b` with a /256 weight: `(a*(256-w) + b*w + 128) >> 8`.
#[inline(always)]
#[must_use]
pub fn lerp8(a: u8, b: u8, w: u32) -> u8 {
    (((a as u32) * (256 - w) + (b as u32) * w + 128) >> 8) as u8
}

/// Expand a 5-bit channel to 8 bits by bit replication (exact at 0 and 31).
#[inline(always)]
#[must_use]
pub fn x5(v: u32) -> u8 {
    ((v << 3) | (v >> 2)) as u8
}

/// Expand a 6-bit channel to 8 bits by bit replication.
#[inline(always)]
#[must_use]
pub fn x6(v: u32) -> u8 {
    ((v << 2) | (v >> 4)) as u8
}

/// Unpack an RGB565 little-endian pair into RGB888.
#[inline(always)]
#[must_use]
pub fn rgb565(lo: u8, hi: u8) -> [u8; 3] {
    let v = (lo as u32) | ((hi as u32) << 8);
    [x5((v >> 11) & 31), x6((v >> 5) & 63), x5(v & 31)]
}

const _: () = assert!(PAL5_LEN == 1376);
const _: () = assert!(BC1_DUAL_LEN == 1296);
