//! Card 201 spike, part 1+2: the AP interface and its own `embassy-net` stack.

use core::net::Ipv4Addr;

use embassy_net::{Runner, Stack, StackResources};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config as WifiConfig, Interface};

use super::{AP_IP, AP_PREFIX};
use crate::mk_static;

// ---------------------------------------------------------------------------
// 1 + 2: the AP interface and its own stack
// ---------------------------------------------------------------------------

/// Build the APSTA config. The AP is **open** (owner's decision, 2026-09-19):
/// the home PSK crosses it in clear, which spec section 8.4 already accepts.
pub fn apsta_config(sta: esp_radio::wifi::sta::StationConfig, ap_ssid: &str) -> WifiConfig {
    let ap = AccessPointConfig::default()
        .with_ssid(ap_ssid.try_into().expect("ap ssid <= 32 bytes"))
        .with_authentication(AuthenticationMethodConfig::Open)
        // Channel 1 is the default. The real firmware should follow the STA's
        // channel once associated: the radio has one PHY, and an AP beaconing
        // on a different channel from the station only gets the off-time.
        .with_channel(1);
    WifiConfig::AccessPointStation(sta, ap)
}

/// The second `embassy-net` stack, on the AP interface, statically addressed.
pub fn ap_stack(seed: u64) -> (Stack<'static>, Runner<'static, Interface>) {
    let interface = Interface::access_point();
    let mut dns_servers = heapless::Vec::<Ipv4Addr, 3>::new();
    let _ = dns_servers.push(AP_IP);
    let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
        address: embassy_net::Ipv4Cidr::new(AP_IP, AP_PREFIX),
        gateway: None,
        dns_servers,
    });
    // Four sockets: the TCP listener, UDP/67, UDP/53, one spare.
    embassy_net::new(
        interface,
        config,
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    )
}

#[embassy_executor::task]
pub async fn ap_net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}
