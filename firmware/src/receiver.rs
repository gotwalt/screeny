//! The device's half of the receive state machine.
//!
//! The state machine itself - spec sections 3.3, 4.7, 6 and 7 - is
//! [`screeny_receiver`], the `no_std` crate `crates/sim` runs on too, so the
//! two implementations that cards 006 and 008 maintained side by side are now
//! one (card 016). What is left here is what only a *device* has: the log
//! lines a serial console shows, the atomics the display task reads, the
//! fixed [`Outbox`] the frame task drains, the private bench opcode, and the
//! idle screen.
//!
//! Every method still takes `now_us`; the tasks in [`crate::net`] own the
//! sockets and the wall clock.

use core::sync::atomic::Ordering;

use embassy_net::{IpAddress, IpEndpoint};
use log::info;
use screeny_proto::control::{ErrorCode, IdleMode, Reply, Telemetry};
use screeny_proto::Rgb888Frame;
use screeny_receiver as rx;

use crate::display::{self, Frame};

pub use screeny_receiver::{Intent, Offer, INFO_MAX, OUT_MAX};

/// Private control opcode, from section 6.3's `0x80`-`0xFF` experimental
/// range. **Bench only**; see [`Core::bench`].
pub const OP_BENCH: u8 = 0x80;

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

/// One datagram the caller must send from the **frame** socket (section 6.2).
pub struct Out {
    /// Where it goes: the source of the frame that provoked it.
    pub to: IpEndpoint,
    buf: [u8; OUT_MAX],
    len: usize,
}

impl Out {
    /// The bytes to send.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// At most four unsolicited packets per drain: a `BUSY` for each of up to two
/// locked-out sources and a `TELEMETRY` for the holder, with slack.
pub type Outbox = heapless::Vec<Out, 4>;

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

/// Everything the shared core cannot decide for itself, on this device.
///
/// Built per call and thrown away: it borrows the caller's outbox, if the call
/// is one that can produce a datagram at all.
struct Device<'a> {
    out: Option<&'a mut Outbox>,
}

impl Device<'_> {
    fn new(out: &mut Outbox) -> Device<'_> {
        Device { out: Some(out) }
    }

    /// For `tick` and the control port, neither of which sends anything from
    /// the frame socket.
    fn quiet() -> Device<'static> {
        Device { out: None }
    }
}

impl rx::Host for Device<'_> {
    type Addr = IpEndpoint;
    type Ip = IpAddress;

    fn ip_of(a: IpEndpoint) -> IpAddress {
        a.addr
    }

    fn micros(&self) -> u64 {
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros()
    }

    fn send_from_frame_sock(&mut self, to: IpEndpoint, bytes: &[u8]) {
        let Some(out) = self.out.as_deref_mut() else {
            return;
        };
        let mut item = Out {
            to,
            buf: [0u8; OUT_MAX],
            len: bytes.len().min(OUT_MAX),
        };
        item.buf[..item.len].copy_from_slice(&bytes[..item.len]);
        let _ = out.push(item);
    }

    fn wifi(&self) -> (&'static str, u8) {
        (crate::SSID, crate::WIFI_STATE.load(Ordering::Relaxed))
    }

    // `set_wifi` keeps the trait's default, `ERR_NOT_PERMITTED`. Card 014 owns
    // credential storage and the rejoin sequence. Until then that is section
    // 6.5's "op disabled in this build" - the spec has no ERR_UNSUPPORTED, and
    // inventing one would put a byte on the wire no sender could interpret.

    fn adjust_telemetry(&self, t: &mut Telemetry) {
        adjust_telemetry(t);
    }

    fn event(&mut self, e: rx::Event<'_, IpEndpoint>) {
        match e {
            rx::Event::StateChanged { from, to } => {
                info!("state: {} -> {}", from.name(), to.name());
            }
            rx::Event::LockTaken { source, .. } => info!("lock: taken by {}", source),
            rx::Event::LockReleased { source, reason } => {
                info!("lock: released ({}) held by {}", reason.name(), source);
            }
            rx::Event::Brightness { requested, applied } => {
                // The shared core reports every SET_BRIGHTNESS; only a change
                // is worth a LUT rebuild.
                if applied != crate::BRIGHTNESS.load(Ordering::Relaxed) {
                    crate::BRIGHTNESS.store(applied, Ordering::Relaxed);
                    crate::OE_OVERRIDE.store(u8::MAX, Ordering::Relaxed);
                    crate::BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
                    info!(
                        "control: brightness {} -> {} ({} OE slots)",
                        requested,
                        applied,
                        display::slots_for(applied)
                    );
                }
            }
            rx::Event::IdleMode { mode } => info!("control: idle mode {}", mode),
            rx::Event::Renamed { name } => info!("control: name is now {:?}", name),
            rx::Event::StatsReset => crate::RENDER_US_MAX_PROTO.store(0, Ordering::Relaxed),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The core
// ---------------------------------------------------------------------------

/// The two telemetry fields this device measures outside the receiver.
///
/// Byte 42 is timed on core 1, by the task that does the pushing, so it lives
/// in an atomic rather than in `Stats`; `RESET_STATS` zeroes it along with
/// everything else. The RSSI belongs to the Wi-Fi task.
fn adjust_telemetry(t: &mut Telemetry) {
    t.rssi_dbm = crate::RSSI_DBM.load(Ordering::Relaxed);
    t.render_us_max = crate::RENDER_US_MAX_PROTO
        .load(Ordering::Relaxed)
        .min(u16::MAX as u32) as u16;
}

/// The receiver, minus its sockets, its clock and its panel.
pub struct Core {
    rx: rx::Receiver<IpEndpoint>,
}

impl Core {
    /// Build a receiver. `id` is the lowercase MAC suffix of section 5.1.
    pub fn new(id: &str) -> Self {
        // Section 5.1: the default instance name is `screeny-<id>`.
        let mut name: heapless::String<{ rx::NAME_MAX }> = heapless::String::new();
        let _ = name.push_str("screeny-");
        let _ = name.push_str(id);
        Core {
            rx: rx::Receiver::new(&rx::Params {
                id,
                fw: crate::FW_VERSION,
                name: &name,
                control_port: crate::CONTROL_PORT,
                brightness: display::DEFAULT_BRIGHTNESS,
                brightness_cap: crate::BRIGHTNESS_CAP,
                rssi_dbm: 0,
                idle_mode: IdleMode::Status,
                timing: rx::Timing::SPEC,
            }),
        }
    }

    // -- accessors -------------------------------------------------------

    /// The friendly name.
    pub fn name(&self) -> &str {
        self.rx.name()
    }

    /// The `GET_INFO` / TXT bytes (section 6.6).
    pub fn info_bytes(&self) -> &[u8] {
        self.rx.info_bytes()
    }

    /// Bumped whenever the panel content must be recomposed.
    pub fn redraw(&self) -> u32 {
        self.rx.redraw
    }

    /// Set by `REBOOT`; the control task sends the reply, then resets.
    pub fn reboot_pending(&self) -> bool {
        self.rx.reboot_pending
    }

    /// Take the "the TXT record changed" flag, for the mDNS re-announce.
    pub fn take_info_changed(&mut self) -> bool {
        self.rx.take_info_changed()
    }

    /// Assemble the telemetry struct of section 6.7.
    pub fn telemetry(&self, now_us: u64) -> Telemetry {
        let mut t = self.rx.telemetry(now_us);
        adjust_telemetry(&mut t);
        t
    }

    /// Count a datagram too long to have been read whole (section 1).
    pub fn count_oversize(&mut self) {
        self.rx.count_oversize();
    }

    // -- the frame port --------------------------------------------------

    /// Offer one datagram drained from the frame socket.
    ///
    /// [`Offer::Keep`] means this datagram is the drain's survivor so far and
    /// the caller must hold on to it; an earlier survivor it thereby throws
    /// away has already been counted in `frames_dropped_superseded`.
    pub fn offer_frame(
        &mut self,
        now_us: u64,
        from: IpEndpoint,
        data: &[u8],
        out: &mut Outbox,
    ) -> Offer {
        self.rx
            .offer_frame(&mut Device::new(out), now_us, from, data)
    }

    /// Close the drain: decode the survivor into `back`, then answer any
    /// `STATS_REQ` seen during it. Returns `true` if `back` now holds a new
    /// frame the caller should publish (section 4.7: swap only on success).
    pub fn flush_frames(
        &mut self,
        now_us: u64,
        survivor: Option<&[u8]>,
        back: &mut Rgb888Frame,
        out: &mut Outbox,
    ) -> bool {
        self.rx
            .flush_frames(&mut Device::new(out), now_us, survivor, back)
    }

    /// Advance the timers: `LIVE -> HOLD`, `HOLD -> IDLE`, `IDENTIFY` expiry.
    pub fn tick(&mut self, now_us: u64) {
        self.rx.tick(&mut Device::quiet(), now_us);
    }

    /// Section 7.3's "any -> Wi-Fi link down -> HOLD".
    pub fn link_down(&mut self, now_us: u64) {
        self.rx.link_down(&mut Device::quiet(), now_us);
    }

    /// What the panel should show now.
    pub fn intent(&self, now_us: u64) -> Intent {
        self.rx.intent(now_us)
    }

    // -- the control port ------------------------------------------------

    /// Handle one datagram from the control socket.
    ///
    /// Returns the length of a reply written into `out`, or `None` when the
    /// spec says to answer nothing.
    pub fn control(
        &mut self,
        now_us: u64,
        from: IpEndpoint,
        data: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        // The bench opcode is this device's alone, so it never reaches the
        // shared core. Peeking for it before parsing would be wrong; peeking
        // after costs one parse the core repeats.
        if let Ok(pkt) = screeny_proto::ControlPacket::parse(data) {
            if pkt.flags & screeny_proto::C_REPLY == 0 && pkt.op == OP_BENCH {
                return self.bench(pkt.body, pkt.req_id, out);
            }
        }
        self.rx
            .control(&mut Device::quiet(), now_us, from, data, out)
    }

    /// The private bench opcode, `0x80`.
    ///
    /// Section 6.3 reserves `0x80`-`0xFF` for experimental and private use, so
    /// this is spec-legal rather than a hole punched in the frame port the way
    /// card 007's `testcmd` was. It matters that it is *not* on the frame
    /// port: every byte that lands there now has to be either a conforming
    /// frame or a `frames_rejected`, or the counter a sender uses to diagnose
    /// its video stream would be lying.
    ///
    /// Body: `[cmd][arg]`.
    ///
    /// | cmd | meaning |
    /// |---|---|
    /// | `'G'` | sRGB gamma on/off |
    /// | `'D'` | temporal dithering on/off |
    /// | `'W'` | first lit output-enable slot (anti-ghost window start) |
    /// | `'O'` | raw output-enable slots, ignoring the power cap, for 10 s |
    /// | `'P'` | hold built-in pattern `arg`; `arg = 255` releases |
    fn bench(&mut self, body: &[u8], req_id: u16, out: &mut [u8]) -> Option<usize> {
        let (cmd, arg) = match body {
            [c, a] => (*c, *a),
            _ => {
                return if req_id == 0 {
                    None
                } else {
                    Reply::Err {
                        code: ErrorCode::BadLength.as_u8(),
                    }
                    .write(OP_BENCH, req_id, out)
                    .ok()
                }
            }
        };
        match cmd {
            b'G' => crate::GAMMA_ON.store(arg != 0, Ordering::Relaxed),
            b'D' => crate::DITHER_ON.store(arg != 0, Ordering::Relaxed),
            b'W' => {
                crate::OE_START.store(arg, Ordering::Relaxed);
                crate::BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
            }
            b'O' => {
                crate::OE_OVERRIDE.store(arg, Ordering::Relaxed);
                crate::OE_OVERRIDE_DEADLINE_MS
                    .store(crate::now_ms().wrapping_add(10_000), Ordering::Relaxed);
                crate::BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
            }
            b'P' => {
                crate::PATTERN_HOLD.store(if arg == 255 { 0 } else { arg + 1 }, Ordering::Relaxed);
                self.rx.redraw = self.rx.redraw.wrapping_add(1);
            }
            _ => {}
        }
        info!("bench: {} {}", cmd as char, arg);
        if req_id == 0 {
            None
        } else {
            Reply::Identify.write(OP_BENCH, req_id, out).ok()
        }
    }

    // -- what the panel should show --------------------------------------

    /// Paint the idle target for the mode in force into `dst`, given the last
    /// streamed frame in `last`.
    ///
    /// The default is `STATUS` and not `BLACK` on purpose (section 7.5): a
    /// black panel is indistinguishable from a broken one.
    pub fn draw_idle(
        &self,
        dst: &mut Frame,
        last: &Frame,
        hostname: &str,
        net: crate::screens::Net,
        phase: u32,
    ) {
        match self.rx.idle_mode() {
            IdleMode::Status | IdleMode::HoldForever => {
                crate::screens::status(dst, self.rx.name(), hostname, net, phase)
            }
            IdleMode::Dim => {
                dst.copy_from(last);
                dst.scale(26); // 10%
            }
            IdleMode::Black => dst.clear(),
        }
    }
}
