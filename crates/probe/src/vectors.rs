//! The card 005 test vectors, and a generated moving pattern.
//!
//! The vectors are what tie the firmware's decoders to the encoders card 002
//! actually measured, so streaming them is the only way to test the device's
//! decode path against payloads nobody wrote for it. The generated pattern is
//! for the cases where a *changing* picture matters — tearing, jitter, a soak
//! that would otherwise show one still image for ten minutes.

use std::fs;
use std::path::{Path, PathBuf};

use screeny_proto::dec::codec;

/// One pre-encoded frame payload and the pixels the frozen card-002 lab
/// decoder produced from it.
pub struct Vector {
    pub name: String,
    pub codec: u8,
    pub payload: Vec<u8>,
    pub expect: Vec<u8>,
}

/// Where the checked-in vectors live, relative to this crate.
pub fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../proto/tests/vectors")
}

/// Every checked-in vector. Panics if the directory has moved: a silently
/// empty list would turn the bit-exactness tests into no-ops.
pub fn all() -> Vec<Vector> {
    let v = load(&default_dir()).expect("crates/proto/tests/vectors");
    assert!(v.len() >= 27, "expected the card-005 vector set");
    v
}

/// Read `manifest.tsv` and every payload it names.
pub fn load(dir: &Path) -> Result<Vec<Vector>, String> {
    let manifest = dir.join("manifest.tsv");
    let text = fs::read_to_string(&manifest)
        .map_err(|e| format!("{}: {e}\n(pass --vectors DIR)", manifest.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        let (Some(name), Some(codec), Some(len)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let codec: u8 = codec.parse().map_err(|_| format!("bad codec in {line:?}"))?;
        let want: usize = len.parse().map_err(|_| format!("bad length in {line:?}"))?;
        let path = dir.join(format!("{name}.bin"));
        let payload = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if payload.len() != want {
            return Err(format!(
                "{name}: manifest says {want} bytes, file has {}",
                payload.len()
            ));
        }
        let rgb = dir.join(format!("{name}.rgb"));
        let expect = fs::read(&rgb).map_err(|e| format!("{}: {e}", rgb.display()))?;
        if expect.len() != screeny_proto::NBYTES {
            return Err(format!(
                "{name}: expected frame is {} bytes, not {}",
                expect.len(),
                screeny_proto::NBYTES
            ));
        }
        out.push(Vector {
            name: name.to_string(),
            codec,
            payload,
            expect,
        });
    }
    if out.is_empty() {
        return Err(format!("no vectors in {}", dir.display()));
    }
    Ok(out)
}

pub fn codec_name(id: u8) -> &'static str {
    match id {
        codec::PAL5 => "PAL5",
        codec::PAL8_LZ => "PAL8_LZ",
        codec::PAL4_LZ => "PAL4_LZ",
        codec::BC1_DUAL => "BC1_DUAL",
        codec::SOLID => "SOLID",
        _ => "?",
    }
}

pub fn codec_by_name(s: &str) -> Option<u8> {
    Some(match s.to_ascii_lowercase().as_str() {
        "pal5" => codec::PAL5,
        "pal8_lz" | "pal8" => codec::PAL8_LZ,
        "pal4_lz" | "pal4" => codec::PAL4_LZ,
        "bc1_dual" | "bc1" => codec::BC1_DUAL,
        "solid" => codec::SOLID,
        _ => s.parse().ok()?,
    })
}

// ---------------------------------------------------------------------------
// A generated moving pattern
// ---------------------------------------------------------------------------

/// Frame `n` of a moving pattern, as a `SOLID` payload: a slow colour cycle.
///
/// Kept dim on purpose. The panel runs off laptop USB and a full-brightness
/// white field across a ten-minute soak is exactly what the bench rules say
/// not to do.
pub fn solid_frame(n: u64) -> Vec<u8> {
    let t = (n % 360) as f64 * std::f64::consts::PI / 180.0;
    let c = |p: f64| ((t + p).sin() * 0.35 + 0.4).clamp(0.0, 1.0);
    vec![
        (c(0.0) * 90.0) as u8,
        (c(2.09) * 90.0) as u8,
        (c(4.19) * 90.0) as u8,
    ]
}

/// Frame `n` of a moving pattern, as a `PAL4_LZ` payload.
///
/// Diagonal bars plus a bouncing block, so that tearing has something to tear
/// and a frozen panel is obvious. 16 colours, which is what the codec is for.
pub fn pal4_frame(n: u64) -> Vec<u8> {
    const W: usize = screeny_proto::W;
    const H: usize = screeny_proto::H;

    // 16 entries of RGB888. A dim ramp, plus one bright marker.
    let mut payload = Vec::with_capacity(1200);
    for i in 0..16u32 {
        let v = (i * 5) as u8;
        payload.push(v);
        payload.push((v * 3) / 4);
        payload.push(v / 2);
    }
    // Index 15 is the bouncing block: readable, not blinding.
    let last = payload.len() - 3;
    payload[last..].copy_from_slice(&[0xc0, 0xa0, 0x30]);

    // Indices, two per byte, high nibble first.
    let phase = (n % 16) as usize;
    let bx = (n % (2 * (W as u64 - 8))) as usize;
    let bx = if bx >= W - 8 { 2 * (W - 8) - bx } else { bx };
    let by = ((n / 3) % (2 * (H as u64 - 8))) as usize;
    let by = if by >= H - 8 { 2 * (H - 8) - by } else { by };

    let mut idx = vec![0u8; W * H / 2];
    for y in 0..H {
        for x in 0..W {
            let inside = x >= bx && x < bx + 8 && y >= by && y < by + 8;
            let v = if inside {
                15u8
            } else {
                // Diagonal bars that crawl one step per frame.
                (((x + y * 2 + phase) / 4) % 12) as u8
            };
            let p = y * W + x;
            if p.is_multiple_of(2) {
                idx[p / 2] |= v << 4;
            } else {
                idx[p / 2] |= v;
            }
        }
    }

    payload.extend_from_slice(&lz_compress(&idx));
    payload
}

/// A deliberately simple encoder for the spec section 4.4 LZ stream:
/// run-length matches where a byte repeats, literals otherwise.
///
/// It is not a good compressor and does not try to be. It exists so the
/// generated pattern is a *legal* variable-rate payload, it exercises the
/// device's overlapping-match path (every match here has `offset = 1`, which
/// section 4.4 calls the format's only run-length encoding), and it never
/// needs a hash table or a window. The checked-in vectors are what test the
/// decoder against a real encoder.
pub fn lz_compress(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + src.len() / 8 + 16);
    let mut i = 0usize;
    while i < src.len() {
        let flags_at = out.len();
        out.push(0u8);
        let mut flags = 0u8;
        for bit in 0..8 {
            if i >= src.len() {
                break;
            }
            // How far the byte before `i` repeats forward. Needs one byte of
            // output already produced, so position 0 is always a literal.
            let mut run = 0usize;
            if i > 0 {
                let prev = src[i - 1];
                while run < 18 && i + run < src.len() && src[i + run] == prev {
                    run += 1;
                }
            }
            if run >= 3 {
                // offset = 1, length = run: eighteen copies of the previous
                // byte at most. The offset is stored biased by one, so "one
                // byte back" is a zero on the wire.
                let off = 0u16;
                out.push((off >> 4) as u8);
                out.push((((off & 0xF) << 4) as u8) | (run as u8 - 3));
                i += run;
            } else {
                flags |= 1 << (7 - bit);
                out.push(src[i]);
                i += 1;
            }
        }
        out[flags_at] = flags;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_pal4_frames_decode() {
        let mut dst = [0u8; screeny_proto::NBYTES];
        for n in [0u64, 1, 7, 33, 1000, 65_535] {
            let p = pal4_frame(n);
            assert!(p.len() <= screeny_proto::MAX_PIXEL_PAYLOAD, "{} bytes", p.len());
            screeny_proto::decode(codec::PAL4_LZ, &p, &mut dst)
                .unwrap_or_else(|e| panic!("frame {n}: {e:?}"));
        }
    }

    #[test]
    fn generated_solid_frames_decode() {
        let mut dst = [0u8; screeny_proto::NBYTES];
        for n in [0u64, 90, 180, 359] {
            screeny_proto::decode(codec::SOLID, &solid_frame(n), &mut dst).unwrap();
        }
    }

    /// The encoder has to round-trip through the crate's own decoder for
    /// every shape of input, not just the ones the pattern happens to make.
    #[test]
    fn lz_round_trips() {
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for trial in 0..200 {
            let mut src = vec![0u8; 1024];
            for b in src.iter_mut() {
                // Long runs sometimes, noise other times.
                *b = if trial % 3 == 0 {
                    (rnd() % 4) as u8
                } else {
                    (rnd() % 256) as u8
                };
            }
            if trial % 5 == 0 {
                src = vec![0x5a; 1024];
            }
            let packed = lz_compress(&src);
            let mut out = [0u8; 1024];
            let used = screeny_proto::dec::lz::inflate(&packed, &mut out, 1024).expect("inflate");
            assert_eq!(used, packed.len(), "trailing bytes, trial {trial}");
            let n = 1024;
            assert_eq!(n, 1024);
            assert_eq!(&out[..], &src[..], "trial {trial}");
        }
    }
}
