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
use screeny_proto::control::{op, Reply, Telemetry};
use screeny_proto::dec::codec;
use screeny_proto::{ControlPacket, F_KEY, F_STATS_REQ};
use screeny_sim::{Config, Faults, SimDevice, Timing};

const T: Duration = Duration::from_secs(3);

/// Every counter on one line. Which of them moved *is* the subject of the
/// test below, so its failures print all of them rather than the one that
/// happened to be compared.
fn counters(t: &Telemetry) -> String {
    format!(
        "frames_rx {}, frames_shown {}, superseded {}, seq_gaps {}, stale {}, rejected {}",
        t.frames_rx,
        t.frames_shown,
        t.frames_dropped_superseded,
        t.seq_gaps,
        t.frames_dropped_stale,
        t.frames_rejected,
    )
}

#[test]
fn a_sender_can_tell_network_loss_from_a_slow_device() {
    // Section 6.9's whole point. Two runs of the same stream, one with 30%
    // of the datagrams lost on the air and one with the device too slow to
    // draw them, and the counters say which is which.
    //
    // **It is the proportions that say it, not an exact count** (card 143).
    // `frames_dropped_superseded` counts two datagrams that landed in one
    // drain of the frame loop, and with frames 4 ms apart a single scheduling
    // hiccup on the frame thread does that on a perfectly clean link: card
    // 141 saw this test fail with "superseded 1, expected 0" under a loaded
    // parallel suite, and starving this test's threads on purpose reproduces
    // it (four superseded out of thirty-nine received, with no slow device
    // anywhere). What separates the two cases is not a zero but an order of
    // magnitude: a third of the stream missing against none of it, and a
    // device that drew nine in ten of what reached it against one that drew
    // one in seven.
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
        // Settle on a condition rather than on a clock. The faults come off
        // and one last frame goes out: loopback delivers in order and the
        // frame thread drains in order, so the moment that frame is the one
        // on the panel, every datagram of the stream before it has been
        // through `offer_frame` and counted. The 200 ms this replaces was a
        // guess at how long the frame thread would be kept off the CPU by the
        // rest of the suite, and the counters are read on the frame thread.
        sim.set_faults(Faults::default());
        let tail = tx.send(codec::SOLID, F_KEY, &p);
        let snap = sim
            .wait_until(T, |s| s.shown.as_ref().map(|m| m.seq) == Some(tail))
            .unwrap_or_else(|| {
                panic!(
                    "frame {tail}, the last of {}, never reached the panel within {T:?}: {}",
                    n + 1,
                    counters(&sim.telemetry())
                )
            });
        // That last frame is one of the stream as far as the counters go.
        (n + 1, snap.telemetry)
    };

    let (sent, lossy) = stream(Faults {
        drop_pct: 30.0,
        ..Faults::default()
    });
    // Network-limited: three datagrams in ten never happened, so they are
    // simply missing from frames_rx. Both ends of the band matter - too few
    // would mean the stream was never read, not that it was lost.
    assert!(
        lossy.frames_rx > sent / 3 && lossy.frames_rx < sent - sent / 5,
        "network-limited: about seven in ten of {sent} should have arrived; {}",
        counters(&lossy)
    );
    // And the device kept up with what did arrive: a lost packet is not a
    // superseded one. Against `frames_rx` rather than against zero, because
    // zero is a statement about the machine's scheduler.
    assert!(
        lossy.frames_dropped_superseded * 4 <= lossy.frames_rx,
        "a lost packet is not a superseded one: next to none of what arrived \
         should have been superseded; {}",
        counters(&lossy)
    );

    let (sent, slow) = stream(Faults {
        decode_ms: 40,
        ..Faults::default()
    });
    // Decode-limited: nothing was lost on the way in. The tail frame proves
    // the drain reached the end of the stream, so a datagram the kernel had
    // dropped would have left a hole behind it and `seq_gaps` would say so -
    // which is why this asserts both, and neither needs to be exact.
    assert!(
        slow.frames_rx >= sent - sent / 20 && slow.seq_gaps <= sent / 20,
        "decode-limited: the device got everything; {}",
        counters(&slow)
    );
    // But it could not draw it in time: over half of everything that reached
    // it was superseded before it could be shown, where the lossy run is
    // under a quarter.
    assert!(
        slow.frames_dropped_superseded > slow.frames_rx / 2,
        "and could not draw it in time; {}",
        counters(&slow)
    );

    // The sentence the test's name makes, said in counters: whichever way the
    // machine's load leans, these two do not look like one another.
    assert!(
        slow.frames_rx > lossy.frames_rx
            && slow.frames_dropped_superseded > lossy.frames_dropped_superseded,
        "loss and slowness must not look alike: lossy [{}] against slow [{}]",
        counters(&lossy),
        counters(&slow)
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
