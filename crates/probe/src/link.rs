//! Sockets, pacing, and the two things a sender has to get right.
//!
//! Spec section 9: `Instant` for everything, absolute scheduling so error
//! does not accumulate, skip rather than burst, and a control socket separate
//! from the frame socket so that a `TELEMETRY` arriving on the frame socket
//! (section 6.4) is unambiguous.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use screeny_proto::control::{ErrorCode, Reply, Request};
use screeny_proto::{ControlPacket, MAX_UDP_PAYLOAD};

/// A control-port conversation with one device.
pub struct Control {
    sock: UdpSocket,
    next_id: u16,
}

impl Control {
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(addr)?;
        Ok(Control { sock, next_id: 1 })
    }

    fn next_id(&mut self) -> u16 {
        // req_id 0 means "no reply wanted" (section 6.1), so it is never ours.
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }

    /// One request, with section 6.1's retry rule: no reply within 250 ms
    /// means retry up to 3 times with the **same** `req_id`, then give up.
    pub fn request(&mut self, req: Request<'_>) -> Result<OwnedReply, String> {
        let req_id = self.next_id();
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        let n = req.write(req_id, &mut buf).map_err(|e| format!("{e:?}"))?;
        let op = req.op();
        for attempt in 0..4 {
            self.sock.send(&buf[..n]).map_err(|e| e.to_string())?;
            self.sock
                .set_read_timeout(Some(Duration::from_millis(250)))
                .map_err(|e| e.to_string())?;
            let mut rx = [0u8; MAX_UDP_PAYLOAD];
            match self.sock.recv(&mut rx) {
                Ok(len) => {
                    let pkt = ControlPacket::parse(&rx[..len])
                        .map_err(|e| format!("reply did not parse: {e:?}"))?;
                    if !pkt.is_reply() || pkt.req_id != req_id || pkt.op != op {
                        continue; // somebody else's packet; keep waiting
                    }
                    return Ok(OwnedReply::from(&pkt));
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    if attempt == 3 {
                        return Err(format!("no reply to op {op:#04x} after 4 attempts"));
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("unreachable".into())
    }

    /// Send hand-built bytes and return whatever comes back within 250 ms.
    /// Used by the conformance probes, which build datagrams the library
    /// would refuse to build.
    pub fn raw(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
        self.sock.send(bytes).map_err(|e| e.to_string())?;
        self.sock
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|e| e.to_string())?;
        let mut rx = [0u8; MAX_UDP_PAYLOAD];
        match self.sock.recv(&mut rx) {
            Ok(n) => Ok(Some(rx[..n].to_vec())),
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// A reply, decoded and detached from its datagram.
///
/// Several variants exist only to be printed: the point of a probe is to
/// show what came back, and `{:?}` on a typed value is a better report than
/// a hex dump. Hence the `allow`.
///
/// `Reply<'a>` borrows the receive buffer, which is awkward to hold across a
/// function return; every variant this tool cares about is either `Copy` or a
/// short string.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum OwnedReply {
    Ping { uptime_ms: u32 },
    Info(Vec<u8>),
    Telemetry(screeny_proto::control::Telemetry),
    Brightness { applied: u8 },
    Idle { mode: u8 },
    Wifi { ssid: String, state: u8 },
    Busy { reason: u8, remaining_ms: u32 },
    Err { code: u8 },
    Empty,
    Undecodable { op: u8, code: ErrorCode },
}

impl OwnedReply {
    fn from(pkt: &ControlPacket<'_>) -> Self {
        match Reply::decode(pkt.op, pkt.flags, pkt.body) {
            Ok(Reply::Ping { uptime_ms }) => OwnedReply::Ping { uptime_ms },
            Ok(Reply::Info(b)) => OwnedReply::Info(b.to_vec()),
            Ok(Reply::Telemetry(t)) => OwnedReply::Telemetry(t),
            Ok(Reply::Brightness { applied }) => OwnedReply::Brightness { applied },
            Ok(Reply::Idle { mode }) => OwnedReply::Idle { mode },
            Ok(Reply::Wifi { ssid, state }) => OwnedReply::Wifi {
                ssid: ssid.to_string(),
                state,
            },
            Ok(Reply::Busy {
                reason,
                lock_holder_ms_remaining,
            }) => OwnedReply::Busy {
                reason,
                remaining_ms: lock_holder_ms_remaining,
            },
            Ok(Reply::Err { code }) => OwnedReply::Err { code },
            Ok(_) => OwnedReply::Empty,
            Err(code) => OwnedReply::Undecodable { op: pkt.op, code },
        }
    }

    pub fn err_code(&self) -> Option<u8> {
        match self {
            OwnedReply::Err { code } => Some(*code),
            _ => None,
        }
    }
}

/// A frame-port sender. One socket, one source identity (section 7.1).
pub struct FrameLink {
    sock: UdpSocket,
    pub sent: u64,
    pub seq: u16,
}

impl FrameLink {
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(addr)?;
        sock.set_nonblocking(true)?;
        Ok(FrameLink {
            sock,
            sent: 0,
            // Section 3.2: `seq` MAY start at any value, and a sender that
            // wants a fresh sequence rebinds rather than resetting. Starting
            // somewhere other than 0 is free evidence that we do not assume it.
            seq: 0x4000,
        })
    }

    pub fn local(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// Send one frame and advance `seq`.
    pub fn send(&mut self, codec: u8, flags: u8, payload: &[u8]) -> io::Result<()> {
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        let pkt = screeny_proto::FramePacket {
            codec,
            flags,
            seq: self.seq,
            timestamp_us: None,
            payload,
        };
        let n = pkt
            .write(&mut buf)
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        self.sock.send(&buf[..n])?;
        self.seq = self.seq.wrapping_add(1);
        self.sent += 1;
        Ok(())
    }

    /// Send hand-built bytes without touching `seq`, for conformance probes.
    pub fn send_raw(&self, bytes: &[u8]) -> io::Result<()> {
        self.sock.send(bytes).map(|_| ())
    }

    /// Drain anything the device sent back. Section 6.2: `TELEMETRY` and
    /// `BUSY` both arrive here, on the frame socket, and a sender MUST accept
    /// them.
    pub fn poll(&self) -> Vec<OwnedReply> {
        let mut out = Vec::new();
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        loop {
            match self.sock.recv(&mut buf) {
                Ok(n) => {
                    if let Ok(pkt) = ControlPacket::parse(&buf[..n]) {
                        if pkt.is_reply() {
                            out.push(OwnedReply::from(&pkt));
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        out
    }
}

/// Spec section 9.1's pacer: absolute deadlines, and a skip rather than a
/// burst when we fall behind.
pub struct Pacer {
    start: Instant,
    period: Duration,
    pub n: u64,
    pub skipped: u64,
}

impl Pacer {
    pub fn new(fps: f64) -> Self {
        Pacer {
            start: Instant::now(),
            period: Duration::from_nanos((1e9 / fps) as u64),
            n: 0,
            skipped: 0,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Sleep until frame `n` is due, then return its index.
    pub fn wait(&mut self) -> u64 {
        let target = self.start + self.period * (self.n as u32);
        sleep_until(target);
        let n = self.n;
        self.n += 1;
        // Never burst to catch up: the device's rx path queues below the
        // socket and the AP aggregates, so a catch-up burst only raises
        // latency for every subsequent frame. Skip instead.
        let behind = Instant::now().saturating_duration_since(self.start);
        let should_be = (behind.as_nanos() / self.period.as_nanos()) as u64;
        if should_be > self.n + 1 {
            self.skipped += should_be - self.n;
            self.n = should_be;
        }
        n
    }
}

/// Sleep to within a spin of `target`. `thread::sleep` accuracy varies by
/// platform and load; spinning the last millisecond costs about 3% of one
/// core at 30 fps.
fn sleep_until(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }
    let d = target - now;
    if d > Duration::from_millis(1) {
        std::thread::sleep(d - Duration::from_millis(1));
    }
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}
