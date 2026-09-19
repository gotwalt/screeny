//! The `_screeny._udp` advertisement, spec section 5.
//!
//! The TXT record is not rebuilt here. It is [`crate::core::Core`]'s
//! `GET_INFO` body, taken apart into key/value pairs and handed to `mdns-sd`
//! as it stands - which is section 6.6's whole point, that a sender given a
//! bare IP gets identical metadata to one that browsed.
//!
//! # The one hard rule
//!
//! The instance name is `screeny-sim`. The real device on this bench is
//! `screeny`, and a second responder claiming that name would make discovery
//! answer with whichever replied first. [`crate::instance_is_reserved`]
//! refuses the collision before a socket is opened.

use std::io;
use std::net::Ipv4Addr;

use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::config::Config;

/// A live registration. Dropping it, or [`Advertisement::shutdown`], sends
/// the goodbye packet of spec section 5.3.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// Register the service.
    ///
    /// # Errors
    ///
    /// Anything `mdns-sd` refuses: no usable interface, a malformed instance
    /// name, a TXT key it will not carry.
    pub fn start(
        cfg: &Config,
        frame_port: u16,
        info_bytes: &[u8],
        addr: Ipv4Addr,
    ) -> io::Result<Self> {
        let daemon = ServiceDaemon::new().map_err(other)?;
        // Section 1: v1 is IPv4 only.
        let _ = daemon.disable_interface(mdns_sd::IfKind::IPv6);

        // Section 5.2's keys, in section 5.2's order, from the same bytes the
        // device answers GET_INFO with.
        let props: Vec<(String, String)> = screeny_proto::txt::iter(info_bytes)
            .map(|e| {
                (
                    String::from_utf8_lossy(e.key).into_owned(),
                    String::from_utf8_lossy(e.value.unwrap_or(b"")).into_owned(),
                )
            })
            .collect();

        // Section 5.1: the SRV target is the host name and the *frame* port.
        let host = format!("{}.local.", cfg.instance);
        let info = ServiceInfo::new(
            screeny_proto::SERVICE_TYPE,
            &cfg.instance,
            &host,
            std::net::IpAddr::V4(addr),
            frame_port,
            &props[..],
        )
        .map_err(other)?;
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(other)?;
        Ok(Advertisement { daemon, fullname })
    }

    /// The registered instance's fully qualified name.
    #[must_use]
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Withdraw the record and stop the daemon.
    pub fn shutdown(self) {
        // Unregister first so the goodbye packet goes out before the daemon
        // stops listening (section 5.3).
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
    }
}

fn other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}
