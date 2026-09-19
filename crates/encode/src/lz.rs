//! The LZ compressor for index planes, and the two LZ wire payloads.
//!
//! Format: spec 4.4, byte-aligned LZSS. Lifted from `lab/src/enc/lz.rs`
//! (card 002) with the match finder's scratch hoisted out of the function -
//! the lab allocated and zeroed a 256 KB hash head table on **every call**,
//! and the ladder calls this up to six times per frame (card 031).
//!
//! The table is also smaller here: the input is at most 2048 bytes, so 8192
//! buckets is already a load factor of a quarter and the chains stay short.

// Positions are stored as `i32` with -1 meaning "end of chain"; the input is
// at most 2048 bytes, so the cast can never wrap.
#![allow(clippy::cast_possible_wrap)]

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 18;
const MAX_OFF: usize = 4096;
const HASH_BITS: u32 = 13;

/// Default hash-chain depth: what the lab used.
pub const CHAIN_FULL: usize = 192;
/// Shorter chains for the fast profile. Costs a little ratio, and the ladder
/// only cares about ratio near a rung boundary.
pub const CHAIN_FAST: usize = 24;

enum Item {
    Lit(u8),
    Mat { off: usize, len: usize },
}

/// Reusable working storage for [`deflate`].
pub struct LzScratch {
    head: Vec<i32>,
    prev: Vec<i32>,
    items: Vec<Item>,
}

impl Default for LzScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl LzScratch {
    /// Allocate the tables once.
    #[must_use]
    pub fn new() -> Self {
        LzScratch {
            head: vec![-1i32; 1 << HASH_BITS],
            prev: vec![-1i32; screeny_proto::NPIX],
            items: Vec::with_capacity(1024),
        }
    }

    fn reset(&mut self, n: usize) {
        self.head.fill(-1);
        if self.prev.len() < n {
            self.prev.resize(n, -1);
        }
        self.prev[..n].fill(-1);
        self.items.clear();
    }
}

#[inline(always)]
fn hash3(s: &[u8], i: usize) -> usize {
    (((s[i] as usize) << 8) ^ ((s[i + 1] as usize) << 4) ^ (s[i + 2] as usize))
        & ((1 << HASH_BITS) - 1)
}

fn best_match(src: &[u8], i: usize, head: &[i32], prev: &[i32], chain: usize) -> (usize, usize) {
    if i + MIN_MATCH > src.len() {
        return (0, 0);
    }
    let maxlen = MAX_MATCH.min(src.len() - i);
    let mut cand = head[hash3(src, i)];
    let mut tries = 0;
    let (mut blen, mut boff) = (0usize, 0usize);
    while cand >= 0 && tries < chain {
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

/// Compress `src` onto the end of `out`. Greedy with one step of lazy
/// matching.
///
/// The stream is self-terminating against a known output length: it ends with
/// the item that completes `src.len()` decoded bytes and carries no padding,
/// which is what spec 4.4 requires.
pub fn deflate(src: &[u8], out: &mut Vec<u8>, scratch: &mut LzScratch, chain: usize) {
    let n = src.len();
    scratch.reset(n);
    let LzScratch { head, prev, items } = scratch;

    let mut i = 0usize;
    while i < n {
        if i + MIN_MATCH <= n {
            let (off, len) = best_match(src, i, head, prev, chain);
            if len >= MIN_MATCH {
                // Index position i before probing i+1, so the chain the lazy
                // probe walks is complete.
                let h = hash3(src, i);
                prev[i] = head[h];
                head[h] = i as i32;
                let mut take = true;
                if i + 1 + MIN_MATCH <= n {
                    let (_, len2) = best_match(src, i + 1, head, prev, chain);
                    if len2 > len {
                        take = false;
                    }
                }
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
                } else {
                    items.push(Item::Lit(src[i]));
                    i += 1;
                }
                continue;
            }
            let h = hash3(src, i);
            prev[i] = head[h];
            head[h] = i as i32;
        }
        items.push(Item::Lit(src[i]));
        i += 1;
    }

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
}

/// Build a `PAL8_LZ` payload: `[n-1][palette][LZ of 2048 index bytes]`
/// (spec 4.2). `pal` must have 1..=256 entries and every index must be `< n`.
#[must_use]
pub fn emit_pal8_lz(
    pal: &[[u8; 3]],
    idx: &[u8; screeny_proto::NPIX],
    scratch: &mut LzScratch,
    chain: usize,
) -> Vec<u8> {
    debug_assert!((1..=256).contains(&pal.len()));
    let mut out = Vec::with_capacity(512);
    out.push((pal.len() - 1) as u8);
    for c in pal {
        out.extend_from_slice(c);
    }
    deflate(idx, &mut out, scratch, chain);
    out
}

/// Build a `PAL4_LZ` payload: `[16 x RGB888][LZ of 1024 nibble bytes]`
/// (spec 4.3). `pal` may be short; missing entries are black.
#[must_use]
pub fn emit_pal4_lz(
    pal: &[[u8; 3]],
    idx: &[u8; screeny_proto::NPIX],
    scratch: &mut LzScratch,
    chain: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    for i in 0..16 {
        out.extend_from_slice(pal.get(i).unwrap_or(&[0, 0, 0]));
    }
    let mut nib = vec![0u8; screeny_proto::NPIX / 2];
    super::pal::pack_nibbles(idx, &mut nib);
    deflate(&nib, &mut out, scratch, chain);
    out
}
