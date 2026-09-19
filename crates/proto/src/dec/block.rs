//! The block decoder: [`super::codec::BC1_DUAL`]. Lifted from
//! `lab/src/dec/block.rs`, which also holds the lab-only block codecs.
//!
//! Spec: `docs/design/protocol-v1.md` section 4.5, which this file defines.
//!
//! ```text
//! [flags : 16 B, one bit per block, MSB first, raster order of blocks]
//! [128 blocks x 10 B]
//!
//! flag 0:  [e0 : RGB565 u16le][e1 : RGB565 u16le][idx : 3 bitplanes x 2 B]  8 levels
//! flag 1:  [e0 : RGB888][e1 : RGB888][idx : 16 x 2 bits = 4 B]              4 levels
//! ```
//!
//! One flag bit per block buys whichever of endpoint *precision* or
//! *gradation* that block actually needs: dark smooth blocks want RGB888
//! endpoints because RGB565's 5-bit steps are coarse where the panel is finest
//! in relative terms, bright smooth ramps want eight levels because four band.
//!
//! Endpoints are stored and interpolated in **sRGB (gamma) space**. On a
//! linear-light LED panel the perceptually even ramp between two colours is
//! the one that is even in sRGB, so this is both the cheap option and the
//! right one; see `docs/research/002-frame-encoding.md`.
//!
//! The local colour table is at most 8 RGB888 entries, so it lives in a
//! 24-byte stack array: no 96-byte palette to keep hot, no bit reader, and
//! perfectly sequential writes into the frame buffer.

use super::{exact, lerp8, rgb565, DecodeError, BC1_DUAL_LEN, W4, W8};
use crate::{Rgb888Frame, H, W};

#[inline(always)]
fn put(dst: &mut Rgb888Frame, x: usize, y: usize, c: [u8; 3]) {
    let o = (y * W + x) * 3;
    dst[o] = c[0];
    dst[o + 1] = c[1];
    dst[o + 2] = c[2];
}

/// The four levels of an RGB888-endpoint block: `lerp(e0, e1, W4[k])`.
#[inline(always)]
fn ramp4(a: [u8; 3], b: [u8; 3]) -> [[u8; 3]; 4] {
    let mut t = [[0u8; 3]; 4];
    for k in 0..4 {
        let w = W4[k];
        t[k] = [
            lerp8(a[0], b[0], w),
            lerp8(a[1], b[1], w),
            lerp8(a[2], b[2], w),
        ];
    }
    t
}

/// The eight levels of an RGB565-endpoint block: `lerp(e0, e1, W8[k])`.
#[inline(always)]
fn ramp8(a: [u8; 3], b: [u8; 3]) -> [[u8; 3]; 8] {
    let mut t = [[0u8; 3]; 8];
    for k in 0..8 {
        let w = W8[k];
        t[k] = [
            lerp8(a[0], b[0], w),
            lerp8(a[1], b[1], w),
            lerp8(a[2], b[2], w),
        ];
    }
    t
}

/// 2-bit index for pixel `j` of a block: 4 per byte, most significant pair
/// first, so pixel 0 is bits 7..6 of byte 0.
#[inline(always)]
fn idx2(bytes: &[u8], j: usize) -> usize {
    ((bytes[j >> 2] >> (6 - 2 * (j & 3))) & 3) as usize
}

/// 3-bit index for pixel `j`, read from three bitplanes of `stride` bytes
/// each: plane 0 is the low bit, plane 2 the high bit, MSB first within a byte
/// so pixel 0 is bit 7.
#[inline(always)]
fn idx3_planes(bytes: &[u8], stride: usize, j: usize) -> usize {
    let byte = j >> 3;
    let sh = 7 - (j & 7);
    let b0 = (bytes[byte] >> sh) & 1;
    let b1 = (bytes[stride + byte] >> sh) & 1;
    let b2 = (bytes[2 * stride + byte] >> sh) & 1;
    (b0 | (b1 << 1) | (b2 << 2)) as usize
}

/// [`super::codec::BC1_DUAL`]: 4x4 blocks, 16 across and 8 down, 10 bytes each
/// behind a 16-byte flag plane.
///
/// # Errors
///
/// [`DecodeError::Short`] or [`DecodeError::Long`] if the payload is not
/// exactly [`BC1_DUAL_LEN`] bytes. It cannot be corrupt: every bit pattern in
/// a block is a legal endpoint pair and index set.
pub fn decode_bc1_dual(src: &[u8], dst: &mut Rgb888Frame) -> Result<(), DecodeError> {
    const NB: usize = (W / 4) * (H / 4); // 128 blocks
    const FLAGS: usize = NB / 8; // 16 bytes
    exact(src.len(), BC1_DUAL_LEN)?;
    let (flags, blocks) = src.split_at(FLAGS);
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let n = by * (W / 4) + bx;
            let b = &blocks[n * 10..][..10];
            let e888 = (flags[n >> 3] >> (7 - (n & 7))) & 1 == 1;
            if e888 {
                let t = ramp4([b[0], b[1], b[2]], [b[3], b[4], b[5]]);
                for j in 0..16 {
                    put(
                        dst,
                        bx * 4 + (j & 3),
                        by * 4 + (j >> 2),
                        t[idx2(&b[6..], j)],
                    );
                }
            } else {
                let t = ramp8(rgb565(b[0], b[1]), rgb565(b[2], b[3]));
                for j in 0..16 {
                    put(
                        dst,
                        bx * 4 + (j & 3),
                        by * 4 + (j >> 2),
                        t[idx3_planes(&b[4..], 2, j)],
                    );
                }
            }
        }
    }
    Ok(())
}
