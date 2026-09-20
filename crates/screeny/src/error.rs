//! One error type for the whole sender.

use std::net::SocketAddr;

use screeny_proto::control::ErrorCode;

/// Anything that can go wrong talking to a device.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A socket operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// No reply to a control request after the retries spec 6.1 allows.
    #[error("no reply from {addr} to op {op:#04x} after {tries} tries ({timeout_ms} ms each)")]
    Timeout {
        /// Who we were asking.
        addr: SocketAddr,
        /// Opcode we sent.
        op: u8,
        /// How many attempts were made.
        tries: u32,
        /// Per-attempt timeout.
        timeout_ms: u64,
    },

    /// The device replied with an error code (spec 6.5).
    #[error("device returned {0:?}")]
    Device(ErrorCode),

    /// A reply arrived but did not parse.
    #[error("malformed reply from {addr}: {what}")]
    BadReply {
        /// Who sent it.
        addr: SocketAddr,
        /// What was wrong with it.
        what: String,
    },

    /// A discovery browse finished without finding anything.
    #[error("no screeny device found on {service} after {secs:.1} s")]
    NotFound {
        /// Service type browsed.
        service: &'static str,
        /// How long we waited.
        secs: f64,
    },

    /// The named instance was not among the ones that answered.
    #[error("no device named {wanted:?} (found: {found})")]
    NoSuchDevice {
        /// The name asked for.
        wanted: String,
        /// The names that did answer.
        found: String,
    },

    /// The mDNS daemon itself failed.
    #[error("mDNS: {0}")]
    Mdns(String),

    /// The device's TXT record or `GET_INFO` body did not parse.
    #[error("device metadata: {0}")]
    Metadata(String),

    /// Nothing in the device's codec list is something this sender can make.
    #[error("device advertises codecs [{0}], none of which this sender produces")]
    NoCommonCodec(String),

    /// A frame handed to the sender was the wrong shape.
    ///
    /// The one failure an embedder can cause by itself, and deliberately its
    /// own variant: everything else in this enum is the network's fault and
    /// [`crate::Link`] handles it without telling the caller.
    #[error("{what} is {got} bytes, expected {want}")]
    Frame {
        /// Which part was wrong, as a noun phrase.
        what: &'static str,
        /// What the caller passed.
        got: usize,
        /// What it should have been.
        want: usize,
    },

    /// An indexed frame had an index outside its palette.
    #[error("index {index} at pixel {pixel} is outside a palette of {palette}")]
    BadIndex {
        /// The offending index.
        index: u8,
        /// Where it was.
        pixel: usize,
        /// How many colours the palette has.
        palette: usize,
    },

    /// The caller asked for a payload budget that cannot work.
    #[error("payload budget {got} is outside 3..={max}")]
    Budget {
        /// What was asked for.
        got: usize,
        /// The protocol's ceiling.
        max: usize,
    },
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;

/// So an embedder whose own trait returns [`std::io::Result`] - the generative
/// art system's `Output::send` is exactly that - can write `?` and stop.
///
/// An [`Error::Io`] keeps its original kind and `errno`; everything else
/// becomes [`std::io::ErrorKind::Other`] carrying this error, so
/// [`Error::hint`] survives a round trip through `io::Error::downcast_ref`.
impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io) => io,
            other => std::io::Error::other(other),
        }
    }
}

/// Which machine's advice [`Error::hint`] should give.
///
/// "Nothing answered a browse" has a different first suspect on every
/// platform, and card 147 was a Linux container being told to open macOS
/// System Settings. The variants are the platforms whose failure modes this
/// crate actually knows something about; everything else gets the advice that
/// is true everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Platform {
    /// macOS, where Local Network privacy is the named failure mode (spec 9.3).
    MacOs,
    /// Linux, where it is the network namespace, the multicast route or the
    /// firewall.
    Linux,
    /// Anywhere else: only the platform-independent advice.
    Other,
}

impl Platform {
    /// The platform this binary was compiled for.
    pub const HOST: Platform = {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            Platform::MacOs
        }
        #[cfg(target_os = "linux")]
        {
            Platform::Linux
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
        {
            Platform::Other
        }
    };
}

/// The paragraph that is true on every platform: discovery is optional.
const ESCAPE: &str = "An empty browse is never fatal: pass `--addr IP[:port]` to skip discovery \
                      entirely, or `--broadcast` to ask over UDP instead.";

/// macOS Local Network privacy (spec 9.3).
const LOCAL_NETWORK: &str = "\
If this Mac is refusing local network access, open System Settings > Privacy & \
Security > Local Network and enable the terminal or binary you are running, \
then try again. A CLI started from Terminal or over SSH is normally exempt, \
but a re-signed, bundled or launchd-started binary is not, and closing the \
Terminal window that started a running sender can revoke the exemption \
mid-run.";

impl Error {
    /// A human-readable next step, when there is an obvious one, for the
    /// platform this binary was built for.
    ///
    /// See [`Error::hint_for`] for the advice itself and why it differs.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        self.hint_for(Platform::HOST)
    }

    /// [`Error::hint`], for a named platform.
    ///
    /// Public and pure so that both texts are built - and tested - wherever
    /// the crate is compiled, rather than only the one `cfg` selected. Card
    /// 147: the `cfg` belongs to [`Platform::HOST`] and nothing else.
    ///
    /// The two failure modes worth naming are different in kind:
    ///
    /// * **macOS**: Local Network privacy. A sandboxed or re-signed binary
    ///   that has not been granted the permission sees multicast silently
    ///   vanish and unicast to a LAN address fail with `EHOSTUNREACH`, with no
    ///   other symptom (spec 9.3). Code signing itself is card 015.
    /// * **Linux**: there is no such permission, and the suspects are all
    ///   network: a container on the default bridge cannot see the LAN's
    ///   multicast, an interface can lack a multicast route, and UDP 5353 can
    ///   be firewalled. A missing avahi is *not* a cause - this crate browses
    ///   for itself - but `avahi-browse` is a useful second opinion when the
    ///   host has it.
    #[must_use]
    pub fn hint_for(&self, platform: Platform) -> Option<String> {
        match self {
            Error::NotFound { .. } => Some(match platform {
                Platform::MacOs => format!(
                    "{LOCAL_NETWORK}\n\nTo tell \"the device is not advertising\" from \"this \
                     process cannot see multicast\", run `dns-sd -B _screeny._udp`, which uses \
                     Apple's own responder. {ESCAPE}"
                ),
                Platform::Linux => format!(
                    "Nothing advertised `_screeny._udp` where this process could see it. On \
                     Linux that is the network rather than a permission: a container on the \
                     default bridge does not carry the LAN's multicast (run it with `--network \
                     host`), an interface may have no multicast route (`ip link` should show \
                     MULTICAST, `ip maddr` the groups joined), and UDP 5353 may be firewalled. \
                     `avahi-browse -rt _screeny._udp` asks the host's own responder for a second \
                     opinion; avahi being absent is not itself a cause, since this browses for \
                     itself. {ESCAPE}"
                ),
                Platform::Other => ESCAPE.to_string(),
            }),
            Error::Io(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EHOSTUNREACH) | Some(libc::ENETUNREACH)
                ) =>
            {
                Some(match platform {
                    Platform::MacOs => format!(
                        "The address is on a local network and the OS refused to route to it.\n\n\
                         {LOCAL_NETWORK}"
                    ),
                    Platform::Linux => "The OS has no route to that address. From a container on \
                                        the default bridge there is none to the LAN: run it with \
                                        `--network host`, or give an address this namespace can \
                                        reach."
                        .to_string(),
                    Platform::Other => {
                        "The OS refused to route to that address. Check the interface and the \
                         route to that subnet."
                            .to_string()
                    }
                })
            }
            Error::Io(e) if e.raw_os_error() == Some(libc::ECONNREFUSED) => Some(
                "The host answered but nothing is listening on that port. Check the port \
                 number - the frame port is 49374 and the control port 49375 by default - \
                 or run `screeny discover` to see what is actually advertised."
                    .to_string(),
            ),
            Error::Timeout { addr, .. } if crate::net::is_private(addr) => {
                let common = format!(
                    "{addr} did not answer. Check the device is powered and on the same network; \
                     `screeny discover` will say whether it is advertising."
                );
                Some(match platform {
                    Platform::MacOs => format!("{common}\n\n{LOCAL_NETWORK}"),
                    Platform::Linux | Platform::Other => common,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screeny_proto::SERVICE_TYPE;

    fn not_found() -> Error {
        Error::NotFound {
            service: SERVICE_TYPE,
            secs: 3.0,
        }
    }

    fn unreachable() -> Error {
        Error::Io(std::io::Error::from_raw_os_error(libc::EHOSTUNREACH))
    }

    /// Card 147: the advice has to name tools that exist on the machine it is
    /// printed on. Both texts are built here whatever this host is, so
    /// neither branch can rot unnoticed.
    #[test]
    fn a_hint_names_only_tools_that_exist_on_its_platform() {
        let mac = not_found().hint_for(Platform::MacOs).expect("macOS advice");
        assert!(mac.contains("System Settings"), "{mac}");
        assert!(mac.contains("dns-sd -B _screeny._udp"), "{mac}");
        assert!(!mac.contains("avahi"), "{mac}");
        assert!(!mac.contains("--network host"), "{mac}");

        let linux = not_found().hint_for(Platform::Linux).expect("Linux advice");
        assert!(linux.contains("avahi-browse -rt _screeny._udp"), "{linux}");
        assert!(linux.contains("--network host"), "{linux}");
        assert!(linux.contains("5353"), "{linux}");
        assert!(!linux.contains("System Settings"), "{linux}");
        assert!(!linux.contains("dns-sd"), "{linux}");
        assert!(!linux.to_lowercase().contains("mac"), "{linux}");

        // The escape hatches are platform-independent and belong in all three.
        for p in [Platform::MacOs, Platform::Linux, Platform::Other] {
            let h = not_found().hint_for(p).expect("some advice everywhere");
            assert!(h.contains("--addr IP[:port]"), "{p:?}: {h}");
            assert!(h.contains("--broadcast"), "{p:?}: {h}");
        }
        let other = not_found().hint_for(Platform::Other).expect("plain advice");
        assert!(!other.contains("System Settings") && !other.contains("avahi"), "{other}");
    }

    /// The same rule for a refused route, which is the other place the macOS
    /// paragraph used to be attached unconditionally.
    #[test]
    fn an_unroutable_address_is_explained_per_platform() {
        let mac = unreachable().hint_for(Platform::MacOs).expect("macOS advice");
        assert!(mac.contains("System Settings"), "{mac}");
        let linux = unreachable().hint_for(Platform::Linux).expect("Linux advice");
        assert!(linux.contains("--network host"), "{linux}");
        assert!(!linux.contains("System Settings"), "{linux}");

        let addr = "192.168.7.221:49375".parse().unwrap();
        let timeout = Error::Timeout {
            addr,
            op: 0x01,
            tries: 3,
            timeout_ms: 250,
        };
        assert!(timeout
            .hint_for(Platform::MacOs)
            .is_some_and(|h| h.contains("System Settings")));
        assert!(timeout
            .hint_for(Platform::Linux)
            .is_some_and(|h| !h.contains("System Settings") && h.contains("screeny discover")));
    }

    /// The one `cfg` left: which platform's advice this binary prints.
    #[test]
    fn hint_uses_this_host_platform() {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        assert_eq!(Platform::HOST, Platform::MacOs);
        #[cfg(target_os = "linux")]
        assert_eq!(Platform::HOST, Platform::Linux);
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
        assert_eq!(Platform::HOST, Platform::Other);

        assert_eq!(not_found().hint(), not_found().hint_for(Platform::HOST));
    }
}
