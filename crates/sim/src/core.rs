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

use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use screeny_proto::control::{wifi_state, IdleMode, SetWifi, Telemetry};
use screeny_proto::{Rgb888Frame, MAX_UDP_PAYLOAD, NBYTES};
use screeny_receiver as rx;

use crate::config::{Config, PanelModel};
use crate::event::Event;
use crate::panel;
use crate::stats::Stats;

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
        (SIM_SSID, wifi_state::CONNECTED)
    }

    fn set_wifi(&mut self, w: &SetWifi<'_>) -> Result<(), u8> {
        // Accepted and logged, never acted on: there is no radio here. The PSK
        // is not logged either - section 8.4's invariant holds in the
        // simulator too, which is why only these two fields are read.
        self.out.events.push(Event::SetWifi {
            ssid: w.ssid.to_string(),
            persist: w.persist,
        });
        Ok(())
    }
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
}

impl Core {
    /// Build a receiver from a configuration.
    #[must_use]
    pub fn new(cfg: &Config) -> Self {
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
    #[must_use]
    pub fn state_byte(&self) -> u8 {
        self.rx.state_byte()
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
            rx, env, panel, survivor, ..
        } = self;
        let mut h = Sim { out, env, panel };
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
        } = self;
        let kept = survivor.take();
        let mut h = Sim { out, env, panel };
        rx.flush_frames(&mut h, now_us, kept.as_deref(), back);
    }

    /// Advance the timers: `LIVE -> HOLD`, `HOLD -> IDLE`, `IDENTIFY` expiry.
    pub fn tick(&mut self, now_us: u64, out: &mut Outbox) {
        let Core {
            rx, env, panel, ..
        } = self;
        let mut h = Sim { out, env, panel };
        rx.tick(&mut h, now_us);
    }

    // -----------------------------------------------------------------
    // Control port (section 6)
    // -----------------------------------------------------------------

    /// Handle one datagram from the control socket.
    pub fn control(&mut self, now_us: u64, from: SocketAddr, data: &[u8], out: &mut Outbox) {
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        let n = {
            let Core {
                rx, env, panel, ..
            } = self;
            let mut h = Sim { out, env, panel };
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
