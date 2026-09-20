//! Spec section 6, in process: the opcodes whose effect the wire cannot see,
//! and the two the conformance suite must never send at a real device.
//!
//! The wire-level half of this file - the `ERR_UNKNOWN_OP`, `ERR_BAD_LENGTH`
//! and `ERR_BAD_ARG` tables, `req_id == 0` silence, the `REPLY` bit, the
//! `GET_INFO` rate limit, and which port discards what - moved into
//! `screeny_probe::suite` in card 080 and runs against this simulator from
//! `tests/conformance.rs`, and against the firmware on the bench. What is
//! left here needs `SimHandle`: the mDNS TXT bytes, the event stream, and the
//! fact that a `SET_WIFI` event carries no PSK field at all.

mod common;

use std::time::Duration;

use common::*;
use screeny_proto::control::{op, IdleMode, Reply, Request, SetWifi, REBOOT_MAGIC};
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
fn the_reboot_magic_that_does_work_is_accepted() {
    // The conformance suite checks the *guard* - `REBOOT` without the magic
    // word is `ERR_BAD_ARG` - because that is safe to ask a real panel. The
    // word that actually works can only be sent to something that will not
    // act on it, so it is asked here and nowhere else.
    let dev = device();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let mut cursor = sim.event_cursor();

    ctrl.send_raw(&raw_control(
        op::REBOOT,
        0,
        903,
        &REBOOT_MAGIC.to_le_bytes(),
    ));
    let reply = ctrl.recv(T).expect("a reply");
    assert_eq!(error_code(&reply), None);
    assert!(matches!(parse_reply(&reply).1, Ok(Reply::Reboot)));
    assert!(
        sim.wait_for(&mut cursor, T, |e| matches!(e, Event::Reboot))
            .is_some(),
        "and it was logged"
    );
    assert!(
        ctrl.call(&Request::Ping, 1).is_some(),
        "the simulator does not act on it"
    );
}

#[test]
fn a_request_with_no_reply_wanted_is_still_carried_out() {
    // Section 6.1's silence half - including that it covers error replies -
    // is a conformance rule. What only the simulator can show is the other
    // half: the request itself still happened, and the event stream records
    // that nothing was sent back.
    let dev = device();
    let sim = dev.handle();
    let ctrl = Ctrl::new(dev.control_addr());
    let mut cursor = sim.event_cursor();

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

// ---------------------------------------------------------------------------

/// A `CONTROL` datagram with an arbitrary opcode and body, which
/// `Request::write` will not build for us. The conformance suite's
/// `control_dgram` is the same three lines for the same reason.
fn raw_control(op: u8, flags: u8, req_id: u16, body: &[u8]) -> Vec<u8> {
    screeny_probe::suite::control_dgram(op, flags, req_id, body)
}
