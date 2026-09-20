//! Spec section 7: source identity, the lock, takeover, and the idle state
//! machine, driven from two sockets - which is the only way to test it, since
//! a source is a UDP 4-tuple and "another sender" means another socket.
//!
//! This is what `screeny-probe lock-test` used to be. It is here so there is
//! one suite rather than two, and `lock-test` is now an alias for
//! `conformance --only 7`.

use std::time::{Duration, Instant};

use screeny_proto::control::{busy_reason, state as tstate, Request};
use screeny_proto::dec::codec;
use screeny_proto::{F_FINAL, F_KEY, F_STATS_REQ};

use super::{verdict, Ctx, Outcome, Rule, RETRY, SLOW};
use crate::enc;
use crate::link::{FrameLink, OwnedReply};

pub fn rules() -> Vec<Rule> {
    vec![
        Rule {
            section: "7.4",
            name: "the first frame adopts a source and the state becomes LIVE",
            secs: 1.0,
            flags: RETRY,
            run: adopt,
        },
        Rule {
            section: "7.4",
            name: "a second source inside LOCK_MS is rejected and told BUSY",
            secs: 1.5,
            flags: RETRY,
            run: locked_out,
        },
        Rule {
            section: "6.2",
            name: "BUSY carries reason LOCKED and remaining <= LOCK_MS",
            secs: 1.5,
            flags: RETRY,
            run: busy_body,
        },
        Rule {
            section: "6.2",
            name: "BUSY is rate-limited to one per second per source",
            secs: 2.5,
            flags: RETRY,
            run: busy_rate_limit,
        },
        Rule {
            section: "6.2",
            name: "a locked-out sender gets BUSY and not TELEMETRY",
            secs: 1.5,
            flags: RETRY,
            run: busy_not_telemetry,
        },
        Rule {
            section: "7.4",
            name: "after LOCK_MS of silence a new source takes over, with no BUSY",
            secs: 1.5,
            flags: RETRY,
            run: takeover,
        },
        Rule {
            section: "7.4",
            name: "a displayed FINAL releases the lock at once",
            secs: 1.5,
            flags: RETRY,
            run: final_releases,
        },
        Rule {
            section: "7.4",
            name: "a FINAL that fails to decode releases nothing",
            secs: 1.5,
            flags: RETRY,
            run: final_that_does_not_decode,
        },
        Rule {
            section: "7.4",
            name: "RELEASE from the same IP on another port gives up the lock",
            secs: 1.0,
            flags: RETRY,
            run: release,
        },
        Rule {
            section: "7.3",
            name: "STREAM_TIMEOUT_MS with no frames -> HOLD, lock released",
            secs: 2.0,
            flags: RETRY,
            run: stream_timeout,
        },
        Rule {
            section: "7.3",
            name: "HOLD_MS in HOLD -> IDLE",
            secs: 13.0,
            flags: SLOW | RETRY,
            run: hold_to_idle,
        },
        Rule {
            section: "7.5",
            name: "HOLD_FOREVER stays in HOLD and still releases the lock",
            secs: 13.0,
            flags: SLOW | RETRY,
            run: hold_forever,
        },
    ]
}

fn dim() -> Vec<u8> {
    enc::solid([35, 30, 45]).0
}

const LOCK_MS: u64 = screeny_proto::LOCK_MS as u64;
const STREAM_TIMEOUT_MS: u64 = screeny_proto::STREAM_TIMEOUT_MS as u64;
const HOLD_MS: u64 = screeny_proto::HOLD_MS as u64;

/// Keep a source's lock alive for `ms`, sending at about 30 fps.
fn keep_alive(link: &mut FrameLink, ms: u64) -> Result<(), String> {
    let t0 = Instant::now();
    let p = dim();
    while t0.elapsed() < Duration::from_millis(ms) {
        link.send(codec::SOLID, F_KEY, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(33));
    }
    Ok(())
}

fn busies(link: &FrameLink) -> Vec<(u8, u32)> {
    link.poll()
        .into_iter()
        .filter_map(|r| match r {
            OwnedReply::Busy {
                reason,
                remaining_ms,
            } => Some((reason, remaining_ms)),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------

fn adopt(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut a = cx.claim()?;
    keep_alive(&mut a, 200)?;
    let t = cx.telemetry()?;
    verdict(
        t.state == tstate::LIVE && t.frames_shown >= 1 && t.frames_rejected == 0,
        format!(
            "state {} shown {} rejected {}",
            crate::state_name(t.state),
            t.frames_shown,
            t.frames_rejected
        ),
    )
}

fn locked_out(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let base = cx.telemetry()?;

    let b = cx.second_source()?;
    let p = dim();
    for _ in 0..5 {
        a.send(codec::SOLID, F_KEY, &p).map_err(|e| e.to_string())?;
        b.send_seq(codec::SOLID, F_KEY, 1, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(40));
    }
    cx.settle();
    let t = cx.telemetry()?;
    verdict(
        t.frames_rejected > base.frames_rejected && t.state == tstate::LIVE,
        format!(
            "rejected {} -> {}, state {}",
            base.frames_rejected,
            t.frames_rejected,
            crate::state_name(t.state)
        ),
    )
}

fn busy_body(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.2: reason 0 LOCKED in v1, and `lock_holder_ms_remaining` is
    // `LOCK_MS - (now - last_frame_at)` clamped at 0 - "a sender that waits
    // that long and retries is guaranteed the takeover branch".
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let b = cx.second_source()?;
    let _ = b.poll();
    let p = dim();

    let mut got: Option<(u8, u32)> = None;
    for _ in 0..6 {
        a.send(codec::SOLID, F_KEY, &p).map_err(|e| e.to_string())?;
        b.send_seq(codec::SOLID, F_KEY, 1, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(40));
        if let Some(x) = busies(&b).into_iter().next() {
            got = Some(x);
            break;
        }
    }
    match got {
        None => verdict(false, "no BUSY arrived on B's frame socket"),
        Some((reason, remaining)) => verdict(
            reason == busy_reason::LOCKED && u64::from(remaining) <= LOCK_MS,
            format!("reason {reason} remaining {remaining} ms (LOCK_MS {LOCK_MS})"),
        ),
    }
}

fn busy_rate_limit(cx: &mut Ctx) -> Result<Outcome, String> {
    // BUSY_MIN_INTERVAL_MS is 1000, so 2.1 s of continuous knocking is at
    // most three packets - and about 60 rejected frames.
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let b = cx.second_source()?;
    let _ = b.poll();
    let p = dim();

    let mut count = 0usize;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(2_100) {
        a.send(codec::SOLID, F_KEY, &p).map_err(|e| e.to_string())?;
        b.send_seq(codec::SOLID, F_KEY, 1, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(33));
        count += busies(&b).len();
    }
    let t = cx.telemetry()?;
    verdict(
        (1..=3).contains(&count),
        format!(
            "{count} BUSY packets for {} rejected frames over 2.1 s",
            t.frames_rejected
        ),
    )
}

fn busy_not_telemetry(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 6.2: a frame that admission *rejects* gets a BUSY instead of
    // telemetry - "its sender is not entitled to the counters of a stream
    // that is not its own" - even when it set STATS_REQ.
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let b = cx.second_source()?;
    let _ = b.poll();
    let p = dim();

    let mut busy = 0usize;
    let mut telem = 0usize;
    for _ in 0..6 {
        a.send(codec::SOLID, F_KEY, &p).map_err(|e| e.to_string())?;
        b.send_seq(codec::SOLID, F_KEY | F_STATS_REQ, 1, &p)
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(40));
        for r in b.poll() {
            match r {
                OwnedReply::Busy { .. } => busy += 1,
                OwnedReply::Telemetry(_) => telem += 1,
                _ => {}
            }
        }
    }
    verdict(
        busy >= 1 && telem == 0,
        format!("{busy} BUSY, {telem} TELEMETRY to the locked-out source"),
    )
}

fn takeover(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let before = cx.telemetry()?;

    // Past LOCK_MS but inside STREAM_TIMEOUT_MS, so this really is a takeover
    // and not just "the panel was free".
    std::thread::sleep(Duration::from_millis(LOCK_MS + 150));
    let mut b = cx.second_source()?;
    let _ = b.poll();
    keep_alive(&mut b, 150)?;
    cx.settle();
    let late_busy = busies(&b).len();
    let t = cx.telemetry()?;
    verdict(
        t.state == tstate::LIVE
            && t.frames_shown > before.frames_shown
            && t.frames_rejected == before.frames_rejected
            && late_busy == 0,
        format!(
            "state {} shown {} -> {}, rejected {} -> {}, {late_busy} BUSY",
            crate::state_name(t.state),
            before.frames_shown,
            t.frames_shown,
            before.frames_rejected,
            t.frames_rejected
        ),
    )
}

fn final_releases(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    a.send(codec::SOLID, F_KEY | F_FINAL, &dim())
        .map_err(|e| e.to_string())?;
    cx.settle();
    let held = cx.telemetry()?;

    // "so any sender may take over instantly" - well inside LOCK_MS.
    let mut b = cx.second_source()?;
    let _ = b.poll();
    b.send(codec::SOLID, F_KEY, &dim())
        .map_err(|e| e.to_string())?;
    cx.settle();
    let late_busy = busies(&b).len();
    let t = cx.telemetry()?;
    verdict(
        held.state == tstate::HOLD && t.state == tstate::LIVE && late_busy == 0,
        format!(
            "FINAL -> {}, then B -> {} ({late_busy} BUSY)",
            crate::state_name(held.state),
            crate::state_name(t.state)
        ),
    )
}

fn final_that_does_not_decode(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 7.4: the lock is released when a FINAL frame is *displayed*. A
    // FINAL whose payload is corrupt was never displayed and releases
    // nothing, so one damaged packet cannot hand the panel to a stranger.
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    a.send(codec::SOLID, F_KEY | F_FINAL, &[1, 2])
        .map_err(|e| e.to_string())?; // one byte short
    cx.settle();
    let after = cx.telemetry()?;

    // B knocks well inside LOCK_MS: it must still be locked out.
    let b = cx.second_source()?;
    let _ = b.poll();
    let rej_before = after.frames_rejected;
    b.send_seq(codec::SOLID, F_KEY, 1, &dim())
        .map_err(|e| e.to_string())?;
    cx.settle();
    let t = cx.telemetry()?;
    verdict(
        after.state == tstate::LIVE
            && after.frames_dropped_decode == 1
            && t.frames_rejected > rej_before,
        format!(
            "state {} decode {} rejected {} -> {}",
            crate::state_name(after.state),
            after.frames_dropped_decode,
            rej_before,
            t.frames_rejected
        ),
    )
}

fn release(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 7.4: "the active source sends RELEASE (from any port on the
    // same IP)". The control socket is never the frame stream's port, so a
    // 4-tuple match would make the opcode impossible to use.
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let live = cx.telemetry()?;
    let ctrl_port = cx.ctrl.local().map_err(|e| e.to_string())?.port();
    let frame_port = a.local().map_err(|e| e.to_string())?.port();
    cx.ctrl.request(Request::Release)?;
    std::thread::sleep(Duration::from_millis(120));
    let t = cx.telemetry()?;
    verdict(
        live.state == tstate::LIVE && t.state == tstate::HOLD && ctrl_port != frame_port,
        format!(
            "{} -> {} (control port {ctrl_port}, stream port {frame_port})",
            crate::state_name(live.state),
            crate::state_name(t.state)
        ),
    )
}

fn stream_timeout(cx: &mut Ctx) -> Result<Outcome, String> {
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    let live = cx.telemetry()?;
    std::thread::sleep(Duration::from_millis(STREAM_TIMEOUT_MS + 300));
    let t = cx.telemetry()?;
    verdict(
        live.state == tstate::LIVE && t.state == tstate::HOLD,
        format!(
            "{} -> {} after {} ms of silence",
            crate::state_name(live.state),
            crate::state_name(t.state),
            STREAM_TIMEOUT_MS + 300
        ),
    )
}

fn hold_to_idle(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 7.3/7.5, at the spec's own HOLD_MS. Eleven seconds of wall
    // clock, which is why this one is behind --slow.
    cx.ctrl
        .request(Request::SetIdle(screeny_proto::control::IdleMode::Status))?;
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    std::thread::sleep(Duration::from_millis(STREAM_TIMEOUT_MS + 300));
    let held = cx.telemetry()?;
    std::thread::sleep(Duration::from_millis(HOLD_MS + 500));
    let t = cx.telemetry()?;
    verdict(
        held.state == tstate::HOLD && t.state == tstate::IDLE,
        format!(
            "{} -> {} after HOLD_MS",
            crate::state_name(held.state),
            crate::state_name(t.state)
        ),
    )
}

fn hold_forever(cx: &mut Ctx) -> Result<Outcome, String> {
    // Section 7.5 mode 1: "the HOLD -> IDLE transition never fires, so the
    // state byte stays HOLD indefinitely and the lock is still released on
    // the stream timeout as usual." Put back to STATUS on the way out - and
    // the suite's own restore does it again for good measure.
    cx.ctrl
        .request(Request::SetIdle(screeny_proto::control::IdleMode::HoldForever))?;
    let mut a = cx.claim()?;
    keep_alive(&mut a, 150)?;
    std::thread::sleep(Duration::from_millis(STREAM_TIMEOUT_MS + HOLD_MS + 800));
    let t = cx.telemetry()?;
    cx.ctrl
        .request(Request::SetIdle(screeny_proto::control::IdleMode::Status))?;
    verdict(
        t.state == tstate::HOLD,
        format!(
            "{} after STREAM_TIMEOUT_MS + HOLD_MS",
            crate::state_name(t.state)
        ),
    )
}
