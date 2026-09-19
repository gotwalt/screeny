//! Nothing that arrives on either port may take the device down.
//!
//! `screeny-proto` has its own totality tests; this file is about the layer
//! above them - that the simulator *counts* a bad datagram and carries on,
//! rather than panicking a thread and going quiet. A device with no MMU on a
//! UDP port is the thing being modelled, and a simulator that survived
//! nonsense by accident would be no evidence at all.

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::control::Request;
use screeny_proto::dec::codec;
use screeny_proto::{Reject, F_KEY, MAX_UDP_PAYLOAD};
use screeny_sim::{Config, Event, SimDevice, Timing};

const T: Duration = Duration::from_secs(2);

fn device() -> SimDevice {
    SimDevice::start(Config {
        timing: Timing {
            info_min_interval_ms: 0,
            ..Timing::SPEC
        },
        ..Config::for_test()
    })
    .unwrap()
}

#[test]
fn every_way_a_frame_datagram_can_be_wrong_is_counted() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();

    let too_long_len = {
        // len = 1465, one over MAX_PIXEL_PAYLOAD, with a datagram to match.
        let mut d = vec![0x53, 0x10, codec::SOLID, F_KEY];
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&1465u16.to_le_bytes());
        d.resize(8 + 1465, 0);
        d
    };

    let cases: &[(&[u8], Reject, &str)] = &[
        (&[], Reject::Short, "an empty datagram"),
        (&[0x53], Reject::Short, "one byte"),
        (
            &[0x53, 0x10, 0x7F, 0x01, 0, 0, 3],
            Reject::Short,
            "seven bytes: one short of a header",
        ),
        (
            &[0x52, 0x10, 0x7F, 0x01, 0, 0, 3, 0, 1, 2, 3],
            Reject::BadMagic,
            "the wrong magic byte",
        ),
        (
            &[0x53, 0x20, 0x7F, 0x01, 0, 0, 3, 0, 1, 2, 3],
            Reject::BadVersion,
            "version 2",
        ),
        (
            &[0x53, 0x00, 0x7F, 0x01, 0, 0, 3, 0, 1, 2, 3],
            Reject::BadVersion,
            "version 0",
        ),
        (
            &[0x53, 0x11, 0x7F, 0x01, 0, 0, 3, 0, 1, 2, 3],
            Reject::BadType,
            "the reserved FRAME_FRAG type",
        ),
        (
            &[0x53, 0x1F, 0x7F, 0x01, 0, 0, 3, 0, 1, 2, 3],
            Reject::BadType,
            "a reserved packet type",
        ),
        (
            &[0x53, 0x12, 0x01, 0x00, 1, 0, 0, 0],
            Reject::BadType,
            "a CONTROL on the frame port",
        ),
        (
            &[0x53, 0x10, 0x7F, 0x01, 0, 0, 200, 0, 1, 2, 3],
            Reject::BadLength,
            "a len the datagram does not back up",
        ),
        (
            &too_long_len,
            Reject::TooLong,
            "a len over MAX_PIXEL_PAYLOAD",
        ),
        (
            &[0x53, 0x10, 0x7F, 0x09, 0, 0, 2, 0, 1, 2],
            Reject::ShortTimestamp,
            "HAS_TS with no room for the timestamp",
        ),
    ];

    for (bytes, want, what) in cases {
        tx.send_raw(bytes);
        let ev = sim
            .wait_for(&mut cursor, T, |e| matches!(e, Event::Rejected { .. }))
            .unwrap_or_else(|| panic!("{what}: expected a rejection"));
        match ev {
            Event::Rejected { reason, .. } => {
                assert_eq!(reason, Some(*want), "{what}");
            }
            other => panic!("{what}: {other:?}"),
        }
    }

    let t = sim.telemetry();
    assert_eq!(t.frames_rejected, cases.len() as u32);
    assert_eq!(t.frames_shown, 0);
    assert_eq!(t.frames_rx, 0);

    // And the device is still a device.
    let mut tx2 = Sender::new(dev.frame_addr());
    let (p, e) = solid([1, 2, 3]);
    tx2.send(codec::SOLID, F_KEY, &p);
    let s = sim.wait_for_frames(1, T).expect("still alive");
    assert_frames_eq(&s.decoded[..], &e, "after all that");
}

#[test]
fn reserved_frame_flag_bits_are_ignored_and_not_rejected() {
    // Section 3.1: "reserved. MUST be 0. A receiver MUST ignore bits it does
    // not know." Ignoring is not the same as rejecting, and a device that
    // rejected them would make adding a flag a breaking change.
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());
    let (p, e) = solid([40, 50, 60]);

    tx.send_seq(codec::SOLID, F_KEY | 0xF0, 1, &p);
    let s = sim.wait_for_frames(1, T).expect("displayed anyway");
    assert_frames_eq(&s.decoded[..], &e, "reserved flags set");
    assert_eq!(s.telemetry.frames_rejected, 0);

    // Even with KEY clear: v1 has no stateful codecs, so every frame decodes
    // standalone regardless of what the bit says.
    tx.send_seq(codec::SOLID, 0, 2, &p);
    sim.wait_for_frames(2, T).expect("and again");
}

#[test]
fn random_bytes_on_either_port_cannot_stop_the_device() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());
    let ctrl = Ctrl::new(dev.control_addr());

    // A seeded xorshift, so a failure repeats.
    let mut s: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };

    for i in 0..4_000 {
        let len = (next() % (MAX_UDP_PAYLOAD as u64 + 8)) as usize;
        let mut d: Vec<u8> = (0..len).map(|_| (next() >> 24) as u8).collect();
        // Every fourth datagram starts with a plausible header, so the fuzz
        // reaches past the magic-byte check and into the decoders.
        if i % 4 == 0 && d.len() >= 8 {
            d[0] = 0x53;
            d[1] = if i % 8 == 0 { 0x10 } else { 0x12 };
        }
        if i % 2 == 0 {
            tx.send_raw(&d);
        } else {
            ctrl.send_raw(&d);
        }
        if i % 256 == 0 {
            // Do not outrun the socket buffers.
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    std::thread::sleep(Duration::from_millis(200));
    // Both threads still answer.
    assert!(
        ctrl.call(&Request::Ping, 1).is_some(),
        "the control thread survived"
    );

    // Some of that noise will have been a well-formed header from one of
    // these two sockets, so a third source may be holding the lock out for
    // LOCK_MS. Keep offering until it lapses.
    let mut tx2 = Sender::new(dev.frame_addr());
    let (p, e) = solid([7, 8, 9]);
    let mut got = None;
    for _ in 0..30 {
        tx2.send(codec::SOLID, F_KEY, &p);
        if let Some(s) = sim.wait_until(Duration::from_millis(100), |s| s.decoded[..3] == [7, 8, 9])
        {
            got = Some(s);
            break;
        }
    }
    let s = got.expect("the frame thread survived");
    assert_frames_eq(&s.decoded[..], &e, "after 4000 random datagrams");
}

#[test]
fn corrupt_payloads_of_the_right_length_are_decode_drops_at_worst() {
    // The decoders are total (proto's own mutation tests say so); what this
    // checks is that the simulator keeps counting and keeps drawing.
    let dev = device();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());

    let (good, expect) = solid([11, 22, 33]);
    tx.send(codec::SOLID, F_KEY, &good);
    sim.wait_for_frames(1, T).unwrap();

    let mut s: u64 = 0xDEAD_BEEF_CAFE_F00D;
    let mut next = || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };

    // Exactly the length each codec demands, entirely random contents.
    let sizes = [
        (codec::PAL5, screeny_proto::dec::PAL5_LEN),
        (codec::BC1_DUAL, screeny_proto::dec::BC1_DUAL_LEN),
        (codec::SOLID, screeny_proto::dec::SOLID_LEN),
        (codec::PAL8_LZ, 900),
        (codec::PAL4_LZ, 400),
    ];
    for round in 0..200 {
        let (c, len) = sizes[round % sizes.len()];
        let payload: Vec<u8> = (0..len).map(|_| (next() >> 24) as u8).collect();
        tx.send(c, F_KEY, &payload);
        std::thread::sleep(Duration::from_millis(1));
    }

    std::thread::sleep(Duration::from_millis(200));
    let t = sim.telemetry();
    assert_eq!(t.frames_rejected, 0, "these were all well-formed packets");
    assert!(t.frames_rx >= 200);
    assert_eq!(
        t.frames_rx,
        t.frames_shown + t.frames_dropped_superseded + t.frames_dropped_decode,
        "every accepted frame is accounted for"
    );

    // PAL5, BC1_DUAL and SOLID have no corrupt case at their fixed length, so
    // some of this random noise really was displayed - and the panel is still
    // a panel.
    assert!(t.frames_shown > 1);
    let s = sim.snapshot();
    assert_eq!(s.decoded.len(), expect.len());
    let mut tx2 = Sender::new(dev.frame_addr());
    std::thread::sleep(Duration::from_millis(600)); // let the lock lapse
    let (p, e) = solid([1, 1, 1]);
    tx2.send(codec::SOLID, F_KEY, &p);
    let s = sim
        .wait_until(T, |s| s.decoded[..3] == [1, 1, 1])
        .expect("a clean frame after the noise");
    assert_frames_eq(&s.decoded[..], &e, "recovered");
}

#[test]
fn a_datagram_larger_than_the_protocol_allows_is_rejected() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();

    // 2000 bytes with a len that claims all of them. Loopback will carry it;
    // the air would not, and the device must not act on it either.
    let mut d = vec![0x53, 0x10, codec::SOLID, F_KEY, 1, 0];
    d.extend_from_slice(&1992u16.to_le_bytes());
    d.resize(2000, 0x5A);
    tx.send_raw(&d);

    let ev = sim
        .wait_for(&mut cursor, T, |e| matches!(e, Event::Rejected { .. }))
        .expect("rejected");
    assert!(matches!(
        ev,
        Event::Rejected {
            reason: Some(Reject::TooLong),
            ..
        }
    ));
    assert_eq!(sim.telemetry().frames_shown, 0);
}
