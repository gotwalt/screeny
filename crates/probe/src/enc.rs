//! Encoders for spec section 4, written from its prose.
//!
//! `screeny-proto` ships decoders only; these are the other half, and they
//! exist so that a test which round-trips through them is testing two
//! independent readings of section 4 against each other. That is also why they
//! are **not** `crates/encode`: the product encoder graded against the product
//! decoder would only prove `proto` agrees with itself.
//!
//! Lifted verbatim from `crates/sim/tests/common/mod.rs` by card 080, so that
//! the conformance suite and the simulator's own tests share one copy.

use std::collections::HashMap;

use screeny_proto::{NBYTES, NPIX, W};

/// A frame in the form a palette codec wants it, plus the RGB888 it must
/// decode back to. The expected pixels are computed straight from the palette
/// and the indices - no decoder is involved, so a test using this is not
/// grading `proto` against itself.
pub struct Indexed {
    pub palette: Vec<[u8; 3]>,
    pub indices: Vec<u8>,
}

impl Indexed {
    /// The RGB888 frame this must decode to.
    pub fn expect(&self) -> Vec<u8> {
        let mut out = vec![0u8; NBYTES];
        for p in 0..NPIX {
            let c = self.palette[self.indices[p] as usize];
            out[p * 3..p * 3 + 3].copy_from_slice(&c);
        }
        out
    }
}

/// `n` distinct colours in horizontal runs of four pixels, which is the shape
/// the card-002 lab used to steer the palette ladder onto a chosen rung - and
/// which the LZ encoder below can actually compress.
pub fn indexed_runs(n: usize) -> Indexed {
    assert!((1..=256).contains(&n));
    let palette: Vec<[u8; 3]> = (0..n)
        .map(|i| {
            let v = i as u32;
            [
                ((v * 37) % 256) as u8,
                ((v * 91 + 13) % 256) as u8,
                ((v * 157 + 71) % 256) as u8,
            ]
        })
        .collect();
    let indices: Vec<u8> = (0..NPIX)
        .map(|p| {
            let x = p % W;
            let y = p / W;
            ((x / 4 + y * 3) % n) as u8
        })
        .collect();
    Indexed { palette, indices }
}

/// `0x7F SOLID`: `[R][G][B]` (section 4.6).
pub fn solid(rgb: [u8; 3]) -> (Vec<u8>, Vec<u8>) {
    let mut expect = vec![0u8; NBYTES];
    for p in 0..NPIX {
        expect[p * 3..p * 3 + 3].copy_from_slice(&rgb);
    }
    (rgb.to_vec(), expect)
}

/// `0x02 PAL5`: 32 RGB888 + 1024 low-nibble bytes + a 256-byte bit-4 plane
/// (section 4.1). Nibbles are two per byte, high nibble first; the bit plane
/// is 8 pixels per byte, MSB first.
pub fn pal5(f: &Indexed) -> Vec<u8> {
    assert!(f.palette.len() <= 32, "PAL5 carries exactly 32 colours");
    let mut out = Vec::with_capacity(1376);
    for i in 0..32 {
        let c = f.palette.get(i).copied().unwrap_or([0, 0, 0]);
        out.extend_from_slice(&c);
    }
    for p in (0..NPIX).step_by(2) {
        let hi = f.indices[p] & 0x0F;
        let lo = f.indices[p + 1] & 0x0F;
        out.push(hi << 4 | lo);
    }
    for p in (0..NPIX).step_by(8) {
        let mut b = 0u8;
        for k in 0..8 {
            if f.indices[p + k] & 0x10 != 0 {
                b |= 1 << (7 - k);
            }
        }
        out.push(b);
    }
    assert_eq!(out.len(), 1376);
    out
}

/// `0x11 PAL4_LZ`: 16 RGB888 then an LZ stream over 1024 packed nibble bytes
/// (section 4.3).
pub fn pal4_lz(f: &Indexed) -> Vec<u8> {
    assert!(f.palette.len() <= 16);
    let mut out = Vec::new();
    for i in 0..16 {
        let c = f.palette.get(i).copied().unwrap_or([0, 0, 0]);
        out.extend_from_slice(&c);
    }
    let mut plane = Vec::with_capacity(NPIX / 2);
    for p in (0..NPIX).step_by(2) {
        plane.push(f.indices[p] << 4 | (f.indices[p + 1] & 0x0F));
    }
    out.extend_from_slice(&lz_compress(&plane));
    out
}

/// `0x10 PAL8_LZ`: `[n-1][palette][LZ -> 2048 index bytes]` (section 4.2).
pub fn pal8_lz(f: &Indexed) -> Vec<u8> {
    let n = f.palette.len();
    assert!((1..=256).contains(&n));
    let mut out = Vec::new();
    out.push((n - 1) as u8);
    for c in &f.palette {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&lz_compress(&f.indices));
    out
}

/// One `BC1_DUAL` block, before packing.
pub struct Block {
    /// `true` for flag 1 (RGB888 endpoints, 4 levels), `false` for flag 0
    /// (RGB565 endpoints, 8 levels).
    pub rgb888: bool,
    pub e0: [u8; 3],
    pub e1: [u8; 3],
    /// One index per pixel, `j = 0..15` in raster order within the block.
    pub idx: [u8; 16],
}

/// `0x28 BC1_DUAL`: 16 flag bytes then 128 ten-byte blocks (section 4.5).
///
/// Returns the payload and the RGB888 it must decode to, the latter computed
/// from the spec's own `lerp` and weight tables rather than from `proto`.
pub fn bc1_dual(blocks: &[Block; 128]) -> (Vec<u8>, Vec<u8>) {
    const W4: [u32; 4] = [0, 85, 171, 256];
    const W8: [u32; 8] = [0, 37, 73, 110, 146, 183, 219, 256];
    let lerp = |a: u8, b: u8, w: u32| -> u8 {
        (((a as u32) * (256 - w) + (b as u32) * w + 128) >> 8) as u8
    };
    // 5-bit and 6-bit truncation, then bit replication back out, which is
    // what an RGB565 endpoint survives.
    let to565 = |c: [u8; 3]| -> u16 {
        ((c[0] as u16 >> 3) << 11) | ((c[1] as u16 >> 2) << 5) | (c[2] as u16 >> 3)
    };
    let from565 = |v: u16| -> [u8; 3] {
        let r = (v >> 11) & 31;
        let g = (v >> 5) & 63;
        let b = v & 31;
        [
            ((r << 3) | (r >> 2)) as u8,
            ((g << 2) | (g >> 4)) as u8,
            ((b << 3) | (b >> 2)) as u8,
        ]
    };

    let mut flags = [0u8; 16];
    let mut body = Vec::with_capacity(1280);
    let mut expect = vec![0u8; NBYTES];

    for (n, blk) in blocks.iter().enumerate() {
        if blk.rgb888 {
            flags[n >> 3] |= 1 << (7 - (n & 7));
        }
        let (e0, e1, levels): ([u8; 3], [u8; 3], usize) = if blk.rgb888 {
            body.extend_from_slice(&blk.e0);
            body.extend_from_slice(&blk.e1);
            let mut packed = [0u8; 4];
            for j in 0..16 {
                packed[j >> 2] |= (blk.idx[j] & 3) << (6 - 2 * (j & 3));
            }
            body.extend_from_slice(&packed);
            (blk.e0, blk.e1, 4)
        } else {
            let v0 = to565(blk.e0);
            let v1 = to565(blk.e1);
            body.extend_from_slice(&v0.to_le_bytes());
            body.extend_from_slice(&v1.to_le_bytes());
            let mut planes = [0u8; 6];
            for j in 0..16 {
                for p in 0..3 {
                    if blk.idx[j] >> p & 1 == 1 {
                        planes[2 * p + (j >> 3)] |= 1 << (7 - (j & 7));
                    }
                }
            }
            body.extend_from_slice(&planes);
            (from565(v0), from565(v1), 8)
        };

        let bx = n % 16;
        let by = n / 16;
        for j in 0..16 {
            let x = bx * 4 + (j & 3);
            let y = by * 4 + (j >> 2);
            let k = (blk.idx[j] as usize) % levels;
            let w = if levels == 4 { W4[k] } else { W8[k] };
            let px = (y * W + x) * 3;
            for ch in 0..3 {
                expect[px + ch] = lerp(e0[ch], e1[ch], w);
            }
        }
    }

    let mut out = Vec::with_capacity(1296);
    out.extend_from_slice(&flags);
    out.extend_from_slice(&body);
    assert_eq!(out.len(), 1296);
    (out, expect)
}

/// A pair of `BC1_DUAL` blocks per flag, so both halves of section 4.5 get
/// exercised in one frame.
pub fn bc1_mixed() -> (Vec<u8>, Vec<u8>) {
    let blocks: [Block; 128] = std::array::from_fn(|n| {
        let rgb888 = n % 2 == 0;
        let levels = if rgb888 { 4u8 } else { 8u8 };
        Block {
            rgb888,
            e0: [(n * 3) as u8, (n * 5 + 20) as u8, (n * 7 + 40) as u8],
            e1: [(255 - n * 2) as u8, (n * 11) as u8, (200 - n) as u8],
            idx: std::array::from_fn(|j| ((j + n) as u8) % levels),
        }
    });
    bc1_dual(&blocks)
}

// ---------------------------------------------------------------------------
// The LZ stream of section 4.4
// ---------------------------------------------------------------------------

/// A greedy LZSS encoder for section 4.4's byte-aligned format.
///
/// Small on purpose: a group is a flag byte then up to eight items, a literal
/// is one byte, a match is two encoding `offset-1` in twelve bits and
/// `length-3` in four. The last group is not padded, and nothing follows the
/// item that completes the output.
pub fn lz_compress(src: &[u8]) -> Vec<u8> {
    // Candidate positions by three-byte key, most recent first.
    let mut index: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    let mut out: Vec<u8> = Vec::new();
    let mut flags_at = 0usize;
    let mut flags = 0u8;
    let mut items = 0usize;
    let mut i = 0usize;

    while i < src.len() {
        if items == 0 {
            flags_at = out.len();
            flags = 0;
            out.push(0);
        }

        let max_len = 18.min(src.len() - i);
        let mut best = (0usize, 0usize);
        if max_len >= 3 {
            let key = [src[i], src[i + 1], src[i + 2]];
            if let Some(cands) = index.get(&key) {
                for &j in cands.iter().take(48) {
                    let off = i - j;
                    if off > 4096 {
                        break;
                    }
                    let mut l = 0;
                    // A match may overlap itself: the decoder copies one byte
                    // at a time, so src[j + l] is always already produced.
                    while l < max_len && src[j + l] == src[i + l] {
                        l += 1;
                    }
                    if l > best.0 {
                        best = (l, off);
                        if l == max_len {
                            break;
                        }
                    }
                }
            }
        }

        let taken = if best.0 >= 3 {
            let (l, off) = best;
            let o = off - 1;
            out.push((o >> 4) as u8);
            out.push((((o & 0x0F) << 4) | (l - 3)) as u8);
            l
        } else {
            flags |= 1 << (7 - items);
            out.push(src[i]);
            1
        };

        for k in i..i + taken {
            if k + 3 <= src.len() {
                index
                    .entry([src[k], src[k + 1], src[k + 2]])
                    .or_default()
                    .insert(0, k);
            }
        }
        i += taken;
        out[flags_at] = flags;
        items = (items + 1) % 8;
    }
    out
}

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

/// Assert two frames are identical, and say where they first differ if not.
pub fn assert_frames_eq(got: &[u8], want: &[u8], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: frame size");
    if got == want {
        return;
    }
    let i = got.iter().zip(want).position(|(a, b)| a != b).unwrap();
    let p = i / 3;
    panic!(
        "{what}: first difference at pixel {p} ({}, {}) channel {}: got {} want {} \
         ({} of {} bytes differ)",
        p % W,
        p / W,
        i % 3,
        got[i],
        want[i],
        got.iter().zip(want).filter(|(a, b)| a != b).count(),
        want.len()
    );
}
