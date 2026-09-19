//! The byte-oriented LZ77 used by [`super::codec::PAL8_LZ`] and
//! [`super::codec::PAL4_LZ`]. Decode half only; the encoder is host-side and
//! lives in `crates/screeny`.
//!
//! Spec: `docs/design/protocol-v1.md` section 4.4, which this file defines.
//! Lifted from `lab/src/dec/lz.rs` (card 002) with the output-length contract
//! tightened: [`inflate`] now produces *exactly* the requested number of bytes
//! or fails, and reports how much of the stream it consumed, so its callers
//! can reject trailing bytes.
//!
//! ```text
//! stream := group*
//! group  := flags:u8 item{0,8}        -- item k is selected by bit (7-k)
//! item   := literal  when the flag bit is 1
//!         | match    when the flag bit is 0
//! literal:= byte                                 -- copied to the output
//! match  := b0:u8 b1:u8
//!           offset = ((b0 << 4) | (b1 >> 4)) + 1   -- 1..=4096, backwards
//!           length = (b1 & 0x0F) + 3               -- 3..=18
//! ```
//!
//! A group ends early when the output is complete: a decoder stops at the
//! first item boundary at which it has produced the required number of bytes,
//! so the encoder need not pad the final group and a trailing flag byte's
//! unused bits mean nothing.
//!
//! Matches copy byte by byte from the output already produced, so a match may
//! overlap itself - `offset = 1, length = 18` is a run of 18 copies of the
//! previous byte, and that is the format's only run-length encoding. There is
//! no separate window: the 4096-byte offset range covers the whole 2048-byte
//! index plane, so every earlier byte is reachable. Decode costs a compare, a
//! shift and a byte copy per item - no tables, no allocation, no bit reader.
//! That is why this format was chosen over heatshrink, whose bit-level coding
//! needs a stateful reader and a ring buffer of its own.

use super::DecodeError;

/// Expand `src` into `dst[..want]`.
///
/// Returns the number of bytes consumed from `src`, which the caller should
/// compare against `src.len()` if trailing bytes are an error.
///
/// # Errors
///
/// * [`DecodeError::Short`] - the stream ended before `want` bytes came out.
/// * [`DecodeError::Corrupt`] - a match reached before the start of the
///   output, or would have overshot `want`, or `dst` is too small for `want`.
///
/// # Totality
///
/// Every iteration of the outer loop writes at least one byte (a literal
/// writes 1, a match at least 3), and the loop runs only while `di < want`,
/// so it terminates in at most `want` iterations regardless of input. Every
/// read of `src` and every write to `dst` is bounds-checked first.
pub fn inflate(src: &[u8], dst: &mut [u8], want: usize) -> Result<usize, DecodeError> {
    if dst.len() < want {
        return Err(DecodeError::Corrupt);
    }
    let mut si = 0usize;
    let mut di = 0usize;
    while di < want {
        if si >= src.len() {
            return Err(DecodeError::Short);
        }
        let flags = src[si];
        si += 1;
        let mut k = 0;
        while k < 8 {
            if di >= want {
                return Ok(si);
            }
            if (flags >> (7 - k)) & 1 == 1 {
                if si >= src.len() {
                    return Err(DecodeError::Short);
                }
                dst[di] = src[si];
                si += 1;
                di += 1;
            } else {
                if si + 1 >= src.len() {
                    return Err(DecodeError::Short);
                }
                let a = src[si] as usize;
                let b = src[si + 1] as usize;
                si += 2;
                let off = ((a << 4) | (b >> 4)) + 1;
                let len = (b & 0xf) + 3;
                // `off > di` catches a match reaching before the output start;
                // `di + len > want` catches one running past the end. Both are
                // checked before a single byte moves.
                if off > di || di + len > want {
                    return Err(DecodeError::Corrupt);
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
    Ok(si)
}
