//! One helper: which of this machine's addresses to print on the status
//! screen when the device is bound to `0.0.0.0`.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

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
