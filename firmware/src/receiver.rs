//! The receive state machine: spec sections 3.3, 4.7, 6 and 7, with no I/O
//! and no clock in it.
//!
//! This is a `no_std` port of `crates/sim/src/core.rs` (card 006), which was
//! written as a pure core precisely so that the device could reuse its logic
//! instead of reinventing it. Every method takes `now_us`; the tasks in
//! [`crate::net`] own the sockets and the wall clock. Where the two differ:
//!
//! * **No allocation anywhere.** The simulator copies an accepted frame's
//!   payload into a `Vec` and decodes it at flush; here the frame task keeps
//!   the winning *datagram* in one of two static buffers and hands it back at
//!   flush, so nothing is copied and [`Core`] stores only a `bool`.
//! * **Fixed-size rate limiters.** Three `HashMap`s become three small arrays
//!   with oldest-entry eviction ([`Limiter`]). Four slots is two more than the
//!   protocol needs: only the lock holder and one hopeful can be sending.
//! * **Outgoing datagrams go in a fixed [`Outbox`]** instead of a `Vec<Vec<u8>>`.
//!   The two unsolicited packets of section 6.2 are 13 and 56 bytes.
//!
//! Everything else - the admission rule, the sequence test, what each counter
//! counts, which socket `BUSY` leaves by, the `RESET_STATS` carve-outs - is
//! the simulator's logic with its comments, because those comments record the
//! seventeen readings card 006 had to settle and they are the specification's
//! real meaning.

use core::sync::atomic::Ordering;

use embassy_net::{IpAddress, IpEndpoint};
use log::info;
use screeny_proto::control::{
    busy_reason, op, state as tstate, ErrorCode, IdleMode, Reply, Request, Telemetry,
};
use screeny_proto::txt::DeviceInfo;
use screeny_proto::{dec, ControlPacket, FramePacket, Rgb888Frame};
use screeny_proto::{C_REPLY, MAX_UDP_PAYLOAD, TYPE_CONTROL, VERSION};

use crate::display::{self, Frame};

/// Longest `GET_INFO` / TXT record we will build. The v1 key set with a
/// 32-byte name is about 130 bytes.
pub const INFO_MAX: usize = 224;

/// Largest unsolicited datagram: a 48-byte `TELEMETRY` body plus the header.
const OUT_MAX: usize = 64;

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

fn queue(out: &mut Outbox, to: IpEndpoint, reply: &Reply<'_>, opcode: u8) {
    let mut item = Out {
        to,
        buf: [0u8; OUT_MAX],
        len: 0,
    };
    // req_id 0: both packets of section 6.2 are unsolicited.
    if let Ok(n) = reply.write(opcode, 0, &mut item.buf) {
        item.len = n;
        let _ = out.push(item);
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The stream state of section 7.3, ignoring overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Showing the idle screen; no active source.
    Idle,
    /// Streaming; a source holds the lock.
    Live,
    /// The last frame is still lit but the lock has been released.
    Hold,
}

impl State {
    /// The telemetry `state` byte.
    pub fn as_u8(self) -> u8 {
        match self {
            State::Idle => tstate::IDLE,
            State::Live => tstate::LIVE,
            State::Hold => tstate::HOLD,
        }
    }

    fn name(self) -> &'static str {
        match self {
            State::Idle => "IDLE",
            State::Live => "LIVE",
            State::Hold => "HOLD",
        }
    }
}

/// What [`Core::offer_frame`] wants the caller to do with the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Keep it: it is the newest acceptable frame of this drain so far.
    Keep,
    /// Forget it. It was rejected, stale, or malformed, and counted.
    Drop,
}

// ---------------------------------------------------------------------------
// Rate limiters
// ---------------------------------------------------------------------------

/// "At most one packet per `min_ms` per source", with a fixed table.
///
/// When the table is full the oldest entry is evicted, which can only ever
/// make the device *more* generous to a source it has forgotten. That is the
/// right direction to fail: the alternative, refusing to track a new source,
/// would silence a `BUSY` that tells a sender why its frames are vanishing.
struct Limiter<const N: usize> {
    key: [Option<IpEndpoint>; N],
    at: [u64; N],
}

impl<const N: usize> Limiter<N> {
    const fn new() -> Self {
        Limiter {
            key: [None; N],
            at: [0; N],
        }
    }

    /// True if a packet may go to `k` now, recording it if so.
    fn allow(&mut self, k: IpEndpoint, now_us: u64, min_us: u64) -> bool {
        let mut free: Option<usize> = None;
        let mut oldest = 0usize;
        for i in 0..N {
            match self.key[i] {
                Some(e) if e == k => {
                    if now_us.saturating_sub(self.at[i]) < min_us {
                        return false;
                    }
                    self.at[i] = now_us;
                    return true;
                }
                None => {
                    free.get_or_insert(i);
                }
                Some(_) => {
                    if self.at[i] < self.at[oldest] {
                        oldest = i;
                    }
                }
            }
        }
        let i = free.unwrap_or(oldest);
        self.key[i] = Some(k);
        self.at[i] = now_us;
        true
    }
}

/// Section 5.5's `GET_INFO` limiter: keyed by source *address*, and with the
/// retransmission exemption, so it remembers the `req_id` it answered.
struct InfoLimiter<const N: usize> {
    key: [Option<IpAddress>; N],
    at: [u64; N],
    req: [u16; N],
}

impl<const N: usize> InfoLimiter<N> {
    const fn new() -> Self {
        InfoLimiter {
            key: [None; N],
            at: [0; N],
            req: [0; N],
        }
    }

    fn allow(&mut self, ip: IpAddress, now_us: u64, min_us: u64, req_id: u16) -> bool {
        let mut free = None;
        let mut oldest = 0usize;
        for i in 0..N {
            match self.key[i] {
                Some(e) if e == ip => {
                    // A repeat of a req_id we already answered inside the
                    // window is section 6.1's retransmission, not a second
                    // request, so it is answered again.
                    let repeat = req_id != 0 && req_id == self.req[i];
                    if now_us.saturating_sub(self.at[i]) < min_us && !repeat {
                        return false;
                    }
                    self.at[i] = now_us;
                    self.req[i] = req_id;
                    return true;
                }
                None => {
                    free.get_or_insert(i);
                }
                Some(_) => {
                    if self.at[i] < self.at[oldest] {
                        oldest = i;
                    }
                }
            }
        }
        let i = free.unwrap_or(oldest);
        self.key[i] = Some(ip);
        self.at[i] = now_us;
        self.req[i] = req_id;
        true
    }
}

// ---------------------------------------------------------------------------
// The core
// ---------------------------------------------------------------------------

/// The receiver, minus its sockets, its clock and its panel.
pub struct Core {
    // --- identity and settings -----------------------------------------
    name: heapless::String<32>,
    id: heapless::String<8>,
    /// `GET_INFO` body and mDNS TXT record: the same bytes (section 6.6).
    info: heapless::Vec<u8, INFO_MAX>,
    brightness: u8,
    idle_mode: IdleMode,

    // --- stream state ---------------------------------------------------
    state: State,
    active_source: Option<IpEndpoint>,
    last_frame_at_us: u64,
    hold_since_us: u64,
    idle_since_us: u64,
    last_seq: u16,
    have_last_seq: bool,

    // --- overlays -------------------------------------------------------
    identify_until_us: Option<u64>,

    // --- counters -------------------------------------------------------
    stats: crate::rxstats::Stats,
    last_codec: u8,

    // --- rate limiters --------------------------------------------------
    busy_sent_at: Limiter<4>,
    telemetry_sent_at: Limiter<4>,
    info_sent_at: InfoLimiter<4>,

    // --- the current drain ----------------------------------------------
    /// Whether the caller is holding a survivor datagram for us.
    pending: bool,
    stats_req: heapless::Vec<IpEndpoint, 2>,

    // --- things the tasks watch -----------------------------------------
    /// Bumped whenever the panel content must be recomposed.
    pub redraw: u32,
    /// Set by `REBOOT`; the control task sends the reply, then resets.
    pub reboot_pending: bool,
    /// Set when `SET_NAME` changed the TXT record, so mDNS re-announces.
    pub info_changed: bool,
}

impl Core {
    /// Build a receiver. `id` is the lowercase MAC suffix of section 5.1.
    pub fn new(id: &str) -> Self {
        let mut c = Core {
            name: heapless::String::new(),
            id: heapless::String::new(),
            info: heapless::Vec::new(),
            brightness: display::DEFAULT_BRIGHTNESS,
            idle_mode: IdleMode::Status,
            state: State::Idle,
            active_source: None,
            last_frame_at_us: 0,
            hold_since_us: 0,
            idle_since_us: 0,
            last_seq: 0,
            have_last_seq: false,
            identify_until_us: None,
            stats: crate::rxstats::Stats::default(),
            last_codec: 0,
            busy_sent_at: Limiter::new(),
            telemetry_sent_at: Limiter::new(),
            info_sent_at: InfoLimiter::new(),
            pending: false,
            stats_req: heapless::Vec::new(),
            redraw: 1,
            reboot_pending: false,
            info_changed: false,
        };
        let _ = c.id.push_str(id);
        // Section 5.1: the default instance name is `screeny-<id>`.
        let _ = c.name.push_str("screeny-");
        let _ = c.name.push_str(id);
        c.rebuild_info();
        c
    }

    // -- accessors -------------------------------------------------------

    /// The telemetry `state` byte. Section 7.3: `IDENTIFY` is an overlay, not
    /// a stream state, and it is what the byte reports while it is up.
    pub fn state_byte(&self) -> u8 {
        if self.identify_until_us.is_some() {
            tstate::IDENTIFY
        } else {
            self.state.as_u8()
        }
    }

    /// The friendly name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `GET_INFO` / TXT bytes (section 6.6).
    pub fn info_bytes(&self) -> &[u8] {
        &self.info
    }

    /// Assemble the telemetry struct of section 6.7.
    pub fn telemetry(&self, now_us: u64) -> Telemetry {
        let mut t = self.stats.telemetry(
            (now_us / 1_000) as u32,
            crate::RSSI_DBM.load(Ordering::Relaxed),
            self.brightness,
            self.state_byte(),
            self.last_codec,
        );
        // Byte 42 is measured on core 1, by the task that does the pushing,
        // so it lives in an atomic rather than in `Stats`. `RESET_STATS`
        // zeroes it along with everything else.
        t.render_us_max = crate::RENDER_US_MAX_PROTO
            .load(Ordering::Relaxed)
            .min(u16::MAX as u32) as u16;
        t
    }

    /// Count a datagram too long to have been read whole (section 1).
    pub fn count_oversize(&mut self) {
        Counters::bump(&mut self.stats.counters.frames_rejected);
    }

    /// Take the "the TXT record changed" flag, for the mDNS re-announce.
    pub fn take_info_changed(&mut self) -> bool {
        core::mem::take(&mut self.info_changed)
    }

    fn rebuild_info(&mut self) {
        let info = DeviceInfo {
            codecs: dec::CODECS_TXT,
            ctrl: crate::CONTROL_PORT,
            fw: crate::FW_VERSION,
            id: &self.id,
            name: &self.name,
            ..DeviceInfo::DEFAULT
        };
        let mut buf = [0u8; INFO_MAX];
        let n = info.write(&mut buf).unwrap_or(0);
        self.info.clear();
        let _ = self.info.extend_from_slice(&buf[..n]);
    }

    // -----------------------------------------------------------------
    // Frame port
    // -----------------------------------------------------------------

    /// Offer one datagram drained from the frame socket.
    ///
    /// Applies section 2's validation, section 7.4's admission rule and
    /// section 3.2's sequence test. [`Offer::Keep`] means this datagram is the
    /// drain's survivor so far and the caller must hold on to it; an earlier
    /// survivor it thereby throws away has already been counted in
    /// `frames_dropped_superseded` here. Nothing is decoded or displayed until
    /// [`Core::flush_frames`].
    pub fn offer_frame(
        &mut self,
        now_us: u64,
        from: IpEndpoint,
        data: &[u8],
        out: &mut Outbox,
    ) -> Offer {
        // Section 1 (and card 006 item 29): a datagram longer than the receive
        // buffer arrives truncated, so the bytes we have are not the bytes
        // that were sent. Discard and count; do not parse the prefix.
        if data.len() > MAX_UDP_PAYLOAD {
            Counters::bump(&mut self.stats.counters.frames_rejected);
            return Offer::Drop;
        }
        let pkt = match FramePacket::parse(data) {
            Ok(p) => p,
            Err(_) => {
                // Section 2: bad magic, version, type or length - and a
                // CONTROL that arrived on the frame port. Section 2.2 counts
                // all of it in frames_rejected, which is frame-port only.
                Counters::bump(&mut self.stats.counters.frames_rejected);
                return Offer::Drop;
            }
        };

        // --- section 7.4, frame admission -------------------------------
        match self.active_source {
            None => self.adopt(now_us, from),
            Some(s) if s == from => {}
            Some(_) => {
                let since = now_us.saturating_sub(self.last_frame_at_us) / 1_000;
                if since >= screeny_proto::LOCK_MS as u64 {
                    self.adopt(now_us, from); // takeover
                } else {
                    Counters::bump(&mut self.stats.counters.frames_rejected);
                    let remaining = screeny_proto::LOCK_MS as u64 - since;
                    self.send_busy(now_us, from, remaining as u32, out);
                    return Offer::Drop;
                }
            }
        }

        // The source is allowed to drive the panel, so a STATS_REQ on this
        // frame gets answered whatever becomes of the pixels (section 6.4):
        // the one frame a second a sender marks is exactly the frame it must
        // not lose track of when something has gone wrong.
        if pkt.wants_stats() && !self.stats_req.contains(&from) {
            let _ = self.stats_req.push(from);
        }

        // --- section 3.2, sequence numbers ------------------------------
        let n = if self.have_last_seq {
            if !screeny_proto::newer(pkt.seq, self.last_seq) {
                Counters::bump(&mut self.stats.counters.frames_dropped_stale);
                return Offer::Drop;
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
        if self.pending {
            Counters::bump(&mut self.stats.counters.frames_dropped_superseded);
        }
        self.pending = true;
        Offer::Keep
    }

    /// Close the drain: decode the survivor into `back`, then answer any
    /// `STATS_REQ` seen during it.
    ///
    /// `survivor` must be the datagram of the last [`Offer::Keep`], or `None`
    /// if there was none. Returns `true` if `back` now holds a new frame that
    /// the caller should publish (section 4.7: swap only on success).
    pub fn flush_frames(
        &mut self,
        now_us: u64,
        survivor: Option<&[u8]>,
        back: &mut Rgb888Frame,
        out: &mut Outbox,
    ) -> bool {
        let mut shown = false;
        if self.pending {
            self.pending = false;
            // Re-parsing costs eight byte loads and keeps the payload out of
            // `Core`, which is what lets this crate allocate nothing.
            if let Some(pkt) = survivor.and_then(|d| FramePacket::parse(d).ok()) {
                let t0 = esp_hal::time::Instant::now();
                let result = dec::decode(pkt.codec, pkt.payload, back);
                self.stats.timings.decode(t0.elapsed().as_micros() as u32);
                match result {
                    Ok(()) => {
                        Counters::bump(&mut self.stats.counters.frames_shown);
                        self.last_codec = pkt.codec;
                        shown = true;
                        // Section 7.4: the lock goes when a FINAL frame is
                        // *displayed*. One that failed to decode was never
                        // displayed and releases nothing, so a single damaged
                        // packet cannot hand the panel to a stranger.
                        if pkt.is_final() {
                            self.release(now_us, "FINAL");
                        }
                    }
                    Err(_) => {
                        // Section 4.7: discard, count, leave the last frame lit.
                        Counters::bump(&mut self.stats.counters.frames_dropped_decode);
                    }
                }
            }
        }

        // Section 3.1: "after processing this frame, send one TELEMETRY
        // reply". After, so the counters the sender reads already include it.
        while let Some(to) = self.stats_req.pop() {
            self.send_telemetry(now_us, to, out);
        }
        shown
    }

    /// Advance the timers: `LIVE -> HOLD`, `HOLD -> IDLE`, `IDENTIFY` expiry.
    pub fn tick(&mut self, now_us: u64) {
        if let Some(until) = self.identify_until_us {
            if now_us >= until {
                self.identify_until_us = None;
                self.redraw = self.redraw.wrapping_add(1);
            }
        }
        match self.state {
            State::Live => {
                let idle_for = now_us.saturating_sub(self.last_frame_at_us) / 1_000;
                if idle_for >= screeny_proto::STREAM_TIMEOUT_MS as u64 {
                    self.release(now_us, "stream timeout");
                }
            }
            State::Hold => {
                // Section 7.5, mode 1: stay in HOLD, never fade.
                if self.idle_mode == IdleMode::HoldForever {
                    return;
                }
                let held = now_us.saturating_sub(self.hold_since_us) / 1_000;
                if held >= screeny_proto::HOLD_MS as u64 {
                    self.set_state(State::Idle, now_us);
                }
            }
            State::Idle => {}
        }
    }

    /// Section 7.3's "any -> Wi-Fi link down -> HOLD".
    pub fn link_down(&mut self, now_us: u64) {
        if self.state == State::Live {
            self.release(now_us, "wifi down");
        }
    }

    fn adopt(&mut self, now_us: u64, from: IpEndpoint) {
        self.active_source = Some(from);
        // Sections 7.3 and 6.8: reset last_seq and the jitter EWMAs, not the
        // counters.
        self.have_last_seq = false;
        self.stats.arrival.on_source_change();
        info!("lock: taken by {}", from);
        self.set_state(State::Live, now_us);
    }

    fn release(&mut self, now_us: u64, why: &str) {
        if let Some(s) = self.active_source.take() {
            info!("lock: released ({}) held by {}", why, s);
        }
        self.have_last_seq = false;
        self.stats.arrival.on_source_change();
        if self.state != State::Idle {
            self.set_state(State::Hold, now_us);
        }
    }

    fn set_state(&mut self, to: State, now_us: u64) {
        if self.state == to {
            return;
        }
        info!("state: {} -> {}", self.state.name(), to.name());
        self.state = to;
        if to == State::Hold {
            self.hold_since_us = now_us;
        }
        if to == State::Idle {
            self.idle_since_us = now_us;
        }
        self.redraw = self.redraw.wrapping_add(1);
    }

    // -----------------------------------------------------------------
    // Unsolicited device packets (section 6.2)
    // -----------------------------------------------------------------

    fn send_busy(&mut self, now_us: u64, to: IpEndpoint, remaining_ms: u32, out: &mut Outbox) {
        let min = screeny_proto::BUSY_MIN_INTERVAL_MS as u64 * 1_000;
        if self.busy_sent_at.allow(to, now_us, min) {
            queue(
                out,
                to,
                &Reply::Busy {
                    reason: busy_reason::LOCKED,
                    lock_holder_ms_remaining: remaining_ms,
                },
                op::BUSY,
            );
        }
    }

    fn send_telemetry(&mut self, now_us: u64, to: IpEndpoint, out: &mut Outbox) {
        let min = screeny_proto::TELEMETRY_MIN_INTERVAL_MS as u64 * 1_000;
        if self.telemetry_sent_at.allow(to, now_us, min) {
            queue(out, to, &Reply::Telemetry(self.telemetry(now_us)), op::TELEMETRY);
        }
    }

    // -----------------------------------------------------------------
    // Control port (section 6)
    // -----------------------------------------------------------------

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
        let pkt = match ControlPacket::parse(data) {
            Ok(p) => p,
            Err(reason) => return self.control_reject(data, reason, out),
        };

        // Section 6.1 tells requests and replies apart by the REPLY bit and by
        // nothing else, so a device that answered replies would answer its own
        // and two of them on one LAN would talk to each other indefinitely.
        if pkt.flags & C_REPLY != 0 {
            return None;
        }
        let req_id = pkt.req_id;

        if pkt.op == OP_BENCH {
            return self.bench(pkt.body, req_id, out);
        }

        // Section 5.5's rate limit, applied before the request is carried out
        // rather than after, because a suppressed GET_INFO must leave no
        // trace. GET_INFO has an empty body, so this does not pre-empt the
        // ERR_BAD_LENGTH a malformed one still deserves.
        if pkt.op == op::GET_INFO && pkt.body.is_empty() {
            const INFO_MIN_US: u64 = 1_000_000; // section 5.5: one per source per second
            if !self.info_sent_at.allow(from.addr, now_us, INFO_MIN_US, req_id) {
                return None;
            }
            if req_id == 0 {
                return None;
            }
            return Reply::Info(&self.info).write(op::GET_INFO, req_id, out).ok();
        }

        let reply = match Request::decode(pkt.op, pkt.body) {
            Ok(req) => self.apply(now_us, from, req),
            Err(code) => Reply::Err { code: code.as_u8() },
        };
        // Section 6.1: req_id 0 means "no reply wanted", and that covers an
        // error reply too - a sender that did not ask to be told cannot be
        // told. The request itself has still been carried out.
        if req_id == 0 {
            return None;
        }
        reply.write(pkt.op, req_id, out).ok()
    }

    /// A datagram the control socket could not parse.
    ///
    /// Two of section 2's rejects have an answer rather than silence, and both
    /// are built out of header bytes that are certainly present: section 6.5's
    /// `ERR_BAD_LENGTH` for a `len` the datagram does not back up, and section
    /// 2.2's optional `ERR_VERSION`.
    fn control_reject(
        &mut self,
        data: &[u8],
        reason: screeny_proto::Reject,
        out: &mut [u8],
    ) -> Option<usize> {
        use screeny_proto::Reject;
        let code = match reason {
            Reject::BadLength => ErrorCode::BadLength,
            Reject::BadVersion => ErrorCode::Version,
            _ => return None,
        };
        let h = screeny_proto::peek(data)?;
        if h.ty != TYPE_CONTROL || h.id == 0 {
            return None;
        }
        if reason == Reject::BadVersion && h.version == VERSION {
            return None;
        }
        // A reply to a reply would be a loop; a device never starts one.
        if h.flags & C_REPLY != 0 {
            return None;
        }
        Reply::Err {
            code: code.as_u8(),
        }
        .write(h.b2, h.id, out)
        .ok()
    }

    /// Carry out a decoded request and say what to reply with.
    fn apply(&mut self, now_us: u64, from: IpEndpoint, req: Request<'_>) -> Reply<'static> {
        match req {
            Request::Ping => Reply::Ping {
                uptime_ms: (now_us / 1_000) as u32,
            },
            // Handled above, where the rate limit lives.
            Request::GetInfo => Reply::Err {
                code: ErrorCode::BadLength.as_u8(),
            },
            // A TELEMETRY request on the control port is solicited, so section
            // 6.2's 100 ms limit - which governs the *unsolicited* reply to a
            // STATS_REQ frame - does not apply to it.
            Request::Telemetry => Reply::Telemetry(self.telemetry(now_us)),
            Request::SetBrightness(level) => {
                let applied = level.min(crate::BRIGHTNESS_CAP);
                if applied != self.brightness {
                    self.brightness = applied;
                    crate::BRIGHTNESS.store(applied, Ordering::Relaxed);
                    crate::OE_OVERRIDE.store(u8::MAX, Ordering::Relaxed);
                    crate::BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
                    info!(
                        "control: brightness {} -> {} ({} OE slots)",
                        level,
                        applied,
                        display::slots_for(applied)
                    );
                }
                Reply::Brightness { applied }
            }
            Request::Identify { duration_ms } => {
                self.identify_until_us = if duration_ms == 0 {
                    None
                } else {
                    Some(now_us + duration_ms as u64 * 1_000)
                };
                self.redraw = self.redraw.wrapping_add(1);
                Reply::Identify
            }
            Request::SetIdle(mode) => {
                self.idle_mode = mode;
                self.redraw = self.redraw.wrapping_add(1);
                info!("control: idle mode {}", mode.as_u8());
                Reply::Idle {
                    mode: mode.as_u8(),
                }
            }
            Request::ResetStats => {
                self.stats.reset();
                crate::RENDER_US_MAX_PROTO.store(0, Ordering::Relaxed);
                Reply::ResetStats
            }
            Request::Release => {
                // Section 7.4 matches on the IP alone, and it has to: RELEASE
                // arrives on the control port, so its source *port* is never
                // the frame stream's. A RELEASE from anyone else is an
                // ordinary acknowledgement that changes nothing.
                if self.active_source.map(|s| s.addr) == Some(from.addr) {
                    self.release(now_us, "RELEASE");
                }
                Reply::Release
            }
            Request::SetName(name) => {
                self.name.clear();
                if self.name.push_str(name).is_err() {
                    return Reply::Err {
                        code: ErrorCode::BadArg.as_u8(),
                    };
                }
                self.rebuild_info();
                self.info_changed = true;
                self.redraw = self.redraw.wrapping_add(1);
                info!("control: name is now {:?}", self.name.as_str());
                Reply::SetName
            }
            Request::GetWifi => Reply::Wifi {
                ssid: crate::SSID,
                state: crate::WIFI_STATE.load(Ordering::Relaxed),
            },
            // Card 014 owns credential storage and the rejoin sequence. Until
            // then the honest answer is section 6.5's "op disabled in this
            // build" - the spec has no ERR_UNSUPPORTED, and inventing one
            // would put a byte on the wire that no sender could interpret.
            Request::SetWifi(_) => Reply::Err {
                code: ErrorCode::NotPermitted.as_u8(),
            },
            Request::Reboot => {
                self.reboot_pending = true;
                Reply::Reboot
            }
        }
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
                crate::OE_OVERRIDE_DEADLINE_MS.store(
                    crate::now_ms().wrapping_add(10_000),
                    Ordering::Relaxed,
                );
                crate::BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
            }
            b'P' => {
                crate::PATTERN_HOLD.store(if arg == 255 { 0 } else { arg + 1 }, Ordering::Relaxed);
                self.redraw = self.redraw.wrapping_add(1);
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
}

use crate::rxstats::Counters;

// ---------------------------------------------------------------------------
// What the panel should show
// ---------------------------------------------------------------------------

/// What the frame task should compose, given the state machine and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Leave the streamed frame alone; the panel already shows it.
    Stream,
    /// Draw the `IDENTIFY` overlay.
    Identify,
    /// Cross-fade from the last streamed frame to the idle target, section
    /// 7.5. `t` is 0..=256.
    Fade { t: u16 },
    /// The idle screen for the mode in force, fully faded in.
    Idle,
}

impl Core {
    /// What the panel should show now.
    pub fn intent(&self, now_us: u64) -> Intent {
        if self.identify_until_us.is_some() {
            return Intent::Identify;
        }
        match self.state {
            State::Live | State::Hold => Intent::Stream,
            State::Idle => {
                let since = now_us.saturating_sub(self.idle_since_us) / 1_000;
                let fade = screeny_proto::FADE_MS as u64;
                if since < fade {
                    Intent::Fade {
                        t: ((since * 256) / fade) as u16,
                    }
                } else {
                    Intent::Idle
                }
            }
        }
    }

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
        match self.idle_mode {
            IdleMode::Status | IdleMode::HoldForever => {
                crate::screens::status(dst, &self.name, hostname, net, phase)
            }
            IdleMode::Dim => {
                dst.copy_from(last);
                dst.scale(26); // 10%
            }
            IdleMode::Black => dst.clear(),
        }
    }
}
