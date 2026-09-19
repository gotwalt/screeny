//! Spec section 7: source identity, the lock, takeover, and the idle state
//! machine.
//!
//! Most of these run with the timing constants compressed, because waiting
//! out a real `HOLD_MS` is ten seconds a test. One test at the end runs the
//! spec's own numbers, so "we compressed them correctly" is not something the
//! rest of the file has to be trusted about.

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::control::{busy_reason, op, IdleMode, Reply, Request};
use screeny_proto::dec::codec;
use screeny_proto::{F_FINAL, F_KEY};
use screeny_sim::{Config, Event, ReleaseReason, SimDevice, State, Timing};

const T: Duration = Duration::from_secs(3);

/// Compressed timings: the same shape as the spec's, an order of magnitude
/// faster.
fn quick() -> Config {
    Config {
        timing: Timing {
            lock_ms: 100,
            stream_timeout_ms: 200,
            hold_ms: 300,
            fade_ms: 50,
            busy_min_interval_ms: 200,
            telemetry_min_interval_ms: 100,
            info_min_interval_ms: 200,
        },
        ..Config::for_test()
    }
}

fn marker(n: u8) -> (Vec<u8>, Vec<u8>) {
    solid([n, 255 - n, 64])
}

#[test]
fn the_first_frame_adopts_a_source_and_goes_live() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    assert_eq!(sim.snapshot().state, State::Idle);
    assert_eq!(sim.snapshot().active_source, None);

    let mut tx = Sender::new(dev.frame_addr());
    let (p, e) = marker(1);
    tx.send(codec::SOLID, F_KEY, &p);

    let s = sim.wait_for_frames(1, T).unwrap();
    assert_eq!(s.state, State::Live);
    assert_eq!(s.state_byte, screeny_proto::control::state::LIVE);
    assert_eq!(s.active_source, Some(tx.addr()));
    assert_frames_eq(&s.decoded[..], &e, "the first frame");
}

#[test]
fn a_second_source_is_locked_out_and_told_so() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let mut a = Sender::new(dev.frame_addr());
    let mut b = Sender::new(dev.frame_addr());
    let (pa, ea) = marker(1);
    let (pb, _) = marker(2);

    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    // Inside LOCK_MS, B's frame is rejected and B gets one BUSY.
    b.send(codec::SOLID, F_KEY, &pb);
    let reply = b.recv(T).expect("a BUSY packet");
    let (pkt, decoded) = parse_reply(&reply);
    assert_eq!(pkt.op, op::BUSY);
    assert!(pkt.is_reply(), "section 6.2: unsolicited packets set REPLY");
    assert_eq!(pkt.req_id, 0, "section 6.2: and req_id 0");
    match decoded.expect("a BUSY body") {
        Reply::Busy {
            reason,
            lock_holder_ms_remaining,
        } => {
            assert_eq!(reason, busy_reason::LOCKED);
            assert!(
                lock_holder_ms_remaining <= 100,
                "no more than LOCK_MS, got {lock_holder_ms_remaining}"
            );
        }
        other => panic!("expected BUSY, got {other:?}"),
    }

    let s = sim.snapshot();
    assert_frames_eq(&s.decoded[..], &ea, "A still owns the panel");
    assert_eq!(s.active_source, Some(a.addr()));
    assert_eq!(s.telemetry.frames_rejected, 1);
    assert_eq!(s.telemetry.frames_shown, 1);

    // Section 6.2: BUSY is rate-limited to one per BUSY_MIN_INTERVAL_MS per
    // source, so a rejected sender that keeps trying is not answered every
    // time.
    for _ in 0..8 {
        a.send(codec::SOLID, F_KEY, &pa);
        b.send(codec::SOLID, F_KEY, &pb);
        std::thread::sleep(Duration::from_millis(10));
    }
    let busies = b.drain().len();
    assert!(busies <= 1, "expected at most one more BUSY, got {busies}");
    assert!(sim.telemetry().frames_rejected >= 8);
}

#[test]
fn a_new_source_takes_over_once_the_lock_lapses() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let mut a = Sender::new(dev.frame_addr());
    let mut b = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();
    let (pa, _) = marker(1);
    let (pb, eb) = marker(2);

    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    // LOCK_MS is 100 here; wait past it but stay inside a stream timeout so
    // this really is a takeover and not just "the panel was free".
    std::thread::sleep(Duration::from_millis(130));
    b.send(codec::SOLID, F_KEY, &pb);

    let ev = sim
        .wait_for(&mut cursor, T, |e| {
            matches!(
                e,
                Event::LockReleased {
                    reason: ReleaseReason::TakenOver,
                    ..
                }
            )
        })
        .expect("a takeover");
    assert!(matches!(ev, Event::LockReleased { source, .. } if source == a.addr()));

    let s = sim.wait_until(T, |s| s.active_source == Some(b.addr())).unwrap();
    assert_frames_eq(&s.decoded[..], &eb, "B has the panel");
    assert_eq!(s.state, State::Live);
    assert_eq!(s.telemetry.frames_rejected, 0, "B was never locked out");
}

#[test]
fn a_final_frame_releases_the_lock_at_once() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let mut a = Sender::new(dev.frame_addr());
    let mut b = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();
    let (pa, ea) = marker(1);
    let (pb, eb) = marker(2);

    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();
    a.send(codec::SOLID, F_KEY | F_FINAL, &pa);

    let ev = sim
        .wait_for(&mut cursor, T, |e| {
            matches!(
                e,
                Event::LockReleased {
                    reason: ReleaseReason::Final,
                    ..
                }
            )
        })
        .expect("FINAL releases the lock");
    assert!(matches!(ev, Event::LockReleased { source, .. } if source == a.addr()));

    let s = sim.snapshot();
    assert_eq!(s.state, State::Hold, "section 7.3: LIVE + FINAL -> HOLD");
    assert_eq!(s.active_source, None);
    assert_frames_eq(&s.decoded[..], &ea, "the FINAL frame is still lit");

    // "so any sender may take over instantly" - no LOCK_MS wait.
    b.send(codec::SOLID, F_KEY, &pb);
    let s = sim.wait_until(T, |s| s.active_source == Some(b.addr())).unwrap();
    assert_eq!(s.state, State::Live);
    assert_frames_eq(&s.decoded[..], &eb, "B took over instantly");
    assert_eq!(s.telemetry.frames_rejected, 0);
}

#[test]
fn release_from_the_holders_address_gives_up_the_panel() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let mut a = Sender::new(dev.frame_addr());
    let (pa, _) = marker(1);
    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    // Section 7.4: "from any port on the same IP". A control socket is
    // necessarily a different port from the frame stream's, so the match has
    // to be on the address alone - which is what section 6.3's "only if the
    // requester currently holds it" has to mean.
    let ctrl = Ctrl::new(dev.control_addr());
    assert_ne!(ctrl.addr().port(), a.addr().port());
    let reply = ctrl.call(&Request::Release, 7).expect("a reply");
    let (pkt, body) = parse_reply(&reply);
    assert_eq!(pkt.req_id, 7);
    assert!(matches!(body, Ok(Reply::Release)));

    let s = sim.wait_until(T, |s| s.active_source.is_none()).unwrap();
    assert_eq!(s.state, State::Hold);
}

#[test]
fn release_is_idempotent_when_nobody_holds_the_lock() {
    // Section 6.3: RELEASE "clears the source lock only if the requester
    // currently holds it". It is still idempotent, so the answer is an
    // ordinary ack and not an error - there is nothing to retry. The
    // wrong-requester half of this rule needs two source addresses and lives
    // in tests/core_rules.rs, where the state machine can be driven directly.
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());

    assert_eq!(sim.snapshot().active_source, None);
    for id in 1..=3u16 {
        let reply = ctrl.call(&Request::Release, id).expect("a reply");
        let (pkt, body) = parse_reply(&reply);
        assert_eq!(pkt.req_id, id);
        assert!(!pkt.is_error(), "an unnecessary RELEASE is not an error");
        assert!(matches!(body, Ok(Reply::Release)));
    }
    assert_eq!(sim.snapshot().state, State::Idle, "and nothing moved");
}

#[test]
fn a_stream_that_stops_goes_to_hold_and_then_to_the_idle_screen() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let mut a = Sender::new(dev.frame_addr());
    let mut cursor = sim.event_cursor();
    let (pa, ea) = marker(3);

    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    // STREAM_TIMEOUT_MS with no frames: LIVE -> HOLD, lock released.
    let ev = sim
        .wait_for(&mut cursor, T, |e| {
            matches!(
                e,
                Event::LockReleased {
                    reason: ReleaseReason::Timeout,
                    ..
                }
            )
        })
        .expect("the stream timeout");
    assert!(matches!(ev, Event::LockReleased { source, .. } if source == a.addr()));
    let s = sim.snapshot();
    assert_eq!(s.state, State::Hold);
    assert_frames_eq(&s.decoded[..], &ea, "HOLD leaves the last frame lit");

    // HOLD_MS later: the idle screen.
    let s = sim
        .wait_until(T, |s| s.state == State::Idle)
        .expect("HOLD_MS to expire");
    assert_eq!(s.state_byte, screeny_proto::control::state::IDLE);
    assert_frames_eq(
        &s.decoded[..],
        &ea,
        "the decoded frame is kept; only the panel changes",
    );

    // And what the panel is scanning out is no longer that frame.
    std::thread::sleep(Duration::from_millis(80)); // past FADE_MS
    let shown = sim.render_display();
    assert_ne!(&shown[..], &ea[..], "the status screen replaced it");
}

#[test]
fn hold_forever_never_fades() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let reply = ctrl
        .call(&Request::SetIdle(IdleMode::HoldForever), 1)
        .expect("a reply");
    assert!(matches!(parse_reply(&reply).1, Ok(Reply::Idle { mode: 1 })));

    let mut a = Sender::new(dev.frame_addr());
    let (pa, ea) = marker(4);
    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    // Well past STREAM_TIMEOUT_MS + HOLD_MS.
    std::thread::sleep(Duration::from_millis(900));
    let s = sim.snapshot();
    assert_eq!(s.state, State::Hold, "section 7.5 mode 1: stay in HOLD");
    assert_eq!(s.idle_mode, IdleMode::HoldForever);
    assert_frames_eq(&sim.render_display()[..], &s.panel[..], "still the frame");
    assert_eq!(s.active_source, None, "the lock was still released");
    assert_frames_eq(&s.decoded[..], &ea, "and the frame is untouched");
}

#[test]
fn identify_is_an_overlay_and_not_a_state() {
    // Section 7.3: "IDENTIFY and PROVISIONING are display overlays, not
    // stream states: frame handling continues underneath and the state byte
    // in telemetry reports the overlay."
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let mut a = Sender::new(dev.frame_addr());
    let ctrl = Ctrl::new(dev.control_addr());
    let (pa, ea) = marker(5);

    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    ctrl.call(&Request::Identify { duration_ms: 400 }, 3)
        .expect("a reply");
    let s = sim
        .wait_until(T, |s| {
            s.state_byte == screeny_proto::control::state::IDENTIFY
        })
        .expect("the overlay");
    assert_eq!(s.state, State::Live, "the stream state is untouched");

    // Frame handling continues underneath.
    let (pb, eb) = marker(6);
    a.send(codec::SOLID, F_KEY, &pb);
    let s = sim.wait_for_frames(2, T).unwrap();
    assert_ne!(&eb[..], &ea[..]);
    assert_frames_eq(&s.decoded[..], &eb, "frames still land");
    assert_ne!(
        &sim.render_display()[..],
        &s.panel[..],
        "but the overlay is what is lit"
    );

    // And it expires on its own, back to whatever the stream state has
    // become in the meantime.
    let s = sim
        .wait_until(T, |s| {
            s.state_byte != screeny_proto::control::state::IDENTIFY
        })
        .expect("the overlay to expire");
    assert_eq!(s.state_byte, s.state.as_u8());
    assert_frames_eq(&s.decoded[..], &eb, "the frame underneath survived");
}

#[test]
fn identify_zero_stops_one_in_progress() {
    let dev = SimDevice::start(quick()).unwrap();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());

    ctrl.call(&Request::Identify { duration_ms: 5_000 }, 1).unwrap();
    sim.wait_until(T, |s| {
        s.state_byte == screeny_proto::control::state::IDENTIFY
    })
    .expect("the overlay");
    ctrl.call(&Request::Identify { duration_ms: 0 }, 2).unwrap();
    sim.wait_until(T, |s| {
        s.state_byte != screeny_proto::control::state::IDENTIFY
    })
    .expect("0 stops it");
}

#[test]
fn the_spec_s_own_constants_behave_the_same_way() {
    // Everything above compresses the timings. This one uses LOCK_MS = 500
    // and STREAM_TIMEOUT_MS = 1000 as written, so the compression is not
    // load-bearing. HOLD_MS is ten seconds and is not exercised here.
    let dev = SimDevice::start(Config::for_test()).unwrap();
    let sim = dev.handle();
    assert_eq!(dev.handle().snapshot().state, State::Idle);

    let mut a = Sender::new(dev.frame_addr());
    let mut b = Sender::new(dev.frame_addr());
    let (pa, ea) = marker(1);
    let (pb, eb) = marker(2);

    a.send(codec::SOLID, F_KEY, &pa);
    sim.wait_for_frames(1, T).unwrap();

    // 300 ms in, still well inside LOCK_MS.
    std::thread::sleep(Duration::from_millis(300));
    b.send(codec::SOLID, F_KEY, &pb);
    std::thread::sleep(Duration::from_millis(50));
    assert_frames_eq(&sim.snapshot().decoded[..], &ea, "A keeps the panel");
    assert_eq!(sim.telemetry().frames_rejected, 1);

    // 300 ms more is past LOCK_MS but inside STREAM_TIMEOUT_MS.
    std::thread::sleep(Duration::from_millis(300));
    b.send(codec::SOLID, F_KEY, &pb);
    let s = sim
        .wait_until(T, |s| s.active_source == Some(b.addr()))
        .expect("takeover at LOCK_MS");
    assert_frames_eq(&s.decoded[..], &eb, "B has it now");
    assert_eq!(s.state, State::Live);

    // And a second of silence drops to HOLD.
    let s = sim
        .wait_until(Duration::from_secs(5), |s| s.state == State::Hold)
        .expect("STREAM_TIMEOUT_MS");
    assert_eq!(s.active_source, None);
}
