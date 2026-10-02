//! Card 362: the Improv Serial parser, encoders and session.
//!
//! The golden frames were built with an independent calculation (a
//! three-line script summing the bytes per the spec and `improv.cpp`), not with
//! the code under test. The spec's own example, `MyWirelessAP` /
//! `mysecurepassword`, gives `01 1E 0C ... 10 ...` for the RPC data; the
//! checksum is the sum rule.

use screeny_provision::improv::*;
use screeny_provision::machine::{Config, Event, FailReason, TrialOutcome};
use screeny_provision::{Provisioner, State as MState};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

const GET_STATE: &str = "494d50524f560103020200e5";
const GET_INFO: &str = "494d50524f560103020300e6";
const SCAN: &str = "494d50524f560103020400e7";
const WIFI: &str =
    "494d50524f56010320011e0c4d79576972656c6573734150106d7973656375726570617373776f7264c1";

fn feed(p: &mut Parser, bytes: &[u8]) -> Vec<String> {
    let mut v = Vec::new();
    for &b in bytes {
        if let Some(x) = p.push(b) {
            v.push(format!("{x:?}"));
        }
    }
    v
}

#[test]
fn every_command_parses() {
    let mut p = Parser::new();
    assert_eq!(feed(&mut p, &hex(GET_STATE)), ["Rpc(GetCurrentState)"]);
    assert_eq!(feed(&mut p, &hex(GET_INFO)), ["Rpc(GetDeviceInfo)"]);
    assert_eq!(feed(&mut p, &hex(SCAN)), ["Rpc(GetWifiNetworks)"]);
    // Hostname get, device name get, network state: all "unknown" here.
    for c in [5u8, 6, 7] {
        let mut out = [0u8; 32];
        let n = encode_rpc_command(&mut out, c, &[]).unwrap();
        assert_eq!(feed(&mut p, &out[..n]), [format!("Rpc(Unknown({c}))")]);
    }
}

#[test]
fn wifi_settings_golden_and_debug_never_prints_the_password() {
    let mut p = Parser::new();
    let mut got = None;
    for &b in &hex(WIFI) {
        if let Some(Parsed::Rpc(Rpc::WifiSettings { ssid, password })) = p.push(b) {
            got = Some((ssid.to_vec(), password.to_vec()));
        }
    }
    assert_eq!(got, Some((b"MyWirelessAP".to_vec(), b"mysecurepassword".to_vec())));

    // Debug of the borrowed forms: lengths only.
    let mut p = Parser::new();
    for &b in &hex(WIFI)[..hex(WIFI).len() - 1] {
        assert!(p.push(b).is_none());
    }
    let last = *hex(WIFI).last().unwrap();
    let r = p.push(last).unwrap();
    let text = format!("{r:?}");
    assert!(text.contains("psk_len: 16") && !text.contains("mysecure"), "{text}");
    let s = Submit { ssid: b"x", password: b"mysecurepassword" };
    assert!(!format!("{s:?}").contains("mysecure"));
}

#[test]
fn encoder_matches_the_golden_wifi_frame() {
    let mut out = [0u8; 128];
    let n = encode_wifi_settings(&mut out, b"MyWirelessAP", b"mysecurepassword").unwrap();
    assert_eq!(&out[..n], &hex(WIFI)[..]);
}

#[test]
fn resyncs_on_log_text_and_partial_headers() {
    let mut p = Parser::new();
    let mut stream = Vec::new();
    stream.extend_from_slice(b"I (123) wifi: IMPR IMPROV\x02 IIMPROV");
    stream.extend_from_slice(&hex(GET_STATE));
    stream.extend_from_slice(b"\r\nINFO screeny: started\r\n");
    stream.extend_from_slice(&hex(SCAN));
    assert_eq!(feed(&mut p, &stream), ["Rpc(GetCurrentState)", "Rpc(GetWifiNetworks)"]);
}

#[test]
fn a_frame_started_inside_another_header_is_found() {
    // "IMPRO" then a real frame: the 'I' that restarts the match must not be lost.
    let mut p = Parser::new();
    let mut stream = b"IMPRO".to_vec();
    stream.extend_from_slice(&hex(GET_INFO));
    assert_eq!(feed(&mut p, &stream), ["Rpc(GetDeviceInfo)"]);
}

#[test]
fn bad_checksum_is_invalid_and_the_stream_recovers() {
    let mut f = hex(GET_STATE);
    *f.last_mut().unwrap() ^= 0x40;
    let mut p = Parser::new();
    assert_eq!(feed(&mut p, &f), ["Invalid"]);
    assert_eq!(feed(&mut p, &hex(GET_STATE)), ["Rpc(GetCurrentState)"]);
}

#[test]
fn wrong_version_is_not_a_frame() {
    let mut f = hex(GET_STATE);
    f[6] = 2;
    let mut p = Parser::new();
    assert!(feed(&mut p, &f).is_empty());
    assert_eq!(feed(&mut p, &hex(GET_STATE)), ["Rpc(GetCurrentState)"]);
}

#[test]
fn inconsistent_inner_lengths_are_invalid() {
    // RPC says 5 payload bytes but the frame carries 0.
    let mut out = [0u8; 32];
    let n = encode_rpc_command(&mut out, cmd::GET_CURRENT_STATE, &[]).unwrap();
    out[10] = 5; // inner length
    out[n - 1] = checksum(&out[..n - 1]);
    let mut p = Parser::new();
    assert_eq!(feed(&mut p, &out[..n]), ["Invalid"]);

    // WIFI_SETTINGS whose password length overruns.
    let mut out = [0u8; 64];
    let n = encode_rpc_command(&mut out, cmd::WIFI_SETTINGS, &[1, b'a', 9, b'p']).unwrap();
    assert_eq!(feed(&mut p, &out[..n]), ["Invalid"]);
    // And one with no password length byte at all.
    let n = encode_rpc_command(&mut out, cmd::WIFI_SETTINGS, &[1, b'a']).unwrap();
    assert_eq!(feed(&mut p, &out[..n]), ["Invalid"]);
}

#[test]
fn oversized_frame_is_refused_once_and_skipped() {
    // A hostname set with a 200-byte name: longer than we keep.
    let payload = [b'h'; 200];
    let mut out = [0u8; 256];
    let n = encode_rpc_command(&mut out, 5, &payload).unwrap();
    let mut p = Parser::new();
    assert_eq!(feed(&mut p, &out[..n]), ["Invalid"]);
    // Nothing inside the skipped tail is read as a header, and what follows works.
    assert_eq!(feed(&mut p, &hex(GET_STATE)), ["Rpc(GetCurrentState)"]);
}

#[test]
fn stray_non_rpc_frames_are_ignored() {
    let mut p = Parser::new();
    assert_eq!(feed(&mut p, &hex("494d50524f5601010102e2")), ["Ignored(1)"]);
}

#[test]
fn the_buffer_is_zeroed_after_a_frame() {
    let mut p = Parser::new();
    feed(&mut p, &hex(WIFI));
    assert!(!p.is_clear());
    p.wipe();
    assert!(p.is_clear());
    // And the next byte after a frame wipes it too (the frame is Done).
    let mut p = Parser::new();
    feed(&mut p, &hex(WIFI));
    p.push(b'x');
    assert!(p.is_clear());
}

#[test]
fn encoders_match_goldens() {
    let mut out = [0u8; REPLY_MAX];
    let info = DeviceInfo { firmware: "screeny-fw", version: "0.10.0", chip: "ESP32", name: "screeny-c0ffee" };
    let cases: [(Reply, &str); 6] = [
        (Reply::State(State::Authorized), "494d50524f5601010102e2"),
        (Reply::State(State::Provisioned), "494d50524f5601010104e4"),
        (Reply::Error(ErrorCode::InvalidRpc), "494d50524f5601020101e2"),
        (Reply::ScanEnd, "494d50524f560104020400e8"),
        (
            Reply::Url([192, 168, 1, 50], cmd::WIFI_SETTINGS),
            "494d50524f56010417011514687474703a2f2f3139322e3136382e312e35302f05",
        ),
        (
            Reply::DeviceInfo,
            "494d50524f5601042903270a73637265656e792d667706302e31302e300545535033320e73637265656e792d63306666656514",
        ),
    ];
    for (r, want) in cases {
        let n = r.encode(&info, &mut out).unwrap();
        assert_eq!(&out[..n], &hex(want)[..], "{r:?}");
    }
}

#[test]
fn encoders_refuse_what_does_not_fit() {
    let mut small = [0u8; 8];
    assert_eq!(encode_state(&mut small, State::Authorized), Err(EncodeError));
    let mut out = [0u8; 300];
    let big = [b'a'; 256];
    assert_eq!(encode_rpc_result(&mut out, 1, &[&big]), Err(EncodeError));
    let half = [b'a'; 130];
    assert_eq!(encode_rpc_result(&mut out, 1, &[&half, &half]), Err(EncodeError));
}

#[test]
fn url_text_every_digit_count() {
    let mut b = [0u8; URL_MAX];
    let n = url_text([0, 9, 10, 255], &mut b);
    assert_eq!(&b[..n], b"http://0.9.10.255/");
    let n = url_text([255, 255, 255, 255], &mut b);
    assert_eq!(n, URL_MAX);
}

#[test]
fn a_client_can_read_what_the_device_writes() {
    let f = hex("494d50524f56010417011514687474703a2f2f3139322e3136382e312e35302f05");
    // Strip header (9) and checksum (1).
    let mut strings = Vec::new();
    let c = for_each_result_string(&f[9..f.len() - 1], |s| strings.push(s.to_vec())).unwrap();
    assert_eq!(c, 1);
    assert_eq!(strings, [b"http://192.168.1.50/".to_vec()]);
    assert_eq!(for_each_result_string(&[1, 5, 1], |_| {}), None);
}

// ---- the session, against the real Provisioner ----------------------------

const SSID: &str = "Example-Wifi1";

fn machine(has_stored: bool) -> Provisioner {
    let mut p = Provisioner::new(&Config { has_stored, ..Config::default() });
    let _ = p.step(Event::Boot, 0);
    p
}

fn parse<'a>(p: &'a mut Parser, f: &[u8]) -> Parsed<'a> {
    for &b in &f[..f.len() - 1] {
        assert!(p.push(b).is_none());
    }
    p.push(f[f.len() - 1]).unwrap()
}

fn drain(r: &mut Replies) -> Vec<Reply> {
    let v = r.iter().copied().collect();
    r.clear();
    v
}

#[test]
fn state_and_info_requests() {
    let m = machine(false);
    let mut s = Session::new();
    let mut p = Parser::new();
    let mut out = Replies::new();

    let o = Observed::from_machine(&m);
    assert_eq!(m.state(), MState::Portal);
    assert!(s.handle(parse(&mut p, &hex(GET_STATE)), &o, 0, &mut out).is_none());
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::State(State::Authorized)]);

    assert!(s.handle(parse(&mut p, &hex(GET_INFO)), &o, 0, &mut out).is_none());
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::DeviceInfo]);

    // Scan: an empty terminator, never an error, so the client falls back to typing.
    assert!(s.handle(parse(&mut p, &hex(SCAN)), &o, 0, &mut out).is_none());
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::ScanEnd]);

    // Unknown command.
    let mut f = [0u8; 16];
    let n = encode_rpc_command(&mut f, 7, &[]).unwrap();
    assert!(s.handle(parse(&mut p, &f[..n]), &o, 0, &mut out).is_none());
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::Error(ErrorCode::UnknownRpc)]);

    // Garbage.
    assert!(s.handle(Parsed::Invalid, &o, 0, &mut out).is_none());
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::InvalidRpc)]);
}

#[test]
fn already_online_reports_provisioned_with_the_url() {
    let mut m = machine(true);
    let _ = m.step(Event::Joined { ip: [192, 168, 1, 50] }, 1_000);
    assert_eq!(m.state(), MState::Online);
    let o = Observed::from_machine(&m);
    let mut s = Session::new();
    let mut p = Parser::new();
    let mut out = Replies::new();
    s.handle(parse(&mut p, &hex(GET_STATE)), &o, 0, &mut out);
    assert_eq!(
        drain(&mut out),
        [
            Reply::Error(ErrorCode::None),
            Reply::State(State::Provisioned),
            Reply::Url([192, 168, 1, 50], cmd::GET_CURRENT_STATE)
        ]
    );
}

fn submit(m: &mut Provisioner, s: &mut Session, now: u32, out: &mut Replies) {
    let mut p = Parser::new();
    let mut f = [0u8; 128];
    let n = encode_wifi_settings(&mut f, SSID.as_bytes(), b"password9").unwrap();
    let o = Observed::from_machine(m);
    let sub = s.handle(parse(&mut p, &f[..n]), &o, now, out).expect("accepted");
    assert_eq!((sub.ssid, sub.password), (SSID.as_bytes(), &b"password9"[..]));
    // The caller's half: the SSID alone goes to the machine.
    let _ = m.step(Event::CredentialsPosted { ssid: SSID }, now);
}

#[test]
fn a_successful_provision_from_the_portal() {
    let mut m = machine(false);
    let mut s = Session::new();
    let mut out = Replies::new();
    submit(&mut m, &mut s, 100, &mut out);
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::State(State::Provisioning)]);
    assert_eq!(m.state(), MState::Trial);

    s.poll(&Observed::from_machine(&m), 200, &mut out);
    assert!(out.is_empty(), "still trying: nothing to say");

    let _ = m.step(Event::Joined { ip: [10, 0, 0, 7] }, 4_000);
    s.poll(&Observed::from_machine(&m), 4_100, &mut out);
    assert_eq!(
        drain(&mut out),
        [Reply::State(State::Provisioned), Reply::Url([10, 0, 0, 7], cmd::WIFI_SETTINGS)]
    );
    assert!(!s.busy());
}

#[test]
fn a_wrong_password_is_unable_to_connect_and_back_to_authorized() {
    let mut m = machine(false);
    let mut s = Session::new();
    let mut out = Replies::new();
    submit(&mut m, &mut s, 100, &mut out);
    out.clear();
    s.poll(&Observed::from_machine(&m), 200, &mut out);
    // AuthError is not retried: the trial fails at once.
    let _ = m.step(Event::JoinFailed { reason: FailReason::AuthError }, 3_000);
    assert!(matches!(m.trial().unwrap().outcome, TrialOutcome::Failed(_)));
    s.poll(&Observed::from_machine(&m), 3_100, &mut out);
    assert_eq!(
        drain(&mut out),
        [Reply::Error(ErrorCode::UnableToConnect), Reply::State(State::Authorized)]
    );
    assert!(!s.busy());
}

#[test]
fn a_stale_outcome_from_an_earlier_trial_is_not_read_as_this_one() {
    let mut m = machine(false);
    let mut s = Session::new();
    let mut out = Replies::new();
    // First attempt fails.
    submit(&mut m, &mut s, 100, &mut out);
    s.poll(&Observed::from_machine(&m), 200, &mut out);
    let _ = m.step(Event::JoinFailed { reason: FailReason::AuthError }, 3_000);
    s.poll(&Observed::from_machine(&m), 3_100, &mut out);
    out.clear();
    // Second attempt: until the machine is in Trial again, the old Failed
    // outcome must not be reported.
    let mut p = Parser::new();
    let mut f = [0u8; 128];
    let n = encode_wifi_settings(&mut f, SSID.as_bytes(), b"password9").unwrap();
    let o = Observed::from_machine(&m);
    assert!(s.handle(parse(&mut p, &f[..n]), &o, 10_000, &mut out).is_some());
    out.clear();
    s.poll(&Observed::from_machine(&m), 10_100, &mut out); // machine has not stepped yet
    assert!(out.is_empty());
}

#[test]
fn a_post_that_never_reaches_the_machine_times_out() {
    let m = machine(false);
    let mut s = Session::new();
    let mut out = Replies::new();
    let mut p = Parser::new();
    let mut f = [0u8; 128];
    let n = encode_wifi_settings(&mut f, SSID.as_bytes(), b"password9").unwrap();
    s.handle(parse(&mut p, &f[..n]), &Observed::from_machine(&m), 0, &mut out);
    out.clear();
    s.poll(&Observed::from_machine(&m), ENTER_TRIAL_MS + 1, &mut out);
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::Unknown), Reply::State(State::Authorized)]);
}

#[test]
fn a_second_submit_while_busy_is_refused() {
    let mut m = machine(false);
    let mut s = Session::new();
    let mut out = Replies::new();
    submit(&mut m, &mut s, 100, &mut out);
    out.clear();
    let mut p = Parser::new();
    let mut f = [0u8; 128];
    let n = encode_wifi_settings(&mut f, SSID.as_bytes(), b"password9").unwrap();
    let o = Observed::from_machine(&m);
    assert!(s.handle(parse(&mut p, &f[..n]), &o, 200, &mut out).is_none());
    assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::Error(ErrorCode::Unknown)]);
    // State requests in the meantime say Provisioning.
    s.handle(parse(&mut p, &hex(GET_STATE)), &o, 300, &mut out);
    assert!(drain(&mut out).contains(&Reply::State(State::Provisioning)));
}

#[test]
fn out_of_range_credentials_are_invalid_rpc() {
    let m = machine(false);
    let o = Observed::from_machine(&m);
    let mut s = Session::new();
    let mut out = Replies::new();
    let mut p = Parser::new();
    let mut f = [0u8; 128];
    for (ssid, pw) in [(&b""[..], &b"password9"[..]), (&[b'a'; 33][..], &b"x"[..]), (&b"ok"[..], &[b'p'; 65][..])] {
        let n = encode_wifi_settings(&mut f, ssid, pw).unwrap();
        // 33 + 65 bytes still fit the parser; the session refuses them.
        assert!(s.handle(parse(&mut p, &f[..n]), &o, 0, &mut out).is_none());
        assert_eq!(drain(&mut out), [Reply::Error(ErrorCode::None), Reply::Error(ErrorCode::InvalidRpc)]);
    }
    assert!(!s.busy());
}

#[test]
fn the_session_and_replies_carry_no_password() {
    // Structural, as in the machine: nothing the session owns or emits has a
    // field for one. Spot-check through Debug of everything.
    let mut m = machine(false);
    let mut s = Session::new();
    let mut out = Replies::new();
    submit(&mut m, &mut s, 100, &mut out);
    let all = format!("{s:?} {out:?} {:?} {:?}", Observed::from_machine(&m), m);
    assert!(!all.contains("password9"), "{all}");
}
