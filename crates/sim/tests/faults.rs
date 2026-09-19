//! The deliberate misbehaviour a sender needs to be tested against, and the
//! frame sink `--dump-dir` is built on.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use screeny_proto::dec::codec;
use screeny_proto::{Rgb888Frame, F_KEY};
use screeny_sim::{Config, Faults, SimDevice, Timing};

const T: Duration = Duration::from_secs(3);

fn slow_lock() -> Timing {
    // Frames arrive a few milliseconds apart in these tests; the spec's own
    // lock and timeout are long enough to hold across that.
    Timing::SPEC
}

/// Send `n` frames from one source, `gap_ms` apart. The sender is passed in
/// rather than created here: rebinding the socket would make it a new source
/// (spec section 7.1) and the old one's lock would shut it out.
fn stream(tx: &mut Sender, n: u32, gap_ms: u64) {
    let (p, _) = solid([5, 6, 7]);
    for _ in 0..n {
        tx.send(codec::SOLID, F_KEY, &p);
        std::thread::sleep(Duration::from_millis(gap_ms));
    }
}

#[test]
fn dropped_datagrams_are_invisible_rather_than_counted() {
    // Loss on the air is not something a receiver can observe directly: the
    // packet never happened. What it sees is a hole in the sequence numbers,
    // which is what seq_gaps is for.
    let dev = SimDevice::start(Config {
        faults: Faults {
            drop_pct: 50.0,
            ..Faults::default()
        },
        timing: slow_lock(),
        ..Config::for_test()
    })
    .unwrap();
    let sim = dev.handle();

    let n = 80u32;
    let mut tx = Sender::new(dev.frame_addr());
    stream(&mut tx, n, 4);
    std::thread::sleep(Duration::from_millis(200));

    let t = sim.telemetry();
    assert!(
        t.frames_rx < n && t.frames_rx > n / 8,
        "about half of {n} should have arrived, got {}",
        t.frames_rx
    );
    assert_eq!(t.frames_rejected, 0, "a dropped packet is not a bad packet");
    assert_eq!(t.frames_dropped_stale, 0);
    assert!(
        t.seq_gaps > 0,
        "the holes in the sequence should be visible"
    );
    // Every frame after the first one that got through is either received
    // or a gap. The ones dropped *before* the stream was noticed at all are
    // unknowable - there is no earlier seq to measure a gap from - which is
    // itself worth pinning down.
    let first_seen = n - (t.frames_rx + t.seq_gaps);
    assert!(
        t.frames_rx + t.seq_gaps <= n,
        "more accounted for than sent: {} + {} > {n}",
        t.frames_rx,
        t.seq_gaps
    );
    assert!(
        first_seen < n / 4,
        "the stream should be noticed early on, not after {first_seen} frames"
    );
}

#[test]
fn the_fault_seed_makes_a_lossy_run_repeat() {
    let run = || {
        let dev = SimDevice::start(Config {
            faults: Faults {
                drop_pct: 40.0,
                ..Faults::default()
            },
            fault_seed: 0xABCD_1234,
            timing: slow_lock(),
            ..Config::for_test()
        })
        .unwrap();
        let sim = dev.handle();
        let mut tx = Sender::new(dev.frame_addr());
        stream(&mut tx, 60, 3);
        std::thread::sleep(Duration::from_millis(200));
        sim.telemetry().frames_rx
    };
    assert_eq!(run(), run(), "the same seed drops the same packets");
}

#[test]
fn a_delayed_link_raises_jitter_without_losing_frames() {
    let dev = SimDevice::start(Config {
        faults: Faults {
            delay_ms: 25,
            ..Faults::default()
        },
        timing: slow_lock(),
        ..Config::for_test()
    })
    .unwrap();
    let sim = dev.handle();

    let mut tx = Sender::new(dev.frame_addr());
    stream(&mut tx, 60, 10);
    std::thread::sleep(Duration::from_millis(300));

    let t = sim.telemetry();
    assert_eq!(
        t.frames_rx + t.frames_dropped_stale,
        60,
        "nothing was lost; a delayed datagram still arrives"
    );
    assert!(
        t.jitter_us > 1_000,
        "a random 0..25 ms hold should be visible as jitter, got {} us",
        t.jitter_us
    );
    assert!(
        t.interarrival_max_us > t.interarrival_us,
        "and the worst interval should be worse than the average"
    );
}

#[test]
fn faults_can_be_turned_on_and_off_while_it_runs() {
    let dev = SimDevice::start(Config {
        timing: slow_lock(),
        ..Config::for_test()
    })
    .unwrap();
    let sim = dev.handle();
    assert!(sim.faults().is_clean());
    let mut tx = Sender::new(dev.frame_addr());

    stream(&mut tx, 20, 3);
    std::thread::sleep(Duration::from_millis(100));
    let clean = sim.telemetry().frames_rx;
    assert_eq!(clean, 20, "a clean link loses nothing on loopback");

    sim.set_faults(Faults {
        drop_pct: 80.0,
        ..Faults::default()
    });
    assert!(!sim.faults().is_clean());
    stream(&mut tx, 40, 3);
    std::thread::sleep(Duration::from_millis(100));
    let lossy = sim.telemetry().frames_rx - clean;
    assert!(lossy < 30, "80% loss should show: {lossy} of 40 arrived");

    sim.set_faults(Faults::default());
    // The frame thread reads the fault settings once per drain, so give it a
    // drain to notice.
    std::thread::sleep(Duration::from_millis(20));
    let before = sim.telemetry().frames_rx;
    stream(&mut tx, 20, 3);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        sim.telemetry().frames_rx - before,
        20,
        "and turning it off restores the link"
    );
}

#[test]
fn the_frame_sink_sees_exactly_the_frames_that_reach_the_panel() {
    // This is what --dump-dir is built on: a callback on the frame thread
    // rather than a poll, so "every Nth displayed frame" means every Nth.
    type Seen = Arc<Mutex<Vec<(u16, Vec<u8>)>>>;
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = Arc::clone(&seen);
        Box::new(move |frame: &Rgb888Frame, meta: &screeny_sim::FrameMeta| {
            seen.lock().unwrap().push((meta.seq, frame[..3].to_vec()));
        }) as screeny_sim::FrameSink
    };

    let dev = SimDevice::start_with(
        Config {
            timing: slow_lock(),
            ..Config::for_test()
        },
        Some(sink),
    )
    .unwrap();
    let sim = dev.handle();
    let mut tx = Sender::new(dev.frame_addr());

    for i in 0..10u8 {
        let (p, _) = solid([i, 100, 200]);
        tx.send(codec::SOLID, F_KEY, &p);
        // Far enough apart that no frame supersedes another.
        std::thread::sleep(Duration::from_millis(20));
    }
    sim.wait_until(T, |s| s.telemetry.frames_shown >= 10)
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 10, "one callback per displayed frame");
    for (i, (seq, px)) in seen.iter().enumerate() {
        assert_eq!(*seq, i as u16);
        assert_eq!(px, &vec![i as u8, 100, 200], "frame {i}");
    }
}

#[test]
fn a_slow_device_never_calls_the_sink_for_a_superseded_frame() {
    let count = Arc::new(Mutex::new(0u32));
    let sink = {
        let count = Arc::clone(&count);
        Box::new(move |_: &Rgb888Frame, _: &screeny_sim::FrameMeta| {
            *count.lock().unwrap() += 1;
        }) as screeny_sim::FrameSink
    };
    let dev = SimDevice::start_with(
        Config {
            faults: Faults {
                decode_ms: 50,
                ..Faults::default()
            },
            timing: slow_lock(),
            ..Config::for_test()
        },
        Some(sink),
    )
    .unwrap();
    let sim = dev.handle();

    let mut tx = Sender::new(dev.frame_addr());
    stream(&mut tx, 60, 3);
    std::thread::sleep(Duration::from_millis(300));

    let t = sim.telemetry();
    assert_eq!(
        *count.lock().unwrap(),
        t.frames_shown,
        "the sink fires once per frames_shown and no more"
    );
    assert!(t.frames_dropped_superseded > 0, "and plenty were dropped");
}
