//! Palette decoders: [`super::codec::PAL5`], [`super::codec::PAL8_LZ`] and
//! [`super::codec::PAL4_LZ`]. Lifted from `lab/src/dec/pal.rs`.
//!
//! ```text
//! PAL5     [pal 32*3][nibbles 1024][bit-4 plane 256]      = 1376 B fixed
//! PAL8_LZ  [n-1][pal n*3][lz stream -> 2048 index bytes]  = variable
//! PAL4_LZ  [pal 16*3][lz stream -> 1024 nibble bytes]     = variable
//! ```
//!
//! Index planes are raster order. Nibbles are two per byte, **high nibble
//! first**. The PAL5 bit plane holds bit 4 of each index, MSB first within a
//! byte, so `idx = nib | (bit << 4)`: no bit reader, no unaligned reads, one
//! branchless expression per pixel.
//!
//! The LZ variants decompress *into the destination frame buffer* and then
//! expand the index plane in place, walking backwards so source and
//! destination never collide. That costs zero scratch RAM, which matters more
//! on an ESP32 than the few cycles it saves.

use super::{exact, lz, DecodeError, PAL5_LEN};
use crate::{Rgb888Frame, NPIX};

const PAL16: usize = 16 * 3;
const PAL32: usize = 32 * 3;

#[inline(always)]
fn put(dst: &mut Rgb888Frame, p: usize, c: &[u8]) {
    dst[p * 3] = c[0];
    dst[p * 3 + 1] = c[1];
    dst[p * 3 + 2] = c[2];
}

/// [`super::codec::PAL5`]: 32-colour palette, 5 bits per pixel.
///
/// # Errors
///
/// [`DecodeError::Short`] or [`DecodeError::Long`] if the payload is not
/// exactly [`PAL5_LEN`] bytes. It cannot be corrupt: every 5-bit index is a
/// valid slot in a 32-entry palette.
pub fn decode_pal5(src: &[u8], dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
    exact(src.len(), PAL5_LEN)?;
    let pal = &src[..PAL32];
    let nib = &src[PAL32..PAL32 + NPIX / 2];
    let hi = &src[PAL32 + NPIX / 2..];
    for p in 0..NPIX {
        let b = nib[p >> 1];
        let low = if p & 1 == 0 { b >> 4 } else { b & 0xf } as usize;
        let bit = ((hi[p >> 3] >> (7 - (p & 7))) & 1) as usize;
        let idx = low | (bit << 4); // 0..=31, always in range
        put(dst, p, &pal[idx * 3..idx * 3 + 3]);
    }
    Ok(())
}

/// [`super::codec::PAL8_LZ`]: up to 256 colours, LZ-compressed index plane.
///
/// # Errors
///
/// [`DecodeError::Short`] if the palette or the stream is truncated,
/// [`DecodeError::Long`] if bytes trail the stream, [`DecodeError::Corrupt`]
/// if the stream is malformed or any decoded index is `>= n`.
pub fn decode_pal8_lz(src: &[u8], dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
    let &n0 = src.first().ok_or(DecodeError::Short)?;
    let plen = (n0 as usize + 1) * 3; // n is 1..=256, so plen is 3..=768
    if src.len() < 1 + plen {
        return Err(DecodeError::Short);
    }
    // Copy the palette onto the stack; the LZ output is about to occupy dst.
    let mut pal = [0u8; 256 * 3];
    pal[..plen].copy_from_slice(&src[1..1 + plen]);

    let stream = &src[1 + plen..];
    let used = lz::inflate(stream, dst, NPIX)?;
    if used != stream.len() {
        return Err(DecodeError::Long);
    }
    // Expand indices to pixels in place, back to front: pixel p writes
    // dst[3p..3p+3] and reads dst[p], and 3p >= p for every p.
    for p in (0..NPIX).rev() {
        let c = dst[p] as usize * 3;
        if c + 3 > plen {
            return Err(DecodeError::Corrupt);
        }
        let (r, g, b) = (pal[c], pal[c + 1], pal[c + 2]);
        dst[p * 3] = r;
        dst[p * 3 + 1] = g;
        dst[p * 3 + 2] = b;
    }
    Ok(())
}

/// [`super::codec::PAL4_LZ`]: 16 colours, LZ-compressed nibble plane.
///
/// # Errors
///
/// [`DecodeError::Short`] if the palette or the stream is truncated,
/// [`DecodeError::Long`] if bytes trail the stream, [`DecodeError::Corrupt`]
/// if the stream is malformed. Every 4-bit index is a valid slot, so a
/// well-formed stream always yields a frame.
pub fn decode_pal4_lz(src: &[u8], dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
    if src.len() < PAL16 {
        return Err(DecodeError::Short);
    }
    let mut pal = [0u8; PAL16];
    pal.copy_from_slice(&src[..PAL16]);

    let stream = &src[PAL16..];
    let used = lz::inflate(stream, dst, NPIX / 2)?;
    if used != stream.len() {
        return Err(DecodeError::Long);
    }
    expand_nib_inplace(dst, &pal);
    Ok(())
}

/// Expand a 1024-byte nibble plane living at `dst[0..1024]` through a 16-entry
/// palette, writing pixels backwards so source and destination never collide.
fn expand_nib_inplace(dst: &mut Rgb888Frame, pal: &[u8; PAL16]) {
    for p in (0..NPIX).rev() {
        let b = dst[p >> 1];
        let idx = if p & 1 == 0 { b >> 4 } else { b & 0xf } as usize;
        // `idx` is 4 bits, so `c` is 0..=45: always inside a 48-byte palette.
        let c = idx * 3;
        // Read before write: for p == 0 the source byte is dst[0], which is
        // also the first byte we overwrite, so pull the colour out first.
        let (r, g, bl) = (pal[c], pal[c + 1], pal[c + 2]);
        dst[p * 3] = r;
        dst[p * 3 + 1] = g;
        dst[p * 3 + 2] = bl;
    }
}
