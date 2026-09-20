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
use log::{info, warn};
use screeny_proto::control::{ErrorCode, IdleMode, Reply, SetWifi, Telemetry};
use screeny_proto::Rgb888Frame;
use screeny_receiver as rx;
use screeny_settings::{Name, Settings, Wifi};

use crate::display::{self, Frame};
use crate::store::{self, Immediate};

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
    /// Where a handler that needs a flash write *before* its reply leaves it.
    /// See [`crate::store::Immediate`]: `SET_NAME` and `SET_WIFI` are the only
    /// two opcodes that can honestly answer `ERR_STORAGE` (spec section 6.5),
    /// and they can only do it because the control task performs the write
    /// while the reply is still in its buffer.
    imm: Option<&'a mut Option<Immediate>>,
}

impl Device<'_> {
    fn new(out: &mut Outbox) -> Device<'_> {
        Device {
            out: Some(out),
            imm: None,
        }
    }

    /// For `tick`, which sends nothing and stores nothing.
    fn quiet() -> Device<'static> {
        Device {
            out: None,
            imm: None,
        }
    }

    /// For the control port: no frame-socket datagrams, but writes to collect.
    fn control(imm: &mut Option<Immediate>) -> Device<'_> {
        Device {
            out: None,
            imm: Some(imm),
        }
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

    /// Spec section 6.3: the SSID actually in use, never the one the build was
    /// compiled with - a default build has no such thing - and never a PSK
    /// (section 8.4). The state has [`crate::WIFI_SET_FAILED`] folded in.
    fn wifi(&self) -> (&'static str, u8) {
        (crate::current_ssid(), crate::wifi_report_state())
    }

    /// Spec section 8.2. The reply must leave before the device disconnects,
    /// so nothing is disconnected here: the pair is validated, parked in
    /// [`Device::imm`] for the control task to write to flash (which is what
    /// makes `ERR_STORAGE` honest), and only then signalled to the Wi-Fi task.
    ///
    /// The PSK is copied into a [`Wifi`] and never formatted.
    fn set_wifi(&mut self, w: &SetWifi<'_>) -> Result<(), u8> {
        let Some(slot) = self.imm.as_deref_mut() else {
            // `set_wifi` can only arrive on the control port, which always
            // builds a `Device::control`. Refusing rather than silently
            // succeeding keeps that true.
            return Err(ErrorCode::NotPermitted.as_u8());
        };
        let wifi = Wifi::new(w.ssid.as_bytes(), w.psk.as_bytes()).map_err(|e| {
            warn!("control: SET_WIFI refused: {:?}", e);
            ErrorCode::BadArg.as_u8()
        })?;
        info!(
            "control: SET_WIFI for a {}-byte SSID, persist {}",
            wifi.ssid.len(),
            w.persist
        );
        *slot = Some(Immediate::Wifi {
            wifi,
            persist: w.persist,
        });
        Ok(())
    }

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
                    // Debounced: a slider sends ~60 of these a second, and the
                    // reply has already been decided, so it cannot report
                    // `ERR_STORAGE`. Noting the change here rather than on
                    // every `SET_BRIGHTNESS` means a sender that keeps setting
                    // the value it already has costs nothing at all.
                    store::note_dirty(store::DIRTY_BRIGHTNESS);
                }
            }
            rx::Event::IdleMode { mode } => {
                info!("control: idle mode {}", mode);
                store::note_dirty(store::DIRTY_IDLE);
            }
            rx::Event::Renamed { name } => {
                info!("control: name is now {:?}", name);
                // Immediate, not debounced: `SET_NAME` is the one naming
                // opcode a person watches the reply of, so it gets to answer
                // `ERR_STORAGE`. A name longer than the store can hold cannot
                // arrive - the receiver has already truncated it to
                // `MAX_NAME_LEN`, which is the store's limit too.
                match (Name::new(name), self.imm.as_deref_mut()) {
                    (Ok(n), Some(slot)) => *slot = Some(Immediate::Name(n)),
                    (Ok(_), None) => {}
                    (Err(e), _) => warn!("control: name not storable: {:?}", e),
                }
            }
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
    /// Build a receiver. `id` is the lowercase MAC suffix of section 5.1, and
    /// `settings` is what card 212's store read out of flash at boot.
    ///
    /// Everything persisted that the state machine owns is seeded here rather
    /// than applied afterwards, so there is no window in which the device is
    /// running under a default it is about to change: mDNS announces the stored
    /// name from its first announcement, and `GET_INFO` never reports the
    /// default one.
    pub fn new(id: &str, settings: &Settings) -> Self {
        // Section 5.1: the default instance name is `screeny-<id>`, and an
        // empty stored name means exactly that.
        let mut name: heapless::String<{ rx::NAME_MAX }> = heapless::String::new();
        if settings.name.is_empty() {
            let _ = name.push_str("screeny-");
            let _ = name.push_str(id);
        } else {
            let _ = name.push_str(settings.name.as_str());
        }
        Core {
            rx: rx::Receiver::new(&rx::Params {
                id,
                fw: crate::FW_VERSION,
                name: &name,
                control_port: crate::CONTROL_PORT,
                brightness: settings.brightness.min(crate::BRIGHTNESS_CAP),
                brightness_cap: crate::BRIGHTNESS_CAP,
                rssi_dbm: 0,
                idle_mode: settings.idle_mode,
                timing: rx::Timing::SPEC,
            }),
        }
    }

    // -- accessors -------------------------------------------------------

    /// The friendly name.
    pub fn name(&self) -> &str {
        self.rx.name()
    }

    /// The idle behaviour in force. The store task reads it here rather than
    /// keeping a copy, so flash can never disagree with the panel.
    pub fn idle_mode(&self) -> IdleMode {
        self.rx.idle_mode()
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
    /// spec says to answer nothing. `imm` comes back `Some` when the opcode
    /// wants a flash write **before** the reply goes out; the caller performs
    /// it and downgrades the reply to `ERR_STORAGE` if it fails.
    pub fn control(
        &mut self,
        now_us: u64,
        from: IpEndpoint,
        data: &[u8],
        out: &mut [u8],
        imm: &mut Option<Immediate>,
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
            .control(&mut Device::control(imm), now_us, from, data, out)
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
