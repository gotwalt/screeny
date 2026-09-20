//! Two helpers: which of this machine's addresses to print on the status
//! screen when the device is bound to `0.0.0.0`, and the address the simulated
//! device believes it holds.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// The local IPv4 address the kernel would use to reach the LAN.
///
/// `connect` on a UDP socket sends nothing - it only resolves a route - so
/// this touches no network and contacts nothing. The address it asks about is
/// in TEST-NET-1 (RFC 5737), which exists precisely to be unroutable.
#[must_use]
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() => Some(*a.ip()),
        _ => None,
    }
}

/// The address the simulated device reports as its own: what it is bound to,
/// or this machine's LAN address when it is bound to `0.0.0.0`.
///
/// One function, so the status screen, the provisioning machine's
/// `Joined { ip }` and `GET /api/v1/status` cannot name three different
/// addresses for the same device.
#[must_use]
pub fn display_addr(bind: IpAddr) -> Ipv4Addr {
    match bind {
        IpAddr::V4(a) if !a.is_unspecified() => a,
        _ => local_ipv4().unwrap_or(Ipv4Addr::LOCALHOST),
    }
}
