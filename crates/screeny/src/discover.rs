//! Finding devices: mDNS browse (spec 5.4), the system resolver, and the
//! broadcast escape hatch (spec 5.5).
//!
//! [`Target`] is the "how to find it" half and [`Target::parse`] the grammar
//! of the one string a user types: an address is used as given, a name with a
//! dot or a port is looked up with the system resolver, and a bare name is a
//! DNS-SD instance to browse for.
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
///
/// Four ways, tried by [`Target::resolve`] in this order, and one string that
/// chooses between them: [`Target::parse`].
#[derive(Clone, Debug, Default)]
pub struct Target {
    /// An explicit `IP[:port]`, which skips discovery entirely.
    pub addr: Option<SocketAddr>,
    /// A host name for the **system resolver** - DNS, `/etc/hosts`, and
    /// whatever else `getaddrinfo` consults on this machine. Card 146: a name
    /// like `host.docker.internal` is neither an address nor a DNS-SD
    /// instance, and before this it was browsed for and reported missing.
    ///
    /// Resolving a host name yields an address and nothing else, so the
    /// control port is taken to be the frame port + 1 exactly as
    /// [`Device::from_addr`] does, and the `GET_INFO` handshake corrects it.
    pub host: Option<String>,
    /// The frame port to use with [`Target::host`]. `None` is
    /// [`screeny_proto::DEFAULT_FRAME_PORT`].
    pub port: Option<u16>,
    /// A DNS-SD instance name (or a unique prefix of one) to pick out.
    pub name: Option<String>,
    /// Browse window.
    pub timeout: Option<Duration>,
    /// Use the broadcast `GET_INFO` probe instead of mDNS (spec 5.5).
    pub broadcast: bool,
}

impl Target {
    /// Read one `--to`-style string: an address, a host name, or a DNS-SD
    /// instance name.
    ///
    /// The grammar, in the order it is tried. Nothing here does any I/O - a
    /// name is classified now and looked up later, on whichever thread calls
    /// [`Target::resolve`].
    ///
    /// | input | read as | resolved by |
    /// |---|---|---|
    /// | `10.0.0.5:49374`, `[::1]:49374`, `10.0.0.5`, `::1`, `[::1]` | an address | nothing; used as given |
    /// | `host.docker.internal`, `panel.lan:49374`, `localhost:5000` | a host name (a dot, or a port, or both) | the system resolver |
    /// | `screeny-4a00a4` | a DNS-SD instance name (no dots, no port) | an mDNS browse |
    ///
    /// Two edges are worth saying out loud, because they are the whole of the
    /// ambiguity:
    ///
    /// * **A bare undotted name stays an mDNS instance name**, which is what
    ///   it has always been and what `screeny-4a00a4` is. Give it a port, or
    ///   a dot, if you mean a host to look up.
    /// * **`something.local` is looked up by the system resolver first** and
    ///   browsed for second (as the instance name with `.local` removed). The
    ///   resolver is right for it on macOS and on a Linux host with nss-mdns,
    ///   and is fast when it works; in a slim container it fails in
    ///   milliseconds and the browse - which this crate does for itself, with
    ///   no responder needed - picks it up. The other order would pay a
    ///   three-second browse on every connect on the machines where the
    ///   resolver already knows the answer.
    #[must_use]
    pub fn parse(s: &str) -> Target {
        Self::read(s, false)
    }

    /// [`Target::parse`] for a string that is meant to be an address: a name
    /// with no dots is a host to look up rather than an instance to browse
    /// for.
    ///
    /// This is what `screeny --addr` means by its argument. `--addr` exists to
    /// *skip* discovery, so it must never turn into a browse.
    #[must_use]
    pub fn direct(s: &str) -> Target {
        Self::read(s, true)
    }

    fn read(s: &str, bare_is_a_host: bool) -> Target {
        let s = s.trim();
        // An address, with or without a port. `[::1]` (brackets, no port) is
        // not an `IpAddr` and not a `SocketAddr`, and people write it.
        if let Ok(a) = s.parse::<SocketAddr>() {
            return Target {
                addr: Some(a),
                ..Target::default()
            };
        }
        let unbracketed = s
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .unwrap_or(s);
        if let Ok(ip) = unbracketed.parse::<IpAddr>() {
            return Target {
                addr: Some(SocketAddr::new(ip, screeny_proto::DEFAULT_FRAME_PORT)),
                ..Target::default()
            };
        }
        // `name:port`. A port pins it to the resolver: an instance name has no
        // port, because the SRV record carries one.
        if let Some((h, p)) = s.rsplit_once(':') {
            if let (false, Ok(port)) = (h.is_empty(), p.parse::<u16>()) {
                return Target {
                    host: Some(h.to_string()),
                    port: Some(port),
                    ..Target::default()
                };
            }
        }
        if bare_is_a_host || s.contains('.') {
            return Target {
                host: Some(s.to_string()),
                ..Target::default()
            };
        }
        Target {
            name: Some(s.to_string()),
            ..Target::default()
        }
    }

    /// Resolve to exactly one device.
    ///
    /// **This blocks**, for as long as a browse window or a system resolver
    /// takes, which is why [`crate::Link`] calls it on its connect thread and
    /// re-calls it on every reconnect - so a name, DNS or mDNS, is followed
    /// when its answer changes.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when nothing answered a browse, [`Error::NoSuchDevice`]
    /// when something did but not the one asked for, and [`Error::Unresolved`]
    /// when a host name means nothing on this machine.
    pub fn resolve(&self) -> Result<Device> {
        if let Some(a) = self.addr {
            return Ok(Device::from_addr(a));
        }
        if let Some(host) = &self.host {
            return self.resolve_host(host);
        }
        self.browse_for(self.name.as_deref())
    }

    /// The system resolver, then - for a name that could also be an instance -
    /// an mDNS browse. See [`Target::parse`] for why in that order.
    fn resolve_host(&self, host: &str) -> Result<Device> {
        let port = self.port.unwrap_or(screeny_proto::DEFAULT_FRAME_PORT);
        let said = match std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) {
            Ok(addrs) => {
                // IPv4 first: the device's stack is IPv4 and so is the
                // simulator, so an IPv4 answer is the one that can work. An
                // IPv6-only name still resolves - it is just last.
                // `pick_address` then applies spec 5.4's preference within
                // that order, exactly as it does to a browse result.
                let mut ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
                ips.sort_by_key(|a| u8::from(a.is_ipv6()));
                if let Some(ip) = net::pick_address(&ips) {
                    let mut device = Device::from_addr(SocketAddr::new(ip, port));
                    device.instance = host.to_string();
                    device.host = Some(host.to_string());
                    device.addresses = ips;
                    return Ok(device);
                }
                "it resolved to no addresses".to_string()
            }
            Err(e) => format!("the system resolver said: {e}"),
        };

        let then = match instance_candidate(host) {
            Some(candidate) => match self.browse_for(Some(candidate)) {
                Ok(d) => return Ok(d),
                Err(e) => {
                    format!("; browsing {SERVICE_TYPE} for an instance called {candidate:?}: {e}")
                }
            },
            None => format!(
                "; it cannot be a {SERVICE_TYPE} instance name either (it has a dot in it), so \
                 nothing was browsed"
            ),
        };
        Err(Error::Unresolved {
            name: self.label(),
            tried: said + &then,
        })
    }

    /// Browse (or broadcast-probe) and pick the one instance asked for.
    fn browse_for(&self, want: Option<&str>) -> Result<Device> {
        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let mut found = if self.broadcast {
            broadcast_probe(timeout, DEFAULT_CONTROL_PORT)?
        } else {
            browse(timeout, if want.is_none() { Some(1) } else { None })?
        };
        if found.is_empty() {
            return Err(Error::NotFound {
                service: SERVICE_TYPE,
                secs: timeout.as_secs_f64(),
            });
        }
        match want {
            None => Ok(found.remove(0)),
            Some(want) => {
                let names: Vec<String> = found.iter().map(|d| d.instance.clone()).collect();
                found
                    .into_iter()
                    .find(|d| {
                        d.instance == want
                            || d.instance.starts_with(want)
                            || d.info.as_ref().is_some_and(|i| i.name == want)
                    })
                    .ok_or_else(|| Error::NoSuchDevice {
                        wanted: want.to_string(),
                        found: names.join(", "),
                    })
            }
        }
    }

    /// What this target was asked for, in the words it was asked in. For a
    /// status strip, a log line, or an error that has to name the name.
    #[must_use]
    pub fn label(&self) -> String {
        match (&self.addr, &self.host, &self.name) {
            (Some(a), _, _) => a.to_string(),
            (None, Some(h), _) => match self.port {
                Some(p) => format!("{h}:{p}"),
                None => h.clone(),
            },
            (None, None, Some(n)) => n.clone(),
            (None, None, None) => "the first panel found".to_string(),
        }
    }
}

/// The DNS-SD instance name a host name could also be: `screeny-4a00a4.local`
/// is `screeny-4a00a4`. `None` if what is left still has a dot in it, because
/// a DNS-SD instance name in this service does not.
fn instance_candidate(host: &str) -> Option<&str> {
    let base = host.strip_suffix('.').unwrap_or(host);
    // `.local` is a DNS label and so case-insensitive; people type `.Local`.
    let base = match base.len().checked_sub(".local".len()) {
        Some(n) if base[n..].eq_ignore_ascii_case(".local") => &base[..n],
        _ => base,
    };
    if base.is_empty() || base.contains('.') {
        None
    } else {
        Some(base)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Card 146's grammar, one row per shape. Pure: nothing here looks
    /// anything up.
    #[test]
    fn a_target_string_is_an_address_a_host_or_an_instance() {
        let addr = |s: &str| Target::parse(s).addr.map(|a| a.to_string());
        assert_eq!(addr("10.0.0.5:49374").as_deref(), Some("10.0.0.5:49374"));
        assert_eq!(addr(" 10.0.0.5 ").as_deref(), Some("10.0.0.5:49374"));
        assert_eq!(addr("[::1]:49374").as_deref(), Some("[::1]:49374"));
        assert_eq!(addr("::1").as_deref(), Some("[::1]:49374"));
        // Brackets without a port are neither an `IpAddr` nor a `SocketAddr`,
        // and people write them anyway.
        assert_eq!(addr("[::1]").as_deref(), Some("[::1]:49374"));

        // A name with a dot, or a port, or both: the system resolver's.
        for (s, host, port) in [
            (
                "host.docker.internal:49374",
                "host.docker.internal",
                Some(49374),
            ),
            ("host.docker.internal", "host.docker.internal", None),
            ("localhost:5000", "localhost", Some(5000)),
            ("workbench.local", "workbench.local", None),
        ] {
            let t = Target::parse(s);
            assert_eq!(t.host.as_deref(), Some(host), "{s}");
            assert_eq!(t.port, port, "{s}");
            assert!(t.addr.is_none() && t.name.is_none(), "{s}");
        }

        // A bare name is still an mDNS instance name, which is what it has
        // always been and what the panel is called.
        let t = Target::parse(" screeny-4a00a4 ");
        assert_eq!(t.name.as_deref(), Some("screeny-4a00a4"));
        assert!(t.addr.is_none() && t.host.is_none());

        // `--addr` means an address: a bare name there is a host to look up,
        // never a browse.
        let t = Target::direct("workbench");
        assert_eq!(t.host.as_deref(), Some("workbench"));
        assert!(t.name.is_none());
        assert_eq!(
            Target::direct("10.0.0.5").addr.map(|a| a.port()),
            Some(49374)
        );
    }

    #[test]
    fn a_label_says_what_was_asked_for() {
        assert_eq!(Target::parse("10.0.0.5").label(), "10.0.0.5:49374");
        assert_eq!(Target::parse("panel.lan").label(), "panel.lan");
        assert_eq!(Target::parse("panel.lan:5000").label(), "panel.lan:5000");
        assert_eq!(Target::parse("screeny-4a00a4").label(), "screeny-4a00a4");
        assert_eq!(Target::default().label(), "the first panel found");
    }

    /// `.local` falls back to a browse for the instance name; a dotted name
    /// cannot be an instance name and is not browsed for at all.
    #[test]
    fn only_an_undotted_name_can_also_be_an_instance() {
        assert_eq!(
            instance_candidate("screeny-4a00a4.local"),
            Some("screeny-4a00a4")
        );
        assert_eq!(
            instance_candidate("screeny-4a00a4.local."),
            Some("screeny-4a00a4")
        );
        assert_eq!(
            instance_candidate("screeny-4a00a4.Local"),
            Some("screeny-4a00a4")
        );
        assert_eq!(instance_candidate("workbench"), Some("workbench"));
        assert_eq!(instance_candidate("host.docker.internal"), None);
        assert_eq!(instance_candidate("a.b.local"), None);
        assert_eq!(instance_candidate(".local"), None);
    }

    /// A host name the resolver does know: the lookup happens in `resolve`,
    /// the port is kept, and the device is built as if the address had been
    /// given directly.
    #[test]
    fn a_host_name_resolves_through_the_system_resolver() {
        let d = Target::parse("localhost:49374")
            .resolve()
            .expect("localhost resolves everywhere this crate builds");
        // IPv4 wins over `::1`, because that is the family a panel - and the
        // simulator - listens on.
        assert_eq!(d.frame.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST), "{:?}", d.frame);
        assert_eq!(d.frame.port(), 49374);
        assert_eq!(
            d.control.port(),
            DEFAULT_CONTROL_PORT,
            "frame + 1 by convention"
        );
        assert_eq!(d.instance, "localhost");
        assert_eq!(d.host.as_deref(), Some("localhost"));

        // A non-default port keeps the +1 convention, as `--addr` always has.
        let d = Target::parse("localhost:6000").resolve().expect("resolves");
        assert_eq!((d.frame.port(), d.control.port()), (6000, 6001));
    }

    /// Card 146's whole point: a name that means nothing says so, names
    /// itself, and is not reported as a missing panel.
    #[test]
    fn a_name_that_resolves_to_nothing_says_which_name_and_what_was_tried() {
        // `.invalid` is reserved by RFC 2606 and must never resolve, so this
        // is a lookup failure by definition rather than by luck.
        let t = Target {
            timeout: Some(Duration::from_millis(1)),
            ..Target::parse("no-such-host.invalid:49374")
        };
        let started = Instant::now();
        let e = t.resolve().expect_err("must not resolve");
        let msg = e.to_string();
        assert!(matches!(e, Error::Unresolved { .. }), "{e:?}");
        assert!(msg.contains("no-such-host.invalid:49374"), "{msg}");
        assert!(msg.contains("is not an address"), "{msg}");
        assert!(
            msg.contains("it has a dot in it"),
            "a dotted name is not browsed for: {msg}"
        );
        assert!(
            !msg.contains("no screeny device found"),
            "this is not a missing panel: {msg}"
        );
        assert!(e.hint().is_some_and(|h| h.contains("--addr")), "{e:?}");
        // No browse was started, so this is a lookup and nothing else.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
