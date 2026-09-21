//! Sockets, threads, and the handle an integration test drives.
//!
//! Three threads and no async runtime:
//!
//! * **frames** - drains the frame socket, hands each datagram to
//!   [`Core::offer_frame`], closes the batch with [`Core::flush_frames`], and
//!   sends whatever the core asked for. Spec section 3.3's "drain until it
//!   would block, newest wins" is this loop.
//! * **control** - the same shape for the control socket.
//! * **tick** - 10 ms heartbeat so `STREAM_TIMEOUT_MS`, `HOLD_MS` and
//!   `IDENTIFY` expire without a packet having to arrive.
//!
//! All three share one `Mutex<Core>`, which makes the device's behaviour
//! serialisable and means a test never sees a half-applied transition.

use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use screeny_proto::control::{IdleMode, Telemetry};
use screeny_proto::{Rgb888Frame, MAX_UDP_PAYLOAD, NBYTES};

use crate::config::{Config, Faults};
use crate::core::{Core, FrameMeta, Outbox};
use crate::event::{Event, State};
use crate::mdns::Advertisement;
use crate::screens::{self, Scene};

/// Called on the frame thread for every frame that reaches the panel.
///
/// This is how `--dump-dir` writes *exactly* every Nth displayed frame rather
/// than approximately.
pub type FrameSink = Box<dyn FnMut(&Rgb888Frame, &FrameMeta) + Send>;

/// How long a socket read waits before the loop checks whether it should stop.
const POLL: Duration = Duration::from_millis(1);
/// Cap on one drain, so a flood cannot starve the stop flag.
const MAX_DRAIN: usize = 256;
/// Events kept for `wait_for_event`.
const EVENT_LOG_CAP: usize = 8192;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct Bus {
    log: Mutex<(VecDeque<Event>, usize)>,
    cv: Condvar,
}

impl Bus {
    fn new() -> Self {
        Bus {
            log: Mutex::new((VecDeque::new(), 0)),
            cv: Condvar::new(),
        }
    }

    fn publish(&self, events: &[Event]) {
        if events.is_empty() {
            return;
        }
        let mut g = self.log.lock().unwrap();
        for e in events {
            g.0.push_back(e.clone());
        }
        while g.0.len() > EVENT_LOG_CAP {
            g.0.pop_front();
            g.1 += 1;
        }
        drop(g);
        self.cv.notify_all();
    }

    /// Absolute index one past the newest event.
    fn cursor(&self) -> usize {
        let g = self.log.lock().unwrap();
        g.1 + g.0.len()
    }

    fn since(&self, from: usize) -> (usize, Vec<Event>) {
        let g = self.log.lock().unwrap();
        let start = from.max(g.1) - g.1;
        let out: Vec<Event> = g.0.iter().skip(start).cloned().collect();
        (g.1 + g.0.len(), out)
    }
}

/// Everything the threads and the HTTP routes share.
///
/// `pub(crate)` rather than private since card 224: `crate::api` serves the
/// device's HTTP API and needs the same `Core` under the same lock, because a
/// second lock or a second copy of the state is exactly how a browser and a
/// UDP sender start disagreeing.
pub(crate) struct Shared {
    core: Mutex<Core>,
    bus: Bus,
    faults: Mutex<Faults>,
    stop: AtomicBool,
    boot: Instant,
    frame_addr: SocketAddr,
    control_addr: SocketAddr,
    http_addr: Mutex<Option<SocketAddr>>,
    display_addr: Ipv4Addr,
    instance: String,
    fade_ms: u32,
    panel_model: crate::config::PanelModel,
    rssi_dbm: i8,
}

impl Shared {
    pub(crate) fn now_us(&self) -> u64 {
        self.boot.elapsed().as_micros() as u64
    }

    /// The device under its one lock.
    pub(crate) fn core(&self) -> &Mutex<Core> {
        &self.core
    }

    /// Put events on the bus a test watches.
    pub(crate) fn publish(&self, events: &[Event]) {
        self.bus.publish(events);
    }

    /// The mDNS instance name, which is one of the names the captive-portal
    /// `Host` rule accepts as this device's own.
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// A consistent read of everything the device is showing and counting.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The last frame that decoded, byte for byte as the sender sent it.
    /// This is what a test asserts on.
    pub decoded: Box<Rgb888Frame>,
    /// The same frame through the panel model and the brightness LUT.
    pub panel: Box<Rgb888Frame>,
    /// Which frame that was, or `None` if none has ever been displayed.
    pub shown: Option<FrameMeta>,
    /// The stream state of section 7.3.
    pub state: State,
    /// The telemetry `state` byte, which reports an overlay if one is up.
    pub state_byte: u8,
    /// The source holding the lock.
    pub active_source: Option<SocketAddr>,
    /// The counters, exactly as a `TELEMETRY` reply would carry them.
    pub telemetry: Telemetry,
    /// Brightness in effect.
    pub brightness: u8,
    /// Idle behaviour in force.
    pub idle_mode: IdleMode,
    /// Friendly name.
    pub name: String,
    /// Where the provisioning machine is (card 224).
    pub wifi_phase: crate::WifiPhase,
    /// The `GET_WIFI` join-state byte, from the same machine.
    pub wifi_state: u8,
    /// The SSID `GET_WIFI` reports. Empty when there is none. **Never a
    /// PSK**: the simulator holds none.
    pub ssid: String,
    /// Whether the captive portal's soft-AP is up.
    pub portal: bool,
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// A cheap, cloneable, thread-safe view of a running device.
///
/// This is the whole of the API an integration test needs: bind a device,
/// take its handle, send it packets, and assert on [`SimHandle::snapshot`],
/// [`SimHandle::telemetry`] and the [`Event`] stream.
#[derive(Clone)]
pub struct SimHandle {
    shared: Arc<Shared>,
}

impl SimHandle {
    /// The address the frame socket is actually bound to. With
    /// [`Config::frame_port`] 0 this is where you find out the port.
    #[must_use]
    pub fn frame_addr(&self) -> SocketAddr {
        self.shared.frame_addr
    }

    /// The address the control socket is actually bound to.
    #[must_use]
    pub fn control_addr(&self) -> SocketAddr {
        self.shared.control_addr
    }

    /// The address the HTTP server is bound to, or `None` when
    /// [`Config::http`](crate::Config::http) is off. With
    /// [`Config::http_port`](crate::Config::http_port) 0 this is where you
    /// find out the port.
    #[must_use]
    pub fn http_addr(&self) -> Option<SocketAddr> {
        *self.shared.http_addr.lock().unwrap()
    }

    /// The base URL of the HTTP API, ready to append a route to.
    #[must_use]
    pub fn http_url(&self) -> Option<String> {
        self.http_addr().map(|a| format!("http://{a}"))
    }

    /// Choose what the scripted radio does with the **next** join attempt.
    ///
    /// Card 081's `--wifi-result`, per request. An attempt already in flight
    /// keeps the outcome it started with only until the next tick, which is
    /// what a test that wants to change its mind mid-join expects.
    pub fn set_wifi_outcome(&self, outcome: crate::WifiOutcome) {
        self.shared
            .core
            .lock()
            .unwrap()
            .wifi_mut()
            .set_outcome(outcome);
    }

    /// Take the WiFi link down, or bring it back.
    ///
    /// Spec section 7.3's "any -> WiFi link down -> `HOLD`" and research 007
    /// section 5.2's `Online -> Joining` after `link_down_ms`, from one call,
    /// so the stream state machine and the provisioning machine cannot be
    /// given half the news.
    pub fn set_link_down(&self, down: bool) {
        let now = self.shared.now_us();
        let mut out = Outbox::default();
        self.shared
            .core
            .lock()
            .unwrap()
            .set_link_down(down, now, &mut out);
        self.shared.bus.publish(&out.events);
        out.clear();
    }

    /// Post credentials the way `POST /api/v1/wifi` does, without an HTTP
    /// request. The PSK is not a parameter, because nothing here keeps one.
    pub fn post_wifi(&self, ssid: &str) -> crate::Posted {
        let now = self.shared.now_us();
        let mut events = Vec::new();
        let posted = self
            .shared
            .core
            .lock()
            .unwrap()
            .wifi_mut()
            .post_credentials(ssid, now, &mut events);
        self.shared.bus.publish(&events);
        posted
    }

    /// The button's five-second hold: forget the network, raise the portal.
    pub fn wifi_wipe(&self) {
        let now = self.shared.now_us();
        let mut events = Vec::new();
        self.shared
            .core
            .lock()
            .unwrap()
            .wifi_mut()
            .wipe(now, &mut events);
        self.shared.bus.publish(&events);
    }

    /// Press the button on the back of the device and hold it for `hold_ms`
    /// (card 230).
    ///
    /// The whole gesture, through the recogniser the firmware runs: under a
    /// second is a short press and raises the status overlay for ten seconds,
    /// past one second the countdown appears on the panel, letting go before
    /// five seconds changes nothing, and five seconds forgets the network and
    /// raises the setup portal.
    ///
    /// **It returns immediately.** The hold is played out against the
    /// recogniser's own clock, so `press_button(5_000)` costs a test
    /// microseconds rather than five seconds; see [`crate::button`].
    pub fn press_button(&self, hold_ms: u32) {
        let now = self.shared.now_us();
        let mut out = Outbox::default();
        self.shared
            .core
            .lock()
            .unwrap()
            .press_button(hold_ms, now, &mut out);
        self.shared.bus.publish(&out.events);
        out.clear();
    }

    /// Tell the machine a phone did or did not join the soft-AP, which is
    /// what gates the portal's ten-minute retry.
    pub fn set_ap_client(&self, present: bool) {
        let now = self.shared.now_us();
        let mut events = Vec::new();
        self.shared
            .core
            .lock()
            .unwrap()
            .wifi_mut()
            .set_ap_client(present, now, &mut events);
        self.shared.bus.publish(&events);
    }

    /// The random number drawn at boot, and again at every reboot. A client
    /// that sees a new one knows the device restarted rather than that the
    /// link flapped.
    #[must_use]
    pub fn boot_id(&self) -> u32 {
        self.shared.core.lock().unwrap().ident().boot_id
    }

    /// Where the provisioning machine is.
    #[must_use]
    pub fn wifi_phase(&self) -> crate::WifiPhase {
        self.shared.core.lock().unwrap().wifi().phase()
    }

    /// The SSID the store holds, if any. There is no method for the PSK,
    /// because the simulator never keeps one (spec section 8.4).
    #[must_use]
    pub fn stored_ssid(&self) -> Option<String> {
        self.shared
            .core
            .lock()
            .unwrap()
            .wifi()
            .stored_ssid()
            .map(str::to_string)
    }

    /// How many times the settings store has been written since boot. A
    /// failed trial join must not move this.
    #[must_use]
    pub fn wifi_commits(&self) -> u32 {
        self.shared.core.lock().unwrap().wifi().commits()
    }

    /// `GET /api/v1/wifi`'s body, without an HTTP request.
    #[must_use]
    pub fn wifi_reply(&self) -> screeny_device_api::reply::WifiReply {
        self.shared.core.lock().unwrap().wifi().wifi_reply()
    }

    /// Microseconds since the device started. The same clock the state
    /// machine and `uptime_ms` use.
    #[must_use]
    pub fn now_us(&self) -> u64 {
        self.shared.now_us()
    }

    /// Read everything at once, under one lock.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let now = self.shared.now_us();
        let c = self.shared.core.lock().unwrap();
        Snapshot {
            decoded: Box::new(*c.decoded()),
            panel: Box::new(*c.panel()),
            shown: c.shown(),
            state: c.state(),
            state_byte: c.state_byte(),
            active_source: c.active_source(),
            telemetry: c.telemetry(now),
            brightness: c.brightness(),
            idle_mode: c.idle_mode(),
            name: c.name().to_string(),
            wifi_phase: c.wifi().phase(),
            wifi_state: c.wifi().wifi_state(),
            ssid: c.wifi().ssid().to_string(),
            portal: c.wifi().ap_up(),
        }
    }

    /// The counters alone, if a whole snapshot is more than you wanted.
    #[must_use]
    pub fn telemetry(&self) -> Telemetry {
        let now = self.shared.now_us();
        self.shared.core.lock().unwrap().telemetry(now)
    }

    /// The `GET_INFO` reply body / mDNS TXT record bytes (spec section 6.6).
    #[must_use]
    pub fn info_bytes(&self) -> Vec<u8> {
        self.shared.core.lock().unwrap().info_bytes().to_vec()
    }

    /// Compose what the panel is scanning out right now: the stream frame,
    /// the idle screen, the cross-fade between them, and any overlay.
    #[must_use]
    pub fn render_display(&self) -> Box<Rgb888Frame> {
        let now = self.shared.now_us();
        let c = self.shared.core.lock().unwrap();
        let mut out = Box::new([0u8; NBYTES]);
        let scene = Scene {
            decoded: c.decoded(),
            panel: c.panel(),
            have_frame: c.shown().is_some(),
            state: c.state(),
            idle_mode: c.idle_mode(),
            now_us: now,
            idle_since_us: c.idle_since_us(),
            fade_ms: self.shared.fade_ms,
            identify: c.identify_until_us().is_some(),
            name: c.name(),
            addr: c.wifi().ip().map_or(self.shared.display_addr, Ipv4Addr::from),
            rssi_dbm: self.shared.rssi_dbm,
            brightness: c.brightness(),
            model: self.shared.panel_model,
            // Card 224: in `Portal` and `Trial` the panel is the provisioning
            // crate's, so the window and the `--dump-dir` PNGs show what the
            // device will show, pixel for pixel.
            //
            // Card 230: and the button's screens outrank the portal's, exactly
            // as they do in `firmware/src/net.rs` - somebody holding the
            // button while the portal is up still has to see what is about to
            // happen.
            provisioning: c.button_screen(now).or_else(|| c.wifi().screen(now)),
            network_down: !c.wifi().online(),
        };
        screens::render(&scene, &mut out);
        out
    }

    /// Change the injected faults while the device is running.
    pub fn set_faults(&self, faults: Faults) {
        self.shared
            .core
            .lock()
            .unwrap()
            .set_extra_decode_us(faults.decode_ms.saturating_mul(1_000));
        *self.shared.faults.lock().unwrap() = faults;
    }

    /// The faults in force.
    #[must_use]
    pub fn faults(&self) -> Faults {
        *self.shared.faults.lock().unwrap()
    }

    /// Change what `GET /api/v1/status` says about the device's health while
    /// it is running: the reset reason, the firmware slot and its state, the
    /// store errors and the three memory numbers (card 192).
    ///
    /// **Nothing else changes.** A `pending_verify` slot does not alter any
    /// behaviour and a `brownout` reason reboots nothing; these are the rows
    /// of a status page, and this is how they are made to appear.
    ///
    /// A simulated `REBOOT` after this call sets the reason back to
    /// [`ResetReason::Software`](screeny_device_api::ResetReason::Software),
    /// as a device's does. Calling this *after* the reboot pins it again.
    pub fn set_health(&self, health: crate::Health) {
        self.shared.core.lock().unwrap().set_health(health);
    }

    /// What the device is currently claiming about its health.
    #[must_use]
    pub fn health(&self) -> crate::Health {
        self.shared.core.lock().unwrap().health()
    }

    /// An index one past the newest event. Pass it to [`SimHandle::events`]
    /// or [`SimHandle::wait_for`] to read only what happens next.
    #[must_use]
    pub fn event_cursor(&self) -> usize {
        self.shared.bus.cursor()
    }

    /// Every event since `from`, and the new cursor.
    #[must_use]
    pub fn events(&self, from: usize) -> (usize, Vec<Event>) {
        self.shared.bus.since(from)
    }

    /// Block until an event after `from` satisfies `pred`, or the timeout
    /// expires. Returns the matching event and advances `from` past it.
    pub fn wait_for<F>(&self, from: &mut usize, timeout: Duration, pred: F) -> Option<Event>
    where
        F: Fn(&Event) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let (next, events) = self.shared.bus.since(*from);
            for (i, e) in events.iter().enumerate() {
                if pred(e) {
                    *from += i + 1;
                    return Some(e.clone());
                }
            }
            *from = next;
            let left = deadline.checked_duration_since(Instant::now())?;
            let g = self.shared.bus.log.lock().unwrap();
            let _ = self
                .shared
                .bus
                .cv
                .wait_timeout(g, left.min(Duration::from_millis(20)))
                .unwrap();
        }
    }

    /// Block until at least `n` frames have been displayed.
    ///
    /// Returns the snapshot taken once the count was reached, or `None` on
    /// timeout. The commonest thing a test wants.
    pub fn wait_for_frames(&self, n: u32, timeout: Duration) -> Option<Snapshot> {
        self.wait_until(timeout, |s| s.telemetry.frames_shown >= n)
    }

    /// Block until `pred` holds of a snapshot, polling every millisecond.
    pub fn wait_until<F>(&self, timeout: Duration, pred: F) -> Option<Snapshot>
    where
        F: Fn(&Snapshot) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.snapshot();
            if pred(&s) {
                return Some(s);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

/// A running fake panel.
///
/// Dropping it, or calling [`SimDevice::shutdown`], stops the threads, closes
/// the sockets and withdraws the mDNS advertisement.
pub struct SimDevice {
    handle: SimHandle,
    threads: Vec<JoinHandle<()>>,
    mdns: Option<Advertisement>,
    http: Option<crate::http::Server>,
}

impl SimDevice {
    /// Bind the sockets and start the device.
    ///
    /// # Errors
    ///
    /// Anything that stops the two sockets binding, and
    /// [`io::ErrorKind::InvalidInput`] if the mDNS instance name is
    /// `screeny` - that is the real device's name, and two responders
    /// claiming it would be worse than useless.
    pub fn start(cfg: Config) -> io::Result<Self> {
        Self::start_with(cfg, None)
    }

    /// As [`SimDevice::start`], with a callback for every displayed frame.
    ///
    /// # Errors
    ///
    /// See [`SimDevice::start`].
    pub fn start_with(cfg: Config, mut sink: Option<FrameSink>) -> io::Result<Self> {
        if cfg.mdns && crate::instance_is_reserved(&cfg.instance) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "mDNS instance name {:?} is reserved for the real device; use {:?}",
                    cfg.instance,
                    crate::DEFAULT_INSTANCE
                ),
            ));
        }

        let frame_sock = UdpSocket::bind((cfg.bind, cfg.frame_port))?;
        let control_sock = UdpSocket::bind((cfg.bind, cfg.control_port))?;
        frame_sock.set_read_timeout(Some(POLL))?;
        control_sock.set_read_timeout(Some(POLL))?;
        // Section 5.5: the device answers a GET_INFO sent to the subnet
        // broadcast address, so the socket has to be willing to receive one.
        let _ = control_sock.set_broadcast(true);
        let frame_addr = frame_sock.local_addr()?;
        let control_addr = control_sock.local_addr()?;

        let mut boot_events = Vec::new();
        let mut core = Core::new_with(&cfg, &mut boot_events);
        // The TXT record must advertise the port a sender can actually reach,
        // which with `control_port: 0` is only known now.
        core.set_control_port(control_addr.port());

        let display_addr = crate::net::display_addr(cfg.bind);

        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            bus: Bus::new(),
            faults: Mutex::new(cfg.faults),
            stop: AtomicBool::new(false),
            boot: Instant::now(),
            frame_addr,
            control_addr,
            http_addr: Mutex::new(None),
            display_addr,
            instance: cfg.instance.clone(),
            fade_ms: cfg.timing.fade_ms,
            panel_model: cfg.panel,
            rssi_dbm: cfg.rssi_dbm,
        });
        shared.bus.publish(&boot_events);

        // The HTTP server, if the configuration asks for it. It binds the same
        // address the sockets do, so `--bind 127.0.0.1` keeps everything on
        // loopback and a test cannot accidentally serve the LAN.
        let http = if cfg.http {
            let bind = SocketAddr::new(cfg.bind, cfg.http_port);
            let handler = crate::api::handler(Arc::clone(&shared));
            let wants_stream = crate::api::wants_stream();
            let server = match crate::http::Server::start(bind, handler, wants_stream) {
                Ok(s) => s,
                // The default port is a *preference*: several sessions run two
                // or three simulators at once and never asked for HTTP at all,
                // and a hard failure on a port they did not choose is a
                // regression for them. A port somebody named is different -
                // they are going to connect to it - so that one still fails.
                Err(e)
                    if e.kind() == std::io::ErrorKind::AddrInUse
                        && !cfg.http_port_explicit
                        && cfg.http_port != 0 =>
                {
                    eprintln!(
                        "screeny-sim: HTTP port {} is busy; using an ephemeral one instead",
                        cfg.http_port
                    );
                    crate::http::Server::start(
                        SocketAddr::new(cfg.bind, 0),
                        crate::api::handler(Arc::clone(&shared)),
                        crate::api::wants_stream(),
                    )?
                }
                Err(e) => return Err(e),
            };
            *shared.http_addr.lock().unwrap() = Some(server.addr());
            Some(server)
        } else {
            None
        };

        let mdns = if cfg.mdns {
            Some(Advertisement::start(
                &cfg,
                frame_addr.port(),
                shared.core.lock().unwrap().info_bytes(),
                display_addr,
            )?)
        } else {
            None
        };

        let frame_sock = Arc::new(frame_sock);
        let control_sock = Arc::new(control_sock);
        let seed = cfg.fault_seed;
        let mut threads = Vec::new();

        {
            let shared = Arc::clone(&shared);
            let fs = Arc::clone(&frame_sock);
            let cs = Arc::clone(&control_sock);
            threads.push(
                thread::Builder::new()
                    .name("sim-frames".into())
                    .spawn(move || frame_loop(&shared, &fs, &cs, &mut sink, seed))?,
            );
        }
        {
            let shared = Arc::clone(&shared);
            let fs = Arc::clone(&frame_sock);
            let cs = Arc::clone(&control_sock);
            threads.push(
                thread::Builder::new()
                    .name("sim-control".into())
                    .spawn(move || control_loop(&shared, &fs, &cs))?,
            );
        }
        {
            let shared = Arc::clone(&shared);
            threads.push(
                thread::Builder::new()
                    .name("sim-tick".into())
                    .spawn(move || tick_loop(&shared))?,
            );
        }

        Ok(SimDevice {
            handle: SimHandle { shared },
            threads,
            mdns,
            http,
        })
    }

    /// A handle to drive this device with. Cheap to clone.
    #[must_use]
    pub fn handle(&self) -> SimHandle {
        self.handle.clone()
    }

    /// The bound frame address.
    #[must_use]
    pub fn frame_addr(&self) -> SocketAddr {
        self.handle.frame_addr()
    }

    /// The bound control address.
    #[must_use]
    pub fn control_addr(&self) -> SocketAddr {
        self.handle.control_addr()
    }

    /// Stop the threads and wait for them.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.handle.shared.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.http.take() {
            h.shutdown();
        }
        if let Some(m) = self.mdns.take() {
            m.shutdown();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// The address the HTTP API is served on, or `None` when it is off.
    #[must_use]
    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.handle.http_addr()
    }
}

impl Drop for SimDevice {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// A datagram waiting out a `--delay-ms` fault.
struct Held {
    due_us: u64,
    from: SocketAddr,
    data: Vec<u8>,
}

fn frame_loop(
    shared: &Arc<Shared>,
    frame_sock: &UdpSocket,
    control_sock: &UdpSocket,
    sink: &mut Option<FrameSink>,
    seed: u64,
) {
    // One byte more than the protocol's ceiling, so an over-budget datagram
    // arrives long enough to be recognised as one rather than silently
    // truncated into a length error (spec section 1).
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD + 1];
    let mut out = Outbox::default();
    let mut held: VecDeque<Held> = VecDeque::new();
    let mut rng = Rng::new(seed);
    let mut batch: Vec<(SocketAddr, Vec<u8>)> = Vec::new();

    while !shared.stop.load(Ordering::SeqCst) {
        let faults = *shared.faults.lock().unwrap();
        batch.clear();

        // Spec section 3.3 step 1: drain until it would block.
        for _ in 0..MAX_DRAIN {
            match frame_sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    // Loss on the air: the datagram never happened, so
                    // nothing is parsed and nothing is counted.
                    if faults.drop_pct > 0.0 && rng.chance(faults.drop_pct) {
                        continue;
                    }
                    let data = buf[..n].to_vec();
                    if faults.delay_ms > 0 {
                        // A uniform 0..=delay_ms hold, which produces jitter
                        // and reordering rather than a constant offset.
                        let d = rng.below(faults.delay_ms as u64 + 1) * 1_000;
                        held.push_back(Held {
                            due_us: shared.now_us() + d,
                            from,
                            data,
                        });
                    } else {
                        batch.push((from, data));
                    }
                }
                Err(e) if would_block(&e) => break,
                Err(_) => break,
            }
        }

        // Release anything whose delay has run out, oldest first.
        if !held.is_empty() {
            let now = shared.now_us();
            let mut keep = VecDeque::with_capacity(held.len());
            let mut due: Vec<Held> = Vec::new();
            for h in held.drain(..) {
                if h.due_us <= now {
                    due.push(h);
                } else {
                    keep.push_back(h);
                }
            }
            held = keep;
            due.sort_by_key(|h| h.due_us);
            for h in due {
                batch.push((h.from, h.data));
            }
        }

        if batch.is_empty() {
            continue;
        }

        let mut shown: Option<(Box<Rgb888Frame>, FrameMeta)> = None;
        {
            let now = shared.now_us();
            let mut core = shared.core.lock().unwrap();
            for (from, data) in batch.drain(..) {
                core.offer_frame(now, from, &data, &mut out);
            }
            core.flush_frames(now, &mut out);
            if sink.is_some() && out.events.iter().any(|e| matches!(e, Event::Shown { .. })) {
                if let Some(meta) = core.shown() {
                    shown = Some((Box::new(*core.decoded()), meta));
                }
            }
        }

        send_all(&out, frame_sock, control_sock);
        shared.bus.publish(&out.events);
        out.clear();

        if let (Some(f), Some((frame, meta))) = (sink.as_mut(), shown) {
            f(&frame, &meta);
        }

        // Pretend the decode was slow. Outside the lock, so the control port
        // stays responsive - and after the swap, so the *next* drain is the
        // one that piles up and raises frames_dropped_superseded.
        if faults.decode_ms > 0 {
            thread::sleep(Duration::from_millis(faults.decode_ms as u64));
        }
    }
}

fn control_loop(shared: &Arc<Shared>, frame_sock: &UdpSocket, control_sock: &UdpSocket) {
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD + 1];
    let mut out = Outbox::default();

    while !shared.stop.load(Ordering::SeqCst) {
        match control_sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let now = shared.now_us();
                {
                    let mut core = shared.core.lock().unwrap();
                    core.control(now, from, &buf[..n], &mut out);
                }
                send_all(&out, frame_sock, control_sock);
                shared.bus.publish(&out.events);
                out.clear();
            }
            Err(e) if would_block(&e) => {}
            Err(_) => break,
        }
    }
}

fn tick_loop(shared: &Arc<Shared>) {
    let mut out = Outbox::default();
    while !shared.stop.load(Ordering::SeqCst) {
        {
            let now = shared.now_us();
            let mut core = shared.core.lock().unwrap();
            core.tick(now, &mut out);
        }
        if !out.is_empty() {
            shared.bus.publish(&out.events);
            out.clear();
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn send_all(out: &Outbox, frame_sock: &UdpSocket, control_sock: &UdpSocket) {
    for (to, data) in &out.from_frame_sock {
        let _ = frame_sock.send_to(data, to);
    }
    for (to, data) in &out.from_control_sock {
        let _ = control_sock.send_to(data, to);
    }
}

fn would_block(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

// ---------------------------------------------------------------------------
// A small deterministic RNG, so a lossy run repeats
// ---------------------------------------------------------------------------

/// xorshift64*. Not cryptography; it decides which packets to throw away.
struct Rng {
    s: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Rng { s: seed | 1 }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.s;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.s = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// True with probability `pct`/100.
    fn chance(&mut self, pct: f32) -> bool {
        let p = (pct.clamp(0.0, 100.0) / 100.0 * u32::MAX as f32) as u32;
        (self.next() >> 32) as u32 <= p
    }

    /// Uniform in `0..n`.
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}
