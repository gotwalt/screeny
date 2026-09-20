//! Card 201 spike, parts 4 and 5: the DHCP server and the DNS catch-all.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_net::Stack;
use embassy_time::Instant;
use log::warn;

use super::{AP_IP, DHCP_BUF, DHCP_LEASES, DNS_BUF};
use crate::mk_static;

// ---------------------------------------------------------------------------
// 4: DHCP
// ---------------------------------------------------------------------------

#[embassy_executor::task]
pub async fn dhcp_task(stack: Stack<'static>) {
    let buffers = mk_static!(UdpBuffers<1, DHCP_BUF, DHCP_BUF, 2>, UdpBuffers::new());
    let udp = Udp::new(stack, buffers);

    let mut socket = match udp
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 67))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            warn!("dhcp: bind failed: {:?}", e);
            return;
        }
    };

    let mut server: edge_dhcp::server::Server<_, DHCP_LEASES> =
        edge_dhcp::server::Server::new(|| Instant::now().as_secs(), AP_IP);
    let mut gw = [AP_IP];
    let mut options = edge_dhcp::server::ServerOptions::new(AP_IP, Some(&mut gw));
    let dns = [AP_IP];
    options.dns = &dns;
    // RFC 8910: tell the client where the portal is, for the OSes that read it.
    options.captive_url = Some("http://192.168.4.1/portal");
    options.lease_duration_secs = 600;

    let buf = mk_static!([u8; DHCP_BUF], [0u8; DHCP_BUF]);
    if let Err(e) =
        edge_dhcp::io::server::run(&mut server, &options, &mut socket, &mut buf[..]).await
    {
        warn!("dhcp: stopped: {:?}", e);
    }
}

// ---------------------------------------------------------------------------
// 5: DNS catch-all
// ---------------------------------------------------------------------------

#[embassy_executor::task]
pub async fn dns_task(stack: Stack<'static>) {
    let buffers = mk_static!(UdpBuffers<1, DNS_BUF, DNS_BUF, 2>, UdpBuffers::new());
    let udp = Udp::new(stack, buffers);
    let rx = mk_static!([u8; DNS_BUF], [0u8; DNS_BUF]);
    let tx = mk_static!([u8; DNS_BUF], [0u8; DNS_BUF]);
    if let Err(e) = edge_captive::io::run(
        &udp,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 53),
        &mut tx[..],
        &mut rx[..],
        AP_IP,
        // Short TTL: the phone must re-ask once the device leaves portal mode.
        core::time::Duration::from_secs(10),
    )
    .await
    {
        warn!("dns: stopped: {:?}", e);
    }
}
