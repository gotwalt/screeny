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
/// To look for one instance by name, use [`browse_for_name`]: it stops the
/// moment that instance answers instead of counting.
///
/// # Errors
///
/// [`Error::Mdns`] if the daemon could not be started or the browse rejected.
pub fn browse(timeout: Duration, want: Option<usize>) -> Result<Vec<Device>> {
    browse_until(timeout, |found| {
        want.is_some_and(|n| found.len() >= n)
    })
}

/// Browse for one instance by name, stopping as soon as **that** instance has
/// resolved (card 176).
///
/// The returned vector is everything that had resolved by the time the browse
/// stopped, in instance-name order; [`Target::browse_for`] hands it to
/// [`pick`], which applies the same rule again and, when nothing matched, can
/// name every instance that did answer.
///
/// # When it can stop early, and when it cannot
///
/// A DNS-SD instance name is unique on a link - that is what conflict
/// resolution in RFC 6762 §9 is for - so once the instance called `want` has
/// resolved, no later answer can change which device was meant. That case
/// returns immediately.
///
/// The two looser matches cannot: whether `want` is a *unique* prefix, and
/// whether one panel or two are calling themselves `want` in their TXT
/// records, are questions only the closed window can answer. Those keep
/// browsing, which also keeps [`Error::NoSuchDevice`] able to list everything
/// that answered.
///
/// # Errors
///
/// As [`browse`].
pub fn browse_for_name(timeout: Duration, want: &str) -> Result<Vec<Device>> {
    browse_until(timeout, |found| found.contains_key(want))
}

/// One turn of a browse loop: what arrived, or that nothing more will.
// One of these exists at a time, as a return value that is matched and
// dropped immediately, so boxing the device would buy an allocation per
// resolve and save nothing.
#[allow(clippy::large_enum_variant)]
enum Step {
    /// A service resolved into a device this sender can talk to.
    Resolved(Device),
    /// Something else happened (a service was added, removed, or ignored).
    Other,
    /// The window closed, or the event source went away.
    Done,
}

/// The browse loop itself, over any source of [`Step`]s.
///
/// Split out from [`browse`] so the stop condition - the whole of card 176 -
/// can be tested against a scripted stream of resolves with no daemon, no
/// multicast and no device on the bench. `next` is given the time left in the
/// window and must not outlast it; `stop` is asked after every resolve.
fn collect_until(
    timeout: Duration,
    mut next: impl FnMut(Duration) -> Step,
    mut stop: impl FnMut(&BTreeMap<String, Device>) -> bool,
) -> Vec<Device> {
    let deadline = Instant::now() + timeout;
    let mut found: BTreeMap<String, Device> = BTreeMap::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match next(left) {
            Step::Resolved(d) => {
                found.insert(d.instance.clone(), d);
                if stop(&found) {
                    break;
                }
            }
            Step::Other => {}
            Step::Done => break,
        }
    }
    found.into_values().collect()
}

/// [`collect_until`] driven by a real mDNS daemon.
fn browse_until(
    timeout: Duration,
    stop: impl FnMut(&BTreeMap<String, Device>) -> bool,
) -> Result<Vec<Device>> {
    let daemon = ServiceDaemon::new().map_err(|e| Error::Mdns(e.to_string()))?;
    let rx = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| Error::Mdns(e.to_string()))?;

    let found = collect_until(
        timeout,
        |left| match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(svc)) => match device_from_service(&svc) {
                Some(d) => Step::Resolved(d),
                None => Step::Other,
            },
            Ok(_) => Step::Other,
            Err(_) => Step::Done,
        },
        stop,
    );
    let _ = daemon.shutdown();
    Ok(found)
}

/// How a device answers to a name, strongest first.
///
/// The order is the precedence [`pick`] applies: a device whose instance name
/// *is* the name asked for beats one that merely starts with it, however the
/// two sort alphabetically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    /// `want` is this device's DNS-SD instance name. Unique on a link.
    Instance,
    /// `want` is the friendly `name=` in its TXT record. Not unique: two
    /// panels can be given the same one.
    Friendly,
    /// `want` is a prefix of its instance name. Unique only if no other
    /// instance starts with it, which is knowable only once browsing stops.
    Prefix,
}

/// Which way, if any, `d` answers to `want`.
fn match_kind(d: &Device, want: &str) -> Option<Match> {
    if d.instance == want {
        Some(Match::Instance)
    } else if d.info.as_ref().is_some_and(|i| i.name == want) {
        Some(Match::Friendly)
    } else if d.instance.starts_with(want) {
        Some(Match::Prefix)
    } else {
        None
    }
}

/// Pick the one device called `want` out of everything that answered.
///
/// Strongest match wins outright ([`Match`]); within one kind of match, two
/// candidates mean the name named neither, so it is
/// [`Error::AmbiguousName`] rather than an arbitrary pick. An exact instance
/// name can never be ambiguous, so naming a panel in full always works.
fn pick(found: Vec<Device>, want: &str) -> Result<Device> {
    let Some(best) = found.iter().filter_map(|d| match_kind(d, want)).min() else {
        return Err(Error::NoSuchDevice {
            wanted: want.to_string(),
            found: names_of(&found),
        });
    };
    let mut hits: Vec<Device> = found
        .into_iter()
        .filter(|d| match_kind(d, want) == Some(best))
        .collect();
    if hits.len() > 1 {
        return Err(Error::AmbiguousName {
            wanted: want.to_string(),
            found: names_of(&hits),
        });
    }
    Ok(hits.remove(0))
}

/// Instance names, in the order given, for an error that has to say what did
/// answer.
fn names_of(devices: &[Device]) -> String {
    devices
        .iter()
        .map(|d| d.instance.clone())
        .collect::<Vec<_>>()
        .join(", ")
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
    /// A DNS-SD instance name to pick out, or the panel's friendly `name=`,
    /// or a prefix of an instance name that is unique among the devices that
    /// answer.
    ///
    /// Card 176: only the first of those three can end a browse early, and
    /// only the first is guaranteed unique. A prefix or a friendly name that
    /// matches two panels is [`Error::AmbiguousName`].
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
    ///
    /// Card 176: a named browse stops the moment that instance answers, so
    /// naming a panel - the preferred way, because it follows a DHCP lease -
    /// costs what an unnamed browse costs and not the whole window. See
    /// [`browse_for_name`] for which matches can stop early and which cannot.
    fn browse_for(&self, want: Option<&str>) -> Result<Device> {
        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let mut found = match (self.broadcast, want) {
            (true, _) => broadcast_probe(timeout, DEFAULT_CONTROL_PORT)?,
            (false, Some(want)) => browse_for_name(timeout, want)?,
            (false, None) => browse(timeout, Some(1))?,
        };
        if found.is_empty() {
            return Err(Error::NotFound {
                service: SERVICE_TYPE,
                secs: timeout.as_secs_f64(),
            });
        }
        match want {
            None => Ok(found.remove(0)),
            Some(want) => pick(found, want),
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

/// The control addresses a broadcast probe asks: the subnet broadcast address
/// of every usable IPv4 interface, on `port`.
///
/// Separate from [`probe`] so that a caller which *cannot* broadcast can say
/// where to ask instead. That is not a test-only concern: a container with no
/// broadcast reachability to the panel's subnet, which is the Docker case the
/// studio is built for, has the same problem and the same answer.
///
/// [`Ipv4Addr::BROADCAST`] is the fallback when this machine has no usable
/// interface to compute a directed broadcast from.
#[must_use]
pub fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
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
    targets
        .into_iter()
        .map(|t| SocketAddr::new(IpAddr::V4(t), port))
        .collect()
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
    probe(timeout, &broadcast_targets(port))
}

/// Send one `GET_INFO` to each of `to` and collect the unicast replies, for
/// `timeout` in total.
///
/// [`broadcast_probe`] is this with [`broadcast_targets`]; anything else is a
/// caller that knows where to ask - a list of panel control addresses in a
/// container, or two simulators on loopback in a test. The reply is what
/// identifies the device, not the address it was asked at: every answer
/// carries the panel's own `id=`, which is what makes a panel found at a new
/// address the *same* panel.
///
/// One socket, one datagram per destination, one window: nothing here scales
/// with how long it is left running.
///
/// # Errors
///
/// [`Error::Io`] if the socket cannot be set up.
pub fn probe(timeout: Duration, to: &[SocketAddr]) -> Result<Vec<Device>> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    // Harmless for a unicast destination, and required for a broadcast one.
    sock.set_broadcast(true)?;

    let req = Request::GetInfo;
    let mut out = vec![0u8; req.encoded_len()];
    let req_id = 0x5350u16; // arbitrary and non-zero: zero means "no reply"
    let n = req
        .write(req_id, &mut out)
        .map_err(|e| Error::Metadata(format!("{e:?} building GET_INFO")))?;

    for t in to {
        // A refused broadcast on one interface should not stop the others.
        let _ = sock.send_to(&out[..n], t);
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
        // record, which a probe does not have. The guess is the same `+1`
        // convention [`Device::from_addr`] and `--addr` have always used, and
        // for a device on the spec's ports it *is*
        // [`screeny_proto::DEFAULT_FRAME_PORT`] - 49375 - 1 = 49374 - so this
        // changes nothing for a real panel and makes the probe usable against
        // a simulator on a port pair of its own. A device that answers with
        // `ctrl=0` has said nothing usable and is ignored.
        let Some(frame_port) = info.ctrl.checked_sub(1) else {
            continue;
        };
        let frame = SocketAddr::new(from.ip(), frame_port);
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

    // ---- Card 176: when a browse may stop -------------------------------
    //
    // These drive [`collect_until`] - the same loop a real browse runs - from
    // a scripted stream of resolves. No daemon, no multicast, nothing that
    // could see or be seen by the panel on this bench, and the timings are
    // the loop's own rather than the network's.

    /// A device that answers to `instance`, and optionally to `friendly`.
    fn dev(instance: &str, friendly: &str) -> Device {
        let mut d = Device::from_addr("10.0.0.5:49374".parse().unwrap());
        d.instance = instance.to_string();
        d.info = Some(DeviceInfo {
            name: friendly.to_string(),
            ..DeviceInfo::default()
        });
        d
    }

    /// An event source that hands over `events` at the offsets given, then
    /// sleeps out whatever is left of the window - which is what a real
    /// browse that hears nothing more does.
    fn scripted(events: Vec<(Duration, Device)>) -> impl FnMut(Duration) -> Step {
        let started = Instant::now();
        let mut queue = events.into_iter();
        move |left| {
            let Some((at, d)) = queue.next() else {
                std::thread::sleep(left);
                return Step::Done;
            };
            let wait = at.saturating_sub(started.elapsed());
            if wait >= left {
                std::thread::sleep(left);
                return Step::Done;
            }
            std::thread::sleep(wait);
            Step::Resolved(d)
        }
    }

    /// The card itself: the instance asked for answers early in a three
    /// second window, and the browse returns then instead of at the end.
    /// Collecting every device still costs the whole window, because
    /// "everything" is only known when nothing more is coming.
    #[test]
    fn a_named_browse_returns_when_that_instance_answers() {
        let script = || {
            vec![
                (Duration::from_millis(20), dev("screeny-aaaa", "kitchen")),
                (Duration::from_millis(60), dev("screeny-bbbb", "desk")),
                (Duration::from_millis(100), dev("screeny-cccc", "shelf")),
            ]
        };

        // Named: stops at the wanted instance, ~60 ms into a 3 s window.
        let started = Instant::now();
        let found = collect_until(DEFAULT_TIMEOUT, scripted(script()), |f| {
            f.contains_key("screeny-bbbb")
        });
        let named = started.elapsed();
        assert!(
            named < Duration::from_millis(600),
            "a named browse waited {named:?} of a {DEFAULT_TIMEOUT:?} window"
        );
        assert_eq!(pick(found, "screeny-bbbb").unwrap().instance, "screeny-bbbb");

        // Unnamed, wanting every device: the whole window, as before.
        let started = Instant::now();
        let found = collect_until(DEFAULT_TIMEOUT, scripted(script()), |_| false);
        let all = started.elapsed();
        assert!(
            all >= DEFAULT_TIMEOUT,
            "an unnamed browse returned after {all:?}, short of the window"
        );
        assert_eq!(found.len(), 3, "an unnamed browse still returns every device");
        println!("card 176: named browse {named:?}, collect-everything browse {all:?}");
    }

    /// The other half: a name nothing answers to still waits the window out,
    /// so the error can list what did answer.
    #[test]
    fn a_name_nothing_answers_to_still_costs_the_window() {
        let window = Duration::from_millis(400);
        let started = Instant::now();
        let found = collect_until(
            window,
            scripted(vec![
                (Duration::from_millis(20), dev("screeny-aaaa", "kitchen")),
                (Duration::from_millis(60), dev("screeny-bbbb", "desk")),
            ]),
            |f| f.contains_key("screeny-zzzz"),
        );
        assert!(started.elapsed() >= window, "{:?}", started.elapsed());
        let e = pick(found, "screeny-zzzz").expect_err("no such device");
        let msg = e.to_string();
        assert!(matches!(e, Error::NoSuchDevice { .. }), "{e:?}");
        assert!(msg.contains("screeny-aaaa") && msg.contains("screeny-bbbb"), "{msg}");
    }

    // ---- Card 141: a probe that can be told where to ask -----------------

    /// A device that answers `GET_INFO` on loopback, once, and then stops.
    ///
    /// Hand-rolled rather than `screeny-sim`, so this crate keeps no
    /// dev-dependency on a crate that depends on it. Blocking socket, one read
    /// timeout, one answer, joined by the test: nothing here can outlive the
    /// test that started it.
    fn fake_device(id: &'static str) -> (u16, std::thread::JoinHandle<()>) {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let port = sock.local_addr().expect("bound").port();
        sock.set_read_timeout(Some(Duration::from_secs(5))).expect("a read timeout");
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; MAX_UDP_PAYLOAD];
            let Ok((n, from)) = sock.recv_from(&mut buf) else { return };
            let Ok(pkt) = ControlPacket::parse(&buf[..n]) else { return };
            let info = screeny_proto::txt::DeviceInfo {
                proto: "1",
                codecs: "16",
                // What the panel says its control port is - here, the port
                // this responder is actually listening on.
                ctrl: port,
                id,
                name: "fake",
                ..screeny_proto::txt::DeviceInfo::DEFAULT
            };
            let mut txt = [0u8; 256];
            let len = info.write(&mut txt).expect("a TXT record");
            let mut out = [0u8; MAX_UDP_PAYLOAD];
            let reply = screeny_proto::control::Reply::Info(&txt[..len]);
            let n = reply.write(op::GET_INFO, pkt.req_id, &mut out).expect("a reply");
            let _ = sock.send_to(&out[..n], from);
        });
        (port, handle)
    }

    /// The card 141 half of spec 5.5: a probe can be told exactly where to
    /// ask, every answer names its own device, and a destination nobody is
    /// listening at costs only the window.
    #[test]
    fn a_probe_can_be_pointed_at_named_addresses() {
        let (port, responder) = fake_device("abc123");
        let at = SocketAddr::from(([127, 0, 0, 1], port));
        // A second destination with nothing behind it: the probe must not
        // care, which is what a broadcast that reaches switched-off panels is.
        let nobody = SocketAddr::from(([127, 0, 0, 1], 1));

        let started = Instant::now();
        let found = probe(Duration::from_secs(2), &[nobody, at]).expect("the socket");
        let took = started.elapsed();
        responder.join().expect("the responder finished");

        assert_eq!(found.len(), 1, "{found:?}");
        let d = &found[0];
        assert_eq!(d.instance, "screeny-abc123", "the reply names the device, not the address");
        assert_eq!(d.info.as_ref().expect("info").id, "abc123");
        assert_eq!(d.control, at, "the control port is where it answered from");
        assert_eq!(d.frame.port(), port - 1, "the frame port is the `+1` convention run backwards");
        println!("card 141: a two-address probe answered in {took:?}");
    }

    /// The default destinations are still the subnet broadcast addresses, on
    /// the port asked for, and never loopback - so `--broadcast` is what it
    /// always was.
    #[test]
    fn the_default_probe_targets_are_the_subnet_broadcasts() {
        let targets = broadcast_targets(DEFAULT_CONTROL_PORT);
        assert!(!targets.is_empty(), "there is always a fallback");
        for t in &targets {
            assert_eq!(t.port(), DEFAULT_CONTROL_PORT, "{t}");
            assert!(t.is_ipv4(), "spec 5.5 is IPv4 broadcast: {t}");
            assert!(!t.ip().is_loopback(), "a broadcast probe is not for loopback: {t}");
        }
    }

    /// The prefix rule, stated: an exact instance name wins over a longer
    /// instance it is a prefix of and over a friendly name, a unique prefix
    /// or friendly name picks its device, and an ambiguous one is an error
    /// rather than whichever sorted first.
    #[test]
    fn a_prefix_must_be_unique_but_a_full_instance_name_never_is_ambiguous() {
        let all = || {
            vec![
                dev("screeny-4a00a4", "desk"),
                dev("screeny-4a00b7", "desk"),
                dev("screeny-7f", "screeny-4a00a4"),
                dev("screeny-9c", "shelf"),
            ]
        };

        // Exact instance name: unambiguous even though `screeny-4a00a4` is
        // also the friendly name of another device, and even though it sorts
        // first among devices it is a prefix of.
        assert_eq!(pick(all(), "screeny-4a00a4").unwrap().instance, "screeny-4a00a4");
        // A unique prefix still resolves, after the full window.
        assert_eq!(pick(all(), "screeny-4a00b").unwrap().instance, "screeny-4a00b7");
        assert_eq!(pick(all(), "screeny-7").unwrap().instance, "screeny-7f");
        // A friendly name resolves when only one panel wears it, and is
        // ambiguous when two do - the same rule as a prefix.
        assert_eq!(pick(all(), "shelf").unwrap().instance, "screeny-9c");
        assert!(matches!(
            pick(all(), "desk"),
            Err(Error::AmbiguousName { .. })
        ));
        assert_eq!(
            pick(
                vec![dev("screeny-aaaa", "kitchen"), dev("screeny-bbbb", "desk")],
                "kitchen"
            )
            .unwrap()
            .instance,
            "screeny-aaaa"
        );
        // A prefix of two instances names neither.
        let e = pick(all(), "screeny-4a00").expect_err("ambiguous");
        assert!(matches!(e, Error::AmbiguousName { .. }), "{e:?}");
        let msg = e.to_string();
        assert!(msg.contains("screeny-4a00a4") && msg.contains("screeny-4a00b7"), "{msg}");
        assert!(!msg.contains("screeny-7f"), "only the ones it matched: {msg}");
    }
}
