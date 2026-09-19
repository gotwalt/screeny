//! Indexed frames go on the wire **exactly**, end to end through
//! `screeny-sim`.
//!
//! This is the property the generative art system is built on (card 011, and
//! `docs/design/generative-art-brief.md` section 5): a piece that renders a
//! palette and an index plane gets those pixels on the panel, not a
//! requantised approximation of them. The assertion is always the same and it
//! is the strongest one available - for every pixel, the decoded frame equals
//! `palette[index]` - and it is checked by the *other* implementation of the
//! protocol, which owes this crate's encoders nothing.
//!
//! The second half of the file pins the fallback: what happens when no exact
//! encoding fits, which must be lossy-but-visible rather than silent.

mod simfix;

use std::time::Duration;

use screeny::encode::MIN_BUDGET;
use screeny::{SenderConfig, Sent};
use simfix::{connect, expand, PATIENCE};

use screeny_proto::dec::codec;
use screeny_proto::NPIX;

/// A deterministic, deliberately incompressible index plane: a 64-bit LCG, so
/// the LZ coders have nothing to find and the fixed-rate rungs are the only
/// ones that can carry it.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..NPIX)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((s >> 33) as usize % n) as u8
        })
        .collect()
}

/// `n` visibly different colours, spread round the hue circle so a mistake
/// shows up as a colour rather than a rounding error.
fn palette(n: usize) -> Vec<[u8; 3]> {
    (0..n)
        .map(|i| {
            let k = i as f64 / n as f64;
            let f = |p: f64| {
                let v = (((k + p) * std::f64::consts::TAU).sin() * 0.5 + 0.5) * 255.0;
                v.round() as u8
            };
            [f(0.0), f(1.0 / 3.0), f(2.0 / 3.0)]
        })
        .collect()
}

/// Send one indexed frame, wait for the simulator to display it, and assert
/// the decoded pixels are `palette[index]` for all 2048 of them.
fn assert_exact(label: &str, pal: &[[u8; 3]], idx: &[u8]) {
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let sent = sender.send_indexed(pal, idx).expect("sends");

    let shot = sim
        .wait_for_frames(1, PATIENCE)
        .unwrap_or_else(|| panic!("{label}: the simulator never displayed a frame"));

    assert!(
        sent.exact(),
        "{label}: the sender does not claim {} colours are exact",
        pal.len()
    );
    assert_eq!(
        &shot.decoded[..],
        expand(pal, idx).as_slice(),
        "{label}: the panel would show something other than palette[index]"
    );
    assert_eq!(shot.telemetry.frames_dropped_decode, 0, "{label}");
    assert_eq!(sender.stats().indexed_exact, 1, "{label}");
    assert_eq!(sender.stats().indexed_fallback, 0, "{label}");

    // The codec the chooser reached for is worth pinning too: it is the
    // difference between 280 bytes and 1376, and a silent move to the bottom
    // rung is how a regression would hide.
    let meta = shot.shown.expect("a frame was shown");
    assert!(
        [codec::PAL4_LZ, codec::PAL8_LZ, codec::PAL5, codec::SOLID].contains(&meta.codec),
        "{label}: unexpected codec {:#04x}",
        meta.codec
    );
    drop(sender);
    drop(dev);
}

#[test]
fn a_two_colour_frame_is_exact() {
    let pal = palette(2);
    let idx: Vec<u8> = (0..NPIX).map(|p| ((p / 8 + p / 512) % 2) as u8).collect();
    assert_exact("2 colours", &pal, &idx);
}

#[test]
fn a_sixteen_colour_frame_is_exact() {
    let pal = palette(16);
    let idx: Vec<u8> = (0..NPIX).map(|p| ((p % 64) / 4) as u8).collect();
    assert_exact("16 colours", &pal, &idx);
}

/// Seventeen is the interesting number: one past `PAL4`'s 4 bits per pixel,
/// so it can only go out as `PAL8_LZ` or `PAL5`.
#[test]
fn a_seventeen_colour_frame_is_exact() {
    let pal = palette(17);
    let idx: Vec<u8> = (0..NPIX).map(|p| (p % 17) as u8).collect();
    assert_exact("17 colours", &pal, &idx);
}

#[test]
fn a_thirty_two_colour_frame_is_exact() {
    let pal = palette(32);
    let idx: Vec<u8> = (0..NPIX).map(|p| (p % 32) as u8).collect();
    assert_exact("32 colours", &pal, &idx);
}

/// The worst case the brief's "up to 32 colours is exact" promise has to
/// survive: indices with no structure at all, at each palette size that
/// changes which rung carries it. Both LZ coders overflow on this, so the
/// frame goes out as raw `PAL5` - fixed-rate at 1376 bytes, and therefore
/// incapable of overflowing.
#[test]
fn incompressible_index_noise_is_still_exact() {
    for n in [2usize, 16, 17, 32] {
        let pal = palette(n);
        let idx = noise(n, 0x5EED_0011 + n as u64);
        assert_exact(&format!("{n}-colour noise"), &pal, &idx);
    }
}

/// `Pixels::Indexed` through the one `send` door must behave identically to
/// `send_indexed`: the enum is a convenience, not a second code path.
#[test]
fn the_pixels_enum_takes_the_same_path() {
    let pal = palette(16);
    let idx = noise(16, 7);
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let sent = sender
        .send(screeny::Pixels::indexed(&pal, &idx))
        .expect("sends");
    assert!(sent.exact());
    let shot = sim.wait_for_frames(1, PATIENCE).expect("displayed");
    assert_eq!(&shot.decoded[..], expand(&pal, &idx).as_slice());
    drop(sender);
    drop(dev);
}

/// A run of frames, every one of them exact, so nothing about the encoder's
/// frame-to-frame state (palette seeds, the dither phase, codec hysteresis)
/// can creep in and make frame 30 approximate.
#[test]
fn exactness_holds_over_a_stream() {
    let pal = palette(24);
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    const N: u32 = 30;
    let mut last = Vec::new();
    for f in 0..N {
        let idx: Vec<u8> = (0..NPIX).map(|p| ((p + f as usize) % 24) as u8).collect();
        assert!(sender.send_indexed(&pal, &idx).expect("sends").exact());
        last = idx;
        // Let the simulator drain: newest-wins means a burst would leave only
        // the last frame, and this test is about all thirty.
        std::thread::sleep(Duration::from_millis(4));
    }
    let shot = sim
        .wait_until(PATIENCE, |s| s.telemetry.frames_shown >= N)
        .expect("all thirty displayed");
    assert_eq!(&shot.decoded[..], expand(&pal, &last).as_slice());
    assert_eq!(sender.stats().indexed_exact, u64::from(N));
    assert_eq!(sender.stats().indexed_fallback, 0);
    drop(sender);
    drop(dev);
}

// ---------------------------------------------------------------------------
// The fallback
// ---------------------------------------------------------------------------

/// More than 32 colours *and* incompressible is the one case no exact
/// encoding can carry: `PAL8_LZ` is the only rung that takes a palette that
/// big and it is variable-rate. The frame must still be shown - a requantised
/// frame beats a dropped one - and the sender must say so.
#[test]
fn an_over_budget_indexed_frame_falls_back_and_says_so() {
    let pal = palette(200);
    let idx = noise(200, 0xABCD);
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let sent = sender.send_indexed(&pal, &idx).expect("sends");

    assert!(sent.is_sent(), "the frame must still go out");
    assert!(!sent.exact(), "200 incompressible colours cannot be exact");
    assert_eq!(sender.stats().indexed_fallback, 1);
    assert_eq!(sender.stats().indexed_exact, 0);
    assert_eq!(sender.stats().last_fallback_colours, 200);

    let shot = sim.wait_for_frames(1, PATIENCE).expect("displayed");
    assert_ne!(
        &shot.decoded[..],
        expand(&pal, &idx).as_slice(),
        "if this were exact the test above it is wrong"
    );
    assert_eq!(shot.telemetry.frames_dropped_decode, 0);
    assert!(sent.bytes() <= sender.budget());
    drop(sender);
    drop(dev);
}

/// A palette of 200 colours that *does* compress stays exact, so the fallback
/// is about the byte budget and not about the palette being large.
#[test]
fn a_large_but_compressible_palette_is_still_exact() {
    let pal = palette(200);
    // Long runs: the LZ coder's best case.
    let idx: Vec<u8> = (0..NPIX).map(|p| ((p / 64) % 200) as u8).collect();
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let sent = sender.send_indexed(&pal, &idx).expect("sends");
    assert!(sent.exact(), "a compressible 200-colour frame should be exact");
    assert_eq!(sent.codec(), Some(codec::PAL8_LZ));
    let shot = sim.wait_for_frames(1, PATIENCE).expect("displayed");
    assert_eq!(&shot.decoded[..], expand(&pal, &idx).as_slice());
    drop(sender);
    drop(dev);
}

/// The other way to run out of room: a budget below `PAL5`'s fixed 1376
/// bytes. Then even 32 colours cannot be exact, and the sender must be as
/// loud about it as it is about the 200-colour case.
#[test]
fn a_budget_below_the_pal5_floor_forces_the_fallback() {
    let pal = palette(32);
    let idx = noise(32, 99);
    let cfg = SenderConfig {
        budget: Some(MIN_BUDGET - 1),
        ..SenderConfig::default()
    };
    let (dev, sim, mut sender) = connect(cfg);
    assert!(sender.budget_is_tight());
    let sent = sender.send_indexed(&pal, &idx).expect("sends");
    assert!(sent.is_sent());
    assert!(!sent.exact());
    assert!(sent.bytes() < MIN_BUDGET);
    assert_eq!(sender.stats().indexed_fallback, 1);
    sim.wait_for_frames(1, PATIENCE).expect("displayed");
    drop(sender);
    drop(dev);
}

// ---------------------------------------------------------------------------
// Malformed frames: the caller's bugs, named
// ---------------------------------------------------------------------------

#[test]
fn a_short_index_plane_is_rejected_before_any_packet() {
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let err = sender.send_indexed(&palette(4), &[0u8; 100]).unwrap_err();
    assert!(
        matches!(err, screeny::Error::Frame { got: 100, want: NPIX, .. }),
        "got {err:?}"
    );
    assert_eq!(sender.stats().frames_sent, 0);
    assert_eq!(sim.telemetry().frames_rx, 0);
    drop(sender);
    drop(dev);
}

#[test]
fn an_index_outside_the_palette_names_the_pixel() {
    let (dev, _sim, mut sender) = connect(SenderConfig::default());
    let mut idx = vec![0u8; NPIX];
    idx[1234] = 9;
    let err = sender.send_indexed(&palette(4), &idx).unwrap_err();
    assert!(
        matches!(
            err,
            screeny::Error::BadIndex {
                index: 9,
                pixel: 1234,
                palette: 4
            }
        ),
        "got {err:?}"
    );
    drop(sender);
    drop(dev);
}

#[test]
fn a_wrongly_sized_rgb_frame_is_rejected() {
    let (dev, _sim, mut sender) = connect(SenderConfig::default());
    let err = sender
        .send(screeny::Pixels::rgb(&[0u8; 1000]))
        .unwrap_err();
    assert!(matches!(err, screeny::Error::Frame { got: 1000, .. }), "got {err:?}");
    drop(sender);
    drop(dev);
}

/// The bridge that makes the art system's `Output` impl a one-liner: its
/// `send` returns `io::Result`, so `screeny::Error` has to become an
/// `io::Error` with `?` and keep its message.
#[test]
fn errors_convert_to_io_errors_without_losing_the_message() {
    let e = screeny::Error::Frame {
        what: "an RGB frame",
        got: 10,
        want: 6144,
    };
    let text = e.to_string();
    let io: std::io::Error = e.into();
    assert_eq!(io.to_string(), text);
    assert!(io.get_ref().is_some(), "the original error should survive");

    // An I/O error keeps its kind, so a caller can still match on it.
    let raw = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    let io: std::io::Error = screeny::Error::Io(raw).into();
    assert_eq!(io.kind(), std::io::ErrorKind::ConnectionRefused);
}

/// `Sender` is the low-level half and does no pacing at all: it sends exactly
/// what it is given, when it is given it. That is the contract `Link` builds
/// the cadence ceiling on top of, and the one an embedder relies on when it
/// wants to do its own pacing.
#[test]
fn the_sender_does_not_pace() {
    let pal = palette(4);
    let idx = vec![0u8; NPIX];
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let t0 = std::time::Instant::now();
    for _ in 0..20 {
        let _: Sent = sender.send_indexed(&pal, &idx).expect("sends");
    }
    assert!(
        t0.elapsed() < Duration::from_millis(200),
        "twenty frames took {:?}; Sender must not be sleeping",
        t0.elapsed()
    );
    assert_eq!(sender.stats().frames_sent, 20);
    // The device may well supersede most of them, which is exactly why
    // `Link` has a cadence ceiling.
    sim.wait_for_frames(1, PATIENCE).expect("at least one shown");
    drop(sender);
    drop(dev);
}

/// `Sender::send` on an RGB frame is the existing chooser path, unchanged;
/// this only pins that the new door reaches it and reports honestly.
#[test]
fn an_rgb_frame_of_few_colours_is_exact_too() {
    let mut f = screeny::Frame::black();
    for p in 0..NPIX {
        f.set_at(p, palette(8)[p % 8]);
    }
    let (dev, sim, mut sender) = connect(SenderConfig::default());
    let sent = sender.send(screeny::Pixels::from(&f)).expect("sends");
    assert!(sent.exact(), "8 colours of RGB should be lossless");
    let shot = sim.wait_for_frames(1, PATIENCE).expect("displayed");
    assert_eq!(&shot.decoded[..], f.as_bytes().as_slice());
    // An RGB frame is not an indexed one, whatever the chooser did with it.
    assert_eq!(sender.stats().indexed_exact, 0);
    drop(sender);
    drop(dev);
}
