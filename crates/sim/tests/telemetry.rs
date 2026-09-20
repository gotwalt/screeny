//! Spec section 6.7 (the 48-byte struct) and 6.9 (what a sender does with
//! it), in process.
//!
//! The wire-level half - that a `STATS_REQ` is answered on the frame port
//! with a 48-byte `REPLY`/`req_id 0` packet, the 100 ms rate limit on it, and
//! what `RESET_STATS` does and does not zero - is now
//! `screeny_probe::suite::telemetry` and runs from `tests/conformance.rs`.
//! What is left needs the simulator: injected faults, and reading the wire
//! back against `SimHandle`'s own view so that a field filled in at the wrong
//! offset is caught.

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::control::{op, Reply};
use screeny_proto::dec::codec;
use screeny_proto::{ControlPacket, F_KEY, F_STATS_REQ};
use screeny_sim::{Config, Faults, SimDevice, Timing};

const T: Duration = Duration::from_secs(3);

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

    // **Poll for the reply rather than looking once.** `drain()` settles for
    // five milliseconds, and the device sends its `TELEMETRY` after it has let
    // go of the core lock - so on a busy machine one look can be too early.
    // That was the flake card 234 uncovered by adding two multi-second tests
    // to this crate; the deadline is the same `T` the rest of the test uses.
    let deadline = std::time::Instant::now() + T;
    let d = loop {
        if let Some(d) = tx
            .drain()
            .into_iter()
            .find(|d| ControlPacket::parse(d).map(|p| p.op) == Ok(op::TELEMETRY))
        {
            break d;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no TELEMETRY came back within {T:?}"
        );
    };
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
