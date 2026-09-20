//! The wire-level conformance suite (card 080).
//!
//! One rule per MUST that an outside observer can actually check, each citing
//! the section of `docs/design/protocol-v1.md` it comes from, run against
//! *any* endpoint that speaks v1: `screeny-sim` on loopback, or the firmware
//! on the bench. Everything here is expressed in terms of the three things the
//! wire carries back - `TELEMETRY`'s eight counters and state byte, `BUSY`,
//! and `GET_INFO` - plus whether a datagram was answered at all.
//!
//! What is deliberately **not** here: bit-exactness of the displayed frame
//! (that needs `--dump-dir` against the simulator or the camera harness), the
//! panel's brightness model, and anything that needs injected faults or a
//! virtual clock. Those stay in `crates/sim/tests`.
//!
//! ## Safety
//!
//! The suite is meant to be pointed at the real panel, so it:
//!
//! - never sends `SET_WIFI` or a valid `REBOOT`, and never needs serial or a
//!   flash;
//! - never raises brightness above what it found, and puts it back on exit -
//!   on success, on failure, on a panic and on ctrl-c;
//! - never sets a name, an idle mode or an identify overlay it does not undo;
//! - sends nothing near full white and nothing that changes faster than a few
//!   hertz;
//! - has a bounded run time, printed before it starts, and one line of output
//!   per rule.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use screeny_proto::control::{IdleMode, Request, Telemetry};

use crate::link::{Control, FrameLink, OwnedReply};

mod arbitration;
mod codecs;
mod control;
mod framing;
mod sequence;
mod telemetry;

/// Rules that only a loopback target can answer honestly. Over real WiFi an
/// oversized datagram is fragmented and the device never sees it, so "the
/// counter did not move" would be indistinguishable from the rule passing.
pub const LOOPBACK_ONLY: u8 = 1 << 0;
/// Rules that wait out `HOLD_MS`. Ten seconds each; `--slow` opts in.
pub const SLOW: u8 = 1 << 1;
/// Rules that drive the panel to the firmware's brightness cap, which may be
/// brighter than what we found. `--cap-probe` opts in.
pub const CAP_PROBE: u8 = 1 << 2;
/// Rules that count an exact number of datagrams, and so are run a second
/// time before they are called a failure. This is UDP over WiFi: a lost probe
/// is not a lost MUST, and no counter can tell the two apart.
pub const RETRY: u8 = 1 << 3;

pub enum Outcome {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// The shape every rule ends with: a verdict and the measurement behind it.
pub fn verdict(ok: bool, detail: impl Into<String>) -> Result<Outcome, String> {
    let d = detail.into();
    Ok(if ok { Outcome::Pass(d) } else { Outcome::Fail(d) })
}

pub struct Rule {
    /// The spec section this comes from, e.g. `"7.4"`.
    pub section: &'static str,
    pub name: &'static str,
    /// Roughly how long it takes, for the estimate printed up front.
    pub secs: f32,
    pub flags: u8,
    pub run: fn(&mut Ctx) -> Result<Outcome, String>,
}

/// Every rule, in the order they run.
pub fn all() -> Vec<Rule> {
    let mut v = Vec::new();
    v.extend(control::rules());
    v.extend(framing::rules());
    v.extend(sequence::rules());
    v.extend(codecs::rules());
    v.extend(telemetry::rules());
    v.extend(arbitration::rules());
    v
}

// ---------------------------------------------------------------------------
// What a rule is handed
// ---------------------------------------------------------------------------

pub struct Ctx {
    pub frame_addr: SocketAddr,
    pub ctrl_addr: SocketAddr,
    pub ctrl: Control,
    /// True when the target is 127.0.0.0/8, which is the only place a
    /// deliberately oversized datagram can be delivered intact.
    pub loopback: bool,
    /// The brightness the device was at when the suite started. Nothing here
    /// ever asks for more than this.
    pub brightness_found: u8,
}

impl Ctx {
    pub fn telemetry(&mut self) -> Result<Telemetry, String> {
        match self.ctrl.request(Request::Telemetry)? {
            OwnedReply::Telemetry(t) => Ok(t),
            other => Err(format!("expected telemetry, got {other:?}")),
        }
    }

    /// `GET_INFO`, retried around section 5.5's one-per-second limit.
    ///
    /// `Control::request` retries with the same `req_id`, which the spec
    /// exempts - but only once the device has *answered* that id. A request
    /// that arrives just after somebody else's opens the window is suppressed
    /// on every attempt, so this backs off with fresh ids instead.
    pub fn get_info(&mut self) -> Result<Vec<u8>, String> {
        let t0 = Instant::now();
        loop {
            match self.ctrl.request(Request::GetInfo) {
                Ok(OwnedReply::Info(b)) => return Ok(b),
                Ok(other) => return Err(format!("expected a GET_INFO reply, got {other:?}")),
                Err(e) => {
                    if t0.elapsed() > Duration::from_secs(4) {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    pub fn reset_stats(&mut self) -> Result<(), String> {
        self.ctrl.request(Request::ResetStats).map(|_| ())
    }

    pub fn release(&mut self) -> Result<(), String> {
        self.ctrl.request(Request::Release).map(|_| ())
    }

    /// Give up whatever lock this host holds, let it settle, zero the
    /// counters, and hand back a fresh source (a new socket is a new 4-tuple,
    /// section 7.1). Every rule that sends frames starts here, so no rule
    /// inherits the previous one's lock or counters.
    pub fn claim(&mut self) -> Result<FrameLink, String> {
        self.release()?;
        std::thread::sleep(Duration::from_millis(80));
        self.reset_stats()?;
        FrameLink::connect(self.frame_addr).map_err(|e| e.to_string())
    }

    /// A second source, for the arbitration rules.
    pub fn second_source(&self) -> Result<FrameLink, String> {
        FrameLink::connect(self.frame_addr).map_err(|e| e.to_string())
    }

    /// Settle time for one frame to be drained, decoded and counted. Generous
    /// on purpose: this host runs several workers at once and the device sees
    /// 1-5 ms of WiFi jitter at 30 fps, so a tight window would report a
    /// conformance failure where there is only load.
    pub const SETTLE: Duration = Duration::from_millis(150);

    pub fn settle(&self) {
        std::thread::sleep(Self::SETTLE);
    }
}

/// A `CONTROL` datagram with an arbitrary opcode, flags and body, which
/// `Request::write` quite rightly will not build.
pub fn control_dgram(op: u8, flags: u8, req_id: u16, body: &[u8]) -> Vec<u8> {
    let mut d = vec![
        screeny_proto::MAGIC,
        (screeny_proto::VERSION << 4) | screeny_proto::TYPE_CONTROL,
        op,
        flags,
    ];
    d.extend_from_slice(&req_id.to_le_bytes());
    d.extend_from_slice(&(body.len() as u16).to_le_bytes());
    d.extend_from_slice(body);
    d
}

/// A `FRAME` datagram with the header written out by hand, so `len` and the
/// payload can disagree.
pub fn frame_dgram(ver_type: u8, codec: u8, flags: u8, seq: u16, len: u16, body: &[u8]) -> Vec<u8> {
    let mut d = vec![screeny_proto::MAGIC, ver_type, codec, flags];
    d.extend_from_slice(&seq.to_le_bytes());
    d.extend_from_slice(&len.to_le_bytes());
    d.extend_from_slice(body);
    d
}

/// One line of "what came back", for the detail column.
pub fn describe(r: &Option<Vec<u8>>) -> String {
    match r {
        None => "no reply".into(),
        Some(b) if b.len() >= 9 => {
            format!("op {:#04x} flags {:#04x} body[0] {:#04x}", b[2], b[3], b[8])
        }
        Some(b) => format!("{} bytes", b.len()),
    }
}

// ---------------------------------------------------------------------------
// Negative framing: "the frame port counted it and did not answer"
// ---------------------------------------------------------------------------

/// Send one datagram the frame port must reject, and check that it landed in
/// `frames_rejected` and nowhere else, and that nothing came back.
///
/// Retried up to three times before it is called a failure. This is UDP over
/// WiFi: a lost probe is not a lost MUST, and `frames_rejected` cannot tell
/// "the firmware ignored it" from "the air ate it". Sent as a burst these
/// were flaky on the real device - two runs in three counted three of four.
pub fn rejected_and_silent(cx: &mut Ctx, bytes: &[u8]) -> Result<Outcome, String> {
    let link = FrameLink::connect(cx.frame_addr).map_err(|e| e.to_string())?;
    let mut answered = 0usize;
    for attempt in 1..=3 {
        cx.reset_stats()?;
        let _ = link.poll();
        link.send_raw(bytes).map_err(|e| e.to_string())?;
        cx.settle();
        let t = cx.telemetry()?;
        answered += link.poll().len();
        let ok = t.frames_rejected == 1
            && t.frames_rx == 0
            && t.frames_shown == 0
            && t.frames_dropped_decode == 0
            && t.frames_dropped_stale == 0;
        if ok && answered == 0 {
            return verdict(true, format!("rejected 1, nothing answered ({attempt} try)"));
        }
        if attempt == 3 {
            return verdict(
                false,
                format!(
                    "rejected {} rx {} shown {} decode {} stale {}, {answered} replies",
                    t.frames_rejected,
                    t.frames_rx,
                    t.frames_shown,
                    t.frames_dropped_decode,
                    t.frames_dropped_stale
                ),
            );
        }
    }
    unreachable!()
}

/// Send one well-formed frame the device must *show*, and check the counters
/// agree. Retried like [`rejected_and_silent`], for the same reason.
pub fn shown(cx: &mut Ctx, codec: u8, flags: u8, payload: &[u8]) -> Result<(bool, String), String> {
    let mut link = cx.claim()?;
    for attempt in 1..=3u32 {
        link.send(codec, flags, payload).map_err(|e| e.to_string())?;
        cx.settle();
        let t = cx.telemetry()?;
        if t.frames_shown >= 1 && t.frames_dropped_decode == 0 && t.frames_rejected == 0 {
            return Ok((
                true,
                format!(
                    "shown {} last_codec {:#04x} ({attempt} try)",
                    t.frames_shown, t.last_codec
                ),
            ));
        }
        if attempt == 3 {
            return Ok((
                false,
                format!(
                    "shown {} decode {} rejected {} rx {}",
                    t.frames_shown, t.frames_dropped_decode, t.frames_rejected, t.frames_rx
                ),
            ));
        }
    }
    unreachable!()
}

// ---------------------------------------------------------------------------
// Restoring what the suite touched
// ---------------------------------------------------------------------------

/// Everything the suite changes, and how to put it back.
///
/// [`RestoreGuard`] covers a normal return and a panic; the ctrl-c handler
/// installed by [`run`] covers the third case. Both go through
/// [`Restore::apply`], which opens its own socket because the handler runs on
/// another thread.
#[derive(Clone, Copy)]
pub struct Restore {
    pub ctrl_addr: SocketAddr,
    pub brightness: u8,
    pub idle: u8,
}

impl Restore {
    /// Put the device back: stop any `IDENTIFY` overlay, restore brightness,
    /// restore the idle mode, and give up the lock.
    pub fn apply(&self) -> Result<Telemetry, String> {
        let mut c = Control::connect(self.ctrl_addr).map_err(|e| e.to_string())?;
        c.request(Request::Identify { duration_ms: 0 })?;
        c.request(Request::SetBrightness(self.brightness))?;
        if let Some(m) = IdleMode::from_u8(self.idle) {
            c.request(Request::SetIdle(m))?;
        }
        c.request(Request::Release)?;
        match c.request(Request::Telemetry)? {
            OwnedReply::Telemetry(t) => Ok(t),
            other => Err(format!("expected telemetry, got {other:?}")),
        }
    }
}

/// Applies a [`Restore`] when it goes out of scope, however it goes out of
/// scope. Disarmed once the runner has applied it explicitly and reported
/// what came back.
pub struct RestoreGuard {
    pub inner: Restore,
    pub armed: bool,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.inner.apply();
        }
    }
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

pub struct Opts {
    pub frame_addr: SocketAddr,
    pub ctrl_addr: SocketAddr,
    /// Run only the rules whose section starts with this.
    pub only: Option<String>,
    pub slow: bool,
    pub cap_probe: bool,
    /// The idle mode to leave the device in. There is no way to read the
    /// current one over the wire (card 131), so it has to be stated; 0
    /// `STATUS` is the spec's default.
    pub restore_idle: u8,
}

pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl Summary {
    pub fn ok(&self) -> bool {
        self.failed == 0
    }
}

/// Print what the suite is about to do, run it, and put the device back.
pub fn run(opts: &Opts) -> Result<Summary, String> {
    let rules: Vec<Rule> = all()
        .into_iter()
        .filter(|r| match &opts.only {
            None => true,
            Some(p) => r.section.starts_with(p.as_str()),
        })
        .collect();
    if rules.is_empty() {
        return Err(format!(
            "no rules match --only {:?}",
            opts.only.clone().unwrap_or_default()
        ));
    }

    let mut ctrl = Control::connect(opts.ctrl_addr).map_err(|e| e.to_string())?;
    // One round trip before anything else, so "the device is not there" is a
    // clear error rather than sixty failures.
    let found = match ctrl.request(Request::Telemetry)? {
        OwnedReply::Telemetry(t) => t,
        other => return Err(format!("expected telemetry, got {other:?}")),
    };
    let loopback = opts.frame_addr.ip().is_loopback();

    let will_run = |r: &Rule| {
        (r.flags & LOOPBACK_ONLY == 0 || loopback)
            && (r.flags & SLOW == 0 || opts.slow)
            && (r.flags & CAP_PROBE == 0 || opts.cap_probe)
    };
    let estimate: f32 = rules.iter().filter(|r| will_run(r)).map(|r| r.secs).sum();

    println!(
        "screeny-probe conformance: {} rules against {} / {} ({})",
        rules.len(),
        opts.frame_addr,
        opts.ctrl_addr,
        if loopback { "loopback" } else { "remote" }
    );
    println!(
        "  brightness found {}, uptime {} s, state {}; estimated {:.0} s",
        found.brightness,
        found.uptime_ms / 1000,
        crate::state_name(found.state),
        estimate
    );
    if !opts.slow {
        println!("  (--slow adds the HOLD_MS rules, about 25 s more)");
    }
    println!();

    // Armed from here on: every exit path puts the device back.
    let restore = Restore {
        ctrl_addr: opts.ctrl_addr,
        brightness: found.brightness,
        idle: opts.restore_idle,
    };
    let mut guard = RestoreGuard {
        inner: restore,
        armed: true,
    };
    let on_interrupt = restore;
    if let Err(e) = ctrlc::set_handler(move || {
        eprintln!("\ninterrupted: restoring the device");
        match on_interrupt.apply() {
            Ok(t) => eprintln!("  brightness {} restored", t.brightness),
            Err(e) => eprintln!("  RESTORE FAILED: {e}"),
        }
        std::process::exit(130);
    }) {
        eprintln!("warning: no ctrl-c handler ({e}); ctrl-c will not restore");
    }

    let mut cx = Ctx {
        frame_addr: opts.frame_addr,
        ctrl_addr: opts.ctrl_addr,
        ctrl,
        loopback,
        brightness_found: found.brightness,
    };

    let mut s = Summary {
        passed: 0,
        failed: 0,
        skipped: 0,
    };
    let total = rules.len();
    for (i, r) in rules.iter().enumerate() {
        let outcome = if r.flags & LOOPBACK_ONLY != 0 && !loopback {
            Outcome::Skip("loopback only: the radio fragments it away".into())
        } else if r.flags & SLOW != 0 && !opts.slow {
            Outcome::Skip("needs --slow (waits out HOLD_MS)".into())
        } else if r.flags & CAP_PROBE != 0 && !opts.cap_probe {
            Outcome::Skip("needs --cap-probe (would raise brightness to the cap)".into())
        } else {
            let first = match (r.run)(&mut cx) {
                Ok(o) => o,
                Err(e) => Outcome::Fail(format!("error: {e}")),
            };
            match first {
                Outcome::Fail(why) if r.flags & RETRY != 0 => match (r.run)(&mut cx) {
                    Ok(Outcome::Fail(again)) => {
                        Outcome::Fail(format!("{again} (first try: {why})"))
                    }
                    Ok(Outcome::Pass(d)) => Outcome::Pass(format!("{d} on the second try")),
                    Ok(o) => o,
                    Err(e) => Outcome::Fail(format!("error: {e} (first try: {why})")),
                },
                other => other,
            }
        };
        let (tag, detail) = match &outcome {
            Outcome::Pass(d) => ("PASS", d),
            Outcome::Fail(d) => ("FAIL", d),
            Outcome::Skip(d) => ("SKIP", d),
        };
        match outcome {
            Outcome::Pass(_) => s.passed += 1,
            Outcome::Fail(_) => s.failed += 1,
            Outcome::Skip(_) => s.skipped += 1,
        }
        println!(
            "[{:>2}/{}] {:<5} {:<56} {tag}  {detail}",
            i + 1,
            total,
            r.section,
            r.name
        );
    }

    println!();
    // Take the restore off the Drop path so its result can be reported.
    guard.armed = false;
    match guard.inner.apply() {
        Ok(t) => println!(
            "restored: brightness {} (found {}), idle mode {}, lock released, state {}",
            t.brightness,
            found.brightness,
            opts.restore_idle,
            crate::state_name(t.state)
        ),
        Err(e) => {
            println!("RESTORE FAILED: {e}");
            s.failed += 1;
        }
    }
    println!(
        "conformance: {} passed, {} failed, {} skipped",
        s.passed, s.failed, s.skipped
    );
    Ok(s)
}
