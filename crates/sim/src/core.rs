//! The receive state machine, with no I/O and no wall clock in it.
//!
//! Since card 016 the state machine itself is
//! [`screeny_receiver`](screeny_receiver), one `no_std` crate shared with the
//! firmware, so a spec change is made once and cannot drift. What lives here
//! is everything a *simulator* adds to it: owned `Vec` outboxes, the event log
//! a test asserts on, and the three frame buffers plus the panel model, which
//! a device keeps in its display driver instead.
//!
//! Everything spec sections 3.3, 4.7, 6 and 7 require of a receiver still
//! happens here in the sense that matters - every method takes the current
//! time as an argument, so the source lock, `HOLD_MS` and the rate limiters
//! are testable in microseconds instead of in real seconds, and
//! [`crate::device`] contains nothing but sockets and threads.
//!
//! The one thing this module does *not* decide is when a drain ends. Spec
//! section 3.3 says to drain the socket until it would block and keep the
//! newest acceptable frame, so the caller feeds datagrams with
//! [`Core::offer_frame`] and then closes the batch with
//! [`Core::flush_frames`], which is the point at which exactly one frame is
//! decoded and swapped onto the panel.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::Instant;

use screeny_proto::control::{IdleMode, SetWifi, Telemetry};
use screeny_proto::{Rgb888Frame, MAX_UDP_PAYLOAD, NBYTES};
use screeny_receiver as rx;

use crate::config::{Config, PanelModel};
use crate::net::display_addr;
use crate::event::Event;
use crate::panel;
use crate::stats::Stats;
use crate::wifi::WifiModel;

pub use screeny_receiver::FrameMeta as SharedFrameMeta;

/// What the panel is showing, apart from any overlay.
pub type FrameMeta = rx::FrameMeta<SocketAddr>;

/// Datagrams the caller must send, and events it should publish.
#[derive(Debug, Default)]
pub struct Outbox {
    /// `(destination, datagram)` pairs to send **from the frame socket**.
    /// Spec section 6.4: a piggybacked `TELEMETRY`, and by the same rule a
    /// `BUSY`, leaves by the port the frame came in on.
    pub from_frame_sock: Vec<(SocketAddr, Vec<u8>)>,
    /// `(destination, datagram)` pairs to send from the control socket.
    pub from_control_sock: Vec<(SocketAddr, Vec<u8>)>,
    /// What happened, in order.
    pub events: Vec<Event>,
}

impl Outbox {
    /// Drop everything, keeping the allocations.
    pub fn clear(&mut self) {
        self.from_frame_sock.clear();
        self.from_control_sock.clear();
        self.events.clear();
    }

    /// True if there is nothing to send and nothing to say.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.from_frame_sock.is_empty()
            && self.from_control_sock.is_empty()
            && self.events.is_empty()
    }
}

/// The simulator's settings that the shared receiver does not hold.
struct Env {
    panel_model: PanelModel,
    /// A monotonic epoch for measuring decode and render cost. Nothing outside
    /// this module sees it: only differences are ever used.
    boot: Instant,
}

/// The panel, such as it is.
struct Panel {
    /// The last frame that decoded successfully: exactly what a sender sent.
    front: Box<Rgb888Frame>,
    /// `front` through the panel model and the brightness LUT.
    out: Box<Rgb888Frame>,
    /// The brightness the LUT in `out` was built with.
    brightness: u8,
}

impl Panel {
    fn apply(&mut self, model: &PanelModel) {
        panel::apply(model, self.brightness, &self.front, &mut self.out);
    }
}

/// The simulator's half of the shared receiver: sockets become `Vec`s, log
/// lines become [`Event`]s, and a decoded frame becomes three buffers.
struct Sim<'a> {
    out: &'a mut Outbox,
    env: &'a Env,
    panel: &'a mut Panel,
    wifi: &'a mut WifiModel,
    /// The time the caller was given. `Host::micros` measures from the core's
    /// own epoch; this is the device's, and it is what the provisioning
    /// machine has to be driven with so every part of the simulator agrees
    /// about what time it is.
    now_us: u64,
}

impl rx::Host for Sim<'_> {
    type Addr = SocketAddr;
    type Ip = IpAddr;

    fn ip_of(a: SocketAddr) -> IpAddr {
        a.ip()
    }

    fn micros(&self) -> u64 {
        self.env.boot.elapsed().as_micros() as u64
    }

    fn send_from_frame_sock(&mut self, to: SocketAddr, bytes: &[u8]) {
        self.out.from_frame_sock.push((to, bytes.to_vec()));
    }

    fn present(&mut self, frame: &Rgb888Frame) -> u32 {
        self.panel.front.copy_from_slice(frame);
        let t0 = Instant::now();
        self.panel.apply(&self.env.panel_model);
        t0.elapsed().as_micros() as u32
    }

    fn event(&mut self, e: rx::Event<'_, SocketAddr>) {
        // Brightness is the one thing the panel has to be told about: the LUT
        // in `out` was built for the old value.
        if let rx::Event::Brightness { applied, .. } = e {
            if applied != self.panel.brightness {
                self.panel.brightness = applied;
                self.panel.apply(&self.env.panel_model);
            }
        }
        if let Some(e) = Event::from_shared(&e) {
            self.out.events.push(e);
        }
    }

    fn wifi(&self) -> (&'static str, u8) {
        (intern(self.wifi.ssid()), self.wifi.wifi_state())
    }

    fn set_wifi(&mut self, w: &SetWifi<'_>) -> Result<(), u8> {
        // Logged first, exactly as before: the event carries the SSID and the
        // persist flag and has no field for a PSK, which is how section 8.4's
        // invariant is kept rather than remembered.
        self.out.events.push(Event::SetWifi {
            ssid: w.ssid.to_string(),
            persist: w.persist,
        });
        // ...and now it feeds the provisioning machine, which is the same one
        // `POST /api/v1/wifi` feeds. `w.psk` is not passed on and is not kept:
        // the scripted radio decides the outcome, so the simulator has no use
        // for a password and therefore never holds one.
        self.wifi
            .post_credentials(w.ssid, self.now_us, &mut self.out.events);
        // Spec section 8.2 is "reply before disconnecting", and the reply is
        // built by the caller the moment this returns, before any scripted
        // join can be delivered by a tick. `Ok` whatever the machine did with
        // it: `SET_WIFI` has always been accepted here, and card 224 may not
        // change what the simulator does on UDP.
        Ok(())
    }
}

/// `screeny_receiver::Host::wifi` returns a `&'static str`, because on the
/// device the SSID lives in a `static` that outlives everything. Here it lives
/// in the provisioning machine and changes, so it is interned: one leak per
/// *distinct* SSID a run ever reports, which is one in every run that is not a
/// test of provisioning itself. Recorded in card 224's log as feedback on the
/// `Host` trait rather than papered over.
fn intern(s: &str) -> &'static str {
    static POOL: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut g = POOL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let pool = g.get_or_insert_with(HashSet::new);
    if let Some(found) = pool.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// The receiver, minus its sockets.
pub struct Core {
    rx: rx::Receiver<SocketAddr>,
    env: Env,
    panel: Panel,
    /// Decode target. Never visible until a decode succeeds (section 4.7).
    back: Box<Rgb888Frame>,
    /// The datagram the shared core asked us to keep for this drain.
    survivor: Option<Vec<u8>>,
    /// The scripted radio and the provisioning machine (card 224).
    wifi: WifiModel,
    /// What the device would report about itself over HTTP but cannot measure
    /// on a host.
    ident: Ident,
}

/// The parts of `GET /api/v1/status` a simulator can only make up: which flash
/// slot is running, why the chip last restarted, how much stack is left. They
/// are constants here, named so that nobody mistakes them for measurements.
#[derive(Debug, Clone)]
pub struct Ident {
    /// The stable short device id.
    pub id: String,
    /// The firmware version string.
    pub fw: String,
    /// Heap in use, bytes. A plausible constant.
    pub heap_used: u32,
    /// Heap total, bytes. The firmware's 64 + 32 KB (card 220).
    pub heap_size: u32,
    /// Stack never touched, bytes.
    pub stack_free: u32,
    /// Settings-store errors since boot. Always 0: there is no flash here.
    pub store_errors: u32,
}

impl Core {
    /// Build a receiver from a configuration.
    ///
    /// The provisioning machine is booted here, so a `Core` driven directly by
    /// a unit test has the same WiFi life a running device has.
    #[must_use]
    pub fn new(cfg: &Config) -> Self {
        let mut boot_events = Vec::new();
        let core = Self::new_with(cfg, &mut boot_events);
        core
    }

    /// As [`Core::new`], collecting the events the boot produced.
    #[must_use]
    pub fn new_with(cfg: &Config, events: &mut Vec<Event>) -> Self {
        let mut r = rx::Receiver::new(&rx::Params {
            id: &cfg.id,
            fw: &cfg.fw,
            name: &cfg.name,
            control_port: cfg.control_port,
            brightness: cfg.brightness,
            brightness_cap: cfg.brightness_cap,
            rssi_dbm: cfg.rssi_dbm,
            idle_mode: cfg.idle_mode,
            timing: cfg.timing,
        });
        r.set_extra_decode_us(cfg.faults.decode_ms.saturating_mul(1_000));
        let brightness = r.brightness();
        Core {
            rx: r,
            env: Env {
                panel_model: cfg.panel,
                boot: Instant::now(),
            },
            panel: Panel {
                front: Box::new([0u8; NBYTES]),
                out: Box::new([0u8; NBYTES]),
                brightness,
            },
            back: Box::new([0u8; NBYTES]),
            survivor: None,
            wifi: WifiModel::new(cfg, display_addr(cfg.bind), events),
            ident: Ident {
                id: cfg.id.clone(),
                fw: cfg.fw.clone(),
                heap_used: 64 * 1024,
                heap_size: 96 * 1024,
                stack_free: 20 * 1024,
                store_errors: 0,
            },
        }
    }

    // -----------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------

    /// The stream state, ignoring overlays.
    #[must_use]
    pub fn state(&self) -> State {
        self.rx.state()
    }

    /// The telemetry `state` byte. Section 7.3: `IDENTIFY` is an overlay, not
    /// a stream state, and it is what the byte reports while it is up.
    ///
    /// Card 224 adds the second overlay, `PROVISIONING`, from the same
    /// machine `GET_WIFI` reads, so the two cannot disagree. `IDENTIFY` wins
    /// when both are up: it lasts seconds and somebody is standing there
    /// asking "which one is this?", while the portal can be up for hours.
    #[must_use]
    pub fn state_byte(&self) -> u8 {
        let base = self.rx.state_byte();
        if base == screeny_proto::control::state::IDENTIFY {
            return base;
        }
        self.wifi.overlay_state().unwrap_or(base)
    }

    /// The scripted radio and the provisioning machine.
    #[must_use]
    pub fn wifi(&self) -> &WifiModel {
        &self.wifi
    }

    /// The made-up half of `GET /api/v1/status`.
    #[must_use]
    pub fn ident(&self) -> &Ident {
        &self.ident
    }

    /// The source that holds the lock, if any.
    #[must_use]
    pub fn active_source(&self) -> Option<SocketAddr> {
        self.rx.active_source()
    }

    /// The last frame that decoded successfully, exactly as the sender sent
    /// it: no panel model, no brightness, no overlay.
    #[must_use]
    pub fn decoded(&self) -> &Rgb888Frame {
        &self.panel.front
    }

    /// The last decoded frame through the panel model and the brightness LUT.
    #[must_use]
    pub fn panel(&self) -> &Rgb888Frame {
        &self.panel.out
    }

    /// Metadata of the displayed frame.
    #[must_use]
    pub fn shown(&self) -> Option<FrameMeta> {
        self.rx.shown()
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> &Stats {
        self.rx.stats()
    }

    /// Brightness currently applied.
    #[must_use]
    pub fn brightness(&self) -> u8 {
        self.rx.brightness()
    }

    /// The idle behaviour in force.
    #[must_use]
    pub fn idle_mode(&self) -> IdleMode {
        self.rx.idle_mode()
    }

    /// The friendly name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.rx.name()
    }

    /// Microseconds left of the `IDENTIFY` overlay, if it is up.
    #[must_use]
    pub fn identify_until_us(&self) -> Option<u64> {
        self.rx.identify_until_us()
    }

    /// When the device entered `HOLD`, for the cross-fade.
    #[must_use]
    pub fn hold_since_us(&self) -> u64 {
        self.rx.hold_since_us()
    }

    /// When the device entered `IDLE`.
    #[must_use]
    pub fn idle_since_us(&self) -> u64 {
        self.rx.idle_since_us()
    }

    /// Fresh `GET_INFO` / TXT bytes (section 6.6).
    #[must_use]
    pub fn info_bytes(&self) -> &[u8] {
        self.rx.info_bytes()
    }

    /// Tell the core which control port it ended up on, so `ctrl=` in the TXT
    /// record is the port a sender can actually reach (spec section 1: the
    /// `ctrl=` key is authoritative). Tests bind port 0.
    pub fn set_control_port(&mut self, port: u16) {
        self.rx.set_control_port(port);
    }

    /// Assemble the telemetry struct of section 6.7.
    #[must_use]
    pub fn telemetry(&self, now_us: u64) -> Telemetry {
        self.rx.telemetry(now_us)
    }

    /// Change the injected decode cost at runtime.
    pub fn set_extra_decode_us(&mut self, us: u32) {
        self.rx.set_extra_decode_us(us);
    }

    // -----------------------------------------------------------------
    // Frame port
    // -----------------------------------------------------------------

    /// Offer one datagram drained from the frame socket.
    ///
    /// Applies section 2's validation, section 7.4's admission rule and
    /// section 3.2's sequence test. An accepted frame becomes the drain's
    /// survivor, superseding any earlier one (section 3.3). Nothing is
    /// decoded or displayed until [`Core::flush_frames`].
    pub fn offer_frame(&mut self, now_us: u64, from: SocketAddr, data: &[u8], out: &mut Outbox) {
        let Core {
            rx,
            env,
            panel,
            survivor,
            wifi,
            ..
        } = self;
        let mut h = Sim {
            out,
            env,
            panel,
            wifi,
            now_us,
        };
        if rx.offer_frame(&mut h, now_us, from, data) == rx::Offer::Keep {
            // The device swaps two static buffers here; a simulator can afford
            // the copy, and it keeps `Core` free of borrowed datagrams.
            *survivor = Some(data.to_vec());
        }
    }

    /// Close the drain: decode the survivor, swap it onto the panel, then
    /// answer any `STATS_REQ` seen during the drain.
    pub fn flush_frames(&mut self, now_us: u64, out: &mut Outbox) {
        let Core {
            rx,
            env,
            panel,
            back,
            survivor,
            wifi,
            ..
        } = self;
        let kept = survivor.take();
        let mut h = Sim {
            out,
            env,
            panel,
            wifi,
            now_us,
        };
        rx.flush_frames(&mut h, now_us, kept.as_deref(), back);
    }

    /// Advance the timers: `LIVE -> HOLD`, `HOLD -> IDLE`, `IDENTIFY` expiry,
    /// and the provisioning machine's own (card 224).
    pub fn tick(&mut self, now_us: u64, out: &mut Outbox) {
        let Core {
            rx,
            env,
            panel,
            wifi,
            ..
        } = self;
        let mut h = Sim {
            out,
            env,
            panel,
            wifi,
            now_us,
        };
        rx.tick(&mut h, now_us);
        self.wifi.tick(now_us, &mut out.events);
    }

    /// Spec section 7.3's "any -> WiFi link down -> `HOLD`", and the
    /// provisioning machine's own link timer, from one call so they cannot be
    /// done by halves.
    pub fn set_link_down(&mut self, down: bool, now_us: u64, out: &mut Outbox) {
        let Core {
            rx,
            env,
            panel,
            wifi,
            ..
        } = self;
        let mut h = Sim {
            out,
            env,
            panel,
            wifi,
            now_us,
        };
        if down {
            rx.link_down(&mut h, now_us);
        }
        self.wifi.set_link_down(down, now_us, &mut out.events);
    }

    /// Drive the provisioning machine directly: `POST /api/v1/wifi`, the
    /// button wipe, a station arriving on the soft-AP.
    pub fn wifi_mut(&mut self) -> &mut WifiModel {
        &mut self.wifi
    }

    // -----------------------------------------------------------------
    // Control port (section 6)
    // -----------------------------------------------------------------

    /// Handle one datagram from the control socket.
    pub fn control(&mut self, now_us: u64, from: SocketAddr, data: &[u8], out: &mut Outbox) {
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        let n = {
            let Core {
                rx,
                env,
                panel,
                wifi,
                ..
            } = self;
            let mut h = Sim {
                out,
                env,
                panel,
                wifi,
                now_us,
            };
            rx.control(&mut h, now_us, from, data, &mut buf)
        };
        if let Some(n) = n {
            out.from_control_sock.push((from, buf[..n].to_vec()));
        }
    }
}

/// The stream state of spec section 7.3.
pub use crate::event::State;

/// The SSID the simulator claims to be on. Not a real network; `GET_WIFI`
/// has to answer something and the spec forbids it answering with a PSK.
pub const SIM_SSID: &str = "simulated";
