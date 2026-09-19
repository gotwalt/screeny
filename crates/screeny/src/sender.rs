//! The paced sender: one datagram per frame, a monotonic clock, and telemetry
//! feeding back into the frame rate and the codec set.
//!
//! ```text
//! FrameSource -> Encoder (encode 3 ways, decode, score, hysteresis)
//!             -> pacer (absolute 33.33 ms schedule, skip never burst)
//!             -> UDP unicast
//!             <- piggybacked TELEMETRY (STATS_REQ once a second) -> adapt
//! ```
//!
//! Spec 9.1 is the whole of the pacing design and it is worth restating: the
//! schedule is **absolute**, so error cannot accumulate, and when the sender
//! falls behind it *skips* frames rather than bursting to catch up. The device
//! shows newest-wins and queues about five frames below the socket, so a
//! catch-up burst would only fill those queues and add latency to every
//! subsequent frame.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use screeny_proto::control::Telemetry;
use screeny_proto::dec::codec;
use screeny_proto::{
    FramePacket, IndexedFrame, Packet, Rgb888Frame, F_FINAL, F_KEY, F_STATS_REQ, MAX_PIXEL_PAYLOAD,
    MAX_UDP_PAYLOAD, NPIX,
};

use crate::device::Device;
use crate::encode::{EncodeConfig, Encoder, Profile, MIN_BUDGET};
use crate::error::{Error, Result};
use crate::frame::{Frame, FrameSource, FrameTime, Pixels};
use crate::net;

/// What became of a frame handed to [`Sender::send`] or [`crate::Link::send`].
///
/// Returned rather than logged, because the two ways a frame can *not* reach
/// the panel - the cadence ceiling and a link that is down - are both normal
/// and both worth a producer knowing about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// On the wire.
    Frame {
        /// Codec id in the header (spec 4).
        codec: u8,
        /// Pixel payload bytes, not counting the 8-byte header.
        bytes: usize,
        /// True if the panel will show exactly these pixels. False means the
        /// chooser had to quantise: see [`Sender::send_indexed`].
        exact: bool,
        /// Sequence number this frame went out with.
        seq: u16,
    },
    /// Dropped by the cadence ceiling: it arrived sooner after the previous
    /// one than the device's current frame rate allows, and the device would
    /// only have superseded it. See [`crate::Cadence`].
    Coalesced,
    /// Dropped because the link is down and reconnecting. Not an error: a
    /// sender that runs for weeks spends some of them with nowhere to send
    /// (see [`crate::Link`]).
    Dropped,
}

impl Sent {
    /// True if the frame reached the wire.
    #[must_use]
    pub fn is_sent(&self) -> bool {
        matches!(self, Sent::Frame { .. })
    }

    /// The codec, if it was sent.
    #[must_use]
    pub fn codec(&self) -> Option<u8> {
        match self {
            Sent::Frame { codec, .. } => Some(*codec),
            _ => None,
        }
    }

    /// True if the panel will show exactly the pixels that were handed over.
    /// A frame that never left is not exact.
    #[must_use]
    pub fn exact(&self) -> bool {
        matches!(self, Sent::Frame { exact: true, .. })
    }

    /// Payload bytes, zero for a frame that never left.
    #[must_use]
    pub fn bytes(&self) -> usize {
        match self {
            Sent::Frame { bytes, .. } => *bytes,
            _ => 0,
        }
    }
}

/// Frame rates the adaptation ladder steps through, as fractions of the
/// requested rate: 30 -> 24 -> 20 -> 15 fps (spec 6.9).
pub const FPS_LADDER: [f64; 4] = [1.0, 0.8, 2.0 / 3.0, 0.5];

/// Codecs whose decode is cheap enough to fall back to when the device is
/// decode-limited. `PAL5` and `BC1_DUAL` are fixed-rate and have no LZ pass
/// (card 002 measured them at roughly a third of `PAL8_LZ`'s instruction
/// count).
pub const CHEAP_CODECS: [u8; 3] = [codec::PAL5, codec::BC1_DUAL, codec::SOLID];

/// Consecutive lossy seconds before the frame rate steps down (spec 6.9).
const BAD_SECONDS: u32 = 3;
/// Consecutive clean seconds before it steps back up (spec 6.9).
const GOOD_SECONDS: u32 = 10;

/// How to stream.
#[derive(Clone, Debug)]
pub struct SenderConfig {
    /// Target frame rate before any adaptation.
    pub fps: f64,
    /// Pixel-payload budget. `None` takes the device's advertised `mtu`.
    pub budget: Option<usize>,
    /// Encoder speed/quality profile.
    pub profile: Profile,
    /// Ask for telemetry roughly this often (spec 6.4: at most one frame in
    /// 100 ms may carry `STATS_REQ`).
    pub stats_interval: Duration,
    /// Apply spec 6.9's adaptation rules.
    pub adapt: bool,
    /// Set `HAS_TS` and stamp each frame with a microsecond sender clock.
    pub timestamps: bool,
    /// Ask for interactive-video QoS (untested on the bench AP; card 013).
    pub qos: bool,
    /// Confirm liveness and learn the codec list with `GET_INFO` before
    /// streaming (spec 9.4 step 2).
    pub handshake: bool,
}

impl Default for SenderConfig {
    fn default() -> Self {
        SenderConfig {
            fps: 30.0,
            budget: None,
            profile: Profile::default(),
            stats_interval: Duration::from_secs(1),
            adapt: true,
            timestamps: false,
            qos: false,
            handshake: true,
        }
    }
}

/// Running totals for a stream.
#[derive(Clone, Debug, Default)]
pub struct SendStats {
    /// Frames put on the wire.
    pub frames_sent: u64,
    /// Frames the schedule passed over because encoding or rendering ran
    /// long. Spec 9.1: skipping is correct, bursting is not.
    pub frames_skipped: u64,
    /// Pixel payload bytes sent.
    pub bytes: u64,
    /// How many frames each codec carried.
    pub by_codec: BTreeMap<u8, u64>,
    /// Total time spent in the encoder.
    pub encode_total: Duration,
    /// Slowest single encode.
    pub encode_max: Duration,
    /// Encode times in 0.5 ms buckets, for a percentile without keeping every
    /// sample.
    pub encode_hist: Vec<u32>,
    /// Smallest gap between two consecutive sends. A burst would show up here
    /// as a gap far below the frame period.
    pub min_gap: Option<Duration>,
    /// Largest gap between two consecutive sends.
    pub max_gap: Option<Duration>,
    /// Frame rate currently targeted.
    pub fps: f64,
    /// How many times adaptation changed the frame rate.
    pub fps_changes: u32,
    /// Most recent telemetry from the device.
    pub telemetry: Option<Telemetry>,
    /// `BUSY` packets received: another sender holds the lock (spec 7.4).
    pub busy: u32,
    /// Telemetry intervals in which the device failed to decode something.
    pub decode_failures: u32,
    /// Codecs withdrawn because the device could not decode them.
    pub codecs_withdrawn: Vec<u8>,
    /// True while the codec set is reduced because the device is
    /// decode-limited.
    pub codec_limited: bool,
    /// Indexed frames that went on the wire exactly, palette and all.
    pub indexed_exact: u64,
    /// Indexed frames that could **not** be encoded exactly inside the budget
    /// and were expanded and run through the lossy chooser instead. Spec 4.8
    /// allows it; an art system that sees this rise is sending more than 32
    /// colours, or index noise its palette cannot compress.
    pub indexed_fallback: u64,
    /// Colour count of the most recent frame that took that fallback, so a
    /// log line can say *why* without keeping every frame.
    pub last_fallback_colours: usize,
    /// When the stream started.
    pub started: Option<Instant>,
}

impl SendStats {
    /// Frames per second actually achieved so far.
    #[must_use]
    pub fn actual_fps(&self) -> f64 {
        match self.started {
            Some(t) if self.frames_sent > 0 => {
                let s = t.elapsed().as_secs_f64();
                if s > 0.0 {
                    self.frames_sent as f64 / s
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }

    /// Mean payload size.
    #[must_use]
    pub fn mean_bytes(&self) -> f64 {
        if self.frames_sent == 0 {
            0.0
        } else {
            self.bytes as f64 / self.frames_sent as f64
        }
    }

    /// Mean encode time.
    #[must_use]
    pub fn mean_encode(&self) -> Duration {
        if self.frames_sent == 0 {
            Duration::ZERO
        } else {
            self.encode_total / self.frames_sent as u32
        }
    }

    /// Encode time at the given percentile, to the nearest 0.5 ms.
    #[must_use]
    pub fn encode_pct(&self, pct: f64) -> Duration {
        let total: u32 = self.encode_hist.iter().sum();
        if total == 0 {
            return Duration::ZERO;
        }
        let want = (total as f64 * pct).ceil() as u32;
        let mut acc = 0;
        for (i, n) in self.encode_hist.iter().enumerate() {
            acc += n;
            if acc >= want {
                return Duration::from_micros((i as u64 + 1) * 500);
            }
        }
        Duration::from_micros(self.encode_hist.len() as u64 * 500)
    }

    fn record_encode(&mut self, d: Duration) {
        self.encode_total += d;
        self.encode_max = self.encode_max.max(d);
        let bucket = (d.as_micros() / 500) as usize;
        if self.encode_hist.len() <= bucket {
            self.encode_hist.resize(bucket + 1, 0);
        }
        self.encode_hist[bucket] += 1;
    }
}

/// Telemetry-driven adaptation state (spec 6.9).
struct Adapt {
    prev: Option<Telemetry>,
    sent_at_prev: u64,
    bad: u32,
    good: u32,
    rung: usize,
}

/// A connected, paced sender.
pub struct Sender {
    device: Device,
    cfg: SenderConfig,
    sock: UdpSocket,
    enc: Encoder,
    my_codecs: Vec<u8>,
    budget: usize,
    seq: u16,
    stats: SendStats,
    adapt: Adapt,
    epoch: Instant,
    buf: Vec<u8>,
    rx: Vec<u8>,
    last_payload: Option<(u8, Vec<u8>)>,
    last_send: Option<Instant>,
    last_stats_req: Option<Instant>,
    last_rx: Instant,
    idx: Box<[u8; NPIX]>,
}

impl Sender {
    /// Connect to a device and negotiate the codec set.
    ///
    /// With `cfg.handshake` set this sends `GET_INFO` first, because a cached
    /// TXT record can be stale and because a reply proves the device is alive
    /// before a single frame is sent (spec 9.4 step 2).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the socket cannot be set up, [`Error::Timeout`] if the
    /// handshake gets no reply, [`Error::NoCommonCodec`] if the device and
    /// this sender share no codec, [`Error::Budget`] for an impossible budget.
    pub fn connect(mut device: Device, cfg: SenderConfig) -> Result<Self> {
        if cfg.handshake {
            let mut ctl = crate::control::ControlClient::connect(device.control)?;
            let info = ctl.info()?;
            device.apply(info);
        }

        let my_codecs: Vec<u8> = screeny_proto::dec::SUPPORTED_CODECS.to_vec();
        let (codecs, dev_budget) = match &device.info {
            Some(i) => {
                let common = i.common_codecs(&my_codecs);
                if common.is_empty() {
                    return Err(Error::NoCommonCodec(
                        i.codecs
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                    ));
                }
                (common, i.budget())
            }
            // No handshake: assume a v1 device, which must support all five.
            None => (my_codecs.clone(), MAX_PIXEL_PAYLOAD),
        };

        let budget = cfg.budget.unwrap_or(dev_budget).min(MAX_PIXEL_PAYLOAD);
        if !(screeny_proto::dec::SOLID_LEN..=MAX_PIXEL_PAYLOAD).contains(&budget) {
            return Err(Error::Budget {
                got: budget,
                max: MAX_PIXEL_PAYLOAD,
            });
        }

        let sock = net::connected_socket(device.frame, true, cfg.qos)?;
        sock.set_nonblocking(true)?;

        let enc = Encoder::new(EncodeConfig {
            codecs: codecs.clone(),
            profile: cfg.profile,
            ..EncodeConfig::default()
        });

        let mut stats = SendStats {
            fps: cfg.fps,
            ..SendStats::default()
        };
        stats.by_codec.clear();

        Ok(Sender {
            device,
            budget,
            my_codecs: codecs,
            sock,
            enc,
            seq: 0,
            stats,
            adapt: Adapt {
                prev: None,
                sent_at_prev: 0,
                bad: 0,
                good: 0,
                rung: 0,
            },
            epoch: Instant::now(),
            buf: vec![0u8; MAX_UDP_PAYLOAD],
            rx: vec![0u8; MAX_UDP_PAYLOAD],
            last_payload: None,
            last_send: None,
            last_stats_req: None,
            // The handshake, if there was one, is itself proof of liveness;
            // without one, give the device the benefit of the doubt until the
            // first `STATS_REQ` has had time to come back.
            last_rx: Instant::now(),
            idx: Box::new([0u8; NPIX]),
            cfg,
        })
    }

    /// The device being streamed to.
    #[must_use]
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Running totals.
    #[must_use]
    pub fn stats(&self) -> &SendStats {
        &self.stats
    }

    /// The payload budget in force.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// True if the budget is below what the palette ladder was designed for.
    #[must_use]
    pub fn budget_is_tight(&self) -> bool {
        self.budget < MIN_BUDGET
    }

    /// The local address frames are sent from. This plus the device's view of
    /// it is the "source" the lock is held by (spec 7.1).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the socket has no local address.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr> {
        Ok(self.sock.local_addr()?)
    }

    /// Encode and send one frame. Returns what the encoder chose.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the datagram could not be sent.
    pub fn send_frame(&mut self, f: &screeny_proto::Rgb888Frame, final_frame: bool) -> Result<u8> {
        let out = self.enc.encode(f, self.budget);
        let st = self.enc.last_stats();
        self.stats.record_encode(st.elapsed);
        self.transmit(out.codec, &out.payload, final_frame)?;
        self.last_payload = Some((out.codec, out.payload));
        Ok(out.codec)
    }

    /// Encode and send one frame, in whichever form the caller has it.
    ///
    /// This is the push API's one door: a caller that owns its own loop calls
    /// it whenever it has a frame, and this handles sequence numbers, the
    /// `STATS_REQ` cadence and the header flags. It does **not** pace - see
    /// [`crate::Link`] for that, and for reconnection.
    ///
    /// An indexed frame of 32 colours or fewer is put on the wire exactly.
    /// See [`Sender::send_indexed`] for what happens when it cannot be.
    ///
    /// # Errors
    ///
    /// [`Error::Frame`] or [`Error::BadIndex`] if the frame is malformed -
    /// the caller's bug - and [`Error::Io`] if the datagram could not be sent.
    pub fn send(&mut self, px: Pixels<'_>) -> Result<Sent> {
        px.check()?;
        match px {
            Pixels::Rgb(bytes) => {
                // `check` has already pinned the length; this is the cast.
                let f: &Rgb888Frame = bytes.try_into().expect("checked above");
                let out = self.enc.encode(f, self.budget);
                let st = self.enc.last_stats();
                self.deliver(out, st.elapsed, st.exact, false)
            }
            Pixels::Indexed { palette, indices } => self.send_indexed(palette, indices),
        }
    }

    /// Send a palette and an index plane.
    ///
    /// **Exactness.** A palette of 32 colours or fewer is always carried
    /// exactly at the default budget: `PAL4_LZ` (<= 16 colours) or `PAL8_LZ`
    /// first, and if the indices are too incompressible for either, raw
    /// `PAL5`, which is fixed-rate at 1376 bytes and therefore cannot
    /// overflow. Every pixel the panel lights is `palette[index]`, with no
    /// requantisation. 33 to 256 colours are exact too *when they compress*,
    /// because only the variable-rate `PAL8_LZ` can carry them.
    ///
    /// **The fallback.** If no exact encoding fits the budget - more than 32
    /// colours and incompressible, or a budget below `PAL5`'s 1376 bytes -
    /// the frame is expanded to RGB and run through the ordinary lossy
    /// chooser, because a requantised frame beats a dropped one. That is
    /// counted in [`SendStats::indexed_fallback`] and flagged in the returned
    /// [`Sent::exact`], so a producer can see it happening and tighten its
    /// palette. Nothing about it is silent.
    ///
    /// # Errors
    ///
    /// [`Error::Frame`] if the index plane is not [`NPIX`] long or the
    /// palette is empty or over 256, [`Error::BadIndex`] if an index falls
    /// outside the palette, and [`Error::Io`] if the datagram could not be
    /// sent.
    pub fn send_indexed(&mut self, palette: &[[u8; 3]], indices: &[u8]) -> Result<Sent> {
        Pixels::indexed(palette, indices).check()?;
        if let Some((pixel, &index)) = indices
            .iter()
            .enumerate()
            .find(|(_, i)| **i as usize >= palette.len())
        {
            return Err(Error::BadIndex {
                index,
                pixel,
                palette: palette.len(),
            });
        }
        self.idx.copy_from_slice(indices);
        let f = IndexedFrame {
            palette,
            indices: &self.idx,
        };
        // Infallible now: the two things `encode_indexed` rejects - a palette
        // outside 1..=256, and an out-of-range index - were both just checked,
        // with a better error than `Corrupt` for each.
        let out = self
            .enc
            .encode_indexed(&f, self.budget)
            .map_err(|e| Error::Metadata(format!("{e:?} encoding an indexed frame")))?;
        let st = self.enc.last_stats();
        if st.exact {
            self.stats.indexed_exact += 1;
        } else {
            self.stats.indexed_fallback += 1;
            self.stats.last_fallback_colours = palette.len();
        }
        self.deliver(out, st.elapsed, st.exact, false)
    }

    /// Put an [`Encoded`] on the wire and account for it.
    fn deliver(
        &mut self,
        out: crate::encode::Encoded,
        encode: Duration,
        exact: bool,
        final_frame: bool,
    ) -> Result<Sent> {
        self.stats.record_encode(encode);
        let seq = self.seq;
        let bytes = out.payload.len();
        self.transmit(out.codec, &out.payload, final_frame)?;
        self.last_payload = Some((out.codec, out.payload));
        Ok(Sent::Frame {
            codec: out.codec,
            bytes,
            exact,
            seq,
        })
    }

    /// When the device was last heard from on the frame socket: a `TELEMETRY`
    /// reply, or a `BUSY`.
    ///
    /// UDP has no connection to lose, so this is the only evidence a sender
    /// has that the device is still there. [`crate::Link`] uses it as a
    /// watchdog; anyone driving [`Sender`] directly gets to decide for
    /// themselves.
    #[must_use]
    pub fn last_feedback(&self) -> Instant {
        self.last_rx
    }

    /// How long the device has been silent.
    #[must_use]
    pub fn silence(&self) -> Duration {
        self.last_rx.elapsed()
    }

    /// The frame rate the sender is targeting now, after spec 6.9 adaptation.
    #[must_use]
    pub fn fps(&self) -> f64 {
        self.stats.fps
    }

    /// The configuration this sender was connected with. `cfg.fps` is the
    /// rate that was *asked for*; [`Sender::fps`] is the one in force after
    /// spec 6.9's adaptation.
    #[must_use]
    pub fn config(&self) -> &SenderConfig {
        &self.cfg
    }

    /// The codec ids in play, after the device's list and any decode-limited
    /// withdrawal.
    #[must_use]
    pub fn codecs(&self) -> &[u8] {
        &self.my_codecs
    }

    /// Put an already-encoded payload on the wire.
    fn transmit(&mut self, codec: u8, payload: &[u8], final_frame: bool) -> Result<()> {
        let mut flags = F_KEY; // every v1 codec is stateless (spec 4)
        if final_frame {
            flags |= F_FINAL;
        }
        let now = Instant::now();
        // Spec 6.4: about one frame per second, never more than one in 100 ms.
        let due = self
            .last_stats_req
            .is_none_or(|t| now.duration_since(t) >= self.cfg.stats_interval);
        let allowed = self
            .last_stats_req
            .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(100));
        if due && allowed {
            flags |= F_STATS_REQ;
            self.last_stats_req = Some(now);
        }

        let pkt = FramePacket {
            codec,
            flags,
            seq: self.seq,
            timestamp_us: if self.cfg.timestamps {
                Some(self.epoch.elapsed().as_micros() as u32)
            } else {
                None
            },
            payload,
        };
        let n = pkt
            .write(&mut self.buf)
            .map_err(|e| Error::Metadata(format!("{e:?} building a frame of {} bytes", payload.len())))?;
        self.sock.send(&self.buf[..n])?;

        self.seq = self.seq.wrapping_add(1);
        self.stats.frames_sent += 1;
        self.stats.bytes += payload.len() as u64;
        *self.stats.by_codec.entry(codec).or_insert(0) += 1;
        // The `FINAL` frame goes out the moment the source ends, right behind
        // the frame before it, so it is not part of the paced stream and does
        // not belong in the gap statistics.
        if let (Some(prev), false) = (self.last_send, final_frame) {
            let gap = now.duration_since(prev);
            self.stats.min_gap = Some(self.stats.min_gap.map_or(gap, |m| m.min(gap)));
            self.stats.max_gap = Some(self.stats.max_gap.map_or(gap, |m| m.max(gap)));
        }
        self.last_send = Some(now);
        Ok(())
    }

    /// Send a `FINAL` frame so the device releases the lock at once instead of
    /// waiting out `STREAM_TIMEOUT_MS` (spec 7.4, 9.4 step 5).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the datagram could not be sent.
    pub fn finish(&mut self) -> Result<()> {
        if let Some((codec, payload)) = self.last_payload.take() {
            self.transmit(codec, &payload, true)?;
        }
        Ok(())
    }

    /// Drain anything the device sent back on the frame socket.
    ///
    /// Spec 6.4: a `TELEMETRY` reply arrives from the device's **frame** port,
    /// which is the one case where a `CONTROL` packet appears there.
    pub fn poll_feedback(&mut self) {
        loop {
            // A failure here is WouldBlock, or a transient ICMP error.
            let Ok(n) = self.sock.recv(&mut self.rx) else {
                return;
            };
            // Anything at all from the device is proof it is still there -
            // even a packet we go on to ignore. This is the reconnect
            // watchdog's only input.
            self.last_rx = Instant::now();
            let Ok(pkt) = Packet::parse(&self.rx[..n]) else {
                continue;
            };
            let Some(c) = pkt.as_control() else { continue };
            use screeny_proto::control::{op, Reply};
            match Reply::decode(c.op, c.flags, c.body) {
                Ok(Reply::Telemetry(t)) => self.on_telemetry(t),
                Ok(Reply::Busy { .. }) => self.stats.busy += 1,
                _ if c.op == op::BUSY => self.stats.busy += 1,
                _ => {}
            }
        }
    }

    /// Apply spec 6.9's table to one telemetry sample.
    fn on_telemetry(&mut self, t: Telemetry) {
        self.stats.telemetry = Some(t);
        let sent_now = self.stats.frames_sent;
        let Some(prev) = self.adapt.prev.replace(t) else {
            self.adapt.sent_at_prev = sent_now;
            return;
        };
        let sent = sent_now.saturating_sub(self.adapt.sent_at_prev);
        self.adapt.sent_at_prev = sent_now;
        if sent == 0 {
            return;
        }

        let d_rx = t.frames_rx.wrapping_sub(prev.frames_rx) as u64;
        let d_superseded = t
            .frames_dropped_superseded
            .wrapping_sub(prev.frames_dropped_superseded);
        let d_decode = t.frames_dropped_decode.wrapping_sub(prev.frames_dropped_decode);
        let d_rejected = t.frames_rejected.wrapping_sub(prev.frames_rejected);

        if d_rejected > 0 {
            // Another sender holds the lock, or a version mismatch. The device
            // will also be sending BUSY; both are counted.
            self.stats.busy += 1;
        }

        if d_decode > 0 {
            // A codec the device does not really support, or a bug in this
            // encoder. Either way stop using it (spec 6.9).
            self.stats.decode_failures += 1;
            let bad = t.last_codec;
            if self.my_codecs.len() > 1 && self.my_codecs.contains(&bad) {
                self.my_codecs.retain(|c| *c != bad);
                self.stats.codecs_withdrawn.push(bad);
                self.enc.set_codecs(self.my_codecs.clone());
            }
        }

        if !self.cfg.adapt {
            return;
        }

        let network_limited = (d_rx as f64) < 0.95 * sent as f64;
        if network_limited {
            self.adapt.bad += 1;
            self.adapt.good = 0;
            if self.adapt.bad >= BAD_SECONDS {
                self.adapt.bad = 0;
                self.step_fps(1);
            }
        } else if d_superseded > 0 {
            // The device gets everything and cannot draw it in time. Lowering
            // the frame rate is explicitly the wrong response; send something
            // cheaper to decode instead.
            self.adapt.bad = 0;
            self.adapt.good = 0;
            self.limit_codecs(true);
        } else if d_decode == 0 {
            self.adapt.bad = 0;
            self.adapt.good += 1;
            if self.adapt.good >= GOOD_SECONDS {
                self.adapt.good = 0;
                if self.stats.codec_limited {
                    self.limit_codecs(false);
                } else {
                    self.step_fps(-1);
                }
            }
        }
    }

    fn step_fps(&mut self, dir: i32) {
        let last = i32::try_from(FPS_LADDER.len()).unwrap_or(1) - 1;
        let next = (i32::try_from(self.adapt.rung).unwrap_or(0) + dir).clamp(0, last) as usize;
        if next == self.adapt.rung {
            return;
        }
        self.adapt.rung = next;
        self.stats.fps = self.cfg.fps * FPS_LADDER[next];
        self.stats.fps_changes += 1;
    }

    fn limit_codecs(&mut self, on: bool) {
        if on == self.stats.codec_limited {
            return;
        }
        self.stats.codec_limited = on;
        if on {
            let cheap: Vec<u8> = self
                .my_codecs
                .iter()
                .copied()
                .filter(|c| CHEAP_CODECS.contains(c))
                .collect();
            if cheap.is_empty() {
                self.stats.codec_limited = false;
            } else {
                self.enc.set_codecs(cheap);
            }
        } else {
            self.enc.set_codecs(self.my_codecs.clone());
        }
    }

    /// Stream from `src` until it ends or `stop` is set, then send `FINAL`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if a datagram could not be sent.
    pub fn run(&mut self, src: &mut dyn FrameSource, stop: &AtomicBool) -> Result<()> {
        self.run_with(src, stop, &mut |_| {})
    }

    /// [`Sender::run`], calling `tick` once per telemetry interval so a caller
    /// can print live stats.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if a datagram could not be sent.
    pub fn run_with(
        &mut self,
        src: &mut dyn FrameSource,
        stop: &AtomicBool,
        tick: &mut dyn FnMut(&SendStats),
    ) -> Result<()> {
        let mut frame = Frame::black();
        let stream_start = Instant::now();
        self.stats.started = Some(stream_start);

        // Absolute schedule. `start`/`n` are reset whenever adaptation moves
        // the frame rate, so a rate change does not look like a stall.
        let mut fps = self.stats.fps;
        let mut period = period_of(fps);
        let mut start = Instant::now();
        let mut n: u64 = 0;
        let mut next_tick = Instant::now() + self.cfg.stats_interval;

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            sleep_until(start + period.mul_f64(n as f64));

            let t = FrameTime {
                index: n,
                elapsed: stream_start.elapsed(),
                fps,
            };
            if !src.render(t, &mut frame) {
                break;
            }
            self.send_frame(&frame, false)?;
            self.poll_feedback();

            n += 1;

            // Spec 9.1: if we have fallen more than two periods behind,
            // resynchronise rather than bursting. The device shows
            // newest-wins, so skipping is the correct repair.
            //
            // One deliberate deviation from the spec's pseudo-code, which
            // sets `n = should_be`: that leaves the next target at roughly
            // *now*, so the frame after a stall goes out immediately behind
            // the stalled one - a two-frame burst, which is the one thing the
            // rule exists to avoid. Resuming at the next whole slot costs one
            // more skipped frame and keeps the stream strictly paced.
            let behind = Instant::now().saturating_duration_since(start);
            let should_be = (behind.as_nanos() / period.as_nanos().max(1)) as u64;
            if should_be > n + 1 {
                self.stats.frames_skipped += should_be + 1 - n;
                n = should_be + 1;
            }

            // Only adaptation writes `stats.fps`, and it writes an exact
            // product of the configured rate and a ladder constant, so this
            // is an identity test rather than a numeric comparison.
            #[allow(clippy::float_cmp)]
            let rate_changed = self.stats.fps != fps;
            if rate_changed {
                fps = self.stats.fps;
                period = period_of(fps);
                start = Instant::now();
                n = 0;
            }
            if Instant::now() >= next_tick {
                next_tick += self.cfg.stats_interval;
                tick(&self.stats);
            }
        }

        self.finish()?;
        self.poll_feedback();
        tick(&self.stats);
        Ok(())
    }
}

/// Frame period for a rate, clamped to something sane.
#[must_use]
pub fn period_of(fps: f64) -> Duration {
    Duration::from_secs_f64(1.0 / fps.clamp(0.01, 1000.0))
}

/// Sleep until `target`: `thread::sleep` for all but the last millisecond,
/// then spin on the monotonic clock (spec 9.1).
///
/// `thread::sleep` accuracy varies by platform and load; spinning the last
/// millisecond costs roughly 3% of one core at 30 fps and is what makes the
/// difference between "30 fps give or take 5 ms" and "30.00 fps".
pub fn sleep_until(target: Instant) {
    const SPIN: Duration = Duration::from_millis(1);
    let now = Instant::now();
    if target <= now {
        return;
    }
    let left = target - now;
    if let Some(coarse) = left.checked_sub(SPIN) {
        std::thread::sleep(coarse);
    }
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}
