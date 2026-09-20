//! Spec sections 3.2 and 3.3: RFC 1982 serial arithmetic on `seq`, the two
//! counters it feeds, and the identity the whole of section 6.9 rests on.
//!
//! Every frame here is sent on its own and given time to be drained, because
//! two frames inside one drain are a *superseded* pair (correct behaviour,
//! useless test) rather than a stale one.

use screeny_proto::dec::codec;
use screeny_proto::F_KEY;

use super::{verdict, Ctx, Outcome, Rule, RETRY};
use crate::enc;

pub fn rules() -> Vec<Rule> {
    vec![
        Rule {
            section: "3.2",
            name: "seq wraps: 0xFFFE, 0xFFFF, 0x0000, 0x0001 are all newer",
            secs: 1.0,
            flags: RETRY,
            run: wrap,
        },
        Rule {
            section: "3.2",
            name: "a duplicate, the past and half the space away -> stale",
            secs: 1.2,
            flags: RETRY,
            run: stale,
        },
        Rule {
            section: "3.2",
            name: "a gap adds seq_gaps and the frame is still shown",
            secs: 0.8,
            flags: RETRY,
            run: gaps,
        },
        Rule {
            section: "3.2",
            name: "a gap across the wrap is counted the same way",
            secs: 0.8,
            flags: RETRY,
            run: gaps_across_the_wrap,
        },
        Rule {
            section: "3.2",
            name: "a new source resets last_seq; the counters keep running",
            secs: 1.5,
            flags: RETRY,
            run: new_source_resets_seq,
        },
        Rule {
            section: "3.3",
            name: "frames_rx = shown + superseded + decode",
            secs: 1.0,
            flags: RETRY,
            run: identity,
        },
    ]
}

fn dim() -> Vec<u8> {
    enc::solid([20, 45, 35]).0
}

/// Send one frame with an explicit `seq` and wait for the drain to close.
fn one(cx: &mut Ctx, link: &crate::link::FrameLink, seq: u16) -> Result<(), String> {
    link.send_seq(codec::SOLID, F_KEY, seq, &dim())
        .map_err(|e| e.to_string())?;
    cx.settle();
    Ok(())
}

fn wrap(cx: &mut Ctx) -> Result<Outcome, String> {
    let link = cx.claim()?;
    for seq in [0xFFFEu16, 0xFFFF, 0x0000, 0x0001] {
        one(cx, &link, seq)?;
    }
    let t = cx.telemetry()?;
    verdict(
        t.frames_shown == 4 && t.frames_dropped_stale == 0 && t.seq_gaps == 0,
        format!(
            "shown {} stale {} gaps {}",
            t.frames_shown, t.frames_dropped_stale, t.seq_gaps
        ),
    )
}

fn stale(cx: &mut Ctx) -> Result<Outcome, String> {
    let link = cx.claim()?;
    one(cx, &link, 100)?;
    let base = cx.telemetry()?;
    // None of these is `newer` than 100: an exact duplicate, one frame back,
    // long ago, and exactly half the sequence space away.
    for seq in [100u16, 99, 1, 100u16.wrapping_add(0x8000)] {
        one(cx, &link, seq)?;
    }
    let t = cx.telemetry()?;
    verdict(
        t.frames_dropped_stale == 4
            && t.frames_shown == base.frames_shown
            && t.frames_rejected == 0
            && t.seq_gaps == base.seq_gaps,
        format!(
            "stale {} shown {} -> {} rejected {} gaps {}",
            t.frames_dropped_stale,
            base.frames_shown,
            t.frames_shown,
            t.frames_rejected,
            t.seq_gaps
        ),
    )
}

fn gaps(cx: &mut Ctx) -> Result<Outcome, String> {
    let link = cx.claim()?;
    one(cx, &link, 10)?;
    one(cx, &link, 11)?;
    let base = cx.telemetry()?;
    // 12..=19 were never seen: eight of them.
    one(cx, &link, 20)?;
    let t = cx.telemetry()?;
    verdict(
        base.seq_gaps == 0
            && t.seq_gaps.wrapping_sub(base.seq_gaps) == 8
            && t.frames_shown.wrapping_sub(base.frames_shown) == 1,
        format!(
            "consecutive gaps {}, then +{} for 12..=19, shown +{}",
            base.seq_gaps,
            t.seq_gaps.wrapping_sub(base.seq_gaps),
            t.frames_shown.wrapping_sub(base.frames_shown)
        ),
    )
}

fn gaps_across_the_wrap(cx: &mut Ctx) -> Result<Outcome, String> {
    let link = cx.claim()?;
    // RFC 1982 only calls a jump of less than half the space "newer", so walk
    // up to 0xFFFF in quarters before stepping over the wrap.
    for seq in [0x4000u16, 0x8000, 0xC000, 0xFFFF] {
        one(cx, &link, seq)?;
    }
    let base = cx.telemetry()?;
    one(cx, &link, 2)?; // 0 and 1 were skipped
    let t = cx.telemetry()?;
    verdict(
        t.seq_gaps.wrapping_sub(base.seq_gaps) == 2
            && t.frames_shown.wrapping_sub(base.frames_shown) == 1,
        format!(
            "0xFFFF -> 2: gaps +{}, shown +{}",
            t.seq_gaps.wrapping_sub(base.seq_gaps),
            t.frames_shown.wrapping_sub(base.frames_shown)
        ),
    )
}

fn new_source_resets_seq(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 7.3 resets `last_seq` when a source is adopted; section 6.7
    // says the counters are free-running and only RESET_STATS clears them. So
    // a second sender starting at seq 0 after a five-figure first sender must
    // not be stale, and the counters must not have gone back to zero.
    let a = cx.claim()?;
    a.send_seq(codec::SOLID, F_KEY, 50_000, &dim())
        .map_err(|e| e.to_string())?;
    cx.settle();
    let base = cx.telemetry()?;

    // Let A's lock lapse so B is a takeover and not a lockout.
    std::thread::sleep(std::time::Duration::from_millis(
        screeny_proto::LOCK_MS as u64 + 150,
    ));
    let b = cx.second_source()?;
    b.send_seq(codec::SOLID, F_KEY, 0, &dim())
        .map_err(|e| e.to_string())?;
    cx.settle();
    let t = cx.telemetry()?;
    verdict(
        base.frames_shown == 1
            && t.frames_shown == 2
            && t.frames_dropped_stale == 0
            && t.frames_rx == 2,
        format!(
            "50000 then 0 from a new socket: shown {} stale {} rx {}",
            t.frames_shown, t.frames_dropped_stale, t.frames_rx
        ),
    )
}

fn identity(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 3.3: `d(frames_rx) = d(frames_shown) + d(superseded) +
    // d(decode)`. Provoked with a burst - which is what makes some of them
    // superseded - plus one undecodable frame, so more than one term is
    // non-zero on any device fast enough to draw everything.
    let mut link = cx.claim()?;
    for _ in 0..12 {
        link.send(codec::SOLID, F_KEY, &dim())
            .map_err(|e| e.to_string())?;
    }
    cx.settle();
    link.send(codec::SOLID, F_KEY, &[1, 2])
        .map_err(|e| e.to_string())?; // one byte short
    cx.settle();
    cx.settle();
    let t = cx.telemetry()?;
    let rhs = t
        .frames_shown
        .wrapping_add(t.frames_dropped_superseded)
        .wrapping_add(t.frames_dropped_decode);
    verdict(
        t.frames_rx == rhs && t.frames_dropped_decode == 1 && t.frames_rejected == 0,
        format!(
            "rx {} = shown {} + superseded {} + decode {}",
            t.frames_rx, t.frames_shown, t.frames_dropped_superseded, t.frames_dropped_decode
        ),
    )
}
