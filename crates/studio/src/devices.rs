//! The device registry: which panels this studio knows about.
//!
//! **A collection from day one**, even though one panel is the expected case
//! (`studio-vision.md`, decision 3). Nothing here is written as though there
//! were a single global device.
//!
//! Two ways in, merged into one list:
//!
//! - **discovery**: a periodic `_screeny._udp` browse. A convenience, not a
//!   requirement - Docker on macOS has no multicast at all, and avahi owns
//!   5353 on the Linux box - so the studio never depends on it;
//! - **manual**: a human typing a name or an address.
//!
//! **Keyed by the device's own stable id**, never by IP. A panel that takes a
//! new DHCP lease is the same panel with the same player; an address is a way
//! of reaching it, not a name for it. A manually added address is not known by
//! id until the studio has spoken to it, so it gets a *provisional* id
//! ([`PENDING`]`:<what was typed>`) and adopts its real one on the first
//! answer - the one moment when a device's key changes, and the one place that
//! has to be handled ([`Registry::resolved`] returns the rename so the player
//! can follow it).
//!
//! Bounded by construction: one browse at a time, one control request in
//! flight per device, a fixed telemetry period, and every fault logged once
//! per device rather than per attempt.

use screeny::proto::control::{state as dev_state, Telemetry};
use screeny::{ControlClient, Device, Target};
use screeny_device_api::reply::{PanicReply, StatusReply};
use screeny_device_api::{FwState, ResetReason, WifiState};
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::state::{unix_now, StoredDevice};

/// Prefix of a provisional id, used until the device says what its real one is.
pub const PENDING: &str = "pending";
/// How long a control request may take before it is a failure. The client
/// retries three times inside this.
pub const CONTROL_TIMEOUT: Duration = Duration::from_millis(400);

/// How the studio reaches one device.
///
/// The distinction the brief asks for: a *name* a human typed is re-resolved
/// on every reconnect and so follows a DHCP lease; a device the *registry* has
/// resolved is attached to by its exact two ports and never browsed for.
#[derive(Clone, Debug)]
pub enum Reach {
    /// Resolved: attach to these ports (`Link::attach`, card 111).
    Resolved(Box<Device>),
    /// A DNS-SD instance name: `Link` re-resolves it on every reconnect.
    Name(String),
    /// A literal address.
    Addr(SocketAddr),
    /// Nothing to go on yet - a discovered device we have lost sight of, or a
    /// manual name we have never reached.
    Unknown,
}

/// A device as the studio knows it right now.
#[derive(Clone, Debug, Default)]
pub struct DeviceRecord {
    pub stored: StoredDevice,
    /// The last resolution: exact addresses and ports.
    pub resolved: Option<Device>,
    /// When it was last seen, by discovery or by answering a control request.
    pub seen_unix: Option<u64>,
    /// When it was last heard **over UDP at the address the link streams to**:
    /// by discovery or by answering a control request - not by its HTTP API.
    /// This is what [`Registry::unheard`] reads. `seen_unix` also counts a
    /// status read, which is right for "is it alive" and wrong for "has it
    /// moved": a status read that still succeeds (a panel behind a forwarded
    /// port; two simulators sharing loopback) kept refreshing it, so a panel
    /// whose frames were going nowhere was never looked for (card 156's
    /// diagnosis of the `moved` test, which failed about one run in six).
    pub udp_seen_unix: Option<u64>,
    /// The last telemetry heard, and when.
    pub telemetry: Option<Telem>,
    /// The last control request that failed, if the last one did.
    pub last_error: Option<String>,
    /// Card 180: what the device's own HTTP API last said about itself.
    /// `None` on firmware that does not serve one, which is normal.
    pub facts: Option<DeviceFacts>,
    /// Card 199: `GET /api/v1/panic`'s last answer - the panic breadcrumb, the
    /// last firmware update's outcome, why this boot started. `None` on
    /// firmware that serves no such route (older than 0.5.2) and on a device
    /// that has not been read yet; both are normal.
    pub panic: Option<PanicFacts>,
    /// The `boot_id` a panic read has already been asked for, whether or not
    /// it answered - so [`Registry::want_panic`] does not ask again until the
    /// device reboots. Live only, never persisted and never on the wire: it is
    /// about this process's own last poll, not a fact about the device.
    panic_asked_for: Option<u32>,
    /// How reading that is going. Never a fault in `/healthz`.
    pub http: HttpHealth,
    /// Which port its HTTP API is on, when it is not
    /// [`crate::devhttp::DEFAULT_PORT`]. Live only, never persisted: it is how
    /// a simulator - which cannot bind 80 without root - is talked to.
    pub http_port: Option<u16>,
    /// Card 164: what this panel has cost the network, and how fast it is
    /// costing it. Live only, never persisted - it is about this process's own
    /// uptime, and a total read back out of a file would be a lie about it.
    pub traffic: TrafficMeter,
    /// When this studio last asked this panel to reboot, and has not yet seen
    /// it come back (card 195).
    ///
    /// Live only, never persisted and never on the wire: it is about this
    /// process's own last few minutes. A studio that restarts has asked for
    /// nothing, which is the honest answer - it cannot know what the studio
    /// before it did.
    pub reboot_ask: Option<Instant>,
}

impl DeviceRecord {
    /// A name for a human: what was set here, then the device's own, then the
    /// instance name, then the id.
    #[must_use]
    pub fn label(&self) -> String {
        if !self.stored.name.is_empty() {
            return self.stored.name.clone();
        }
        if let Some(n) = self.resolved.as_ref().and_then(|d| d.info.as_ref()).map(|i| i.name.clone()) {
            if !n.is_empty() {
                return n;
            }
        }
        if !self.stored.instance.is_empty() {
            return self.stored.instance.clone();
        }
        self.stored.id.clone()
    }

    /// How to reach it, best first.
    #[must_use]
    pub fn reach(&self) -> Reach {
        // A human-typed name wins over a stale resolution: re-resolving is how
        // a link follows the device across a DHCP lease.
        if let Some(d) = &self.resolved {
            return Reach::Resolved(Box::new(d.clone()));
        }
        if !self.stored.address.is_empty() {
            if let Some(a) = parse_addr(&self.stored.address) {
                return Reach::Addr(a);
            }
            return Reach::Name(self.stored.address.clone());
        }
        if !self.stored.instance.is_empty() {
            return Reach::Name(self.stored.instance.clone());
        }
        Reach::Unknown
    }

    /// The control port, when we know it.
    #[must_use]
    pub fn control_addr(&self) -> Option<SocketAddr> {
        self.resolved.as_ref().map(|d| d.control)
    }

    /// Where its HTTP API is: **the resolved address, on port 80**.
    ///
    /// Derived rather than stored, because an address is a way of reaching a
    /// panel and not a name for it - the same reason the registry is keyed by
    /// id. `default_port` is the studio-wide override (`--device-http-port`);
    /// [`DeviceRecord::http_port`] is this device's own, and wins.
    ///
    /// `None` until the device has been resolved: there is nowhere to ask yet.
    #[must_use]
    pub fn http_addr(&self, default_port: u16) -> Option<SocketAddr> {
        let port = self.http_port.unwrap_or(default_port);
        self.resolved.as_ref().map(|d| SocketAddr::new(d.frame.ip(), port))
    }

    /// True when the device has been heard from recently.
    #[must_use]
    pub fn online(&self, within_s: u64) -> bool {
        self.seen_unix.is_some_and(|s| unix_now().saturating_sub(s) <= within_s)
    }
}

/// What the device says about itself (spec 6.7), in the shape the dashboard
/// puts on screen.
#[derive(Clone, Debug, Serialize)]
pub struct Telem {
    /// When this was heard, so the UI can say "3 s ago" or "never".
    pub heard_unix: u64,
    pub uptime_s: u64,
    pub rssi_dbm: i8,
    pub brightness: u8,
    pub state: &'static str,
    pub frames_rx: u32,
    pub frames_shown: u32,
    pub drops: Drops,
    pub interarrival_us: u16,
    pub jitter_us: u16,
    pub decode_us: u16,
}

/// The four drop counters, by cause, plus the gaps the device noticed.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Drops {
    pub stale: u32,
    pub superseded: u32,
    pub decode: u32,
    pub rejected: u32,
    pub seq_gaps: u32,
}

impl Telem {
    #[must_use]
    pub fn of(t: &Telemetry) -> Self {
        Telem {
            heard_unix: unix_now(),
            uptime_s: u64::from(t.uptime_ms) / 1000,
            rssi_dbm: t.rssi_dbm,
            brightness: t.brightness,
            state: state_name(t.state),
            frames_rx: t.frames_rx,
            frames_shown: t.frames_shown,
            drops: Drops {
                stale: t.frames_dropped_stale,
                superseded: t.frames_dropped_superseded,
                decode: t.frames_dropped_decode,
                rejected: t.frames_rejected,
                seq_gaps: t.seq_gaps,
            },
            interarrival_us: t.interarrival_us,
            jitter_us: t.jitter_us,
            decode_us: t.decode_us,
        }
    }
}

// ------------------------------------ card 164: what a panel costs the net ----

/// The rate's time constant: a change is most of the way into the figure after
/// about this long.
///
/// Five seconds because the number is read by a person watching a page, and
/// the two things worth seeing - a stream starting and a panel going away -
/// should show up while they are still looking, without the figure twitching
/// with every frame that is a few bytes larger than the last.
pub const TRAFFIC_WINDOW: Duration = Duration::from_secs(5);

/// **What is added per datagram to turn a payload into a figure for the
/// wire**: an IPv4 header and a UDP header (`screeny::UDP_OVERHEAD`).
///
/// There is no equivalent for the HTTP path and none is invented: TCP's
/// retransmissions, its ACKs and its handshake are invisible from user space,
/// so [`Traffic::http`] is the bytes written to and read from the socket and
/// nothing else. The README says so, and so does the page.
const UDP: u64 = screeny::UDP_OVERHEAD;

/// Bytes and packets one way on one path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Flow {
    /// Payload bytes. For UDP, the datagram's payload; for HTTP, the bytes
    /// written to or read from the socket. Headers are **not** in here.
    pub bytes: u64,
    /// Datagrams, for a UDP path. For HTTP: requests one way, replies the
    /// other - a connection's worth, not a segment's.
    pub packets: u64,
}

impl Flow {
    fn take(&mut self, w: screeny::Wire) {
        self.bytes += w.bytes;
        self.packets += w.packets;
    }

    /// Payload plus `overhead` bytes per packet.
    fn on_the_wire(&self, overhead: u64) -> u64 {
        self.bytes + self.packets * overhead
    }
}

/// One path, both ways.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct PathFlow {
    pub out: Flow,
    /// `in` is a keyword in Rust and the obvious name on the wire.
    #[serde(rename = "in")]
    pub inbound: Flow,
}

/// Bytes a second, each way, on each path.
///
/// **Computed in one place** - [`Registry::sample_traffic`], on the
/// supervisor's own tick - so that every browser reads the same number and
/// nothing has to divide two counters it fetched at two different moments. An
/// exponentially weighted average with [`TRAFFIC_WINDOW`] as its time
/// constant, which means the figure is independent of how often the tick
/// happens to run.
///
/// These are **wire** bytes: payload plus [`UDP`] per datagram for the two UDP
/// paths, and payload alone for HTTP.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Rates {
    pub out: f64,
    #[serde(rename = "in")]
    pub inbound: f64,
    pub frames_out: f64,
    pub frames_in: f64,
    pub control_out: f64,
    pub control_in: f64,
    pub http_out: f64,
    pub http_in: f64,
}

/// **What one panel has cost this studio's network, over every path.**
///
/// The three paths are the three conversations the studio has with a panel,
/// and they are kept apart because they answer different questions:
///
/// * `frames` - the stream, UDP on the frame port, and the `TELEMETRY` and
///   `BUSY` datagrams that come back along it. Nearly all of the traffic.
/// * `control` - UDP on the control port: the telemetry poll, brightness,
///   identify, rename, reboot, and the `GET_INFO` handshake at the head of
///   every link session. Requests out, replies in.
/// * `http` - TCP port 80 on the panel: `GET /api/v1/status` every ten
///   seconds (card 180). Bytes written and bytes read.
///
/// **What is not here**, deliberately: the mDNS browse and the broadcast probe
/// (card 141). Neither is traffic *with a panel* - a browse is multicast to
/// nobody in particular and a probe is one datagram to the subnet that every
/// panel answers - so attributing either to a device would be inventing a
/// number.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Traffic {
    pub frames: PathFlow,
    pub control: PathFlow,
    pub http: PathFlow,
    /// Every path together, **with the per-datagram overhead added**, so
    /// `bytes` here is what the link carried rather than what the payloads
    /// held. `packets` is the plain sum.
    pub total: PathFlow,
    pub rate: Rates,
    /// The rate's time constant, in seconds, so the page can say what the
    /// number is an average over rather than guessing.
    pub window_s: f64,
}

/// The counters, plus what [`Registry::sample_traffic`] needs to turn them
/// into a rate. Not on the wire: [`Traffic`] is.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrafficMeter {
    /// Cumulative, and only ever growing.
    pub totals: Traffic,
    /// The last lifetime figure read off this device's link. The link is
    /// rebuilt whenever the studio re-aims it, and a rebuilt link counts from
    /// zero, so a figure **below** the last one is a new link and is taken
    /// whole rather than as a difference.
    last_link: screeny::LinkTraffic,
    /// When the rate was last worked out, and the wire bytes it was worked out
    /// from: `[frames_out, frames_in, control_out, control_in, http_out,
    /// http_in]`.
    at: Option<Instant>,
    was: [u64; 6],
}

impl TrafficMeter {
    /// The six wire figures, in the order [`TrafficMeter::was`] holds them.
    fn wire(&self) -> [u64; 6] {
        let t = &self.totals;
        [
            t.frames.out.on_the_wire(UDP),
            t.frames.inbound.on_the_wire(UDP),
            t.control.out.on_the_wire(UDP),
            t.control.inbound.on_the_wire(UDP),
            t.http.out.on_the_wire(0),
            t.http.inbound.on_the_wire(0),
        ]
    }

    /// The counters as `/api/v1/status` reports them: the three paths as they
    /// stand **now**, with `total` rolled up from them.
    ///
    /// Rolled up here rather than on the tick, because the paths are added to
    /// whenever a request finishes and the tick is once a second: a `total`
    /// computed on the tick would be a second behind the rows above it, and a
    /// page that showed both would be visibly inconsistent with itself.
    #[must_use]
    pub fn reported(&self) -> Traffic {
        let mut t = self.totals;
        t.total.out = Flow {
            bytes: t.frames.out.on_the_wire(UDP) + t.control.out.on_the_wire(UDP) + t.http.out.bytes,
            packets: t.frames.out.packets + t.control.out.packets + t.http.out.packets,
        };
        t.total.inbound = Flow {
            bytes: t.frames.inbound.on_the_wire(UDP) + t.control.inbound.on_the_wire(UDP) + t.http.inbound.bytes,
            packets: t.frames.inbound.packets + t.control.inbound.packets + t.http.inbound.packets,
        };
        t.window_s = TRAFFIC_WINDOW.as_secs_f64();
        t
    }

    /// Advance the rate. `now` is passed in so that one pass over the fleet
    /// uses one instant.
    fn sample(&mut self, now: Instant) {
        let wire = self.wire();
        let Some(then) = self.at.replace(now) else {
            // The first pass has no interval to divide by, so it sets the
            // baseline and reports nothing. A studio that has just started
            // says "0 KB/s" for one tick rather than inventing a spike.
            self.was = wire;
            return;
        };
        let dt = now.saturating_duration_since(then).as_secs_f64();
        if dt <= 0.0 {
            return;
        }
        // 1 - e^(-dt/tau): the weight this interval carries. Working it out
        // from dt rather than fixing it means the figure means the same thing
        // whatever the tick period is, which is what lets the tests run the
        // supervisor fast.
        let alpha = 1.0 - (-dt / TRAFFIC_WINDOW.as_secs_f64()).exp();
        let step = |rate: &mut f64, i: usize| {
            let delta = wire[i].saturating_sub(self.was[i]) as f64;
            *rate += alpha * (delta / dt - *rate);
        };
        let r = &mut self.totals.rate;
        step(&mut r.frames_out, 0);
        step(&mut r.frames_in, 1);
        step(&mut r.control_out, 2);
        step(&mut r.control_in, 3);
        step(&mut r.http_out, 4);
        step(&mut r.http_in, 5);
        r.out = r.frames_out + r.control_out + r.http_out;
        r.inbound = r.frames_in + r.control_in + r.http_in;
        self.was = wire;
    }
}

// --------------------------------------------------- the device's own HTTP ----

/// Bytes of core-0 stack left untouched, below which the margin has started to
/// go and the page says so quietly.
///
/// **Card 195: these are measured on the real
/// device across several builds, and this is the reasoning.** Interrupts land
/// on core 0's stack at about **256 bytes of context per level**, and
/// `stack_free` is a *high-water mark*: it only ever falls, so a reading is the
/// worst moment since boot rather than this moment. A healthy 0.4.3 reads
/// **17-20 KB**; the 0.4.0 build that worried them read **4-5 KB**. Eight
/// kilobytes is well below healthy and still well above the build that was in
/// trouble, which is what makes it a warning rather than a fault.
///
/// Card 180's single line at 2048 was chosen from two readings, one of which
/// (`stack_free 4312`) turned out to be the *unhealthy* build. It was too late
/// to be worth printing: by then the margin is a few interrupt levels.
pub const STACK_WARN: u32 = 8192;

/// Bytes of core-0 stack left untouched, below which it is a fault.
///
/// Four kilobytes is sixteen levels of interrupt context, or one exception
/// frame plus picoserve's buffer - the next things that would ask for the
/// space. Below it the next interrupt is the one that smashes the guard.
/// This is the level the 0.4.0 build was sitting at.
pub const STACK_FAULT: u32 = 4096;

/// The old name for the fault level, kept because it is on the wire.
///
/// `/api/v1/status` is additive: a field that has been published is not
/// removed. [`DeviceFacts::low_stack`] is still there and still means what it
/// said, which is now the fault level.
pub const LOW_STACK: u32 = STACK_FAULT;

/// Fraction of the heap in use, above which the page says so.
///
/// **The measured warning line, and a fault** (card 195): steady state is
/// 45.6 KB of 90 KB - **51%** - and the measured worst instant, with the setup
/// AP up, is 54 KB (**60%**). 85% is therefore nowhere near either, so a line
/// drawn there is a real change rather than noise, and it still leaves ~13 KB,
/// which is more than one connection and one frame buffer ever ask for.
pub const HIGH_HEAP: f32 = 0.85;

/// How long after the studio asks a panel to reboot a new `boot_id` still
/// counts as *that* reboot.
///
/// The device is back in a few seconds, but the studio does not look that
/// often: the status poll is [`crate::MIN_DEVICE_HTTP_EVERY`] (10 s) and a read
/// that fails while the device is down backs the next one off, up to twelve
/// passes. Two minutes is that cap - the longest the studio can go between
/// reads - so an ask covers the first read that can possibly see the reboot.
/// Anything later we would rather count as a crash than excuse as ours.
pub const REBOOT_ASK_WINDOW: Duration = Duration::from_secs(120);

/// Reset reasons that are the ordinary way this device restarts: power
/// applied, our own `REBOOT` or `esp_restart`, and the reset pin - which is
/// `espflash` on this bench. Anything else (brownout, panic, either watchdog)
/// is something that *happened to* the device and is worth a person's eye.
///
/// **`software` cannot be trusted to mean "clean".** The ESP32 cannot tell a
/// panic from any other software reset, so today `software` covers our own
/// `REBOOT`, a reflash, *and* a crash-and-restart. That is why it is quiet here
/// and why [`DeviceFacts::unasked_reboots`] exists: the honest signal is not
/// the reason but a `boot_id` change the studio did not ask for. When the
/// firmware reports panics (an RTC breadcrumb, a later firmware card), `panic`
/// becomes a reason of its own and this comment can go.
const QUIET_RESETS: [ResetReason; 3] = [ResetReason::PowerOn, ResetReason::Software, ResetReason::External];

/// What only the device knows: `GET /api/v1/status`, plus what the studio can
/// work out by having watched more than one of them.
///
/// The device's own shapes are embedded rather than restated (one
/// implementation of each thing): the fields of
/// [`StatusReply`](screeny_device_api::reply::StatusReply) are flattened
/// straight into this on the wire, so a field the firmware adds reaches the
/// page in the same commit that adds it.
///
/// **`Debug` is written by hand and redacts the SSID.** The status payload
/// carries the real network name, which is credential-adjacent: it may be
/// on the owner's page and in the studio's own
/// `/api/v1/status`, and it must never reach a log line, the state file, a
/// fixture or a commit message. Deriving `Debug` here would put it one
/// `{:?}` away from stderr, and [`DeviceRecord`] derives `Debug`.
#[derive(Clone, Serialize)]
pub struct DeviceFacts {
    /// When this was read, so the page can say "3 s ago".
    pub heard_unix: u64,
    /// Everything the device said, in the shared shapes.
    #[serde(flatten)]
    pub reply: StatusReply,
    /// How many times `boot_id` has **changed** since this studio started:
    /// the number of times the device rebooted while we were watching.
    ///
    /// Counted from `boot_id` and never inferred from uptime: a device whose
    /// link merely flapped keeps its `boot_id`, and a device that rebooted
    /// draws a new one.
    pub reboots: u32,
    /// How many of those reboots **this studio did not ask for**, and whose
    /// `reset_reason` is `software` (card 195).
    ///
    /// The studio asks for exactly one kind of reboot - its own reboot control,
    /// `POST /api/v1/device/reboot` - and remembers when it did
    /// ([`Registry::asked_to_reboot`]). A `boot_id` change with no ask behind
    /// it, on a chip that says it restarted in software, is the nearest thing
    /// to "it may have crashed" this firmware can offer: see [`QUIET_RESETS`]
    /// for why `software` alone means nothing. **A reflash looks exactly the
    /// same from here**, which is why the page says this quietly and it is
    /// never a fault, and never a problem in `/healthz`.
    pub unasked_reboots: u32,
    /// The link is up: a non-null `ip`.
    ///
    /// Until firmware card 223, `wifi_state` can read `failed` on a device
    /// that is plainly connected, because it is the sticky result of the last
    /// credentials *attempt*. The address is the honest answer, so it is the
    /// one the page believes.
    pub link_up: bool,
    /// `wifi_state` says `failed` and yet there is an address. A note about
    /// the last WiFi change, **never** a fault.
    pub wifi_stale_failure: bool,
    /// Free stack is below [`STACK_WARN`]: the margin has started to go.
    ///
    /// **Implied by [`DeviceFacts::stack_fault`]** - a device below the fault
    /// line is also below the warning one - so the page reads the fault first.
    pub stack_warn: bool,
    /// Free stack is below [`STACK_FAULT`].
    pub stack_fault: bool,
    /// The old name for [`DeviceFacts::stack_fault`], kept because it is on the
    /// wire: `/api/v1/status` is additive and a published field stays.
    pub low_stack: bool,
    /// The heap is fuller than [`HIGH_HEAP`].
    pub low_heap: bool,
    /// The running slot is not `valid`.
    pub bad_fw_state: bool,
    /// The chip last restarted for a reason that is not one of
    /// [`QUIET_RESETS`]: a brownout, a panic or a watchdog.
    pub odd_reset: bool,
}

impl DeviceFacts {
    /// Read one status reply, carrying forward what only a previous read can
    /// say - the reboot counts.
    ///
    /// `ask` is the reboot this studio asked for and has not yet seen happen.
    /// A `boot_id` change **consumes** it, whether or not it was still fresh,
    /// so that one ask excuses exactly one reboot and a stale ask cannot excuse
    /// a second one months later.
    #[must_use]
    pub fn of(reply: StatusReply, previous: Option<&DeviceFacts>, ask: &mut Option<Instant>) -> Self {
        let rebooted = previous.is_some_and(|p| p.reply.boot_id != reply.boot_id);
        let reboots = previous.map_or(0, |p| p.reboots) + u32::from(rebooted);

        // Who asked for it. The ask is taken on any reboot - it was about the
        // one that has now happened - and only counts if it is inside the
        // window, because the alternative is an ask from last week quietly
        // excusing tonight's crash.
        let asked_for = rebooted && ask.take().is_some_and(|at| at.elapsed() <= REBOOT_ASK_WINDOW);
        let unasked = rebooted && !asked_for && reply.reset_reason == ResetReason::Software;
        let unasked_reboots = previous.map_or(0, |p| p.unasked_reboots) + u32::from(unasked);

        let link_up = reply.ip.is_some();
        let heap = if reply.heap_size == 0 {
            0.0
        } else {
            reply.heap_used as f32 / reply.heap_size as f32
        };
        let stack_fault = reply.stack_free < STACK_FAULT;
        DeviceFacts {
            heard_unix: unix_now(),
            reboots,
            unasked_reboots,
            link_up,
            wifi_stale_failure: link_up && reply.wifi_state == WifiState::Failed,
            stack_warn: reply.stack_free < STACK_WARN,
            stack_fault,
            low_stack: stack_fault,
            low_heap: heap > HIGH_HEAP,
            bad_fw_state: reply.fw_state != FwState::Valid,
            odd_reset: !QUIET_RESETS.contains(&reply.reset_reason),
            reply,
        }
    }
}

/// Hand-written, and deliberately blind to the SSID. See the type's docs.
impl std::fmt::Debug for DeviceFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceFacts")
            .field("fw", &self.reply.fw.as_str())
            .field("boot_id", &self.reply.boot_id)
            .field("uptime_ms", &self.reply.uptime_ms)
            .field("heap", &format_args!("{}/{}", self.reply.heap_used, self.reply.heap_size))
            .field("stack_free", &self.reply.stack_free)
            .field("wifi_state", &self.reply.wifi_state)
            .field("ssid", &if self.reply.ssid.is_some() { "<redacted>" } else { "<none>" })
            .field("reboots", &self.reboots)
            .field("unasked_reboots", &self.unasked_reboots)
            .field("reset_reason", &self.reply.reset_reason)
            .field("store_errors", &self.reply.store_errors)
            .finish_non_exhaustive()
    }
}

/// `GET /api/v1/panic`'s last answer (card 199): the panel's RTC panic
/// breadcrumb, the last firmware update's outcome and why this boot started -
/// the facts that cannot change while the device runs, and so are read once
/// per `boot_id` on a route of their own rather than on every status poll
/// (see [`screeny_device_api::reply::PanicReply`]'s docs, and
/// `crate::fleet::status_once`, which asks for it).
///
/// Its own type beside [`DeviceFacts`] rather than folded into it, the same
/// way [`PanicReply`] is its own route beside `StatusReply`: it is read at a
/// different rate and can be absent - older firmware, and a device that has
/// not rebooted since the studio started asking - independently of whether
/// `facts` is there.
///
/// `#[derive(Debug)]` is safe here, unlike `DeviceFacts`: nothing in
/// [`PanicReply`] carries the SSID or anything else credential-adjacent.
#[derive(Clone, Debug, Serialize)]
pub struct PanicFacts {
    /// When this was read.
    pub heard_unix: u64,
    /// Everything the device said, in the shared shape.
    #[serde(flatten)]
    pub reply: PanicReply,
    /// `last_panic.consecutive > 1`: the one fact here that colours the
    /// Picture screen's status chip (`common.js`'s `attention`). Decided once,
    /// here, so the page reads a flag rather than a threshold of its own.
    pub repeat: bool,
}

impl PanicFacts {
    #[must_use]
    fn of(reply: PanicReply) -> Self {
        let repeat = reply.last_panic.as_ref().is_some_and(|p| p.consecutive > 1);
        PanicFacts { heard_unix: unix_now(), reply, repeat }
    }
}

/// What one status read changed, for the caller's log line.
///
/// Deliberately only counters and flags: the reply itself carries the SSID and
/// is never handed back out of [`Registry::heard_http`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Heard {
    /// Reboots since the studio started.
    pub reboots: u32,
    /// How many of those the studio did not ask for.
    pub unasked_reboots: u32,
    /// This read is the first since the device rebooted.
    pub rebooted: bool,
    /// ...and nobody here asked it to.
    pub unasked: bool,
}

/// How reading a device's HTTP API is going.
///
/// **None of this is ever a problem in `/healthz`.** A panel with no HTTP
/// server is normal - older firmware, or the portable profile pointed at a
/// simulator started with `--no-http` - and a panel that is switched off is
/// normal life for a thing that runs for months.
#[derive(Clone, Debug, Default, Serialize)]
pub struct HttpHealth {
    /// True once the studio has decided this device serves no HTTP API. It
    /// keeps looking, slowly, because a firmware update changes the answer.
    pub absent: bool,
    /// The last failure, in words; `None` when the last read worked.
    pub last_error: Option<String>,
    /// Status reads that worked, since the studio started.
    pub reads: u64,
    /// Whether the one line about this device has been said. Not on the wire:
    /// it is about this process's stderr, not about the device.
    #[serde(skip)]
    pub said: bool,
    /// **Card 118: the panel rebooted, so ask it about itself now.** Set by
    /// [`Registry::heard`] when UDP telemetry says the uptime went backwards,
    /// and taken by the status poller, which drops this device's backoff.
    ///
    /// Not on the wire: it is about the studio's next ten seconds, not about
    /// the device.
    #[serde(skip)]
    pub retry_now: bool,
}

fn state_name(s: u8) -> &'static str {
    match s {
        dev_state::IDLE => "idle",
        dev_state::LIVE => "live",
        dev_state::HOLD => "hold",
        dev_state::IDENTIFY => "identify",
        dev_state::PROVISIONING => "provisioning",
        _ => "unknown",
    }
}

/// How a discovery browse is going, for the status route. A browse that finds
/// nothing is normal, not an error (`crates/screeny/README.md`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct DiscoveryHealth {
    pub enabled: bool,
    pub browses: u64,
    pub last_browse_unix: Option<u64>,
    pub last_found: usize,
    /// The last browse that failed. Not a reason to be unhealthy: the studio
    /// works from configured addresses alone, by design.
    pub last_error: Option<String>,
    /// Card 141: `GET_INFO` probes sent looking for a panel that has moved.
    /// Far fewer than `browses`: one only happens when a device has been
    /// unheard past `Config::stale_after`.
    pub probes: u64,
    /// How many devices a probe has moved to a new address.
    pub moved: u64,
    /// The last probe that failed, in words. Not a reason to be unhealthy
    /// either - a machine with no broadcast route still runs the studio.
    pub last_probe_error: Option<String>,
}

/// Every device the studio knows about.
#[derive(Default)]
pub struct Registry {
    devices: Mutex<BTreeMap<String, DeviceRecord>>,
    discovery: Mutex<DiscoveryHealth>,
}

/// What changed, so the caller can move a player and persist.
#[derive(Clone, Debug, Default)]
pub struct Changes {
    /// Devices whose key changed when their real id arrived: `(old, new)`.
    pub renamed: Vec<(String, String)>,
    /// Devices that appeared.
    pub added: Vec<String>,
}

/// A device a probe found somewhere else (card 141).
#[derive(Clone, Debug)]
pub struct Moved {
    /// The device's own id: the whole reason this is a move and not a new
    /// device.
    pub id: String,
    /// Where the studio thought it was. Empty means "nowhere we knew of" - a
    /// panel discovered by mDNS on a machine where mDNS has since stopped
    /// working, which is the Docker case.
    pub from: String,
    /// Where it answered from.
    pub to: String,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Registry::default()
    }

    /// Adopt what was in the state file.
    pub fn load(&self, stored: Vec<StoredDevice>) {
        let mut devices = self.lock();
        for s in stored {
            if s.id.is_empty() {
                continue;
            }
            devices.insert(
                s.id.clone(),
                DeviceRecord { stored: s, ..DeviceRecord::default() },
            );
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, DeviceRecord>> {
        self.devices.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn list(&self) -> Vec<DeviceRecord> {
        self.lock().values().cloned().collect()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<DeviceRecord> {
        self.lock().get(id).cloned()
    }

    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// What to write to the state file.
    #[must_use]
    pub fn stored(&self) -> Vec<StoredDevice> {
        self.lock().values().map(|d| d.stored.clone()).collect()
    }

    #[must_use]
    pub fn discovery_health(&self) -> DiscoveryHealth {
        self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn set_discovery_enabled(&self, on: bool) {
        self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner).enabled = on;
    }

    /// Add a device a human typed in.
    ///
    /// `to` is an `IP[:PORT]` or a DNS-SD instance name. The id is provisional
    /// until the device answers; [`Registry::resolved`] replaces it then.
    ///
    /// # Errors
    ///
    /// If `to` is empty.
    pub fn add_manual(&self, to: &str, name: &str) -> Result<String, String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("give a name or an address".into());
        }
        let mut devices = self.lock();
        // Already known by that address or instance name? Then this is a
        // rename, not a second device.
        if let Some(existing) = devices.values_mut().find(|d| d.stored.address == to || d.stored.instance == to) {
            existing.stored.manual = true;
            if !name.is_empty() {
                existing.stored.name = name.to_string();
            }
            if existing.stored.address.is_empty() {
                existing.stored.address = to.to_string();
            }
            return Ok(existing.stored.id.clone());
        }
        let id = format!("{PENDING}:{to}");
        let stored = StoredDevice {
            id: id.clone(),
            name: name.to_string(),
            instance: if parse_addr(to).is_some() { String::new() } else { to.to_string() },
            address: to.to_string(),
            manual: true,
        };
        devices.insert(id.clone(), DeviceRecord { stored, ..DeviceRecord::default() });
        Ok(id)
    }

    /// This device is somewhere else now.
    ///
    /// An address is a way of reaching a panel, not a name for it, so when the
    /// panel moves something has to say where to: a human
    /// (`POST /api/v1/devices/add`), or - card 141 - a `GET_INFO` probe that
    /// heard the panel's own `id=` at a new address ([`Registry::probed`]).
    /// The device keeps its id, and therefore its player and everything it was
    /// playing; only the way there changes. Clearing the resolution makes the
    /// next poll ask the new address who it is, which is also how a wrong
    /// address is caught.
    pub fn set_address(&self, id: &str, to: &str) -> bool {
        let to = to.trim();
        match self.lock().get_mut(id) {
            Some(d) => {
                d.stored.address = to.to_string();
                if parse_addr(to).is_none() {
                    d.stored.instance = to.to_string();
                }
                d.resolved = None;
                d.last_error = None;
                true
            }
            None => false,
        }
    }

    /// A device we have not heard from in a long time: throw away where we
    /// thought it was and find out again.
    ///
    /// An address from an hour ago is worse than no address at all. For a
    /// device known by name this is what sends the next browse looking; for
    /// one known by address it costs a `GET_INFO` and catches the case where
    /// something else has taken that address.
    pub fn stale(&self, id: &str) {
        if let Some(d) = self.lock().get_mut(id) {
            d.resolved = None;
        }
    }

    /// Forget a device entirely. The caller stops its player.
    pub fn forget(&self, id: &str) -> bool {
        self.lock().remove(id).is_some()
    }

    /// What this studio calls it. Empty means "use the device's own name".
    pub fn rename(&self, id: &str, name: &str) -> bool {
        match self.lock().get_mut(id) {
            Some(d) => {
                d.stored.name = name.trim().to_string();
                true
            }
            None => false,
        }
    }

    /// A device answered, or a browse found it. Merge it in, adopting its real
    /// id if this is the first time we have heard it.
    ///
    /// Returns `(id, renamed_from)`.
    pub fn resolved(&self, dev: &Device) -> (String, Option<String>) {
        let real = dev.info.as_ref().map(|i| i.id.clone()).filter(|i| !i.is_empty());
        let mut devices = self.lock();

        // The key it should have: its own id, or a provisional one built from
        // its instance name so that a device seen twice is one device.
        let key = real.clone().unwrap_or_else(|| format!("{PENDING}:{}", dev.instance));

        // Which record is this, if any? By real id, then by the provisional id
        // a human's typing made, then by instance name, then by address.
        let typed_addr = dev.frame.to_string();
        let ip_only = dev.frame.ip().to_string();
        let old_key = devices
            .keys()
            .find(|k| **k == key)
            .or_else(|| {
                devices
                    .iter()
                    .find(|(_, d)| {
                        (!d.stored.instance.is_empty() && d.stored.instance == dev.instance)
                            || (!d.stored.address.is_empty()
                                && (d.stored.address == typed_addr || d.stored.address == ip_only || d.stored.address == dev.instance))
                    })
                    .map(|(k, _)| k)
            })
            .cloned();

        let mut record = match &old_key {
            Some(k) => devices.remove(k).unwrap_or_default(),
            None => DeviceRecord::default(),
        };
        record.stored.id = key.clone();
        if record.stored.instance.is_empty() && !dev.instance.is_empty() && parse_addr(&dev.instance).is_none() {
            record.stored.instance = dev.instance.clone();
        }
        record.resolved = Some(dev.clone());
        record.seen_unix = Some(unix_now());
        record.udp_seen_unix = Some(unix_now());
        record.last_error = None;
        devices.insert(key.clone(), record);

        let renamed = old_key.filter(|k| *k != key);
        (key, renamed)
    }

    /// Record telemetry heard from a device.
    ///
    /// **Card 118: uptime that went backwards is a reboot**, and the one thing
    /// UDP telemetry can say about one - a panel that has restarted counts from
    /// zero again. Reboots themselves are still counted from `boot_id` and
    /// nothing else ([`Registry::heard_http`]); this is only a hint to the
    /// status poller that now is the moment to ask, because a panel comes back
    /// on UDP seconds before its HTTP workers are listening and whatever the
    /// poller decided while it was refusing is about the panel before the
    /// reboot.
    pub fn heard(&self, id: &str, t: &Telemetry) {
        if let Some(d) = self.lock().get_mut(id) {
            let fresh = Telem::of(t);
            if d.telemetry.as_ref().is_some_and(|was| fresh.uptime_s < was.uptime_s) {
                d.http.retry_now = true;
            }
            d.telemetry = Some(fresh);
            d.seen_unix = Some(unix_now());
            d.udp_seen_unix = Some(unix_now());
            d.last_error = None;
        }
    }

    /// Take the "this panel rebooted, ask it now" flag, clearing it.
    ///
    /// The status poller is the only caller: it is a hand-off between the two
    /// polls, not a fact about the device, which is why it is taken rather than
    /// read.
    pub fn take_http_retry(&self, id: &str) -> bool {
        match self.lock().get_mut(id) {
            Some(d) => std::mem::take(&mut d.http.retry_now),
            None => false,
        }
    }

    // ----------------------------- card 164: what this panel costs the net ----

    /// **Read a player's link totals into this device's counters.**
    ///
    /// The link's figure is a lifetime one and only ever grows, so the
    /// ordinary case is a difference. The exception is a link the studio threw
    /// away and rebuilt - re-aiming it at a resolved address, output switched
    /// off and on, a panel that moved - which counts from zero again; a figure
    /// below the last one is exactly that, and is taken whole.
    ///
    /// Called once a second from the supervisor, which is the one place that
    /// already walks every player. Nothing on the frame path touches this.
    pub fn metered_link(&self, id: &str, now: screeny::LinkTraffic) {
        let mut devices = self.lock();
        let Some(d) = devices.get_mut(id) else { return };
        let m = &mut d.traffic;
        // **One decision for the whole link**, not one per counter. A rebuilt
        // link resets every field at once, and the frame counters are the ones
        // that will have moved, so a single field going backwards is what
        // says so - and the handshake, whose figure is the same 24 bytes in
        // both lives, is then taken whole with the rest rather than looking
        // unchanged.
        let was = m.last_link;
        let back = |now: screeny::Wire, was: screeny::Wire| now.packets < was.packets || now.bytes < was.bytes;
        let fresh = back(now.frames.out, was.frames.out)
            || back(now.frames.inbound, was.frames.inbound)
            || back(now.control.out, was.control.out)
            || back(now.control.inbound, was.control.inbound);
        let step = |to: &mut Flow, now: screeny::Wire, was: screeny::Wire| {
            to.bytes += if fresh { now.bytes } else { now.bytes - was.bytes };
            to.packets += if fresh { now.packets } else { now.packets - was.packets };
        };
        step(&mut m.totals.frames.out, now.frames.out, was.frames.out);
        step(&mut m.totals.frames.inbound, now.frames.inbound, was.frames.inbound);
        step(&mut m.totals.control.out, now.control.out, was.control.out);
        step(&mut m.totals.control.inbound, now.control.inbound, was.control.inbound);
        m.last_link = now;
    }

    /// One control request, on this device's control port: what it cost, both
    /// ways. A fresh [`screeny::ControlClient`] per request, so this is added
    /// rather than differenced.
    pub fn metered_control(&self, id: &str, t: screeny::Traffic) {
        if let Some(d) = self.lock().get_mut(id) {
            d.traffic.totals.control.out.take(t.out);
            d.traffic.totals.control.inbound.take(t.inbound);
        }
    }

    /// One `GET /api/v1/status` on this device's own HTTP port: the bytes
    /// written and the bytes read.
    ///
    /// Counted **whether or not the read worked**: a request that timed out
    /// still went out, and a reply that turned out not to be this API still
    /// came down the wire. `packets` here is one request and one reply, not
    /// segments - TCP's are not visible from user space and are not guessed
    /// at, which is also why no overhead is added to this path.
    pub fn metered_http(&self, id: &str, out: u64, inbound: u64) {
        if let Some(d) = self.lock().get_mut(id) {
            let t = &mut d.traffic.totals.http;
            if out > 0 {
                t.out.bytes += out;
                t.out.packets += 1;
            }
            if inbound > 0 {
                t.inbound.bytes += inbound;
                t.inbound.packets += 1;
            }
        }
    }

    /// **Work out every device's rate, once.**
    ///
    /// The supervisor calls this on its own tick and nothing else does, which
    /// is what makes the figure the same in every browser: `/api/v1/status`
    /// only reads it. Every device is sampled, including one with no player
    /// and one that has gone away, so a panel that stops being sent frames
    /// *decays to zero* rather than freezing at whatever it was last doing.
    pub fn sample_traffic(&self) {
        self.sample_traffic_at(Instant::now());
    }

    /// [`Registry::sample_traffic`] at a given instant.
    ///
    /// One instant for the whole pass, so two devices sampled in the same tick
    /// divide by the same interval. Exposed so that a test can drive a
    /// five-second time constant without waiting five seconds; the supervisor
    /// passes `Instant::now()`.
    pub fn sample_traffic_at(&self, now: Instant) {
        for d in self.lock().values_mut() {
            d.traffic.sample(now);
        }
    }

    /// What one device has cost, for `/api/v1/status`.
    #[must_use]
    pub fn traffic(&self, id: &str) -> Traffic {
        self.lock().get(id).map(|d| d.traffic.reported()).unwrap_or_default()
    }

    /// Record that a control request failed. Logged by the caller, once.
    pub fn control_failed(&self, id: &str, why: String) {
        if let Some(d) = self.lock().get_mut(id) {
            d.last_error = Some(why);
        }
    }

    /// Record one `GET /api/v1/status`.
    ///
    /// The reply is **merged, not substituted**: the frame counters and the
    /// link state stay where they were (UDP telemetry and the link object),
    /// and this adds what only the device knows. It also counts as having
    /// heard from the device, because it is: the studio just had a TCP
    /// conversation with it at the address it thought it was at.
    ///
    /// Returns [`Heard`] and nothing from the payload, so the caller can say
    /// what happened **without ever being handed the reply** - which carries
    /// the SSID.
    pub fn heard_http(&self, id: &str, reply: StatusReply) -> Option<Heard> {
        let mut devices = self.lock();
        let d = devices.get_mut(id)?;
        // The ask goes in and out by value: `DeviceFacts::of` consumes it if
        // this read is the one that saw the reboot.
        let mut ask = d.reboot_ask;
        let facts = DeviceFacts::of(reply, d.facts.as_ref(), &mut ask);
        d.reboot_ask = ask;
        let reboots = facts.reboots;
        let unasked_reboots = facts.unasked_reboots;
        let rebooted = d.facts.as_ref().is_some_and(|p| p.reboots != reboots);
        let unasked = d.facts.as_ref().is_some_and(|p| p.unasked_reboots != unasked_reboots);
        d.facts = Some(facts);
        d.seen_unix = Some(unix_now());
        d.http.absent = false;
        d.http.last_error = None;
        d.http.reads += 1;
        // A device that is answering again may stop again, and that is worth
        // one more line when it does.
        d.http.said = false;
        Some(Heard { reboots, unasked_reboots, rebooted, unasked })
    }

    // -------------------------------------------- card 199: the panic route ----

    /// **Should the poller ask this device for `GET /api/v1/panic`?**
    ///
    /// True exactly once per `boot_id`: this is the first status read at
    /// `boot_id` whose last panic ask, if any, was for a different one (or
    /// there has never been one). The caller decides the answer with
    /// [`Registry::heard_panic`], which marks the ask taken whether the read
    /// worked or not - that is what keeps this "once", not a retry loop.
    ///
    /// False for a device this registry does not know, which a reboot in the
    /// moment between the status read and this call would otherwise turn into
    /// a panic read against nowhere.
    #[must_use]
    pub fn want_panic(&self, id: &str, boot_id: u32) -> bool {
        self.lock().get(id).is_some_and(|d| d.panic_asked_for != Some(boot_id))
    }

    /// Record that `GET /api/v1/panic` was asked for `boot_id`, and what it
    /// said - `None` for a fault, a 404 included.
    ///
    /// The ask is marked taken **either way**: a 404 (firmware older than
    /// 0.5.2) or a read that timed out must not be retried every ten seconds
    /// until the device reboots again, which is the same "once per `boot_id`"
    /// rule the read itself is bound by. A fault leaves [`DeviceRecord::panic`]
    /// where it was, the same way [`Registry::http_failed`] leaves `facts`:
    /// the last thing the device said about itself stays readable.
    pub fn heard_panic(&self, id: &str, boot_id: u32, reply: Option<PanicReply>) {
        if let Some(d) = self.lock().get_mut(id) {
            d.panic_asked_for = Some(boot_id);
            if let Some(reply) = reply {
                d.panic = Some(PanicFacts::of(reply));
            }
        }
    }

    /// **The studio has just asked this panel to reboot.**
    ///
    /// Recorded *before* the request goes out, not after it succeeds: a panel
    /// that received `REBOOT` and rebooted before its acknowledgement got back
    /// has still done what we asked, and an ask that turns out to have reached
    /// nobody costs only that the next two minutes' crash is not named as one
    /// ([`REBOOT_ASK_WINDOW`]). Crying wolf is the expensive mistake here.
    ///
    /// Returns false if there is no such device.
    pub fn asked_to_reboot(&self, id: &str) -> bool {
        match self.lock().get_mut(id) {
            Some(d) => {
                d.reboot_ask = Some(Instant::now());
                true
            }
            None => false,
        }
    }

    /// Record that a status read did not work.
    ///
    /// Returns true the **first** time it is worth saying out loud for this
    /// device, and never again until a read succeeds: a panel with no HTTP
    /// server would otherwise write a line every ten seconds for months.
    ///
    /// `absent` means "this firmware serves no such API", which is a stronger
    /// claim than "that connection was refused" and one the *caller* makes
    /// (card 118: a panel that has answered before has already disproved it).
    pub fn http_failed(&self, id: &str, absent: bool, why: String) -> bool {
        let mut devices = self.lock();
        let Some(d) = devices.get_mut(id) else { return false };
        d.http.absent = absent;
        d.http.last_error = Some(why);
        // The facts are left where they were on purpose: "the last thing it
        // said about itself" stays readable, with its own age beside it, the
        // same way telemetry does.
        let say = !d.http.said;
        d.http.said = true;
        say
    }

    /// Point this device's HTTP API at a port other than
    /// [`crate::devhttp::DEFAULT_PORT`]. Live only; nothing is persisted.
    pub fn set_http_port(&self, id: &str, port: Option<u16>) -> bool {
        match self.lock().get_mut(id) {
            Some(d) => {
                d.http_port = port;
                // A different port is a different server: whatever we decided
                // about the old one does not apply.
                d.http = HttpHealth::default();
                true
            }
            None => false,
        }
    }

    /// A browse finished. `found` is every device it saw.
    pub fn browsed(&self, found: &[Device], error: Option<String>) -> Changes {
        {
            let mut h = self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            h.browses += 1;
            h.last_browse_unix = Some(unix_now());
            h.last_found = found.len();
            if let Some(e) = &error {
                if h.last_error.as_deref() != Some(e.as_str()) {
                    eprintln!("studio: discovery: {e}");
                }
            }
            h.last_error = error;
        }
        let mut changes = Changes::default();
        for dev in found {
            let known = self.lock().contains_key(&dev.info.as_ref().map_or_else(String::new, |i| i.id.clone()));
            let (id, renamed) = self.resolved(dev);
            if let Some(from) = renamed {
                changes.renamed.push((from, id.clone()));
            } else if !known {
                changes.added.push(id);
            }
        }
        changes
    }

    // ------------------------------------------- card 141: it has moved ----

    /// **The devices a probe would be for**: known by their own id, and not
    /// heard from in longer than `stale_after`.
    ///
    /// Two exclusions, both deliberate:
    ///
    /// * a *provisional* id ([`PENDING`]) is a device nobody has ever spoken
    ///   to, so no `id=` in any reply can be matched against it - probing on
    ///   its behalf could only guess;
    /// * a device that has answered recently is not lost, and
    ///   [`Registry::probed`] must not touch it: taking its resolution away
    ///   would drop a link that is happily streaming.
    #[must_use]
    pub fn unheard(&self, stale_after: Duration) -> Vec<String> {
        let now = unix_now();
        let limit = stale_after.as_secs();
        self.lock()
            .iter()
            .filter(|(id, d)| {
                !id.starts_with(PENDING) && d.udp_seen_unix.map_or(u64::MAX, |s| now.saturating_sub(s)) > limit
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// **A probe answered: follow any of `candidates` that has moved.**
    ///
    /// The reply carries the panel's own `id=`, and that is the whole of the
    /// matching rule:
    ///
    /// * *same id, new address* - the same panel, somewhere else. Its address
    ///   is updated by [`Registry::set_address`], so it keeps its id, its
    ///   player, its patch and its seed, and the next telemetry poll confirms
    ///   the new address with a `GET_INFO` of its own.
    /// * *an id nobody here knows* - not this studio's business. Discovery and
    ///   a human adding a device are how panels arrive; a probe only follows
    ///   ones already known.
    /// * *a different id at an address we hold* - not a move. The record that
    ///   holds that address is left exactly as it was, because nothing was
    ///   heard about *it*.
    ///
    /// A device reached **by name** is left alone as well, and that is the
    /// point of the card rather than an omission: a name is re-resolved on
    /// every reconnect and already follows a lease, and overwriting it with a
    /// literal address would take that away.
    ///
    /// `error` is the probe failing outright, kept for `/api/v1/status` and
    /// never a fault: a machine with no broadcast route still runs a studio.
    pub fn probed(&self, found: &[Device], candidates: &[String], error: Option<String>) -> Vec<Moved> {
        {
            let mut h = self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            h.probes += 1;
            h.last_probe_error = error;
        }
        let mut moves: Vec<Moved> = Vec::new();
        {
            let devices = self.lock();
            for dev in found {
                let Some(id) = dev.info.as_ref().map(|i| i.id.as_str()).filter(|i| !i.is_empty()) else { continue };
                if !candidates.iter().any(|c| c == id) {
                    continue;
                }
                let Some(record) = devices.get(id) else { continue };
                let at = record.stored.address.trim();
                // A name, not an address: it follows itself. See above.
                if !at.is_empty() && parse_addr(at).is_none() {
                    continue;
                }
                // Already where we think it is: the panel is alive and it is
                // the telemetry that is failing. Not a move, and re-setting
                // the address would throw away a resolution for nothing.
                if parse_addr(at) == Some(dev.frame) {
                    continue;
                }
                moves.push(Moved { id: id.to_string(), from: at.to_string(), to: dev.frame.to_string() });
            }
        }
        for m in &moves {
            self.set_address(&m.id, &m.to);
        }
        if !moves.is_empty() {
            let mut h = self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            h.moved += moves.len() as u64;
        }
        moves
    }
}

/// `IP`, `IP:PORT` or `[v6]:PORT`; anything else is a name.
#[must_use]
pub fn parse_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Some(a);
    }
    s.parse::<std::net::IpAddr>().ok().map(|ip| SocketAddr::new(ip, screeny::proto::DEFAULT_FRAME_PORT))
}

// ------------------------------------------------------------- the wire ----

/// One `GET_INFO` on a control address: who is this, really?
///
/// Blocking, and short - the client times out in 250 ms and tries three times.
/// Call it from `spawn_blocking`.
///
/// # Errors
///
/// If the device does not answer.
pub fn identify_at(frame: SocketAddr) -> Result<Device, String> {
    identify_at_counted(frame).1
}

/// [`identify_at`], saying what it cost on the control port (card 164).
///
/// The cost comes back whether or not the device answered: four datagrams at a
/// panel that is off is exactly the traffic the page is meant to be able to
/// account for.
pub fn identify_at_counted(frame: SocketAddr) -> (screeny::Traffic, Result<Device, String>) {
    let mut dev = Device::from_addr(frame);
    let mut client = match control(dev.control) {
        Ok(c) => c,
        Err(e) => return (screeny::Traffic::default(), Err(e)),
    };
    let info = client.info().map_err(|e| e.to_string());
    let cost = client.traffic();
    (cost, info.map(|i| { dev.apply(i); dev }))
}

/// **Run one control request and say what it cost** (card 164).
///
/// Every control conversation the studio has goes through here or through
/// [`identify_at_counted`], so "what has this panel cost me" has one answer
/// rather than one per caller. The cost is reported however the request went:
/// a request that timed out is four datagrams the network carried.
pub fn control_call<T>(addr: SocketAddr, f: impl FnOnce(&mut ControlClient) -> Result<T, String>) -> (screeny::Traffic, Result<T, String>) {
    let mut c = match control(addr) {
        Ok(c) => c,
        Err(e) => return (screeny::Traffic::default(), Err(e)),
    };
    let out = f(&mut c);
    (c.traffic(), out)
}

/// One browse of `_screeny._udp`.
///
/// # Errors
///
/// If mDNS itself fails. Finding nothing is `Ok(vec![])`, not an error.
pub fn browse(timeout: Duration) -> Result<Vec<Device>, String> {
    screeny::discover::browse(timeout, None).map_err(|e| e.to_string())
}

/// One `GET_INFO` probe (spec 5.5), asked at `to` - the subnet broadcast
/// addresses, or wherever `Config::probe_to` says.
///
/// Blocking and bounded: one datagram per destination, one window, whatever
/// answered. Finding nothing is `Ok(vec![])` - the normal case, because a
/// probe only happens when a panel is missing and a panel is usually missing
/// because it is switched off.
///
/// # Errors
///
/// If the socket cannot be set up - no broadcast route, or no permission.
pub fn probe(timeout: Duration, to: &[SocketAddr]) -> Result<Vec<Device>, String> {
    screeny::discover::probe(timeout, to).map_err(|e| e.to_string())
}

/// Resolve a DNS-SD instance name to a device.
///
/// # Errors
///
/// If nothing with that name answers in `timeout`.
pub fn resolve_name(name: &str, timeout: Duration) -> Result<Device, String> {
    Target { name: Some(name.to_string()), timeout: Some(timeout), ..Target::default() }
        .resolve()
        .map_err(|e| e.to_string())
}

/// A control client pointed at a device, with this crate's timeout.
///
/// # Errors
///
/// If the socket cannot be opened.
pub fn control(addr: SocketAddr) -> Result<ControlClient, String> {
    let mut c = ControlClient::connect(addr).map_err(|e| e.to_string())?;
    c.set_timeout(CONTROL_TIMEOUT);
    Ok(c)
}

/// The moment used for "how long ago", in the same units everywhere.
#[must_use]
pub fn since(then: Instant) -> f64 {
    then.elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use screeny::DeviceInfo;

    fn device(instance: &str, id: &str, addr: &str) -> Device {
        let mut d = Device::from_addr(addr.parse().expect("an address"));
        d.instance = instance.to_string();
        d.apply(DeviceInfo { id: id.to_string(), name: String::new(), ctrl: d.control.port(), ..info() });
        d
    }

    fn info() -> DeviceInfo {
        DeviceInfo {
            txtvers: 1,
            proto: "1".into(),
            w: 64,
            h: 32,
            codecs: vec![0x10],
            codecs_raw: "10".into(),
            mtu: 1464,
            ctrl: 49375,
            fw: "test".into(),
            id: String::new(),
            name: String::new(),
        }
    }

    /// What a conforming device answers, from the shared golden file.
    fn status() -> StatusReply {
        serde_json::from_str(include_str!("../../device-api/tests/golden/status.json")).expect("the golden status")
    }

    /// The two levels measured on the real device, and the heap line, read
    /// off one reply each. The numbers come from there; what is pinned here is
    /// which side of them a reading falls on.
    #[test]
    fn the_stack_has_two_levels_and_the_heap_has_one() {
        let facts = |stack: u32, used: u32, size: u32| {
            DeviceFacts::of(
                StatusReply { stack_free: stack, heap_used: used, heap_size: size, ..status() },
                None,
                &mut None,
            )
        };
        // The healthy panel: ~20 KB of stack, 51% of 90 KB.
        let good = facts(20_272, 45_612, 90_112);
        assert!(!good.stack_warn && !good.stack_fault && !good.low_heap);

        // The margin going, and gone.
        let warn = facts(6000, 45_612, 90_112);
        assert!(warn.stack_warn && !warn.stack_fault, "6000 B is a warning, not a fault");
        assert!(!warn.low_stack, "the old name is the fault level");
        let fault = facts(3000, 45_612, 90_112);
        assert!(fault.stack_fault && fault.stack_warn, "a fault is below the warning line too");
        assert!(fault.low_stack, "and the old name follows it");

        // Exactly on a line is not past it.
        assert!(!facts(STACK_WARN, 0, 1).stack_warn);
        assert!(!facts(STACK_FAULT, 0, 1).stack_fault);
        assert!(facts(STACK_FAULT - 1, 0, 1).stack_fault);

        // The heap: the worst instant measured on the device (60%) is
        // quiet; 90% is not.
        assert!(!facts(20_000, 54 * 1024, 90_112).low_heap);
        assert!(facts(20_000, 88_474, 98_304).low_heap);
        // A device that reports no heap at all is not a device in trouble.
        assert!(!facts(20_000, 0, 0).low_heap);
    }

    /// **One ask excuses one reboot**, and a reboot with no ask behind it is
    /// counted. The window is what an integration test cannot show without
    /// waiting two minutes, so it is shown here.
    #[test]
    fn an_ask_excuses_exactly_one_reboot() {
        let first = DeviceFacts::of(status(), None, &mut None);
        assert_eq!(first.reboots, 0, "the first read ever cannot be a reboot");
        assert_eq!(first.unasked_reboots, 0);

        // Rebooted, and we asked: counted as a reboot, not as a mystery, and
        // the ask is used up.
        let rebooted = StatusReply { boot_id: first.reply.boot_id + 1, reset_reason: ResetReason::Software, ..status() };
        let mut ask = Some(Instant::now());
        let second = DeviceFacts::of(rebooted.clone(), Some(&first), &mut ask);
        assert_eq!(second.reboots, 1);
        assert_eq!(second.unasked_reboots, 0, "this studio asked for it");
        assert!(ask.is_none(), "the ask is consumed, so it cannot excuse a second reboot");

        // Rebooted again, and nobody here asked.
        let again = StatusReply { boot_id: rebooted.boot_id + 1, reset_reason: ResetReason::Software, ..status() };
        let third = DeviceFacts::of(again, Some(&second), &mut ask);
        assert_eq!(third.reboots, 2);
        assert_eq!(third.unasked_reboots, 1, "the second reboot had no ask behind it");
        assert!(!third.odd_reset, "and it is still a quiet reset reason: the page says it gently");
    }

    /// An ask from long ago excuses nothing. It is taken all the same - it was
    /// about a reboot that has already happened or never will - so it cannot
    /// sit there waiting to excuse next month's crash.
    #[test]
    fn an_ask_older_than_the_window_does_not_excuse_a_reboot() {
        let first = DeviceFacts::of(status(), None, &mut None);
        let stale = Instant::now().checked_sub(REBOOT_ASK_WINDOW + Duration::from_secs(1));
        // A machine whose clock has not been up that long: nothing to test.
        let Some(stale) = stale else { return };
        let mut ask = Some(stale);
        let rebooted = StatusReply { boot_id: first.reply.boot_id + 1, reset_reason: ResetReason::Software, ..status() };
        let second = DeviceFacts::of(rebooted, Some(&first), &mut ask);
        assert_eq!(second.unasked_reboots, 1, "an ask from before the window is not an excuse");
        assert!(ask.is_none(), "and it is cleared rather than left lying around");
    }

    /// A reboot whose reason is *not* `software` is not one of these at all:
    /// a brownout is something that happened to the panel and already has a
    /// tone of its own.
    #[test]
    fn a_brownout_is_not_an_unasked_for_software_reboot() {
        let first = DeviceFacts::of(status(), None, &mut None);
        let rebooted = StatusReply { boot_id: first.reply.boot_id + 1, reset_reason: ResetReason::Brownout, ..status() };
        let second = DeviceFacts::of(rebooted, Some(&first), &mut None);
        assert_eq!(second.reboots, 1);
        assert_eq!(second.unasked_reboots, 0, "it is not a mystery: the panel says what happened");
        assert!(second.odd_reset, "and that stands out on its own");
    }

    /// The registry's half: the ask is recorded per device and read back by
    /// the next status read.
    #[test]
    fn the_registry_remembers_which_panel_was_asked_to_reboot() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let other = reg.add_manual("192.0.2.8", "other").expect("added");
        assert!(reg.asked_to_reboot(&id));
        assert!(!reg.asked_to_reboot("nobody"), "there is no such panel");
        assert!(reg.get(&id).expect("there").reboot_ask.is_some());
        assert!(reg.get(&other).expect("there").reboot_ask.is_none(), "one panel's ask is not another's");

        // One read, then a reboot: counted, and not counted as unasked.
        let reply = status();
        reg.heard_http(&id, reply.clone()).expect("recorded");
        let heard = reg
            .heard_http(&id, StatusReply { boot_id: reply.boot_id + 1, reset_reason: ResetReason::Software, ..reply })
            .expect("recorded");
        assert_eq!(heard.reboots, 1);
        assert!(heard.rebooted);
        assert_eq!(heard.unasked_reboots, 0, "we asked for this one");
        assert!(!heard.unasked);
        assert!(reg.get(&id).expect("there").reboot_ask.is_none(), "the ask was used up");
    }

    /// **Card 118: uptime that went backwards is the panel rebooting**, and
    /// that is a hand-off to the status poller: ask this one now, whatever the
    /// backoff said. Uptime that went *forwards* is a panel that is simply
    /// running, and says nothing.
    #[test]
    fn uptime_going_backwards_tells_the_status_poller_to_ask_again() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let telem = |uptime_ms: u32| Telemetry { uptime_ms, ..Telemetry::default() };

        // The first telemetry ever is not a reboot: there is nothing to have
        // gone backwards from.
        reg.heard(&id, &telem(90_000));
        assert!(!reg.take_http_retry(&id), "the first reading cannot be a reboot");

        // A panel that has simply been running.
        reg.heard(&id, &telem(120_000));
        assert!(!reg.take_http_retry(&id), "uptime going up is a panel that is up");

        // And one that came back.
        reg.heard(&id, &telem(2_000));
        assert!(reg.take_http_retry(&id), "90 s of uptime became 2 s: it rebooted");
        assert!(!reg.take_http_retry(&id), "taken, not read: one reboot is one extra poll");
        assert!(!reg.take_http_retry("nobody"), "there is no such panel");
    }

    // ------------------------------ card 164: what a panel costs the net ----

    fn wire(bytes: u64, packets: u64) -> screeny::Wire {
        screeny::Wire { bytes, packets }
    }

    /// The counter the frame path feeds: a link's lifetime figure, banked as a
    /// **difference**, and a link that was rebuilt - which counts from zero
    /// again - taken whole. The studio rebuilds a link whenever it re-aims it,
    /// so this is the ordinary case and not an edge.
    #[test]
    fn a_rebuilt_link_adds_to_the_total_instead_of_resetting_it() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));

        let link = |out: u64, packets: u64| screeny::LinkTraffic {
            frames: screeny::Traffic { out: wire(out, packets), inbound: wire(packets * 20, packets) },
            control: screeny::Traffic { out: wire(24, 1), inbound: wire(96, 1) },
        };
        reg.metered_link(&id, link(1000, 10));
        reg.metered_link(&id, link(3000, 30));
        let t = reg.traffic(&id);
        assert_eq!(t.frames.out.bytes, 3000, "a difference, not a sum of snapshots");
        assert_eq!(t.frames.out.packets, 30);
        assert_eq!(t.control.out.packets, 1, "one handshake, seen twice, is one handshake");

        // The link is thrown away and built again: its counters start at zero,
        // and the studio's must not.
        reg.metered_link(&id, link(200, 2));
        let t = reg.traffic(&id);
        assert_eq!(t.frames.out.bytes, 3200, "the new link's bytes are added whole");
        assert_eq!(t.frames.out.packets, 32);
        assert_eq!(t.control.out.packets, 2, "and its handshake is a second one");

        // And it never goes backwards, whatever it is handed.
        reg.metered_link(&id, screeny::LinkTraffic::default());
        assert_eq!(reg.traffic(&id).frames.out.packets, 32);
    }

    /// The other two paths, and the overhead rule: 28 B a datagram for the two
    /// UDP paths, **nothing** for HTTP, where user space cannot see the
    /// segments.
    #[test]
    fn the_total_is_the_payload_plus_the_overhead_only_where_there_is_one() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));

        reg.metered_link(
            &id,
            screeny::LinkTraffic {
                frames: screeny::Traffic { out: wire(1000, 10), inbound: wire(40, 2) },
                ..screeny::LinkTraffic::default()
            },
        );
        reg.metered_control(&id, screeny::Traffic { out: wire(24, 4), inbound: wire(0, 0) });
        reg.metered_http(&id, 130, 400);
        reg.sample_traffic();

        let t = reg.traffic(&id);
        assert_eq!(t.http.out, Flow { bytes: 130, packets: 1 }, "one request");
        assert_eq!(t.http.inbound, Flow { bytes: 400, packets: 1 }, "one reply");
        assert_eq!(
            t.total.out.bytes,
            1000 + 28 * 10 + 24 + 28 * 4 + 130,
            "UDP pays 28 B a datagram and HTTP pays nothing"
        );
        assert_eq!(t.total.out.packets, 10 + 4 + 1);
        assert_eq!(t.total.inbound.bytes, 40 + 28 * 2 + 400);
        assert!((t.window_s - TRAFFIC_WINDOW.as_secs_f64()).abs() < f64::EPSILON);

        // A read that produced nothing in one direction does not invent a
        // packet in it.
        reg.metered_http(&id, 130, 0);
        assert_eq!(reg.traffic(&id).http.inbound.packets, 1);
    }

    /// **The rate, on a synthetic clock**: the first sample sets a baseline
    /// rather than inventing a spike, a steady stream settles on the figure
    /// the arithmetic says it should, and a panel that stops being sent
    /// anything **decays to nothing** rather than freezing at what it was last
    /// doing.
    ///
    /// 30 datagrams a second of 1000 payload bytes is 30 KB/s of payload and
    /// 30.84 KB/s on the wire, which is what the page has to say.
    #[test]
    fn the_rate_settles_on_the_arithmetic_and_decays_back_to_nothing() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let start = Instant::now();
        let mut sent = 0u64;
        let mut packets = 0u64;
        let mut stream = |secs: u64| {
            sent += 30_000;
            packets += 30;
            reg.metered_link(
                &id,
                screeny::LinkTraffic {
                    frames: screeny::Traffic { out: wire(sent, packets), inbound: wire(0, 0) },
                    ..screeny::LinkTraffic::default()
                },
            );
            reg.sample_traffic_at(start + Duration::from_secs(secs));
        };

        stream(1);
        assert_eq!(reg.traffic(&id).rate.out, 0.0, "the first pass has no interval to divide by");
        for s in 2..=60 {
            stream(s);
        }

        let want = 30_000.0 + 28.0 * 30.0; // payload plus the overhead rule
        let busy = reg.traffic(&id).rate.out;
        assert!(
            (busy - want).abs() < want * 0.01,
            "a steady 30 fps at 1000 B should read {want:.0} B/s, not {busy:.0}"
        );
        assert!(
            (reg.traffic(&id).rate.frames_out - busy).abs() < f64::EPSILON,
            "with only the frame path in play, the total is the frame path"
        );

        // Unplugged. Nothing is added; the figure has to fall away rather than
        // sit at 30 KB/s for ever.
        for s in 61..=120 {
            reg.sample_traffic_at(start + Duration::from_secs(s));
        }
        let quiet = reg.traffic(&id).rate.out;
        assert!(quiet < busy / 1000.0, "a panel nobody is sending to: {busy:.0} -> {quiet:.3}");
        assert_eq!(reg.traffic(&id).frames.out.packets, packets, "and the total is untouched");
    }

    /// A device the studio has never counted anything for reads as zero rather
    /// than as missing, and one that does not exist does not panic.
    #[test]
    fn a_panel_that_has_cost_nothing_says_so() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        reg.sample_traffic();
        let t = reg.traffic(&id);
        assert_eq!(t.total.out.bytes, 0);
        assert_eq!(t.rate.out, 0.0);
        reg.metered_control("nobody", screeny::Traffic { out: wire(24, 1), inbound: wire(0, 0) });
        reg.metered_http("nobody", 1, 1);
        reg.metered_link("nobody", screeny::LinkTraffic::default());
        assert_eq!(reg.traffic("nobody").total.out.bytes, 0);
    }

    #[test]
    fn a_manual_address_gets_a_provisional_id_and_adopts_its_real_one() {
        let reg = Registry::new();
        let id = reg.add_manual("192.0.2.7:49374", "desk").expect("added");
        assert!(id.starts_with(PENDING), "{id}");
        assert_eq!(reg.list().len(), 1);

        // The device answers: the record keeps its name and its manual flag,
        // and its key becomes the device's own id.
        let (new_id, renamed) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        assert_eq!(new_id, "abc123");
        assert_eq!(renamed.as_deref(), Some(id.as_str()));
        assert_eq!(reg.list().len(), 1, "adopting an id must not make a second device");
        let d = reg.get("abc123").expect("still there");
        assert_eq!(d.stored.name, "desk");
        assert!(d.stored.manual);
        assert_eq!(d.stored.instance, "screeny-abc");
        assert_eq!(d.label(), "desk");
    }

    /// The property the whole registry is for: a device that moves address is
    /// the same device, with the same player.
    #[test]
    fn a_device_that_changes_address_keeps_its_id() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let (again, renamed) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.99:49374"));
        assert_eq!(id, again);
        assert!(renamed.is_none());
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.get("abc123").expect("there").resolved.expect("resolved").frame.to_string(), "192.0.2.99:49374");
    }

    // ------------------------------------------- card 141: it has moved ----

    /// A device as the state file hands it back after a restart: its real id,
    /// an address from last time, and nothing heard yet.
    fn stored_at(id: &str, address: &str) -> StoredDevice {
        StoredDevice {
            id: id.to_string(),
            name: "desk".into(),
            instance: String::new(),
            address: address.to_string(),
            manual: true,
        }
    }

    /// Who a probe is for: a device with a real id that has not been heard
    /// from. Not a pending one - there is no id to match a reply against - and
    /// not one that answered a moment ago.
    #[test]
    fn a_probe_is_only_for_devices_that_have_gone_quiet() {
        let reg = Registry::new();
        reg.load(vec![stored_at("abc123", "192.0.2.7:49374")]);
        reg.add_manual("192.0.2.8", "typed").expect("added");
        reg.resolved(&device("screeny-live", "live01", "192.0.2.9:49374"));

        let unheard = reg.unheard(Duration::from_secs(120));
        assert_eq!(unheard, vec!["abc123".to_string()], "{unheard:?}");
        // And a studio whose device has just answered probes for nobody.
        let reg2 = Registry::new();
        reg2.resolved(&device("screeny-live", "live01", "192.0.2.9:49374"));
        assert!(reg2.unheard(Duration::from_secs(120)).is_empty());
    }

    /// **The card**: a panel that answers a probe from somewhere else is the
    /// same panel. Its id, and so its player, survives; the address is the
    /// only thing that changes; and the HTTP polling follows the new IP by
    /// itself once the next poll has confirmed it.
    #[test]
    fn a_probe_follows_a_device_that_answers_from_a_new_address() {
        let reg = Registry::new();
        reg.load(vec![stored_at("abc123", "192.0.2.7:49374")]);
        let before = reg.get("abc123").expect("loaded");
        assert_eq!(before.http_addr(80), None, "nothing is resolved yet");

        let candidates = reg.unheard(Duration::from_secs(120));
        let moves = reg.probed(&[device("screeny-abc", "abc123", "192.0.2.99:49374")], &candidates, None);
        assert_eq!(moves.len(), 1, "{moves:?}");
        assert_eq!(moves[0].id, "abc123");
        assert_eq!(moves[0].from, "192.0.2.7:49374");
        assert_eq!(moves[0].to, "192.0.2.99:49374");

        let d = reg.get("abc123").expect("still the same device");
        assert_eq!(reg.list().len(), 1, "a move must never make a second device");
        assert_eq!(d.stored.address, "192.0.2.99:49374");
        assert_eq!(d.stored.name, "desk", "and it is still the panel it was");
        assert!(d.resolved.is_none(), "the next poll asks the new address who it is");
        assert!(matches!(d.reach(), Reach::Addr(a) if a.to_string() == "192.0.2.99:49374"));

        // That poll: the device answers, and the HTTP address - which is
        // derived from the frame address's IP - is now the new one, with
        // nobody having said anything about HTTP at all.
        reg.resolved(&device("screeny-abc", "abc123", "192.0.2.99:49374"));
        let d = reg.get("abc123").expect("there");
        assert_eq!(d.http_addr(80).map(|a| a.to_string()).as_deref(), Some("192.0.2.99:80"));
        assert_eq!(reg.discovery_health().moved, 1);
        assert_eq!(reg.discovery_health().probes, 1);
    }

    /// Everything a probe must *not* do. Each of these was a way of losing a
    /// device or a link.
    #[test]
    fn a_probe_leaves_alone_everything_it_was_not_about() {
        let reg = Registry::new();
        reg.load(vec![stored_at("abc123", "192.0.2.7:49374"), stored_at("byname", "screeny-byname")]);
        reg.resolved(&device("screeny-live", "live01", "192.0.2.9:49374"));
        let candidates = reg.unheard(Duration::from_secs(120));

        let moves = reg.probed(
            &[
                // A device this studio has never heard of, at an address it
                // has never heard of.
                device("screeny-new", "new999", "192.0.2.50:49374"),
                // A *different* device answering at an address we hold for
                // `abc123`. Not a move: nothing was heard about `abc123`.
                device("screeny-other", "oth777", "192.0.2.7:49374"),
                // A device known by name. It re-resolves its name on every
                // reconnect and must not be pinned to an address.
                device("screeny-byname", "byname", "192.0.2.60:49374"),
                // A device that is streaming happily and simply also
                // answered. Taking its resolution away would drop its link.
                device("screeny-live", "live01", "192.0.2.80:49374"),
            ],
            &candidates,
            None,
        );
        assert!(moves.is_empty(), "{moves:?}");
        assert_eq!(reg.list().len(), 3, "a probe never adds a device");
        assert_eq!(reg.get("abc123").expect("there").stored.address, "192.0.2.7:49374");
        assert_eq!(reg.get("byname").expect("there").stored.address, "screeny-byname");
        let live = reg.get("live01").expect("there");
        assert!(live.resolved.is_some(), "the streaming device kept its resolution");
        assert_eq!(live.resolved.expect("resolved").frame.to_string(), "192.0.2.9:49374");
        assert_eq!(reg.discovery_health().moved, 0);

        // The panel answering from the address we already have is not a move
        // either: it is alive and something else is wrong.
        let same = reg.probed(&[device("screeny-abc", "abc123", "192.0.2.7:49374")], &candidates, None);
        assert!(same.is_empty(), "{same:?}");
    }

    #[test]
    fn a_device_with_no_id_is_keyed_by_its_instance_name() {
        let reg = Registry::new();
        let mut d = Device::from_addr("192.0.2.7:49374".parse().expect("addr"));
        d.instance = "screeny-noid".into();
        let (id, _) = reg.resolved(&d);
        assert_eq!(id, format!("{PENDING}:screeny-noid"));
    }

    #[test]
    fn reach_prefers_a_resolution_then_what_was_typed() {
        let reg = Registry::new();
        reg.add_manual("screeny-abc", "").expect("added");
        let rec = reg.list().remove(0);
        assert!(matches!(rec.reach(), Reach::Name(n) if n == "screeny-abc"));

        reg.add_manual("192.0.2.7", "").expect("added");
        let rec = reg.get(&format!("{PENDING}:192.0.2.7")).expect("there");
        assert!(matches!(rec.reach(), Reach::Addr(a) if a.to_string() == "192.0.2.7:49374"));

        reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let rec = reg.get("abc123").expect("there");
        assert!(matches!(rec.reach(), Reach::Resolved(_)));
    }

    #[test]
    fn adding_the_same_address_twice_is_one_device() {
        let reg = Registry::new();
        let a = reg.add_manual("192.0.2.7", "one").expect("added");
        let b = reg.add_manual("192.0.2.7", "two").expect("added");
        assert_eq!(a, b);
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.get(&a).expect("there").stored.name, "two");
        assert!(reg.add_manual("   ", "").is_err());
    }

    #[test]
    fn what_is_stored_survives_a_round_trip() {
        let reg = Registry::new();
        reg.add_manual("192.0.2.7", "desk").expect("added");
        reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let stored = reg.stored();
        assert_eq!(stored.len(), 1);

        let back = Registry::new();
        back.load(stored);
        let d = back.get("abc123").expect("loaded");
        assert_eq!(d.stored.name, "desk");
        assert_eq!(d.stored.instance, "screeny-abc");
        // A resolution is deliberately not persisted: an address from last
        // month is worse than no address at all.
        assert!(d.resolved.is_none());
        assert!(matches!(d.reach(), Reach::Addr(_)));
    }
}
