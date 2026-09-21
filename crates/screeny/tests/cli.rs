//! The `screeny` binary, driven against the in-process receiver.
//!
//! This is card 009's acceptance criterion executed literally:
//! `screeny pattern bars --addr 127.0.0.1:PORT` drives a receiver at a steady
//! 30 fps. The device on the bench is never involved - `--addr` points at
//! loopback and the receiver is two sockets in this process.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{Receiver, RxConfig};
use screeny_sim::{Config as SimConfig, SimDevice};

fn screeny() -> Command {
    Command::new(env!("CARGO_BIN_EXE_screeny"))
}

/// A free, **consecutive** UDP port pair: `--addr HOST:PORT` guesses the
/// control port as frame + 1, the way a device address typed by a human does
/// (`Device::from_addr`), so the pair has to really be adjacent - mirroring
/// `embed.rs`'s `free_port_pair`.
fn free_port_pair() -> u16 {
    use std::net::UdpSocket;
    loop {
        let a = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let p = a.local_addr().expect("addr").port();
        drop(a);
        if UdpSocket::bind(("127.0.0.1", p + 1)).is_ok() && p < u16::MAX - 1 {
            return p;
        }
    }
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = screeny().args(args).output().expect("run screeny");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn pattern_bars_drives_a_receiver_at_thirty_fps() {
    let rx = Receiver::start_adjacent(RxConfig {
        keep_pixels: true,
        ..RxConfig::default()
    });
    let addr = rx.frame_addr.to_string();
    let (ok, stdout, stderr) = run(&[
        "pattern",
        "bars",
        "--addr",
        &addr,
        "--fps",
        "30",
        "--duration",
        "2",
    ]);
    assert!(ok, "screeny failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("streaming bars"), "{stdout}");

    let state = rx.shutdown();
    assert!(state.decode_errors.is_empty(), "{:?}", state.decode_errors);
    let n = state.frames.len();
    assert!(
        (55..=65).contains(&n),
        "{n} frames in 2 s, expected about 60"
    );
    let fps = state.fps();
    assert!((29.4..=30.6).contains(&fps), "receiver measured {fps:.2} fps");

    // `bars` is an eight-colour frame over a grey ramp: the ladder carries it
    // losslessly, so what arrived is what was rendered.
    let want = screeny::Pattern::Bars.frame(0);
    let got = state.frames[0].pixels.as_ref().unwrap();
    assert_eq!(&got[..], want.as_bytes());
    // And the GET_INFO handshake happened over the control port.
    assert!(state.control_requests >= 1);
}

/// `screeny clock` is the built-in demo that authors its own palette, and
/// card 092's whole point: it must reach the panel as `PAL4_LZ` carrying that
/// palette, never expanded to RGB and requantised on the way.
///
/// The fixed `--at` keeps the phrase and its colour ramp deterministic; the
/// sheen and the progress marker still move, so these are twenty different
/// frames, not one repeated.
#[test]
fn clock_streams_its_own_palette_exactly() {
    let rx = Receiver::start_adjacent(RxConfig {
        keep_pixels: true,
        ..RxConfig::default()
    });
    let addr = rx.frame_addr.to_string();
    let (ok, stdout, stderr) = run(&[
        "clock",
        "--at",
        "11:42:50",
        "--addr",
        &addr,
        "--fps",
        "30",
        "--duration",
        "1",
    ]);
    assert!(ok, "screeny failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("streaming clock"), "{stdout}");
    // The summary says the exactness claim out loud, and says nothing about
    // requantised frames.
    assert!(
        stdout.contains("indexed frames exact on the wire"),
        "{stdout}"
    );
    assert!(!stdout.contains("requantised"), "{stdout}");
    assert!(stdout.contains("pal4-lz 100%"), "{stdout}");

    let state = rx.shutdown();
    assert!(state.decode_errors.is_empty(), "{:?}", state.decode_errors);
    let n = state.frames.len();
    assert!((25..=35).contains(&n), "{n} frames in 1 s, expected about 30");
    for f in &state.frames {
        assert_eq!(f.codec, screeny::proto::dec::codec::PAL4_LZ);
        // Eleven colours: one black, a 7-step text ramp, a 3-step accent.
        let px = f.pixels.as_ref().unwrap();
        let frame = screeny::Frame::from_bytes(&px[..]).expect("6144 bytes");
        let colours = frame.distinct_colours();
        assert!(colours <= 11, "{colours} colours in a clock frame");
    }
}

#[test]
fn info_prints_what_the_device_advertises() {
    let rx = Receiver::start_adjacent(RxConfig::default());
    let addr = rx.frame_addr.to_string();
    let (ok, stdout, stderr) = run(&["info", "--addr", &addr]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("panel    64x32"), "{stdout}");
    assert!(stdout.contains("pal8-lz (16)"), "{stdout}");
    assert!(stdout.contains("test receiver"), "{stdout}");
    rx.shutdown();
}

#[test]
fn ping_and_brightness_and_identify_work_over_the_control_port() {
    let rx = Receiver::start_adjacent(RxConfig::default());
    let addr = rx.frame_addr.to_string();

    let (ok, stdout, stderr) = run(&["ping", "--addr", &addr, "-c", "3", "--interval", "0.01"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("3/3 replies"), "{stdout}");

    let (ok, stdout, stderr) = run(&["brightness", "200", "--addr", &addr]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("brightness 60") && stdout.contains("cap"),
        "{stdout}"
    );

    let (ok, stdout, stderr) = run(&["identify", "--addr", &addr, "--ms", "500"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("identifying"), "{stdout}");

    let (ok, stdout, stderr) = run(&["stats", "--addr", &addr, "-n", "2", "--interval", "0.05"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("uptime"), "{stdout}");

    rx.shutdown();
}

/// Card 187: `applied` can differ from what was asked for two different
/// reasons now - a cap pulling it down, or the floor (card 136) pushing it
/// up - and the CLI has to say which. Against `screeny-sim`, the second
/// implementation of the wire protocol (card 006), not the in-process fake
/// above: only `screeny-sim` shares `screeny_receiver::clamp_brightness` with
/// the firmware, so it is the one place on this host that actually applies
/// the floor.
#[test]
fn brightness_wording_tells_a_raise_from_a_cap() {
    // 1..=5 light no output-enable slots at all (25 slots, spec 6.3): raised
    // to the floor, which the wording takes from the reply (6 today), not
    // from a literal.
    let port = free_port_pair();
    let dev = SimDevice::start(SimConfig { frame_port: port, control_port: port + 1, ..SimConfig::for_test() }).expect("sim starts");
    let addr = format!("127.0.0.1:{port}");
    let (ok, stdout, stderr) = run(&["brightness", "3", "--addr", &addr]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("brightness 6") && stdout.contains("raised to the dimmest level the panel can show"),
        "{stdout}"
    );
    drop(dev);

    // A device capped below 255 still reports the cap, not the floor - the
    // cap always wins, per spec 6.3's last clause.
    let port = free_port_pair();
    let dev =
        SimDevice::start(SimConfig { frame_port: port, control_port: port + 1, brightness_cap: 120, ..SimConfig::for_test() })
            .expect("sim starts");
    let addr = format!("127.0.0.1:{port}");
    let (ok, stdout, stderr) = run(&["brightness", "255", "--addr", &addr]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("brightness 120") && stdout.contains("the firmware cap is lower"), "{stdout}");
    drop(dev);
}

/// Rebooting is the one control op that is not idempotent, so it needs the
/// flag.
#[test]
fn reboot_needs_confirmation() {
    let (ok, _out, err) = run(&["reboot", "--addr", "127.0.0.1:1"]);
    assert!(!ok);
    assert!(err.contains("--yes"), "{err}");
}

#[test]
fn pipe_streams_raw_frames_from_stdin() {
    let rx = Receiver::start_adjacent(RxConfig::default());
    let addr = rx.frame_addr.to_string();
    let mut child = screeny()
        .args(["pipe", "--addr", &addr, "--fps", "60"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let frames: Vec<screeny::Frame> = (0..10)
        .map(|i| screeny::Frame::solid([i * 20, 40, 90]))
        .collect();
    {
        let mut stdin = child.stdin.take().unwrap();
        for f in &frames {
            stdin.write_all(f.as_bytes()).unwrap();
        }
    }
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let state = rx.shutdown();
    assert!(state.decode_errors.is_empty());
    assert_eq!(state.frames.len(), 11, "10 frames plus the FINAL repeat");
    for (i, f) in state.frames.iter().take(10).enumerate() {
        assert_eq!(f.codec, screeny::proto::dec::codec::SOLID);
        let px = f.pixels.as_ref().unwrap();
        assert_eq!(&px[..3], &[(i as u8) * 20, 40, 90]);
    }
}

#[test]
fn a_truncated_frame_on_stdin_is_reported_not_displayed() {
    let rx = Receiver::start_adjacent(RxConfig::default());
    let addr = rx.frame_addr.to_string();
    let mut child = screeny()
        .args(["pipe", "--addr", &addr, "--fps", "120"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let mut stdin = child.stdin.take().unwrap();
        let f = screeny::Frame::solid([1, 2, 3]);
        stdin.write_all(f.as_bytes()).unwrap();
        stdin.write_all(&f.as_bytes()[..100]).unwrap();
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("part-way through a frame"), "{err}");
    let state = rx.shutdown();
    // The complete frame, plus its FINAL repeat. The partial one is dropped.
    assert_eq!(state.frames.len(), 2);
}

#[test]
fn encode_stats_reports_without_sending_anything() {
    let (ok, stdout, stderr) = run(&[
        "encode-stats",
        "--pattern",
        "gradient",
        "-n",
        "4",
        "--per-frame",
        "--quality",
    ]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("4 frames, budget 1464 B, profile full"), "{stdout}");
    assert!(stdout.contains("0 over budget"), "{stdout}");
    assert!(stdout.contains("dE"), "{stdout}");
    // Four per-frame lines plus the header.
    assert!(
        stdout.lines().filter(|l| l.contains("bc1-dual") || l.contains("pal")).count() >= 4,
        "{stdout}"
    );
}

#[test]
fn encode_stats_reads_raw_frames_from_stdin() {
    let mut child = screeny()
        .args(["encode-stats", "--budget", "1376"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let mut stdin = child.stdin.take().unwrap();
        for i in 0..3u8 {
            stdin
                .write_all(screeny::Pattern::Sweep.frame(u64::from(i)).as_bytes())
                .unwrap();
        }
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("3 frames, budget 1376 B"), "{stdout}");
    assert!(stdout.contains("0 over budget"), "{stdout}");
}

#[test]
fn pattern_list_names_every_pattern() {
    let (ok, stdout, _) = run(&["pattern", "--list"]);
    assert!(ok);
    for p in screeny::Pattern::ALL {
        assert!(stdout.contains(p.name()), "{} missing\n{stdout}", p.name());
    }
}

#[test]
fn an_unknown_pattern_lists_the_known_ones() {
    let (ok, _out, err) = run(&["pattern", "nope", "--addr", "127.0.0.1:1"]);
    assert!(!ok);
    assert!(err.contains("unknown pattern") && err.contains("bars"), "{err}");
}

/// A device that is not there must fail quickly and say something useful,
/// never hang.
///
/// On loopback a closed port answers with ICMP, so this is the refused path;
/// the silent path, where the Local Network hint applies (spec 9.3, card
/// 015), is covered by `loopback::control_requests_time_out`.
#[test]
fn an_unreachable_device_fails_with_a_hint() {
    let started = std::time::Instant::now();
    let (ok, _out, err) = run(&["info", "--addr", "127.0.0.1:1"]);
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(5), "took too long");
    assert!(err.contains("nothing is listening"), "{err}");
    assert!(err.contains("screeny discover"), "{err}");
}

#[test]
fn help_lists_every_subcommand() {
    let (ok, stdout, _) = run(&["--help"]);
    assert!(ok);
    for cmd in [
        "discover",
        "info",
        "stats",
        "brightness",
        "identify",
        "reboot",
        "ping",
        "pattern",
        "pipe",
        "encode-stats",
    ] {
        assert!(stdout.contains(cmd), "{cmd} missing from --help\n{stdout}");
    }
}
