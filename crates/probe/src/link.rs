//! Sockets, pacing, and the two things a sender has to get right.
//!
//! Spec section 9: `Instant` for everything, absolute scheduling so error
//! does not accumulate, skip rather than burst, and a control socket separate
//! from the frame socket so that a `TELEMETRY` arriving on the frame socket
//! (section 6.4) is unambiguous.
//!
//! Card 080 merged `crates/sim/tests/common/mod.rs`'s `Sender`/`Ctrl` into
//! these two types, so the conformance suite and the simulator's own tests
//! drive one implementation. That is why each has both styles of API: the
//! probe's `request`, which picks a `req_id` and applies section 6.1's retry
//! rule, and the tests' `call`/`send`, which pin the `req_id` and never
//! retry because the rule under test is often "and nothing came back".

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use screeny_proto::control::{ErrorCode, Reply, Request};
use screeny_proto::{ControlPacket, FramePacket, MAX_UDP_PAYLOAD};

/// How long a single request waits before it is retried (section 6.1).
pub const REPLY_TIMEOUT: Duration = Duration::from_millis(250);

/// A control-port conversation with one device.
pub struct Control {
    sock: UdpSocket,
    next_id: u16,
}

impl Control {
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(addr)?;
        sock.set_read_timeout(Some(REPLY_TIMEOUT))?;
        Ok(Control { sock, next_id: 1 })
    }

    /// This client's address.
    pub fn local(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// This client's address, for tests that would only unwrap it anyway.
    pub fn addr(&self) -> SocketAddr {
        self.sock.local_addr().expect("a bound control socket")
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
                .set_read_timeout(Some(REPLY_TIMEOUT))
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

    /// Send a request with an explicit `req_id` and do not wait.
    pub fn send(&self, req: &Request<'_>, req_id: u16) {
        let mut buf = vec![0u8; req.encoded_len()];
        let n = req.write(req_id, &mut buf).expect("request fits");
        self.sock.send(&buf[..n]).expect("send request");
    }

    /// Send hand-built bytes, for datagrams `Request::write` will not build.
    pub fn send_raw(&self, bytes: &[u8]) {
        self.sock.send(bytes).expect("send raw");
    }

    /// Wait up to `timeout` for one datagram.
    pub fn recv(&self, timeout: Duration) -> Option<Vec<u8>> {
        self.sock.set_read_timeout(Some(timeout)).ok()?;
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        match self.sock.recv(&mut buf) {
            Ok(n) => Some(buf[..n].to_vec()),
            Err(_) => None,
        }
    }

    /// Send a request with an explicit `req_id` and wait once for its reply.
    /// No retry: "and nothing came back" is a rule in its own right.
    pub fn call(&self, req: &Request<'_>, req_id: u16) -> Option<Vec<u8>> {
        self.send(req, req_id);
        self.recv(Duration::from_millis(500))
    }

    /// Send hand-built bytes and return whatever comes back within 250 ms.
    /// Used by the conformance suite, which builds datagrams the library
    /// would refuse to build.
    pub fn raw(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
        self.raw_timeout(bytes, REPLY_TIMEOUT)
    }

    /// `raw` with the wait spelled out.
    pub fn raw_timeout(
        &mut self,
        bytes: &[u8],
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>, String> {
        self.sock.send(bytes).map_err(|e| e.to_string())?;
        self.sock
            .set_read_timeout(Some(timeout))
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
    pub fn from(pkt: &ControlPacket<'_>) -> Self {
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

/// Split a reply datagram into its envelope and its decoded body.
pub fn parse_reply(d: &[u8]) -> (ControlPacket<'_>, Result<Reply<'_>, ErrorCode>) {
    let pkt = ControlPacket::parse(d).expect("a reply is a well-formed CONTROL");
    let body = Reply::decode(pkt.op, pkt.flags, pkt.body);
    (pkt, body)
}

/// The error code in a reply, or `None` if it was not an error.
pub fn error_code(d: &[u8]) -> Option<u8> {
    let (_, r) = parse_reply(d);
    match r {
        Ok(Reply::Err { code }) => Some(code),
        _ => None,
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
            seq: 0,
        })
    }

    /// Start the sequence somewhere other than 0. Section 3.2: `seq` MAY start
    /// at any value, and a sender that wants a fresh sequence rebinds rather
    /// than resetting, so a non-zero start is free evidence that nothing here
    /// assumes 0.
    pub fn starting_at(mut self, seq: u16) -> Self {
        self.seq = seq;
        self
    }

    pub fn local(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// This sender's own address, which is its identity as far as spec
    /// section 7.1 is concerned.
    pub fn addr(&self) -> SocketAddr {
        self.sock.local_addr().expect("a bound frame socket")
    }

    /// Send one frame and advance `seq`, returning the `seq` used.
    pub fn send(&mut self, codec: u8, flags: u8, payload: &[u8]) -> io::Result<u16> {
        let seq = self.seq;
        self.send_seq(codec, flags, seq, payload)?;
        self.seq = self.seq.wrapping_add(1);
        self.sent += 1;
        Ok(seq)
    }

    /// Send one frame with an explicit sequence number.
    pub fn send_seq(&self, codec: u8, flags: u8, seq: u16, payload: &[u8]) -> io::Result<()> {
        self.send_full(codec, flags, seq, None, payload)
    }

    /// Send one frame with everything spelled out, including `HAS_TS`.
    pub fn send_full(
        &self,
        codec: u8,
        flags: u8,
        seq: u16,
        timestamp_us: Option<u32>,
        payload: &[u8],
    ) -> io::Result<()> {
        let pkt = FramePacket {
            codec,
            flags,
            seq,
            timestamp_us,
            payload,
        };
        let mut buf = vec![0u8; pkt.encoded_len()];
        let n = pkt
            .write(&mut buf)
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        self.sock.send(&buf[..n])?;
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
        self.drain_raw(None)
            .iter()
            .filter_map(|d| match ControlPacket::parse(d) {
                Ok(pkt) if pkt.is_reply() => Some(OwnedReply::from(&pkt)),
                _ => None,
            })
            .collect()
    }

    /// Drain the raw datagrams already queued, waiting `settle` for stragglers.
    /// A short wait is the difference between "the device did not answer" and
    /// "the answer had not arrived yet", which is most of what this suite asks.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.drain_raw(Some(Duration::from_millis(5)))
    }

    fn drain_raw(&self, settle: Option<Duration>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        let _ = self.sock.set_nonblocking(settle.is_none());
        if let Some(t) = settle {
            let _ = self.sock.set_read_timeout(Some(t));
        }
        while let Ok(n) = self.sock.recv(&mut buf) {
            out.push(buf[..n].to_vec());
        }
        let _ = self.sock.set_nonblocking(true);
        out
    }

    /// Wait up to `timeout` for one datagram on the frame socket.
    pub fn recv(&self, timeout: Duration) -> Option<Vec<u8>> {
        let _ = self.sock.set_nonblocking(false);
        let _ = self.sock.set_read_timeout(Some(timeout));
        let mut buf = [0u8; MAX_UDP_PAYLOAD];
        let r = match self.sock.recv(&mut buf) {
            Ok(n) => Some(buf[..n].to_vec()),
            Err(_) => None,
        };
        let _ = self.sock.set_nonblocking(true);
        r
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
