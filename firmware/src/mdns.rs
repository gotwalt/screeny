//! DNS-SD responder: spec section 5, plus card 222's `_http._tcp`.
//!
//! The service is `_screeny._udp.local.`, the `SRV` target is
//! `screeny-<id>.local.` with the **frame** port, and the TXT record is built
//! by *parsing the `GET_INFO` body back out again*. That is not a roundabout
//! way of writing the keys twice: it is the point of section 6.6. One table
//! in the firmware, one parser in the sender, and a sender that browsed mDNS
//! is guaranteed identical metadata to one that was handed a bare IP, because
//! the two came from the same bytes.
//!
//! Since card 222 the same instance name is advertised a second time as
//! `_http._tcp.local.` on port 80, so a browser or `dns-sd -B _http._tcp`
//! finds the status page without knowing anything about screeny. The two
//! services share one [`Host`], so they share the `A` record and the host
//! name; only the `SRV` port and the TXT keys differ.
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
use edge_mdns::{
    ChainedHostAnswers, HostAnswersMdnsHandler, MdnsHandler, MdnsRequest, MdnsResponse,
};
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use log::{info, warn};
use screeny_proto::txt;

use crate::net::{CORE, INFO_CHANGED};
use crate::receiver::INFO_MAX;
use crate::{FRAME_PORT, mk_static};

const SERVICE: &str = "_screeny";
const PROTOCOL: &str = "_udp";

/// Card 222's second service: the status page and the JSON API.
const HTTP_SERVICE: &str = "_http";
const HTTP_PROTOCOL: &str = "_tcp";

/// One mDNS datagram.
///
/// **1,024 since card 223**, down from 1,500, and the number is spent four
/// times over: `UdpBuffers`' receive and transmit halves and the two
/// `VecBufAccess`es below, so the constant is worth 4 KB of `.bss` - which on
/// this chip is 4 KB of core 0's stack. It was the second of the two levers
/// card 223 was told to reach for (`docs/research/010-stack-and-ram-levers.md`
/// section 5, "MDNS_BUF - 1500 twice over"), and it is worth 1,904 bytes.
///
/// Why 1,024 is not a guess: what this device *sends* is a PTR, an SRV, a TXT
/// and an A record for each of two services, and the TXT is bounded by
/// [`INFO_MAX`] (224 bytes) - under 600 bytes all told. What it *receives* is
/// a query, and 1,024 is comfortably over the 576-byte message every DNS
/// implementation is required to accept. 1,500 was the Ethernet MTU rather
/// than a measurement of anything. If some other device on the LAN ever sends
/// a query larger than this, that one query goes unanswered and is re-sent;
/// nothing this device advertises is affected.
const MDNS_BUF: usize = 1024;

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

/// How often the responder checks the station's address (card 251).
///
/// embassy-net 0.9 offers `wait_config_up` and `wait_config_down` and **no
/// "the address changed" event**, and a DHCP renewal that is answered with a
/// *different* address goes from one `Configured` straight to the next
/// without the config ever being down (smoltcp's `dhcpv4` socket). So a timer:
/// one `config_v4()` read - a borrow of the stack, no I/O - every five
/// seconds. Five because the cost of being late is a sender that keeps the
/// old address for that long, and a router event that moves a panel takes
/// longer than that to settle anyway.
const ADDRESS_POLL: Duration = Duration::from_secs(5);

/// The gap between the first announcement and the repeat RFC 6762 section 8.3
/// asks for ("at least two ... one second apart").
///
/// `edge-mdns` sends one when `run` starts and then only when its broadcast
/// signal fires. Multicast over WiFi is the least reliable thing this device
/// sends - no ACK, the lowest basic rate - and card 251's whole lesson is that
/// a stale `A` record does not heal by itself, so the announcement that tells
/// the LAN about a new address gets its second copy.
const SECOND_ANNOUNCE: Duration = Duration::from_secs(1);

/// The station's IPv4 address right now, or `None` while DHCP is out.
fn address(stack: embassy_net::Stack<'static>) -> Option<Ipv4Addr> {
    stack.config_v4().map(|c| c.address.address())
}

/// Returns once the station's address is no longer `bound` - a different
/// lease, or none at all. See [`ADDRESS_POLL`].
async fn address_moved(stack: embassy_net::Stack<'static>, bound: Ipv4Addr) {
    loop {
        Timer::after(ADDRESS_POLL).await;
        if address(stack) != Some(bound) {
            return;
        }
    }
}

/// The responder, for the life of the device.
///
/// **Two loops since card 251.** Until fw 0.11.3 the address was read once,
/// at boot, and baked into the socket's multicast join, `Mdns` and [`Host`];
/// only [`INFO_CHANGED`] rebuilt anything, and that kept the boot-time
/// address. So a panel whose DHCP lease moved without a reboot (twice on the
/// bench: a router event at night, both panels at once) answered every query
/// for `screeny-<id>.local` with the address it no longer had, for as long as
/// it stayed up - 32 hours, the second time - while its HTTP page answered at
/// the new one. The outer loop now owns one *address*: wait for a lease, bind,
/// say goodbye to the previous address if there was a different one, announce,
/// answer; when the address moves or goes away, drop the socket and start
/// again. The inner loop is the old one, one record per `SET_NAME`.
///
/// It also no longer *returns* on a failed bind or a responder error. A task
/// that returns is gone until the next reboot, and a send that fails because
/// the lease went away underneath it is exactly the case this loop now
/// expects; it waits [`ADDRESS_POLL`] and starts over instead.
///
/// The frame and control sockets need none of this: they are bound to a port
/// on any address (`net.rs`), and `GET /api/v1/status`'s `ip` is republished
/// from `config_v4()` on every frame-task tick.
#[embassy_executor::task]
pub async fn mdns_task(
    stack: embassy_net::Stack<'static>,
    hostname: &'static str,
    id: &'static str,
) {
    let buffers = mk_static!(UdpBuffers<1, MDNS_BUF, MDNS_BUF, 2>, UdpBuffers::new());
    let udp = Udp::new(stack, buffers);
    let signal = mk_static!(Signal<NoopRawMutex, ()>, Signal::new());
    let recv_buf = VecBufAccess::<NoopRawMutex, MDNS_BUF>::new();
    let send_buf = VecBufAccess::<NoopRawMutex, MDNS_BUF>::new();

    // The address the LAN was last told about, for the goodbye.
    let mut announced: Option<Ipv4Addr> = None;

    loop {
        stack.wait_config_up().await;
        let Some(ipv4) = address(stack) else {
            // Up and gone again between the two reads. Rare; do not spin.
            Timer::after(SECOND_ANNOUNCE).await;
            continue;
        };

        // Bind 0.0.0.0:5353 and join 224.0.0.251. `edge-mdns`'s `DEFAULT_SOCKET`
        // is the IPv6 one and its own docs say not to use it in production, so
        // name the v4 socket explicitly. `UdpBuffers<1, ..>` holds one socket:
        // the previous address's was dropped at the end of its iteration. The
        // group is never left (dropping the socket does not), and joining it
        // again is a no-op in smoltcp 0.13, so a rebind cannot fail on that.
        let socket = match io::bind(&udp, IPV4_DEFAULT_SOCKET, Some(ipv4), None).await {
            Ok(s) => s,
            Err(e) => {
                warn!("mdns: bind failed: {:?}; retrying", e);
                Timer::after(ADDRESS_POLL).await;
                continue;
            }
        };

        let mdns = io::Mdns::<_, _, _, _, _, NoopRawMutex>::new(
            Some(ipv4),
            None,
            &socket,
            &socket,
            &recv_buf,
            &send_buf,
            HwRng(esp_hal::rng::Rng::new()),
            signal,
        );

        // RFC 6762 section 10.1's goodbye: the old `A` record with a TTL of
        // zero, so a cache drops it now instead of serving it for up to 120 s
        // beside the new one (`edge-mdns` does not set the cache-flush bit,
        // so the announcement alone does not displace it). The `A` record
        // only - the services have not gone anywhere, and a goodbye for them
        // would make every browser drop the panel and find it again. Best
        // effort: a lost goodbye costs at most one TTL.
        if let Some(old) = announced.filter(|old| *old != ipv4) {
            let gone = Host {
                hostname,
                ipv4: old,
                ipv6: Ipv6Addr::UNSPECIFIED,
                ttl: Ttl::from_secs(0),
            };
            let sent = mdns
                .query(|buf| {
                    match HostAnswersMdnsHandler::new(&gone).handle(MdnsRequest::None, buf)? {
                        MdnsResponse::Reply { data, .. } => Ok(data.len()),
                        MdnsResponse::None => Ok(0),
                    }
                })
                .await;
            match sent {
                Ok(()) => info!(
                    "mdns: address moved {} -> {}; goodbye sent for {}",
                    old, ipv4, old
                ),
                Err(e) => warn!(
                    "mdns: address moved {} -> {}; goodbye failed: {:?}",
                    old, ipv4, e
                ),
            }
        }
        announced = Some(ipv4);

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

            // The web service's own TXT. Deliberately three short keys: every byte
            // here is in every response that answers either service, and the
            // useful ones are "which device is this" and "where is the page".
            // RFC 6763 section 6.5 wants `txtvers` first.
            let mut http_kvs: heapless::Vec<(&str, &str), 3> = heapless::Vec::new();
            let _ = http_kvs.push(("txtvers", "1"));
            let _ = http_kvs.push(("id", id));
            let _ = http_kvs.push(("path", "/"));

            let http_service = Service {
                name: &instance,
                priority: 0,
                weight: 0,
                service: HTTP_SERVICE,
                protocol: HTTP_PROTOCOL,
                port: crate::http::HTTP_PORT,
                service_subtypes: &[],
                txt_kvs: &http_kvs,
            };

            info!(
                "mdns: {}.local -> {} as {}.{}.{}.local port {} ({} txt keys), and {}.{}.local port {}",
                hostname,
                ipv4,
                instance.as_str(),
                SERVICE,
                PROTOCOL,
                FRAME_PORT,
                kvs.len(),
                HTTP_SERVICE,
                HTTP_PROTOCOL,
                crate::http::HTTP_PORT,
            );

            // Chained, not two responders: one socket, one pair of buffers, and
            // both services answered out of the same packet. Each of them emits
            // the `A` record, which a resolver de-duplicates.
            let handler = HostAnswersMdnsHandler::new(ChainedHostAnswers::new(
                ServiceAnswers::new(&host, &service),
                ServiceAnswers::new(&host, &http_service),
            ));
            // `run` announces as it starts; `watch` sends the repeat a second
            // later and then waits for the address to move.
            let watch = async {
                Timer::after(SECOND_ANNOUNCE).await;
                signal.signal(());
                address_moved(stack, ipv4).await;
            };
            match select3(mdns.run(handler), INFO_CHANGED.wait(), watch).await {
                Either3::First(r) => {
                    // A send that failed because the lease went away under it is
                    // the usual way here. Either way: start over from the address.
                    warn!("mdns: responder stopped ({:?}); restarting", r.err());
                    Timer::after(ADDRESS_POLL).await;
                    break;
                }
                // SET_NAME changed the record: rebuild and re-announce.
                Either3::Second(()) => info!("mdns: record changed, re-announcing"),
                Either3::Third(()) => {
                    info!(
                        "mdns: {} is no longer this station's address; rebinding",
                        ipv4
                    );
                    break;
                }
            }
        }
    }
}
