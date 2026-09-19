//! Spec section 3.2 (sequence numbers) and 3.3 (newest wins).

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::dec::codec;
use screeny_proto::F_KEY;
use screeny_sim::{Config, DropCause, Event, Faults, SimDevice};

const T: Duration = Duration::from_secs(3);

fn device() -> SimDevice {
    SimDevice::start(Config::for_test()).expect("bind loopback")
}

/// A SOLID payload whose colour encodes `n`, so "which frame is on the panel"
/// is answerable by looking at one pixel.
fn marker(n: u8) -> (Vec<u8>, Vec<u8>) {
    solid([n, 255 - n, 128])
}

#[test]
fn a_sequence_that_wraps_keeps_going() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());

    // Section 3.2: seq wraps mod 2^16 and RFC 1982 says 0 is newer than
    // 0xFFFF. Three frames straddling the wrap, one at a time so each is
    // displayed rather than superseded.
    for (i, seq) in [0xFFFEu16, 0xFFFF, 0x0000, 0x0001].iter().enumerate() {
        let (p, e) = marker(i as u8 + 1);
        tx.send_seq(codec::SOLID, F_KEY, *seq, &p);
        let s = sim
            .wait_until(T, |s| s.telemetry.frames_shown > i as u32)
            .unwrap_or_else(|| panic!("seq {seq:#06x} never displayed"));
        assert_frames_eq(&s.decoded[..], &e, &format!("seq {seq:#06x}"));
        assert_eq!(s.shown.unwrap().seq, *seq);
    }

    let t = sim.telemetry();
    assert_eq!(t.frames_shown, 4);
    assert_eq!(t.frames_dropped_stale, 0, "nothing here was stale");
    assert_eq!(t.seq_gaps, 0, "nor was anything skipped");
}

#[test]
fn duplicates_and_reordered_frames_are_stale() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();

    let (p10, e10) = marker(10);
    tx.send_seq(codec::SOLID, F_KEY, 100, &p10);
    sim.wait_for_frames(1, T).expect("seq 100");

    // A duplicate, a frame from the past, and exactly half the sequence space
    // away - section 3.2's `newer` says none of these is newer.
    let (p99, _) = marker(99);
    for (seq, what) in [
        (100u16, "an exact duplicate"),
        (99, "one frame in the past"),
        (1, "long in the past"),
        (100u16.wrapping_add(0x8000), "exactly half the space away"),
    ] {
        tx.send_seq(codec::SOLID, F_KEY, seq, &p99);
        let ev = sim.wait_for(&mut cursor, T, |e| {
            matches!(
                e,
                Event::Dropped {
                    cause: DropCause::Stale,
                    ..
                }
            )
        });
        assert!(ev.is_some(), "{what} (seq {seq}) should have been stale");
    }

    let s = sim.snapshot();
    assert_frames_eq(&s.decoded[..], &e10, "the panel still shows seq 100");
    assert_eq!(s.telemetry.frames_dropped_stale, 4);
    assert_eq!(s.telemetry.frames_shown, 1);
    assert_eq!(s.telemetry.frames_rx, 1, "a stale frame is not accepted");
    assert_eq!(s.telemetry.seq_gaps, 0, "a stale frame cannot open a gap");
}

#[test]
fn skipped_sequence_numbers_are_counted_as_gaps() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());
    let (p, _) = marker(1);

    tx.send_seq(codec::SOLID, F_KEY, 10, &p);
    sim.wait_for_frames(1, T).unwrap();
    assert_eq!(sim.telemetry().seq_gaps, 0, "the first frame skips nothing");

    tx.send_seq(codec::SOLID, F_KEY, 11, &p);
    sim.wait_for_frames(2, T).unwrap();
    assert_eq!(
        sim.telemetry().seq_gaps,
        0,
        "consecutive frames leave no gap"
    );

    tx.send_seq(codec::SOLID, F_KEY, 20, &p);
    sim.wait_for_frames(3, T).unwrap();
    assert_eq!(sim.telemetry().seq_gaps, 8, "12..=19 were never seen");

    // And across the wrap. RFC 1982 only calls a jump of less than half the
    // space "newer", so walk up to 0xFFFF in quarters first.
    let mut shown = 3u32;
    for seq in [0x4000u16, 0x8000, 0xC000, 0xFFFF] {
        tx.send_seq(codec::SOLID, F_KEY, seq, &p);
        shown += 1;
        sim.wait_for_frames(shown, T)
            .unwrap_or_else(|| panic!("seq {seq:#06x}"));
    }
    let before = sim.telemetry().seq_gaps;
    tx.send_seq(codec::SOLID, F_KEY, 2, &p);
    sim.wait_for_frames(shown + 1, T).unwrap();
    assert_eq!(
        sim.telemetry().seq_gaps - before,
        2,
        "0 and 1 were skipped across the wrap"
    );
}

#[test]
fn a_slow_decode_supersedes_rather_than_queues() {
    // Section 3.3: the device must not let frames queue. With a 60 ms
    // pretend-decode and frames arriving every few milliseconds, most of them
    // are thrown away *in the drain*, which is frames_dropped_superseded and
    // not frames_dropped_stale - the distinction section 6.9 asks a sender to
    // act on.
    let mut cfg = Config::for_test();
    cfg.faults = Faults {
        decode_ms: 60,
        ..Faults::default()
    };
    let dev = SimDevice::start(cfg).expect("bind loopback");
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());

    let n = 60u32;
    for i in 0..n {
        let (p, _) = marker((i % 200) as u8);
        tx.send(codec::SOLID, F_KEY, &p);
        std::thread::sleep(Duration::from_millis(4));
    }

    let s = sim
        .wait_until(T, |s| {
            s.telemetry.frames_rx + s.telemetry.frames_dropped_stale >= n
        })
        .expect("all frames accounted for");
    let t = s.telemetry;

    assert_eq!(t.frames_rx, n, "every datagram was accepted");
    assert!(
        t.frames_dropped_superseded > 10,
        "expected the drain to throw most frames away, got {}",
        t.frames_dropped_superseded
    );
    assert_eq!(
        t.frames_rx,
        t.frames_shown + t.frames_dropped_superseded + t.frames_dropped_decode,
        "every accepted frame is either shown, superseded or undecodable"
    );
    assert_eq!(t.frames_dropped_stale, 0, "nothing arrived out of order");
    assert!(
        t.decode_us >= 60_000,
        "a 60 ms pretend-decode should show up in decode_us, got {}",
        t.decode_us
    );

    // And the newest frame really is the one on the panel.
    let (last, expect) = marker(200);
    tx.send(codec::SOLID, F_KEY, &last);
    let s = sim
        .wait_until(T, |s| s.shown.map(|m| m.seq) == Some(n as u16))
        .expect("the last frame");
    assert_frames_eq(&s.decoded[..], &expect, "newest wins");
}

#[test]
fn a_new_source_resets_the_sequence_but_not_the_counters() {
    // Section 7.3: adopting a source resets last_seq and the jitter EWMAs.
    // Section 6.7: the counters are free-running and only RESET_STATS clears
    // them. A second sender starting at seq 0 must therefore not be stale.
    let mut cfg = Config::for_test();
    cfg.timing.lock_ms = 60;
    cfg.timing.stream_timeout_ms = 120;
    let dev = SimDevice::start(cfg).expect("bind loopback");
    let sim = dev.handle();

    let a = Sender::new(dev.frame_addr());
    let (p, _) = marker(1);
    a.send_seq(codec::SOLID, F_KEY, 50_000, &p);
    sim.wait_for_frames(1, T).unwrap();

    std::thread::sleep(Duration::from_millis(150));

    let b = Sender::new(dev.frame_addr());
    let (p2, e2) = marker(2);
    b.send_seq(codec::SOLID, F_KEY, 0, &p2);
    let s = sim
        .wait_until(T, |s| s.telemetry.frames_shown >= 2)
        .expect("the second sender's first frame");

    assert_frames_eq(&s.decoded[..], &e2, "the new source took the panel");
    assert_eq!(
        s.telemetry.frames_dropped_stale, 0,
        "seq 0 is not stale here"
    );
    assert_eq!(s.telemetry.frames_rx, 2, "counters kept running");
    assert_eq!(s.active_source, Some(b.addr()));
}

#[test]
fn interarrival_and_jitter_track_a_paced_stream() {
    // Section 6.8, on the device's own clock. 20 ms pacing on loopback is
    // steady enough that the EWMA should land near it.
    let dev = device();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());
    let (p, _) = marker(4);

    for _ in 0..40 {
        tx.send(codec::SOLID, F_KEY, &p);
        std::thread::sleep(Duration::from_millis(20));
    }
    let t = sim.telemetry();
    assert!(
        (12_000..32_000).contains(&t.interarrival_us),
        "expected roughly 20 ms, got {} us",
        t.interarrival_us
    );
    assert!(
        t.jitter_us < t.interarrival_us,
        "jitter {} should be well under the interval {}",
        t.jitter_us,
        t.interarrival_us
    );
    assert!(t.interarrival_max_us >= t.interarrival_us);
}
