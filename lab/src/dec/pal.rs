//! Palette decoders. Wire layouts (after the mode byte):
//!
//! ```text
//! PAL4     [pal 16*3][nibbles 1024]                              = 1073 B total
//! PAL5     [pal 32*3][nibbles 1024][hi bitplane 256]             = 1377 B total
//! PAL8     [n-1][pal n*3][indices 2048]                          = 2050+3n B
//! PAL8_LZ  [n-1][pal n*3][lz stream -> 2048 index bytes]         = variable
//! PAL4_LZ  [pal 16*3][lz stream -> 1024 nibble bytes]            = variable
//! ```
//!
//! Index planes are in raster order. The PAL5 bitplane holds bit 4 of each
//! index, MSB-first within each byte, so `idx = nib | (bit << 4)`: no
//! bit-reader, no unaligned reads, one branchless expression per pixel.
//!
//! The LZ variants decompress *into the destination frame buffer* and then
//! expand the index plane in place, walking backwards. That costs zero scratch
//! RAM, which matters more on an ESP32 than the few cycles it saves.

use super::{DecErr, Pixels, NBYTES, NPIX};

const PAL16: usize = 16 * 3;
const PAL32: usize = 32 * 3;

#[inline(always)]
fn put(dst: &mut Pixels, p: usize, c: &[u8]) {
    dst[p * 3] = c[0];
    dst[p * 3 + 1] = c[1];
    dst[p * 3 + 2] = c[2];
}

/// Expand a 1024-byte nibble plane living at `dst[0..1024]` using a 16-entry
/// palette, writing pixels backwards so source and destination never collide.
fn expand_nib_inplace(dst: &mut Pixels, pal: &[u8; PAL16]) {
    for p in (0..NPIX).rev() {
        let b = dst[p >> 1];
        let idx = if p & 1 == 0 { b >> 4 } else { b & 0xf } as usize;
        let c = idx * 3;
        // Read before write: for p == 0 the source byte is dst[0], which is
        // also the first byte we overwrite, so pull the colour out first.
        let (r, g, bl) = (pal[c], pal[c + 1], pal[c + 2]);
        dst[p * 3] = r;
        dst[p * 3 + 1] = g;
        dst[p * 3 + 2] = bl;
    }
}

pub fn dec_pal4(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    if src.len() < PAL16 + NPIX / 2 {
        return Err(DecErr::Short);
    }
    let pal = &src[..PAL16];
    let nib = &src[PAL16..];
    for p in 0..NPIX {
        let b = nib[p >> 1];
        let idx = if p & 1 == 0 { b >> 4 } else { b & 0xf } as usize;
        put(dst, p, &pal[idx * 3..idx * 3 + 3]);
    }
    Ok(())
}

pub fn dec_pal5(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    if src.len() < PAL32 + NPIX / 2 + NPIX / 8 {
        return Err(DecErr::Short);
    }
    let pal = &src[..PAL32];
    let nib = &src[PAL32..PAL32 + NPIX / 2];
    let hi = &src[PAL32 + NPIX / 2..];
    for p in 0..NPIX {
        let b = nib[p >> 1];
        let low = if p & 1 == 0 { b >> 4 } else { b & 0xf } as usize;
        let bit = ((hi[p >> 3] >> (7 - (p & 7))) & 1) as usize;
        let idx = low | (bit << 4);
        put(dst, p, &pal[idx * 3..idx * 3 + 3]);
    }
    Ok(())
}

fn split_pal8(src: &[u8]) -> Result<(&[u8], &[u8]), DecErr> {
    let n = *src.first().ok_or(DecErr::Short)? as usize + 1;
    let plen = n * 3;
    if src.len() < 1 + plen {
        return Err(DecErr::Short);
    }
    Ok((&src[1..1 + plen], &src[1 + plen..]))
}

pub fn dec_pal8(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    let (pal, idx) = split_pal8(src)?;
    if idx.len() < NPIX {
        return Err(DecErr::Short);
    }
    for p in 0..NPIX {
        let c = idx[p] as usize * 3;
        if c + 3 > pal.len() {
            return Err(DecErr::Corrupt);
        }
        put(dst, p, &pal[c..c + 3]);
    }
    Ok(())
}

pub fn dec_pal8_lz(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    let n = *src.first().ok_or(DecErr::Short)? as usize + 1;
    let plen = n * 3;
    if src.len() < 1 + plen {
        return Err(DecErr::Short);
    }
    // Copy the palette onto the stack; the LZ output is about to occupy dst.
    let mut pal = [0u8; 256 * 3];
    pal[..plen].copy_from_slice(&src[1..1 + plen]);

    let produced = super::lz::inflate(&src[1 + plen..], dst, NPIX)?;
    if produced != NPIX {
        return Err(DecErr::Corrupt);
    }
    for p in (0..NPIX).rev() {
        let c = dst[p] as usize * 3;
        if c + 3 > plen {
            return Err(DecErr::Corrupt);
        }
        let (r, g, b) = (pal[c], pal[c + 1], pal[c + 2]);
        dst[p * 3] = r;
        dst[p * 3 + 1] = g;
        dst[p * 3 + 2] = b;
    }
    Ok(())
}

pub fn dec_pal4_lz(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    if src.len() < PAL16 {
        return Err(DecErr::Short);
    }
    let mut pal = [0u8; PAL16];
    pal.copy_from_slice(&src[..PAL16]);
    let produced = super::lz::inflate(&src[PAL16..], dst, NPIX / 2)?;
    if produced != NPIX / 2 {
        return Err(DecErr::Corrupt);
    }
    expand_nib_inplace(dst, &pal);
    Ok(())
}

const _: () = assert!(NBYTES == NPIX * 3);
