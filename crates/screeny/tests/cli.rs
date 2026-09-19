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

fn screeny() -> Command {
    Command::new(env!("CARGO_BIN_EXE_screeny"))
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
