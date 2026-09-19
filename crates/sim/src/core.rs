//! The receive state machine, with no I/O and no wall clock in it.
//!
//! Everything spec sections 3.3, 4.7, 6 and 7 require of a receiver lives
//! here, and every method takes the current time as an argument. That makes
//! the whole of the device's behaviour - the source lock, `HOLD_MS`, the two
//! rate limiters - testable in microseconds instead of in real seconds, and
//! it means [`crate::device`] contains nothing but sockets and threads.
//!
//! The one thing this module does *not* decide is when a drain ends. Spec
//! section 3.3 says to drain the socket until it would block and keep the
//! newest acceptable frame, so the caller feeds datagrams with
//! [`Core::offer_frame`] and then closes the batch with
//! [`Core::flush_frames`], which is the point at which exactly one frame is
//! decoded and swapped onto the panel.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use screeny_proto::control::{op, state as tstate, wifi_state, IdleMode, Reply, Request};
use screeny_proto::control::{busy_reason, ErrorCode, Telemetry};
use screeny_proto::txt::DeviceInfo;
use screeny_proto::{dec, ControlPacket, FramePacket, Rgb888Frame};
use screeny_proto::{C_REPLY, MAX_UDP_PAYLOAD, NBYTES, TYPE_CONTROL, VERSION};

use crate::config::{Config, PanelModel, Timing};
use crate::event::{DropCause, Event, ReleaseReason, State};
use crate::panel;
use crate::stats::{Counters, Stats};

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
        self.from_frame_sock.is_empty() && self.from_control_sock.is_empty() && self.events.is_empty()
    }
}

/// A frame accepted during the current drain and not yet superseded.
struct Pending {
    from: SocketAddr,
    seq: u16,
    codec: u8,
    is_final: bool,
    timestamp_us: Option<u32>,
    payload: Vec<u8>,
}

/// What the panel is showing, apart from any overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    /// Sequence number of the displayed frame.
    pub seq: u16,
    /// Its codec id.
    pub codec: u8,
    /// Its pixel payload size in bytes, `HAS_TS` prefix excluded.
    pub bytes: usize,
    /// Its `HAS_TS` timestamp, if any.
    pub timestamp_us: Option<u32>,
    /// Who sent it.
    pub from: SocketAddr,
}

/// The receiver, minus its sockets.
pub struct Core {
    // --- identity and settings ------------------------------------------
    name: String,
    id: String,
    fw: String,
    brightness_cap: u8,
    brightness: u8,
    idle_mode: IdleMode,
    rssi_dbm: i8,
    timing: Timing,
    panel_model: PanelModel,
    /// `GET_INFO` body and mDNS TXT record, the same bytes (section 6.6).
    info: Vec<u8>,
    /// The control port we advertise, filled in once the socket is bound.
    ctrl_port: u16,

    // --- stream state ----------------------------------------------------
    state: State,
    active_source: Option<SocketAddr>,
    last_frame_at_us: u64,
    /// When the device entered `HOLD`.
    hold_since_us: u64,
    /// When the device entered `IDLE`.
    idle_since_us: u64,
    last_seq: u16,
    have_last_seq: bool,

    // --- display ---------------------------------------------------------
    /// The last frame that decoded successfully: exactly what a sender sent.
    front: Box<Rgb888Frame>,
    /// Decode target. Never visible until a decode succeeds (section 4.7).
    back: Box<Rgb888Frame>,
    /// `front` through the panel model and the brightness LUT.
    panel_out: Box<Rgb888Frame>,
    /// Metadata of the frame in `front`, `None` until one is displayed.
    shown: Option<FrameMeta>,

    // --- overlays --------------------------------------------------------
    identify_until_us: Option<u64>,

    // --- counters --------------------------------------------------------
    stats: Stats,
    /// Codec of the last frame shown, telemetry byte 47.
    last_codec: u8,
    /// Extra microseconds to add to every decode, for `--decode-ms`.
    extra_decode_us: u32,

    // --- rate limiters ---------------------------------------------------
    busy_sent_at: HashMap<SocketAddr, u64>,
    telemetry_sent_at: HashMap<SocketAddr, u64>,
    /// Section 5.5: `(when, req_id we answered)` per source address.
    info_sent_at: HashMap<IpAddr, (u64, u16)>,

    // --- the current drain -----------------------------------------------
    pending: Option<Pending>,
    stats_req: Vec<SocketAddr>,
}

impl Core {
    /// Build a receiver from a configuration.
    #[must_use]
    pub fn new(cfg: &Config) -> Self {
        let mut c = Core {
            name: cfg.name.clone(),
            id: cfg.id.clone(),
            fw: cfg.fw.clone(),
            brightness_cap: cfg.brightness_cap,
            brightness: cfg.brightness.min(cfg.brightness_cap),
            idle_mode: cfg.idle_mode,
            rssi_dbm: cfg.rssi_dbm,
            timing: cfg.timing,
            panel_model: cfg.panel,
            info: Vec::new(),
            ctrl_port: cfg.control_port,
            state: State::Idle,
            active_source: None,
            last_frame_at_us: 0,
            hold_since_us: 0,
            idle_since_us: 0,
            last_seq: 0,
            have_last_seq: false,
            front: Box::new([0u8; NBYTES]),
            back: Box::new([0u8; NBYTES]),
            panel_out: Box::new([0u8; NBYTES]),
            shown: None,
            identify_until_us: None,
            stats: Stats::default(),
            last_codec: 0,
            extra_decode_us: cfg.faults.decode_ms.saturating_mul(1_000),
            busy_sent_at: HashMap::new(),
            telemetry_sent_at: HashMap::new(),
            info_sent_at: HashMap::new(),
            pending: None,
            stats_req: Vec::new(),
        };
        c.rebuild_info();
        c
    }

    // -----------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------

    /// The stream state, ignoring overlays.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The telemetry `state` byte. Section 7.3: `IDENTIFY` is an overlay, not
    /// a stream state, and it is what the byte reports while it is up.
    #[must_use]
    pub fn state_byte(&self) -> u8 {
        if self.identify_until_us.is_some() {
            tstate::IDENTIFY
        } else {
            self.state.as_u8()
        }
    }

    /// The source that holds the lock, if any.
    #[must_use]
    pub fn active_source(&self) -> Option<SocketAddr> {
        self.active_source
    }

    /// The last frame that decoded successfully, exactly as the sender sent
    /// it: no panel model, no brightness, no overlay.
    #[must_use]
    pub fn decoded(&self) -> &Rgb888Frame {
        &self.front
    }

    /// The last decoded frame through the panel model and the brightness LUT.
    #[must_use]
    pub fn panel(&self) -> &Rgb888Frame {
        &self.panel_out
    }

    /// Metadata of the displayed frame.
    #[must_use]
    pub fn shown(&self) -> Option<FrameMeta> {
        self.shown
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Brightness currently applied.
    #[must_use]
    pub fn brightness(&self) -> u8 {
        self.brightness
    }

    /// The idle behaviour in force.
    #[must_use]
    pub fn idle_mode(&self) -> IdleMode {
        self.idle_mode
    }

    /// The friendly name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Microseconds left of the `IDENTIFY` overlay, if it is up.
    #[must_use]
    pub fn identify_until_us(&self) -> Option<u64> {
        self.identify_until_us
    }

    /// When the device entered `HOLD`, for the cross-fade.
    #[must_use]
    pub fn hold_since_us(&self) -> u64 {
        self.hold_since_us
    }

    /// When the device entered `IDLE`.
    #[must_use]
    pub fn idle_since_us(&self) -> u64 {
        self.idle_since_us
    }

    /// Fresh `GET_INFO` / TXT bytes (section 6.6).
    #[must_use]
    pub fn info_bytes(&self) -> &[u8] {
        &self.info
    }

    /// Tell the core which control port it ended up on, so `ctrl=` in the TXT
    /// record is the port a sender can actually reach (spec section 1: the
    /// `ctrl=` key is authoritative). Tests bind port 0.
    pub fn set_control_port(&mut self, port: u16) {
        self.ctrl_port = port;
        self.rebuild_info();
    }

    /// Assemble the telemetry struct of section 6.7.
    #[must_use]
    pub fn telemetry(&self, now_us: u64) -> Telemetry {
        self.stats.telemetry(
            (now_us / 1_000) as u32,
            self.rssi_dbm,
            self.brightness,
            self.state_byte(),
            self.last_codec,
        )
    }

    /// Change the injected decode cost at runtime.
    pub fn set_extra_decode_us(&mut self, us: u32) {
        self.extra_decode_us = us;
    }

    fn rebuild_info(&mut self) {
        let info = DeviceInfo {
            codecs: dec::CODECS_TXT,
            ctrl: self.ctrl_port,
            fw: &self.fw,
            id: &self.id,
            name: &self.name,
            ..DeviceInfo::DEFAULT
        };
        let mut buf = [0u8; 512];
        let n = info.write(&mut buf).expect("TXT record fits in 512 bytes");
        self.info.clear();
        self.info.extend_from_slice(&buf[..n]);
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
        // Section 1: "A sender MUST NOT emit a datagram whose UDP payload
        // exceeds 1472 bytes." One that does is a protocol violation before
        // it is anything else, and the device's receive buffer is that size,
        // so a longer one arrives truncated and could not be trusted anyway.
        if data.len() > MAX_UDP_PAYLOAD {
            Counters::bump(&mut self.stats.counters.frames_rejected);
            out.events.push(Event::Rejected {
                from,
                reason: Some(screeny_proto::Reject::TooLong),
            });
            return;
        }
        let pkt = match FramePacket::parse(data) {
            Ok(p) => p,
            Err(reason) => {
                // Section 2: bad magic, version, type or length. Section 6.7
                // counts all of it in frames_rejected.
                Counters::bump(&mut self.stats.counters.frames_rejected);
                out.events.push(Event::Rejected {
                    from,
                    reason: Some(reason),
                });
                return;
            }
        };

        // --- section 7.4, frame admission -------------------------------
        match self.active_source {
            None => self.adopt(now_us, from, out),
            Some(s) if s == from => {}
            Some(s) => {
                let since = now_us.saturating_sub(self.last_frame_at_us) / 1_000;
                if since >= self.timing.lock_ms as u64 {
                    out.events.push(Event::LockReleased {
                        source: s,
                        reason: ReleaseReason::TakenOver,
                    });
                    self.adopt(now_us, from, out);
                } else {
                    Counters::bump(&mut self.stats.counters.frames_rejected);
                    out.events.push(Event::Rejected { from, reason: None });
                    let remaining = self.timing.lock_ms as u64 - since;
                    self.send_busy(now_us, from, remaining as u32, out);
                    return;
                }
            }
        }

        // The source is allowed to drive the panel, so a STATS_REQ on this
        // frame gets answered whatever becomes of the pixels. See the note in
        // spec section 6.4: the one frame a second a sender marks is exactly
        // the frame it must not lose track of when things go wrong.
        if pkt.wants_stats() && !self.stats_req.contains(&from) {
            self.stats_req.push(from);
        }

        // --- section 3.2, sequence numbers ------------------------------
        let n = if self.have_last_seq {
            if !screeny_proto::newer(pkt.seq, self.last_seq) {
                Counters::bump(&mut self.stats.counters.frames_dropped_stale);
                out.events.push(Event::Dropped {
                    from,
                    seq: pkt.seq,
                    cause: DropCause::Stale,
                });
                return;
            }
            let g = screeny_proto::gap(pkt.seq, self.last_seq);
            self.stats.counters.seq_gaps = self.stats.counters.seq_gaps.wrapping_add(g as u32);
            g as u32 + 1
        } else {
            1
        };
        self.last_seq = pkt.seq;
        self.have_last_seq = true;

        // --- accepted (section 7.4) -------------------------------------
        self.last_frame_at_us = now_us;
        Counters::bump(&mut self.stats.counters.frames_rx);
        self.stats.arrival.record(now_us, n);

        // --- section 3.3, newest wins -----------------------------------
        if let Some(old) = self.pending.take() {
            Counters::bump(&mut self.stats.counters.frames_dropped_superseded);
            out.events.push(Event::Dropped {
                from: old.from,
                seq: old.seq,
                cause: DropCause::Superseded,
            });
        }
        self.pending = Some(Pending {
            from,
            seq: pkt.seq,
            codec: pkt.codec,
            is_final: pkt.is_final(),
            timestamp_us: pkt.timestamp_us,
            payload: pkt.payload.to_vec(),
        });
    }

    /// Close the drain: decode the survivor, swap it onto the panel, then
    /// answer any `STATS_REQ` seen during the drain.
    pub fn flush_frames(&mut self, now_us: u64, out: &mut Outbox) {
        if let Some(p) = self.pending.take() {
            let t0 = Instant::now();
            let result = dec::decode(p.codec, &p.payload, &mut self.back);
            let decode_us = t0.elapsed().as_micros() as u64 + self.extra_decode_us as u64;
            self.stats.timings.decode(decode_us);

            match result {
                Ok(()) => {
                    // Section 3.3 step 3: swap at a refresh boundary, so a
                    // partially decoded frame is never scanned out.
                    std::mem::swap(&mut self.front, &mut self.back);
                    Counters::bump(&mut self.stats.counters.frames_shown);
                    self.last_codec = p.codec;
                    self.shown = Some(FrameMeta {
                        seq: p.seq,
                        codec: p.codec,
                        bytes: p.payload.len(),
                        timestamp_us: p.timestamp_us,
                        from: p.from,
                    });
                    let t1 = Instant::now();
                    panel::apply(&self.panel_model, self.brightness, &self.front, &mut self.panel_out);
                    self.stats.timings.render(t1.elapsed().as_micros() as u64);
                    out.events.push(Event::Shown {
                        from: p.from,
                        seq: p.seq,
                        codec: p.codec,
                        bytes: p.payload.len(),
                        timestamp_us: p.timestamp_us,
                    });
                    // Section 7.4: the lock goes when a FINAL frame is
                    // *displayed*. A FINAL frame that failed to decode was
                    // never displayed, so it does not release anything; the
                    // stream timeout will, a second later.
                    if p.is_final {
                        self.release(now_us, ReleaseReason::Final, out);
                    }
                }
                Err(e) => {
                    // Section 4.7: discard, count it, leave the previous frame lit.
                    Counters::bump(&mut self.stats.counters.frames_dropped_decode);
                    out.events.push(Event::Dropped {
                        from: p.from,
                        seq: p.seq,
                        cause: DropCause::Decode(e),
                    });
                }
            }
        }

        // Section 3.1: "after processing this frame, send one TELEMETRY
        // reply". After, so the counters the sender reads already include it.
        let reqs = std::mem::take(&mut self.stats_req);
        for to in reqs {
            self.send_telemetry(now_us, to, out);
        }
        self.stats_req.clear();
    }

    /// Advance the timers: `LIVE -> HOLD`, `HOLD -> IDLE`, `IDENTIFY` expiry.
    pub fn tick(&mut self, now_us: u64, out: &mut Outbox) {
        if let Some(until) = self.identify_until_us {
            if now_us >= until {
                self.identify_until_us = None;
            }
        }
        match self.state {
            State::Live => {
                let idle_for = now_us.saturating_sub(self.last_frame_at_us) / 1_000;
                if idle_for >= self.timing.stream_timeout_ms as u64 {
                    self.release(now_us, ReleaseReason::Timeout, out);
                }
            }
            State::Hold => {
                // Section 7.5, mode 1: stay in HOLD, never fade.
                if self.idle_mode == IdleMode::HoldForever {
                    return;
                }
                let held = now_us.saturating_sub(self.hold_since_us) / 1_000;
                if held >= self.timing.hold_ms as u64 {
                    self.set_state(State::Idle, now_us, out);
                    self.idle_since_us = now_us;
                }
            }
            State::Idle => {}
        }
        self.prune_rate_limiters(now_us);
    }

    fn adopt(&mut self, now_us: u64, from: SocketAddr, out: &mut Outbox) {
        let displaced = self.active_source;
        self.active_source = Some(from);
        // Sections 7.3 and 6.8: reset last_seq and the jitter EWMAs, not the
        // counters.
        self.have_last_seq = false;
        self.stats.arrival.on_source_change();
        out.events.push(Event::LockTaken {
            source: from,
            displaced,
        });
        self.set_state(State::Live, now_us, out);
    }

    fn release(&mut self, now_us: u64, reason: ReleaseReason, out: &mut Outbox) {
        if let Some(s) = self.active_source.take() {
            out.events.push(Event::LockReleased { source: s, reason });
        }
        self.have_last_seq = false;
        self.stats.arrival.on_source_change();
        if self.state != State::Idle {
            self.set_state(State::Hold, now_us, out);
        }
    }

    fn set_state(&mut self, to: State, now_us: u64, out: &mut Outbox) {
        if self.state == to {
            return;
        }
        let from = self.state;
        self.state = to;
        if to == State::Hold {
            self.hold_since_us = now_us;
        }
        if to == State::Idle {
            self.idle_since_us = now_us;
        }
        out.events.push(Event::StateChanged { from, to });
    }

    // -----------------------------------------------------------------
    // Unsolicited device packets (section 6.2)
    // -----------------------------------------------------------------

    fn send_busy(&mut self, now_us: u64, to: SocketAddr, remaining_ms: u32, out: &mut Outbox) {
        let min = self.timing.busy_min_interval_ms as u64 * 1_000;
        let ok = match self.busy_sent_at.get(&to) {
            Some(&t) => now_us.saturating_sub(t) >= min,
            None => true,
        };
        if ok {
            self.busy_sent_at.insert(to, now_us);
            let reply = Reply::Busy {
                reason: busy_reason::LOCKED,
                lock_holder_ms_remaining: remaining_ms,
            };
            if let Some(d) = encode_reply(&reply, op::BUSY, 0) {
                out.from_frame_sock.push((to, d));
            }
        }
        out.events.push(Event::Busy { to, sent: ok });
    }

    fn send_telemetry(&mut self, now_us: u64, to: SocketAddr, out: &mut Outbox) {
        let min = self.timing.telemetry_min_interval_ms as u64 * 1_000;
        let ok = match self.telemetry_sent_at.get(&to) {
            Some(&t) => now_us.saturating_sub(t) >= min,
            None => true,
        };
        if !ok {
            out.events.push(Event::TelemetryRateLimited { to });
            return;
        }
        self.telemetry_sent_at.insert(to, now_us);
        let reply = Reply::Telemetry(self.telemetry(now_us));
        if let Some(d) = encode_reply(&reply, op::TELEMETRY, 0) {
            out.from_frame_sock.push((to, d));
            out.events.push(Event::TelemetrySent { to });
        }
    }

    fn prune_rate_limiters(&mut self, now_us: u64) {
        // Nothing here is load-bearing; it just stops a long run against many
        // sources from growing three maps without bound.
        let cutoff = 10_000_000u64;
        self.busy_sent_at
            .retain(|_, &mut t| now_us.saturating_sub(t) < cutoff);
        self.telemetry_sent_at
            .retain(|_, &mut t| now_us.saturating_sub(t) < cutoff);
        self.info_sent_at
            .retain(|_, &mut (t, _)| now_us.saturating_sub(t) < cutoff);
    }

    // -----------------------------------------------------------------
    // Control port (section 6)
    // -----------------------------------------------------------------

    /// Handle one datagram from the control socket.
    pub fn control(&mut self, now_us: u64, from: SocketAddr, data: &[u8], out: &mut Outbox) {
        let pkt = match ControlPacket::parse(data) {
            Ok(p) => p,
            Err(reason) => return self.control_reject(from, data, reason, out),
        };

        // Section 6.1 draws requests and replies apart by the REPLY bit. A
        // reply arriving at a device is somebody else's packet.
        if pkt.flags & C_REPLY != 0 {
            out.events.push(Event::ControlRejected {
                from,
                reason: screeny_proto::Reject::BadType,
            });
            return;
        }

        let req_id = pkt.req_id;

        // Section 5.5's rate limit, before the request is carried out rather
        // than after, because a suppressed GET_INFO must leave no trace.
        // GET_INFO has an empty body, so this does not pre-empt the
        // ERR_BAD_LENGTH a malformed one still deserves.
        if pkt.op == op::GET_INFO
            && pkt.body.is_empty()
            && !self.info_allowed(now_us, from.ip(), req_id)
        {
            out.events.push(Event::Control {
                from,
                op: pkt.op,
                req_id,
                error: None,
                replied: false,
            });
            return;
        }

        let reply = match Request::decode(pkt.op, pkt.body) {
            Ok(req) => self.apply(now_us, from, req, out),
            Err(code) => Some(Reply::Err {
                code: code.as_u8(),
            }),
        };
        emit_reply(out, from, pkt.op, req_id, reply);
    }

    /// A datagram the control socket could not parse.
    ///
    /// Two of section 2's rejects have an answer rather than silence, and both
    /// of them are built out of header bytes that are certainly present:
    /// section 6.5's `ERR_BAD_LENGTH` for a `len` the datagram does not back
    /// up, and section 2.2's optional `ERR_VERSION` for a `CONTROL` of a
    /// version we do not speak. Everything else is discarded.
    fn control_reject(
        &mut self,
        from: SocketAddr,
        data: &[u8],
        reason: screeny_proto::Reject,
        out: &mut Outbox,
    ) {
        use screeny_proto::Reject;
        out.events.push(Event::ControlRejected { from, reason });
        let code = match reason {
            Reject::BadLength => ErrorCode::BadLength,
            Reject::BadVersion => ErrorCode::Version,
            _ => return,
        };
        let Some(h) = screeny_proto::peek(data) else {
            return;
        };
        if h.ty != TYPE_CONTROL || h.id == 0 {
            return;
        }
        if reason == Reject::BadVersion && h.version == VERSION {
            return;
        }
        // A reply to a reply would be a loop; a device never starts one.
        if h.flags & C_REPLY != 0 {
            return;
        }
        emit_reply(
            out,
            from,
            h.b2,
            h.id,
            Some(Reply::Err {
                code: code.as_u8(),
            }),
        );
    }

    /// Section 5.5's "one reply per source address per second", with the
    /// retry exemption the amended spec text spells out: a repeat of a
    /// `req_id` we already answered inside the window is section 6.1's
    /// retransmission, not a second request, so it is answered again.
    fn info_allowed(&mut self, now_us: u64, ip: IpAddr, req_id: u16) -> bool {
        let min = self.timing.info_min_interval_ms as u64 * 1_000;
        if let Some(&(t, last_id)) = self.info_sent_at.get(&ip) {
            if now_us.saturating_sub(t) < min && !(req_id != 0 && req_id == last_id) {
                return false;
            }
        }
        self.info_sent_at.insert(ip, (now_us, req_id));
        true
    }

    /// Carry out a decoded request and say what to reply with.
    fn apply<'s>(
        &'s mut self,
        now_us: u64,
        from: SocketAddr,
        req: Request<'_>,
        out: &mut Outbox,
    ) -> Option<Reply<'s>> {
        Some(match req {
            Request::Ping => Reply::Ping {
                uptime_ms: (now_us / 1_000) as u32,
            },
            Request::GetInfo => Reply::Info(&self.info),
            // A TELEMETRY request on the control port is solicited, so section
            // 6.2's 100 ms limit - which governs the *unsolicited* reply to a
            // STATS_REQ frame - does not apply to it.
            Request::Telemetry => Reply::Telemetry(self.telemetry(now_us)),
            Request::SetBrightness(level) => {
                let applied = level.min(self.brightness_cap);
                if applied != self.brightness {
                    self.brightness = applied;
                    self.refresh_panel();
                }
                out.events.push(Event::Brightness {
                    requested: level,
                    applied,
                });
                Reply::Brightness { applied }
            }
            Request::Identify { duration_ms } => {
                self.identify_until_us = if duration_ms == 0 {
                    None
                } else {
                    Some(now_us + duration_ms as u64 * 1_000)
                };
                out.events.push(Event::Identify { duration_ms });
                Reply::Identify
            }
            Request::SetIdle(mode) => {
                self.idle_mode = mode;
                out.events.push(Event::IdleMode {
                    mode: mode.as_u8(),
                });
                Reply::Idle {
                    mode: mode.as_u8(),
                }
            }
            Request::ResetStats => {
                self.stats.reset();
                out.events.push(Event::StatsReset);
                Reply::ResetStats
            }
            Request::Release => {
                // Section 7.4 matches on the IP alone, and it has to: RELEASE
                // arrives on the control port, so its source *port* is never
                // the frame stream's.
                if self.active_source.map(|s| s.ip()) == Some(from.ip()) {
                    self.release(now_us, ReleaseReason::Released, out);
                }
                Reply::Release
            }
            Request::SetName(name) => {
                self.name = name.to_string();
                self.rebuild_info();
                out.events.push(Event::Renamed {
                    name: self.name.clone(),
                });
                Reply::SetName
            }
            Request::GetWifi => Reply::Wifi {
                ssid: SIM_SSID,
                state: wifi_state::CONNECTED,
            },
            Request::SetWifi(w) => {
                // Accepted and logged, never acted on: there is no radio here.
                // The PSK is not logged either - section 8.4's invariant holds
                // in the simulator too.
                out.events.push(Event::SetWifi {
                    ssid: w.ssid.to_string(),
                    persist: w.persist,
                });
                Reply::SetWifi
            }
            Request::Reboot => {
                out.events.push(Event::Reboot);
                Reply::Reboot
            }
        })
    }

    /// Re-run the panel model, e.g. after a brightness change.
    fn refresh_panel(&mut self) {
        panel::apply(
            &self.panel_model,
            self.brightness,
            &self.front,
            &mut self.panel_out,
        );
    }
}

/// The SSID the simulator claims to be on. Not a real network; `GET_WIFI`
/// has to answer something and the spec forbids it answering with a PSK.
pub const SIM_SSID: &str = "simulated";

/// Queue a reply datagram, or record that we deliberately sent none.
fn emit_reply(
    out: &mut Outbox,
    from: SocketAddr,
    op: u8,
    req_id: u16,
    reply: Option<Reply<'_>>,
) {
    let error = match &reply {
        Some(Reply::Err { code }) => Some(*code),
        _ => None,
    };
    // Section 6.1: req_id 0 means "no reply wanted", and that covers an error
    // reply too - a sender that did not ask to be told cannot be told.
    let replied = match reply {
        Some(r) if req_id != 0 => match encode_reply(&r, op, req_id) {
            Some(d) => {
                out.from_control_sock.push((from, d));
                true
            }
            None => false,
        },
        _ => false,
    };
    out.events.push(Event::Control {
        from,
        op,
        req_id,
        error,
        replied,
    });
}

/// Serialise a reply into a fresh datagram.
fn encode_reply(reply: &Reply<'_>, op: u8, req_id: u16) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let n = reply.write(op, req_id, &mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}
