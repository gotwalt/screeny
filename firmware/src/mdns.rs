//! DNS-SD responder, so senders find the panel by name instead of by IP.
//!
//! Card 003 owns the service definition; this exists to prove that the crates
//! resolve and compile together on this target, because `edge-mdns` ships no
//! `no_std` example and the question "does mDNS work here" would otherwise
//! stay open until someone had hardware in hand.
//!
//! Untested against a real network. The two things most likely to be wrong
//! are whether multicast frames reach us through esp-radio at all, and
//! whether the buffers below are big enough for a real query.

use core::convert::Infallible;
use core::net::{Ipv4Addr, Ipv6Addr};

use edge_mdns::buf::VecBufAccess;
use edge_mdns::domain::base::Ttl;
use edge_mdns::host::{Host, Service, ServiceAnswers};
use edge_mdns::io::{self, IPV4_DEFAULT_SOCKET};
use edge_mdns::HostAnswersMdnsHandler;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use log::{info, warn};

use crate::{mk_static, CONTROL_PORT, COLS, FRAME_PORT, ROWS};

/// The name the panel answers to: `screeny.local`, service `_screeny._udp`.
const HOSTNAME: &str = "screeny";
const SERVICE: &str = "_screeny";
const PROTOCOL: &str = "_udp";

/// An mDNS query or response fits in one datagram many times over; 1500 is
/// the honest ceiling and costs 3 KB of the two buffers together.
const MDNS_BUF: usize = 1500;

/// `esp_hal::rng::Rng` in the shape `edge-mdns` wants. Implementing the
/// fallible trait is enough: `rand_core` blankets `Rng` over anything whose
/// error is `Infallible`.
struct HwRng(esp_hal::rng::Rng);

impl rand_core::TryRng for HwRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok(self.0.random())
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(((self.0.random() as u64) << 32) | self.0.random() as u64)
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        for chunk in dst.chunks_mut(4) {
            let word = self.0.random().to_ne_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }
}

#[embassy_executor::task]
pub async fn mdns_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    let Some(config) = stack.config_v4() else {
        warn!("mdns: no IPv4 address, giving up");
        return;
    };
    let ipv4: Ipv4Addr = config.address.address();

    // One socket, and buffers big enough for the largest query we might see.
    let buffers = mk_static!(UdpBuffers<1, MDNS_BUF, MDNS_BUF, 2>, UdpBuffers::new());
    let udp = Udp::new(stack, buffers);

    // Bind 0.0.0.0:5353 and join 224.0.0.251. `edge-mdns`'s `DEFAULT_SOCKET`
    // is the IPv6 one and its own docs say not to use it in production, so
    // name the v4 socket explicitly.
    let socket = match io::bind(&udp, IPV4_DEFAULT_SOCKET, Some(ipv4), None).await {
        Ok(s) => s,
        Err(e) => {
            // The most likely cause is that joining the multicast group
            // failed, which would sink the whole discovery design.
            warn!("mdns: bind failed: {:?}", e);
            return;
        }
    };

    let signal = mk_static!(Signal<NoopRawMutex, ()>, Signal::new());
    let recv_buf = VecBufAccess::<NoopRawMutex, MDNS_BUF>::new();
    let send_buf = VecBufAccess::<NoopRawMutex, MDNS_BUF>::new();

    let mdns = io::Mdns::<_, _, _, _, _, NoopRawMutex>::new(
        Some(ipv4),
        None,
        &socket,
        &socket,
        recv_buf,
        send_buf,
        HwRng(esp_hal::rng::Rng::new()),
        signal,
    );

    let host = Host {
        hostname: HOSTNAME,
        ipv4,
        ipv6: Ipv6Addr::UNSPECIFIED,
        ttl: Ttl::from_secs(60),
    };

    // TXT records let a sender decide whether it can talk to us before it
    // sends a single frame. Card 003 fixes the real key set; the codec list
    // is a placeholder until card 002 lands.
    let service = Service {
        name: HOSTNAME,
        priority: 0,
        weight: 0,
        service: SERVICE,
        protocol: PROTOCOL,
        port: FRAME_PORT,
        service_subtypes: &[],
        txt_kvs: &[
            ("proto", "1"),
            ("w", "64"),
            ("h", "32"),
            ("ctrl", "49375"),
            ("codecs", "raw"),
        ],
    };

    // Keep the compiler honest about the control port until card 008 binds it.
    debug_assert_eq!(CONTROL_PORT, 49375);
    debug_assert_eq!((COLS, ROWS), (64, 32));

    info!(
        "mdns: announcing {}.local as {}.{}.{}.local on port {}",
        HOSTNAME, HOSTNAME, SERVICE, PROTOCOL, FRAME_PORT
    );

    let handler = HostAnswersMdnsHandler::new(ServiceAnswers::new(&host, &service));
    if let Err(e) = mdns.run(handler).await {
        warn!("mdns: responder stopped: {:?}", e);
    }
}
