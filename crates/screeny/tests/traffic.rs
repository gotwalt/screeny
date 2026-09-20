//! Card 164: what a stream costs the network, counted where it happens.
//!
//! The counters are additive - nothing about the wire or about the sender's
//! behaviour changes - so what has to be shown is only that they are *right*:
//! every datagram counted once, in the direction it went, with the header in
//! the byte figure and the IP/UDP overhead deliberately left out of it.
//!
//! Over loopback against the in-process receiver in `tests/common`. Nothing
//! here touches the bench device or the LAN.

mod common;

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use common::{Receiver, RxConfig};
use screeny::{FrameTime, LinkConfig, Pattern, Pixels, Sender, SenderConfig, UDP_OVERHEAD};

fn cfg(fps: f64) -> SenderConfig {
    SenderConfig {
        fps,
        stats_interval: Duration::from_millis(100),
        ..SenderConfig::default()
    }
}

/// The frame port, both ways.
///
/// **Bytes are the whole datagram payload**, which is the 8-byte frame header
/// plus the pixels `SendStats::bytes` counts - not the pixels alone, because
/// the question this answers is what the network carried. Packets are
/// datagrams, `FINAL` included, which is the same rule `bytes` and
/// `frames_sent` already follow (card 153).
#[test]
fn a_stream_counts_every_datagram_it_sent_and_every_one_that_came_back() {
    let rx = Receiver::start(RxConfig::default());
    let mut sender = Sender::connect(rx.device(), cfg(60.0)).expect("connect");
    let stop = AtomicBool::new(false);

    let mut n = 0u64;
    let mut src = screeny::FnSource::new("bars", move |t: FrameTime, out: &mut screeny::Frame| {
        Pattern::Bars.render_at(t.index, out);
        n += 1;
        n <= 30
    });
    sender.run(&mut src, &stop).expect("run");

    let s = sender.stats().clone();
    drop(sender);
    let state = rx.shutdown();

    // One packet per datagram, and the receiver saw exactly those.
    assert_eq!(s.frames.out.packets, s.frames_sent, "one datagram per frame sent");
    assert_eq!(state.frames.len() as u64, s.frames.out.packets, "the receiver saw every one");

    // The header, and only the header, is the difference from the pixel count.
    // 8 bytes: this config sets no timestamps, so there is no extension.
    assert_eq!(
        s.frames.out.bytes,
        s.bytes + 8 * s.frames_sent,
        "the datagram is the pixels plus an 8-byte header"
    );

    // The IP and UDP headers are *not* in the counter - they are the caller's
    // to add, because they are the one part of this user space cannot see.
    assert_eq!(
        s.frames.out.on_the_wire(UDP_OVERHEAD),
        s.frames.out.bytes + UDP_OVERHEAD * s.frames.out.packets
    );

    // Telemetry came back on the frame port (spec 6.4) and is counted as
    // received: a small number of small packets against a large number of
    // large ones, which is the shape the Panel screen is meant to show.
    assert!(state.stats_requests > 0, "the stream asked for telemetry");
    assert!(s.frames.inbound.packets > 0, "and the replies are counted");
    assert!(
        s.frames.inbound.bytes < s.frames.out.bytes / 10,
        "in {} B over {} packets against out {} B: the panel talks back very little",
        s.frames.inbound.bytes,
        s.frames.inbound.packets,
        s.frames.out.bytes
    );

    // The handshake is on the *control* port and is counted separately: one
    // GET_INFO out, one reply in, once per session.
    assert_eq!(s.control.out.packets, 1, "one GET_INFO");
    assert_eq!(s.control.inbound.packets, 1, "and one reply");
    assert!(s.control.out.bytes > 0 && s.control.inbound.bytes > 0);
}

/// A sender told not to shake hands spends nothing on the control port.
#[test]
fn without_a_handshake_the_control_port_costs_nothing() {
    let rx = Receiver::start(RxConfig::default());
    let sender = Sender::connect(
        rx.device(),
        SenderConfig { handshake: false, ..cfg(30.0) },
    )
    .expect("connect");
    assert_eq!(sender.stats().control, screeny::Traffic::default());
    drop(sender);
    rx.shutdown();
}

/// **The property the studio depends on: the totals only grow.**
///
/// A `Link` rebuilds its socket whenever the panel reboots or the studio
/// re-aims it, and a `Sender`'s own counters go with that socket. The link
/// banks the difference instead of copying, so a reconnect adds to the figure
/// rather than resetting it.
#[test]
fn a_link_keeps_its_totals_across_a_reconnect() {
    let rx = Receiver::start(RxConfig::default());
    let mut link = screeny::Link::attach(rx.device(), LinkConfig::default()).expect("attach");

    let frame = vec![0u8; screeny::proto::NPIX * 3];
    for _ in 0..10 {
        link.send(Pixels::rgb(&frame)).expect("send");
        std::thread::sleep(Duration::from_millis(20));
    }
    let first = link.stats().traffic;
    assert!(first.frames.out.packets >= 5, "{first:?}");
    assert_eq!(first.control.out.packets, 1, "one handshake so far");

    // Re-aim at the same device: FINAL goes out, the session ends, a new one
    // is built. Everything the first session cost has to still be there.
    link.reattach(rx.device());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !link.state().is_up() {
        link.send(Pixels::rgb(&frame)).expect("send");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(link.state().is_up(), "the second session should have come up");
    for _ in 0..10 {
        link.send(Pixels::rgb(&frame)).expect("send");
        std::thread::sleep(Duration::from_millis(20));
    }

    let second = link.stats().traffic;
    assert!(
        second.frames.out.packets > first.frames.out.packets,
        "the second session must add to the first: {first:?} then {second:?}"
    );
    assert!(second.frames.out.bytes > first.frames.out.bytes);
    assert_eq!(second.control.out.packets, 2, "two sessions, two handshakes");

    // And closing the link banks the FINAL frame rather than losing it.
    let before = link.stats().traffic.frames.out.packets;
    link.close();
    assert!(link.stats().traffic.frames.out.packets > before, "FINAL is a datagram too");

    drop(link);
    rx.shutdown();
}

/// A link with nowhere to send counts nothing at all - the "a device that is
/// away costs nothing" half of the card.
#[test]
fn a_link_to_nobody_counts_nothing() {
    // Port 1 on loopback: nothing is listening and nothing can be.
    let mut link = screeny::Link::open_deferred(
        screeny::Target::parse("127.0.0.1:1"),
        LinkConfig::default(),
    );
    let frame = vec![0u8; screeny::proto::NPIX * 3];
    for _ in 0..5 {
        link.send(Pixels::rgb(&frame)).expect("the network cannot fail a send");
    }
    assert_eq!(link.stats().traffic, screeny::LinkTraffic::default());
}
