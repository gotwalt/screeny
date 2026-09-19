//! Finding devices: mDNS browse (spec 5.4) and the broadcast escape hatch
//! (spec 5.5).
//!
//! Two rules from the spec shape this module:
//!
//! * **An empty browse is normal and retriable, never fatal.** macOS caches
//!   local-network denials in a way that makes multicast silently vanish until
//!   the socket is recreated, so "found nothing" is a condition to report and
//!   hint about, not a crash.
//! * **`--addr` always works.** Every path through the CLI can skip this
//!   module entirely.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};
use screeny_proto::control::{op, Request};
use screeny_proto::{ControlPacket, DEFAULT_CONTROL_PORT, MAX_UDP_PAYLOAD, SERVICE_TYPE};

use crate::device::{Device, DeviceInfo};
use crate::error::{Error, Result};
use crate::net;

/// Default browse window. Long enough for a device that is awake to answer
/// twice, short enough not to feel broken (spec 9.4 step 1).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Browse `_screeny._udp.local.` and return everything that resolved.
///
/// Returns as soon as `want` distinct instances have resolved, or when
/// `timeout` expires. An empty result is `Ok(vec![])`: deciding that is an
/// error is the caller's business, because `screeny discover` wants to print
/// "nothing found" and a streaming command wants to fail.
///
/// # Errors
///
/// [`Error::Mdns`] if the daemon could not be started or the browse rejected.
pub fn browse(timeout: Duration, want: Option<usize>) -> Result<Vec<Device>> {
    let daemon = ServiceDaemon::new().map_err(|e| Error::Mdns(e.to_string()))?;
    let rx = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| Error::Mdns(e.to_string()))?;

    let deadline = Instant::now() + timeout;
    let mut found: BTreeMap<String, Device> = BTreeMap::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(svc)) => {
                if let Some(d) = device_from_service(&svc) {
                    found.insert(d.instance.clone(), d);
                    if want.is_some_and(|n| found.len() >= n) {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}

/// Instance name of a DNS-SD full name: everything before the service type.
fn instance_of(fullname: &str) -> String {
    fullname
        .strip_suffix(&format!(".{SERVICE_TYPE}"))
        .unwrap_or(fullname)
        .to_string()
}

fn device_from_service(svc: &mdns_sd::ResolvedService) -> Option<Device> {
    let addresses: Vec<IpAddr> = svc.addresses.iter().map(mdns_sd::ScopedIp::to_ip_addr).collect();
    let ip = net::pick_address(&addresses)?;
    let info = DeviceInfo::from_pairs(
        svc.txt_properties
            .iter()
            .map(|p| (p.key(), p.val().unwrap_or(b""))),
    )
    .ok()?;
    // Spec 5.2: a service missing a required key MUST be ignored, which
    // `from_pairs` already enforces; this is the version check.
    if !info.speaks_v1() {
        return None;
    }
    let frame = SocketAddr::new(ip, svc.port);
    let control = SocketAddr::new(ip, info.ctrl);
    Some(Device {
        instance: instance_of(&svc.fullname),
        host: Some(svc.host.clone()),
        frame,
        control,
        addresses,
        info: Some(info),
    })
}

/// How the CLI was told to find a device.
#[derive(Clone, Debug, Default)]
pub struct Target {
    /// An explicit `IP[:port]`, which skips discovery entirely.
    pub addr: Option<SocketAddr>,
    /// A DNS-SD instance name (or a unique prefix of one) to pick out.
    pub name: Option<String>,
    /// Browse window.
    pub timeout: Option<Duration>,
    /// Use the broadcast `GET_INFO` probe instead of mDNS (spec 5.5).
    pub broadcast: bool,
}

impl Target {
    /// Resolve to exactly one device.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when nothing answered, [`Error::NoSuchDevice`] when
    /// something did but not the one asked for.
    pub fn resolve(&self) -> Result<Device> {
        if let Some(a) = self.addr {
            return Ok(Device::from_addr(a));
        }
        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let mut found = if self.broadcast {
            broadcast_probe(timeout, DEFAULT_CONTROL_PORT)?
        } else {
            browse(timeout, if self.name.is_none() { Some(1) } else { None })?
        };
        if found.is_empty() {
            return Err(Error::NotFound {
                service: SERVICE_TYPE,
                secs: timeout.as_secs_f64(),
            });
        }
        match &self.name {
            None => Ok(found.remove(0)),
            Some(want) => {
                let names: Vec<String> = found.iter().map(|d| d.instance.clone()).collect();
                found
                    .into_iter()
                    .find(|d| {
                        d.instance == *want
                            || d.instance.starts_with(want.as_str())
                            || d.info.as_ref().is_some_and(|i| i.name == *want)
                    })
                    .ok_or_else(|| Error::NoSuchDevice {
                        wanted: want.clone(),
                        found: names.join(", "),
                    })
            }
        }
    }
}

/// Ask for `GET_INFO` on the subnet broadcast address and collect the unicast
/// replies (spec 5.5).
///
/// This is the "mDNS is broken on this machine today" escape hatch. Frame data
/// is never broadcast; this is one small control packet.
///
/// # Errors
///
/// [`Error::Io`] if the socket cannot be set up.
pub fn broadcast_probe(timeout: Duration, port: u16) -> Result<Vec<Device>> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;

    let req = Request::GetInfo;
    let mut out = vec![0u8; req.encoded_len()];
    let req_id = 0x5350u16; // arbitrary and non-zero: zero means "no reply"
    let n = req
        .write(req_id, &mut out)
        .map_err(|e| Error::Metadata(format!("{e:?} building GET_INFO")))?;

    let mut targets: Vec<Ipv4Addr> = net::local_ipv4()
        .into_iter()
        .filter(|i| !i.addr.is_loopback() && u32::from(i.mask) != 0)
        .map(|i| Ipv4Addr::from(u32::from(i.addr) | !u32::from(i.mask)))
        .collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        targets.push(Ipv4Addr::BROADCAST);
    }
    for t in &targets {
        // A refused broadcast on one interface should not stop the others.
        let _ = sock.send_to(&out[..n], SocketAddr::new(IpAddr::V4(*t), port));
    }

    let deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let mut found: BTreeMap<String, Device> = BTreeMap::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        sock.set_read_timeout(Some(left))?;
        let (got, from) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
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
        let Ok(pkt) = ControlPacket::parse(&buf[..got]) else {
            continue;
        };
        if !pkt.is_reply() || pkt.op != op::GET_INFO || pkt.req_id != req_id {
            continue;
        }
        let Ok(info) = DeviceInfo::parse(pkt.body) else {
            continue;
        };
        if !info.speaks_v1() {
            continue;
        }
        // The reply came from the control port; the frame port is in the SRV
        // record, which we do not have here, so fall back to the default.
        let frame = SocketAddr::new(from.ip(), screeny_proto::DEFAULT_FRAME_PORT);
        let control = SocketAddr::new(from.ip(), info.ctrl);
        let instance = if info.id.is_empty() {
            from.ip().to_string()
        } else {
            format!("screeny-{}", info.id)
        };
        found.insert(
            instance.clone(),
            Device {
                instance,
                host: None,
                frame,
                control,
                addresses: vec![from.ip()],
                info: Some(info),
            },
        );
    }
    Ok(found.into_values().collect())
}
