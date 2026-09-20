//! An in-process screeny receiver, built directly on `screeny_proto`.
//!
//! The real firmware does not speak the final protocol yet (card 008) and the
//! simulator is being built in parallel (card 006), so the sender is tested
//! against this: two UDP sockets on loopback with ephemeral ports, the spec's
//! parsing and sequence rules, proto's decoders, and enough of the control
//! surface to answer a handshake. It is deliberately small - it exists to be
//! obviously correct, not to be a device.
//!
//! What it does implement, because the sender depends on it:
//!
//! * spec 2/3 validation and the `newer()` sequence test;
//! * spec 6.4 - a `STATS_REQ` frame is answered with `TELEMETRY` **from the
//!   frame port** to the frame's source port;
//! * spec 6.3 control opcodes, including `GET_INFO` returning the TXT record;
//! * loss injection, so the adaptation rules in spec 6.9 can be exercised.

#![allow(dead_code)]

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use screeny::proto::control::{op, Reply, Request, Telemetry};
use screeny::proto::txt::DeviceInfo as TxtInfo;
use screeny::proto::{
    ControlPacket, FramePacket, Rgb888Frame, MAX_UDP_PAYLOAD, NBYTES,
};

/// One accepted frame.
pub struct RxFrame {
    pub seq: u16,
    pub codec: u8,
    pub flags: u8,
    pub bytes: usize,
    pub at: Instant,
    pub timestamp_us: Option<u32>,
    /// Decoded pixels, if the receiver was told to keep them.
    pub pixels: Option<Box<Rgb888Frame>>,
}

/// Everything the test can look at afterwards.
#[derive(Default)]
pub struct RxState {
    pub frames: Vec<RxFrame>,
    pub telemetry: Telemetry,
    pub last_seq: Option<u16>,
    /// Datagrams thrown away by the loss injector.
    pub lost: u32,
    /// Datagrams that failed `Packet::parse`.
    pub rejected: u32,
    /// Payloads that failed to decode. Any of these is a bug in the sender.
    pub decode_errors: Vec<(u16, u8, screeny::proto::DecodeError)>,
    pub stats_requests: u32,
    pub control_requests: u32,
    pub started: Option<Instant>,
}

impl RxState {
    /// The frames that were part of the paced stream, which is all of them
    /// except the `FINAL` one.
    ///
    /// `FINAL` repeats the last payload the moment the source ends (spec 9.4
    /// step 5), microseconds behind the frame before it, so it adds a frame
    /// to any count without adding to the span - a 1.7% error on a 2 s run at
    /// 30 fps. Worse, whether it is here at all is a race with `shutdown()`:
    /// card 093 caught it in 9 of 20 identical runs, which made every rate
    /// measured from these frames bimodal. `Sender`'s own `min_gap` leaves it
    /// out for the same reason.
    pub fn paced(&self) -> impl Iterator<Item = &RxFrame> {
        self.frames
            .iter()
            .filter(|f| f.flags & screeny::proto::F_FINAL == 0)
    }

    /// Gaps between successive arrivals in the paced stream.
    pub fn gaps(&self) -> Vec<Duration> {
        let at: Vec<Instant> = self.paced().map(|f| f.at).collect();
        at.windows(2).map(|w| w[1].duration_since(w[0])).collect()
    }

    /// Frames per second over the arrival window, measured across the
    /// intervals between arrivals rather than as a count over elapsed time:
    /// `n` frames span `n - 1` intervals, and a count divided by a window
    /// with ragged ends means something different at every run length (card
    /// 093).
    pub fn fps(&self) -> f64 {
        let at: Vec<Instant> = self.paced().map(|f| f.at).collect();
        if at.len() < 2 {
            return 0.0;
        }
        let span = at[at.len() - 1].duration_since(at[0]).as_secs_f64();
        (at.len() - 1) as f64 / span
    }
}

/// Configuration knobs.
pub struct RxConfig {
    /// Keep decoded pixels for every frame.
    pub keep_pixels: bool,
    /// Drop one frame in N on the way in (0 = lossless).
    pub drop_one_in: u32,
    /// What `GET_INFO` and the TXT record say.
    pub info: OwnedInfo,
}

impl Default for RxConfig {
    fn default() -> Self {
        RxConfig {
            keep_pixels: true,
            drop_one_in: 0,
            info: OwnedInfo::default(),
        }
    }
}

/// The metadata the fake device advertises.
#[derive(Clone)]
pub struct OwnedInfo {
    pub codecs: String,
    pub mtu: u16,
    pub fw: String,
    pub id: String,
    pub name: String,
}

impl Default for OwnedInfo {
    fn default() -> Self {
        OwnedInfo {
            codecs: screeny::proto::dec::CODECS_TXT.to_string(),
            mtu: screeny::proto::MAX_PIXEL_PAYLOAD as u16,
            fw: "0.0.0-test".into(),
            id: "abcdef".into(),
            name: "test receiver".into(),
        }
    }
}

impl OwnedInfo {
    fn txt(&self, ctrl: u16) -> Vec<u8> {
        let d = TxtInfo {
            txtvers: 1,
            proto: "1",
            w: screeny::proto::W as u16,
            h: screeny::proto::H as u16,
            codecs: &self.codecs,
            mtu: self.mtu,
            ctrl,
            fw: &self.fw,
            id: &self.id,
            name: &self.name,
        };
        let mut out = vec![0u8; 512];
        let n = d.write(&mut out).expect("TXT record");
        out.truncate(n);
        out
    }
}

/// A running fake device.
pub struct Receiver {
    pub frame_addr: SocketAddr,
    pub control_addr: SocketAddr,
    pub state: Arc<Mutex<RxState>>,
    /// Bumped whenever the control thread answers something.
    pub brightness: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Receiver {
    /// Bind both sockets on loopback with ephemeral ports and start serving.
    pub fn start(cfg: RxConfig) -> Receiver {
        let fsock = UdpSocket::bind("127.0.0.1:0").expect("bind frame socket");
        let csock = UdpSocket::bind("127.0.0.1:0").expect("bind control socket");
        Self::start_on(fsock, csock, cfg)
    }

    fn start_on(fsock: UdpSocket, csock: UdpSocket, cfg: RxConfig) -> Receiver {
        fsock
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        csock
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let frame_addr = fsock.local_addr().unwrap();
        let control_addr = csock.local_addr().unwrap();

        let state = Arc::new(Mutex::new(RxState {
            started: Some(Instant::now()),
            ..RxState::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let brightness = Arc::new(AtomicU32::new(30));

        let txt = cfg.info.txt(control_addr.port());
        let keep = cfg.keep_pixels;
        let drop_one_in = cfg.drop_one_in;

        let mut threads = Vec::new();
        {
            let state = state.clone();
            let stop = stop.clone();
            threads.push(std::thread::spawn(move || {
                frame_loop(&fsock, &state, &stop, keep, drop_one_in);
            }));
        }
        {
            let state = state.clone();
            let stop = stop.clone();
            let brightness = brightness.clone();
            threads.push(std::thread::spawn(move || {
                control_loop(&csock, &state, &stop, &txt, &brightness);
            }));
        }

        Receiver {
            frame_addr,
            control_addr,
            state,
            brightness,
            stop,
            threads,
        }
    }

    /// Like [`Receiver::start`], but with the control port one above the
    /// frame port, the way the defaults are laid out (49374/49375).
    ///
    /// `screeny --addr IP:PORT` guesses the control port that way when it has
    /// no TXT record to read, so the CLI tests need a receiver that matches.
    pub fn start_adjacent(cfg: RxConfig) -> Receiver {
        for _ in 0..64 {
            let f = UdpSocket::bind("127.0.0.1:0").expect("bind frame socket");
            let port = f.local_addr().unwrap().port();
            let Some(next) = port.checked_add(1) else {
                continue;
            };
            let Ok(c) = UdpSocket::bind(("127.0.0.1", next)) else {
                continue;
            };
            return Self::start_on(f, c, cfg);
        }
        panic!("could not find an adjacent pair of free ports");
    }

    /// A device pointing at this receiver, with the right control port.
    pub fn device(&self) -> screeny::Device {
        let mut d = screeny::Device::from_addr(self.frame_addr);
        d.control = self.control_addr;
        d
    }

    /// How many frames have been accepted so far.
    pub fn frame_count(&self) -> usize {
        self.state.lock().unwrap().frames.len()
    }

    /// Block until `n` frames have arrived, or `timeout` passes.
    pub fn wait_for(&self, n: usize, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if self.frame_count() >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// Stop both threads and hand back what was collected.
    pub fn shutdown(self) -> RxState {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads {
            let _ = t.join();
        }
        Arc::try_unwrap(self.state)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default()
    }
}

fn frame_loop(
    sock: &UdpSocket,
    state: &Arc<Mutex<RxState>>,
    stop: &AtomicBool,
    keep: bool,
    drop_one_in: u32,
) {
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let mut seen = 0u32;
    let mut out = vec![0u8; MAX_UDP_PAYLOAD];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let at = Instant::now();
        let Ok(pkt) = FramePacket::parse(&buf[..n]) else {
            state.lock().unwrap().rejected += 1;
            continue;
        };
        seen += 1;
        if drop_one_in > 0 && seen.is_multiple_of(drop_one_in) {
            let mut s = state.lock().unwrap();
            s.lost += 1;
            continue;
        }

        let mut s = state.lock().unwrap();
        // Spec 3.2: only a strictly newer sequence number is accepted.
        if let Some(last) = s.last_seq {
            if !screeny::proto::newer(pkt.seq, last) {
                s.telemetry.frames_dropped_stale += 1;
                continue;
            }
            s.telemetry.seq_gaps += u32::from(screeny::proto::gap(pkt.seq, last));
        }
        s.last_seq = Some(pkt.seq);
        s.telemetry.frames_rx += 1;

        let mut px = Box::new([0u8; NBYTES]);
        match screeny::proto::decode(pkt.codec, pkt.payload, &mut px) {
            Ok(()) => {
                s.telemetry.frames_shown += 1;
                s.telemetry.last_codec = pkt.codec;
                s.frames.push(RxFrame {
                    seq: pkt.seq,
                    codec: pkt.codec,
                    flags: pkt.flags,
                    bytes: pkt.payload.len(),
                    at,
                    timestamp_us: pkt.timestamp_us,
                    pixels: if keep { Some(px) } else { None },
                });
            }
            Err(e) => {
                s.telemetry.frames_dropped_decode += 1;
                s.decode_errors.push((pkt.seq, pkt.codec, e));
            }
        }

        if pkt.wants_stats() {
            s.stats_requests += 1;
            s.telemetry.uptime_ms = s
                .started
                .map_or(0, |t| t.elapsed().as_millis() as u32);
            let t = s.telemetry;
            drop(s);
            // Spec 6.4: the reply goes out of the *frame* socket, to the
            // datagram's source port.
            if let Ok(len) = Reply::Telemetry(t).write(op::TELEMETRY, 0, &mut out) {
                let _ = sock.send_to(&out[..len], from);
            }
        }
    }
}

fn control_loop(
    sock: &UdpSocket,
    state: &Arc<Mutex<RxState>>,
    stop: &AtomicBool,
    txt: &[u8],
    brightness: &AtomicU32,
) {
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let mut out = vec![0u8; MAX_UDP_PAYLOAD];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Ok(pkt) = ControlPacket::parse(&buf[..n]) else {
            state.lock().unwrap().rejected += 1;
            continue;
        };
        if pkt.is_reply() {
            continue;
        }
        let (uptime, tel) = {
            let mut s = state.lock().unwrap();
            s.control_requests += 1;
            s.telemetry.uptime_ms = s
                .started
                .map_or(0, |t| t.elapsed().as_millis() as u32);
            s.telemetry.brightness = brightness.load(Ordering::Relaxed) as u8;
            (s.telemetry.uptime_ms, s.telemetry)
        };

        let reply = match Request::decode(pkt.op, pkt.body) {
            Ok(Request::Ping) => Reply::Ping { uptime_ms: uptime },
            Ok(Request::GetInfo) => Reply::Info(txt),
            Ok(Request::Telemetry) => Reply::Telemetry(tel),
            Ok(Request::SetBrightness(v)) => {
                // Like the firmware, clamp to a compile-time cap and report
                // what was actually applied.
                let applied = v.min(60);
                brightness.store(u32::from(applied), Ordering::Relaxed);
                Reply::Brightness { applied }
            }
            Ok(Request::Identify { .. }) => Reply::Identify,
            Ok(Request::SetIdle(m)) => Reply::Idle { mode: m.as_u8() },
            Ok(Request::ResetStats) => {
                let mut s = state.lock().unwrap();
                s.telemetry = Telemetry::default();
                s.last_seq = None;
                Reply::ResetStats
            }
            Ok(Request::Release) => Reply::Release,
            Ok(Request::SetName(_)) => Reply::SetName,
            Ok(Request::GetWifi) => Reply::Wifi {
                ssid: "Example-Wifi1",
                state: screeny::proto::control::wifi_state::CONNECTED,
            },
            Ok(Request::SetWifi(_)) => Reply::SetWifi,
            Ok(Request::Reboot) => Reply::Reboot,
            Err(code) => Reply::Err {
                code: code.as_u8(),
            },
        };
        if pkt.req_id == 0 {
            continue; // spec 6.1: no reply wanted
        }
        if let Ok(len) = reply.write(pkt.op, pkt.req_id, &mut out) {
            let _ = sock.send_to(&out[..len], from);
        }
    }
}

// ---------------------------------------------------------------------------
// Test content
// ---------------------------------------------------------------------------

/// A tiny deterministic PRNG, so failures reproduce.
pub struct Rng(pub u32);

impl Rng {
    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    pub fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n.max(1)
    }
    pub fn byte(&mut self) -> u8 {
        (self.next_u32() >> 13) as u8
    }
}

/// Frames chosen to be hard on the encoder in different ways.
pub fn adversarial_frames(rng: &mut Rng) -> Vec<(&'static str, screeny::Frame)> {
    use screeny::proto::{H, W};
    let mut out: Vec<(&'static str, screeny::Frame)> = Vec::new();

    // Every pixel a different random colour: no palette, no LZ matches.
    let mut f = screeny::Frame::black();
    for p in 0..screeny::proto::NPIX {
        f.set_at(p, [rng.byte(), rng.byte(), rng.byte()]);
    }
    out.push(("random", f));

    // Two colours in a 1px checkerboard: few colours, zero LZ redundancy at
    // byte granularity for the 8bpp plane.
    let mut f = screeny::Frame::black();
    for y in 0..H {
        for x in 0..W {
            f.set(x, y, if (x + y) % 2 == 0 { [255; 3] } else { [0; 3] });
        }
    }
    out.push(("checker2", f));

    // Exactly 16 random colours in random positions: the PAL4_LZ rung with
    // an incompressible index plane.
    let pal: Vec<[u8; 3]> = (0..16).map(|_| [rng.byte(), rng.byte(), rng.byte()]).collect();
    let mut f = screeny::Frame::black();
    for p in 0..screeny::proto::NPIX {
        f.set_at(p, pal[rng.below(16) as usize]);
    }
    out.push(("rand16", f));

    // Exactly 32, same idea: this is the frame that can overflow both LZ
    // rungs and must still come out exact via raw PAL5.
    let pal: Vec<[u8; 3]> = (0..32).map(|_| [rng.byte(), rng.byte(), rng.byte()]).collect();
    let mut f = screeny::Frame::black();
    for p in 0..screeny::proto::NPIX {
        f.set_at(p, pal[rng.below(32) as usize]);
    }
    out.push(("rand32", f));

    // 256 random colours: the top of the lossless ladder.
    let pal: Vec<[u8; 3]> = (0..256).map(|_| [rng.byte(), rng.byte(), rng.byte()]).collect();
    let mut f = screeny::Frame::black();
    for p in 0..screeny::proto::NPIX {
        f.set_at(p, pal[rng.below(256) as usize]);
    }
    out.push(("rand256", f));

    // 257 colours: one more than the ladder can carry losslessly.
    let pal: Vec<[u8; 3]> = (0..257).map(|_| [rng.byte(), rng.byte(), rng.byte()]).collect();
    let mut f = screeny::Frame::black();
    for p in 0..screeny::proto::NPIX {
        f.set_at(p, pal[(p % 257).min(256)]);
    }
    out.push(("rand257", f));

    // Single colour, and near-single colour.
    out.push(("solid", screeny::Frame::solid([17, 200, 3])));
    let mut f = screeny::Frame::solid([17, 200, 3]);
    f.set_at(1234, [18, 200, 3]);
    out.push(("almost-solid", f));

    // All black and all white: the extremes of the panel model.
    out.push(("black", screeny::Frame::black()));
    out.push(("white", screeny::Frame::solid([255; 3])));

    // Noise in the darkest codes, where the panel has almost no levels.
    let mut f = screeny::Frame::black();
    for p in 0..screeny::proto::NPIX {
        f.set_at(p, [rng.byte() >> 5, rng.byte() >> 5, rng.byte() >> 5]);
    }
    out.push(("darknoise", f));

    out
}
