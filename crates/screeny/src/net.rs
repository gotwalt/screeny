//! Socket setup and the two bits of host networking the spec asks for:
//! `IP_DONTFRAG`, and picking the right address when a device resolves to
//! several (spec 9.2, 5.4).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::os::unix::io::AsRawFd;

/// Set the local "do not fragment" bit, so an over-budget datagram fails here
/// rather than being fragmented and then silently dropped by the device
/// (embassy-net does not reassemble; spec 1).
///
/// Best effort: a kernel that does not know the option is not a reason to
/// refuse to send.
pub fn set_dontfrag(sock: &UdpSocket) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const IP_DONTFRAG: libc::c_int = 28;

    let fd = sock.as_raw_fd();
    let rc = unsafe {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            let on: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                IP_DONTFRAG,
                std::ptr::addr_of!(on).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        }
        #[cfg(target_os = "linux")]
        {
            let mode: libc::c_int = libc::IP_PMTUDISC_DO;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_MTU_DISCOVER,
                std::ptr::addr_of!(mode).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
        {
            let _ = fd;
            0
        }
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Ask for interactive-video treatment: `IP_TOS` = AF41.
///
/// Whether the bench AP honours DSCP on the **downlink** to the ESP32 is the
/// part that matters and is untested - that is card 013 - so this is opt-in
/// and failures are ignored (spec 9.2).
pub fn set_qos(sock: &UdpSocket) {
    let tos: libc::c_int = 0x88; // AF41
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_TOS,
            std::ptr::addr_of!(tos).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// Bind an ephemeral UDP socket and `connect` it to `target`.
///
/// Connecting caches the route and turns later sends into `send()`, which is
/// what the pacing loop wants; it also means a `TELEMETRY` reply from the
/// device's frame port (spec 6.4) is the only thing this socket can receive.
pub fn connected_socket(target: SocketAddr, dontfrag: bool, qos: bool) -> io::Result<UdpSocket> {
    let bind: SocketAddr = match target {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
    };
    let sock = UdpSocket::bind(bind)?;
    sock.connect(target)?;
    if dontfrag {
        // Not fatal: some stacks refuse the option on an unconnected family.
        let _ = set_dontfrag(&sock);
    }
    if qos {
        set_qos(&sock);
    }
    Ok(sock)
}

// ------------------------------------------------ card 164: what it cost ----

/// What a datagram costs on the wire beyond its payload: an IPv4 header (20 B)
/// and a UDP header (8 B).
///
/// Everything this crate counts is **payload**, because that is the only thing
/// user space can see honestly. A caller that wants the figure a router would
/// bill adds this per datagram; [`Wire::on_the_wire`] is that sum. There is no
/// equivalent for TCP - retransmissions, ACKs and the handshake are invisible
/// from up here - so an HTTP byte count is the bytes written and read and
/// nothing more.
pub const UDP_OVERHEAD: u64 = 28;

/// Bytes and datagrams one way on one socket (card 164).
///
/// Two `u64` added where a counter is already being incremented: no lock, no
/// allocation, nothing logged, and nothing that a frame path can feel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wire {
    /// Payload bytes: what was handed to `send`, or what `recv` returned.
    /// **Headers are not in here** - see [`UDP_OVERHEAD`].
    pub bytes: u64,
    /// Datagrams.
    pub packets: u64,
}

impl Wire {
    /// Count one datagram of `bytes` payload.
    pub fn add(&mut self, bytes: usize) {
        self.bytes += bytes as u64;
        self.packets += 1;
    }

    /// Fold another counter into this one.
    pub fn fold(&mut self, other: Wire) {
        self.bytes += other.bytes;
        self.packets += other.packets;
    }

    /// Payload plus `overhead` bytes per datagram: what the link actually
    /// carried. Pass [`UDP_OVERHEAD`] for UDP and `0` for a stream.
    #[must_use]
    pub fn on_the_wire(&self, overhead: u64) -> u64 {
        self.bytes + self.packets * overhead
    }
}

/// One socket's traffic, both ways (card 164).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    /// What this host sent.
    pub out: Wire,
    /// What it received.
    pub inbound: Wire,
}

impl Traffic {
    /// Fold another counter into this one.
    pub fn fold(&mut self, other: Traffic) {
        self.out.fold(other.out);
        self.inbound.fold(other.inbound);
    }
}

/// True if `a` is in one of the ranges a home LAN uses. Used only to decide
/// whether a failure is worth attaching the Local Network hint to.
#[must_use]
pub fn is_private(a: &SocketAddr) -> bool {
    match a.ip() {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// An IPv4 address of one of this host's interfaces, with its netmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Iface {
    /// The interface's address.
    pub addr: Ipv4Addr,
    /// Its netmask.
    pub mask: Ipv4Addr,
}

impl Iface {
    /// True if `a` is on this interface's subnet.
    #[must_use]
    pub fn contains(&self, a: Ipv4Addr) -> bool {
        let (a, n, m) = (
            u32::from(a),
            u32::from(self.addr),
            u32::from(self.mask),
        );
        m != 0 && (a & m) == (n & m)
    }
}

/// Every IPv4 address this host has, with netmasks.
///
/// Uses `getifaddrs(3)`, which is the only unsafe code in this crate. The
/// list is advisory: if it comes back empty, callers fall back to "take the
/// first address the device offered".
#[must_use]
pub fn local_ipv4() -> Vec<Iface> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills `head` with an owned list on success, which
    // is freed below and never escapes this function.
    unsafe {
        if libc::getifaddrs(&raw mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null()
                && (*ifa.ifa_addr).sa_family as libc::c_int == libc::AF_INET
                && !ifa.ifa_netmask.is_null()
            {
                let a = &*ifa.ifa_addr.cast::<libc::sockaddr_in>();
                let m = &*ifa.ifa_netmask.cast::<libc::sockaddr_in>();
                out.push(Iface {
                    addr: Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)),
                    mask: Ipv4Addr::from(u32::from_be(m.sin_addr.s_addr)),
                });
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

/// Pick the best of the addresses a service resolved to.
///
/// Spec 5.4: prefer an IPv4 address on the same subnet as one of the host's
/// interfaces. Loopback wins outright (that is a simulator or a test), then a
/// same-subnet address, then any IPv4, then anything at all.
#[must_use]
pub fn pick_address(addrs: &[IpAddr]) -> Option<IpAddr> {
    let ifaces = local_ipv4();
    let v4 = |a: &&IpAddr| matches!(a, IpAddr::V4(_));
    addrs
        .iter()
        .find(|a| a.is_loopback())
        .or_else(|| {
            addrs.iter().find(|a| match a {
                IpAddr::V4(v) => ifaces.iter().any(|i| i.contains(*v)),
                IpAddr::V6(_) => false,
            })
        })
        .or_else(|| addrs.iter().find(v4))
        .or_else(|| addrs.first())
        .copied()
}
