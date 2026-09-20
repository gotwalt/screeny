//! How many datagrams one drain can ask its caller to send, and how big.
//!
//! Card 234. The firmware sends the receiver's unsolicited datagrams
//! (section 6.2) one at a time from the frame task, and since this card each
//! one has a deadline: `firmware/src/net.rs`'s `send_bounded` gives it 200 ms
//! before it gives up, warns, and - after three in a row - re-binds the
//! socket. That price is only acceptable because the *count* is bounded: four
//! datagrams is at worst 800 ms of a frozen frame loop on a device that is
//! already broken, where an unbounded count would be a stall of its own.
//!
//! **Where the bound actually comes from is not where it looks.** It is not
//! section 6.2's rate limits: [`screeny_receiver::Limiter`] has four slots and
//! *evicts the oldest* when it is full, deliberately ("which can only ever
//! make the device more generous to a source it has forgotten"), so twelve new
//! sources in one drain get twelve `BUSY`s. The bound is the firmware's
//! `Outbox`, a `heapless::Vec<Out, 4>` whose `push` failure is discarded
//! (`firmware/src/receiver.rs`). These tests pin both halves: what the
//! receiver *offers*, and what the firmware's four slots keep - because
//! `firmware/src/net.rs`'s `FRAME_TX_BUF` (512 bytes, four `tx_meta` slots) is
//! sized from that four and would be under-sized if anyone raised it.

use screeny_proto::control::{op, IdleMode, Telemetry};
use screeny_receiver::{Host, Params, Receiver, Timing, OUT_MAX};

/// The number of slots `firmware/src/receiver.rs`'s `Outbox` has.
const FIRMWARE_OUTBOX: usize = 4;

/// A host shaped like the firmware's: it keeps at most [`FIRMWARE_OUTBOX`]
/// datagrams and counts the ones it had to throw away.
#[derive(Default)]
struct Outbox {
    /// `(destination, length, opcode)` for each datagram that was kept.
    kept: Vec<(u32, usize, u8)>,
    /// Datagrams the receiver offered past the fourth, which the firmware's
    /// `let _ = out.push(item)` silently discards.
    dropped: usize,
}

impl Outbox {
    fn clear(&mut self) {
        self.kept.clear();
    }
}

impl Host for Outbox {
    type Addr = (u32, u16);
    type Ip = u32;

    fn ip_of(a: (u32, u16)) -> u32 {
        a.0
    }

    fn micros(&self) -> u64 {
        0
    }

    fn send_from_frame_sock(&mut self, to: (u32, u16), bytes: &[u8]) {
        if self.kept.len() < FIRMWARE_OUTBOX {
            self.kept.push((to.0, bytes.len(), bytes[2]));
        } else {
            self.dropped += 1;
        }
    }

    fn wifi(&self) -> (&'static str, u8) {
        ("Example-Wifi1", 0)
    }

    fn adjust_telemetry(&self, _t: &mut Telemetry) {}
}

fn receiver() -> Receiver<(u32, u16)> {
    Receiver::new(&Params {
        id: "abcdef",
        fw: "0.0.0-test",
        name: "screeny-abcdef",
        idle_mode: IdleMode::Status,
        timing: Timing::SPEC,
        ..Params::default()
    })
}

/// A minimal `SOLID` frame (spec section 4.1): one RGB triple. Written out
/// rather than taken from `screeny_probe::enc`, which this `no_std` crate does
/// not and should not depend on.
fn solid_frame(seq: u16) -> Vec<u8> {
    let payload = [7u8, 9, 11];
    let mut d = vec![
        screeny_proto::MAGIC,
        (screeny_proto::VERSION << 4) | screeny_proto::TYPE_FRAME,
        screeny_proto::dec::codec::SOLID,
        screeny_proto::F_KEY | screeny_proto::F_STATS_REQ,
    ];
    d.extend_from_slice(&seq.to_le_bytes());
    d.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    d.extend_from_slice(&payload);
    d
}

fn blank() -> screeny_proto::Rgb888Frame {
    [0u8; screeny_proto::NBYTES]
}

/// Twelve sources in one drain, all refused, all asking for stats: the
/// firmware's send loop still only ever has four small datagrams to push.
#[test]
fn the_firmware_outbox_caps_one_drains_send_loop_at_four() {
    let mut rx = receiver();
    let mut h = Outbox::default();
    let mut frame = blank();

    // The holder takes the lock first, so everyone after it is locked out.
    rx.offer_frame(&mut h, 0, (1, 5000), &solid_frame(1));
    for i in 2..=12u32 {
        rx.offer_frame(&mut h, 1_000, (i, 5000), &solid_frame(i as u16));
    }
    rx.flush_frames(&mut h, 1_000, Some(&solid_frame(1)), &mut frame);

    assert!(
        !h.kept.is_empty(),
        "twelve sources and nothing came back; the test is not exercising section 6.2"
    );
    assert_eq!(
        h.kept.len(),
        FIRMWARE_OUTBOX,
        "the outbox should be full; it holds {:?}",
        h.kept
    );
    // The surprise this test exists to pin. If `Limiter`'s eviction rule or
    // `LIMIT_SLOTS` ever changed so that this were zero, the four-slot outbox
    // would stop being the thing that bounds the send loop, and
    // `FRAME_TX_BUF`'s reasoning in `firmware/src/net.rs` would need re-doing
    // from whatever bounds it instead.
    assert!(
        h.dropped > 0,
        "the receiver offered no more than four datagrams for twelve new sources, \
         so the outbox is no longer what bounds the firmware's send loop - \
         re-check FRAME_TX_BUF"
    );

    for (to, len, opcode) in &h.kept {
        assert!(
            *len <= OUT_MAX,
            "a {len}-byte datagram for source {to}; OUT_MAX is {OUT_MAX}"
        );
        assert!(
            *opcode == op::BUSY || *opcode == op::TELEMETRY,
            "section 6.2 has two unsolicited packets; this one is {opcode:#04x}"
        );
    }

    // `FRAME_TX_BUF` is `8 * OUT_MAX` = 512 bytes for a queue that is four
    // `tx_meta` slots deep. One drain must fit in it with room to spare.
    let bytes: usize = h.kept.iter().map(|(_, n, _)| n).sum();
    assert!(
        bytes <= 4 * OUT_MAX,
        "one drain queued {bytes} bytes; FRAME_TX_BUF assumes at most {}",
        4 * OUT_MAX
    );
}

/// Sustained: thirty drains of eight refused sources each, and no drain ever
/// hands the send loop more than the outbox holds.
#[test]
fn a_sustained_flood_never_bursts_past_the_outbox() {
    let mut rx = receiver();
    let mut h = Outbox::default();
    let mut frame = blank();
    let mut worst = 0usize;
    let mut total = 0usize;

    for round in 0..30u32 {
        let now = round as u64 * 20_000; // a 20 ms frame tick
        h.clear();
        let seq = round as u16 + 1;
        rx.offer_frame(&mut h, now, (1, 5000), &solid_frame(seq));
        for i in 2..=8u32 {
            rx.offer_frame(&mut h, now, (i, 5000), &solid_frame(seq));
        }
        rx.flush_frames(&mut h, now, Some(&solid_frame(seq)), &mut frame);
        worst = worst.max(h.kept.len());
        total += h.kept.len();
    }

    assert!(
        worst <= FIRMWARE_OUTBOX,
        "one drain of a sustained flood handed the send loop {worst} datagrams"
    );
    // 30 drains x 8 sources = 240 refused frames over 600 ms of virtual time.
    // Even with the limiter evicting, the firmware never pushes more than four
    // per drain, so the worst the send loop can cost is bounded whatever the
    // flood does.
    assert!(
        total <= 30 * FIRMWARE_OUTBOX,
        "{total} datagrams over 30 drains; four per drain is the ceiling"
    );
}
