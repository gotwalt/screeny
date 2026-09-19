//! Cross-check against the frozen card-002 lab.
//!
//! `tests/vectors/` holds, for every vector, the wire payload (`.bin`) and the
//! 6144-byte RGB888 frame the **lab's own decoder** produced from it (`.rgb`).
//! The lab's encoders and decoders were written independently of each other
//! and measured against real content, so agreeing with them bit for bit is the
//! evidence that lifting the decoders into this crate changed nothing.
//!
//! Regenerate with `cd tools/gen-vectors && cargo run --release`. See
//! `tests/vectors/README.md`.

use std::path::{Path, PathBuf};

use screeny_proto::dec::{self, DecodeError};
use screeny_proto::{Rgb888Frame, NBYTES};

struct Vector {
    name: String,
    codec: u8,
    payload: Vec<u8>,
    expect: Vec<u8>,
}

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

fn load() -> Vec<Vector> {
    let dir = vectors_dir();
    let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).expect("manifest.tsv");
    let mut out = Vec::new();
    for line in manifest.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        let name = f.next().expect("name").to_string();
        let codec: u8 = f.next().expect("codec").parse().expect("codec id");
        let want_len: usize = f.next().expect("len").parse().expect("payload len");
        let payload = std::fs::read(dir.join(format!("{name}.bin"))).expect("payload");
        let expect = std::fs::read(dir.join(format!("{name}.rgb"))).expect("expected frame");
        assert_eq!(payload.len(), want_len, "{name}: manifest length");
        assert_eq!(expect.len(), NBYTES, "{name}: expected frame size");
        out.push(Vector {
            name,
            codec,
            payload,
            expect,
        });
    }
    assert!(!out.is_empty(), "no vectors");
    out
}

fn blank() -> Box<Rgb888Frame> {
    Box::new([0u8; NBYTES])
}

#[test]
fn every_vector_decodes_to_the_lab_s_pixels() {
    let vectors = load();
    let mut seen = [false; 256];
    for v in &vectors {
        let mut dst = blank();
        dec::decode(v.codec, &v.payload, &mut dst)
            .unwrap_or_else(|e| panic!("{}: {e:?}", v.name));
        assert!(
            dst[..] == v.expect[..],
            "{}: pixels differ from the lab decoder",
            v.name
        );
        seen[v.codec as usize] = true;
    }
    for c in dec::SUPPORTED_CODECS {
        assert!(seen[c as usize], "no vector covers codec 0x{c:02x}");
    }
    eprintln!("{} vectors across 5 codecs", vectors.len());
}

#[test]
fn vector_lengths_match_the_spec_table() {
    for v in load() {
        match v.codec {
            dec::codec::PAL5 => assert_eq!(v.payload.len(), dec::PAL5_LEN, "{}", v.name),
            dec::codec::BC1_DUAL => {
                assert_eq!(v.payload.len(), dec::BC1_DUAL_LEN, "{}", v.name);
            }
            dec::codec::SOLID => assert_eq!(v.payload.len(), dec::SOLID_LEN, "{}", v.name),
            // Section 4: the variable-rate codecs must still fit one datagram.
            _ => assert!(
                v.payload.len() <= screeny_proto::MAX_PIXEL_PAYLOAD,
                "{} is {} bytes",
                v.name,
                v.payload.len()
            ),
        }
    }
}

#[test]
fn a_vector_in_a_real_packet_round_trips() {
    // The whole point of the header/decoder split: parse a datagram, hand
    // `codec` and `payload` straight to `decode`.
    use screeny_proto::{FramePacket, F_KEY};
    for v in load() {
        let pkt = FramePacket {
            codec: v.codec,
            flags: F_KEY,
            seq: 1234,
            timestamp_us: Some(99),
            payload: &v.payload,
        };
        let mut buf = vec![0u8; screeny_proto::MAX_UDP_PAYLOAD];
        let n = pkt.write(&mut buf).expect("write");
        let back = FramePacket::parse(&buf[..n]).expect("parse");
        assert_eq!(back.timestamp_us, Some(99));

        let mut dst = blank();
        dec::decode(back.codec, back.payload, &mut dst).expect("decode");
        assert!(dst[..] == v.expect[..], "{}", v.name);
    }
}

#[test]
fn truncating_a_real_payload_never_succeeds_by_accident() {
    for v in load() {
        for cut in 0..v.payload.len() {
            let mut dst = blank();
            match dec::decode(v.codec, &v.payload[..cut], &mut dst) {
                Err(_) => {}
                Ok(()) => panic!("{} truncated to {cut} bytes decoded", v.name),
            }
        }
    }
}

#[test]
fn appending_to_a_real_payload_is_rejected() {
    // Section 2.3 puts padding beyond `len`, so a payload that is longer than
    // its codec calls for is a bug, not padding.
    for v in load() {
        let mut longer = v.payload.clone();
        longer.push(0);
        let mut dst = blank();
        assert_eq!(
            dec::decode(v.codec, &longer, &mut dst),
            Err(DecodeError::Long),
            "{}",
            v.name
        );
    }
}
