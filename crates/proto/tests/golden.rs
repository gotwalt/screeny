//! Golden byte vectors for every packet type.
//!
//! These are written out by hand from `docs/design/protocol-v1.md`, not
//! produced by this crate, so they fail if the encoder and the decoder drift
//! together. Every `assert_eq!` against a byte literal here is a claim about
//! the wire, and the spec section is named next to it.

use screeny_proto::control::{
    busy_reason, op, state, wifi_state, ErrorCode, IdleMode, Reply, Request, SetWifi, Telemetry,
    REBOOT_MAGIC,
};
use screeny_proto::txt::{self, DeviceInfo};
use screeny_proto::{
    ControlPacket, FramePacket, Packet, Reject, C_ERROR, C_REPLY, F_KEY, F_STATS_REQ,
};

/// Build into a scratch buffer and hand back the bytes that were written.
fn built<F: FnOnce(&mut [u8]) -> usize>(f: F) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let n = f(&mut buf);
    buf[..n].to_vec()
}

// ---------------------------------------------------------------------------
// FRAME, section 3 and the worked example in section 10
// ---------------------------------------------------------------------------

#[test]
fn frame_worked_example() {
    // Section 10: a 30 fps sender's 31st frame, codec 0x10 (PAL8_LZ),
    // keyframe, asking for stats, 1200 bytes of pixels, seq = 0x0100.
    let payload = vec![0xAAu8; 1200];
    let pkt = FramePacket {
        codec: 0x10,
        flags: F_KEY | F_STATS_REQ,
        seq: 0x0100,
        timestamp_us: None,
        payload: &payload,
    };
    let bytes = built(|b| pkt.write(b).unwrap());
    assert_eq!(
        &bytes[..8],
        &[0x53, 0x10, 0x10, 0x03, 0x00, 0x01, 0xB0, 0x04],
        "section 10 header"
    );
    assert_eq!(bytes.len(), 1208);
    assert_eq!(FramePacket::parse(&bytes).unwrap(), pkt);
}

#[test]
fn frame_has_ts_moves_the_payload_to_offset_12() {
    let payload = [1u8, 2, 3];
    let pkt = FramePacket {
        codec: 0x7F,
        // HAS_TS is spelled out here only so the round trip is an identity;
        // `write` would have set it from `timestamp_us` either way.
        flags: F_KEY | screeny_proto::F_HAS_TS,
        seq: 7,
        timestamp_us: Some(0x1234_5678),
        payload: &payload,
    };
    let bytes = built(|b| pkt.write(b).unwrap());
    assert_eq!(
        bytes,
        vec![
            0x53, 0x10, 0x7F, 0x09, // magic, v1 FRAME, SOLID, KEY|HAS_TS
            0x07, 0x00, // seq
            0x07, 0x00, // len = 4 timestamp + 3 pixels
            0x78, 0x56, 0x34, 0x12, // u32le timestamp
            1, 2, 3,
        ]
    );
    let back = FramePacket::parse(&bytes).unwrap();
    assert_eq!(back, pkt);
    assert!(back.has_ts());
    assert_eq!(back.payload, &[1, 2, 3], "HAS_TS prefix is stripped");
}

#[test]
fn write_derives_has_ts_from_the_timestamp() {
    // A caller that sets HAS_TS by hand but passes no timestamp cannot build
    // an inconsistent packet, and vice versa.
    let lying = FramePacket {
        codec: 2,
        flags: 0xFF,
        seq: 0,
        timestamp_us: None,
        payload: &[],
    };
    assert_eq!(built(|b| lying.write(b).unwrap())[3], 0xF7, "HAS_TS cleared");

    let quiet = FramePacket {
        codec: 2,
        flags: 0,
        seq: 0,
        timestamp_us: Some(0),
        payload: &[],
    };
    assert_eq!(built(|b| quiet.write(b).unwrap())[3], 0x08, "HAS_TS set");
}

#[test]
fn frame_rejects() {
    let ok = built(|b| {
        FramePacket {
            codec: 2,
            flags: F_KEY,
            seq: 1,
            timestamp_us: None,
            payload: &[9, 9, 9],
        }
        .write(b)
        .unwrap()
    });
    assert!(FramePacket::parse(&ok).is_ok());

    // 2.1 magic
    let mut bad = ok.clone();
    bad[0] = 0x54;
    assert_eq!(FramePacket::parse(&bad), Err(Reject::BadMagic));

    // 2.2 version
    let mut bad = ok.clone();
    bad[1] = 0x20;
    assert_eq!(FramePacket::parse(&bad), Err(Reject::BadVersion));

    // 2.2 a CONTROL on the frame port
    let mut bad = ok.clone();
    bad[1] = 0x12;
    assert_eq!(FramePacket::parse(&bad), Err(Reject::BadType));
    // ...and a reserved type
    let mut bad = ok.clone();
    bad[1] = 0x11;
    assert_eq!(FramePacket::parse(&bad), Err(Reject::BadType));
    assert_eq!(Packet::parse(&bad), Err(Reject::BadType));

    // 2.3 len longer than the datagram
    let mut bad = ok.clone();
    bad[6] = 0xFF;
    assert_eq!(FramePacket::parse(&bad), Err(Reject::BadLength));

    // 2.3 FRAME len > 1464
    let mut big = vec![0u8; 8 + 1465];
    big[..8].copy_from_slice(&[0x53, 0x10, 0x02, 0x01, 0, 0, 0xB9, 0x05]);
    assert_eq!(u16::from_le_bytes([big[6], big[7]]), 1465);
    assert_eq!(FramePacket::parse(&big), Err(Reject::TooLong));

    // section 3: HAS_TS with no room for the timestamp
    let mut bad = ok.clone();
    bad[3] |= 0x08;
    bad[6] = 3;
    assert_eq!(FramePacket::parse(&bad), Err(Reject::ShortTimestamp));

    // fewer than 8 bytes
    for n in 0..8 {
        assert_eq!(FramePacket::parse(&ok[..n]), Err(Reject::Short));
    }
}

#[test]
fn bytes_past_len_are_padding() {
    // Section 2.3: "Bytes beyond 8 + len are padding and MUST be ignored".
    let mut d = vec![0x53, 0x10, 0x02, 0x01, 0, 0, 2, 0, 0xAA, 0xBB];
    d.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let f = FramePacket::parse(&d).unwrap();
    assert_eq!(f.payload, &[0xAA, 0xBB]);
}

// ---------------------------------------------------------------------------
// CONTROL, section 6
// ---------------------------------------------------------------------------

#[test]
fn control_requests() {
    // op, request, expected full datagram with req_id = 0x0102
    let cases: Vec<(Request, Vec<u8>)> = vec![
        (Request::Ping, vec![0x53, 0x12, 0x01, 0x00, 2, 1, 0, 0]),
        (Request::GetInfo, vec![0x53, 0x12, 0x02, 0x00, 2, 1, 0, 0]),
        (Request::Telemetry, vec![0x53, 0x12, 0x03, 0x00, 2, 1, 0, 0]),
        (
            Request::SetBrightness(200),
            vec![0x53, 0x12, 0x04, 0x00, 2, 1, 1, 0, 200],
        ),
        (
            Request::Identify { duration_ms: 3000 },
            vec![0x53, 0x12, 0x05, 0x00, 2, 1, 2, 0, 0xB8, 0x0B],
        ),
        (
            Request::SetIdle(IdleMode::Dim),
            vec![0x53, 0x12, 0x06, 0x00, 2, 1, 1, 0, 2],
        ),
        (
            Request::ResetStats,
            vec![0x53, 0x12, 0x07, 0x00, 2, 1, 0, 0],
        ),
        (Request::Release, vec![0x53, 0x12, 0x08, 0x00, 2, 1, 0, 0]),
        (
            Request::SetName("Desk"),
            vec![0x53, 0x12, 0x09, 0x00, 2, 1, 5, 0, 4, b'D', b'e', b's', b'k'],
        ),
        (Request::GetWifi, vec![0x53, 0x12, 0x0A, 0x00, 2, 1, 0, 0]),
        (
            Request::SetWifi(SetWifi {
                ssid: "Example-Wifi1",
                psk: "password9",
                persist: true,
            }),
            {
                let mut v = vec![0x53, 0x12, 0x0B, 0x00, 2, 1, 25, 0, 13];
                v.extend_from_slice(b"Example-Wifi1");
                v.push(9);
                v.extend_from_slice(b"password9");
                v.push(1);
                v
            },
        ),
        (
            Request::Reboot,
            vec![0x53, 0x12, 0x0D, 0x00, 2, 1, 4, 0, b'R', b'B', b'O', b'O'],
        ),
    ];

    for (req, want) in cases {
        let got = built(|b| req.write(0x0102, b).unwrap());
        assert_eq!(got, want, "{req:?} on the wire");
        assert_eq!(got.len(), req.encoded_len());

        let pkt = ControlPacket::parse(&got).unwrap();
        assert_eq!(pkt.op, req.op());
        assert_eq!(pkt.req_id, 0x0102);
        assert!(!pkt.is_reply());
        assert_eq!(Request::decode(pkt.op, pkt.body), Ok(req));
    }
}

#[test]
fn reboot_magic_is_rboo_little_endian() {
    assert_eq!(REBOOT_MAGIC.to_le_bytes(), *b"RBOO");
    assert_eq!(REBOOT_MAGIC, 0x4F4F_4252);
    assert_eq!(
        Request::decode(op::REBOOT, b"RBOX"),
        Err(ErrorCode::BadArg),
        "a REBOOT without the guard word is refused"
    );
}

#[test]
fn control_replies() {
    let telem = Telemetry {
        uptime_ms: 1_234_567,
        frames_rx: 900,
        frames_shown: 895,
        frames_dropped_stale: 1,
        frames_dropped_superseded: 3,
        frames_dropped_decode: 0,
        frames_rejected: 2,
        seq_gaps: 5,
        interarrival_us: 33_333,
        jitter_us: 412,
        interarrival_max_us: 65_535,
        decode_us: 1_100,
        decode_us_max: 2_048,
        render_us_max: 700,
        rssi_dbm: -57,
        brightness: 30,
        state: state::LIVE,
        last_codec: 0x10,
    };

    let cases: Vec<(u8, Reply, Vec<u8>)> = vec![
        (
            op::PING,
            Reply::Ping { uptime_ms: 70_000 },
            vec![0x53, 0x12, 0x01, 0x01, 9, 0, 4, 0, 0x70, 0x11, 0x01, 0x00],
        ),
        (
            op::SET_BRIGHTNESS,
            Reply::Brightness { applied: 30 },
            vec![0x53, 0x12, 0x04, 0x01, 9, 0, 1, 0, 30],
        ),
        (
            op::IDENTIFY,
            Reply::Identify,
            vec![0x53, 0x12, 0x05, 0x01, 9, 0, 0, 0],
        ),
        (
            op::SET_IDLE,
            Reply::Idle { mode: 1 },
            vec![0x53, 0x12, 0x06, 0x01, 9, 0, 1, 0, 1],
        ),
        (
            op::GET_WIFI,
            Reply::Wifi {
                ssid: "Example-Wifi1",
                state: wifi_state::CONNECTED,
            },
            {
                let mut v = vec![0x53, 0x12, 0x0A, 0x01, 9, 0, 15, 0, 13];
                v.extend_from_slice(b"Example-Wifi1");
                v.push(2);
                v
            },
        ),
        (
            op::BUSY,
            Reply::Busy {
                reason: busy_reason::LOCKED,
                lock_holder_ms_remaining: 420,
            },
            vec![0x53, 0x12, 0x0C, 0x01, 9, 0, 5, 0, 0, 0xA4, 0x01, 0x00, 0x00],
        ),
        (
            op::SET_BRIGHTNESS,
            Reply::Err {
                code: ErrorCode::BadArg.as_u8(),
            },
            vec![0x53, 0x12, 0x04, 0x03, 9, 0, 1, 0, 0x05],
        ),
    ];

    for (opcode, reply, want) in cases {
        let got = built(|b| reply.write(opcode, 9, b).unwrap());
        assert_eq!(got, want, "{reply:?} on the wire");
        let pkt = ControlPacket::parse(&got).unwrap();
        assert!(pkt.is_reply());
        assert_eq!(pkt.flags & C_REPLY, C_REPLY);
        assert_eq!(Reply::decode(pkt.op, pkt.flags, pkt.body), Ok(reply));
    }

    // TELEMETRY: the 48-byte struct, offsets straight out of section 6.7.
    let got = built(|b| Reply::Telemetry(telem).write(op::TELEMETRY, 0, b).unwrap());
    assert_eq!(got.len(), 8 + 48);
    assert_eq!(&got[..8], &[0x53, 0x12, 0x03, 0x01, 0, 0, 48, 0]);
    let body = &got[8..];
    assert_eq!(&body[0..4], &1_234_567u32.to_le_bytes());
    assert_eq!(&body[4..8], &900u32.to_le_bytes());
    assert_eq!(&body[28..32], &5u32.to_le_bytes());
    assert_eq!(&body[32..34], &33_333u16.to_le_bytes());
    assert_eq!(body[44], (-57i8) as u8);
    assert_eq!(body[45], 30);
    assert_eq!(body[46], state::LIVE);
    assert_eq!(body[47], 0x10);
    assert_eq!(Telemetry::decode(body), Ok(telem));
}

#[test]
fn telemetry_is_versioned_by_len() {
    // Section 6.7: a future device may append fields and set a larger len; a
    // v1 sender reads the first 48 bytes and ignores the rest, and rejects a
    // body shorter than 48.
    let t = Telemetry {
        uptime_ms: 42,
        ..Telemetry::default()
    };
    let mut body = [0u8; 64];
    t.write(&mut body).unwrap();
    body[48..].fill(0xEE);
    assert_eq!(Telemetry::decode(&body), Ok(t));
    assert_eq!(Telemetry::decode(&body[..47]), Err(ErrorCode::BadLength));
    assert_eq!(Telemetry::decode(&body[..48]), Ok(t));
}

#[test]
fn request_decode_reports_the_error_code_to_reply_with() {
    // Section 6.5's codes are exactly what the device puts in the error reply.
    assert_eq!(Request::decode(0x7E, &[]), Err(ErrorCode::UnknownOp));
    assert_eq!(Request::decode(op::PING, &[0]), Err(ErrorCode::BadLength));
    assert_eq!(Request::decode(op::SET_BRIGHTNESS, &[]), Err(ErrorCode::BadLength));
    assert_eq!(
        Request::decode(op::SET_BRIGHTNESS, &[1, 2]),
        Err(ErrorCode::BadLength)
    );
    assert_eq!(Request::decode(op::SET_IDLE, &[4]), Err(ErrorCode::BadArg));
    // SET_NAME longer than 32 bytes
    let mut long = vec![33u8];
    long.extend(std::iter::repeat(b'x').take(33));
    assert_eq!(Request::decode(op::SET_NAME, &long), Err(ErrorCode::BadArg));
    // SET_NAME whose length byte disagrees with the body
    assert_eq!(
        Request::decode(op::SET_NAME, &[4, b'a']),
        Err(ErrorCode::BadLength)
    );
    // SET_NAME that is not UTF-8
    assert_eq!(
        Request::decode(op::SET_NAME, &[1, 0xFF]),
        Err(ErrorCode::BadArg)
    );
    // SET_WIFI with an empty SSID (section 8.2 says 1..=32)
    assert_eq!(
        Request::decode(op::SET_WIFI, &[0, 0, 0]),
        Err(ErrorCode::BadArg)
    );
    // SET_WIFI with a reserved flag bit set
    assert_eq!(
        Request::decode(op::SET_WIFI, &[1, b'a', 0, 0x02]),
        Err(ErrorCode::BadArg)
    );
    // BUSY is device -> sender only; it is not a request.
    assert_eq!(Request::decode(op::BUSY, &[]), Err(ErrorCode::UnknownOp));
}

#[test]
fn error_reply_parses_whatever_op_it_echoes() {
    for opcode in [op::PING, op::SET_WIFI, 0xF0] {
        let got = built(|b| Reply::Err { code: 0x04 }.write(opcode, 1, b).unwrap());
        assert_eq!(got[3], C_REPLY | C_ERROR);
        let pkt = ControlPacket::parse(&got).unwrap();
        assert_eq!(
            Reply::decode(pkt.op, pkt.flags, pkt.body),
            Ok(Reply::Err { code: 0x04 })
        );
    }
    assert_eq!(ErrorCode::from_u8(0x04), Some(ErrorCode::Busy));
    assert_eq!(ErrorCode::from_u8(0x00), None);
    assert_eq!(ErrorCode::from_u8(0x0A), None);
}

#[test]
fn a_frame_on_the_control_port_is_discarded() {
    let f = built(|b| {
        FramePacket {
            codec: 2,
            flags: F_KEY,
            seq: 0,
            timestamp_us: None,
            payload: &[],
        }
        .write(b)
        .unwrap()
    });
    assert_eq!(ControlPacket::parse(&f), Err(Reject::BadType));
    // ...but the general parser sees it, which is what a sender's frame
    // socket needs for the section 6.4 telemetry reply.
    assert!(matches!(Packet::parse(&f), Ok(Packet::Frame(_))));
}

// ---------------------------------------------------------------------------
// DNS-SD TXT, sections 5.2 and 6.6
// ---------------------------------------------------------------------------

#[test]
fn txt_matches_the_section_6_6_example() {
    let info = DeviceInfo {
        codecs: screeny_proto::dec::CODECS_TXT,
        fw: "0.1.0",
        id: "a4cf12",
        name: "Desk panel",
        ..DeviceInfo::DEFAULT
    };
    let bytes = built(|b| info.write(b).unwrap());

    // Section 6.6 spells out the first four strings byte for byte.
    assert_eq!(
        &bytes[..10],
        b"\x09txtvers=1".as_slice(),
        "txtvers must be first (RFC 6763 6.5)"
    );
    assert_eq!(&bytes[10..18], b"\x07proto=1".as_slice());
    assert_eq!(&bytes[18..23], b"\x04w=64".as_slice());
    assert_eq!(&bytes[23..28], b"\x04h=32".as_slice());

    // Key order is section 5.2's order.
    let keys: Vec<&[u8]> = txt::iter(&bytes).map(|e| e.key).collect();
    assert_eq!(
        keys,
        vec![
            &b"txtvers"[..],
            b"proto",
            b"w",
            b"h",
            b"codecs",
            b"mtu",
            b"ctrl",
            b"fw",
            b"id",
            b"name",
        ]
    );
    assert_eq!(txt::find(&bytes, "mtu"), Some(&b"1464"[..]));
    assert_eq!(txt::find(&bytes, "ctrl"), Some(&b"49375"[..]));
    assert_eq!(txt::find(&bytes, "nope"), None);

    let back = DeviceInfo::parse(&bytes).unwrap();
    assert_eq!(back, info);
    assert_eq!(
        back.codec_ids().collect::<Vec<_>>(),
        screeny_proto::dec::SUPPORTED_CODECS.to_vec(),
        "codecs= is the supported set in preference order"
    );
    assert!(back.supports(0x11));
    assert!(!back.supports(0x03));
    // Section 9.4 step 3: the device's preference order wins.
    assert_eq!(back.best_codec(&[0x02, 0x28]), Some(0x28));
    assert_eq!(back.best_codec(&[0x03]), None);
}

#[test]
fn txt_requires_the_keys_section_5_2_calls_required() {
    use screeny_proto::txt::TxtError;
    let full = built(|b| {
        DeviceInfo {
            codecs: "2",
            ..DeviceInfo::DEFAULT
        }
        .write(b)
        .unwrap()
    });
    assert!(DeviceInfo::parse(&full).is_ok());

    for (key, entry) in [
        ("proto", &b"\x07proto=1"[..]),
        ("w", b"\x04w=64"),
        ("h", b"\x04h=32"),
        ("codecs", b"\x08codecs=2"),
        ("ctrl", b"\x0Actrl=49375"),
    ] {
        let mut without = Vec::new();
        let mut rest = full.as_slice();
        while !rest.is_empty() {
            let n = rest[0] as usize + 1;
            let (s, tail) = rest.split_at(n);
            if s != entry {
                without.extend_from_slice(s);
            }
            rest = tail;
        }
        assert_ne!(without.len(), full.len(), "{key} entry not found to remove");
        assert_eq!(DeviceInfo::parse(&without), Err(TxtError::MissingKey(key)));
    }

    // Optional keys fall back, unknown keys are tolerated.
    let mut plus = full.clone();
    plus.extend_from_slice(b"\x0Bfuture=yes!");
    let info = DeviceInfo::parse(&plus).unwrap();
    assert_eq!(info.mtu, 1464);
    assert_eq!(info.fw, "");
    assert_eq!(info.name, "");
}

#[test]
fn txt_codec_list_skips_what_it_cannot_read() {
    let info = DeviceInfo {
        codecs: "16,,999,17,x,2",
        ..DeviceInfo::DEFAULT
    };
    assert_eq!(info.codec_ids().collect::<Vec<_>>(), vec![16, 17, 2]);
}

// ---------------------------------------------------------------------------
// Sequence numbers, section 3.2
// ---------------------------------------------------------------------------

#[test]
fn seq_is_rfc_1982_serial_arithmetic() {
    use screeny_proto::{gap, newer};
    assert!(newer(1, 0));
    assert!(!newer(0, 0), "a duplicate is not newer");
    assert!(!newer(0, 1));
    assert!(newer(0, 0xFFFF), "wrap");
    assert!(newer(0x7FFF, 0));
    assert!(!newer(0x8000, 0), "exactly half the space is not newer");
    assert!(newer(0xFFFF, 0x8000));

    assert_eq!(gap(1, 0), 0, "consecutive frames leave no gap");
    assert_eq!(gap(5, 0), 4);
    assert_eq!(gap(2, 0xFFFF), 2);
    assert_eq!(gap(0, 0), 0, "a duplicate cannot manufacture a gap");
    assert_eq!(gap(0, 5), 0, "nor can a reordered frame");
}
