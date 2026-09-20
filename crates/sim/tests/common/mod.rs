//! The shared sender, the shared encoders, and the vectors.
//!
//! All three used to live here. Card 080 moved them into `screeny_probe`,
//! which is a library as well as the bench binary, so that the wire-level
//! conformance suite and these tests drive **one** sender and **one** set of
//! section-4 encoders rather than two that drift apart. What is left in this
//! file is the thin test-shaped adapter: a `Sender` whose methods panic
//! instead of returning `io::Result`, because on a loopback socket a failure
//! is a broken test rather than a condition to handle, and an `unwrap` at
//! every one of the sixty-odd call sites would be noise.
//!
//! The encoders are still deliberately **not** `crates/encode`. They are
//! written from spec section 4's prose, so a test that round-trips through
//! them is testing two independent readings of the spec against each other;
//! the product encoder graded against the product decoder would only prove
//! `screeny-proto` agrees with itself. `screeny_probe::enc` says the same.

// This module is compiled separately into each test binary, and no one binary
// uses all of it.
#![allow(dead_code, unused_imports)]

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

pub use screeny_probe::enc::{
    assert_frames_eq, bc1_dual, bc1_mixed, indexed_runs, lz_compress, pal4_lz, pal5, pal8_lz,
    solid, Block, Indexed,
};
pub use screeny_probe::link::{error_code, parse_reply, OwnedReply};
pub use screeny_probe::vectors::{all as vectors, Vector};

use screeny_probe::link::FrameLink;

/// A sender's control socket. Spec section 9.2: bound separately from the
/// frame socket, so a `TELEMETRY` arriving on the frame socket is unambiguous.
pub type Ctrl = screeny_probe::link::Control;

/// A sender's frame socket: sends `FRAME`s, and receives the `TELEMETRY` and
/// `BUSY` packets spec section 6.4 says arrive back on it.
///
/// One `screeny_probe::link::FrameLink` underneath; this only turns its
/// `io::Result` into a panic.
pub struct Sender {
    pub link: FrameLink,
}

impl Sender {
    /// Bind an ephemeral loopback port and aim at `dst`.
    pub fn new(dst: SocketAddr) -> Sender {
        Sender {
            link: FrameLink::connect(dst).expect("bind sender"),
        }
    }

    /// This sender's own address, which is its identity as far as spec
    /// section 7.1 is concerned.
    pub fn addr(&self) -> SocketAddr {
        self.link.addr()
    }

    /// The next sequence number this sender will use.
    pub fn seq(&self) -> u16 {
        self.link.seq
    }

    /// Send one frame, incrementing `seq` afterwards, and return the `seq`
    /// it used.
    pub fn send(&mut self, codec: u8, flags: u8, payload: &[u8]) -> u16 {
        self.link.send(codec, flags, payload).expect("send frame")
    }

    /// Send one frame with an explicit sequence number.
    pub fn send_seq(&self, codec: u8, flags: u8, seq: u16, payload: &[u8]) {
        self.link
            .send_seq(codec, flags, seq, payload)
            .expect("send frame");
    }

    /// Send one frame with everything spelled out, including `HAS_TS`.
    pub fn send_full(
        &self,
        codec: u8,
        flags: u8,
        seq: u16,
        timestamp_us: Option<u32>,
        payload: &[u8],
    ) {
        self.link
            .send_full(codec, flags, seq, timestamp_us, payload)
            .expect("send frame");
    }

    /// Send arbitrary bytes, for the malformed-packet tests.
    pub fn send_raw(&self, bytes: &[u8]) {
        self.link.send_raw(bytes).expect("send raw");
    }

    /// Wait for one datagram on the frame socket.
    pub fn recv(&self, timeout: Duration) -> Option<Vec<u8>> {
        self.link.recv(timeout)
    }

    /// Drain anything already queued.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.link.drain()
    }
}

/// A loopback port nobody is listening on, for "the device is not there" tests.
pub fn dead_addr() -> io::Result<SocketAddr> {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let a = s.local_addr()?;
    drop(s);
    Ok(a)
}
