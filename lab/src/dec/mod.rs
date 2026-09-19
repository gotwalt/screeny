//! Frame decoders. **This module is `no_std`, allocation-free and
//! floating-point-free** so it can be lifted verbatim into the firmware's
//! shared `proto` crate. `lab/nostd-check` compiles exactly this file tree as
//! `#![no_std]` with no allocator to keep that honest.
//!
//! Every decoder has the shape
//! `fn(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr>` and writes the
//! full 64x32 RGB888 frame. `decode` dispatches on `src[0]`, the mode byte,
//! which is the first byte of the codec payload (the transport header from
//! card 003 sits in front of it).
//!
//! All arithmetic is 32-bit integer: adds, shifts, small constant multiplies
//! and table lookups. No divides in the hot paths, no LUTs larger than 8
//! entries, no per-frame state. Gamma correction is *not* done here -- it
//! belongs in the panel driver's sRGB8 -> BCM duty table, which is shared by
//! every mode.

#![allow(clippy::needless_range_loop)]

pub const W: usize = 64;
pub const H: usize = 32;
pub const NPIX: usize = W * H;
pub const NBYTES: usize = NPIX * 3;

/// A decoded frame: 64*32 pixels, RGB888, row-major, top-left origin.
pub type Pixels = [u8; NBYTES];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecErr {
    /// Payload shorter than the mode requires.
    Short,
    /// Unknown mode byte.
    BadMode,
    /// Self-inconsistent payload (bad length field, LZ match out of range).
    Corrupt,
}

/// Mode bytes. Kept sparse and grouped so the firmware can range-test.
pub mod mode {
    // --- palette family -------------------------------------------------
    /// 16-colour palette, 4 bits/pixel.
    pub const PAL4: u8 = 0x01;
    /// 32-colour palette, 4-bit plane + 1 extra bitplane.
    pub const PAL5: u8 = 0x02;
    /// up-to-256-colour palette, 8 bits/pixel, uncompressed (never fits; diagnostic only).
    pub const PAL8: u8 = 0x03;
    /// up-to-256-colour palette, 8 bits/pixel, LZ-compressed index plane.
    pub const PAL8_LZ: u8 = 0x10;
    /// 16-colour palette, LZ-compressed nibble plane.
    pub const PAL4_LZ: u8 = 0x11;

    // --- block family ---------------------------------------------------
    /// 4x4 blocks, 2x RGB565 endpoints, 2-bit indices (classic BC1 rate).
    pub const BC1: u8 = 0x20;
    /// 4x4 blocks, 2x RGB565 endpoints, 3-bit indices.
    pub const BC1_I3: u8 = 0x21;
    /// 4x4 blocks, 2x RGB888 endpoints, 2-bit indices.
    pub const BC1_E888: u8 = 0x22;
    /// 4x2 blocks, 2x RGB444 endpoints, 2-bit indices.
    pub const BLK42: u8 = 0x23;
    /// 8x4 blocks, 2x RGB565 endpoints, 3-bit indices.
    pub const BLK84_I3: u8 = 0x24;
    /// 4x4 blocks, 4 free RGB444 colours, 2-bit indices (colour-cell).
    pub const CC4: u8 = 0x25;
    /// 4x2 blocks, 2 free RGB565 colours, 1-bit indices (colour-cell).
    pub const CC2_42: u8 = 0x26;
    /// 4x4 blocks, 2 free RGB888 colours, 1-bit indices. Exact for any block
    /// containing at most two distinct colours -- i.e. lossless on most text.
    pub const CC2_44: u8 = 0x27;
    /// 4x4 blocks, 10 B each, with a per-block flag choosing between
    /// (RGB565 endpoints + 3-bit indices) and (RGB888 endpoints + 2-bit
    /// indices). Spends its bits on whichever of precision or gradation the
    /// block actually needs.
    pub const BC1_DUAL: u8 = 0x28;

    // --- YCoCg family ---------------------------------------------------
    /// Y 4bpp full-res, Co/Cg 3bpp at 2x2 subsampling.
    pub const YCOCG_420: u8 = 0x30;
    /// Y 5bpp full-res, Co/Cg 5bpp at 4x4 subsampling.
    pub const YCOCG_410: u8 = 0x31;

    // --- degenerate ------------------------------------------------------
    /// Whole frame is one RGB888 colour (3 bytes). The graceful-degradation floor.
    pub const SOLID: u8 = 0x7f;
}

pub mod block;
pub mod lz;
pub mod pal;
pub mod ycocg;

/// Decode one codec payload into `dst`.
pub fn decode(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    let &m = src.first().ok_or(DecErr::Short)?;
    let body = &src[1..];
    match m {
        mode::PAL4 => pal::dec_pal4(body, dst),
        mode::PAL5 => pal::dec_pal5(body, dst),
        mode::PAL8 => pal::dec_pal8(body, dst),
        mode::PAL8_LZ => pal::dec_pal8_lz(body, dst),
        mode::PAL4_LZ => pal::dec_pal4_lz(body, dst),
        mode::BC1 => block::dec_bc1(body, dst),
        mode::BC1_I3 => block::dec_bc1_i3(body, dst),
        mode::BC1_E888 => block::dec_bc1_e888(body, dst),
        mode::BLK42 => block::dec_blk42(body, dst),
        mode::BLK84_I3 => block::dec_blk84_i3(body, dst),
        mode::CC4 => block::dec_cc4(body, dst),
        mode::CC2_42 => block::dec_cc2_42(body, dst),
        mode::CC2_44 => block::dec_cc2_44(body, dst),
        mode::BC1_DUAL => block::dec_bc1_dual(body, dst),
        mode::YCOCG_420 => ycocg::dec_420(body, dst),
        mode::YCOCG_410 => ycocg::dec_410(body, dst),
        mode::SOLID => dec_solid(body, dst),
        _ => Err(DecErr::BadMode),
    }
}

fn dec_solid(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    if src.len() < 3 {
        return Err(DecErr::Short);
    }
    let mut i = 0;
    while i < NBYTES {
        dst[i] = src[0];
        dst[i + 1] = src[1];
        dst[i + 2] = src[2];
        i += 3;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared integer primitives. These are the only "maths" the firmware needs.
// ---------------------------------------------------------------------------

/// Interpolation weights for 4 levels, scaled to 256 so that
/// `W4[k] + W4[3-k] == 256`. Replaces BC1's /3 with a shift.
pub const W4: [u32; 4] = [0, 85, 171, 256];
/// Interpolation weights for 8 levels; `W8[k] + W8[7-k] == 256`.
pub const W8: [u32; 8] = [0, 37, 73, 110, 146, 183, 219, 256];

/// Blend `a` and `b` with a /256 weight table entry: `a*(256-w) + b*w`.
#[inline(always)]
pub fn lerp8(a: u8, b: u8, w: u32) -> u8 {
    (((a as u32) * (256 - w) + (b as u32) * w + 128) >> 8) as u8
}

/// Expand a 5-bit channel to 8 bits (replicating high bits, exact at 0 and 31).
#[inline(always)]
pub fn x5(v: u32) -> u8 {
    ((v << 3) | (v >> 2)) as u8
}
/// Expand a 6-bit channel to 8 bits.
#[inline(always)]
pub fn x6(v: u32) -> u8 {
    ((v << 2) | (v >> 4)) as u8
}
/// Expand a 4-bit channel to 8 bits.
#[inline(always)]
pub fn x4(v: u32) -> u8 {
    ((v << 4) | v) as u8
}

/// Unpack an RGB565 little-endian pair into RGB888.
#[inline(always)]
pub fn rgb565(lo: u8, hi: u8) -> [u8; 3] {
    let v = (lo as u32) | ((hi as u32) << 8);
    [x5((v >> 11) & 31), x6((v >> 5) & 63), x5(v & 31)]
}

/// Unpack 12 bits of RGB444 starting at `b[0]`'s low nibble ordering
/// `[rrrrgggg][bbbb....]` -- i.e. 1.5 bytes per colour, two colours per 3 bytes.
#[inline(always)]
pub fn rgb444_even(b: &[u8]) -> [u8; 3] {
    [
        x4((b[0] >> 4) as u32),
        x4((b[0] & 0xf) as u32),
        x4((b[1] >> 4) as u32),
    ]
}
/// The odd (second) RGB444 colour of a 3-byte pair.
#[inline(always)]
pub fn rgb444_odd(b: &[u8]) -> [u8; 3] {
    [
        x4((b[1] & 0xf) as u32),
        x4((b[2] >> 4) as u32),
        x4((b[2] & 0xf) as u32),
    ]
}

#[inline(always)]
pub fn clamp255(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}
