//! Spec section 6.4 (telemetry piggybacked on the frame stream), 6.7 (the
//! 48-byte struct) and 6.9 (what a sender does with it).

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::control::{op, Reply};
use screeny_proto::dec::codec;
use screeny_proto::{ControlPacket, F_KEY, F_STATS_REQ};
use screeny_sim::{Config, Faults, SimDevice, Timing};

const T: Duration = Duration::from_secs(3);

#[test]
fn a_stats_request_is_answered_on_the_frame_port() {
    // Section 6.4: the reply comes "from the frame port to the datagram's
    // source port ... and a sender MUST accept it there". A sender's control
    // socket sees nothing.
    let dev = SimDevice::start(Config::for_test()).unwrap();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());
    let ctrl = Ctrl::new(dev.control_addr());

    let (p, _) = solid([3, 4, 5]);
    tx.send(codec::SOLID, F_KEY | F_STATS_REQ, &p);

    let d = tx.recv(T).expect("a TELEMETRY on the frame socket");
    let pkt = ControlPacket::parse(&d).expect("a CONTROL packet on the frame port");
    assert_eq!(pkt.op, op::TELEMETRY);
    assert!(pkt.is_reply(), "section 6.2: REPLY set");
    assert_eq!(pkt.req_id, 0, "section 6.2: req_id 0, nobody asked by id");
    assert_eq!(pkt.body.len(), 48);
    assert!(
        ctrl.recv(Duration::from_millis(100)).is_none(),
        "the control socket sees nothing"
    );

    match Reply::decode(pkt.op, pkt.flags, pkt.body).unwrap() {
        Reply::Telemetry(t) => {
            assert_eq!(t.frames_rx, 1);
            assert_eq!(t.frames_shown, 1, "after processing this frame");
            assert_eq!(t.last_codec, codec::SOLID);
            assert_eq!(t.state, screeny_proto::control::state::LIVE);
            assert_eq!(t.brightness, 255);
            assert_eq!(t.frames_dropped_stale, 0);
            assert_eq!(t.frames_rejected, 0);
            assert!(t.uptime_ms < 60_000, "a fresh device");
            assert_eq!(t.rssi_dbm, -55);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        sim.telemetry().frames_shown,
        1,
        "and the handle agrees with the wire"
    );
}

#[test]
fn telemetry_is_rate_limited_to_one_per_hundred_milliseconds() {
    // Section 6.2. A sender that ignores section 6.4's "MUST NOT set it on
    // more than one frame in 100 ms" gets one reply, not thirty.
    let dev = SimDevice::start(Config::for_test()).unwrap();
    let mut tx = Sender::new(dev.frame_addr());
    let (p, _) = solid([1, 1, 1]);

    for _ in 0..12 {
        tx.send(codec::SOLID, F_KEY | F_STATS_REQ, &p);
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    let replies = tx.drain();
    assert!(
        (1..=2).contains(&replies.len()),
        "expected one reply for ~60 ms of requests, got {}",
        replies.len()
    );
    for d in &replies {
        assert_eq!(ControlPacket::parse(d).unwrap().op, op::TELEMETRY);
    }

    // Past the window, another one.
    std::thread::sleep(Duration::from_millis(120));
    tx.send(codec::SOLID, F_KEY | F_STATS_REQ, &p);
    assert!(tx.recv(T).is_some(), "the window reopened");
}

#[test]
fn a_sender_can_tell_network_loss_from_a_slow_device() {
    // Section 6.9's whole point. Two runs of the same stream, one with 30%
    // of the datagrams lost on the air and one with the device too slow to
    // draw them, and the counters say which is which.
    let stream = |faults: Faults| {
        let dev = SimDevice::start(Config {
            faults,
            timing: Timing {
                // Keep the source lock alive across the 4 ms gaps.
                ..Timing::SPEC
            },
            ..Config::for_test()
        })
        .unwrap();
        let sim = dev.handle();
        let mut tx = Sender::new(dev.frame_addr());
        let (p, _) = solid([2, 2, 2]);
        let n = 60u32;
        for _ in 0..n {
            tx.send(codec::SOLID, F_KEY, &p);
            std::thread::sleep(Duration::from_millis(4));
        }
        std::thread::sleep(Duration::from_millis(200));
        (n, sim.telemetry())
    };

    let (sent, lossy) = stream(Faults {
        drop_pct: 30.0,
        ..Faults::default()
    });
    assert!(
        lossy.frames_rx < (sent as f32 * 0.95) as u32,
        "network-limited: frames_rx {} out of {sent} sent",
        lossy.frames_rx
    );
    assert_eq!(
        lossy.frames_dropped_superseded, 0,
        "a lost packet is not a superseded one"
    );

    let (sent, slow) = stream(Faults {
        decode_ms: 40,
        ..Faults::default()
    });
    assert_eq!(
        slow.frames_rx, sent,
        "decode-limited: the device got everything"
    );
    assert!(
        slow.frames_dropped_superseded > 0,
        "and could not draw it in time"
    );
}

#[test]
fn reset_stats_zeroes_the_counters_the_wire_reports() {
    let dev = SimDevice::start(Config::for_test()).unwrap();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let mut tx = Sender::new(dev.frame_addr());
    let (p, _) = solid([6, 7, 8]);

    for _ in 0..5 {
        tx.send(codec::SOLID, F_KEY, &p);
        std::thread::sleep(Duration::from_millis(15));
    }
    sim.wait_until(T, |s| s.telemetry.frames_rx >= 5).unwrap();

    ctrl.call(&screeny_proto::control::Request::ResetStats, 1)
        .expect("a reply");
    let t = sim
        .wait_until(T, |s| s.telemetry.frames_rx == 0)
        .unwrap()
        .telemetry;
    assert_eq!(t.frames_shown, 0);
    assert_eq!(t.seq_gaps, 0);
    assert_eq!(t.decode_us_max, 0);
    assert_eq!(t.interarrival_max_us, 0);
    assert_ne!(t.uptime_ms, 0, "uptime is not a counter");
    assert_eq!(
        t.last_codec,
        codec::SOLID,
        "nor is the codec of the frame still on the panel"
    );
}

#[test]
fn the_telemetry_struct_round_trips_through_its_own_offsets() {
    // Section 6.7 is a table of byte offsets. Reading the wire back with
    // proto's decoder and comparing against the handle's view catches any
    // field the simulator fills in at the wrong place.
    let dev = SimDevice::start(Config::for_test()).unwrap();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());
    let (p, _) = solid([1, 2, 3]);

    // One at a time, so each is its own drain and none supersedes another.
    tx.send(codec::SOLID, F_KEY, &p);
    sim.wait_for_frames(1, T).unwrap();

    tx.send_seq(codec::SOLID, F_KEY, 0, &p); // a duplicate of seq 0: stale
    sim.wait_until(T, |s| s.telemetry.frames_dropped_stale == 1)
        .expect("stale");

    tx.send(codec::BC1_DUAL, F_KEY, &p); // 3 bytes where 1296 are required
    sim.wait_until(T, |s| s.telemetry.frames_dropped_decode == 1)
        .expect("decode drop");

    tx.send(codec::SOLID, F_KEY | F_STATS_REQ, &p);

    let d = tx
        .drain()
        .into_iter()
        .find(|d| ControlPacket::parse(d).map(|p| p.op) == Ok(op::TELEMETRY))
        .expect("a TELEMETRY");
    let pkt = ControlPacket::parse(&d).unwrap();
    let Ok(Reply::Telemetry(wire)) = Reply::decode(pkt.op, pkt.flags, pkt.body) else {
        panic!()
    };

    assert_eq!(wire.frames_dropped_stale, 1);
    assert_eq!(wire.frames_dropped_decode, 1);
    assert!(wire.frames_shown >= 2);
    assert_eq!(wire.state, screeny_proto::control::state::LIVE);
    assert_eq!(wire.last_codec, codec::SOLID);

    let local = sim.telemetry();
    assert_eq!(wire.frames_rx, local.frames_rx);
    assert_eq!(wire.frames_shown, local.frames_shown);
    assert_eq!(wire.frames_dropped_stale, local.frames_dropped_stale);
    assert_eq!(wire.frames_dropped_decode, local.frames_dropped_decode);
    assert_eq!(wire.seq_gaps, local.seq_gaps);
    assert_eq!(wire.rssi_dbm, local.rssi_dbm);
    assert_eq!(wire.brightness, local.brightness);
}
