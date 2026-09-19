//! The pacer (spec 9.1): absolute scheduling, and skipping rather than
//! bursting when something runs long.
//!
//! These tests are wall-clock measurements, so they take the lock below and
//! run one at a time - two 30 fps streams racing each other on a loaded
//! machine would measure the machine, not the pacer. They are also
//! deliberately cheap on the encoder: a near-solid frame takes the `SOLID`
//! shortcut, so what is being timed is the schedule and not the chooser.
//! `benches/encode` is where encode cost is measured.
//!
//! Set `SCREENY_PACING_SECS` to shorten the long run when iterating.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Receiver, RxConfig};
use screeny::sender::{period_of, sleep_until};
use screeny::{Frame, FrameTime, Sender, SenderConfig};

/// Serialises the wall-clock tests in this file.
static CLOCK: Mutex<()> = Mutex::new(());

/// What counts as a burst: two sends closer together than this fraction of
/// the frame period.
///
/// A real catch-up burst puts frames microseconds apart - under a percent of
/// a period - so this only has to sit clear of ordinary scheduler jitter,
/// which on a loaded laptop moves a send by a few milliseconds either way.
const BURST_FRACTION: f64 = 0.4;

fn secs(default: f64) -> f64 {
    std::env::var("SCREENY_PACING_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn rx() -> Receiver {
    Receiver::start(RxConfig {
        keep_pixels: false,
        ..RxConfig::default()
    })
}

fn stream_cfg(fps: f64) -> SenderConfig {
    SenderConfig {
        fps,
        // No adaptation: this is a measurement of the schedule, and a rate
        // change mid-run would be measuring something else.
        adapt: false,
        stats_interval: Duration::from_secs(5),
        ..SenderConfig::default()
    }
}

/// A source that changes one pixel per frame, so every frame is distinct but
/// the encoder's `SOLID` shortcut still applies.
fn cheap_source(limit: Option<u64>) -> impl screeny::FrameSource {
    screeny::FnSource::new("cheap", move |t: FrameTime, out: &mut Frame| {
        let v = (t.index % 200) as u8;
        for p in 0..screeny::proto::NPIX {
            out.set_at(p, [v, 40, 90]);
        }
        limit.is_none_or(|n| t.index < n)
    })
}

#[test]
fn holds_thirty_fps_within_one_percent() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let secs = secs(10.0);
    let rx = rx();
    let mut sender = Sender::connect(rx.device(), stream_cfg(30.0)).expect("connect");

    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        let d = Duration::from_secs_f64(secs);
        std::thread::spawn(move || {
            std::thread::sleep(d);
            s.store(true, Ordering::SeqCst);
        });
    }
    let mut src = cheap_source(None);
    sender.run(&mut src, &stop).expect("run");

    let sent = sender.stats().frames_sent;
    let achieved = sender.stats().actual_fps();
    let skipped = sender.stats().frames_skipped;
    let min_gap = sender.stats().min_gap.unwrap();
    drop(sender);

    let state = rx.shutdown();
    let arrived = state.fps();

    assert!(
        (29.7..=30.3).contains(&achieved),
        "sender managed {achieved:.3} fps over {secs} s ({sent} frames), outside 30.0 +-1%"
    );
    assert!(
        (29.7..=30.3).contains(&arrived),
        "receiver saw {arrived:.3} fps, outside 30.0 +-1%"
    );
    assert_eq!(skipped, 0, "nothing should have been skipped on an idle run");
    // No two frames close together: the schedule is
    // absolute, so error must not accumulate and then be paid off in a burst.
    assert!(
        min_gap >= period_of(30.0).mul_f64(BURST_FRACTION),
        "smallest gap was {min_gap:?}, a burst"
    );

    // And the absolute schedule must not drift: the last arrival should sit
    // within a couple of periods of where it was due.
    let n = state.frames.len();
    let span = state.frames[n - 1].at.duration_since(state.frames[0].at);
    let due = period_of(30.0).mul_f64((n - 1) as f64);
    let drift = span.as_secs_f64() - due.as_secs_f64();
    assert!(
        drift.abs() < 2.0 * period_of(30.0).as_secs_f64(),
        "drifted {:.1} ms over {n} frames",
        drift * 1000.0
    );
}

/// After a stall the sender must skip the frames it missed and pick the
/// schedule back up, never send a burst to catch up (spec 9.1: the device
/// shows newest-wins and its queues are shallow, so a burst only adds
/// latency).
#[test]
fn skips_rather_than_bursting_after_a_stall() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let rx = rx();
    let mut sender = Sender::connect(rx.device(), stream_cfg(30.0)).expect("connect");
    let stop = AtomicBool::new(false);

    let stall_at = 20u64;
    let stall = Duration::from_millis(300); // nine frame periods
    let mut src = screeny::FnSource::new("stalling", move |t: FrameTime, out: &mut Frame| {
        if t.index == stall_at {
            std::thread::sleep(stall);
        }
        for p in 0..screeny::proto::NPIX {
            out.set_at(p, [(t.index % 200) as u8, 40, 90]);
        }
        t.index < 90
    });
    sender.run(&mut src, &stop).expect("run");

    let skipped = sender.stats().frames_skipped;
    let min_gap = sender.stats().min_gap.unwrap();
    let sent = sender.stats().frames_sent;
    drop(sender);
    let state = rx.shutdown();

    assert!(
        skipped >= 7,
        "a 300 ms stall should have skipped about nine frames, not {skipped}"
    );
    assert!(
        min_gap >= period_of(30.0).mul_f64(BURST_FRACTION),
        "smallest gap after the stall was {min_gap:?}: that is a catch-up burst"
    );
    // The stall shows up as one long gap and nothing else.
    let gaps = state.gaps();
    let long = gaps
        .iter()
        .filter(|g| **g > period_of(30.0).mul_f64(1.5))
        .count();
    assert!(long <= 1, "{long} long gaps, expected just the stall");
    assert!(sent >= 60, "only {sent} frames sent");

    // Sequence numbers count frames *sent*, so they stay contiguous even
    // though wall-clock frames were skipped (spec 3.2).
    for (i, f) in state.frames.iter().enumerate() {
        assert_eq!(f.seq, i as u16);
    }
}

/// The pacer is used at rates other than 30, and the same guarantees hold.
#[test]
fn other_frame_rates_are_paced_too() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for fps in [10.0f64, 24.0, 60.0] {
        let rx = rx();
        let mut sender = Sender::connect(rx.device(), stream_cfg(fps)).expect("connect");
        let stop = AtomicBool::new(false);
        let n = (fps * 1.5) as u64;
        let mut src = cheap_source(Some(n));
        sender.run(&mut src, &stop).expect("run");
        let min_gap = sender.stats().min_gap.unwrap();
        drop(sender);
        // Measured at the receiver over the intervals between arrivals: over
        // a run this short, "frames divided by elapsed" is biased high by the
        // frame at t=0.
        let achieved = rx.shutdown().fps();
        assert!(
            (achieved - fps).abs() < fps * 0.03,
            "asked for {fps}, got {achieved:.2}"
        );
        assert!(min_gap >= period_of(fps).mul_f64(BURST_FRACTION), "burst at {fps} fps");
    }
}

/// `sleep_until` is the piece that makes the rest possible: sleep for all but
/// the last millisecond, then spin. It must never return early.
#[test]
fn sleep_until_does_not_return_early() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for ms in [0u64, 1, 5, 33] {
        for _ in 0..20 {
            let target = Instant::now() + Duration::from_millis(ms);
            sleep_until(target);
            let now = Instant::now();
            assert!(now >= target, "returned {:?} early", target - now);
            // And not absurdly late. The bound is generous because
            // `thread::sleep` is at the scheduler's mercy and this runs
            // alongside whatever else the machine is doing; the point of the
            // final spin is that the *common* case lands within microseconds,
            // which the fps tests above measure directly.
            assert!(
                now.duration_since(target) < Duration::from_millis(25),
                "overslept {ms} ms by {:?}",
                now.duration_since(target)
            );
        }
    }
    // A target in the past returns at once.
    let t0 = Instant::now();
    sleep_until(t0 - Duration::from_secs(1));
    assert!(t0.elapsed() < Duration::from_millis(2));
}
