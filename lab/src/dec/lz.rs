//! A minimal LZSS/LZ77 byte compressor's decode half.
//!
//! Chosen over heatshrink because heatshrink's bit-level encoding needs a
//! stateful bit reader and a separate ring buffer; this format is byte-aligned
//! and decompresses straight into the destination with no window of its own,
//! which is the property that matters on an MCU.
//!
//! Stream: a flag byte, then 8 items, MSB flag first.
//! * flag 1: one literal byte.
//! * flag 0: two bytes `[oooooooo][oooollll]`, offset = 12 bits + 1 (1..4096),
//!   length = 4 bits + 3 (3..18).
//!
//! 4096 bytes of window covers the whole 2048-byte index plane, so any earlier
//! byte is reachable. Decode is a compare, a shift and a byte copy per item;
//! no tables, no allocation, no bit reader.

use super::DecErr;

pub fn inflate(src: &[u8], dst: &mut [u8], limit: usize) -> Result<usize, DecErr> {
    let mut si = 0usize;
    let mut di = 0usize;
    while di < limit {
        if si >= src.len() {
            return Err(DecErr::Short);
        }
        let flags = src[si];
        si += 1;
        let mut k = 0;
        while k < 8 {
            if di >= limit {
                return Ok(di);
            }
            if (flags >> (7 - k)) & 1 == 1 {
                if si >= src.len() {
                    return Err(DecErr::Short);
                }
                dst[di] = src[si];
                si += 1;
                di += 1;
            } else {
                if si + 1 >= src.len() {
                    return Err(DecErr::Short);
                }
                let a = src[si] as usize;
                let b = src[si + 1] as usize;
                si += 2;
                let off = ((a << 4) | (b >> 4)) + 1;
                let len = (b & 0xf) + 3;
                if off > di || di + len > limit {
                    return Err(DecErr::Corrupt);
                }
                let mut j = 0;
                while j < len {
                    dst[di] = dst[di - off];
                    di += 1;
                    j += 1;
                }
            }
            k += 1;
        }
    }
    Ok(di)
}
