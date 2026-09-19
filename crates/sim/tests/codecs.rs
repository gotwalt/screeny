//! Every codec, over a real UDP socket, bit for bit.
//!
//! The point of this file: a sender encodes, the datagram crosses loopback,
//! the simulator decodes, and the pixels on the panel are *identical* to what
//! the sender meant. Anything less and a sender author cannot tell their own
//! encoder's error from the device's.

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::dec::codec;
use screeny_proto::{DecodeError, F_KEY, NBYTES};
use screeny_sim::{Config, DropCause, Event, SimDevice};

const T: Duration = Duration::from_secs(2);

fn device() -> SimDevice {
    SimDevice::start(Config::for_test()).expect("bind loopback")
}

#[test]
fn every_checked_in_vector_arrives_bit_exact() {
    let dev = device();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());

    let vectors = vectors();
    let mut shown = 0u32;
    for v in &vectors {
        tx.send(v.codec, F_KEY, &v.payload);
        shown += 1;
        let s = sim
            .wait_until(T, |s| s.telemetry.frames_shown >= shown)
            .unwrap_or_else(|| panic!("{}: never displayed", v.name));
        assert_frames_eq(&s.decoded[..], &v.expect, &v.name);
        assert_eq!(s.shown.unwrap().codec, v.codec, "{}", v.name);
        assert_eq!(
            s.telemetry.last_codec, v.codec,
            "{}: telemetry byte 47",
            v.name
        );
        assert_eq!(s.shown.unwrap().bytes, v.payload.len(), "{}", v.name);
    }

    let t = sim.telemetry();
    assert_eq!(t.frames_rx, vectors.len() as u32);
    assert_eq!(t.frames_shown, vectors.len() as u32);
    assert_eq!(t.frames_dropped_decode, 0);
    assert_eq!(t.frames_rejected, 0);
    assert_eq!(t.seq_gaps, 0);
}

#[test]
fn hand_built_payloads_arrive_bit_exact() {
    let dev = device();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());
    let mut shown = 0u32;

    let mut check = |codec: u8, payload: &[u8], expect: &[u8], what: &str, shown: &mut u32| {
        tx.send(codec, F_KEY, payload);
        *shown += 1;
        let s = sim
            .wait_until(T, |s| s.telemetry.frames_shown >= *shown)
            .unwrap_or_else(|| panic!("{what}: never displayed"));
        assert_frames_eq(&s.decoded[..], expect, what);
    };

    let (p, e) = solid([0, 128, 128]);
    check(codec::SOLID, &p, &e, "SOLID teal", &mut shown);

    let f = indexed_runs(32);
    check(codec::PAL5, &pal5(&f), &f.expect(), "PAL5 32 colours", &mut shown);

    let f = indexed_runs(16);
    check(
        codec::PAL4_LZ,
        &pal4_lz(&f),
        &f.expect(),
        "PAL4_LZ 16 colours",
        &mut shown,
    );

    for n in [1usize, 2, 17, 100, 256] {
        let f = indexed_runs(n);
        let payload = pal8_lz(&f);
        assert!(
            payload.len() <= screeny_proto::MAX_PIXEL_PAYLOAD,
            "PAL8_LZ {n} colours: {} bytes is over budget",
            payload.len()
        );
        check(
            codec::PAL8_LZ,
            &payload,
            &f.expect(),
            &format!("PAL8_LZ {n} colours"),
            &mut shown,
        );
    }

    let (p, e) = bc1_mixed();
    check(
        codec::BC1_DUAL,
        &p,
        &e,
        "BC1_DUAL, both flag halves",
        &mut shown,
    );
}

#[test]
fn a_timestamp_prefix_does_not_disturb_the_pixels() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());

    let (p, e) = solid([17, 99, 200]);
    tx.send_full(codec::SOLID, F_KEY, 1, Some(0xDEAD_BEEF), &p);
    let s = sim.wait_for_frames(1, T).expect("displayed");
    assert_frames_eq(&s.decoded[..], &e, "SOLID with HAS_TS");
    let meta = s.shown.unwrap();
    assert_eq!(meta.timestamp_us, Some(0xDEAD_BEEF));
    assert_eq!(meta.bytes, 3, "the 4-byte prefix is not pixel payload");
}

#[test]
fn an_unshowable_frame_leaves_the_previous_one_lit() {
    let dev = device();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();

    let (good, expect) = solid([9, 9, 9]);
    tx.send(codec::SOLID, F_KEY, &good);
    sim.wait_for_frames(1, T).expect("the good frame");

    // Section 4: 0x00 and 0xFF are reserved and MUST be rejected; 0x03 is a
    // lab-only id a v1 device does not advertise. Section 4.7: all three are
    // frames_dropped_decode, and the panel keeps what it had.
    let bad: &[(u8, &[u8], &str)] = &[
        (0x00, &good, "reserved codec 0x00"),
        (0xFF, &good, "reserved codec 0xFF"),
        (0x03, &good, "lab-only codec 0x03"),
        (codec::SOLID, &good[..2], "SOLID payload one byte short"),
        (codec::SOLID, &[1, 2, 3, 4], "SOLID payload one byte long"),
        (codec::PAL5, &good, "PAL5 payload far too short"),
        (codec::BC1_DUAL, &good, "BC1_DUAL payload far too short"),
        (codec::PAL8_LZ, &[0u8], "PAL8_LZ with a palette and no stream"),
    ];
    for (c, payload, what) in bad {
        tx.send(*c, F_KEY, payload);
        let ev = sim
            .wait_for(&mut cursor, T, |e| {
                matches!(e, Event::Dropped { cause: DropCause::Decode(_), .. })
            })
            .unwrap_or_else(|| panic!("{what}: expected a decode drop"));
        if let Event::Dropped { cause: DropCause::Decode(e), .. } = ev {
            assert_ne!(e, DecodeError::Corrupt, "{what}: unexpected error shape");
        }
        let s = sim.snapshot();
        assert_frames_eq(&s.decoded[..], &expect, what);
        assert_eq!(s.telemetry.frames_shown, 1, "{what}: nothing new was shown");
    }

    let t = sim.telemetry();
    assert_eq!(t.frames_dropped_decode, bad.len() as u32);
    assert_eq!(t.frames_rx, 1 + bad.len() as u32, "all of them were accepted");
    assert_eq!(t.frames_rejected, 0, "a bad codec is not a bad packet");
}

#[test]
fn bytes_beyond_len_are_padding_and_are_ignored() {
    // Section 2.3: "Bytes beyond 8 + len are padding and MUST be ignored; a
    // sender MAY pad." Section 4 puts the exact-length rule *inside* len, so
    // padding a fixed-size codec out to a round number still decodes.
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());

    let (p, e) = solid([250, 5, 60]);
    let mut datagram = vec![0x53, 0x10, codec::SOLID, F_KEY, 1, 0, 3, 0];
    datagram.extend_from_slice(&p);
    datagram.extend_from_slice(&[0xAA; 64]); // padding
    tx.send_raw(&datagram);

    let s = sim.wait_for_frames(1, T).expect("displayed");
    assert_frames_eq(&s.decoded[..], &e, "padded SOLID");
    assert_eq!(s.shown.unwrap().bytes, 3);
}

#[test]
fn the_panel_model_is_applied_to_the_panel_and_not_to_the_pixels() {
    let dev = device();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());

    // A value that is not on a 64-level duty step, so the model has to move it.
    let (p, e) = solid([3, 77, 254]);
    tx.send(codec::SOLID, F_KEY, &p);
    let s = sim.wait_for_frames(1, T).expect("displayed");

    assert_frames_eq(&s.decoded[..], &e, "decoded is what the sender sent");
    assert_ne!(
        &s.panel[..],
        &s.decoded[..],
        "the panel quantises to 64 duty steps; these pixels are not on one"
    );
    // Still 64x32 and still plausible.
    assert_eq!(s.panel.len(), NBYTES);
    for px in s.panel.chunks(3) {
        assert!(px[2] > px[0], "blue was the largest channel and still is");
    }
}
