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

impl Error {
    /// A human-readable next step, when there is an obvious one.
    ///
    /// macOS Local Network privacy is the failure mode worth naming: a
    /// sandboxed or re-signed binary that has not been granted the permission
    /// sees multicast silently vanish and unicast to a LAN address fail with
    /// `EHOSTUNREACH`, with no other symptom (spec 9.3). Code signing itself
    /// is card 015; this is only about saying so.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        const LOCAL_NETWORK: &str = "\
If this Mac is refusing local network access, open System Settings > Privacy & \
Security > Local Network and enable the terminal or binary you are running, \
then try again. A CLI started from Terminal or over SSH is normally exempt, \
but a re-signed, bundled or launchd-started binary is not, and closing the \
Terminal window that started a running sender can revoke the exemption \
mid-run.";
        match self {
            Error::NotFound { .. } => Some(format!(
                "{LOCAL_NETWORK}\n\nTo tell \"the device is not advertising\" from \"this process \
                 cannot see multicast\", run `dns-sd -B _screeny._udp`, which uses Apple's own \
                 responder. An empty browse is never fatal: pass `--addr IP[:port]` to skip \
                 discovery entirely, or `--broadcast` to ask over UDP instead."
            )),
            Error::Io(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EHOSTUNREACH) | Some(libc::ENETUNREACH)
                ) =>
            {
                Some(format!(
                    "The address is on a local network and the OS refused to route to it.\n\n\
                     {LOCAL_NETWORK}"
                ))
            }
            Error::Timeout { addr, .. } if crate::net::is_private(addr) => Some(format!(
                "{addr} did not answer. Check the device is powered and on the same network; \
                 `screeny discover` will say whether it is advertising.\n\n{LOCAL_NETWORK}"
            )),
            _ => None,
        }
    }
}
