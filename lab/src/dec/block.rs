//! Block decoders (BC1 family and colour-cell family).
//!
//! All of these share one shape: read a handful of endpoint/palette colours
//! for a WxH tile, build a tiny local colour table, then look pixels up in it.
//! The local table is at most 8 entries of RGB888, so it lives in registers /
//! a 24-byte stack array. That is the reason this family is attractive on an
//! MCU: no palette of 96 bytes to keep hot, no bit reader, perfectly
//! sequential writes into the frame buffer.
//!
//! Interpolation uses `lerp8` with the /256 weight tables from the parent
//! module, which reproduces BC1's 1/3 and 2/3 points to within one code value
//! while replacing the divide with a multiply and a shift.
//!
//! Endpoints are stored in **sRGB (gamma) space** and interpolated there. On a
//! linear-light LED panel the perceptually even ramp between two colours is
//! the one that is even in sRGB, so this is both the cheap option and the
//! right one; see `docs/research/002-frame-encoding.md`.

use super::{lerp8, rgb444_even, rgb444_odd, rgb565, DecErr, Pixels, H, W, W4, W8};

#[inline(always)]
fn put(dst: &mut Pixels, x: usize, y: usize, c: [u8; 3]) {
    let o = (y * W + x) * 3;
    dst[o] = c[0];
    dst[o + 1] = c[1];
    dst[o + 2] = c[2];
}

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

/// 2-bit index, MSB-first, 4 indices per byte.
#[inline(always)]
fn idx2(bytes: &[u8], j: usize) -> usize {
    ((bytes[j >> 2] >> (6 - 2 * (j & 3))) & 3) as usize
}

/// 3-bit index read from three bitplanes each `stride` bytes long.
#[inline(always)]
fn idx3_planes(bytes: &[u8], stride: usize, j: usize) -> usize {
    let byte = j >> 3;
    let sh = 7 - (j & 7);
    let b0 = (bytes[byte] >> sh) & 1;
    let b1 = (bytes[stride + byte] >> sh) & 1;
    let b2 = (bytes[2 * stride + byte] >> sh) & 1;
    (b0 | (b1 << 1) | (b2 << 2)) as usize
}

// --- 4x4, RGB565 endpoints, 2-bit indices: 8 B/block, 1024 B/frame ---------
pub fn dec_bc1(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 8;
    if src.len() < (W / 4) * (H / 4) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
            let t = ramp4(rgb565(b[0], b[1]), rgb565(b[2], b[3]));
            for j in 0..16 {
                put(dst, bx * 4 + (j & 3), by * 4 + (j >> 2), t[idx2(&b[4..], j)]);
            }
        }
    }
    Ok(())
}

// --- 4x4, RGB565 endpoints, 3-bit indices: 10 B/block, 1280 B/frame --------
pub fn dec_bc1_i3(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 10;
    if src.len() < (W / 4) * (H / 4) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
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
    Ok(())
}

// --- 4x4, RGB888 endpoints, 2-bit indices: 10 B/block, 1280 B/frame --------
pub fn dec_bc1_e888(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 10;
    if src.len() < (W / 4) * (H / 4) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
            let t = ramp4([b[0], b[1], b[2]], [b[3], b[4], b[5]]);
            for j in 0..16 {
                put(dst, bx * 4 + (j & 3), by * 4 + (j >> 2), t[idx2(&b[6..], j)]);
            }
        }
    }
    Ok(())
}

// --- 4x2, RGB444 endpoints, 2-bit indices: 5 B/block, 1280 B/frame ---------
pub fn dec_blk42(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 5;
    if src.len() < (W / 4) * (H / 2) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 2 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
            let t = ramp4(rgb444_even(b), rgb444_odd(b));
            for j in 0..8 {
                put(dst, bx * 4 + (j & 3), by * 2 + (j >> 2), t[idx2(&b[3..], j)]);
            }
        }
    }
    Ok(())
}

// --- 8x4, RGB565 endpoints, 3-bit indices: 16 B/block, 1024 B/frame --------
pub fn dec_blk84_i3(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 16;
    if src.len() < (W / 8) * (H / 4) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 4 {
        for bx in 0..W / 8 {
            let b = &src[(by * (W / 8) + bx) * BS..][..BS];
            let t = ramp8(rgb565(b[0], b[1]), rgb565(b[2], b[3]));
            for j in 0..32 {
                put(
                    dst,
                    bx * 8 + (j & 7),
                    by * 4 + (j >> 3),
                    t[idx3_planes(&b[4..], 4, j)],
                );
            }
        }
    }
    Ok(())
}

// --- 4x4, four free RGB444 colours, 2-bit indices: 10 B/block, 1280 B ------
pub fn dec_cc4(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 10;
    if src.len() < (W / 4) * (H / 4) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
            let t = [
                rgb444_even(&b[0..3]),
                rgb444_odd(&b[0..3]),
                rgb444_even(&b[3..6]),
                rgb444_odd(&b[3..6]),
            ];
            for j in 0..16 {
                put(dst, bx * 4 + (j & 3), by * 4 + (j >> 2), t[idx2(&b[6..], j)]);
            }
        }
    }
    Ok(())
}

// --- 4x2, two free RGB565 colours, 1-bit indices: 5 B/block, 1280 B --------
pub fn dec_cc2_42(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 5;
    if src.len() < (W / 4) * (H / 2) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 2 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
            let t = [rgb565(b[0], b[1]), rgb565(b[2], b[3])];
            for j in 0..8 {
                let bit = ((b[4] >> (7 - j)) & 1) as usize;
                put(dst, bx * 4 + (j & 3), by * 2 + (j >> 2), t[bit]);
            }
        }
    }
    Ok(())
}

// --- 4x4 dual mode: 16 B flag plane + 128 x 10 B, 1297 B/frame ------------
//
// Measurement said the two things a block can be short of are *endpoint
// precision* (dark, smooth blocks: RGB565's 5-bit steps are coarse where the
// panel is finest in relative terms) and *gradation* (bright smooth ramps:
// four levels band). One flag bit per block buys whichever it needs.
pub fn dec_bc1_dual(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const NB: usize = (W / 4) * (H / 4);
    const FLAGS: usize = NB / 8;
    if src.len() < FLAGS + NB * 10 {
        return Err(DecErr::Short);
    }
    let (flags, blocks) = src.split_at(FLAGS);
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let n = by * (W / 4) + bx;
            let b = &blocks[n * 10..][..10];
            let e888 = (flags[n >> 3] >> (7 - (n & 7))) & 1 == 1;
            if e888 {
                let t = ramp4([b[0], b[1], b[2]], [b[3], b[4], b[5]]);
                for j in 0..16 {
                    put(dst, bx * 4 + (j & 3), by * 4 + (j >> 2), t[idx2(&b[6..], j)]);
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

// --- 4x4, two free RGB888 colours, 1-bit indices: 8 B/block, 1024 B --------
pub fn dec_cc2_44(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const BS: usize = 8;
    if src.len() < (W / 4) * (H / 4) * BS {
        return Err(DecErr::Short);
    }
    for by in 0..H / 4 {
        for bx in 0..W / 4 {
            let b = &src[(by * (W / 4) + bx) * BS..][..BS];
            let t = [[b[0], b[1], b[2]], [b[3], b[4], b[5]]];
            for j in 0..16 {
                let bit = ((b[6 + (j >> 3)] >> (7 - (j & 7))) & 1) as usize;
                put(dst, bx * 4 + (j & 3), by * 4 + (j >> 2), t[bit]);
            }
        }
    }
    Ok(())
}
