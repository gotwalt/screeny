//! The embedding API: a link to a panel that heals itself.
//!
//! [`crate::Sender`] is the low-level half - one socket, one session, one
//! frame at a time, and every failure handed straight back to the caller.
//! That is the right shape for the CLI, which can print an error and exit,
//! and the wrong shape for a program that owns its own render loop and is
//! expected to keep running for weeks while the panel reboots, the Wi-Fi
//! drops and DHCP hands out a new address.
//!
//! [`Link`] is that program's interface. It wraps a `Sender` and adds three
//! things:
//!
//! * **Reconnection.** A link that goes down re-resolves the target (by mDNS
//!   name if that is how it was found), opens a *new* socket per spec 3.2 and
//!   resumes, with exponential backoff, on a background thread so the render
//!   loop never blocks on a three-second browse. Frames pushed meanwhile are
//!   dropped and counted, not turned into errors the caller has to handle.
//! * **A cadence ceiling.** The device shows newest-wins and queues about
//!   five frames below its socket, so pushing faster than it draws buys
//!   nothing and costs latency. [`Cadence::Limit`] decimates to the rate the
//!   device is actually keeping up with, which is also the rate spec 6.9's
//!   adaptation has settled on.
//! * **`FINAL` on drop**, so the device releases the source lock at once
//!   instead of waiting out `STREAM_TIMEOUT_MS`.
//!
//! **Pacing is the caller's job**, deliberately. A generative art system has
//! its own clock, its own limiter and its own reasons to render when it
//! renders; a library that slept on its behalf would be fighting it. `Link`
//! never sleeps. [`Pace`] is there for callers who want spec 9.1's absolute
//! schedule without writing it themselves, and is entirely opt-in.
//!
//! ```no_run
//! use screeny::{Link, LinkConfig, Pixels, Target};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let (palette, indices): (Vec<[u8; 3]>, Vec<u8>) = (vec![], vec![]);
//! let mut link = Link::open(Target::default(), LinkConfig::default())?;
//! loop {
//!     // ... render a palette of <= 32 colours and 2048 indices ...
//!     link.send(Pixels::indexed(&palette, &indices))?;   // exact, or dropped
//!     std::thread::sleep(std::time::Duration::from_millis(16));
//! }
//! # }
//! ```

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::device::Device;
use crate::discover::Target;
use crate::encode::MIN_BUDGET;
use crate::error::{Error, Result};
use crate::frame::{FrameTime, Pixels};
use crate::net::Traffic;
use crate::sender::{period_of, sleep_until, SendStats, Sender, SenderConfig, Sent};

/// What to do with a frame that arrives sooner than the device can use it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cadence {
    /// Send every frame the caller offers. Pacing is entirely theirs, and so
    /// is the consequence: a device that cannot keep up counts the surplus as
    /// `frames_dropped_superseded`, which spec 6.9 reads as "too slow to
    /// decode" and answers by switching to cheaper codecs.
    Free,
    /// Drop frames that arrive before the next slot is due, on an absolute
    /// schedule at the rate the sender is currently targeting - which starts
    /// at [`SenderConfig::fps`] and follows spec 6.9's ladder down and back
    /// up. A producer rendering at 60 fps into a 30 fps panel sends every
    /// other frame and nothing is superseded.
    #[default]
    Limit,
}

/// How long to wait between reconnection attempts.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    /// Delay before the second attempt. The first is immediate.
    pub first: Duration,
    /// Ceiling. A panel that is unplugged for a week should be retried once
    /// every this often, not once every 2^n seconds.
    pub max: Duration,
    /// Multiplier per failed attempt.
    pub factor: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            first: Duration::from_millis(250),
            max: Duration::from_secs(10),
            factor: 2.0,
        }
    }
}

impl Backoff {
    /// Delay after `attempt` consecutive failures. `attempt` 0 is immediate.
    ///
    /// Computed in seconds rather than by `Duration::mul_f64`, which panics
    /// on overflow: a link that has been down for a week has a very large
    /// attempt count, and the honest answer for all of them is `max`.
    #[must_use]
    pub fn delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        // `f64::max` returns the other operand when one is NaN, so this also
        // makes a NaN factor harmless.
        let growth = self.factor.max(1.0).powf(f64::from(attempt - 1));
        let secs = self.first.as_secs_f64() * growth; // may be inf; min() handles it
        Duration::from_secs_f64(secs.min(self.max.as_secs_f64()).max(0.0))
    }
}

/// How to run a [`Link`].
#[derive(Clone, Debug)]
pub struct LinkConfig {
    /// Everything about one session: frame rate, budget, encode profile,
    /// `STATS_REQ` cadence, adaptation, timestamps, QoS, handshake.
    pub sender: SenderConfig,
    /// What to do with frames offered faster than the device's rate.
    pub cadence: Cadence,
    /// Reconnect when the link goes down. Off, the link stays down and every
    /// frame is [`Sent::Dropped`] - which is what a one-shot tool wants.
    pub reconnect: bool,
    /// Declare the link down after this long with nothing heard from the
    /// device *while actively sending*. UDP has no connection to lose, so a
    /// rebooted or re-addressed device looks exactly like a working one until
    /// the telemetry it owes us fails to arrive. [`Duration::ZERO`] disables
    /// the watchdog, leaving only socket errors to notice.
    pub silence: Duration,
    /// Retry schedule.
    pub backoff: Backoff,
}

impl Default for LinkConfig {
    fn default() -> Self {
        LinkConfig {
            sender: SenderConfig::default(),
            cadence: Cadence::default(),
            reconnect: true,
            // Five `STATS_REQ` intervals: long enough that a single lost
            // telemetry packet is not a reconnection, short enough that a
            // rebooted panel comes back inside a few seconds.
            silence: Duration::from_secs(5),
            backoff: Backoff::default(),
        }
    }
}

/// Where a [`Link`] is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Connected; frames go out.
    Up,
    /// An attempt to connect is in flight on a background thread.
    Connecting,
    /// Down, waiting out the backoff before the next attempt.
    Waiting,
    /// Down for good: either [`LinkConfig::reconnect`] is off, or
    /// [`Link::close`] was called.
    Closed,
}

impl LinkState {
    /// True only for [`LinkState::Up`].
    #[must_use]
    pub fn is_up(&self) -> bool {
        *self == LinkState::Up
    }
}

/// **What a link has cost the network**, over its whole life (card 164).
///
/// Two sockets, because they are two different conversations with the panel:
/// the frame port carries the stream and the telemetry that comes back along
/// it, and the control port carries the `GET_INFO` handshake at the head of
/// every session. A caller that wants one number adds them.
///
/// Payload bytes and datagrams; add [`crate::UDP_OVERHEAD`] per datagram for
/// the figure on the wire. Every field only grows - a session that ends is
/// banked here before its [`SendStats`] go away with the socket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkTraffic {
    /// The frame port: frames out, `TELEMETRY` and `BUSY` in.
    pub frames: Traffic,
    /// The control port: one `GET_INFO` and its reply per session.
    pub control: Traffic,
}

impl LinkTraffic {
    /// Payload bytes sent on both ports, plus `overhead` per datagram.
    #[must_use]
    pub fn out_on_the_wire(&self, overhead: u64) -> u64 {
        self.frames.out.on_the_wire(overhead) + self.control.out.on_the_wire(overhead)
    }

    /// Payload bytes received on both ports, plus `overhead` per datagram.
    #[must_use]
    pub fn in_on_the_wire(&self, overhead: u64) -> u64 {
        self.frames.inbound.on_the_wire(overhead) + self.control.inbound.on_the_wire(overhead)
    }
}

/// Lifetime totals for a [`Link`], across every session it has had.
///
/// [`Link::session`] has the per-session detail - telemetry, codec mix, encode
/// percentiles - which necessarily resets when the socket does.
#[derive(Clone, Debug, Default)]
pub struct LinkStats {
    /// Frames handed to [`Link::send`], whatever became of them.
    pub frames_offered: u64,
    /// Frames put on the wire.
    pub frames_sent: u64,
    /// Frames dropped by the cadence ceiling.
    pub frames_coalesced: u64,
    /// Frames dropped because the link was down.
    pub frames_dropped: u64,
    /// Pixel payload bytes sent.
    pub bytes: u64,
    /// **What this link has cost the network**, across every session (card
    /// 164). Unlike [`LinkStats::bytes`], which is pixels, this is whole
    /// datagrams on both ports and both ways.
    pub traffic: LinkTraffic,
    /// Indexed frames that went out exactly.
    pub indexed_exact: u64,
    /// Indexed frames that had to take the lossy fallback
    /// ([`Sender::send_indexed`]).
    pub indexed_fallback: u64,
    /// Sessions opened, including the first.
    pub sessions: u32,
    /// Times an established link went down.
    pub drops: u32,
    /// Connection attempts that failed.
    pub connect_failures: u32,
    /// Why the last attempt failed, or why the last session ended.
    pub last_error: Option<String>,
    /// When the current session started.
    pub connected_since: Option<Instant>,
    /// When the link went down, if it is down.
    pub down_since: Option<Instant>,
}

/// What the device and the negotiated session allow, for a producer that
/// wants to adapt to them.
///
/// Everything here can change while a link runs: the frame rate follows spec
/// 6.9's ladder, the codec set narrows if the device fails to decode
/// something, and a reconnect to a device with different firmware can change
/// the budget outright.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// False when the link is down; every other field is then the last known
    /// value, or the configured default if there has never been a session.
    pub connected: bool,
    /// Frame rate the sender is targeting now.
    pub fps: f64,
    /// Frame rate that was asked for, before adaptation.
    pub configured_fps: f64,
    /// Pixel payload budget in bytes.
    pub budget: usize,
    /// Palettes up to this size go on the wire exactly **whatever the
    /// indices** - that is `PAL5`'s fixed-rate guarantee. Larger palettes are
    /// still exact when they compress; see [`Sender::send_indexed`]. Zero
    /// means no size is guaranteed, which happens only at a budget below
    /// [`MIN_BUDGET`] or against a device that does not advertise `PAL5`.
    pub exact_palette: usize,
    /// Codec ids in play.
    pub codecs: Vec<u8>,
    /// True while the codec set is reduced because the device is
    /// decode-limited (spec 6.9).
    pub codec_limited: bool,
    /// Panel size the device advertises, if it has said.
    pub panel: Option<(u16, u16)>,
}

/// A self-healing connection to a panel.
///
/// See the [module documentation](self) for what it does and why. Construct
/// with [`Link::open`] (blocking, fails if the panel is not there) or
/// [`Link::open_deferred`] (never fails; connects in the background). A
/// caller that has already resolved its device - a service that keeps its own
/// list, or a test talking to a simulator on ephemeral ports - uses
/// [`Link::attach`] or [`Link::attach_deferred`] instead and skips discovery
/// altogether.
pub struct Link {
    target: Target,
    /// Set by [`Link::attach`]: the device to reconnect to verbatim, instead
    /// of resolving `target` again. Cleared by [`Link::retarget`].
    attached: Option<Device>,
    cfg: LinkConfig,
    sender: Option<Sender>,
    pending: Option<Receiver<Result<Sender>>>,
    attempt: u32,
    next_attempt: Instant,
    closed: bool,
    /// Set when a session ended and [`LinkConfig::reconnect`] is off. Cleared
    /// by [`Link::retarget`], which is an explicit instruction to try again.
    given_up: bool,
    stats: LinkStats,
    /// Card 164: how much of the **current** session's traffic has already
    /// been folded into `stats.traffic`. A session's counters go away with its
    /// socket, so they are banked as deltas rather than copied.
    taken: LinkTraffic,
    /// Absolute schedule for the cadence ceiling.
    next_due: Option<Instant>,
    last_tx: Option<Instant>,
    /// Kept so [`Link::limits`] can answer while the link is down.
    last_limits: Limits,
}

/// What one connection attempt has to work with.
///
/// The two doors into a `Link` differ in exactly this and nothing else: a
/// target is re-resolved on every attempt (and so follows a name), a device is
/// reused verbatim (and so keeps both of its ports).
// A `Device` is much bigger than a `Target`, and boxing it would buy an
// allocation on a value that is built once per connection attempt, moved onto
// the connect thread and consumed there.
#[allow(clippy::large_enum_variant)]
enum Aim {
    Find(Target),
    Known(Device),
}

impl Aim {
    fn resolve(self) -> Result<Device> {
        match self {
            Aim::Find(t) => t.resolve(),
            Aim::Known(d) => Ok(d),
        }
    }
}

impl Link {
    /// Resolve the target, connect, and return a running link.
    ///
    /// This blocks: discovery takes up to [`Target::timeout`] and the
    /// `GET_INFO` handshake up to another 750 ms. That is right for a program
    /// that wants to fail loudly at start-up if the panel is not there, and
    /// wrong for a daemon - use [`Link::open_deferred`] for that.
    ///
    /// # Errors
    ///
    /// Whatever [`Target::resolve`] and [`Sender::connect`] return.
    pub fn open(target: Target, cfg: LinkConfig) -> Result<Self> {
        let mut link = Self::open_deferred(target, cfg);
        let sender = link.connect_now()?;
        link.adopt(sender);
        Ok(link)
    }

    /// A link that has not connected yet and never fails to be created.
    ///
    /// The first connection attempt happens in the background on the first
    /// [`Link::send`] or [`Link::poll`]; until it lands, frames are
    /// [`Sent::Dropped`]. This is what a long-running sender wants: a panel
    /// that is switched off at start-up is not different in kind from one
    /// switched off an hour later, and neither should be a special case in
    /// the caller.
    #[must_use]
    pub fn open_deferred(target: Target, cfg: LinkConfig) -> Self {
        Self::build(target, None, cfg)
    }

    /// Connect to a device that is already resolved.
    ///
    /// [`Link::open`] goes through [`Target::resolve`], which for a bare
    /// address builds the device with [`Device::from_addr`] - and that takes
    /// the control port to be frame + 1. True of the spec's defaults
    /// (49374/49375), never true of an ephemeral pair. A caller that knows
    /// both ports - a service that browsed once and kept its `Device`s, or a
    /// test pointing at a simulator - hands the device over instead of
    /// describing how to find it again. This is [`Sender::connect`]'s door,
    /// with reconnection above it.
    ///
    /// **Reconnection keeps these exact sockets.** An attached link never
    /// browses: every retry reuses the address and both ports it was given,
    /// so it comes back after a reboot at the same address and does *not*
    /// follow the device across a DHCP lease. Following a lease is what an
    /// instance name is for, and the way to get it is [`Link::open`] with
    /// `Target { name: Some(..), .. }`, or [`Link::retarget`] with one later.
    /// (A `Device`'s `instance` is not enough: it can be synthesised from an
    /// address, so re-browsing on it would silently turn an address the
    /// caller chose into a name it did not.)
    ///
    /// # Errors
    ///
    /// Whatever [`Sender::connect`] returns.
    pub fn attach(device: Device, cfg: LinkConfig) -> Result<Self> {
        let mut link = Self::attach_deferred(device, cfg);
        let sender = link.connect_now()?;
        link.adopt(sender);
        Ok(link)
    }

    /// [`Link::attach`] without the handshake up front: never fails, connects
    /// in the background, and reconnects to the same sockets forever.
    #[must_use]
    pub fn attach_deferred(device: Device, cfg: LinkConfig) -> Self {
        // `target()` must still answer something honest for an attached link.
        // The frame address is that: it is where the frames go, and it is what
        // `Target { addr }` would have meant.
        let target = Target {
            addr: Some(device.frame),
            ..Target::default()
        };
        Self::build(target, Some(device), cfg)
    }

    fn build(target: Target, attached: Option<Device>, cfg: LinkConfig) -> Self {
        let last_limits = Limits {
            connected: false,
            fps: cfg.sender.fps,
            configured_fps: cfg.sender.fps,
            budget: cfg.sender.budget.unwrap_or(screeny_proto::MAX_PIXEL_PAYLOAD),
            exact_palette: 0,
            codecs: Vec::new(),
            codec_limited: false,
            panel: None,
        };
        Link {
            target,
            attached,
            cfg,
            sender: None,
            pending: None,
            attempt: 0,
            next_attempt: Instant::now(),
            closed: false,
            given_up: false,
            stats: LinkStats {
                down_since: Some(Instant::now()),
                ..LinkStats::default()
            },
            taken: LinkTraffic::default(),
            next_due: None,
            last_tx: None,
            last_limits,
        }
    }

    /// Offer a frame.
    ///
    /// Returns what became of it: on the wire, dropped by the cadence
    /// ceiling, or dropped because the link is down. **The network cannot
    /// make this fail.** The only errors are the caller's own -
    /// [`Error::Frame`] for a frame of the wrong shape and [`Error::BadIndex`]
    /// for an index outside its palette - so an embedder that has its frame
    /// sizes right can `unwrap` and never think about it again.
    ///
    /// This also drains the device's telemetry, which is what drives spec
    /// 6.9's adaptation and the reconnect watchdog, so a caller that goes
    /// quiet for a while should call [`Link::poll`] instead of nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Frame`], [`Error::BadIndex`].
    pub fn send(&mut self, px: Pixels<'_>) -> Result<Sent> {
        // The whole check, before the cadence ceiling can discard the frame:
        // an error the caller can fix must not depend on whether this frame
        // happened to be the one that was kept.
        px.validate()?;
        self.stats.frames_offered += 1;
        self.poll();

        let Some(sender) = self.sender.as_mut() else {
            self.stats.frames_dropped += 1;
            return Ok(Sent::Dropped);
        };

        // The cadence ceiling, on an absolute schedule so a caller whose rate
        // is not a multiple of the device's still gets as close to the
        // device's rate as decimation allows.
        let now = Instant::now();
        if self.cfg.cadence == Cadence::Limit {
            let period = period_of(sender.fps());
            let slack = period / 10;
            match self.next_due {
                Some(due) if now + slack < due => {
                    self.stats.frames_coalesced += 1;
                    return Ok(Sent::Coalesced);
                }
                Some(due) => self.next_due = Some((due + period).max(now)),
                None => self.next_due = Some(now + period),
            }
        }

        let before_exact = sender.stats().indexed_exact;
        let before_fallback = sender.stats().indexed_fallback;
        let out = sender.send(px);
        // Account for the encode even if the datagram did not make it: the
        // exactness decision was taken either way.
        self.stats.indexed_exact += sender.stats().indexed_exact - before_exact;
        self.stats.indexed_fallback += sender.stats().indexed_fallback - before_fallback;

        match out {
            Ok(sent) => {
                self.stats.frames_sent += 1;
                self.stats.bytes += sent.bytes() as u64;
                self.last_tx = Some(now);
                self.sender.as_mut().expect("still connected").poll_feedback();
                self.absorb();
                Ok(sent)
            }
            // Unreachable: `validate` above rejects both before a frame is
            // ever counted as offered. Kept so that the accounting invariant
            // `offered == sent + coalesced + dropped` cannot be broken later
            // by a new validation the encoder learns to do.
            Err(e @ (Error::Frame { .. } | Error::BadIndex { .. })) => {
                self.stats.frames_offered -= 1;
                Err(e)
            }
            Err(e) => {
                // A socket error on a connected UDP socket is an ICMP report
                // coming home: the device is gone, or the route is. Tear the
                // session down and let the backoff bring it back.
                self.lose(format!("send failed: {e}"));
                self.stats.frames_dropped += 1;
                Ok(Sent::Dropped)
            }
        }
    }

    /// Drain telemetry, run the silence watchdog, and drive reconnection.
    ///
    /// [`Link::send`] calls this itself. Call it directly when frames are not
    /// flowing - between scenes, while a piece is loading - so a link that
    /// went down during the pause is already back up when they resume.
    pub fn poll(&mut self) {
        if let Some(s) = self.sender.as_mut() {
            s.poll_feedback();
        }
        self.absorb();
        self.watchdog();
        if self.sender.is_none() {
            self.pump();
        }
        if let Some(s) = self.sender.as_ref() {
            let fresh = limits_of(s);
            self.last_limits = fresh;
        }
    }

    /// **Bank what the current session has put on the wire** (card 164).
    ///
    /// A [`Sender`]'s counters die with its socket, and a link that runs for
    /// months rebuilds that socket every time the panel reboots or moves. So
    /// the lifetime figure is kept here and this folds in the **difference**
    /// since the last look, which makes it monotonic by construction rather
    /// than by anyone remembering to read it before a reconnect.
    ///
    /// Cheap enough to call per frame: eight `u64` subtractions on values the
    /// sender has already written, no lock and no allocation.
    fn absorb(&mut self) {
        let Some(s) = self.sender.as_ref() else { return };
        let now = LinkTraffic {
            frames: s.stats().frames,
            control: s.stats().control,
        };
        let step = |to: &mut crate::net::Wire, now: crate::net::Wire, was: crate::net::Wire| {
            to.bytes += now.bytes.saturating_sub(was.bytes);
            to.packets += now.packets.saturating_sub(was.packets);
        };
        step(&mut self.stats.traffic.frames.out, now.frames.out, self.taken.frames.out);
        step(&mut self.stats.traffic.frames.inbound, now.frames.inbound, self.taken.frames.inbound);
        step(&mut self.stats.traffic.control.out, now.control.out, self.taken.control.out);
        step(&mut self.stats.traffic.control.inbound, now.control.inbound, self.taken.control.inbound);
        self.taken = now;
    }

    /// Declare the link down if the device has stopped answering while we are
    /// still streaming at it.
    fn watchdog(&mut self) {
        if self.cfg.silence.is_zero() {
            return;
        }
        let Some(s) = self.sender.as_ref() else { return };
        // Only meaningful while we are actually sending: `STATS_REQ` rides on
        // frames, so a link nobody is pushing to is silent for good reason.
        // "Still streaming" means a frame went out inside the same window we
        // are judging the silence over - so a producer pushing every few
        // seconds is covered too, not just one at 30 fps.
        let streaming = self
            .last_tx
            .is_some_and(|t| t.elapsed() < self.cfg.silence);
        if streaming && s.stats().frames_sent > 0 && s.silence() > self.cfg.silence {
            let silence = s.silence();
            self.lose(format!(
                "no telemetry for {:.1} s while streaming",
                silence.as_secs_f64()
            ));
        }
    }

    /// Advance the reconnection state machine. Never blocks.
    fn pump(&mut self) {
        if self.closed || self.given_up {
            return;
        }
        if let Some(rx) = self.pending.as_ref() {
            match rx.try_recv() {
                Ok(Ok(sender)) => {
                    self.pending = None;
                    self.adopt(sender);
                    return;
                }
                Ok(Err(e)) => {
                    self.pending = None;
                    self.stats.connect_failures += 1;
                    self.stats.last_error = Some(e.to_string());
                    self.attempt = self.attempt.saturating_add(1);
                    self.next_attempt = Instant::now() + self.cfg.backoff.delay(self.attempt);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.attempt = self.attempt.saturating_add(1);
                    self.next_attempt = Instant::now() + self.cfg.backoff.delay(self.attempt);
                }
            }
        }
        if Instant::now() >= self.next_attempt {
            self.spawn_attempt();
        }
    }

    /// Which device to connect to on the next attempt: the one that was
    /// attached, verbatim, or whatever resolving the target finds now.
    ///
    /// Taken by value so it can cross onto the connect thread.
    fn aim(&self) -> Aim {
        match &self.attached {
            Some(d) => Aim::Known(d.clone()),
            None => Aim::Find(self.target.clone()),
        }
    }

    /// Resolve and connect on a background thread.
    ///
    /// On a thread because both halves block: an mDNS browse for up to
    /// [`Target::timeout`], and the handshake for up to three 250 ms
    /// round trips. A render loop must not stop for either.
    fn spawn_attempt(&mut self) {
        let (tx, rx) = mpsc::sync_channel(1);
        let aim = self.aim();
        let cfg = self.cfg.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("screeny-link-connect".into())
            .spawn(move || {
                // The receiver is gone if the link was dropped meanwhile,
                // which is fine: the Sender goes with it, unused.
                let _ = tx.send(aim.resolve().and_then(|d| Sender::connect(d, cfg)));
            });
        match spawned {
            Ok(_) => self.pending = Some(rx),
            Err(e) => {
                self.stats.connect_failures += 1;
                self.stats.last_error = Some(format!("could not spawn a connect thread: {e}"));
                self.attempt = self.attempt.saturating_add(1);
                self.next_attempt = Instant::now() + self.cfg.backoff.delay(self.attempt);
            }
        }
    }

    /// Connect synchronously, for [`Link::open`] and [`Link::attach`].
    fn connect_now(&mut self) -> Result<Sender> {
        match self
            .aim()
            .resolve()
            .and_then(|d| Sender::connect(d, self.cfg.sender.clone()))
        {
            Ok(s) => Ok(s),
            Err(e) => {
                self.stats.connect_failures += 1;
                self.stats.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    fn adopt(&mut self, sender: Sender) {
        // A fresh session counts from zero, and its handshake is already in
        // its `SendStats` - so nothing has been taken from it yet (card 164).
        self.taken = LinkTraffic::default();
        self.stats.sessions += 1;
        self.stats.connected_since = Some(Instant::now());
        self.stats.down_since = None;
        self.attempt = 0;
        self.next_due = None;
        self.last_tx = None;
        self.last_limits = limits_of(&sender);
        self.sender = Some(sender);
    }

    /// Tear down a session that has stopped working. No `FINAL`: there is
    /// nobody to send it to, and if there is, the stream timeout releases the
    /// lock in a second.
    fn lose(&mut self, why: String) {
        // Bank what this session cost before its counters go with its socket.
        self.absorb();
        self.sender = None;
        self.next_due = None;
        self.last_tx = None;
        self.last_limits.connected = false;
        self.stats.drops += 1;
        self.stats.down_since = Some(Instant::now());
        self.stats.connected_since = None;
        self.stats.last_error = Some(why);
        self.attempt = 0;
        self.next_attempt = Instant::now();
        self.given_up = !self.cfg.reconnect;
    }

    /// Point the link at a different device, without the caller having to
    /// rebuild anything or lose its lifetime statistics.
    ///
    /// The current session, if any, is closed politely with `FINAL`, and the
    /// new target is connected on the next [`Link::send`] or [`Link::poll`].
    ///
    /// A link whose [`Target`] names an instance rather than an address
    /// follows the device by itself - each reconnect re-browses `_screeny._udp`
    /// and picks up whatever address DHCP has handed out since. This is for
    /// the other case: an address-pinned target that has moved, or a
    /// deployment that switches panels.
    ///
    /// This also **detaches** a link built with [`Link::attach`]: the device
    /// it was pinned to is forgotten and every attempt from here resolves the
    /// new target. Point it at another known device with
    /// [`Link::reattach`].
    pub fn retarget(&mut self, target: Target) {
        self.reaim(Aim::Find(target));
    }

    /// [`Link::retarget`] for a device that is already resolved: the same
    /// hand-over, and the same pinned reconnection [`Link::attach`] describes.
    pub fn reattach(&mut self, device: Device) {
        self.reaim(Aim::Known(device));
    }

    fn reaim(&mut self, aim: Aim) {
        if let Some(s) = self.sender.as_mut() {
            let _ = s.finish();
            s.poll_feedback();
        }
        // `FINAL` and whatever came back with it are this session's too.
        self.absorb();
        self.sender = None;
        self.pending = None;
        match aim {
            Aim::Find(t) => {
                self.target = t;
                self.attached = None;
            }
            Aim::Known(d) => {
                self.target = Target {
                    addr: Some(d.frame),
                    ..Target::default()
                };
                self.attached = Some(d);
            }
        }
        self.next_due = None;
        self.last_tx = None;
        self.last_limits.connected = false;
        self.closed = false;
        self.given_up = false;
        self.attempt = 0;
        self.next_attempt = Instant::now();
        self.stats.connected_since = None;
        self.stats.down_since = Some(Instant::now());
    }

    /// Where the link is pointed. For an attached link this is the device's
    /// frame address; [`Link::attached`] has the whole device, including the
    /// control port a `Target` cannot express.
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The device this link is pinned to, if it was built with
    /// [`Link::attach`]. `None` for a link that resolves its target - use
    /// [`Link::device`] for the device a live session settled on.
    #[must_use]
    pub fn attached(&self) -> Option<&Device> {
        self.attached.as_ref()
    }

    /// Send `FINAL` and stop. The link does not reconnect afterwards.
    ///
    /// [`Drop`] does this, so an embedder that lets the link fall out of
    /// scope releases the device's source lock at once (spec 7.4, 9.4 step 5)
    /// rather than leaving the last frame lit for `STREAM_TIMEOUT_MS`.
    pub fn close(&mut self) {
        self.closed = true;
        self.pending = None;
        if let Some(s) = self.sender.as_mut() {
            let _ = s.finish();
            s.poll_feedback();
        }
        // The `FINAL` frame is a datagram like any other (card 164).
        self.absorb();
        self.sender = None;
        self.last_limits.connected = false;
        self.stats.connected_since = None;
    }

    /// Where the link is.
    #[must_use]
    pub fn state(&self) -> LinkState {
        if self.closed || self.given_up {
            LinkState::Closed
        } else if self.sender.is_some() {
            LinkState::Up
        } else if self.pending.is_some() {
            LinkState::Connecting
        } else {
            LinkState::Waiting
        }
    }

    /// Lifetime totals.
    #[must_use]
    pub fn stats(&self) -> &LinkStats {
        &self.stats
    }

    /// The current session's detail: telemetry, codec mix, encode times,
    /// adaptation. `None` while the link is down.
    #[must_use]
    pub fn session(&self) -> Option<&SendStats> {
        self.sender.as_ref().map(Sender::stats)
    }

    /// The device, while there is one.
    #[must_use]
    pub fn device(&self) -> Option<&crate::Device> {
        self.sender.as_ref().map(Sender::device)
    }

    /// What the device and the session allow. While the link is down this is
    /// the last known set, with `connected` false.
    #[must_use]
    pub fn limits(&self) -> Limits {
        match self.sender.as_ref() {
            Some(s) => limits_of(s),
            None => self.last_limits.clone(),
        }
    }

    /// The rate the cadence ceiling is enforcing, which is the rate a
    /// producer should render at. Falls back to the configured rate while the
    /// link is down, so a caller can drive its own clock from this
    /// unconditionally.
    #[must_use]
    pub fn fps(&self) -> f64 {
        self.sender
            .as_ref()
            .map_or(self.cfg.sender.fps, Sender::fps)
    }

    /// A pacer set to [`Link::fps`], for a caller that wants spec 9.1's
    /// schedule rather than its own.
    #[must_use]
    pub fn pacer(&self) -> Pace {
        Pace::new(self.fps())
    }

    /// The configuration in force.
    #[must_use]
    pub fn config(&self) -> &LinkConfig {
        &self.cfg
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for Link {
    // Deliberately a summary rather than every field: the interesting state
    // is derived (`state()`), and a `Receiver<Result<Sender>>` prints nothing
    // a human wants.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("state", &self.state())
            .field("device", &self.device().map(crate::Device::label))
            .field("frames_sent", &self.stats.frames_sent)
            .field("sessions", &self.stats.sessions)
            .finish_non_exhaustive()
    }
}

fn limits_of(s: &Sender) -> Limits {
    let info = s.device().info.as_ref();
    let codecs = s.codecs().to_vec();
    Limits {
        connected: true,
        fps: s.fps(),
        configured_fps: s.config().fps,
        budget: s.budget(),
        exact_palette: if s.budget() >= MIN_BUDGET
            && codecs.contains(&screeny_proto::dec::codec::PAL5)
        {
            32
        } else {
            0
        },
        codecs,
        codec_limited: s.stats().codec_limited,
        panel: info.map(|i| (i.w, i.h)),
    }
}

// ---------------------------------------------------------------------------
// Pacing, for callers who want ours
// ---------------------------------------------------------------------------

/// Spec 9.1's pacer, on its own, for a caller that owns its loop but does not
/// want to write a clock.
///
/// Two rules, both from the spec and both worth restating: the schedule is
/// **absolute**, so error cannot accumulate, and when the caller falls behind
/// the missed slots are **skipped, never burst**, because the device shows
/// newest-wins and a catch-up burst would only add latency to every frame
/// after it.
///
/// ```no_run
/// use screeny::{Link, LinkConfig, Pixels, Target};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let (pal, idx): (Vec<[u8; 3]>, Vec<u8>) = (vec![], vec![]);
/// let mut link = Link::open(Target::default(), LinkConfig::default())?;
/// let mut pace = link.pacer();
/// loop {
///     let t = pace.tick();                 // sleeps until the next slot
///     # let _ = t;
///     link.send(Pixels::indexed(&pal, &idx))?;
/// }
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Pace {
    start: Instant,
    period: Duration,
    n: u64,
    skipped: u64,
    fps: f64,
}

impl Pace {
    /// A pacer for `fps`, starting now.
    #[must_use]
    pub fn new(fps: f64) -> Self {
        Pace {
            start: Instant::now(),
            period: period_of(fps),
            n: 0,
            skipped: 0,
            fps,
        }
    }

    /// Change the rate. The schedule restarts from now, so a rate change does
    /// not look to the pacer like a stall.
    pub fn set_fps(&mut self, fps: f64) {
        #[allow(clippy::float_cmp)] // an identity test: the caller passes a constant
        if fps == self.fps {
            return;
        }
        self.fps = fps;
        self.period = period_of(fps);
        self.start = Instant::now();
        self.n = 0;
    }

    /// Sleep until the next slot and return its position in the stream.
    ///
    /// If the caller overran, the slots that went by are counted in
    /// [`Pace::skipped`] and the next whole slot is waited for; this never
    /// returns twice without sleeping.
    pub fn tick(&mut self) -> FrameTime {
        sleep_until(self.start + self.period.mul_f64(self.n as f64));
        let index = self.n;
        self.n += 1;

        // Spec 9.1: more than two periods behind, resynchronise. Resuming at
        // the *next* whole slot rather than at "now" costs one more skipped
        // frame and avoids the two-frame burst that the rule exists to stop.
        let behind = Instant::now().saturating_duration_since(self.start);
        let should_be = (behind.as_nanos() / self.period.as_nanos().max(1)) as u64;
        if should_be > self.n + 1 {
            self.skipped += should_be + 1 - self.n;
            self.n = should_be + 1;
        }

        FrameTime {
            index,
            elapsed: self.start.elapsed(),
            fps: self.fps,
        }
    }

    /// Slots skipped because the caller overran.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// The rate being paced.
    #[must_use]
    pub fn fps(&self) -> f64 {
        self.fps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_starts_immediate_and_is_capped() {
        let b = Backoff::default();
        assert_eq!(b.delay(0), Duration::ZERO);
        assert_eq!(b.delay(1), Duration::from_millis(250));
        assert_eq!(b.delay(2), Duration::from_millis(500));
        assert_eq!(b.delay(3), Duration::from_secs(1));
        assert_eq!(b.delay(20), b.max);
        // A link that has been down for weeks: the exponent overflows every
        // integer type in sight, and the answer is still `max`.
        assert_eq!(b.delay(u32::MAX), b.max);

        // Degenerate configurations must not panic either.
        let flat = Backoff {
            first: Duration::ZERO,
            max: Duration::from_secs(1),
            factor: 0.0,
        };
        assert_eq!(flat.delay(1), Duration::ZERO);
        assert_eq!(flat.delay(u32::MAX), Duration::ZERO);
        let nan = Backoff {
            factor: f64::NAN,
            ..Backoff::default()
        };
        assert_eq!(nan.delay(5), nan.first);
    }

    #[test]
    fn a_pacer_skips_rather_than_bursting() {
        let mut p = Pace::new(100.0);
        p.tick();
        std::thread::sleep(Duration::from_millis(60));
        let t0 = Instant::now();
        p.tick();
        assert!(
            p.skipped() >= 4,
            "six periods went by; {} skipped",
            p.skipped()
        );
        assert!(
            t0.elapsed() < Duration::from_millis(15),
            "the next slot should be close, not a catch-up burst"
        );
    }
}
