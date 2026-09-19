//! YCoCg-R 4:2:0 / 4:1:0 decoders.
//!
//! YCoCg-R is used rather than YCbCr because its forward and inverse are
//! pure adds and shifts -- no matrix, no fixed-point multiply:
//!
//! ```text
//! forward: Co = R-B;  t = B + (Co>>1);  Cg = G-t;  Y = t + (Cg>>1)
//! inverse: t  = Y - (Cg>>1);  G = Cg+t;  B = t - (Co>>1);  R = B + Co
//! ```
//!
//! Wire layouts (after the mode byte):
//!
//! ```text
//! YCOCG_420  [Y 4bpp nibbles 1024][Co 3 planes 3*64][Cg 3 planes 3*64] = 1409 B
//! YCOCG_410  [Y 4bpp nibbles 1024][Y bit4 plane 256]
//!            [Co 5 planes 5*16][Cg 5 planes 5*16]                      = 1441 B
//! ```
//!
//! Chroma is upsampled by replication (nearest), the cheapest option; a
//! bilinear upsample would cost a multiply-add per pixel per channel and is
//! not obviously worth it at this size.
//!
//! Chroma quantisation is `c = (cq - 2^(m-1)) << (9-m)`, which puts an exact
//! zero in the level set (neutral greys stay neutral -- essential, since grey
//! text on black is a core content class) at the cost of only reaching +256-step
//! on the positive side. At m=5 that costs at most 8/511 of chroma range; at
//! m=3 it visibly desaturates the most extreme colours.

use super::{clamp255, DecErr, Pixels, H, NPIX, W};

#[inline(always)]
fn bit(planes: &[u8], stride: usize, k: usize, j: usize) -> i32 {
    ((planes[k * stride + (j >> 3)] >> (7 - (j & 7))) & 1) as i32
}

#[inline(always)]
fn inverse(y: i32, co: i32, cg: i32, dst: &mut Pixels, o: usize) {
    let t = y - (cg >> 1);
    let g = cg + t;
    let b = t - (co >> 1);
    let r = b + co;
    dst[o] = clamp255(r);
    dst[o + 1] = clamp255(g);
    dst[o + 2] = clamp255(b);
}

/// Y 4bpp, Co/Cg 3bpp at 2x2 subsampling.
pub fn dec_420(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const YLEN: usize = NPIX / 2;
    const CS: usize = (W / 2) * (H / 2); // 512 chroma samples
    const CPLANE: usize = CS / 8; // 64 bytes per bitplane
    if src.len() < YLEN + 6 * CPLANE {
        return Err(DecErr::Short);
    }
    let yp = &src[..YLEN];
    let co_p = &src[YLEN..YLEN + 3 * CPLANE];
    let cg_p = &src[YLEN + 3 * CPLANE..];

    for y in 0..H {
        for x in 0..W {
            let p = y * W + x;
            let b = yp[p >> 1];
            let yq = if p & 1 == 0 { b >> 4 } else { b & 0xf } as i32;
            let yy = yq * 17; // exact 4->8 bit expand

            let c = (y >> 1) * (W / 2) + (x >> 1);
            let coq = bit(co_p, CPLANE, 0, c)
                | (bit(co_p, CPLANE, 1, c) << 1)
                | (bit(co_p, CPLANE, 2, c) << 2);
            let cgq = bit(cg_p, CPLANE, 0, c)
                | (bit(cg_p, CPLANE, 1, c) << 1)
                | (bit(cg_p, CPLANE, 2, c) << 2);
            inverse(yy, (coq - 4) << 6, (cgq - 4) << 6, dst, p * 3);
        }
    }
    Ok(())
}

/// Y 5bpp, Co/Cg 5bpp at 4x4 subsampling.
pub fn dec_410(src: &[u8], dst: &mut Pixels) -> Result<(), DecErr> {
    const YLEN: usize = NPIX / 2;
    const YHI: usize = NPIX / 8;
    const CS: usize = (W / 4) * (H / 4); // 128 chroma samples
    const CPLANE: usize = CS / 8; // 16 bytes per bitplane
    if src.len() < YLEN + YHI + 10 * CPLANE {
        return Err(DecErr::Short);
    }
    let yp = &src[..YLEN];
    let yh = &src[YLEN..YLEN + YHI];
    let co_p = &src[YLEN + YHI..YLEN + YHI + 5 * CPLANE];
    let cg_p = &src[YLEN + YHI + 5 * CPLANE..];

    for y in 0..H {
        for x in 0..W {
            let p = y * W + x;
            let b = yp[p >> 1];
            let low = if p & 1 == 0 { b >> 4 } else { b & 0xf } as u32;
            let hib = ((yh[p >> 3] >> (7 - (p & 7))) & 1) as u32;
            let yq = low | (hib << 4);
            let yy = ((yq << 3) | (yq >> 2)) as i32; // 5->8 bit expand

            let c = (y >> 2) * (W / 4) + (x >> 2);
            let mut coq = 0i32;
            let mut cgq = 0i32;
            for k in 0..5 {
                coq |= bit(co_p, CPLANE, k, c) << k;
                cgq |= bit(cg_p, CPLANE, k, c) << k;
            }
            inverse(yy, (coq - 16) << 4, (cgq - 16) << 4, dst, p * 3);
        }
    }
    Ok(())
}
