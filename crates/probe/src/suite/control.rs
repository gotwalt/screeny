//! Spec sections 5.2, 5.5, 6.1, 6.3, 6.5, 6.6 and 8.4: what the control port
//! says it is, when it answers, and which error it gives when it will not.
//!
//! Nothing here sends `SET_WIFI` or a valid `REBOOT` at a device. The
//! `SET_WIFI` *error* cases are real rules and are checked, but only against
//! loopback: a malformed `SET_WIFI` that earns `ERR_BAD_ARG` is harmless by
//! construction, and betting the bench device's association on that reading
//! of the firmware is not a bet worth making.

use std::time::Duration;

use screeny_proto::control::{op, ErrorCode, IdleMode, Request};
use screeny_proto::txt;

use super::{control_dgram, describe, verdict, Ctx, Outcome, Rule, CAP_PROBE, LOOPBACK_ONLY};
use crate::link::OwnedReply;

pub fn rules() -> Vec<Rule> {
    vec![
        Rule {
            section: "6.6",
            name: "GET_INFO is the TXT wire format, txtvers first",
            secs: 0.3,
            flags: 0,
            run: info_is_txt,
        },
        Rule {
            section: "5.2",
            name: "TXT: all five v1 codecs, 64x32, mtu 1464, a live ctrl port",
            secs: 0.3,
            flags: 0,
            run: info_contents,
        },
        Rule {
            section: "6.3",
            name: "every opcode a safe probe may use is implemented",
            secs: 1.5,
            flags: 0,
            run: every_safe_opcode,
        },
        Rule {
            section: "6.1",
            name: "a reply echoes the request's op and req_id",
            secs: 0.2,
            flags: 0,
            run: reply_echoes,
        },
        Rule {
            section: "6.1",
            name: "req_id 0 buys silence, error replies included",
            secs: 0.8,
            flags: 0,
            run: req_id_zero_is_silence,
        },
        Rule {
            section: "6.1",
            name: "a CONTROL that arrives with REPLY set is discarded",
            secs: 0.6,
            flags: 0,
            run: reply_bit_discarded,
        },
        Rule {
            section: "6.3",
            name: "an unknown opcode earns ERR_UNKNOWN_OP, not silence",
            secs: 0.6,
            flags: 0,
            run: unknown_op,
        },
        Rule {
            section: "6.5",
            name: "a body that is not the opcode's size -> ERR_BAD_LENGTH",
            secs: 0.8,
            flags: 0,
            run: bad_length,
        },
        Rule {
            section: "6.5",
            name: "a len the datagram does not back up -> ERR_BAD_LENGTH",
            secs: 0.2,
            flags: 0,
            run: bad_length_header,
        },
        Rule {
            section: "6.5",
            name: "the right size and the wrong value -> ERR_BAD_ARG",
            secs: 0.8,
            flags: 0,
            run: bad_arg,
        },
        Rule {
            section: "6.5",
            name: "SET_WIFI's own ERR_BAD_LENGTH and ERR_BAD_ARG cases",
            secs: 0.4,
            flags: LOOPBACK_ONLY,
            run: bad_arg_setwifi,
        },
        Rule {
            section: "2.2",
            name: "a CONTROL of an unknown version -> ERR_VERSION, v1 framing",
            secs: 0.2,
            flags: 0,
            run: err_version,
        },
        Rule {
            section: "6.3",
            name: "REBOOT without the magic is ERR_BAD_ARG and does not reboot",
            secs: 0.5,
            flags: 0,
            run: reboot_is_guarded,
        },
        Rule {
            section: "6.3",
            name: "SET_BRIGHTNESS applies exactly what it reports",
            secs: 0.6,
            flags: 0,
            run: brightness_applies,
        },
        Rule {
            section: "6.3",
            name: "SET_BRIGHTNESS 255 is clamped and reports the firmware cap",
            secs: 0.4,
            flags: CAP_PROBE,
            run: brightness_cap,
        },
        Rule {
            section: "7.5",
            name: "SET_IDLE accepts 0..=3 and echoes the mode it set",
            secs: 0.5,
            flags: 0,
            run: set_idle,
        },
        Rule {
            section: "7.3",
            name: "IDENTIFY is an overlay in the state byte, and 0 stops it",
            secs: 1.0,
            flags: 0,
            run: identify_overlay,
        },
        Rule {
            section: "8.4",
            name: "GET_WIFI returns an SSID and a join state, never the PSK",
            secs: 0.2,
            flags: 0,
            run: get_wifi,
        },
        Rule {
            section: "5.5",
            name: "GET_INFO: one new request per source per second",
            secs: 2.5,
            flags: 0,
            run: info_rate_limit,
        },
    ]
}

// ---------------------------------------------------------------------------

fn info_is_txt(cx: &mut Ctx) -> Result<Outcome, String> {
    let body = cx.get_info()?;
    let mut keys: Vec<String> = Vec::new();
    for e in txt::iter(&body) {
        keys.push(String::from_utf8_lossy(e.key).into_owned());
    }
    // Section 5.2: "txtvers MUST be first (RFC 6763 6.5)", and every entry is
    // a length-prefixed key=value, which `txt::iter` yielding anything at all
    // already proves.
    let first_ok = keys.first().map(|k| k == "txtvers").unwrap_or(false);
    let required = ["txtvers", "proto", "w", "h", "codecs", "mtu", "ctrl"];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|r| !keys.iter().any(|k| k == r))
        .collect();
    verdict(
        first_ok && missing.is_empty(),
        format!(
            "{} B, {} keys, first {:?}{}",
            body.len(),
            keys.len(),
            keys.first().map(String::as_str).unwrap_or(""),
            if missing.is_empty() {
                String::new()
            } else {
                format!(", missing {missing:?}")
            }
        ),
    )
}

fn info_contents(cx: &mut Ctx) -> Result<Outcome, String> {
    let body = cx.get_info()?;
    let info = match txt::DeviceInfo::parse(&body) {
        Ok(i) => i,
        Err(e) => return verdict(false, format!("TXT did not parse: {e:?}")),
    };
    // Section 4.7: "A v1 device MUST support all five."
    let all_five = screeny_proto::dec::SUPPORTED_CODECS
        .iter()
        .all(|&c| info.supports(c));
    let geometry = info.w == screeny_proto::W as u16 && info.h == screeny_proto::H as u16;
    let mtu = info.mtu == screeny_proto::MAX_PIXEL_PAYLOAD as u16;
    // Section 5.2: ctrl= is the port a sender can actually reach, and we are
    // talking to it, so it had better be the one we used.
    let ctrl_ok = info.ctrl == cx.ctrl_addr.port();
    verdict(
        all_five && geometry && mtu && ctrl_ok && info.proto == "1" && info.txtvers == 1,
        format!(
            "proto {} {}x{} mtu {} ctrl {} codecs {} fw {}",
            info.proto,
            info.w,
            info.h,
            info.mtu,
            info.ctrl,
            info.codec_ids().count(),
            info.fw
        ),
    )
}

fn every_safe_opcode(cx: &mut Ctx) -> Result<Outcome, String> {
    // The device's own name, so SET_NAME is exercised without changing
    // anything. Section 6.3 persists it, and a suite that renamed the panel
    // would be leaving a mess behind.
    let body = cx.get_info()?;
    let name = txt::DeviceInfo::parse(&body)
        .map(|i| i.name.to_string())
        .unwrap_or_default();
    let brightness = cx.brightness_found;

    let mut bad: Vec<String> = Vec::new();
    let mut check = |cx: &mut Ctx, what: &str, req: Request<'_>| {
        match cx.ctrl.request(req) {
            Ok(r) => {
                if let Some(c) = r.err_code() {
                    bad.push(format!("{what} -> {:?}", ErrorCode::from_u8(c)));
                }
            }
            Err(e) => bad.push(format!("{what}: {e}")),
        };
    };

    check(cx, "PING", Request::Ping);
    check(cx, "TELEMETRY", Request::Telemetry);
    check(cx, "SET_BRIGHTNESS", Request::SetBrightness(brightness));
    check(cx, "IDENTIFY", Request::Identify { duration_ms: 60 });
    check(cx, "IDENTIFY 0", Request::Identify { duration_ms: 0 });
    check(cx, "SET_IDLE", Request::SetIdle(IdleMode::Status));
    check(cx, "RESET_STATS", Request::ResetStats);
    check(cx, "RELEASE", Request::Release);
    check(cx, "GET_WIFI", Request::GetWifi);
    if !name.is_empty() {
        check(cx, "SET_NAME (to its own name)", Request::SetName(&name));
    }

    verdict(
        bad.is_empty(),
        if bad.is_empty() {
            format!("10 opcodes answered; name left as {name:?}")
        } else {
            bad.join("; ")
        },
    )
}

fn reply_echoes(cx: &mut Ctx) -> Result<Outcome, String> {
    let d = control_dgram(op::PING, 0, 0x5AA5, &[]);
    let r = cx.ctrl.raw(&d)?;
    let ok = matches!(&r, Some(b) if b.len() >= 8
        && b[2] == op::PING
        && b[3] & screeny_proto::C_REPLY != 0
        && u16::from_le_bytes([b[4], b[5]]) == 0x5AA5);
    verdict(ok, describe(&r))
}

fn req_id_zero_is_silence(cx: &mut Ctx) -> Result<Outcome, String> {
    // A valid request, and a request that would otherwise be an error. Both
    // are silence: a sender that said it did not want an answer does not get
    // one even when what it sent was rubbish.
    let a = cx.ctrl.raw(&control_dgram(op::PING, 0, 0, &[]))?;
    let b = cx.ctrl.raw(&control_dgram(0x7E, 0, 0, &[]))?;
    let c = cx.ctrl.raw(&control_dgram(op::PING, 0, 0, &[1, 2, 3]))?;
    verdict(
        a.is_none() && b.is_none() && c.is_none(),
        format!(
            "PING {}, unknown op {}, bad length {}",
            describe(&a),
            describe(&b),
            describe(&c)
        ),
    )
}

fn reply_bit_discarded(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.1: requests and replies are told apart by this bit and by
    // nothing else, so a device that answered replies would answer its own.
    let a = cx
        .ctrl
        .raw(&control_dgram(op::PING, screeny_proto::C_REPLY, 5, &[]))?;
    let b = cx.ctrl.raw(&control_dgram(
        op::PING,
        screeny_proto::C_REPLY | screeny_proto::C_ERROR,
        6,
        &[2],
    ))?;
    // And the device is still there afterwards.
    let alive = cx.ctrl.request(Request::Ping).is_ok();
    verdict(
        a.is_none() && b.is_none() && alive,
        format!("{}, {}, still answering: {alive}", describe(&a), describe(&b)),
    )
}

fn unknown_op(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut bad = Vec::new();
    for op_id in [0x00u8, 0x0C, 0x0E, 0x40, 0x7E, 0x7F, 0xFF] {
        let r = cx.ctrl.raw(&control_dgram(op_id, 0, 0x900, &[]))?;
        let ok = matches!(&r, Some(b) if b.len() >= 9
            && b[2] == op_id
            && b[3] & screeny_proto::C_ERROR != 0
            && b[8] == ErrorCode::UnknownOp.as_u8()
            && u16::from_le_bytes([b[4], b[5]]) == 0x900);
        if !ok {
            bad.push(format!("{op_id:#04x} {}", describe(&r)));
        }
    }
    verdict(
        bad.is_empty(),
        if bad.is_empty() {
            "7 opcodes, all ERR_UNKNOWN_OP echoing op and req_id".into()
        } else {
            bad.join("; ")
        },
    )
}

/// `(op, body, what)` triples that must earn one particular error code.
fn expect_error(
    cx: &mut Ctx,
    code: ErrorCode,
    cases: &[(u8, &[u8], &str)],
) -> Result<Outcome, String> {
    let mut bad = Vec::new();
    for (o, body, what) in cases {
        let r = cx.ctrl.raw(&control_dgram(*o, 0, 0x901, body))?;
        let ok = matches!(&r, Some(b) if b.len() >= 9
            && b[3] & screeny_proto::C_ERROR != 0
            && b[8] == code.as_u8());
        if !ok {
            bad.push(format!("{what}: {}", describe(&r)));
        }
    }
    verdict(
        bad.is_empty(),
        if bad.is_empty() {
            format!("{} cases, all {:?}", cases.len(), code)
        } else {
            bad.join("; ")
        },
    )
}

fn bad_length(cx: &mut Ctx) -> Result<Outcome, String> {
    let cases: &[(u8, &[u8], &str)] = &[
        (op::PING, &[0], "PING with a body"),
        (op::GET_INFO, &[1, 2], "GET_INFO with a body"),
        (op::SET_BRIGHTNESS, &[], "SET_BRIGHTNESS with no level"),
        (op::SET_BRIGHTNESS, &[1, 2], "SET_BRIGHTNESS with two"),
        (op::IDENTIFY, &[5], "IDENTIFY with one byte"),
        (op::SET_IDLE, &[], "SET_IDLE with no mode"),
        (op::REBOOT, &[1, 2, 3], "REBOOT with three bytes"),
        (op::SET_NAME, &[], "SET_NAME with no length byte"),
        (op::SET_NAME, &[4, b'a'], "SET_NAME whose length overruns"),
    ];
    expect_error(cx, ErrorCode::BadLength, cases)
}

fn bad_length_header(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.5: the reply is built from bytes 2 and 4-5, which are present
    // in any datagram long enough to have a header, so it still echoes the
    // right opcode and req_id although nothing after the header is trustworthy.
    let mut d = control_dgram(op::PING, 0, 0x2A, &[]);
    d[6] = 0xFF; // len = 255, with no body at all
    let r = cx.ctrl.raw(&d)?;
    let ok = matches!(&r, Some(b) if b.len() >= 9
        && b[2] == op::PING
        && u16::from_le_bytes([b[4], b[5]]) == 0x2A
        && b[8] == ErrorCode::BadLength.as_u8());
    verdict(ok, describe(&r))
}

fn bad_arg(cx: &mut Ctx) -> Result<Outcome, String> {
    let long_name = [&[33u8][..], &[b'x'; 33][..]].concat();
    let cases: &[(u8, &[u8], &str)] = &[
        (op::SET_IDLE, &[4], "an idle mode v1 does not define"),
        (op::SET_IDLE, &[255], "and another"),
        (op::REBOOT, &[0, 0, 0, 0], "REBOOT without the magic"),
        (op::REBOOT, b"RBOX", "REBOOT mistyped"),
        (op::SET_NAME, &long_name, "a name over 32 bytes"),
        (op::SET_NAME, &[2, 0xFF, 0xFE], "a name that is not UTF-8"),
    ];
    expect_error(cx, ErrorCode::BadArg, cases)
}

fn bad_arg_setwifi(cx: &mut Ctx) -> Result<Outcome, String> {
    // Both halves of section 8.2's body rules, kept together because they are
    // the only `SET_WIFI` this suite ever sends and both are refused before
    // the device could act on them. Loopback only all the same: betting the
    // bench device's association on that reading of the firmware is not a bet
    // worth making.
    let short: &[(u8, &[u8], &str)] = &[(op::SET_WIFI, &[1, b'x'], "SET_WIFI cut short")];
    if let Outcome::Fail(why) = expect_error(cx, ErrorCode::BadLength, short)? {
        return verdict(false, why);
    }
    let cases: &[(u8, &[u8], &str)] = &[
        (op::SET_WIFI, &[0, 0, 0], "SET_WIFI with an empty SSID"),
        (op::SET_WIFI, &[1, b'x', 0, 0x02], "SET_WIFI reserved flag"),
    ];
    expect_error(cx, ErrorCode::BadArg, cases)
}

fn err_version(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut d = control_dgram(op::GET_INFO, 0, 7, &[]);
    d[1] = (9 << 4) | screeny_proto::TYPE_CONTROL;
    let r = cx.ctrl.raw(&d)?;
    let ok = matches!(&r, Some(b) if b.len() >= 9
        && b[1] == (screeny_proto::VERSION << 4) | screeny_proto::TYPE_CONTROL
        && u16::from_le_bytes([b[4], b[5]]) == 7
        && b[8] == ErrorCode::Version.as_u8());

    // And the same thing with req_id 0 is still silence (section 6.1).
    let mut d0 = control_dgram(op::GET_INFO, 0, 0, &[]);
    d0[1] = (9 << 4) | screeny_proto::TYPE_CONTROL;
    let silent = cx.ctrl.raw(&d0)?.is_none();

    verdict(
        ok && silent,
        format!("{}, req_id 0 silent: {silent}", describe(&r)),
    )
}

fn reboot_is_guarded(cx: &mut Ctx) -> Result<Outcome, String> {
    // The magic that *works* is never sent by this suite at all. What is
    // checked is that the guard is there: the wrong word earns ERR_BAD_ARG
    // and the device keeps counting up.
    let before = cx.telemetry()?.uptime_ms;
    let body = 0xDEAD_BEEFu32.to_le_bytes();
    let r = cx.ctrl.raw(&control_dgram(op::REBOOT, 0, 0x6666, &body))?;
    let errored = matches!(&r, Some(b) if b.len() >= 9 && b[8] == ErrorCode::BadArg.as_u8());
    std::thread::sleep(Duration::from_millis(250));
    let after = cx.telemetry()?.uptime_ms;
    // A reboot would send uptime backwards.
    let alive = after >= before;
    verdict(
        errored && alive,
        format!("{}, uptime {before} -> {after} ms", describe(&r)),
    )
}

fn brightness_applies(cx: &mut Ctx) -> Result<Outcome, String> {
    // Never above what we found (the panel runs off laptop USB), so this
    // steps *down* and back. The cap itself is the opt-in rule below.
    let found = cx.brightness_found;
    if found < 4 {
        return Ok(Outcome::Skip(format!(
            "found at brightness {found}: nothing below it to test with"
        )));
    }
    let low = found / 2;
    let applied = match cx.ctrl.request(Request::SetBrightness(low))? {
        OwnedReply::Brightness { applied } => applied,
        other => return verdict(false, format!("expected a brightness reply, got {other:?}")),
    };
    let reported = cx.telemetry()?.brightness;
    let back = match cx.ctrl.request(Request::SetBrightness(found))? {
        OwnedReply::Brightness { applied } => applied,
        other => return verdict(false, format!("expected a brightness reply, got {other:?}")),
    };
    verdict(
        applied == low && reported == low && back == found,
        format!("{found} -> {low}: applied {applied}, telemetry {reported}, back to {back}"),
    )
}

fn brightness_cap(cx: &mut Ctx) -> Result<Outcome, String> {
    let found = cx.brightness_found;
    let cap = match cx.ctrl.request(Request::SetBrightness(255))? {
        OwnedReply::Brightness { applied } => applied,
        other => return verdict(false, format!("expected a brightness reply, got {other:?}")),
    };
    cx.ctrl.request(Request::SetBrightness(found))?;
    verdict(cap < 255, format!("applied {cap} for a requested 255"))
}

fn set_idle(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut bad = Vec::new();
    for m in [
        IdleMode::Black,
        IdleMode::Dim,
        IdleMode::HoldForever,
        IdleMode::Status,
    ] {
        match cx.ctrl.request(Request::SetIdle(m))? {
            OwnedReply::Idle { mode } if mode == m.as_u8() => {}
            other => bad.push(format!("{m:?} -> {other:?}")),
        }
    }
    verdict(
        bad.is_empty(),
        if bad.is_empty() {
            "0..=3 all echoed; left at STATUS".into()
        } else {
            bad.join("; ")
        },
    )
}

fn identify_overlay(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 7.3: an overlay, not a stream state. It shows in the state byte
    // and it "MUST work in any state". Kept short, and stopped explicitly.
    cx.ctrl.request(Request::Identify { duration_ms: 3_000 })?;
    std::thread::sleep(Duration::from_millis(150));
    let during = cx.telemetry()?.state;
    cx.ctrl.request(Request::Identify { duration_ms: 0 })?;
    std::thread::sleep(Duration::from_millis(150));
    let after = cx.telemetry()?.state;
    verdict(
        during == screeny_proto::control::state::IDENTIFY
            && after != screeny_proto::control::state::IDENTIFY,
        format!(
            "{} then {}",
            crate::state_name(during),
            crate::state_name(after)
        ),
    )
}

fn get_wifi(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 8.4's invariant: the PSK is never returned. The body is
    // `u8 n`, `n` bytes of SSID, `u8 state` and nothing else, so a body
    // longer than that would be the place a PSK could hide.
    match cx.ctrl.request(Request::GetWifi)? {
        OwnedReply::Wifi { ssid, state } => verdict(
            !ssid.is_empty() && state <= 3,
            format!("{} byte SSID, state {state}", ssid.len()),
        ),
        other => verdict(false, format!("expected a wifi reply, got {other:?}")),
    }
}

fn info_rate_limit(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 5.5 and section 6.1 have to hold at once: the limit counts new
    // requests, and a repeat of a req_id already answered is a retransmission
    // and must be answered again. Start from a closed-window-free state by
    // waiting one whole interval out.
    std::thread::sleep(Duration::from_millis(1_100));
    let info = |id: u16| control_dgram(op::GET_INFO, 0, id, &[]);
    let first = cx.ctrl.raw(&info(0x8888))?;
    let repeat = cx.ctrl.raw(&info(0x8888))?;
    let fresh = cx.ctrl.raw(&info(0x8889))?;
    std::thread::sleep(Duration::from_millis(1_100));
    let reopened = cx.ctrl.raw(&info(0x888A))?;
    verdict(
        first.is_some() && repeat.is_some() && fresh.is_none() && reopened.is_some(),
        format!(
            "first {}, same id {}, new id {}, after 1.1 s {}",
            first.is_some(),
            repeat.is_some(),
            fresh.is_some(),
            reopened.is_some()
        ),
    )
}
