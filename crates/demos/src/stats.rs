//! The four numbers the brief says catch most problems before the hardware
//! does: distinct colours, estimated encoded size against the 1464-byte
//! budget, average picture level, and frame-to-frame luminance change.

use crate::color::luma_lin;
use crate::frame::{Frame, NPIX};
use crate::panel::Panel;
use std::collections::HashMap;

pub const BUDGET: usize = 1464;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// Goes on the wire bit-exact, in the named codec.
    Exact(&'static str),
    /// Over budget losslessly; the sender will reduce the palette or block-code.
    Lossy,
}

#[derive(Clone, Debug)]
pub struct FrameStats {
    pub colors: usize,
    pub est_bytes: usize,
    pub wire: Wire,
    /// Average emitted luminance, 0..1.
    pub apl: f32,
}

impl FrameStats {
    pub fn label(&self) -> String {
        let w = match self.wire {
            Wire::Exact(c) => c,
            Wire::Lossy => "LOSSY",
        };
        format!(
            "{} colors  ~{} B {}  APL {:.0}%",
            self.colors,
            self.est_bytes,
            w,
            self.apl * 100.0
        )
    }
}

pub fn distinct_colors(f: &Frame) -> usize {
    let mut set = std::collections::HashSet::with_capacity(512);
    for c in f.pixels() {
        set.insert(c);
    }
    set.len()
}

pub fn apl(f: &Frame, p: &Panel) -> f32 {
    let mut acc = 0f32;
    for c in f.pixels() {
        acc += luma_lin(p.emit(c));
    }
    acc / NPIX as f32
}

/// Mean absolute change in emitted luminance between two frames, 0..1. The
/// brief's flash limiter watches this (no full-field flashing above 3 Hz).
pub fn luma_delta(a: &Frame, b: &Frame, p: &Panel) -> f32 {
    let mut acc = 0f32;
    for i in 0..NPIX {
        let ca = [a.px[i * 3], a.px[i * 3 + 1], a.px[i * 3 + 2]];
        let cb = [b.px[i * 3], b.px[i * 3 + 1], b.px[i * 3 + 2]];
        acc += (luma_lin(p.emit(ca)) - luma_lin(p.emit(cb))).abs();
    }
    acc / NPIX as f32
}

/// Index a frame's colours in first-seen order.
fn index_frame(f: &Frame) -> (Vec<[u8; 3]>, Vec<u8>) {
    let mut map: HashMap<[u8; 3], u8> = HashMap::new();
    let mut pal: Vec<[u8; 3]> = Vec::new();
    let mut idx = Vec::with_capacity(NPIX);
    for c in f.pixels() {
        let n = pal.len();
        if n > 255 {
            // Caller only uses the index stream when the palette fits.
            idx.push(0);
            continue;
        }
        let e = *map.entry(c).or_insert_with(|| {
            pal.push(c);
            n as u8
        });
        idx.push(e);
    }
    (pal, idx)
}

/// A generic byte-oriented LZSS size estimate: greedy matches in a 4 KB
/// window, 1 flag bit per token, 1 byte per literal, 2 bytes per match. The
/// wire format's real LZ (`lab/src/dec/lz.rs`) differs in detail, so treat
/// this as an estimate - it is here to catch "this frame is nowhere near
/// fitting", not to predict the last twenty bytes.
pub fn lz_estimate(data: &[u8]) -> usize {
    const WINDOW: usize = 4096;
    const MIN_MATCH: usize = 3;
    const MAX_MATCH: usize = 255;
    let mut heads: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    let mut tokens = 0usize;
    let mut bytes = 0usize;
    let mut i = 0usize;
    while i < data.len() {
        let mut best = 0usize;
        let mut key_ok = false;
        if i + MIN_MATCH <= data.len() {
            let key = [data[i], data[i + 1], data[i + 2]];
            key_ok = true;
            if let Some(list) = heads.get(&key) {
                for &p in list.iter().rev().take(24) {
                    if i - p > WINDOW {
                        break;
                    }
                    let mut l = 0usize;
                    while l < MAX_MATCH && i + l < data.len() && data[p + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best {
                        best = l;
                    }
                }
            }
            heads.entry(key).or_default().push(i);
        }
        let _ = key_ok;
        if best >= MIN_MATCH {
            tokens += 1;
            bytes += 2;
            for k in 1..best {
                if i + k + MIN_MATCH <= data.len() {
                    let key = [data[i + k], data[i + k + 1], data[i + k + 2]];
                    heads.entry(key).or_default().push(i + k);
                }
            }
            i += best;
        } else {
            tokens += 1;
            bytes += 1;
            i += 1;
        }
    }
    bytes + tokens.div_ceil(8)
}

fn pack_nibbles(idx: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(idx.len() / 2);
    for pair in idx.chunks(2) {
        out.push((pair[0] << 4) | (pair[1] & 0x0f));
    }
    out
}

/// Estimate how this frame goes on the wire (protocol-v1 section 4).
pub fn frame_stats(f: &Frame, p: &Panel) -> FrameStats {
    let (pal, idx) = index_frame(f);
    let n = distinct_colors(f);
    let apl = apl(f, p);
    let (est, wire) = if n == 1 {
        (3, Wire::Exact("SOLID"))
    } else if n <= 16 {
        let sz = 48 + lz_estimate(&pack_nibbles(&idx));
        if sz <= BUDGET {
            (sz, Wire::Exact("PAL4_LZ"))
        } else {
            (1376, Wire::Exact("PAL5"))
        }
    } else if n <= 32 {
        let sz = 1 + 3 * pal.len() + lz_estimate(&idx);
        if sz <= BUDGET {
            (sz, Wire::Exact("PAL8_LZ"))
        } else {
            (1376, Wire::Exact("PAL5"))
        }
    } else if n <= 256 {
        let sz = 1 + 3 * pal.len() + lz_estimate(&idx);
        if sz <= BUDGET {
            (sz, Wire::Exact("PAL8_LZ"))
        } else {
            (sz, Wire::Lossy)
        }
    } else {
        (0, Wire::Lossy)
    };
    FrameStats {
        colors: n,
        est_bytes: est,
        wire,
        apl,
    }
}
