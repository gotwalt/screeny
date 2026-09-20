//! Spec sections 1, 2 and 3.1: what the frame port accepts, what it throws
//! away, and the one counter that says so.
//!
//! Every rule here is "the datagram landed in `frames_rejected` and nowhere
//! else, and nothing came back", or its opposite. Both halves matter: a
//! device that answered rubbish on the frame port would be a reflector, and a
//! device that silently dropped a frame it should have counted would make
//! section 6.9's diagnosis table useless.

use screeny_proto::control::op;
use screeny_proto::dec::codec;
use screeny_proto::F_KEY;

use super::{
    control_dgram, frame_dgram, rejected_and_silent, shown, verdict, Ctx, Outcome, Rule,
    LOOPBACK_ONLY,
};
use crate::enc;

const V1_FRAME: u8 = (screeny_proto::VERSION << 4) | screeny_proto::TYPE_FRAME;

pub fn rules() -> Vec<Rule> {
    vec![
        Rule {
            section: "2.1",
            name: "the wrong magic byte -> frames_rejected, no reply",
            secs: 0.5,
            flags: 0,
            run: bad_magic,
        },
        Rule {
            section: "2.2",
            name: "a version that is not 1 -> frames_rejected",
            secs: 1.5,
            flags: 0,
            run: bad_version,
        },
        Rule {
            section: "2.2",
            name: "a reserved packet type (FRAME_FRAG, 0xF) -> frames_rejected",
            secs: 1.0,
            flags: 0,
            run: reserved_type,
        },
        Rule {
            section: "2.2",
            name: "a CONTROL on the frame port -> counted, never answered",
            secs: 0.5,
            flags: 0,
            run: control_on_frame_port,
        },
        Rule {
            section: "2.2",
            name: "a FRAME on the control port -> dropped, not counted, not answered",
            secs: 0.6,
            flags: 0,
            run: frame_on_control_port,
        },
        Rule {
            section: "2.2",
            name: "a datagram too short to hold a header -> frames_rejected",
            secs: 1.0,
            flags: 0,
            run: too_short,
        },
        Rule {
            section: "2.3",
            name: "8 + len beyond the datagram -> frames_rejected",
            secs: 0.5,
            flags: 0,
            run: len_overruns,
        },
        Rule {
            section: "2.3",
            name: "len over MAX_PIXEL_PAYLOAD -> frames_rejected",
            secs: 0.5,
            flags: LOOPBACK_ONLY,
            run: len_too_long,
        },
        Rule {
            section: "1",
            name: "a datagram larger than 1472 is discarded, not parsed",
            secs: 0.5,
            flags: LOOPBACK_ONLY,
            run: datagram_too_long,
        },
        Rule {
            section: "3",
            name: "HAS_TS with no room for the timestamp -> frames_rejected",
            secs: 0.5,
            flags: 0,
            run: has_ts_truncated,
        },
        Rule {
            section: "3",
            name: "HAS_TS is parsed and skipped; the frame is still shown",
            secs: 0.4,
            flags: 0,
            run: has_ts_happy,
        },
        Rule {
            section: "2.3",
            name: "bytes beyond 8 + len are padding; the frame is still shown",
            secs: 0.4,
            flags: 0,
            run: padding_ignored,
        },
        Rule {
            section: "3.1",
            name: "reserved frame flag bits are ignored, not rejected",
            secs: 0.4,
            flags: 0,
            run: reserved_flags,
        },
        Rule {
            section: "3.1",
            name: "a frame with KEY clear still decodes in v1",
            secs: 0.4,
            flags: 0,
            run: key_clear,
        },
    ]
}

/// Three bytes of `SOLID`, kept dim: the panel runs off laptop USB.
fn dim() -> Vec<u8> {
    enc::solid([30, 40, 50]).0
}

// ---------------------------------------------------------------------------

fn bad_magic(cx: &mut Ctx) -> Result<Outcome, String> {
    rejected_and_silent(cx, &[0x00; 16])
}

fn bad_version(cx: &mut Ctx) -> Result<Outcome, String> {
    for v in [0u8, 2, 9] {
        let d = frame_dgram(
            (v << 4) | screeny_proto::TYPE_FRAME,
            codec::SOLID,
            F_KEY,
            1,
            3,
            &dim(),
        );
        match rejected_and_silent(cx, &d)? {
            Outcome::Pass(_) => {}
            Outcome::Fail(why) => return verdict(false, format!("version {v}: {why}")),
            o => return Ok(o),
        }
    }
    verdict(true, "versions 0, 2 and 9 all counted")
}

fn reserved_type(cx: &mut Ctx) -> Result<Outcome, String> {
    for t in [0x1u8, 0xF] {
        let d = frame_dgram(
            (screeny_proto::VERSION << 4) | t,
            codec::SOLID,
            F_KEY,
            1,
            3,
            &dim(),
        );
        match rejected_and_silent(cx, &d)? {
            Outcome::Pass(_) => {}
            Outcome::Fail(why) => return verdict(false, format!("type {t:#x}: {why}")),
            o => return Ok(o),
        }
    }
    verdict(true, "types 0x1 and 0xF both counted")
}

fn control_on_frame_port(cx: &mut Ctx) -> Result<Outcome, String> {
    rejected_and_silent(cx, &control_dgram(op::PING, 0, 0x9999, &[]))
}

fn frame_on_control_port(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 2.2: `frames_rejected` counts the frame port only. "A datagram
    // discarded on the control port is counted nowhere: the control channel
    // answers rather than counts, and folding control-port noise into a
    // counter a sender reads to diagnose its *video* stream would make that
    // counter useless."
    cx.reset_stats()?;
    let d = frame_dgram(V1_FRAME, codec::SOLID, F_KEY, 1, 3, &dim());
    let r = cx.ctrl.raw(&d)?;
    cx.settle();
    let t = cx.telemetry()?;
    verdict(
        r.is_none() && t.frames_rejected == 0 && t.frames_shown == 0 && t.frames_rx == 0,
        format!(
            "reply {}, rejected {} shown {} rx {}",
            r.is_some(),
            t.frames_rejected,
            t.frames_shown,
            t.frames_rx
        ),
    )
}

fn too_short(cx: &mut Ctx) -> Result<Outcome, String> {
    for (what, bytes) in [
        ("empty", &[][..]),
        ("one byte", &[0x53][..]),
        ("seven bytes", &[0x53, V1_FRAME, codec::SOLID, F_KEY, 0, 0, 3][..]),
    ] {
        match rejected_and_silent(cx, bytes)? {
            Outcome::Pass(_) => {}
            Outcome::Fail(why) => return verdict(false, format!("{what}: {why}")),
            o => return Ok(o),
        }
    }
    verdict(true, "empty, 1 byte and 7 bytes all counted")
}

fn len_overruns(cx: &mut Ctx) -> Result<Outcome, String> {
    // len = 200 with three bytes of payload.
    let d = frame_dgram(V1_FRAME, codec::SOLID, F_KEY, 1, 200, &dim());
    rejected_and_silent(cx, &d)
}

fn len_too_long(cx: &mut Ctx) -> Result<Outcome, String> {
    // len = 1465, one over MAX_PIXEL_PAYLOAD, with a datagram that backs it
    // up - so the only thing wrong is the length. That makes the datagram
    // 1473 bytes, which is one over the protocol's own ceiling and so cannot
    // survive a real link: loopback only.
    let d = frame_dgram(
        V1_FRAME,
        codec::SOLID,
        F_KEY,
        1,
        (screeny_proto::MAX_PIXEL_PAYLOAD + 1) as u16,
        &vec![0u8; screeny_proto::MAX_PIXEL_PAYLOAD + 1],
    );
    rejected_and_silent(cx, &d)
}

fn datagram_too_long(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 1: a receiver's frame buffer is 1472 bytes, so a longer
    // datagram cannot be read whole and MUST be discarded rather than parsed
    // from the truncated prefix it managed to read.
    let d = frame_dgram(V1_FRAME, codec::SOLID, F_KEY, 1, 1992, &vec![0x5A; 1992]);
    rejected_and_silent(cx, &d)
}

fn has_ts_truncated(cx: &mut Ctx) -> Result<Outcome, String> {
    // HAS_TS set, len = 2: no room for the four-byte timestamp.
    let d = frame_dgram(
        V1_FRAME,
        codec::SOLID,
        F_KEY | screeny_proto::F_HAS_TS,
        1,
        2,
        &[1, 2],
    );
    rejected_and_silent(cx, &d)
}

fn has_ts_happy(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 3: "A v1 receiver MUST parse and skip these 4 bytes correctly
    // even if it ignores the value". Over the wire that shows up as the frame
    // being shown at all - a receiver that did not skip them would hand the
    // decoder seven bytes where SOLID wants three.
    let link = cx.claim()?;
    let p = dim();
    for attempt in 1..=3u32 {
        link.send_full(codec::SOLID, F_KEY, 100 + attempt as u16, Some(0xDEAD_BEEF), &p)
            .map_err(|e| e.to_string())?;
        cx.settle();
        let t = cx.telemetry()?;
        if t.frames_shown >= 1 && t.frames_dropped_decode == 0 {
            return verdict(
                true,
                format!("shown {} last_codec {:#04x}", t.frames_shown, t.last_codec),
            );
        }
        if attempt == 3 {
            return verdict(
                false,
                format!(
                    "shown {} decode {} rejected {}",
                    t.frames_shown, t.frames_dropped_decode, t.frames_rejected
                ),
            );
        }
    }
    unreachable!()
}

fn padding_ignored(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 2.3: "Bytes beyond 8 + len are padding and MUST be ignored".
    // Section 4 puts the exact-length rule *inside* len, so padding a
    // fixed-size codec out to a round number still decodes.
    let link = cx.claim()?;
    let mut body = dim();
    body.extend_from_slice(&[0xAA; 64]);
    for attempt in 1..=3u32 {
        let d = frame_dgram(V1_FRAME, codec::SOLID, F_KEY, 200 + attempt as u16, 3, &body);
        link.send_raw(&d).map_err(|e| e.to_string())?;
        cx.settle();
        let t = cx.telemetry()?;
        if t.frames_shown >= 1 && t.frames_dropped_decode == 0 && t.frames_rejected == 0 {
            return verdict(true, format!("64 bytes of padding, shown {}", t.frames_shown));
        }
        if attempt == 3 {
            return verdict(
                false,
                format!(
                    "shown {} decode {} rejected {}",
                    t.frames_shown, t.frames_dropped_decode, t.frames_rejected
                ),
            );
        }
    }
    unreachable!()
}

fn reserved_flags(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 3.1: "reserved. MUST be 0. A receiver MUST ignore bits it does
    // not know." Ignoring is not rejecting - a device that rejected them
    // would make adding a flag a breaking change. `STATS_REQ` is deliberately
    // not among them, so this does not also provoke a telemetry reply.
    let (ok, detail) = shown(cx, codec::SOLID, F_KEY | 0xF0, &dim())?;
    verdict(ok, detail)
}

fn key_clear(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 4: every v1 codec is stateless, so a frame decodes standalone
    // whatever the KEY bit says. The rule that a non-KEY frame is held back
    // only bites once a stateful codec exists.
    let (ok, detail) = shown(cx, codec::SOLID, 0, &dim())?;
    verdict(ok, detail)
}
