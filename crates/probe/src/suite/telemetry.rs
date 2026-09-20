//! Spec sections 6.2, 6.4, 6.7 and 6.8: the packet that comes back on the
//! *frame* port, what it is rate-limited to, and what `RESET_STATS` does and
//! does not touch.

use std::time::{Duration, Instant};

use screeny_proto::control::{op, Reply, Request};
use screeny_proto::dec::codec;
use screeny_proto::{ControlPacket, F_KEY, F_STATS_REQ};

use super::{verdict, Ctx, Outcome, Rule, RETRY};
use crate::enc;
use crate::link::OwnedReply;

pub fn rules() -> Vec<Rule> {
    vec![
        Rule {
            section: "6.4",
            name: "STATS_REQ is answered on the frame port: 48 B, REPLY, req_id 0",
            secs: 0.8,
            flags: RETRY,
            run: stats_req_on_the_frame_port,
        },
        Rule {
            section: "6.2",
            name: "unsolicited TELEMETRY is rate-limited to one per 100 ms",
            secs: 1.0,
            flags: RETRY,
            run: telemetry_rate_limit,
        },
        Rule {
            section: "6.2",
            name: "a TELEMETRY request on the control port is never rate-limited",
            secs: 0.6,
            flags: 0,
            run: telemetry_requests_are_not_limited,
        },
        Rule {
            section: "3.1",
            name: "STATS_REQ is answered for a stale frame too",
            secs: 1.0,
            flags: RETRY,
            run: stats_req_on_a_stale_frame,
        },
        Rule {
            section: "6.8",
            name: "interarrival and jitter track a 30 fps stream",
            secs: 2.5,
            flags: RETRY,
            run: interarrival,
        },
        Rule {
            section: "6.8",
            name: "RESET_STATS zeroes the counters the wire reports",
            secs: 1.0,
            flags: RETRY,
            run: reset_stats_zeroes,
        },
        Rule {
            section: "6.8",
            name: "RESET_STATS leaves uptime, last_codec, the state and the lock",
            secs: 1.0,
            flags: RETRY,
            run: reset_stats_keeps,
        },
    ]
}

fn dim() -> Vec<u8> {
    enc::solid([25, 35, 45]).0
}

fn stats_req_on_the_frame_port(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.4: the reply comes "from the frame port to the datagram's
    // source port ... and a sender MUST accept it there". Section 6.2: it
    // carries REPLY and req_id 0, because nobody asked for it by id.
    let mut link = cx.claim()?;
    link.send(codec::SOLID, F_KEY | F_STATS_REQ, &dim())
        .map_err(|e| e.to_string())?;
    let d = match link.recv(Duration::from_millis(600)) {
        Some(d) => d,
        None => return verdict(false, "no TELEMETRY came back on the frame socket"),
    };
    let pkt = match ControlPacket::parse(&d) {
        Ok(p) => p,
        Err(e) => return verdict(false, format!("not a CONTROL packet: {e:?}")),
    };
    // The control socket must see nothing: section 9.2 binds them separately
    // so that this is unambiguous.
    let on_ctrl = cx.ctrl.recv(Duration::from_millis(100)).is_some();
    let body_ok = pkt.body.len() == 48; // section 6.7
    let decoded = matches!(
        Reply::decode(pkt.op, pkt.flags, pkt.body),
        Ok(Reply::Telemetry(_))
    );
    verdict(
        pkt.op == op::TELEMETRY && pkt.is_reply() && pkt.req_id == 0 && body_ok && decoded
            && !on_ctrl,
        format!(
            "op {:#04x} reply {} req_id {} body {} B, control socket saw {}",
            pkt.op,
            pkt.is_reply(),
            pkt.req_id,
            pkt.body.len(),
            if on_ctrl { "something" } else { "nothing" }
        ),
    )
}

fn telemetry_rate_limit(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.2: one per 100 ms per source. A sender that ignores section
    // 6.4's "MUST NOT set it on more than one frame in 100 ms" gets one
    // reply, not twelve. Twelve frames over ~60 ms, so at most two windows
    // can have opened even with a loaded host.
    let mut link = cx.claim()?;
    let p = dim();
    for _ in 0..12 {
        link.send(codec::SOLID, F_KEY | F_STATS_REQ, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(60));
    let replies = link.drain().len();

    // Past the window, another one.
    std::thread::sleep(Duration::from_millis(150));
    link.send(codec::SOLID, F_KEY | F_STATS_REQ, &p)
        .map_err(|e| e.to_string())?;
    let reopened = link.recv(Duration::from_millis(600)).is_some();
    verdict(
        (1..=2).contains(&replies) && reopened,
        format!("{replies} replies for 12 marked frames in ~60 ms; window reopened: {reopened}"),
    )
}

fn telemetry_requests_are_not_limited(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.2: "The 100 ms limit governs the **unsolicited** TELEMETRY...
    // A TELEMETRY *request* on the control port is solicited and is answered
    // every time."
    let t0 = Instant::now();
    let mut answers = 0;
    while t0.elapsed() < Duration::from_millis(400) {
        if matches!(
            cx.ctrl.request(Request::Telemetry)?,
            OwnedReply::Telemetry(_)
        ) {
            answers += 1;
        }
    }
    verdict(answers > 5, format!("{answers} answers in 400 ms"))
}

fn stats_req_on_a_stale_frame(cx: &mut Ctx) -> Result<Outcome, String> {
    // Sections 3.1 and 6.2, closed by card 006: "A frame from the source that
    // holds the lock is answered whether or not its pixels reach the panel."
    // The one frame a second a sender marks is exactly the frame it cannot
    // afford to lose track of.
    let link = cx.claim()?;
    let p = dim();
    link.send_seq(codec::SOLID, F_KEY, 500, &p)
        .map_err(|e| e.to_string())?;
    cx.settle();
    let _ = link.drain();
    // Past the 100 ms unsolicited window, so this reply is not suppressed.
    std::thread::sleep(Duration::from_millis(120));

    // seq 499 is not newer than 500: stale, and still answered.
    link.send_seq(codec::SOLID, F_KEY | F_STATS_REQ, 499, &p)
        .map_err(|e| e.to_string())?;
    let answered = link.recv(Duration::from_millis(600)).is_some();
    let t = cx.telemetry()?;
    verdict(
        answered && t.frames_dropped_stale == 1,
        format!("answered: {answered}, stale {}", t.frames_dropped_stale),
    )
}

fn interarrival(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.8, measured on the device's own clock. The bounds are wide on
    // purpose: this host runs several workers at once and the device sees
    // roughly 1-5 ms of inter-arrival jitter at 30 fps. What is being tested
    // is that the EWMA is seeded and fed at all, not that the bench is quiet.
    let mut link = cx.claim()?;
    let p = dim();
    let period = Duration::from_millis(33);
    let mut next = Instant::now();
    for _ in 0..50 {
        link.send(codec::SOLID, F_KEY, &p)
            .map_err(|e| e.to_string())?;
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    cx.settle();
    let t = cx.telemetry()?;
    // 33 ms nominal; anything from 15 to 60 ms is the sender being late, not
    // the device being wrong. (The field is a `u16` saturating at 65535, so
    // there is no room for a looser ceiling than that anyway.)
    let plausible = (15_000..=60_000).contains(&t.interarrival_us);
    verdict(
        plausible && t.jitter_us < t.interarrival_us && t.interarrival_max_us >= t.interarrival_us,
        format!(
            "interarrival {} us, jitter {} us, max {} us (33 ms nominal)",
            t.interarrival_us, t.jitter_us, t.interarrival_max_us
        ),
    )
}

fn reset_stats_zeroes(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut link = cx.claim()?;
    let p = dim();
    for _ in 0..5 {
        link.send(codec::SOLID, F_KEY, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(20));
    }
    cx.settle();
    let before = cx.telemetry()?;
    cx.reset_stats()?;
    let t = cx.telemetry()?;
    verdict(
        before.frames_rx >= 5
            && t.frames_rx == 0
            && t.frames_shown == 0
            && t.seq_gaps == 0
            && t.frames_dropped_stale == 0
            && t.frames_dropped_superseded == 0
            && t.frames_dropped_decode == 0
            && t.frames_rejected == 0
            && t.decode_us_max == 0
            && t.interarrival_max_us == 0,
        format!(
            "rx {} -> {}, shown {} -> {}, decode_us_max {} -> {}",
            before.frames_rx,
            t.frames_rx,
            before.frames_shown,
            t.frames_shown,
            before.decode_us_max,
            t.decode_us_max
        ),
    )
}

fn reset_stats_keeps(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.8: "RESET_STATS zeroes the counters of section 6.7 and these
    // three, and nothing else: it is a measurement control, not a stream
    // control. It does not release the lock, change the state, clear
    // `last_seq`, or blank the panel."
    let link = cx.claim()?;
    let p = dim();
    link.send_seq(codec::SOLID, F_KEY, 700, &p)
        .map_err(|e| e.to_string())?;
    cx.settle();
    let before = cx.telemetry()?;
    cx.reset_stats()?;
    let after = cx.telemetry()?;

    // `last_seq` survived, so a repeat of 700 is still stale.
    link.send_seq(codec::SOLID, F_KEY, 700, &p)
        .map_err(|e| e.to_string())?;
    cx.settle();
    let t = cx.telemetry()?;
    verdict(
        after.uptime_ms >= before.uptime_ms
            && after.last_codec == codec::SOLID
            && after.state == before.state
            && t.frames_dropped_stale == 1,
        format!(
            "uptime {} -> {} ms, last_codec {:#04x}, state {}, last_seq kept: stale {}",
            before.uptime_ms,
            after.uptime_ms,
            after.last_codec,
            crate::state_name(after.state),
            t.frames_dropped_stale
        ),
    )
}
