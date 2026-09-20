//! The pacer (spec 9.1): absolute scheduling, and skipping rather than
//! bursting when something runs long.
//!
//! These tests are wall-clock measurements, so they take the lock below and
//! run one at a time - two 30 fps streams racing each other on a loaded
//! machine would measure the machine, not the pacer. They are also
//! deliberately cheap on the encoder: a near-solid frame takes the `SOLID`
//! shortcut, so what is being timed is the schedule and not the chooser.
//! `benches/encode` is where encode cost is measured.
//!
//! # What is the pacer's and what is the operating system's
//!
//! The pacer owns **the schedule it asks for**: slot `n` is due at
//! `start + n * period`, absolutely, so error cannot accumulate. The OS owns
//! **when the thread actually wakes**, and on a busy laptop it is late by a
//! few milliseconds several times a minute. Every number below is the first
//! quantity contaminated by the second, so the assertions are chosen to be
//! blind to the contamination (card 093):
//!
//! - **Rate** is the slope of lateness across the run, not a frame count
//!   divided by a wall-clock window, and not the median interval either. See
//!   [`Timeline::rate_error`], which is where the choice is argued and the
//!   two easier estimators are shown failing. It means the same thing at
//!   every run length, which a count over a window with ragged ends does not:
//!   measured over 50 runs on a loaded host it stayed inside +-0.13% whether
//!   the run was 2 s or 10 s, while `SendStats::actual_fps()` read 30.99 at
//!   2 s and 30.19 at 10 s on the same pacer.
//! - **Drift** is measured once over the whole run, normalised by the number
//!   of *slots* rather than of frames so a legitimate skip does not read as
//!   drift. It does not grow with the run for a correct pacer, so its
//!   tolerance does not have to either.
//! - **Bursts** are only counted when the frame before them was on time. An
//!   absolute schedule answers a late frame by shortening the next gap - that
//!   is the schedule working, and the old `min_gap >= 0.4 * period` assertion
//!   punished it. A pacer that rebases onto "now" and fires twice in a row
//!   with nothing late behind it is the real fault, and the stall test below
//!   is where it is provoked deliberately.
//!
//! Set `SCREENY_PACING_SECS` to shorten the long run when iterating; every
//! assertion here holds at 2 s exactly as it does at the 10 s default.

mod common;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Receiver, RxConfig, RxState};
use screeny::sender::{period_of, sleep_until};
use screeny::{Frame, FrameTime, Sender, SenderConfig};

/// Serialises the wall-clock tests in this file.
static CLOCK: Mutex<()> = Mutex::new(());

/// What counts as a burst: two sends closer together than this fraction of
/// the frame period.
///
/// A real catch-up burst puts frames microseconds apart - under a percent of
/// a period - so this only has to sit clear of ordinary scheduler jitter,
/// which on a loaded laptop moves a send by a few milliseconds either way.
const BURST_FRACTION: f64 = 0.4;

/// How late a wake-up has to be before a short gap after it is the absolute
/// schedule catching up rather than a burst. A gap below `BURST_FRACTION` of
/// a period can only follow a wake-up at least `1 - BURST_FRACTION` of a
/// period late, so anything at or under this is unexplained.
const EXPLAINS_A_SHORT_GAP: f64 = 1.0 - BURST_FRACTION;

/// How far off its slot a wake-up has to be before it counts as the host
/// disturbing the measurement rather than ordinary jitter, in periods.
///
/// Half a period, because that is where a late wake-up starts to distort the
/// *next* gap enough to matter. Ordinary lateness on this bench is much
/// smaller and much more boring: `thread::sleep` on macOS overshoots by a few
/// milliseconds, so a typical wake-up at 30 fps is 3-4 ms past its slot and
/// every wake-up in a run is past it by about the same amount, which is why
/// the intervals are still 33.333 ms. Reported, never asserted on: the pacer
/// cannot make itself late, it can only ask for a time and be handed a later
/// one.
const DISTURBED: f64 = 0.5;

fn secs(default: f64) -> f64 {
    std::env::var("SCREENY_PACING_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn rx() -> Receiver {
    Receiver::start(RxConfig {
        keep_pixels: false,
        ..RxConfig::default()
    })
}

fn stream_cfg(fps: f64) -> SenderConfig {
    SenderConfig {
        fps,
        // No adaptation: this is a measurement of the schedule, and a rate
        // change mid-run would be measuring something else.
        adapt: false,
        stats_interval: Duration::from_secs(5),
        ..SenderConfig::default()
    }
}

/// One wake-up: the slot the pacer chose, and the instant it handed that
/// slot's frame over to be rendered.
#[derive(Clone, Copy)]
struct Tick {
    slot: u64,
    at: Instant,
}

type Log = Arc<Mutex<Vec<Tick>>>;

/// A source that records every wake-up and changes one pixel per frame, so
/// every frame is distinct but the encoder's `SOLID` shortcut still applies.
///
/// `work` runs before the frame is painted, which is where a test that wants
/// to stall the pacer puts the stall.
fn recording_source(
    log: &Log,
    limit: Option<u64>,
    mut work: impl FnMut(u64),
) -> impl screeny::FrameSource {
    let log = Arc::clone(log);
    screeny::FnSource::new("cheap", move |t: FrameTime, out: &mut Frame| {
        if !limit.is_none_or(|n| t.index < n) {
            return false;
        }
        // First thing, before any work: this is the moment `sleep_until`
        // handed control back, which is the closest the test can stand to the
        // schedule itself.
        let at = Instant::now();
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Tick { slot: t.index, at });
        work(t.index);
        let v = (t.index % 200) as u8;
        for p in 0..screeny::proto::NPIX {
            out.set_at(p, [v, 40, 90]);
        }
        true
    })
}

/// The run as the pacer drove it: which slot each frame belonged to and when
/// the pacer woke for it.
struct Timeline {
    period: f64,
    ticks: Vec<Tick>,
}

impl Timeline {
    fn take(log: &Log, fps: f64) -> Timeline {
        let ticks = std::mem::take(
            &mut *log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        assert!(
            ticks.len() >= 8,
            "only {} wake-ups recorded: the run never got going",
            ticks.len()
        );
        Timeline {
            period: period_of(fps).as_secs_f64(),
            ticks,
        }
    }

    fn len(&self) -> usize {
        self.ticks.len()
    }

    /// Slots between the first and last wake-up, skipped ones included.
    fn slots(&self) -> u64 {
        self.ticks[self.len() - 1].slot - self.ticks[0].slot
    }

    /// Slots the pacer passed over. Should agree with
    /// `SendStats::frames_skipped`.
    fn skipped(&self) -> u64 {
        self.slots() + 1 - self.len() as u64
    }

    fn span(&self) -> f64 {
        self.ticks[self.len() - 1]
            .at
            .duration_since(self.ticks[0].at)
            .as_secs_f64()
    }

    /// One estimate of the frame period per pair of wake-ups, each divided by
    /// the slots between them so a skip reads as a longer wait and not a
    /// longer period.
    fn per_slot(&self) -> Vec<f64> {
        self.ticks
            .windows(2)
            .map(|w| {
                w[1].at.duration_since(w[0].at).as_secs_f64() / (w[1].slot - w[0].slot) as f64
            })
            .collect()
    }

    /// Raw interval between consecutive wake-ups, in periods.
    fn gaps(&self) -> Vec<f64> {
        self.ticks
            .windows(2)
            .map(|w| w[1].at.duration_since(w[0].at).as_secs_f64() / self.period)
            .collect()
    }

    /// How far each wake-up sat from where the absolute schedule put it, in
    /// periods, anchored on the first wake-up. Never negative for a pacer
    /// that schedules absolutely and a `sleep_until` that does not return
    /// early; positive values are the OS, or a pacer that rebased its grid.
    fn lateness(&self) -> Vec<f64> {
        let t0 = self.ticks[0];
        self.ticks
            .iter()
            .map(|t| {
                (t.at.duration_since(t0.at).as_secs_f64() - (t.slot - t0.slot) as f64 * self.period)
                    / self.period
            })
            .collect()
    }

    /// Seconds by which the whole run ran long or short of the slots it
    /// covered. Does not grow with run length for a correct pacer.
    fn drift(&self) -> f64 {
        self.span() - self.slots() as f64 * self.period
    }

    /// How far the rate the run actually held sits from the rate that was
    /// asked for, as a fraction: `+0.01` is a frame period 1% too long.
    ///
    /// It is the slope of lateness across the run - the median of the first
    /// half against the median of the second half - and neither of the two
    /// obvious estimators does the job:
    ///
    /// - The **median interval** is bimodal on this bench. `thread::sleep`
    ///   overshoots by about 4 ms, more than `sleep_until`'s 1 ms spin
    ///   window, so a wake-up sits either on its slot or about 4 ms past it,
    ///   and every flip between the two states makes one interval long and
    ///   the next one short. The intervals pile up at three values instead of
    ///   one, and with a few dozen of them under load the median settles on a
    ///   side pile: measured at 33.99 ms on a 2 s run whose mean interval was
    ///   33.48 ms.
    /// - The **mean interval** is [`Timeline::drift`] rewritten, which is a
    ///   two-sample estimator: it is the first wake-up against the last, so
    ///   its noise is one wake-up's lateness however long the run is. Over
    ///   10 s that is nothing; over 2 s it is half the 1% budget.
    ///
    /// Medians of halves shrug off the flips, and the two of them are about
    /// half the run apart, so the estimate gets better the longer the run -
    /// which is what lets the same +-1% hold at 2 s and at 10 s.
    fn rate_error(&self) -> f64 {
        let late = self.lateness();
        let half = late.len() / 2;
        let lo = median(&sorted(late[..half].to_vec()));
        let hi = median(&sorted(late[half..].to_vec()));
        (hi - lo) / (self.slots() as f64 / 2.0)
    }

    /// Short gaps with nothing behind them to explain the catch-up: the
    /// definition of a burst that a busy host cannot manufacture, because a
    /// busy host can only ever make the *previous* wake-up late.
    fn unexplained_bursts(&self) -> Vec<usize> {
        let (gaps, late) = (self.gaps(), self.lateness());
        (0..gaps.len())
            .filter(|&i| gaps[i] < BURST_FRACTION && late[i] <= EXPLAINS_A_SHORT_GAP)
            .collect()
    }

    /// Wake-ups the host held up by more than [`DISTURBED`] of a period. The
    /// pacer cannot cause these, so this is a reading of the scheduler and it
    /// goes in every failure message: a run with a lot of them was measured
    /// on a machine too busy for the finer numbers to mean much.
    fn disturbed(&self) -> usize {
        self.lateness().iter().filter(|l| **l > DISTURBED).count()
    }

    /// The same run as the receiver saw it: one arrival per wake-up, against
    /// the slot the pacer meant it for, so every measure above can be asked
    /// of the wire as well as of the schedule.
    fn at_the_receiver(&self, arrivals: &[Instant]) -> Timeline {
        assert_eq!(arrivals.len(), self.len(), "an arrival per paced frame");
        Timeline {
            period: self.period,
            ticks: self
                .ticks
                .iter()
                .zip(arrivals)
                .map(|(t, at)| Tick { slot: t.slot, at: *at })
                .collect(),
        }
    }

    fn summary(&self) -> String {
        let late = sorted(self.lateness());
        let mut s = String::new();
        let _ = write!(
            s,
            "{} wake-ups over {:.3} s, {} slots ({} skipped); \
             period {:+.3}% of {:.4} ms (median interval {:.4} ms); drift {:+.1} ms; \
             lateness min {:.2} ms median {:.2} ms p95 {:.2} ms max {:.2} ms; \
             {} wake-ups held up past half a period by the host",
            self.len(),
            self.span(),
            self.slots() + 1,
            self.skipped(),
            self.rate_error() * 100.0,
            self.period * 1e3,
            median(&sorted(self.per_slot())) * 1e3,
            self.drift() * 1e3,
            late[0] * self.period * 1e3,
            median(&late) * self.period * 1e3,
            pct(&late, 0.95) * self.period * 1e3,
            late[late.len() - 1] * self.period * 1e3,
            self.disturbed(),
        );
        s
    }
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a duration"));
    v
}

/// `v` must be sorted.
fn median(v: &[f64]) -> f64 {
    if v.len() % 2 == 1 {
        v[v.len() / 2]
    } else {
        (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0
    }
}

/// `v` must be sorted.
fn pct(v: &[f64], p: f64) -> f64 {
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// When each frame of the paced stream arrived. `RxState::paced` is what
/// leaves the `FINAL` frame out, and says why.
fn paced_arrivals(state: &RxState) -> Vec<Instant> {
    state.paced().map(|f| f.at).collect()
}

/// The pacer holds the asked-for rate, keeps an absolute schedule, and does
/// not burst - at any run length. `SCREENY_PACING_SECS` shortens it.
#[test]
fn holds_thirty_fps_within_one_percent() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let secs = secs(10.0);
    let period = period_of(30.0).as_secs_f64();
    let rx = rx();
    let mut sender = Sender::connect(rx.device(), stream_cfg(30.0)).expect("connect");

    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        let d = Duration::from_secs_f64(secs);
        std::thread::spawn(move || {
            std::thread::sleep(d);
            s.store(true, Ordering::SeqCst);
        });
    }
    let log: Log = Log::default();
    let mut src = recording_source(&log, None, |_| {});
    sender.run(&mut src, &stop).expect("run");

    let sent = sender.stats().frames_sent;
    let skipped = sender.stats().frames_skipped;
    let min_gap = sender.stats().min_gap.unwrap();
    drop(sender);

    let state = rx.shutdown();
    let tl = Timeline::take(&log, 30.0);
    let report = tl.summary();
    eprintln!("{secs} s at 30 fps: {report}");

    // Bookkeeping first, so a later failure is about timing and not about
    // frames that never happened. One send per wake-up, plus the `FINAL`
    // frame `finish()` adds; every slot the schedule passed over counted.
    assert_eq!(
        sent,
        tl.len() as u64 + 1,
        "{} wake-ups but {sent} frames sent (one FINAL frame expected on top)",
        tl.len()
    );
    assert_eq!(skipped, tl.skipped(), "skip accounting disagrees: {report}");

    // The rate: the slope of lateness across the run, which is the estimator
    // that survives a busy host and means the same thing at every run length.
    // See `Timeline::rate_error`.
    assert!(
        tl.rate_error().abs() <= 0.01,
        "the run held a frame period {:+.3}% off 33.3333 ms, outside +-1%: {report}",
        tl.rate_error() * 100.0
    );

    // No accumulated drift. Slot-normalised, so a skip forced by the host is
    // not mistaken for the schedule slipping. This bound is a constant
    // because a correct pacer's drift is a constant - it is one wake-up's
    // lateness at each end of the run, not a per-frame error times the number
    // of frames.
    assert!(
        tl.drift().abs() <= 1.5 * period,
        "drifted {:+.1} ms off an absolute schedule: {report}",
        tl.drift() * 1e3
    );

    // The schedule is absolute, which is to say no frame is ever sent before
    // the slot it belongs to. This is also why the burst check below cannot
    // be tripped by a busy host: a gap can only be short if the wake-up
    // before it was late, and the host can only make wake-ups late.
    let late = sorted(tl.lateness());
    assert!(
        late[0] >= -0.02,
        "a frame went out {:.2} ms before its slot: {report}",
        late[0] * period * 1e3
    );

    // No two frames close together with nothing to explain it. An absolute
    // schedule does answer a late frame with a short gap - that is the error
    // being paid off rather than accumulated - so the frame before a short
    // gap has to have been on time for it to be a burst.
    let bursts = tl.unexplained_bursts();
    assert!(
        bursts.is_empty(),
        "{} frames went out less than {BURST_FRACTION} of a period after an on-time frame, \
         first at slot {}: {report}",
        bursts.len(),
        tl.ticks[bursts[0] + 1].slot,
    );
    // Same property one layer out, on the datagrams themselves, which carry
    // the encoder's jitter on top of the schedule's. `min_gap` cannot be
    // asserted on unconditionally - a wake-up the host held up by more than
    // `1 - BURST_FRACTION` of a period licenses a short gap after it - but
    // when nothing was held up that far, nothing may be close together.
    let max_late = late[late.len() - 1];
    if max_late <= EXPLAINS_A_SHORT_GAP {
        assert!(
            min_gap.as_secs_f64() >= BURST_FRACTION * period,
            "smallest gap between datagrams was {min_gap:?}, and no wake-up was late \
             enough to explain it: a burst. {report}"
        );
    }

    // Skips on an idle run are the host, not the pacer - but past a few per
    // second the host was too busy for any of this to be a measurement.
    assert!(
        (skipped as f64) <= 0.01 * tl.slots() as f64,
        "{skipped} slots skipped on an idle run: the host was too disturbed to \
         measure the pacer. {report}"
    );

    // And the frames really went out at that spacing: the timeline above is
    // taken before the encoder runs, so on its own it would not notice a
    // sender that paced its wake-ups and then sat on the datagrams.
    let arrivals = paced_arrivals(&state);
    assert_eq!(
        arrivals.len(),
        tl.len(),
        "{} frames sent but {} arrived: {report}",
        tl.len(),
        arrivals.len()
    );
    let wire = tl.at_the_receiver(&arrivals);
    assert!(
        wire.rate_error().abs() <= 0.01,
        "frames arrived at a period {:+.3}% off 33.3333 ms, outside +-1%: {}",
        wire.rate_error() * 100.0,
        wire.summary()
    );
}

/// After a stall the sender must skip the frames it missed and pick the
/// schedule back up, never send a burst to catch up (spec 9.1: the device
/// shows newest-wins and its queues are shallow, so a burst only adds
/// latency).
///
/// The stalls are deliberately spread across the phase of a frame period.
/// After a stall ending `f` of the way into slot `m`, a correct pacer resumes
/// at slot `m + 1` and so waits `1 - f` of a period, while a pacer that
/// resumes at slot `m` fires at once. A single stall therefore proves nothing
/// unless it happens to end late in a slot - with `f` near zero a correct
/// pacer's own gap is nearly a full period and a bursting one's is nearly
/// zero, but with `f` near one they swap. Three stalls a third of a period
/// apart cannot all land in the same quarter of the phase, so at least two of
/// the three separate the two pacers whatever the sleeps overshoot by.
#[test]
fn skips_rather_than_bursting_after_a_stall() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let rx = rx();
    let mut sender = Sender::connect(rx.device(), stream_cfg(30.0)).expect("connect");
    let stop = AtomicBool::new(false);
    let period = period_of(30.0);

    // Nine and a bit frame periods each, at three different points within a
    // period, and far enough apart that the stream is back on the schedule
    // before the next one.
    let stalls: [(u64, Duration); 3] = [
        (20, period.mul_f64(9.17)),
        (50, period.mul_f64(9.50)),
        (80, period.mul_f64(9.83)),
    ];
    let log: Log = Log::default();
    let mut src = recording_source(&log, Some(100), move |slot| {
        if let Some((_, d)) = stalls.iter().find(|(at, _)| *at == slot) {
            std::thread::sleep(*d);
        }
    });
    sender.run(&mut src, &stop).expect("run");

    let skipped = sender.stats().frames_skipped;
    let min_gap = sender.stats().min_gap.unwrap();
    let sent = sender.stats().frames_sent;
    drop(sender);
    let state = rx.shutdown();
    let tl = Timeline::take(&log, 30.0);
    let report = tl.summary();
    eprintln!("three stalls at 30 fps: {report}");

    assert!(
        skipped >= 3 * 7,
        "three nine-period stalls should have skipped about thirty frames, not {skipped}: {report}"
    );
    assert_eq!(skipped, tl.skipped(), "skip accounting disagrees: {report}");
    assert!(sent >= 60, "only {sent} frames sent: {report}");

    // The heart of it. The wake-up after each stall must be *on its slot*. A
    // pacer that resumes at the slot containing "now" is already late for it
    // by however far into that slot the stall ended, and so sends at once -
    // the two-frame burst spec 9.1 forbids, measured at 55 us in card 009. A
    // pacer that resumes at the next slot waits out the rest of the period.
    // Lateness is the measure rather than the gap, because the gap a correct
    // pacer leaves depends on where in the slot the stall ended and the gap a
    // bursting one leaves is always zero.
    let late = tl.lateness();
    let mut resumed: Vec<(u64, f64)> = Vec::new();
    for i in 1..tl.len() {
        // A jump of more than two slots is the resync, not a slow frame.
        if tl.ticks[i].slot - tl.ticks[i - 1].slot > 2 {
            resumed.push((tl.ticks[i].slot, late[i] - late[i - 1]));
        }
    }
    assert!(
        resumed.len() >= stalls.len(),
        "expected a resync per stall, saw {resumed:?}: {report}"
    );
    let off_slot = resumed.iter().filter(|(_, l)| *l > 0.25).count();
    assert!(
        off_slot <= 1,
        "{off_slot} of {} frames after a stall went out late for the slot they claimed \
         ({resumed:?}: slot, and lateness in periods): the pacer resumed on the slot it \
         was already inside instead of the next one, so those frames left immediately \
         behind the stalled frame. {report}",
        resumed.len()
    );

    // Each stall shows up as one long gap at the wire and nothing else. The
    // threshold is five periods, not one and a half: the stalls are nine
    // periods long and no amount of host jitter reaches a sixth of a second,
    // so this counts stalls and only stalls.
    let long = state
        .gaps()
        .iter()
        .filter(|g| **g > period.mul_f64(5.0))
        .count();
    assert_eq!(
        long,
        stalls.len(),
        "{long} long gaps, expected exactly the {} stalls: {report}",
        stalls.len()
    );
    eprintln!("smallest gap between datagrams after the stalls: {min_gap:?}");

    // Sequence numbers count frames *sent*, so they stay contiguous even
    // though wall-clock frames were skipped (spec 3.2).
    for (i, f) in state.frames.iter().enumerate() {
        assert_eq!(f.seq, i as u16);
    }
}

/// The pacer is used at rates other than 30, and the same guarantees hold.
#[test]
fn other_frame_rates_are_paced_too() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for fps in [10.0f64, 24.0, 60.0] {
        let period = period_of(fps).as_secs_f64();
        let rx = rx();
        let mut sender = Sender::connect(rx.device(), stream_cfg(fps)).expect("connect");
        let stop = AtomicBool::new(false);
        let log: Log = Log::default();
        let mut src = recording_source(&log, Some((fps * 1.5) as u64), |_| {});
        sender.run(&mut src, &stop).expect("run");
        drop(sender);
        let state = rx.shutdown();
        let tl = Timeline::take(&log, fps);
        let report = tl.summary();
        eprintln!("{fps} fps: {report}");

        // The slope again, not frames over elapsed: these runs are a second
        // and a half long, where the frame at t=0 is worth 4% on its own.
        // Three percent rather than one: at 60 fps this host's 4 ms of sleep
        // overshoot is a quarter of a period, and there are only 90 slots to
        // average it over.
        assert!(
            tl.rate_error().abs() <= 0.03,
            "asked for {fps} fps and the run held a period {:+.3}% off {:.3} ms: {report}",
            tl.rate_error() * 100.0,
            period * 1e3
        );
        assert!(
            tl.unexplained_bursts().is_empty(),
            "burst at {fps} fps: {report}"
        );
        assert_eq!(
            paced_arrivals(&state).len(),
            tl.len(),
            "frames went missing at {fps} fps: {report}"
        );
    }
}

/// `sleep_until` is the piece that makes the rest possible: sleep for all but
/// the last millisecond, then spin. It must never return early.
#[test]
fn sleep_until_does_not_return_early() {
    let _lock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut over = Vec::new();
    for ms in [0u64, 1, 5, 33] {
        for _ in 0..20 {
            let target = Instant::now() + Duration::from_millis(ms);
            sleep_until(target);
            let now = Instant::now();
            // The half that is `sleep_until`'s own: it may overshoot, it may
            // never undershoot.
            assert!(now >= target, "returned {:?} early", target - now);
            over.push(now.duration_since(target).as_secs_f64());
        }
    }
    // The other half belongs to the scheduler, so it is asserted on the
    // middle of the distribution rather than on the worst of eighty samples:
    // the point of the final spin is that the *common* case lands within
    // microseconds. One preempted sleep on a busy machine is not a bug in
    // `sleep_until`, and the fps tests above are where a pacer that is
    // habitually late fails.
    let over = sorted(over);
    assert!(
        median(&over) < 1e-3,
        "typical overshoot was {:.3} ms (p95 {:.3}, max {:.3}): the final spin is not working",
        median(&over) * 1e3,
        pct(&over, 0.95) * 1e3,
        over[over.len() - 1] * 1e3
    );

    // A target in the past returns at once.
    let t0 = Instant::now();
    sleep_until(t0 - Duration::from_secs(1));
    assert!(t0.elapsed() < Duration::from_millis(2));
}
