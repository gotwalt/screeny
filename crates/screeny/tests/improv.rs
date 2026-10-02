//! Card 362: the `screeny improv` bench client against the simulator's Improv
//! socket (the same `Session` the firmware runs), and the client's own framing.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};

use screeny::improv::{drain_frames, query, render, request_frame, Ask, Frame, Port};
use screeny_provision::improv::{encode_wifi_settings, ErrorCode, State};
use screeny_sim::{Config, SimDevice, WifiOutcome, WifiTiming};

const T: Duration = Duration::from_secs(5);
const SSID: &str = "Example-Wifi1";
const PSK: &str = "password9";

fn sim(outcome: WifiOutcome) -> SimDevice {
    SimDevice::start(Config {
        start_in_portal: true,
        wifi_outcome: outcome,
        wifi_join_ms: 40,
        wifi_timing: WifiTiming {
            join_attempt_ms: 300,
            join_attempts: 3,
            trial_attempts: 3,
            ap_grace_ms: 200,
            portal_retry_ms: 1_000,
            link_down_ms: 200,
            connected_screen_ms: 1_000,
        },
        improv_port: Some(0),
        ..Config::for_test()
    })
    .expect("bind loopback")
}

fn connect(dev: &SimDevice) -> TcpStream {
    let s = TcpStream::connect(dev.improv_addr().expect("improv on")).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    s
}

/// Read frames until `done` says enough, or time runs out. Also returns the raw
/// bytes, so a test can prove the password never came back.
fn collect(s: &mut TcpStream, mut done: impl FnMut(&[Frame]) -> bool) -> (Vec<Frame>, Vec<u8>) {
    let (mut frames, mut raw, mut buf) = (Vec::new(), Vec::new(), Vec::new());
    let start = Instant::now();
    let mut tmp = [0u8; 256];
    while start.elapsed() < T && !done(&frames) {
        match s.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                buf.extend_from_slice(&tmp[..n]);
                frames.extend(drain_frames(&mut buf));
            }
            Err(_) => {}
        }
    }
    (frames, raw)
}

#[test]
fn info_state_and_scan_through_the_client() {
    let dev = sim(WifiOutcome::Ok);
    let mut s = connect(&dev);

    let f = query(&mut s, Ask::Info, T).unwrap();
    assert_eq!(
        render(Ask::Info, &f),
        ["firmware: screeny-sim", "version: sim", "chip: sim", "name: screeny-sim"]
    );
    let f = query(&mut s, Ask::State, T).unwrap();
    assert_eq!(render(Ask::State, &f), ["state: Authorized"]);
    let f = query(&mut s, Ask::Scan, T).unwrap();
    assert_eq!(render(Ask::Scan, &f), ["scan complete"]);
}

#[test]
fn a_provision_over_the_socket_ends_provisioned_with_the_url_and_no_password_echo() {
    let dev = sim(WifiOutcome::Ok);
    let mut s = connect(&dev);
    let mut f = [0u8; 128];
    let n = encode_wifi_settings(&mut f, SSID.as_bytes(), PSK.as_bytes()).unwrap();
    s.write_all(&f[..n]).unwrap();

    let (frames, raw) = collect(&mut s, |fr| {
        fr.iter().any(|f| matches!(f, Frame::Result { command: 1, .. }))
    });
    assert!(frames.contains(&Frame::State(State::Provisioning)), "{frames:?}");
    assert!(frames.contains(&Frame::State(State::Provisioned)), "{frames:?}");
    let url = frames.iter().find_map(|f| match f {
        Frame::Result { command: 1, strings } => strings.first().cloned(),
        _ => None,
    });
    assert!(url.expect("a url").starts_with("http://"), "{frames:?}");
    assert!(!raw.windows(PSK.len()).any(|w| w == PSK.as_bytes()), "the password came back");

    // And the model saw the SSID, committed once, through the shared path.
    assert_eq!(dev.handle().stored_ssid().as_deref(), Some(SSID));
    assert_eq!(dev.handle().wifi_commits(), 1);
    // State now says Provisioned with the same URL.
    let f = query(&mut s, Ask::State, T).unwrap();
    let lines = render(Ask::State, &f);
    assert_eq!(lines[0], "state: Provisioned");
    assert!(lines[1].starts_with("url: http://"), "{lines:?}");
}

#[test]
fn a_failed_join_reports_unable_to_connect_and_commits_nothing() {
    let dev = sim(WifiOutcome::Fail);
    let mut s = connect(&dev);
    let mut f = [0u8; 128];
    let n = encode_wifi_settings(&mut f, SSID.as_bytes(), PSK.as_bytes()).unwrap();
    s.write_all(&f[..n]).unwrap();

    let (frames, _) = collect(&mut s, |fr| {
        fr.contains(&Frame::Error(ErrorCode::UnableToConnect))
            && fr.last() == Some(&Frame::State(State::Authorized))
    });
    assert!(frames.contains(&Frame::Error(ErrorCode::UnableToConnect)), "{frames:?}");
    assert_eq!(frames.last(), Some(&Frame::State(State::Authorized)));
    assert_eq!(dev.handle().wifi_commits(), 0);
}

#[test]
fn log_text_around_frames_does_not_confuse_the_device() {
    let dev = sim(WifiOutcome::Ok);
    let mut s = connect(&dev);
    s.write_all(b"I (1234) noise IMPRO garbage\r\n").unwrap();
    s.write_all(&request_frame(Ask::Info)).unwrap();
    let (frames, _) = collect(&mut s, |fr| fr.iter().any(|f| matches!(f, Frame::Result { command: 3, .. })));
    assert!(frames.iter().any(|f| matches!(f, Frame::Result { command: 3, .. })), "{frames:?}");
}

// ---- the client on its own ------------------------------------------------

#[test]
fn request_frames_are_the_spec_goldens() {
    assert_eq!(hex(&request_frame(Ask::State)), "494d50524f560103020200e5");
    assert_eq!(hex(&request_frame(Ask::Info)), "494d50524f560103020300e6");
    assert_eq!(hex(&request_frame(Ask::Scan)), "494d50524f560103020400e7");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A port that answers nothing until it has been asked twice, to exercise the
/// resend, and that hands bytes back in odd-sized pieces.
struct Slow {
    writes: usize,
    out: Vec<u8>,
}

impl Read for Slow {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.writes < 2 || self.out.is_empty() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        let n = self.out.len().min(buf.len()).min(5);
        buf[..n].copy_from_slice(&self.out[..n]);
        self.out.drain(..n);
        Ok(n)
    }
}
impl Write for Slow {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.writes += 1;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn query_resends_until_answered_and_reassembles_split_frames() {
    // 494d.. state Authorized, preceded by log text.
    let mut out = b"ets Jun  8 2016 rst:0x1 (POWERON_RESET)\r\n".to_vec();
    out.extend((0..hex_len("494d50524f5601010102e2")).map(|i| {
        u8::from_str_radix(&"494d50524f5601010102e2"[2 * i..2 * i + 2], 16).unwrap()
    }));
    let mut p = Slow { writes: 0, out };
    let port: &mut dyn Port = &mut p;
    let f = query(port, Ask::State, Duration::from_secs(5)).unwrap();
    assert_eq!(f, [Frame::State(State::Authorized)]);
    assert!(p.writes >= 2);
}

fn hex_len(s: &str) -> usize {
    s.len() / 2
}

#[test]
fn query_times_out_on_a_silent_port() {
    let mut p = Slow { writes: 0, out: Vec::new() };
    let r = query(&mut p, Ask::Info, Duration::from_millis(300));
    assert!(matches!(r, Err(screeny::improv::ImprovError::Timeout(_))));
}

#[test]
fn the_binary_talks_to_the_sim_and_offers_no_way_to_send_a_password() {
    let dev = sim(WifiOutcome::Ok);
    let addr = dev.improv_addr().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_screeny"))
        .args(["improv", "--port", &format!("tcp:{addr}"), "--wait", "5", "state"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "state: Authorized");

    // The subcommand has no credential argument at all.
    let help = Command::new(env!("CARGO_BIN_EXE_screeny")).args(["improv", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout).to_lowercase();
    assert!(!help.contains("password") || help.contains("cannot send credentials"), "{help}");
    for flag in ["--ssid", "--psk", "--password", "--pass"] {
        let r = Command::new(env!("CARGO_BIN_EXE_screeny"))
            .args(["improv", "--port", "tcp:127.0.0.1:1", flag, "x"])
            .output()
            .unwrap();
        assert!(!r.status.success(), "{flag} must not exist");
    }
}
