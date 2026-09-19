//! Totality under mutation: no input may panic, index out of bounds, write
//! outside `dst`, or fail to terminate.
//!
//! This is the card's second priority and the reason the crate exists in this
//! shape: the firmware parses whatever arrives on a UDP port, on a chip with
//! no MMU, where an out-of-bounds write is not a crash but a silently wrong
//! device. There is no fuzzing infrastructure here on purpose - a seeded
//! xorshift and plain loops run under `cargo test`, in CI, every time.
//!
//! What "cannot go out of bounds" means mechanically: [`Rgb888Frame`] is a
//! fixed-size array, so *any* stray index inside a decoder is a panic in a
//! debug build, and these tests run in debug. A test that completes is a
//! proof that none of these several hundred thousand inputs reached one.
//! Termination is proved the same crude way: a decoder that looped forever
//! would hang the suite rather than pass it.

use screeny_proto::control::{Reply, Request, Telemetry};
use screeny_proto::dec::{self, lz};
use screeny_proto::txt::{self, DeviceInfo};
use screeny_proto::{ControlPacket, FramePacket, Packet, Rgb888Frame, HEADER_LEN, NBYTES};

/// xorshift32. Seeded, so a failure is reproducible from the seed alone.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() as usize) % n
        }
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 13) as u8
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
    /// Between 0 and `max - 1` random bytes.
    fn bytes_below(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max);
        self.bytes(n)
    }
}

fn blank() -> Box<Rgb888Frame> {
    Box::new([0u8; NBYTES])
}

/// The real payloads, as a seed corpus: mutating something valid reaches
/// deeper into a decoder than random bytes ever will.
fn corpus() -> Vec<(u8, Vec<u8>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).expect("manifest.tsv");
    let mut out = Vec::new();
    for line in manifest.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        let name = f.next().unwrap();
        let codec: u8 = f.next().unwrap().parse().unwrap();
        out.push((
            codec,
            std::fs::read(dir.join(format!("{name}.bin"))).expect("payload"),
        ));
    }
    out
}

/// One of six mutations, chosen by the rng. Between them they cover the three
/// things that break a decoder: a length that lies, a field that points
/// outside itself, and a bit that flips a branch.
fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut v = src.to_vec();
    match rng.below(6) {
        // Flip one to sixteen bits.
        0 => {
            if !v.is_empty() {
                for _ in 0..=rng.below(16) {
                    let i = rng.below(v.len());
                    v[i] ^= 1 << rng.below(8);
                }
            }
        }
        // Overwrite a run with random bytes.
        1 => {
            if !v.is_empty() {
                let at = rng.below(v.len());
                let n = rng.below(v.len() - at).min(64);
                for b in &mut v[at..at + n] {
                    *b = rng.byte();
                }
            }
        }
        // Truncate anywhere, including to nothing.
        2 => {
            let n = rng.below(v.len() + 1);
            v.truncate(n);
        }
        // Extend with junk: the "payload longer than the codec wants" case.
        3 => {
            let n = 1 + rng.below(32);
            v.extend((0..n).map(|_| rng.byte()));
        }
        // Splice: keep a prefix, append a random tail.
        4 => {
            let at = rng.below(v.len() + 1);
            v.truncate(at);
            v.extend(rng.bytes_below(80));
        }
        // Replace wholesale.
        _ => v = rng.bytes_below(1500),
    }
    v
}

#[test]
fn no_payload_can_break_a_decoder() {
    let corpus = corpus();
    let mut rng = Rng(0xC0DE_0005);
    let mut dst = blank();
    let mut ok = 0usize;
    let mut tried = 0usize;

    // Every mutant is offered to every codec, not just the one it came from:
    // a device decodes whatever the header's `codec` byte says, and a sender
    // bug (or an attacker) can disagree with the payload.
    for _ in 0..40_000 {
        let (_, seed) = &corpus[rng.below(corpus.len())];
        let m = mutate(&mut rng, seed);
        for c in dec::SUPPORTED_CODECS {
            tried += 1;
            if dec::decode(c, &m, &mut dst).is_ok() {
                ok += 1;
            }
        }
        // Reserved and lab-only ids must be refused, never decoded.
        for c in [0x00, 0x01, 0x03, 0x20, 0x30, 0xF0, 0xFF] {
            assert_eq!(
                dec::decode(c, &m, &mut dst),
                Err(dec::DecodeError::UnsupportedCodec)
            );
        }
    }

    // Pure noise, for the lengths the corpus does not reach.
    for _ in 0..12_000 {
        let m = rng.bytes_below(1465);
        for c in dec::SUPPORTED_CODECS {
            tried += 1;
            if dec::decode(c, &m, &mut dst).is_ok() {
                ok += 1;
            }
        }
    }

    eprintln!("{tried} decode attempts, {ok} accepted");
    assert!(tried > 250_000);
    assert!(ok > 0, "mutation never produced a decodable payload");
}

/// The simplest legal encoding of `src`: every item a literal, so every
/// group is `0xFF` followed by eight bytes. Mutating one of these lands
/// squarely in the match path, which random bytes rarely reach.
fn all_literals(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + src.len() / 8 + 1);
    for chunk in src.chunks(8) {
        out.push(0xFFu8 >> (8 - chunk.len()) << (8 - chunk.len()));
        out.extend_from_slice(chunk);
    }
    out
}

#[test]
fn lz_streams_always_terminate_and_stay_in_bounds() {
    let mut rng = Rng(0x1EE7_0005);
    let mut dst = blank();
    let mut produced_exact = 0usize;

    // Sanity: the helper really does produce streams this decoder accepts.
    for want in [1usize, 7, 8, 9, 1024, 2048] {
        let src = rng.bytes(want);
        let stream = all_literals(&src);
        let used = lz::inflate(&stream, &mut dst[..], want).expect("all-literal stream");
        assert_eq!(used, stream.len());
        assert_eq!(&dst[..want], &src[..]);
    }

    for i in 0..60_000 {
        let want = if i % 2 == 0 { 2048 } else { 1024 };
        let stream = match i % 3 {
            // Short random streams: these mostly run out mid-item.
            0 => rng.bytes_below(24),
            // Long random streams: deep into the match path.
            1 => rng.bytes_below(1400),
            // A valid stream, mutated: the interesting middle ground.
            _ => {
                let src = rng.bytes(want);
                let valid = all_literals(&src);
                mutate(&mut rng, &valid)
            }
        };
        if let Ok(used) = lz::inflate(&stream, &mut dst[..], want) {
            assert!(used <= stream.len());
            produced_exact += 1;
        }
    }
    eprintln!("{produced_exact} of 60000 streams inflated to the exact size");
    assert!(produced_exact > 100, "the success path was never exercised");
    // `dst` must be small-but-legal too: inflate must refuse, not index past.
    let mut tiny = [0u8; 8];
    assert_eq!(
        lz::inflate(&[0xFF, 1, 2, 3, 4, 5, 6, 7, 8, 9], &mut tiny, 2048),
        Err(dec::DecodeError::Corrupt)
    );
    assert_eq!(lz::inflate(&[], &mut tiny, 0), Ok(0));
}

#[test]
fn lz_rejects_matches_that_reach_before_the_output() {
    // The single most dangerous field in the format: a 12-bit offset that
    // points behind the start of what has been produced.
    let mut dst = blank();
    // flags = 0 -> every item is a match; the first one has nothing behind it.
    assert_eq!(
        lz::inflate(&[0x00, 0xFF, 0xFF], &mut dst[..], 2048),
        Err(dec::DecodeError::Corrupt)
    );
    // One literal, then a match of offset 2 with only 1 byte produced.
    assert_eq!(
        lz::inflate(&[0x80, 0x41, 0x00, 0x10], &mut dst[..], 2048),
        Err(dec::DecodeError::Corrupt)
    );
    // Offset 1, length 3, after one literal: legal, and self-overlapping.
    let used = lz::inflate(&[0x80, 0x41, 0x00, 0x00], &mut dst[..], 4).unwrap();
    assert_eq!(used, 4);
    assert_eq!(&dst[..4], b"AAAA");
    // A match that would overshoot `want`.
    assert_eq!(
        lz::inflate(&[0x80, 0x41, 0x00, 0x0F], &mut dst[..], 4),
        Err(dec::DecodeError::Corrupt)
    );
}

#[test]
fn no_datagram_can_break_the_packet_parser() {
    let mut rng = Rng(0xFEED_0005);
    let corpus = corpus();

    // Real datagrams to mutate.
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for (codec, payload) in corpus.iter().take(6) {
        let mut buf = vec![0u8; 1600];
        let n = FramePacket {
            codec: *codec,
            flags: 0x0B,
            seq: 42,
            timestamp_us: Some(7),
            payload,
        }
        .write(&mut buf)
        .unwrap();
        seeds.push(buf[..n].to_vec());
    }
    for op in 0..0x10u8 {
        let mut buf = [0u8; 128];
        let n = ControlPacket {
            op,
            flags: 0x01,
            req_id: 3,
            body: &[1, 2, 3, 4, 5],
        }
        .write(&mut buf)
        .unwrap();
        seeds.push(buf[..n].to_vec());
    }

    let mut parsed = 0usize;
    for i in 0..120_000 {
        let d = if i % 4 == 0 {
            rng.bytes_below(80)
        } else {
            {
                let pick = rng.below(seeds.len());
                mutate(&mut rng, &seeds[pick])
            }
        };

        // Whatever comes back, the invariants hold.
        match Packet::parse(&d) {
            Ok(Packet::Frame(f)) => {
                parsed += 1;
                assert!(f.encoded_len() <= d.len());
                assert!(f.payload.len() <= screeny_proto::MAX_PIXEL_PAYLOAD);
                assert_eq!(f.has_ts(), f.timestamp_us.is_some());
                // Re-writing a parsed packet reproduces it exactly.
                let mut out = vec![0u8; d.len() + 8];
                let n = f.write(&mut out).unwrap();
                assert_eq!(FramePacket::parse(&out[..n]), Ok(f));
            }
            Ok(Packet::Control(c)) => {
                parsed += 1;
                assert_eq!(c.encoded_len(), HEADER_LEN + c.body.len());
                assert!(c.encoded_len() <= d.len());
                let mut out = vec![0u8; d.len() + 8];
                let n = c.write(&mut out).unwrap();
                assert_eq!(ControlPacket::parse(&out[..n]), Ok(c));

                // And the bodies parse (or refuse) without panicking.
                let _ = Request::decode(c.op, c.body);
                let _ = Reply::decode(c.op, c.flags, c.body);
                let _ = Telemetry::decode(c.body);
            }
            Err(_) => {}
        }
        // The port-specific entry points must never disagree about a packet
        // they both accept.
        if let Ok(f) = FramePacket::parse(&d) {
            assert_eq!(Packet::parse(&d), Ok(Packet::Frame(f)));
        }
        if let Ok(c) = ControlPacket::parse(&d) {
            assert_eq!(Packet::parse(&d), Ok(Packet::Control(c)));
        }
    }
    eprintln!("{parsed} of 120000 mutated datagrams parsed");
    assert!(parsed > 1_000, "mutation never produced a valid packet");
}

#[test]
fn no_body_can_break_a_control_op() {
    let mut rng = Rng(0x0BAD_0005);
    let mut decoded = 0usize;
    for _ in 0..80_000 {
        let op = if rng.next() & 1 == 0 {
            (rng.below(0x10)) as u8
        } else {
            rng.byte()
        };
        let body = rng.bytes_below(120);

        if let Ok(req) = Request::decode(op, &body) {
            decoded += 1;
            // Anything that decodes must re-encode to the same bytes.
            assert_eq!(req.op(), op);
            let mut out = vec![0u8; req.encoded_len() + 4];
            let n = req.write(0x1234, &mut out).unwrap();
            assert_eq!(n, req.encoded_len());
            assert_eq!(&out[HEADER_LEN..n], &body[..]);
            let pkt = ControlPacket::parse(&out[..n]).unwrap();
            assert_eq!(Request::decode(pkt.op, pkt.body), Ok(req));
        }
        let flags = rng.byte();
        if let Ok(rep) = Reply::decode(op, flags, &body) {
            let mut out = vec![0u8; rep.encoded_len() + 4];
            let n = rep.write(op, 0x1234, &mut out).unwrap();
            let pkt = ControlPacket::parse(&out[..n]).unwrap();
            assert_eq!(Reply::decode(pkt.op, pkt.flags, pkt.body), Ok(rep));
        }
        let _ = Telemetry::decode(&body);
    }
    eprintln!("{decoded} of 80000 random bodies were valid requests");
    assert!(decoded > 100);
}

#[test]
fn no_txt_record_can_break_the_parser() {
    let mut rng = Rng(0x7C70_0005);
    let mut good = [0u8; 256];
    let gn = DeviceInfo {
        codecs: dec::CODECS_TXT,
        fw: "0.1.0",
        id: "a4cf12",
        name: "Desk panel",
        ..DeviceInfo::DEFAULT
    }
    .write(&mut good)
    .unwrap();

    let mut parsed = 0usize;
    for i in 0..60_000 {
        let d = if i % 3 == 0 {
            rng.bytes_below(300)
        } else {
            mutate(&mut rng, &good[..gn])
        };

        // Iteration must always terminate and stay inside the buffer.
        let mut n = 0;
        for e in txt::iter(&d) {
            n += 1;
            assert!(e.key.len() <= 255);
            assert!(n <= d.len(), "iterator produced more entries than bytes");
        }
        let _ = txt::find(&d, "codecs");

        if let Ok(info) = DeviceInfo::parse(&d) {
            parsed += 1;
            // The codec list is derived, so it must be walkable too.
            let ids: Vec<u8> = info.codec_ids().collect();
            assert!(ids.len() <= info.codecs.len() + 1);
            let _ = info.best_codec(&dec::SUPPORTED_CODECS);
            // A record we could read, we can write back and read again.
            let mut out = [0u8; 1024];
            if let Ok(n) = info.write(&mut out) {
                assert_eq!(DeviceInfo::parse(&out[..n]), Ok(info));
            }
        }
    }
    eprintln!("{parsed} of 60000 mutated TXT records parsed as DeviceInfo");
    assert!(parsed > 100);
}

#[test]
fn indexed_frame_expand_refuses_an_index_outside_the_palette() {
    use screeny_proto::{IndexedFrame, NPIX};
    let mut idx = Box::new([0u8; NPIX]);
    let pal = [[1u8, 2, 3], [4, 5, 6]];
    let mut dst = blank();
    IndexedFrame {
        palette: &pal,
        indices: &idx,
    }
    .expand(&mut dst)
    .unwrap();
    assert_eq!(&dst[..6], &[1, 2, 3, 1, 2, 3]);

    idx[NPIX - 1] = 2;
    assert_eq!(
        IndexedFrame {
            palette: &pal,
            indices: &idx,
        }
        .expand(&mut dst),
        Err(dec::DecodeError::Corrupt)
    );
    // An empty palette is refused rather than divided by.
    assert!(IndexedFrame {
        palette: &[],
        indices: &idx,
    }
    .expand(&mut dst)
    .is_err());
}
