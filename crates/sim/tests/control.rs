//! Spec section 6: every opcode, every error code, and the rules about when
//! the device answers at all.

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::control::{op, ErrorCode, IdleMode, Reply, Request, SetWifi, REBOOT_MAGIC};
use screeny_proto::dec::codec;
use screeny_proto::txt::DeviceInfo;
use screeny_proto::{dec, F_KEY};
use screeny_sim::{Config, Event, SimDevice, Timing};

const T: Duration = Duration::from_secs(2);

/// Section 5.5's GET_INFO rate limit is off here so that a test can ask more
/// than once a second. It has a test of its own below.
fn device() -> SimDevice {
    SimDevice::start(Config {
        timing: Timing {
            info_min_interval_ms: 0,
            ..Timing::SPEC
        },
        ..Config::for_test()
    })
    .expect("bind loopback")
}

#[test]
fn every_opcode_is_implemented() {
    let dev = device();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let mut cursor = sim.event_cursor();

    // PING
    let (pkt, body) = {
        let d = ctrl.call(&Request::Ping, 1).expect("PING");
        let (p, b) = parse_reply(&d);
        (p.op, b.map(|b| matches!(b, Reply::Ping { .. })))
    };
    assert_eq!(pkt, op::PING);
    assert_eq!(body, Ok(true));

    // GET_INFO: section 6.6 says the body is the TXT record, and section 5.2
    // says which keys have to be in it.
    let d = ctrl.call(&Request::GetInfo, 2).expect("GET_INFO");
    let (p, b) = parse_reply(&d);
    assert_eq!(p.op, op::GET_INFO);
    let Ok(Reply::Info(txt)) = b else {
        panic!("expected Info")
    };
    let info = DeviceInfo::parse(txt).expect("a parseable TXT record");
    assert_eq!(info.txtvers, 1);
    assert_eq!(info.proto, "1");
    assert_eq!((info.w, info.h), (64, 32));
    assert_eq!(info.codecs, dec::CODECS_TXT);
    assert_eq!(info.mtu, screeny_proto::MAX_PIXEL_PAYLOAD as u16);
    assert_eq!(
        info.ctrl,
        dev.control_addr().port(),
        "ctrl= must be the port a sender can actually reach"
    );
    for id in dec::SUPPORTED_CODECS {
        assert!(info.supports(id), "a v1 device must decode {id:#04x}");
    }
    assert_eq!(
        txt,
        &sim.info_bytes()[..],
        "the mDNS TXT record is the same bytes"
    );

    // TELEMETRY
    let d = ctrl.call(&Request::Telemetry, 3).expect("TELEMETRY");
    let (p, b) = parse_reply(&d);
    assert_eq!(p.op, op::TELEMETRY);
    assert_eq!(p.body.len(), 48, "section 6.7: 48 bytes");
    assert!(matches!(b, Ok(Reply::Telemetry(_))));

    // SET_BRIGHTNESS
    let d = ctrl.call(&Request::SetBrightness(77), 4).unwrap();
    assert!(matches!(
        parse_reply(&d).1,
        Ok(Reply::Brightness { applied: 77 })
    ));
    assert_eq!(sim.snapshot().brightness, 77);

    // IDENTIFY
    let d = ctrl
        .call(&Request::Identify { duration_ms: 50 }, 5)
        .unwrap();
    assert!(matches!(parse_reply(&d).1, Ok(Reply::Identify)));

    // SET_IDLE
    for mode in [
        IdleMode::Black,
        IdleMode::Dim,
        IdleMode::HoldForever,
        IdleMode::Status,
    ] {
        let d = ctrl.call(&Request::SetIdle(mode), 6).unwrap();
        assert!(matches!(parse_reply(&d).1, Ok(Reply::Idle { mode: m }) if m == mode.as_u8()));
        assert_eq!(sim.snapshot().idle_mode, mode);
    }

    // RESET_STATS
    let d = ctrl.call(&Request::ResetStats, 7).unwrap();
    assert!(matches!(parse_reply(&d).1, Ok(Reply::ResetStats)));

    // RELEASE
    let d = ctrl.call(&Request::Release, 8).unwrap();
    assert!(matches!(parse_reply(&d).1, Ok(Reply::Release)));

    // SET_NAME, and the TXT record follows it.
    let d = ctrl.call(&Request::SetName("Desk panel"), 9).unwrap();
    assert!(matches!(parse_reply(&d).1, Ok(Reply::SetName)));
    assert_eq!(sim.snapshot().name, "Desk panel");
    let d = ctrl.call(&Request::GetInfo, 10).unwrap();
    let (_, b) = parse_reply(&d);
    let Ok(Reply::Info(txt)) = b else { panic!() };
    assert_eq!(
        DeviceInfo::parse(txt).unwrap().name,
        "Desk panel",
        "SET_NAME changes what GET_INFO says"
    );

    // GET_WIFI never carries the PSK: section 8.4's invariant.
    let d = ctrl.call(&Request::GetWifi, 11).unwrap();
    let (_, b) = parse_reply(&d);
    let Ok(Reply::Wifi { ssid, state }) = b else {
        panic!("expected Wifi")
    };
    assert!(!ssid.is_empty());
    assert_eq!(state, screeny_proto::control::wifi_state::CONNECTED);

    // SET_WIFI is accepted, logged, and not acted on.
    let d = ctrl
        .call(
            &Request::SetWifi(SetWifi {
                ssid: "Example-Wifi1",
                psk: "password9",
                persist: true,
            }),
            12,
        )
        .unwrap();
    assert!(matches!(parse_reply(&d).1, Ok(Reply::SetWifi)));
    let ev = sim
        .wait_for(&mut cursor, T, |e| matches!(e, Event::SetWifi { .. }))
        .expect("logged");
    match ev {
        Event::SetWifi { ssid, persist } => {
            assert_eq!(ssid, "Example-Wifi1");
            assert!(persist);
        }
        other => panic!("{other:?}"),
    }
    // The event carries no PSK field at all, which is the only way to be sure
    // it is never logged.

    // REBOOT is accepted, logged, and not acted on.
    let d = ctrl.call(&Request::Reboot, 13).unwrap();
    assert!(matches!(parse_reply(&d).1, Ok(Reply::Reboot)));
    assert!(sim
        .wait_for(&mut cursor, T, |e| matches!(e, Event::Reboot))
        .is_some());
    assert!(
        ctrl.call(&Request::Ping, 14).is_some(),
        "and the device is still there"
    );
}

#[test]
fn brightness_is_clamped_to_the_cap_and_the_reply_says_so() {
    // Section 6.3: "applied in the reply is the value actually in effect,
    // which is how a sender learns the cap".
    let dev = SimDevice::start(Config {
        brightness_cap: 120,
        ..Config::for_test()
    })
    .unwrap();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());

    let d = ctrl.call(&Request::SetBrightness(255), 1).unwrap();
    assert!(matches!(
        parse_reply(&d).1,
        Ok(Reply::Brightness { applied: 120 })
    ));
    assert_eq!(sim.snapshot().brightness, 120);
    assert_eq!(sim.telemetry().brightness, 120);

    let d = ctrl.call(&Request::SetBrightness(30), 2).unwrap();
    assert!(matches!(
        parse_reply(&d).1,
        Ok(Reply::Brightness { applied: 30 })
    ));
}

#[test]
fn brightness_changes_what_the_panel_shows_and_not_what_was_decoded() {
    let dev = device();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let mut tx = Sender::new(dev.frame_addr());

    let (p, e) = solid([200, 200, 200]);
    tx.send(codec::SOLID, F_KEY, &p);
    let bright = sim.wait_for_frames(1, T).unwrap();
    assert_frames_eq(&bright.decoded[..], &e, "decoded");

    ctrl.call(&Request::SetBrightness(40), 1).unwrap();
    let dim = sim.wait_until(T, |s| s.brightness == 40).unwrap();
    assert_frames_eq(&dim.decoded[..], &e, "decoded is untouched by brightness");
    assert!(
        dim.panel[0] < bright.panel[0],
        "but the panel is dimmer: {} vs {}",
        dim.panel[0],
        bright.panel[0]
    );

    ctrl.call(&Request::SetBrightness(0), 2).unwrap();
    let off = sim.wait_until(T, |s| s.brightness == 0).unwrap();
    assert!(off.panel.iter().all(|&b| b == 0), "brightness 0 is black");
    assert_frames_eq(&off.decoded[..], &e, "and still nothing to do with pixels");
}

#[test]
fn every_error_code_a_request_can_earn() {
    let dev = device();
    let ctrl = Ctrl::new(dev.control_addr());

    // ERR_UNKNOWN_OP: section 6.3 says "not silence, so a sender can probe".
    for op_id in [0x00u8, 0x0C, 0x0E, 0x40, 0x7F, 0x80, 0xFF] {
        let d = raw_control(op_id, 0, 900, &[]);
        ctrl.send_raw(&d);
        let reply = ctrl.recv(T).unwrap_or_else(|| panic!("op {op_id:#04x}"));
        assert_eq!(
            error_code(&reply),
            Some(ErrorCode::UnknownOp.as_u8()),
            "op {op_id:#04x}"
        );
        let (pkt, _) = parse_reply(&reply);
        assert_eq!(pkt.op, op_id, "an error reply echoes the request's opcode");
        assert_eq!(pkt.req_id, 900);
    }

    // ERR_BAD_LENGTH: a body that is not the opcode's size.
    let cases: &[(u8, &[u8], &str)] = &[
        (op::PING, &[0], "PING with a body"),
        (op::GET_INFO, &[1, 2], "GET_INFO with a body"),
        (op::SET_BRIGHTNESS, &[], "SET_BRIGHTNESS with no level"),
        (op::SET_BRIGHTNESS, &[1, 2], "SET_BRIGHTNESS with two"),
        (op::IDENTIFY, &[5], "IDENTIFY with one byte"),
        (op::REBOOT, &[1, 2, 3], "REBOOT with three"),
        (op::SET_NAME, &[], "SET_NAME with no length byte"),
        (op::SET_NAME, &[4, b'a'], "SET_NAME whose length overruns"),
        (op::SET_WIFI, &[1, b'x'], "SET_WIFI cut short"),
    ];
    for (o, body, what) in cases {
        ctrl.send_raw(&raw_control(*o, 0, 901, body));
        let reply = ctrl.recv(T).unwrap_or_else(|| panic!("{what}"));
        assert_eq!(
            error_code(&reply),
            Some(ErrorCode::BadLength.as_u8()),
            "{what}"
        );
    }

    // ERR_BAD_ARG: the right size, the wrong value.
    let long_name = [&[33u8][..], &[b'x'; 33][..]].concat();
    let cases: &[(u8, &[u8], &str)] = &[
        (op::SET_IDLE, &[4], "an idle mode v1 does not define"),
        (op::SET_IDLE, &[255], "and another"),
        (op::REBOOT, &[0, 0, 0, 0], "REBOOT without the magic"),
        (op::REBOOT, b"RBOX", "REBOOT mistyped"),
        (op::SET_NAME, &long_name, "a name over 32 bytes"),
        (op::SET_NAME, &[2, 0xFF, 0xFE], "a name that is not UTF-8"),
        (op::SET_WIFI, &[0, 0, 0], "SET_WIFI with an empty SSID"),
        (op::SET_WIFI, &[1, b'x', 0, 0x02], "SET_WIFI reserved flag"),
    ];
    for (o, body, what) in cases {
        ctrl.send_raw(&raw_control(*o, 0, 902, body));
        let reply = ctrl.recv(T).unwrap_or_else(|| panic!("{what}"));
        assert_eq!(
            error_code(&reply),
            Some(ErrorCode::BadArg.as_u8()),
            "{what}"
        );
    }

    // The magic that does work.
    ctrl.send_raw(&raw_control(
        op::REBOOT,
        0,
        903,
        &REBOOT_MAGIC.to_le_bytes(),
    ));
    let reply = ctrl.recv(T).unwrap();
    assert_eq!(error_code(&reply), None);
}

#[test]
fn a_request_id_of_zero_means_no_reply_wanted() {
    // Section 6.1: "the device MUST NOT reply to a request with req_id == 0
    // except where an opcode says otherwise". That covers error replies too -
    // a sender that did not ask to be told cannot be told.
    let dev = device();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let mut cursor = sim.event_cursor();

    ctrl.send(&Request::Ping, 0);
    assert!(ctrl.recv(Duration::from_millis(150)).is_none());
    ctrl.send_raw(&raw_control(0x77, 0, 0, &[]));
    assert!(ctrl.recv(Duration::from_millis(150)).is_none());

    // But the request itself was carried out.
    ctrl.send(&Request::SetBrightness(88), 0);
    let s = sim.wait_until(T, |s| s.brightness == 88).expect("applied");
    assert_eq!(s.brightness, 88);
    assert!(sim
        .wait_for(&mut cursor, T, |e| matches!(
            e,
            Event::Control {
                req_id: 0,
                replied: false,
                ..
            }
        ))
        .is_some());

    // And a normal req_id still gets an answer afterwards.
    assert!(ctrl.call(&Request::Ping, 1).is_some());
}

#[test]
fn a_malformed_header_still_gets_the_error_the_spec_names() {
    let dev = device();
    let ctrl = Ctrl::new(dev.control_addr());

    // Section 6.5: ERR_BAD_LENGTH is "len inconsistent with the datagram".
    // The op and req_id are in bytes that are certainly present, so the reply
    // can still be addressed to the right request.
    let d = [0x53, 0x12, op::PING, 0x00, 0x2A, 0x00, 0xFF, 0x00];
    ctrl.send_raw(&d);
    let reply = ctrl.recv(T).expect("ERR_BAD_LENGTH");
    assert_eq!(error_code(&reply), Some(ErrorCode::BadLength.as_u8()));
    let (pkt, _) = parse_reply(&reply);
    assert_eq!(pkt.op, op::PING);
    assert_eq!(pkt.req_id, 0x2A);

    // Section 2.2: "A device MAY reply to a CONTROL packet of an unknown
    // version with an ERR_VERSION error using version 1 framing."
    let d = [0x53, 0x92, op::GET_INFO, 0x00, 0x07, 0x00, 0x00, 0x00];
    ctrl.send_raw(&d);
    let reply = ctrl.recv(T).expect("ERR_VERSION");
    assert_eq!(error_code(&reply), Some(ErrorCode::Version.as_u8()));
    let (pkt, _) = parse_reply(&reply);
    assert_eq!(pkt.req_id, 7);
    assert_eq!(reply[1], 0x12, "and the reply is framed as version 1");

    // A wrong-version packet with req_id 0 is still silence.
    let d = [0x53, 0x92, op::GET_INFO, 0x00, 0x00, 0x00, 0x00, 0x00];
    ctrl.send_raw(&d);
    assert!(ctrl.recv(Duration::from_millis(150)).is_none());
}

#[test]
fn get_info_is_rate_limited_but_a_retry_is_not() {
    // Section 5.5 rate-limits GET_INFO to one reply per source address per
    // second, so that a broadcast probe cannot be amplified. Section 6.1 says
    // a sender with no reply retries up to three times with the *same*
    // req_id. Both have to hold at once: the limit counts new requests, and
    // a repeat of a req_id already answered is a retransmission.
    let dev = SimDevice::start(Config {
        timing: Timing {
            info_min_interval_ms: 400,
            ..Timing::SPEC
        },
        ..Config::for_test()
    })
    .unwrap();
    let ctrl = Ctrl::new(dev.control_addr());

    assert!(ctrl.call(&Request::GetInfo, 100).is_some(), "the first");
    assert!(
        ctrl.call(&Request::GetInfo, 100).is_some(),
        "a retry of the same req_id is answered"
    );
    assert!(
        ctrl.call(&Request::GetInfo, 101).is_none(),
        "a new request inside the window is not"
    );

    std::thread::sleep(Duration::from_millis(450));
    assert!(
        ctrl.call(&Request::GetInfo, 102).is_some(),
        "and the window reopens"
    );

    // Nothing else is rate-limited: PING and TELEMETRY answer every time.
    for id in 200..210u16 {
        assert!(ctrl.call(&Request::Ping, id).is_some(), "PING {id}");
        assert!(
            ctrl.call(&Request::Telemetry, id).is_some(),
            "TELEMETRY {id}"
        );
    }
}

#[test]
fn a_reply_arriving_at_the_device_is_ignored() {
    // Section 6.1 separates requests from replies by the REPLY bit. A device
    // that answered replies would answer its own, and two of them would talk
    // to each other forever.
    let dev = device();
    let ctrl = Ctrl::new(dev.control_addr());

    let d = raw_control_flags(op::PING, screeny_proto::C_REPLY, 5, &[0, 0, 0, 0]);
    ctrl.send_raw(&d);
    assert!(ctrl.recv(Duration::from_millis(150)).is_none());

    let d = raw_control_flags(
        op::PING,
        screeny_proto::C_REPLY | screeny_proto::C_ERROR,
        5,
        &[2],
    );
    ctrl.send_raw(&d);
    assert!(ctrl.recv(Duration::from_millis(150)).is_none());

    assert!(ctrl.call(&Request::Ping, 1).is_some(), "still alive");
}

#[test]
fn a_frame_on_the_control_port_is_discarded() {
    // Section 2.2: "A receiver MUST discard a FRAME received on the control
    // port and a CONTROL received on the frame port".
    let dev = device();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let (p, _) = solid([1, 2, 3]);

    let mut d = vec![0x53, 0x10, codec::SOLID, F_KEY, 1, 0, 3, 0];
    d.extend_from_slice(&p);
    ctrl.send_raw(&d);
    assert!(ctrl.recv(Duration::from_millis(150)).is_none());

    std::thread::sleep(Duration::from_millis(50));
    let s = sim.snapshot();
    assert_eq!(s.telemetry.frames_shown, 0, "it was not a frame");
    assert_eq!(
        s.telemetry.frames_rejected, 0,
        "nor is the control port's rubbish counted against the frame stream"
    );
    assert!(ctrl.call(&Request::Ping, 1).is_some());
}

#[test]
fn a_control_packet_on_the_frame_port_is_rejected() {
    let dev = device();
    let sim = dev.handle();
    let tx = Sender::new(dev.frame_addr());

    tx.send_raw(&raw_control(op::PING, 0, 1, &[]));
    let s = sim
        .wait_until(T, |s| s.telemetry.frames_rejected >= 1)
        .expect("counted in frames_rejected");
    assert_eq!(s.telemetry.frames_shown, 0);
    assert!(
        tx.recv(Duration::from_millis(150)).is_none(),
        "and certainly not answered"
    );
}

// ---------------------------------------------------------------------------

/// A `CONTROL` datagram with an arbitrary opcode and body, which
/// `Request::write` will not build for us.
fn raw_control(op: u8, flags: u8, req_id: u16, body: &[u8]) -> Vec<u8> {
    raw_control_flags(op, flags, req_id, body)
}

fn raw_control_flags(op: u8, flags: u8, req_id: u16, body: &[u8]) -> Vec<u8> {
    let mut d = vec![0x53, 0x12, op, flags];
    d.extend_from_slice(&req_id.to_le_bytes());
    d.extend_from_slice(&(body.len() as u16).to_le_bytes());
    d.extend_from_slice(body);
    d
}
