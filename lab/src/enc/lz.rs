//! LZ compressor for index planes, plus the variable-rate palette codec that
//! uses it.
//!
//! ## Graceful degradation
//!
//! A variable-rate codec is only usable here if it can *always* produce
//! something that fits. `PalLzCodec` walks a quality ladder from lossless down
//! and returns the first rung that fits in the budget. The bottom rung is a
//! fixed-rate 1377-byte PAL5 frame, so the ladder can never fail.
//!
//! Note the ladder's bottom rungs drop dithering: ordered dither adds
//! high-frequency noise that costs LZ far more than it gains in quality, so
//! once we are squeezing, undithered compresses much better.

use super::pal::{emit_pal5, map, pack_nibbles};
use super::quant::{self, Palette};
use super::{Codec, Dither, EncCtx};
use crate::color::oklab_srgb8;
use crate::dec::mode;
use crate::frame::{Frame, NPIX};
use std::collections::HashMap;

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 18;
const MAX_OFF: usize = 4096;
const CHAIN_LIMIT: usize = 192;

enum Item {
    Lit(u8),
    Mat { off: usize, len: usize },
}

#[inline]
fn hash3(s: &[u8], i: usize) -> usize {
    (((s[i] as usize) << 8) ^ ((s[i + 1] as usize) << 4) ^ (s[i + 2] as usize)) & 0xffff
}

fn best_match(src: &[u8], i: usize, head: &[i32], prev: &[i32]) -> (usize, usize) {
    if i + MIN_MATCH > src.len() {
        return (0, 0);
    }
    let maxlen = MAX_MATCH.min(src.len() - i);
    let mut cand = head[hash3(src, i)];
    let mut tries = 0;
    let (mut blen, mut boff) = (0usize, 0usize);
    while cand >= 0 && tries < CHAIN_LIMIT {
        let c = cand as usize;
        let off = i - c;
        if off > MAX_OFF {
            break;
        }
        let mut l = 0;
        while l < maxlen && src[c + l] == src[i + l] {
            l += 1;
        }
        if l > blen {
            blen = l;
            boff = off;
            if l == maxlen {
                break;
            }
        }
        cand = prev[c];
        tries += 1;
    }
    if blen >= MIN_MATCH {
        (boff, blen)
    } else {
        (0, 0)
    }
}

/// Compress `src`. Greedy with one step of lazy matching.
pub fn deflate(src: &[u8]) -> Vec<u8> {
    let n = src.len();
    let mut head = vec![-1i32; 1 << 16];
    let mut prev = vec![-1i32; n.max(1)];
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if i + MIN_MATCH <= n {
            let (off, len) = best_match(src, i, &head, &prev);
            if len >= MIN_MATCH {
                // Lazy: if the next position matches strictly longer, emit a
                // literal now and take the better match next round.
                let mut take = true;
                if i + 1 + MIN_MATCH <= n {
                    // Index position i before probing i+1 so the chain is complete.
                    let h = hash3(src, i);
                    prev[i] = head[h];
                    head[h] = i as i32;
                    let (_, len2) = best_match(src, i + 1, &head, &prev);
                    if len2 > len {
                        take = false;
                    }
                    // Undo is unnecessary: indexing i is always correct.
                    if take {
                        for k in 1..len {
                            if i + k + MIN_MATCH <= n {
                                let h = hash3(src, i + k);
                                prev[i + k] = head[h];
                                head[h] = (i + k) as i32;
                            }
                        }
                        items.push(Item::Mat { off, len });
                        i += len;
                        continue;
                    }
                    items.push(Item::Lit(src[i]));
                    i += 1;
                    continue;
                }
                for k in 0..len {
                    if i + k + MIN_MATCH <= n {
                        let h = hash3(src, i + k);
                        prev[i + k] = head[h];
                        head[h] = (i + k) as i32;
                    }
                }
                items.push(Item::Mat { off, len });
                i += len;
                continue;
            }
            let h = hash3(src, i);
            prev[i] = head[h];
            head[h] = i as i32;
        }
        items.push(Item::Lit(src[i]));
        i += 1;
    }

    let mut out = Vec::with_capacity(n);
    for group in items.chunks(8) {
        let mut flags = 0u8;
        for (j, it) in group.iter().enumerate() {
            if matches!(it, Item::Lit(_)) {
                flags |= 0x80 >> j;
            }
        }
        out.push(flags);
        for it in group {
            match *it {
                Item::Lit(b) => out.push(b),
                Item::Mat { off, len } => {
                    let o = off - 1;
                    out.push((o >> 4) as u8);
                    out.push((((o & 0xf) << 4) | (len - 3)) as u8);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------

pub struct PalLzCodec;

impl PalLzCodec {
    pub fn new() -> Self {
        PalLzCodec
    }
}

impl Default for PalLzCodec {
    fn default() -> Self {
        Self::new()
    }
}

/// Map every pixel to its exact palette slot. Only valid when the palette is
/// the frame's own colour set.
fn exact_indices(f: &Frame, pal: &[[u8; 3]]) -> Vec<u8> {
    let lut: HashMap<[u8; 3], u8> = pal
        .iter()
        .enumerate()
        .map(|(i, c)| (*c, i as u8))
        .collect();
    (0..NPIX).map(|p| lut[&f.at(p)]).collect()
}

fn emit_pal8_lz(pal: &[[u8; 3]], idx: &[u8]) -> Vec<u8> {
    let mut out = vec![mode::PAL8_LZ, (pal.len() - 1) as u8];
    for c in pal {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&deflate(idx));
    out
}

fn emit_pal4_lz(pal: &[[u8; 3]], idx: &[u8]) -> Vec<u8> {
    let mut out = vec![mode::PAL4_LZ];
    for i in 0..16 {
        out.extend_from_slice(pal.get(i).unwrap_or(&[0, 0, 0]));
    }
    out.extend_from_slice(&deflate(&pack_nibbles(idx)));
    out
}

/// The full quality ladder, best first. Public so `hybrid` can reuse it.
pub fn ladder(f: &Frame, budget: usize, t: usize) -> Vec<u8> {
    let hist = quant::histogram(f);
    let ncol = hist.bins.len();

    // Rung 1/2: the frame's own colours -- mathematically lossless.
    if ncol <= 256 {
        let pal: Vec<[u8; 3]> = hist.bins.iter().map(|b| b.0).collect();
        let idx = exact_indices(f, &pal);
        if ncol <= 16 {
            let v = emit_pal4_lz(&pal, &idx);
            if v.len() <= budget {
                return v;
            }
        }
        let v = emit_pal8_lz(&pal, &idx);
        if v.len() <= budget {
            return v;
        }
    }

    // Rungs 3-5: adaptive palettes, undithered (dither destroys the LZ ratio).
    for &k in &[256usize, 128, 64, 32] {
        if k >= ncol {
            continue;
        }
        let pal = quant::build(f, k, None);
        let idx: Vec<u8> = (0..NPIX)
            .map(|p| pal.nearest(oklab_srgb8(f.at(p))) as u8)
            .collect();
        let v = emit_pal8_lz(&pal.srgb, &idx);
        if v.len() <= budget {
            return v;
        }
    }

    // Rung 6: 16 colours in a nibble plane.
    let pal16 = quant::build(f, 16, None);
    let idx: Vec<u8> = (0..NPIX)
        .map(|p| pal16.nearest(oklab_srgb8(f.at(p))) as u8)
        .collect();
    let v = emit_pal4_lz(&pal16.srgb, &idx);
    if v.len() <= budget {
        return v;
    }

    // Floor: fixed-rate 1377-byte PAL5, dithered. Always fits a 1450 budget.
    let pal = quant::build(f, 32, None);
    let idx = map(&pal, f, Dither::Ordered, t);
    emit_pal5(&pal, &idx)
}

/// Which rung of the ladder produced this payload, for reporting.
pub fn rung_name(payload: &[u8]) -> &'static str {
    match payload[0] {
        mode::PAL4_LZ => "pal4-lz",
        mode::PAL8_LZ => "pal8-lz",
        mode::PAL5 => "pal5-raw(floor)",
        _ => "?",
    }
}

impl Codec for PalLzCodec {
    fn name(&self) -> &'static str {
        "pal-lz"
    }
    fn note(&self) -> &'static str {
        "variable rate: lossless palette+LZ, degrading to 256/128/64/32/16 colours, floor PAL5"
    }
    fn encode(&self, f: &Frame, budget: usize, ctx: &mut EncCtx) -> Vec<u8> {
        ladder(f, budget, ctx.frame_idx)
    }
}

/// Exposed so the harness can report how lossless the ladder managed to be.
pub fn is_lossless(f: &Frame, payload: &[u8]) -> bool {
    let mut dst = Box::new([0u8; crate::frame::NBYTES]);
    if crate::dec::decode(payload, &mut dst).is_err() {
        return false;
    }
    dst.as_ref() == f.px.as_ref()
}

pub fn unused(_: &Palette) {}
