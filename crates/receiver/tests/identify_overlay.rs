//! `IDENTIFY` owns the panel: frames keep arriving underneath it, and none of
//! them is shown until it ends (card 247, item 1).
//!
//! Spec section 6.3 says `IDENTIFY` "overrides the display ... and MUST work
//! in any state, including while another sender holds the lock", and section
//! 7.3 says it is an overlay rather than a stream state - "frame handling
//! continues underneath". `docs/design/device-web.md` decision 7 is the other
//! half: the frame path is the product, so *handling* a frame means all of it -
//! accepted, decoded, counted, telemetry answered - and the single thing an
//! overlay takes away is the swap onto the panel.
//!
//! The firmware got the first half right and the second half wrong for as long
//! as the overlay has existed. `firmware/src/net.rs` published every decoded
//! frame and then, microseconds later, drew the identify screen into a
//! *different* slot of the lock-free triple buffer that core 1 latches ~154
//! times a second; at 30 fps the panel caught the stream in that gap several
//! times a second. The owner saw "the art flickering through" the status
//! screen on the card 230 bench, and reproduced it with no button involved at
//! all (`screeny identify --ms 10000` over a live stream). The setup, update
//! and button screens never flickered because the same task already gated
//! their publish.
//!
//! So the rule is [`Intent::shows_frames`], here, where there is one of it,
//! and these tests are what pins it to the state machine rather than to a
//! `matches!` in one caller.

use screeny_proto::control::{IdleMode, Telemetry};
use screeny_receiver::{Host, Intent, Params, Receiver, Timing};

/// The least a `Host` can be: this test never looks at what was sent.
#[derive(Default)]
struct Silent;

impl Host for Silent {
    type Addr = (u32, u16);
    type Ip = u32;

    fn ip_of(a: (u32, u16)) -> u32 {
        a.0
    }

    fn micros(&self) -> u64 {
        0
    }

    fn send_from_frame_sock(&mut self, _to: (u32, u16), _bytes: &[u8]) {}

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

/// A minimal `SOLID` frame (spec section 4.1): one RGB triple, no stats
/// request, so nothing comes back and the test has nothing to ignore.
fn solid_frame(seq: u16, rgb: [u8; 3]) -> Vec<u8> {
    let mut d = vec![
        screeny_proto::MAGIC,
        (screeny_proto::VERSION << 4) | screeny_proto::TYPE_FRAME,
        screeny_proto::dec::codec::SOLID,
        screeny_proto::F_KEY,
    ];
    d.extend_from_slice(&seq.to_le_bytes());
    d.extend_from_slice(&(rgb.len() as u16).to_le_bytes());
    d.extend_from_slice(&rgb);
    d
}

/// An `IDENTIFY` control request (spec section 6.3), `req_id` 0 - the same
/// shape the firmware's button synthesises.
fn identify_for(ms: u16) -> Vec<u8> {
    let mut buf = [0u8; 32];
    let n = screeny_proto::control::Request::Identify { duration_ms: ms }
        .write(0, &mut buf)
        .expect("an identify request encodes");
    buf[..n].to_vec()
}

fn blank() -> screeny_proto::Rgb888Frame {
    [0u8; screeny_proto::NBYTES]
}

/// Feed one frame in and decode it, the way the firmware's frame task does.
/// Returns what `flush_frames` said: "there is a newly decoded frame".
fn one_frame(
    rx: &mut Receiver<(u32, u16)>,
    h: &mut Silent,
    now_us: u64,
    seq: u16,
    rgb: [u8; 3],
    into: &mut screeny_proto::Rgb888Frame,
) -> bool {
    let d = solid_frame(seq, rgb);
    rx.offer_frame(h, now_us, (1, 5000), &d);
    rx.flush_frames(h, now_us, Some(&d[..]), into)
}

#[test]
fn a_frame_under_identify_is_decoded_and_counted_and_not_shown() {
    let mut rx = receiver();
    let mut h = Silent;
    let mut frame = blank();
    let mut reply = [0u8; 64];

    // A stream is live and the panel is showing it.
    assert!(one_frame(&mut rx, &mut h, 0, 1, [10, 20, 30], &mut frame));
    assert!(
        rx.intent(0).shows_frames(),
        "a plain live stream shows its frames"
    );
    assert_eq!(rx.stats().counters.frames_shown, 1);

    // The overlay goes up, over the top of it.
    rx.control(&mut h, 1_000, (2, 6000), &identify_for(10_000), &mut reply);
    assert_eq!(rx.intent(1_000), Intent::Identify);
    assert!(
        !rx.intent(1_000).shows_frames(),
        "IDENTIFY owns the panel: nothing decoded underneath it may be swapped in"
    );

    // Frame handling continues underneath, all of it. `flush_frames` still
    // says "decoded", the pixels still land in the caller's buffer, and the
    // counter still moves - section 7.3's "frame handling continues".
    let before = rx.stats().counters.frames_shown;
    assert!(
        one_frame(&mut rx, &mut h, 2_000, 2, [40, 50, 60], &mut frame),
        "a frame arriving under the overlay is still decoded"
    );
    assert_eq!(&frame[..3], &[40, 50, 60], "and it is still decoded *here*");
    assert_eq!(
        rx.stats().counters.frames_shown,
        before + 1,
        "and still counted - an overlay must not make the sender's numbers lie"
    );
    assert_eq!(
        rx.state(),
        screeny_receiver::State::Live,
        "the stream state is untouched by an overlay (section 7.3)"
    );
    // What the caller must *not* do with it:
    assert!(!rx.intent(2_000).shows_frames());
}

#[test]
fn the_picture_returns_on_the_first_frame_after_the_overlay() {
    let mut rx = receiver();
    let mut h = Silent;
    let mut frame = blank();
    let mut reply = [0u8; 64];

    assert!(one_frame(&mut rx, &mut h, 0, 1, [10, 20, 30], &mut frame));
    rx.control(&mut h, 0, (2, 6000), &identify_for(200), &mut reply);
    assert!(!rx.intent(0).shows_frames());

    // Still up a tick before its deadline...
    rx.tick(&mut h, 199_000);
    assert!(!rx.intent(199_000).shows_frames());

    // ...and gone the tick after it, without anybody asking it to stop.
    rx.tick(&mut h, 200_000);
    assert_eq!(rx.intent(200_000), Intent::Stream);
    assert!(
        rx.intent(200_000).shows_frames(),
        "the next frame after the overlay is the one that puts the picture back"
    );
    assert!(one_frame(&mut rx, &mut h, 201_000, 2, [40, 50, 60], &mut frame));
}

#[test]
fn identify_zero_stops_it_and_the_panel_is_the_streams_again() {
    // Section 6.3: `IDENTIFY` with `duration_ms` 0 stops one in progress.
    let mut rx = receiver();
    let mut h = Silent;
    let mut reply = [0u8; 64];

    rx.control(&mut h, 0, (2, 6000), &identify_for(5_000), &mut reply);
    assert!(!rx.intent(0).shows_frames());
    rx.control(&mut h, 1_000, (2, 6000), &identify_for(0), &mut reply);
    assert!(rx.intent(1_000).shows_frames());
}

#[test]
fn the_idle_screens_are_not_overlays() {
    // `Fade` and `Idle` compose *from* the last streamed frame and there is no
    // sender to take the panel from, so they answer `true`: a frame that
    // arrives during a cross-fade goes straight up, which is section 7.5's
    // "a new frame cancels the fade".
    assert!(Intent::Stream.shows_frames());
    assert!(Intent::Idle.shows_frames());
    assert!(Intent::Fade { t: 0 }.shows_frames());
    assert!(Intent::Fade { t: 256 }.shows_frames());
    assert!(!Intent::Identify.shows_frames());
}
