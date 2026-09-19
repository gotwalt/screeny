//! DNS-SD responder: spec section 5.
//!
//! The service is `_screeny._udp.local.`, the `SRV` target is
//! `screeny-<id>.local.` with the **frame** port, and the TXT record is built
//! by *parsing the `GET_INFO` body back out again*. That is not a roundabout
//! way of writing the keys twice: it is the point of section 6.6. One table
//! in the firmware, one parser in the sender, and a sender that browsed mDNS
//! is guaranteed identical metadata to one that was handed a bare IP, because
//! the two came from the same bytes.
//!
//! `SET_NAME` changes those bytes, so the responder is restarted on
//! [`crate::net::INFO_CHANGED`], which also re-announces (RFC 6762 section
//! 8.3) from the new record.

use core::convert::Infallible;
use core::net::{Ipv4Addr, Ipv6Addr};

use edge_mdns::buf::VecBufAccess;
use edge_mdns::domain::base::Ttl;
use edge_mdns::host::{Host, Service, ServiceAnswers};
use edge_mdns::io::{self, IPV4_DEFAULT_SOCKET};
use edge_mdns::HostAnswersMdnsHandler;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use log::{info, warn};
use screeny_proto::txt;

use crate::net::{CORE, INFO_CHANGED};
use crate::receiver::INFO_MAX;
use crate::{mk_static, FRAME_PORT};

const SERVICE: &str = "_screeny";
const PROTOCOL: &str = "_udp";

/// An mDNS query or response fits in one datagram many times over; 1500 is
/// the honest ceiling and costs 3 KB of the two buffers together.
const MDNS_BUF: usize = 1500;

/// `esp_hal::rng::Rng` in the shape `edge-mdns` wants.
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
pub async fn mdns_task(stack: embassy_net::Stack<'static>, hostname: &'static str) {
    stack.wait_config_up().await;
    let Some(config) = stack.config_v4() else {
        warn!("mdns: no IPv4 address, giving up");
        return;
    };
    let ipv4: Ipv4Addr = config.address.address();

    let buffers = mk_static!(UdpBuffers<1, MDNS_BUF, MDNS_BUF, 2>, UdpBuffers::new());
    let udp = Udp::new(stack, buffers);

    // Bind 0.0.0.0:5353 and join 224.0.0.251. `edge-mdns`'s `DEFAULT_SOCKET`
    // is the IPv6 one and its own docs say not to use it in production, so
    // name the v4 socket explicitly.
    let socket = match io::bind(&udp, IPV4_DEFAULT_SOCKET, Some(ipv4), None).await {
        Ok(s) => s,
        Err(e) => {
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
        hostname,
        ipv4,
        ipv6: Ipv6Addr::UNSPECIFIED,
        // Section 5.1: 120 s for SRV/TXT/A, per RFC 6762 section 10.
        ttl: Ttl::from_secs(120),
    };

    loop {
        // Take a private copy of the GET_INFO body. The `Service` below
        // borrows from it, so it has to outlive the responder run, and the
        // core's copy is behind a mutex two other tasks need.
        let mut info = [0u8; INFO_MAX];
        let n;
        let mut instance = heapless::String::<32>::new();
        {
            let guard = CORE.lock().await;
            let core = guard.as_ref().expect("core exists");
            let b = core.info_bytes();
            n = b.len().min(INFO_MAX);
            info[..n].copy_from_slice(&b[..n]);
            let _ = instance.push_str(core.name());
        }

        // Section 6.6's bytes, read back as section 5.2's keys. Order is
        // preserved by the iterator, so `txtvers` stays first as RFC 6763
        // section 6.5 requires.
        let mut kvs: heapless::Vec<(&str, &str), 12> = heapless::Vec::new();
        for e in txt::iter(&info[..n]) {
            let (Ok(k), Some(Ok(v))) = (
                core::str::from_utf8(e.key),
                e.value.map(core::str::from_utf8),
            ) else {
                continue;
            };
            let _ = kvs.push((k, v));
        }

        let service = Service {
            name: &instance,
            priority: 0,
            weight: 0,
            service: SERVICE,
            protocol: PROTOCOL,
            // Section 5.1: the SRV record carries the *frame* port; the
            // control port is the `ctrl=` TXT key, which is already in `kvs`.
            port: FRAME_PORT,
            service_subtypes: &[],
            txt_kvs: &kvs,
        };

        info!(
            "mdns: {}.local -> {} as {}.{}.{}.local port {} ({} txt keys)",
            hostname,
            ipv4,
            instance.as_str(),
            SERVICE,
            PROTOCOL,
            FRAME_PORT,
            kvs.len()
        );

        let handler = HostAnswersMdnsHandler::new(ServiceAnswers::new(&host, &service));
        match select(mdns.run(handler), INFO_CHANGED.wait()).await {
            Either::First(Err(e)) => {
                warn!("mdns: responder stopped: {:?}", e);
                return;
            }
            Either::First(Ok(())) => return,
            // SET_NAME changed the record: rebuild and re-announce.
            Either::Second(()) => info!("mdns: record changed, re-announcing"),
        }
    }
}
