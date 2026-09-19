//! The embedding API: pushing frames, the cadence ceiling, and a link that
//! survives the panel going away.
//!
//! Everything here runs against `screeny_sim::SimDevice` on loopback with
//! ephemeral ports and mDNS off. "The device rebooted" is modelled by
//! dropping the `SimDevice` - both sockets close - and starting a new one,
//! either on the ports the old one had (a reboot) or on fresh ones with a new
//! identity (DHCP moved it).
//!
//! Every test here is bounded by `PATIENCE` (2 s) or by an explicit short
//! deadline. Nothing sleeps for a wall-clock timeout it did not choose.

mod simfix;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use screeny::{Cadence, Link, LinkConfig, LinkState, Pixels, SenderConfig, Sent, Target};
use screeny_proto::NPIX;
use screeny_sim::{SimDevice, SimHandle};
use simfix::{device_for, sim_config, PATIENCE};

const PAL: [[u8; 3]; 4] = [[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]];

fn indices(k: usize) -> Vec<u8> {
    (0..NPIX).map(|p| ((p + k) % 4) as u8).collect()
}

/// A `LinkConfig` that reconnects quickly and gives up on silence quickly, so
/// a reconnection test takes a second rather than fifteen.
fn brisk() -> LinkConfig {
    LinkConfig {
        sender: SenderConfig {
            // `STATS_REQ` on nearly every frame, so the watchdog has fresh
            // evidence to work from.
            stats_interval: Duration::from_millis(100),
            ..SenderConfig::default()
        },
        silence: Duration::from_millis(400),
        backoff: screeny::Backoff {
            first: Duration::from_millis(20),
            max: Duration::from_millis(100),
            factor: 2.0,
        },
        ..LinkConfig::default()
    }
}

/// A target that names an address, so `resolve` is instant and no test ever
/// touches mDNS. Reconnection re-runs exactly this.
fn target_at(frame: SocketAddr) -> Target {
    Target {
        addr: Some(frame),
        timeout: Some(Duration::from_millis(200)),
        ..Target::default()
    }
}

/// Start a simulator on the given ports whose control port is frame + 1, so a
/// bare `Target { addr }` finds both halves - which is what an embedder will
/// actually have on the bench.
fn sim_pair(frame_port: u16) -> (SimDevice, SimHandle) {
    let dev = SimDevice::start(sim_config(frame_port, frame_port + 1)).expect("sim starts");
    let sim = dev.handle();
    (dev, sim)
}

/// A simulator on *some* free port pair, retrying if another test in this
/// process grabbed the pair between choosing it and binding it.
fn sim_anywhere() -> (SimDevice, SimHandle, u16) {
    for _ in 0..20 {
        let p = free_port_pair();
        if let Ok(dev) = SimDevice::start(sim_config(p, p + 1)) {
            let sim = dev.handle();
            return (dev, sim, p);
        }
    }
    panic!("could not find a free frame/control port pair on loopback");
}

/// Bind a throwaway socket to find a free port, then release it. Racy in
/// principle; in a test process on loopback, fine, and the alternative is
/// asking the simulator for an ephemeral pair that is not frame/frame+1.
fn free_port_pair() -> u16 {
    use std::net::UdpSocket;
    loop {
        let a = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let p = a.local_addr().expect("addr").port();
        drop(a);
        // The control port must be free too.
        if UdpSocket::bind(("127.0.0.1", p + 1)).is_ok() && p < u16::MAX - 1 {
            return p;
        }
    }
}

fn push(link: &mut Link, k: usize) -> Sent {
    let idx = indices(k);
    link.send(Pixels::indexed(&PAL, &idx)).expect("well formed")
}

/// Push frames until `pred` holds of the link, or the deadline passes.
fn push_until(link: &mut Link, timeout: Duration, pred: impl Fn(&Link) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    let mut k = 0usize;
    while Instant::now() < deadline {
        if pred(link) {
            return true;
        }
        push(link, k);
        k += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    pred(link)
}

// ---------------------------------------------------------------------------
// The push model
// ---------------------------------------------------------------------------

#[test]
fn a_link_opens_sends_and_reports_what_it_did() {
    let (dev, sim, _port) = sim_anywhere();
    let mut link = Link::open(target_at(dev.frame_addr()), brisk()).expect("opens");

    assert_eq!(link.state(), LinkState::Up);
    assert_eq!(link.device().map(|d| d.frame), Some(dev.frame_addr()));

    let sent = push(&mut link, 0);
    assert!(sent.is_sent() && sent.exact(), "{sent:?}");

    let shot = sim.wait_for_frames(1, PATIENCE).expect("displayed");
    let expected = simfix::expand(&PAL, &indices(0));
    assert_eq!(&shot.decoded[..], expected.as_slice());

    assert_eq!(link.stats().frames_sent, 1);
    assert_eq!(link.stats().indexed_exact, 1);
    assert_eq!(link.stats().sessions, 1);
    drop(link);
    drop(dev);
}

/// What the device allows has to be visible, because a producer that cannot
/// see the budget cannot choose a palette against it.
#[test]
fn the_devices_limits_are_visible_to_the_embedder() {
    let (dev, _sim, _port) = sim_anywhere();
    let link = Link::open(target_at(dev.frame_addr()), brisk()).expect("opens");
    let lim = link.limits();
    assert!(lim.connected);
    assert_eq!(lim.panel, Some((64, 32)));
    assert_eq!(lim.exact_palette, 32, "PAL5 guarantees 32 at this budget");
    assert!(lim.budget >= screeny::encode::MIN_BUDGET);
    assert!(lim.codecs.contains(&screeny_proto::dec::codec::PAL5));
    assert!(!lim.codec_limited);
    assert_eq!(lim.fps, 30.0);
    assert_eq!(lim.configured_fps, 30.0);
    drop(link);
    drop(dev);
}

/// The cadence ceiling: a producer rendering at 60 fps into a 30 fps panel
/// must have about half its frames coalesced, and the device must see no
/// superseded frames at all.
#[test]
fn a_sixty_fps_producer_is_decimated_to_the_devices_rate() {
    let (dev, sim, _port) = sim_anywhere();
    let mut link = Link::open(target_at(dev.frame_addr()), brisk()).expect("opens");

    let start = Instant::now();
    let mut k = 0usize;
    // One second of a 60 fps loop.
    while start.elapsed() < Duration::from_millis(1000) {
        push(&mut link, k);
        k += 1;
        screeny::sender::sleep_until(start + Duration::from_micros(16_667 * k as u64));
    }

    let s = link.stats();
    assert!(k >= 40, "the producer only managed {k} frames");
    assert_eq!(s.frames_offered, k as u64);
    assert!(
        (20..=40).contains(&s.frames_sent),
        "sent {} of {k} offered; expected about the device's 30 fps",
        s.frames_sent
    );
    assert!(s.frames_coalesced > 0);
    assert_eq!(s.frames_sent + s.frames_coalesced, s.frames_offered);
    assert_eq!(
        sim.telemetry().frames_dropped_superseded,
        0,
        "the whole point of the ceiling is that the device is never overrun"
    );
    drop(link);
    drop(dev);
}

/// `Cadence::Free` hands pacing entirely back to the caller, which is what an
/// embedder that does its own scheduling wants.
#[test]
fn free_cadence_sends_everything() {
    let (dev, _sim, _port) = sim_anywhere();
    let cfg = LinkConfig {
        cadence: Cadence::Free,
        ..brisk()
    };
    let mut link = Link::open(target_at(dev.frame_addr()), cfg).expect("opens");
    for k in 0..20 {
        assert!(push(&mut link, k).is_sent());
    }
    assert_eq!(link.stats().frames_sent, 20);
    assert_eq!(link.stats().frames_coalesced, 0);
    drop(link);
    drop(dev);
}

/// Dropping the link must release the device's source lock at once rather
/// than leaving it to time out (spec 7.4, 9.4 step 5).
#[test]
fn dropping_the_link_sends_final() {
    let (dev, sim, _port) = sim_anywhere();
    let mut link = Link::open(target_at(dev.frame_addr()), brisk()).expect("opens");
    push(&mut link, 0);
    sim.wait_for_frames(1, PATIENCE).expect("displayed");
    assert!(sim.snapshot().active_source.is_some());

    drop(link);

    let released = sim
        .wait_until(PATIENCE, |s| s.active_source.is_none())
        .is_some();
    assert!(released, "the source lock should be released by FINAL");
    drop(dev);
}

// ---------------------------------------------------------------------------
// Reconnection
// ---------------------------------------------------------------------------

/// The panel reboots: same address, same ports, new everything else. The link
/// must notice by itself, reconnect, and go on accepting frames throughout.
#[test]
fn a_link_survives_the_device_rebooting_on_the_same_port() {
    let (dev, sim, port) = sim_anywhere();
    let addr = dev.frame_addr();
    let mut link = Link::open(target_at(addr), brisk()).expect("opens");

    assert!(push_until(&mut link, PATIENCE, |l| l.stats().frames_sent >= 5));
    sim.wait_for_frames(1, PATIENCE).expect("displayed");

    // The panel goes away. Both sockets close with it.
    drop(sim);
    drop(dev);

    // Frames keep being accepted while there is nowhere to send them: this is
    // the contract that keeps the embedder free of error handling.
    let went_down = push_until(&mut link, PATIENCE, |l| !l.state().is_up());
    assert!(went_down, "the link never noticed the device had gone");
    let dropped_before = link.stats().frames_dropped;
    assert!(dropped_before > 0);

    // It comes back on the same ports.
    let (dev2, sim2) = sim_pair(port);
    let back = push_until(&mut link, PATIENCE, |l| l.stats().sessions >= 2);
    assert!(
        back,
        "never reconnected: state {:?}, last error {:?}",
        link.state(),
        link.stats().last_error
    );
    assert!(push_until(&mut link, PATIENCE, |l| l
        .session()
        .is_some_and(|s| s.frames_sent > 0)));

    let shot = sim2.wait_for_frames(1, PATIENCE).expect("the new sim draws");
    // Which phase of the test pattern landed depends on how many frames the
    // reconnect took; that it is one of the palette's own colours, exactly,
    // is the part that matters.
    assert!(
        PAL.contains(&[shot.decoded[0], shot.decoded[1], shot.decoded[2]]),
        "the new session's first frame is not palette-exact: {:?}",
        &shot.decoded[..3]
    );
    assert!(link.stats().drops >= 1);
    // Not one frame was ever an error to the caller.
    assert_eq!(
        link.stats().frames_offered,
        link.stats().frames_sent + link.stats().frames_coalesced + link.stats().frames_dropped
    );
    drop(link);
    drop(dev2);
}

/// DHCP moved the panel: it comes back on different ports, under a different
/// name. `Link::retarget` moves an existing link to it mid-stream, keeping
/// the lifetime statistics and never making the caller handle an error.
///
/// (A target that names an *instance* rather than an address needs no
/// retargeting: every reconnect re-browses `_screeny._udp` and picks up the
/// new address. That path cannot be exercised here, because these tests run
/// with mDNS off by rule.)
#[test]
fn a_link_follows_the_device_to_a_new_address() {
    let (dev, sim, port) = sim_anywhere();

    let mut link = Link::open_deferred(target_at(dev.frame_addr()), brisk());
    assert!(push_until(&mut link, PATIENCE, |l| l.state().is_up()));
    assert!(push_until(&mut link, PATIENCE, |l| l.stats().frames_sent >= 3));
    sim.wait_for_frames(1, PATIENCE).expect("displayed");
    let sent_before = link.stats().frames_sent;

    drop(sim);
    drop(dev);
    assert!(push_until(&mut link, PATIENCE, |l| !l.state().is_up()));

    // It comes back somewhere else entirely: new ports, new sockets, new
    // source identity as far as the device is concerned (spec 3.2, 7.1).
    let (dev2, sim2, port2) = sim_anywhere();
    assert_ne!(port2, port);
    link.retarget(target_at(dev2.frame_addr()));

    assert!(
        push_until(&mut link, PATIENCE, |l| l.stats().sessions >= 2),
        "never reconnected at the new address: {:?}",
        link.stats().last_error
    );
    assert!(push_until(&mut link, PATIENCE, |l| l
        .session()
        .is_some_and(|s| s.frames_sent > 0)));
    let shot = sim2.wait_for_frames(1, PATIENCE).expect("the new sim draws");
    assert!(
        PAL.contains(&[shot.decoded[0], shot.decoded[1], shot.decoded[2]]),
        "still palette-exact after the move"
    );

    assert_eq!(link.device().map(|d| d.frame), Some(dev2.frame_addr()));
    assert!(link.stats().frames_sent > sent_before, "totals carried over");
    assert_eq!(
        link.stats().frames_offered,
        link.stats().frames_sent + link.stats().frames_coalesced + link.stats().frames_dropped
    );
    drop(link);
    drop(dev2);
}

/// The watchdog is the only thing that can notice a device that stopped
/// answering without the socket erroring - which on a real LAN is the normal
/// case, because UDP to a dead host just succeeds.
#[test]
fn silence_alone_brings_the_link_down() {
    let (dev, _sim, _port) = sim_anywhere();
    let mut link = Link::open(target_at(dev.frame_addr()), brisk()).expect("opens");
    assert!(push_until(&mut link, PATIENCE, |l| l.stats().frames_sent >= 5));

    // Take the device away without anything else changing.
    drop(dev);
    let t0 = Instant::now();
    assert!(push_until(&mut link, PATIENCE, |l| !l.state().is_up()));
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "took {:?} to notice 400 ms of silence",
        t0.elapsed()
    );
    let why = link.stats().last_error.clone().unwrap_or_default();
    assert!(
        why.contains("telemetry") || why.contains("send failed"),
        "unhelpful reason: {why:?}"
    );
}

/// With reconnection off, a link that goes down stays down and says so, which
/// is what a one-shot tool wants.
#[test]
fn reconnection_can_be_turned_off() {
    let (dev, _sim, port) = sim_anywhere();
    let cfg = LinkConfig {
        reconnect: false,
        ..brisk()
    };
    let mut link = Link::open(target_at(dev.frame_addr()), cfg).expect("opens");
    assert!(push_until(&mut link, PATIENCE, |l| l.stats().frames_sent >= 3));
    drop(dev);
    assert!(push_until(&mut link, PATIENCE, |l| l.state()
        == LinkState::Closed));

    // The panel comes back on the same ports. A reconnecting link would pick
    // it up within a backoff or two; this one must not.
    let (dev2, _sim2) = sim_pair(port);
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        assert!(matches!(push(&mut link, 0), Sent::Dropped));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(link.stats().sessions, 1);
    drop(link);
    drop(dev2);
}

/// A link opened before the panel exists must connect when it appears, with
/// no special case in the caller and nothing thrown away but frames.
#[test]
fn a_deferred_link_connects_when_the_device_appears() {
    let port = free_port_pair();
    let mut link = Link::open_deferred(target_at(SocketAddr::from(([127, 0, 0, 1], port))), brisk());
    assert!(!link.state().is_up());
    assert!(matches!(push(&mut link, 0), Sent::Dropped));

    let (dev, sim) = sim_pair(port);
    assert!(
        push_until(&mut link, PATIENCE, |l| l.state().is_up()),
        "never connected: {:?}",
        link.stats().last_error
    );
    assert!(push_until(&mut link, PATIENCE, |l| l.stats().frames_sent > 0));
    sim.wait_for_frames(1, PATIENCE).expect("displayed");
    assert_eq!(link.stats().sessions, 1);
    drop(link);
    drop(dev);
}

/// Reconnecting must not block the caller's loop: the resolve and handshake
/// happen on a background thread, and `send` stays fast even while the target
/// is a black hole.
#[test]
fn sending_into_a_dead_link_stays_fast() {
    // 203.0.113.0/24 is TEST-NET-3 (RFC 5737): reserved for documentation, so
    // nothing on this bench answers and nothing is disturbed by trying.
    let cfg = LinkConfig {
        sender: SenderConfig {
            handshake: true,
            ..brisk().sender
        },
        ..brisk()
    };
    let mut link = Link::open_deferred(target_at("203.0.113.9:49374".parse().unwrap()), cfg);
    let t0 = Instant::now();
    for k in 0..200 {
        assert!(matches!(push(&mut link, k), Sent::Dropped));
    }
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "200 sends into a dead link took {:?}; the connect must be off-thread",
        t0.elapsed()
    );
    assert_eq!(link.stats().frames_dropped, 200);
    assert!(!link.state().is_up());
}

#[test]
fn a_malformed_frame_is_still_the_callers_error_even_when_the_link_is_down() {
    let mut link = Link::open_deferred(target_at("203.0.113.9:49374".parse().unwrap()), brisk());
    let err = link.send(Pixels::rgb(&[0u8; 10])).unwrap_err();
    assert!(matches!(err, screeny::Error::Frame { got: 10, .. }), "{err:?}");
    // And it is not counted as a frame that went anywhere.
    assert_eq!(link.stats().frames_sent, 0);
    assert_eq!(link.stats().frames_offered, 0);
}

/// An out-of-range index is only caught inside the encoder, after the frame
/// has been counted as offered. It must still not break the accounting - the
/// invariant is about frames, and that was never one.
#[test]
fn a_bad_index_does_not_break_the_accounting() {
    let (dev, _sim, _port) = sim_anywhere();
    let mut link = Link::open(target_at(dev.frame_addr()), brisk()).expect("opens");
    push(&mut link, 0);

    let mut idx = indices(0);
    idx[7] = 99;
    let err = link.send(Pixels::indexed(&PAL, &idx)).unwrap_err();
    assert!(
        matches!(
            err,
            screeny::Error::BadIndex {
                index: 99,
                pixel: 7,
                palette: 4
            }
        ),
        "{err:?}"
    );
    let s = link.stats();
    assert_eq!(
        s.frames_offered,
        s.frames_sent + s.frames_coalesced + s.frames_dropped
    );
    assert_eq!(s.frames_offered, 1);
    drop(link);
    drop(dev);
}

/// The fixture's own promise: the `Device` a test builds points at the
/// simulator's real ephemeral ports, so nothing here can accidentally reach
/// the bench panel on 49374/49375.
#[test]
fn the_fixture_never_points_at_the_default_ports() {
    let dev = SimDevice::start(sim_config(0, 0)).expect("sim starts");
    let d = device_for(dev.frame_addr(), dev.control_addr());
    assert!(d.frame.ip().is_loopback());
    assert_ne!(d.frame.port(), screeny_proto::DEFAULT_FRAME_PORT);
    assert_ne!(d.control.port(), screeny_proto::DEFAULT_CONTROL_PORT);
}
