//! The state machine on a virtual clock.
//!
//! `Core` has no sockets and no wall clock, so these tests can be exact where
//! the loopback tests can only be approximate: `LOCK_MS` to the microsecond,
//! `HOLD_MS` without waiting ten seconds, and source addresses that loopback
//! will not hand out - two senders on *different IPs*, which is the case
//! section 7.4's "from any port on the same IP" is actually about.

mod common;

use std::net::SocketAddr;

use common::*;
use screeny_proto::control::{op, IdleMode, Reply, Request};
use screeny_proto::dec::codec;
use screeny_proto::{ControlPacket, FramePacket, F_FINAL, F_KEY, F_STATS_REQ};
use screeny_sim::{Config, Core, Event, Outbox, ReleaseReason, State, Timing};

const MS: u64 = 1_000;

fn cfg() -> Config {
    Config {
        timing: Timing::SPEC,
        ..Config::for_test()
    }
}

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

/// Build a `FRAME` datagram the way a sender would.
fn frame(codec: u8, flags: u8, seq: u16, payload: &[u8]) -> Vec<u8> {
    let pkt = FramePacket {
        codec,
        flags,
        seq,
        timestamp_us: None,
        payload,
    };
    let mut buf = vec![0u8; pkt.encoded_len()];
    let n = pkt.write(&mut buf).unwrap();
    buf.truncate(n);
    buf
}

/// Build a control request datagram.
fn request(req: &Request<'_>, req_id: u16) -> Vec<u8> {
    let mut buf = vec![0u8; req.encoded_len()];
    let n = req.write(req_id, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

/// Offer one frame and close the drain, as the device thread would.
fn deliver(core: &mut Core, now: u64, from: SocketAddr, data: &[u8], out: &mut Outbox) {
    core.offer_frame(now, from, data, out);
    core.flush_frames(now, out);
}

#[test]
fn the_lock_lapses_exactly_at_lock_ms() {
    let a = addr("10.0.0.1:5000");
    let b = addr("10.0.0.2:6000");
    let (p, _) = solid([1, 2, 3]);
    let lock_us = Timing::SPEC.lock_ms as u64 * MS;

    // One microsecond before LOCK_MS: still A's.
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(core.active_source(), Some(a));
    out.clear();
    deliver(
        &mut core,
        lock_us - 1,
        b,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(core.active_source(), Some(a), "one microsecond too early");
    assert_eq!(core.stats().counters.frames_rejected, 1);
    assert_eq!(out.from_frame_sock.len(), 1, "and B is told it is BUSY");

    // Exactly at LOCK_MS: B's.
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    out.clear();
    deliver(
        &mut core,
        lock_us,
        b,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(core.active_source(), Some(b), "section 7.4 says >=");
    assert_eq!(core.stats().counters.frames_rejected, 0);
    assert!(out.from_frame_sock.is_empty(), "a takeover is not BUSY");
}

#[test]
fn busy_reports_how_long_the_holder_has_left() {
    let a = addr("10.0.0.1:5000");
    let b = addr("10.0.0.2:6000");
    let (p, _) = solid([1, 2, 3]);
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    out.clear();
    deliver(
        &mut core,
        200 * MS,
        b,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    let (_, d) = out.from_frame_sock.first().expect("a BUSY");
    let pkt = ControlPacket::parse(d).unwrap();
    assert_eq!(pkt.op, op::BUSY);
    match Reply::decode(pkt.op, pkt.flags, pkt.body).unwrap() {
        Reply::Busy {
            lock_holder_ms_remaining,
            ..
        } => assert_eq!(
            lock_holder_ms_remaining, 300,
            "LOCK_MS 500 minus the 200 ms since the last accepted frame"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn busy_is_rate_limited_per_source_and_not_globally() {
    let a = addr("10.0.0.1:5000");
    let b = addr("10.0.0.2:6000");
    let c = addr("10.0.0.3:7000");
    let (p, _) = solid([1, 2, 3]);
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    out.clear();

    // B and C are both locked out; each gets its own first BUSY.
    deliver(
        &mut core,
        MS,
        b,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    deliver(
        &mut core,
        MS,
        c,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(out.from_frame_sock.len(), 2);
    out.clear();

    // A keeps streaming, so its lock never lapses.
    deliver(
        &mut core,
        100 * MS,
        a,
        &frame(codec::SOLID, F_KEY, 2, &p),
        &mut out,
    );
    out.clear();

    // B tries again inside BUSY_MIN_INTERVAL_MS: silence.
    deliver(
        &mut core,
        101 * MS,
        b,
        &frame(codec::SOLID, F_KEY, 2, &p),
        &mut out,
    );
    assert!(out.from_frame_sock.is_empty());
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, Event::Busy { sent: false, .. })));
    out.clear();

    // And once the interval is up, one more.
    let after = MS + Timing::SPEC.busy_min_interval_ms as u64 * MS;
    deliver(
        &mut core,
        after - MS,
        a,
        &frame(codec::SOLID, F_KEY, 3, &p),
        &mut out,
    );
    out.clear();
    deliver(
        &mut core,
        after,
        b,
        &frame(codec::SOLID, F_KEY, 3, &p),
        &mut out,
    );
    assert_eq!(out.from_frame_sock.len(), 1);
}

#[test]
fn release_matches_on_the_address_and_not_the_port() {
    // Section 7.4: "the active source sends RELEASE (from any port on the
    // same IP)". A control socket is never the frame stream's port, so an
    // exact 4-tuple match would make RELEASE impossible to use.
    let stream = addr("10.0.0.1:5000");
    let same_ip_other_port = addr("10.0.0.1:41999");
    let other_ip = addr("10.0.0.2:5000");
    let (p, _) = solid([1, 2, 3]);

    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    deliver(
        &mut core,
        0,
        stream,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    out.clear();

    // A stranger's RELEASE is acknowledged and does nothing.
    core.control(MS, other_ip, &request(&Request::Release, 1), &mut out);
    assert_eq!(core.active_source(), Some(stream), "not the holder");
    let (_, d) = out.from_control_sock.first().expect("an ack");
    let pkt = ControlPacket::parse(d).unwrap();
    assert!(
        !pkt.is_error(),
        "an ineffective RELEASE is still not an error"
    );
    out.clear();

    // The holder's own, from a different port, does.
    core.control(
        2 * MS,
        same_ip_other_port,
        &request(&Request::Release, 2),
        &mut out,
    );
    assert_eq!(core.active_source(), None);
    assert_eq!(core.state(), State::Hold);
    assert!(out.events.iter().any(|e| matches!(
        e,
        Event::LockReleased {
            reason: ReleaseReason::Released,
            ..
        }
    )));
}

#[test]
fn hold_becomes_idle_exactly_at_hold_ms() {
    let a = addr("10.0.0.1:5000");
    let (p, _) = solid([1, 2, 3]);
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(core.state(), State::Live);

    // Section 7.3: STREAM_TIMEOUT_MS with no accepted frame -> HOLD.
    let timeout = Timing::SPEC.stream_timeout_ms as u64 * MS;
    core.tick(timeout - 1, &mut out);
    assert_eq!(core.state(), State::Live, "one microsecond too early");
    core.tick(timeout, &mut out);
    assert_eq!(core.state(), State::Hold);
    assert_eq!(core.active_source(), None, "and the lock went with it");

    // HOLD_MS from the moment HOLD was entered, not from the last frame.
    let hold = Timing::SPEC.hold_ms as u64 * MS;
    core.tick(timeout + hold - 1, &mut out);
    assert_eq!(core.state(), State::Hold);
    core.tick(timeout + hold, &mut out);
    assert_eq!(core.state(), State::Idle);
}

#[test]
fn hold_forever_stays_in_hold_for_a_day() {
    let a = addr("10.0.0.1:5000");
    let (p, _) = solid([1, 2, 3]);
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();

    core.control(
        0,
        a,
        &request(&Request::SetIdle(IdleMode::HoldForever), 1),
        &mut out,
    );
    deliver(
        &mut core,
        MS,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    core.tick(86_400 * 1_000 * MS, &mut out);
    assert_eq!(core.state(), State::Hold);
    assert_eq!(core.idle_mode(), IdleMode::HoldForever);
}

#[test]
fn a_final_frame_that_does_not_decode_does_not_release_the_lock() {
    // Section 7.4 releases the lock when a FINAL frame is *displayed*. A
    // FINAL frame whose payload is corrupt was never displayed, so the lock
    // stays until the stream times out - which matters, because otherwise a
    // single damaged packet would hand the panel to anyone.
    let a = addr("10.0.0.1:5000");
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    let (good, _) = solid([1, 2, 3]);

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &good),
        &mut out,
    );
    assert_eq!(core.active_source(), Some(a));

    deliver(
        &mut core,
        MS,
        a,
        &frame(codec::SOLID, F_KEY | F_FINAL, 2, &[1, 2]),
        &mut out,
    );
    assert_eq!(core.active_source(), Some(a), "still A's panel");
    assert_eq!(core.stats().counters.frames_dropped_decode, 1);

    // A FINAL frame that does decode releases it.
    deliver(
        &mut core,
        2 * MS,
        a,
        &frame(codec::SOLID, F_KEY | F_FINAL, 3, &good),
        &mut out,
    );
    assert_eq!(core.active_source(), None);
}

#[test]
fn a_whole_drain_is_counted_then_only_the_survivor_is_shown() {
    // Section 3.3 in one test: five frames arrive between two wakes of the
    // frame task. All five are accepted (frames_rx), four are superseded, and
    // the fifth is what the panel shows.
    let a = addr("10.0.0.1:5000");
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();

    let mut expect = Vec::new();
    for i in 1..=5u8 {
        let (p, e) = solid([i, i, i]);
        core.offer_frame(
            i as u64 * MS,
            a,
            &frame(codec::SOLID, F_KEY, i as u16, &p),
            &mut out,
        );
        expect = e;
    }
    assert_eq!(core.stats().counters.frames_rx, 5);
    assert_eq!(core.stats().counters.frames_shown, 0, "nothing yet");
    core.flush_frames(5 * MS, &mut out);

    let c = core.stats().counters;
    assert_eq!(c.frames_rx, 5);
    assert_eq!(c.frames_shown, 1);
    assert_eq!(c.frames_dropped_superseded, 4);
    assert_eq!(c.frames_dropped_stale, 0);
    assert_eq!(core.shown().unwrap().seq, 5);
    assert_frames_eq(&core.decoded()[..], &expect, "the survivor");
}

#[test]
fn one_telemetry_answers_a_whole_drain_of_stats_requests() {
    // Section 6.4 says a sender must not set STATS_REQ on more than one frame
    // in 100 ms, and section 6.2 rate-limits the reply to the same. A sender
    // that ignores the first rule gets one reply, not five.
    let a = addr("10.0.0.1:5000");
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    let (p, _) = solid([1, 2, 3]);

    for i in 1..=5u16 {
        core.offer_frame(
            i as u64 * MS,
            a,
            &frame(codec::SOLID, F_KEY | F_STATS_REQ, i, &p),
            &mut out,
        );
    }
    core.flush_frames(5 * MS, &mut out);
    assert_eq!(out.from_frame_sock.len(), 1, "one reply for the drain");

    let (to, d) = &out.from_frame_sock[0];
    assert_eq!(*to, a, "section 6.4: to the datagram's source");
    let pkt = ControlPacket::parse(d).unwrap();
    assert_eq!(pkt.op, op::TELEMETRY);
    assert_eq!(pkt.req_id, 0);
    assert!(pkt.is_reply());
    match Reply::decode(pkt.op, pkt.flags, pkt.body).unwrap() {
        Reply::Telemetry(t) => {
            // "After processing this frame": the counters already include the
            // whole drain.
            assert_eq!(t.frames_rx, 5);
            assert_eq!(t.frames_shown, 1);
            assert_eq!(t.frames_dropped_superseded, 4);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_stats_request_is_answered_even_when_the_frame_is_not_shown() {
    // The one frame a second a sender marks is exactly the frame it cannot
    // afford to lose track of: dropping the reply when the frame is stale or
    // undecodable would hide the failure the sender is asking about.
    let a = addr("10.0.0.1:5000");
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    let (p, _) = solid([1, 2, 3]);

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 10, &p),
        &mut out,
    );
    out.clear();

    // Stale, and still answered.
    deliver(
        &mut core,
        200 * MS,
        a,
        &frame(codec::SOLID, F_KEY | F_STATS_REQ, 3, &p),
        &mut out,
    );
    assert_eq!(out.from_frame_sock.len(), 1, "stale frame, telemetry sent");
    assert_eq!(core.stats().counters.frames_dropped_stale, 1);
    out.clear();

    // Undecodable, and still answered - after the rate limit has elapsed.
    deliver(
        &mut core,
        400 * MS,
        a,
        &frame(codec::SOLID, F_KEY | F_STATS_REQ, 11, &[9]),
        &mut out,
    );
    assert_eq!(out.from_frame_sock.len(), 1, "bad payload, telemetry sent");
    assert_eq!(core.stats().counters.frames_dropped_decode, 1);
}

#[test]
fn a_locked_out_sender_gets_busy_and_not_telemetry() {
    // Section 6.2 gives the two unsolicited packets different jobs. A source
    // that is not allowed to drive the panel is told that, rather than being
    // handed the counters of a stream that is not its own.
    let a = addr("10.0.0.1:5000");
    let b = addr("10.0.0.2:5000");
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    let (p, _) = solid([1, 2, 3]);

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    out.clear();
    deliver(
        &mut core,
        MS,
        b,
        &frame(codec::SOLID, F_KEY | F_STATS_REQ, 1, &p),
        &mut out,
    );
    assert_eq!(out.from_frame_sock.len(), 1);
    let (to, d) = &out.from_frame_sock[0];
    assert_eq!(*to, b);
    assert_eq!(ControlPacket::parse(d).unwrap().op, op::BUSY);
}

#[test]
fn reset_stats_clears_the_counters_and_leaves_the_stream_alone() {
    let a = addr("10.0.0.1:5000");
    let mut core = Core::new(&cfg());
    let mut out = Outbox::default();
    let (p, expect) = solid([4, 5, 6]);

    deliver(
        &mut core,
        0,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    deliver(
        &mut core,
        MS,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(core.stats().counters.frames_shown, 1);
    assert_eq!(core.stats().counters.frames_dropped_stale, 1);

    core.control(2 * MS, a, &request(&Request::ResetStats, 1), &mut out);
    let c = core.stats().counters;
    assert_eq!(c.frames_rx, 0);
    assert_eq!(c.frames_shown, 0);
    assert_eq!(c.frames_dropped_stale, 0);

    assert_eq!(core.active_source(), Some(a), "RESET_STATS is not RELEASE");
    assert_eq!(core.state(), State::Live);
    assert_frames_eq(&core.decoded()[..], &expect, "nor does it blank the panel");

    // And last_seq survives, so the next frame is judged the same way.
    deliver(
        &mut core,
        3 * MS,
        a,
        &frame(codec::SOLID, F_KEY, 1, &p),
        &mut out,
    );
    assert_eq!(core.stats().counters.frames_dropped_stale, 1);
}

#[test]
fn uptime_and_telemetry_agree_with_the_clock_they_are_given() {
    let mut core = Core::new(&cfg());
    let t = core.telemetry(1_234_567);
    assert_eq!(t.uptime_ms, 1_234);
    assert_eq!(t.state, screeny_proto::control::state::IDLE);
    assert_eq!(t.brightness, 255);
    assert_eq!(t.last_codec, 0, "nothing has been shown");

    let mut out = Outbox::default();
    core.control(
        2_000_000,
        addr("10.0.0.1:1"),
        &request(&Request::SetBrightness(40), 1),
        &mut out,
    );
    assert_eq!(core.telemetry(2_000_000).brightness, 40);
}
