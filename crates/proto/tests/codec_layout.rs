//! Bit layouts, built by hand from the spec prose.
//!
//! `lab_vectors.rs` proves this crate agrees with the lab. That is worth a
//! lot, but both sides could agree and both be wrong about what section 4
//! *says*. These tests build payloads byte by byte from the text of sections
//! 4.1 to 4.6 - nibble order, plane order, index bit order, the interpolation
//! weights - and assert on the pixels that come out. If the prose and the code
//! ever drift, one of these fails.

use screeny_proto::dec::{self, codec, lz};
use screeny_proto::{Rgb888Frame, H, NBYTES, NPIX, W};

fn blank() -> Box<Rgb888Frame> {
    Box::new([0u8; NBYTES])
}

fn px(f: &Rgb888Frame, x: usize, y: usize) -> [u8; 3] {
    let o = (y * W + x) * 3;
    [f[o], f[o + 1], f[o + 2]]
}

// ---------------------------------------------------------------------------
// 4.1 PAL5
// ---------------------------------------------------------------------------

#[test]
fn pal5_index_is_low_nibble_or_high_bit() {
    // "Pixel i has index nib(i) | bit4(i) << 4. Nibbles are two per byte, high
    // nibble first; the bit plane is 8 pixels per byte, MSB first."
    let mut p = vec![0u8; dec::PAL5_LEN];
    for i in 0..32 {
        p[i * 3] = (i * 8) as u8;
        p[i * 3 + 1] = (i * 8 + 1) as u8;
        p[i * 3 + 2] = (i * 8 + 2) as u8;
    }
    let nib_at = 32 * 3;
    let plane_at = nib_at + NPIX / 2;
    for i in 0..NPIX {
        let idx = i % 32;
        let low = (idx & 0x0F) as u8;
        if i % 2 == 0 {
            p[nib_at + (i >> 1)] |= low << 4; // high nibble first
        } else {
            p[nib_at + (i >> 1)] |= low;
        }
        if idx >= 16 {
            p[plane_at + (i >> 3)] |= 1 << (7 - (i & 7)); // MSB first
        }
    }

    let mut f = blank();
    dec::decode(codec::PAL5, &p, &mut f).unwrap();
    for i in 0..NPIX {
        let idx = i % 32;
        assert_eq!(
            px(&f, i % W, i / W),
            [(idx * 8) as u8, (idx * 8 + 1) as u8, (idx * 8 + 2) as u8],
            "pixel {i}"
        );
    }

    // Fixed size, exactly.
    let mut short = p.clone();
    short.pop();
    assert_eq!(
        dec::decode(codec::PAL5, &short, &mut f),
        Err(dec::DecodeError::Short)
    );
    let mut long = p.clone();
    long.push(0);
    assert_eq!(
        dec::decode(codec::PAL5, &long, &mut f),
        Err(dec::DecodeError::Long)
    );
}

// ---------------------------------------------------------------------------
// 4.4 LZ stream
// ---------------------------------------------------------------------------

/// One stream item, exactly as section 4.4 defines it.
enum Item {
    Lit(u8),
    Mat { off: usize, len: usize },
}

/// Assemble items into groups of eight, flag byte first, MSB = item 0.
fn lz_stream(items: &[Item]) -> Vec<u8> {
    let mut out = Vec::new();
    for group in items.chunks(8) {
        let mut flags = 0u8;
        for (k, it) in group.iter().enumerate() {
            if matches!(it, Item::Lit(_)) {
                flags |= 0x80 >> k;
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

#[test]
fn lz_literals_and_matches_are_packed_as_section_4_4_says() {
    let stream = lz_stream(&[
        Item::Lit(b'A'),
        Item::Lit(b'B'),
        Item::Lit(b'C'),
        Item::Mat { off: 3, len: 9 },
    ]);
    // flags 1110_0000, three literals, then offset-1 = 2 and length-3 = 6.
    assert_eq!(stream, vec![0b1110_0000, b'A', b'B', b'C', 0x00, 0x26]);

    let mut out = [0u8; 32];
    let used = lz::inflate(&stream, &mut out, 12).unwrap();
    assert_eq!(used, stream.len(), "the whole stream is consumed");
    assert_eq!(&out[..12], b"ABCABCABCABC");

    // The format's only run-length encoding: a self-overlapping match.
    let stream = lz_stream(&[Item::Lit(b'Z'), Item::Mat { off: 1, len: 18 }]);
    let used = lz::inflate(&stream, &mut out, 19).unwrap();
    assert_eq!(used, stream.len());
    assert_eq!(&out[..19], &[b'Z'; 19]);

    // Offset and length are at their extremes.
    assert_eq!(
        lz_stream(&[Item::Mat { off: 4096, len: 18 }]),
        vec![0b0000_0000, 0xFF, 0xFF]
    );
    assert_eq!(
        lz_stream(&[Item::Mat { off: 1, len: 3 }]),
        vec![0b0000_0000, 0x00, 0x00]
    );

    // A group stops at the item that completes the output: the encoder need
    // not pad, and a decoder must not read the bytes that are not there.
    let stream = lz_stream(&[Item::Lit(1), Item::Lit(2), Item::Lit(3)]);
    assert_eq!(lz::inflate(&stream, &mut out, 2), Ok(3));
    assert_eq!(&out[..2], &[1, 2]);
}

// ---------------------------------------------------------------------------
// 4.2 / 4.3 palette + LZ
// ---------------------------------------------------------------------------

/// A stream that fills `n` bytes with `value`: one literal, then offset-1
/// matches, which is the cheapest legal way to say "all the same".
fn flat_stream(value: u8, n: usize) -> Vec<u8> {
    let mut items = vec![Item::Lit(value)];
    let mut produced = 1;
    while produced < n {
        let len = (n - produced).clamp(3, 18);
        items.push(Item::Mat { off: 1, len });
        produced += len;
    }
    lz_stream(&items)
}

#[test]
fn pal8_lz_is_palette_count_minus_one_then_palette_then_stream() {
    // "[n-1 : u8][palette : n x RGB888][LZ stream -> exactly 2048 index bytes]"
    let mut p = vec![2u8]; // n - 1 = 2, so n = 3
    p.extend_from_slice(&[10, 11, 12, 20, 21, 22, 30, 31, 32]);
    p.extend_from_slice(&flat_stream(1, NPIX));

    let mut f = blank();
    dec::decode(codec::PAL8_LZ, &p, &mut f).unwrap();
    for i in 0..NPIX {
        assert_eq!(px(&f, i % W, i / W), [20, 21, 22], "pixel {i}");
    }

    // "Every decoded index MUST be < n; otherwise the frame is corrupt."
    let mut bad = vec![2u8];
    bad.extend_from_slice(&[10, 11, 12, 20, 21, 22, 30, 31, 32]);
    bad.extend_from_slice(&flat_stream(3, NPIX));
    assert_eq!(
        dec::decode(codec::PAL8_LZ, &bad, &mut f),
        Err(dec::DecodeError::Corrupt)
    );

    // A stream that produces too few bytes is short, not a partial frame.
    let mut truncated = vec![2u8];
    truncated.extend_from_slice(&[10, 11, 12, 20, 21, 22, 30, 31, 32]);
    truncated.extend_from_slice(&flat_stream(1, NPIX - 100));
    assert_eq!(
        dec::decode(codec::PAL8_LZ, &truncated, &mut f),
        Err(dec::DecodeError::Short)
    );
}

#[test]
fn pal4_lz_is_sixteen_colours_then_a_nibble_plane() {
    // "[palette : 16 x RGB888 = 48 B][LZ stream -> exactly 1024 bytes of
    // packed nibbles]", high nibble first as in PAL5.
    let mut p = Vec::new();
    for i in 0..16u8 {
        p.extend_from_slice(&[i * 16, 255 - i * 16, i]);
    }
    // 0x5A: even pixels index 5, odd pixels index 10.
    p.extend_from_slice(&flat_stream(0x5A, NPIX / 2));

    let mut f = blank();
    dec::decode(codec::PAL4_LZ, &p, &mut f).unwrap();
    for i in 0..NPIX {
        let idx: u8 = if i % 2 == 0 { 5 } else { 10 };
        assert_eq!(
            px(&f, i % W, i / W),
            [idx * 16, 255 - idx * 16, idx],
            "pixel {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4.5 BC1_DUAL
// ---------------------------------------------------------------------------

#[test]
fn bc1_dual_flag_selects_the_block_layout() {
    let mut p = vec![0u8; dec::BC1_DUAL_LEN];
    // Block 0 gets flag 1 (RGB888 endpoints, 4 levels); block 1 keeps flag 0
    // (RGB565 endpoints, 8 levels). Flags are MSB first.
    p[0] = 0b1000_0000;
    let blocks = 16;

    // Block 0: e0 = black, e1 = white, indices j & 3.
    let b0 = blocks;
    p[b0..b0 + 6].copy_from_slice(&[0, 0, 0, 255, 255, 255]);
    // Four 2-bit indices per byte, most significant pair first: 0,1,2,3.
    p[b0 + 6..b0 + 10].copy_from_slice(&[0b00_01_10_11; 4]);

    // Block 1: e0 = RGB565 black, e1 = RGB565 white, indices j & 7 from three
    // two-byte bitplanes, plane 0 = LSB.
    let b1 = blocks + 10;
    p[b1..b1 + 4].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    p[b1 + 4..b1 + 10].copy_from_slice(&[0x55, 0x55, 0x33, 0x33, 0x0F, 0x0F]);

    let mut f = blank();
    dec::decode(codec::BC1_DUAL, &p, &mut f).unwrap();

    // W4 = [0,85,171,256] through lerp(0,255,w) = (255*w + 128) >> 8.
    let level4 = [0u8, 85, 170, 255];
    for y in 0..4 {
        for (x, &want) in level4.iter().enumerate() {
            assert_eq!(px(&f, x, y), [want, want, want], "block 0 ({x},{y})");
        }
    }

    // W8 = [0,37,73,110,146,183,219,256] through the same lerp.
    let level8 = [0u8, 37, 73, 110, 145, 182, 218, 255];
    for y in 0..4 {
        for x in 0..4 {
            // j = (y & 3) * 4 + (x & 3); idx = j & 7.
            let want = level8[(y * 4 + x) & 7];
            assert_eq!(px(&f, 4 + x, y), [want, want, want], "block 1 ({x},{y})");
        }
    }

    // Endpoints are exact at both ends: RGB565 0xFFFF expands to pure white.
    assert_eq!(px(&f, 3, 0), [255, 255, 255]);
    assert_eq!(px(&f, 7, 1), [255, 255, 255]);
}

#[test]
fn bc1_dual_blocks_are_in_raster_order() {
    // Block n = by*16 + bx covers x = bx*4.., y = by*4... Mark the last block
    // and check it landed in the bottom-right 4x4.
    let mut p = vec![0u8; dec::BC1_DUAL_LEN];
    let n = 127;
    p[n >> 3] = 1 << (7 - (n & 7)); // flag 1: RGB888 endpoints
    let at = 16 + n * 10;
    p[at..at + 6].copy_from_slice(&[7, 8, 9, 7, 8, 9]); // both endpoints equal
    let mut f = blank();
    dec::decode(codec::BC1_DUAL, &p, &mut f).unwrap();
    for y in H - 4..H {
        for x in W - 4..W {
            assert_eq!(px(&f, x, y), [7, 8, 9], "({x},{y})");
        }
    }
    assert_eq!(px(&f, W - 5, H - 1), [0, 0, 0], "one pixel to the left");
    assert_eq!(px(&f, W - 1, H - 5), [0, 0, 0], "one pixel above");
}

#[test]
fn bc1_dual_rgb565_endpoints_expand_by_bit_replication() {
    // 5-bit 31 -> 255, 5-bit 1 -> 0b00001000 | 0 = 8; 6-bit 63 -> 255.
    let mut p = vec![0u8; dec::BC1_DUAL_LEN];
    // Block 0, flag 0, e0 = e1 = RGB565 (r=31, g=0, b=1) = 0xF801.
    p[16..20].copy_from_slice(&[0x01, 0xF8, 0x01, 0xF8]);
    let mut f = blank();
    dec::decode(codec::BC1_DUAL, &p, &mut f).unwrap();
    assert_eq!(px(&f, 0, 0), [255, 0, 8]);
}

// ---------------------------------------------------------------------------
// 4.6 SOLID, and 4.7's rules
// ---------------------------------------------------------------------------

#[test]
fn solid_is_three_bytes() {
    let mut f = blank();
    dec::decode(codec::SOLID, &[1, 2, 3], &mut f).unwrap();
    for i in 0..NPIX {
        assert_eq!(px(&f, i % W, i / W), [1, 2, 3]);
    }
    assert_eq!(
        dec::decode(codec::SOLID, &[1, 2], &mut f),
        Err(dec::DecodeError::Short)
    );
    assert_eq!(
        dec::decode(codec::SOLID, &[1, 2, 3, 4], &mut f),
        Err(dec::DecodeError::Long)
    );
}

#[test]
fn reserved_and_lab_only_codecs_are_rejected() {
    let mut f = blank();
    // 0x00 and 0xFF: "MUST be rejected". 0x01/0x03/0x20-0x27/0x30/0x31: the
    // lab-only ids. 0xF0-0xFE: experimental, which this device does not offer.
    for c in [
        0x00u8, 0x01, 0x03, 0x04, 0x20, 0x21, 0x27, 0x30, 0x31, 0x7E, 0x80, 0xF0, 0xFE, 0xFF,
    ] {
        assert!(!dec::is_supported(c), "0x{c:02x}");
        assert_eq!(
            dec::decode(c, &[0; 1400], &mut f),
            Err(dec::DecodeError::UnsupportedCodec),
            "0x{c:02x}"
        );
    }
    for c in dec::SUPPORTED_CODECS {
        assert!(dec::is_supported(c), "0x{c:02x}");
    }
    assert_eq!(dec::SUPPORTED_CODECS.len(), 5);
}
