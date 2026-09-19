//! The control-port client (spec 6).
//!
//! Requests are idempotent or explicitly guarded, and the protocol has no
//! retransmission of its own, so a request that gets no reply within 250 ms is
//! retried up to three times with the **same** `req_id` (spec 6.1).

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use screeny_proto::control::{op, ErrorCode, IdleMode, Reply, Request, Telemetry};
use screeny_proto::{ControlPacket, MAX_UDP_PAYLOAD};

use crate::device::DeviceInfo;
use crate::error::{Error, Result};

/// Per-attempt reply timeout (spec 6.1).
pub const REPLY_TIMEOUT: Duration = Duration::from_millis(250);
/// Attempts before giving up, counting the first (spec 6.1: "retry up to 3
/// times", so four datagrams in the worst case).
pub const TRIES: u32 = 4;

/// A client for one device's control port.
pub struct ControlClient {
    sock: UdpSocket,
    addr: SocketAddr,
    next_id: u16,
    timeout: Duration,
    tries: u32,
    buf: Vec<u8>,
}

impl ControlClient {
    /// Bind an ephemeral socket and connect it to `addr`.
    ///
    /// The control socket is deliberately separate from the frame socket, so
    /// that a `TELEMETRY` reply arriving on the frame socket (spec 6.4) is
    /// unambiguous.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the socket cannot be bound or connected.
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        let sock = crate::net::connected_socket(addr, false, false)?;
        Ok(ControlClient {
            sock,
            addr,
            next_id: 1,
            timeout: REPLY_TIMEOUT,
            tries: TRIES,
            buf: vec![0u8; MAX_UDP_PAYLOAD],
        })
    }

    /// Override the per-attempt timeout.
    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    /// Override the attempt count.
    pub fn set_tries(&mut self, n: u32) {
        self.tries = n.max(1);
    }

    /// The address being talked to.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn next_req_id(&mut self) -> u16 {
        // `req_id == 0` means "no reply wanted" (spec 6.1), so it is skipped.
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }

    /// Send a request and hand the reply to `f`.
    ///
    /// `f` sees a borrowed [`Reply`]; extract what you need from it. An error
    /// reply (spec 6.5) is turned into [`Error::Device`] before `f` is called.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] after the retries are exhausted, [`Error::Device`]
    /// for an error reply, [`Error::BadReply`] for a malformed one.
    pub fn request<T>(
        &mut self,
        req: &Request<'_>,
        f: impl FnOnce(&Reply<'_>) -> std::result::Result<T, String>,
    ) -> Result<T> {
        let op = req.op();
        let req_id = self.next_req_id();
        let mut out = vec![0u8; req.encoded_len()];
        let n = req
            .write(req_id, &mut out)
            .map_err(|e| Error::Metadata(format!("{e:?} while building op {op:#04x}")))?;

        for _ in 0..self.tries {
            self.sock.send(&out[..n])?;
            let deadline = Instant::now() + self.timeout;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                self.sock.set_read_timeout(Some(left))?;
                let got = match self.sock.recv(&mut self.buf) {
                    Ok(n) => n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break
                    }
                    Err(e) => return Err(e.into()),
                };
                let Ok(pkt) = ControlPacket::parse(&self.buf[..got]) else {
                    continue; // not ours: ignore, keep waiting
                };
                if !pkt.is_reply() || pkt.req_id != req_id || pkt.op != op {
                    continue;
                }
                let reply = Reply::decode(pkt.op, pkt.flags, pkt.body).map_err(|e| {
                    Error::BadReply {
                        addr: self.addr,
                        what: format!("{e:?} decoding op {op:#04x}"),
                    }
                })?;
                if let Reply::Err { code } = reply {
                    return Err(Error::Device(
                        ErrorCode::from_u8(code).unwrap_or(ErrorCode::UnknownOp),
                    ));
                }
                return f(&reply).map_err(|what| Error::BadReply {
                    addr: self.addr,
                    what,
                });
            }
        }
        Err(Error::Timeout {
            addr: self.addr,
            op,
            tries: self.tries,
            timeout_ms: self.timeout.as_millis() as u64,
        })
    }

    /// `PING`: returns the device's uptime and the measured round trip.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn ping(&mut self) -> Result<(Duration, u32)> {
        let t0 = Instant::now();
        let uptime = self.request(&Request::Ping, |r| match r {
            Reply::Ping { uptime_ms } => Ok(*uptime_ms),
            other => Err(format!("expected PING reply, got {other:?}")),
        })?;
        Ok((t0.elapsed(), uptime))
    }

    /// `GET_INFO`: the authoritative metadata, which may differ from a cached
    /// TXT record (spec 9.4 step 2).
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`], plus [`Error::Metadata`] if the body
    /// does not parse.
    pub fn info(&mut self) -> Result<DeviceInfo> {
        let bytes = self.request(&Request::GetInfo, |r| match r {
            Reply::Info(b) => Ok(b.to_vec()),
            other => Err(format!("expected GET_INFO reply, got {other:?}")),
        })?;
        DeviceInfo::parse(&bytes)
    }

    /// `TELEMETRY` (spec 6.7).
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn telemetry(&mut self) -> Result<Telemetry> {
        self.request(&Request::Telemetry, |r| match r {
            Reply::Telemetry(t) => Ok(*t),
            other => Err(format!("expected TELEMETRY reply, got {other:?}")),
        })
    }

    /// `SET_BRIGHTNESS`; returns the level actually applied, which is how a
    /// sender learns the firmware's cap.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn set_brightness(&mut self, level: u8) -> Result<u8> {
        self.request(&Request::SetBrightness(level), |r| match r {
            Reply::Brightness { applied } => Ok(*applied),
            other => Err(format!("expected SET_BRIGHTNESS reply, got {other:?}")),
        })
    }

    /// `IDENTIFY` for `duration_ms` (0 stops it).
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn identify(&mut self, duration_ms: u16) -> Result<()> {
        self.request(&Request::Identify { duration_ms }, |r| match r {
            Reply::Identify => Ok(()),
            other => Err(format!("expected IDENTIFY reply, got {other:?}")),
        })
    }

    /// `SET_IDLE`; returns the mode in effect.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn set_idle(&mut self, mode: IdleMode) -> Result<u8> {
        self.request(&Request::SetIdle(mode), |r| match r {
            Reply::Idle { mode } => Ok(*mode),
            other => Err(format!("expected SET_IDLE reply, got {other:?}")),
        })
    }

    /// `RESET_STATS`.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn reset_stats(&mut self) -> Result<()> {
        self.request(&Request::ResetStats, |r| match r {
            Reply::ResetStats => Ok(()),
            other => Err(format!("expected RESET_STATS reply, got {other:?}")),
        })
    }

    /// `RELEASE`: give up the source lock (spec 7.4).
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn release(&mut self) -> Result<()> {
        self.request(&Request::Release, |r| match r {
            Reply::Release => Ok(()),
            other => Err(format!("expected RELEASE reply, got {other:?}")),
        })
    }

    /// `SET_NAME`, persisted by the device.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn set_name(&mut self, name: &str) -> Result<()> {
        self.request(&Request::SetName(name), |r| match r {
            Reply::SetName => Ok(()),
            other => Err(format!("expected SET_NAME reply, got {other:?}")),
        })
    }

    /// `GET_WIFI`: the stored SSID (never the PSK) and the join state.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn get_wifi(&mut self) -> Result<(String, u8)> {
        self.request(&Request::GetWifi, |r| match r {
            Reply::Wifi { ssid, state } => Ok((ssid.to_string(), *state)),
            other => Err(format!("expected GET_WIFI reply, got {other:?}")),
        })
    }

    /// `REBOOT`, guarded by the magic word (spec 6.3). The reply is sent
    /// before the device reboots.
    ///
    /// # Errors
    ///
    /// As [`ControlClient::request`].
    pub fn reboot(&mut self) -> Result<()> {
        self.request(&Request::Reboot, |r| match r {
            Reply::Reboot => Ok(()),
            other => Err(format!("expected REBOOT reply, got {other:?}")),
        })
    }
}

/// `op` names, for printing.
#[must_use]
pub fn op_name(o: u8) -> &'static str {
    match o {
        op::PING => "PING",
        op::GET_INFO => "GET_INFO",
        op::TELEMETRY => "TELEMETRY",
        op::SET_BRIGHTNESS => "SET_BRIGHTNESS",
        op::IDENTIFY => "IDENTIFY",
        op::SET_IDLE => "SET_IDLE",
        op::RESET_STATS => "RESET_STATS",
        op::RELEASE => "RELEASE",
        op::SET_NAME => "SET_NAME",
        op::GET_WIFI => "GET_WIFI",
        op::SET_WIFI => "SET_WIFI",
        op::BUSY => "BUSY",
        op::REBOOT => "REBOOT",
        _ => "?",
    }
}
