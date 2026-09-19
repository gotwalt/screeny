//! End to end over loopback, against the in-process receiver in
//! `tests/common`.
//!
//! Nothing here touches the real device: the receiver binds ephemeral ports on
//! 127.0.0.1 and speaks the protocol through `screeny_proto`.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{Receiver, RxConfig};
use screeny::proto::{F_FINAL, F_KEY, F_STATS_REQ};
use screeny::{ControlClient, FrameTime, Pattern, Sender, SenderConfig};

fn cfg(fps: f64) -> SenderConfig {
    SenderConfig {
        fps,
        stats_interval: Duration::from_millis(200),
        ..SenderConfig::default()
    }
}

#[test]
fn handshake_learns_the_device_metadata() {
    let rx = Receiver::start(RxConfig::default());
    let sender = Sender::connect(rx.device(), cfg(30.0)).expect("connect");
    let info = sender
        .device()
        .info
        .as_ref()
        .expect("GET_INFO should have filled this in");
    assert_eq!(info.w, 64);
    assert_eq!(info.h, 32);
    assert_eq!(info.codecs, screeny::proto::dec::SUPPORTED_CODECS.to_vec());
    assert_eq!(info.mtu as usize, screeny::proto::MAX_PIXEL_PAYLOAD);
    assert_eq!(sender.budget(), screeny::proto::MAX_PIXEL_PAYLOAD);
    // The control port from the TXT record wins over the guess made from the
    // frame port.
    assert_eq!(sender.device().control, rx.control_addr);
    drop(sender);
    rx.shutdown();
}

#[test]
fn a_short_stream_arrives_intact() {
    let rx = Receiver::start(RxConfig::default());
    let mut sender = Sender::connect(rx.device(), cfg(60.0)).expect("connect");
    let stop = AtomicBool::new(false);

    let mut n = 0u64;
    let mut src = screeny::FnSource::new("bars", move |t: FrameTime, out: &mut screeny::Frame| {
        Pattern::Bars.render_at(t.index, out);
        n += 1;
        n <= 20
    });
    sender.run(&mut src, &stop).expect("run");
    let sent = sender.stats().frames_sent;
    drop(sender);

    let state = rx.shutdown();
    assert!(state.decode_errors.is_empty(), "{:?}", state.decode_errors);
    assert_eq!(
        state.frames.len() as u64,
        sent,
        "sent {sent}, received {}",
        state.frames.len()
    );
    assert!(state.frames.len() >= 19);

    // Sequence numbers increment by one and every frame is a keyframe: all
    // five v1 codecs are stateless (spec 4).
    for (i, f) in state.frames.iter().enumerate() {
        assert_eq!(f.seq, i as u16, "seq jumped at frame {i}");
        assert!(f.flags & F_KEY != 0, "frame {i} is not a keyframe");
        assert!(f.bytes <= screeny::proto::MAX_PIXEL_PAYLOAD);
    }
    // The last frame carries FINAL, so the device can release the lock at
    // once instead of waiting out STREAM_TIMEOUT_MS (spec 7.4).
    assert!(
        state.frames.last().unwrap().flags & F_FINAL != 0,
        "no FINAL frame"
    );
    assert_eq!(state.telemetry.frames_dropped_decode, 0);
}

/// `bars` has eight colours over a grey ramp, so the ladder should carry it
/// losslessly. What arrives at the receiver should be the frame that was
/// rendered, pixel for pixel.
#[test]
fn low_colour_frames_arrive_pixel_exact() {
    let rx = Receiver::start(RxConfig::default());
    let mut sender = Sender::connect(rx.device(), cfg(60.0)).expect("connect");
    let stop = AtomicBool::new(false);
    let mut n = 0u64;
    let mut src = screeny::FnSource::new("bars", move |t: FrameTime, out: &mut screeny::Frame| {
        Pattern::Bars.render_at(t.index, out);
        n += 1;
        n <= 5
    });
    sender.run(&mut src, &stop).expect("run");
    drop(sender);

    let want = Pattern::Bars.frame(0);
    let state = rx.shutdown();
    for (i, f) in state.frames.iter().enumerate() {
        let px = f.pixels.as_ref().expect("pixels kept");
        assert_eq!(&px[..], want.as_bytes(), "frame {i} differs");
    }
}

#[test]
fn stats_requests_are_piggybacked_and_answered() {
    let rx = Receiver::start(RxConfig::default());
    let mut sender = Sender::connect(rx.device(), cfg(60.0)).expect("connect");
    let stop = AtomicBool::new(false);
    let mut n = 0u64;
    let mut src = screeny::FnSource::new("bars", move |t: FrameTime, out: &mut screeny::Frame| {
        Pattern::Bars.render_at(t.index, out);
        n += 1;
        n <= 60 // one second at 60 fps, so several 200 ms stats intervals
    });
    sender.run(&mut src, &stop).expect("run");
    let got_telemetry = sender.stats().telemetry.is_some();
    drop(sender);

    let state = rx.shutdown();
    let asked = state
        .frames
        .iter()
        .filter(|f| f.flags & F_STATS_REQ != 0)
        .count();
    assert!(
        (3..=8).contains(&asked),
        "{asked} frames asked for stats over ~1 s at a 200 ms interval"
    );
    // Spec 6.4: never more than one STATS_REQ in 100 ms.
    let mut prev: Option<std::time::Instant> = None;
    for f in state.frames.iter().filter(|f| f.flags & F_STATS_REQ != 0) {
        if let Some(p) = prev {
            assert!(
                f.at.duration_since(p) >= Duration::from_millis(95),
                "two stats requests within 100 ms"
            );
        }
        prev = Some(f.at);
    }
    assert!(got_telemetry, "the sender never saw a TELEMETRY reply");
}

/// Loss on the way in must not break anything: the sender keeps sending, the
/// receiver counts gaps, and no frame is corrupted. With adaptation on and
/// one frame in four lost, the sender should also step its rate down (spec
/// 6.9's network-limited rule).
#[test]
fn survives_loss_and_steps_the_rate_down() {
    let rx = Receiver::start(RxConfig {
        drop_one_in: 4,
        keep_pixels: false,
        ..RxConfig::default()
    });
    let mut sender = Sender::connect(
        rx.device(),
        SenderConfig {
            fps: 60.0,
            stats_interval: Duration::from_millis(100),
            ..SenderConfig::default()
        },
    )
    .expect("connect");

    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            s.store(true, Ordering::SeqCst);
        });
    }
    let mut src = Pattern::Sweep;
    sender.run(&mut src, &stop).expect("run");

    let fps_changes = sender.stats().fps_changes;
    let fps_now = sender.stats().fps;
    let sent = sender.stats().frames_sent;
    drop(sender);

    let state = rx.shutdown();
    assert!(sent > 20, "only {sent} frames sent");
    assert!(state.lost > 0, "the loss injector never fired");
    assert!(state.decode_errors.is_empty(), "{:?}", state.decode_errors);
    assert!(state.telemetry.seq_gaps > 0, "gaps were not noticed");
    assert!(
        fps_changes > 0 && fps_now < 60.0,
        "25% loss should have stepped the rate down; changes={fps_changes} fps={fps_now}"
    );
}

/// With telemetry flowing and nothing going wrong, the sender must leave the
/// frame rate alone.
#[test]
fn a_clean_link_does_not_trigger_adaptation() {
    let rx = Receiver::start(RxConfig {
        keep_pixels: false,
        ..RxConfig::default()
    });
    let mut sender = Sender::connect(
        rx.device(),
        SenderConfig {
            fps: 60.0,
            stats_interval: Duration::from_millis(100),
            ..SenderConfig::default()
        },
    )
    .expect("connect");
    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            s.store(true, Ordering::SeqCst);
        });
    }
    sender.run(&mut Pattern::Sweep, &stop).expect("run");
    assert_eq!(sender.stats().fps_changes, 0);
    assert_eq!(sender.stats().fps, 60.0);
    assert!(!sender.stats().codec_limited);
    assert_eq!(sender.stats().busy, 0);
    drop(sender);
    rx.shutdown();
}

#[test]
fn control_client_speaks_every_op_the_receiver_implements() {
    let rx = Receiver::start(RxConfig::default());
    let mut c = ControlClient::connect(rx.control_addr).expect("connect");

    let (rtt, _uptime) = c.ping().expect("ping");
    assert!(rtt < Duration::from_millis(250));

    let info = c.info().expect("info");
    assert!(info.speaks_v1());
    assert_eq!(info.ctrl, rx.control_addr.port());
    assert_eq!(info.best_codec(&[0x28, 0x10]), Some(0x10));

    // The cap is lower than what was asked for, which is how a sender learns
    // the firmware's limit (spec 6.3).
    assert_eq!(c.set_brightness(200).expect("brightness"), 60);
    assert_eq!(c.set_brightness(10).expect("brightness"), 10);

    c.identify(1000).expect("identify");
    c.set_idle(screeny::proto::control::IdleMode::Dim)
        .expect("set idle");
    c.set_name("bench panel").expect("set name");
    c.reset_stats().expect("reset stats");
    c.release().expect("release");
    let (ssid, state) = c.get_wifi().expect("get wifi");
    assert_eq!(ssid, "Example-Wifi1");
    assert_eq!(state, screeny::proto::control::wifi_state::CONNECTED);
    let t = c.telemetry().expect("telemetry");
    assert_eq!(t.brightness, 10);

    rx.shutdown();
}

/// A control request that gets no answer must time out and retry rather than
/// block forever, and say where it was pointed.
#[test]
fn control_requests_time_out() {
    // Bind a socket and never read it, so requests go nowhere.
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = dead.local_addr().unwrap();
    let mut c = ControlClient::connect(addr).expect("connect");
    c.set_timeout(Duration::from_millis(30));
    c.set_tries(2);
    let started = std::time::Instant::now();
    let e = c.ping().expect_err("should time out");
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(matches!(e, screeny::Error::Timeout { .. }), "{e:?}");
    assert!(e.to_string().contains(&addr.to_string()));
    // Loopback counts as a local address, so the hint is attached.
    assert!(e.hint().is_some_and(|h| h.contains("Local Network")));
}

#[test]
fn a_budget_below_the_device_mtu_is_honoured() {
    let rx = Receiver::start(RxConfig::default());
    let mut sender = Sender::connect(
        rx.device(),
        SenderConfig {
            fps: 120.0,
            budget: Some(1376),
            ..cfg(120.0)
        },
    )
    .expect("connect");
    assert_eq!(sender.budget(), 1376);
    let stop = AtomicBool::new(false);
    let mut n = 0u64;
    let mut src =
        screeny::FnSource::new("gradient", move |t: FrameTime, out: &mut screeny::Frame| {
            Pattern::Gradient.render_at(t.index, out);
            n += 1;
            n <= 10
        });
    sender.run(&mut src, &stop).expect("run");
    drop(sender);
    let state = rx.shutdown();
    assert!(!state.frames.is_empty());
    for f in &state.frames {
        assert!(f.bytes <= 1376, "{} bytes over a 1376 budget", f.bytes);
    }
}

/// A source that ends immediately still produces a clean shutdown.
#[test]
fn an_empty_source_is_not_an_error() {
    let rx = Receiver::start(RxConfig::default());
    let mut sender = Sender::connect(rx.device(), cfg(30.0)).expect("connect");
    let stop = AtomicBool::new(false);
    let mut src = screeny::FnSource::new("nothing", |_t, _out: &mut screeny::Frame| false);
    sender.run(&mut src, &stop).expect("run");
    assert_eq!(sender.stats().frames_sent, 0);
    drop(sender);
    let state = rx.shutdown();
    assert!(state.frames.is_empty());
}
