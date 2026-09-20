//! Card 220's `apsta-probe` build: what a soft-AP alongside the station
//! actually costs in heap, measured on the device.
//!
//! **Measurement, not product.** Nothing here is a design for the portal; the
//! build cards own that. This exists because `docs/research/007-device-web-and-portal.md`
//! section 6 had to stop at "APSTA is unmeasured", and every later card's RAM
//! budget is downstream of that one number.
//!
//! What it does, in one bounded run, from one boot:
//!
//! 1. **Stage 1 — station only.** Exactly the shipping firmware: the same
//!    [`crate::station_loop`], the same stream from the Studio, for
//!    [`STAGE1_S`] seconds. The heap line at the end of it is the baseline.
//! 2. **Stage 2 — APSTA.** `Config::AccessPointStation` with the same station
//!    config and an **open** AP named `screeny-<id>`. `set_config` stops and
//!    restarts the radio when the mode changes, so the station drops and
//!    re-associates; that is the same transition the portal will make, so it
//!    is worth measuring rather than avoiding.
//! 3. **Stage 3 — APSTA idle.** The second `embassy-net` stack runs on the AP
//!    interface at a static 192.168.4.1/24 with **nothing listening**: no
//!    DHCP, no DNS, no HTTP. Those are later cards, and their cost is a later
//!    measurement. The station keeps streaming.
//!
//! The heap is read from [`esp_alloc::HEAP`] every 5 s. This build also turns
//! on esp-alloc's `internal-heap-stats`, which tracks a true `max_usage`
//! across every allocation rather than only the ones a 5 s sample happens to
//! see — the radio's transients are exactly what a sample would miss. It costs
//! CPU on every alloc and dealloc, which is why the default build does not
//! have it.
//!
//! The AP is open on purpose (owner's decision, 2026-09-19) and the PSK
//! invariant of spec section 8.4 holds here as everywhere: nothing in this
//! file logs, returns or draws a password, and it never prints the station's
//! SSID.

use core::net::Ipv4Addr;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::select;
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer, with_timeout};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, Interface, PowerSaveMode, WifiController,
};
use log::{info, warn};

use screeny_settings::Wifi;

use crate::{mk_static, stack_probe, station_config, station_loop};

/// The soft-AP's own address. 192.168.4.1/24 is what every ESP soft-AP uses,
/// so a phone that has met one before already expects the number.
pub const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const AP_PREFIX: u8 = 24;

/// How long the station runs alone before the AP is raised. Long enough for
/// the association, DHCP, mDNS and the Studio's stream to be in steady state,
/// so the baseline is a baseline and not a boot transient.
const STAGE1_S: u64 = 60;

/// Seconds after the switch at which the stack high-water mark is reported a
/// second time, so the AP's own frames are inside the measurement.
const STAGE3_STACK_S: u64 = 60;

const HEAP_PERIOD_S: u64 = 5;

/// True once `set_config(AccessPointStation)` has returned `Ok`. Read by the
/// heap task so every line says which regime it belongs to.
pub static APSTA_UP: AtomicBool = AtomicBool::new(false);

/// The AP's `embassy-net` stack: static address, no DHCP client, no sockets.
///
/// `StackResources<4>` matches what card 201 costed (~3.9 KB of `.bss` with
/// the interface), so the number this build prints is comparable with that
/// table even though nothing is listening yet.
pub fn ap_stack(seed: u64) -> (Stack<'static>, Runner<'static, Interface>) {
    let interface = Interface::access_point();
    let mut dns_servers = heapless::Vec::<Ipv4Addr, 3>::new();
    let _ = dns_servers.push(AP_IP);
    let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
        address: embassy_net::Ipv4Cidr::new(AP_IP, AP_PREFIX),
        gateway: None,
        dns_servers,
    });
    embassy_net::new(
        interface,
        config,
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    )
}

fn heap_line(tag: &str) {
    let s = esp_alloc::HEAP.stats();
    info!(
        "apsta-probe: {} | heap used {} of {} ({} free) | max usage {} | total alloc {} freed {}",
        tag,
        s.current_usage,
        s.size,
        s.size - s.current_usage,
        s.max_usage,
        s.total_allocated,
        s.total_freed,
    );
}

/// `esp_alloc::HEAP.stats()` every 5 s, with the lowest free the sampler has
/// ever seen and esp-alloc's own all-allocations `max_usage` beside it.
#[embassy_executor::task]
pub async fn heap_task() {
    let mut min_free = usize::MAX;
    loop {
        Timer::after(Duration::from_secs(HEAP_PERIOD_S)).await;
        let s = esp_alloc::HEAP.stats();
        let free = s.size - s.current_usage;
        min_free = min_free.min(free);
        info!(
            "heap: apsta {} | used {} of {} | free {} | min free sampled {} | max usage {}",
            APSTA_UP.load(Ordering::Relaxed) as u8,
            s.current_usage,
            s.size,
            free,
            min_free,
            s.max_usage,
        );
    }
}

/// Owns the controller and the AP runner, and walks the three stages.
///
/// It owns both so that nothing has to be signalled across tasks: the switch,
/// the reconnect and the AP stack all happen in one place, in order.
#[embassy_executor::task]
pub async fn probe_task(
    mut controller: WifiController<'static>,
    ap_ssid: &'static str,
    mut ap_runner: Runner<'static, Interface>,
    stored: Option<Wifi>,
) {
    info!(
        "apsta-probe: stage 1, station only, {} s before the AP goes up",
        STAGE1_S
    );
    // `station_loop` never returns; the timeout is how stage 1 ends. By then
    // the loop is sitting in its 2 s RSSI poll, which is a safe place to drop.
    let _ = with_timeout(
        Duration::from_secs(STAGE1_S),
        station_loop(&mut controller, stored.clone()),
    )
    .await;
    heap_line("stage 1 done: station-only baseline");

    let ap = AccessPointConfig::default()
        .with_ssid(ap_ssid.try_into().expect("ap ssid <= 32 bytes"))
        .with_authentication(AuthenticationMethodConfig::Open)
        // Channel 1 is only the starting point. The ESP32 has one PHY, so the
        // radio drags the soft-AP onto the station's channel as soon as the
        // station associates and announces it with a CSA.
        .with_channel(1);

    // Card 212: credentials come from the store now, so the station half of the
    // APSTA config is built from whatever the station is actually using rather
    // than from a compiled-in constant. With nothing stored (and no `bench-wifi`
    // pair) the probe still measures the heap, which is its whole job; it just
    // will not associate.
    let builtin = crate::builtin_wifi();
    let sta = stored
        .as_ref()
        .or(builtin.as_ref())
        .and_then(station_config)
        .unwrap_or_default();

    info!("apsta-probe: stage 2, raising open AP {:?}", ap_ssid);
    match controller.set_config(&WifiConfig::AccessPointStation(sta, ap)) {
        Ok(()) => {
            APSTA_UP.store(true, Ordering::Relaxed);
            // The mode change stopped and restarted the radio, which resets
            // power saving; the frame path needs it off.
            if let Err(e) = controller.set_power_saving(PowerSaveMode::None) {
                warn!("apsta-probe: set_power_saving after the switch failed {e:?}");
            }
            heap_line("stage 2: APSTA mode set");
        }
        Err(e) => {
            warn!("apsta-probe: set_config(AccessPointStation) failed {e:?} - staying station only");
            heap_line("stage 2: FAILED, still station only");
        }
    }

    info!("apsta-probe: stage 3, AP stack at {} with nothing listening", AP_IP);
    let stack_line = async {
        Timer::after(Duration::from_secs(STAGE3_STACK_S)).await;
        if let Some(hw) = stack_probe::CORE0.high_water() {
            info!(
                "stack: core 0 main high-water {} of {} bytes after APSTA, {} free",
                hw,
                stack_probe::CORE0.size(),
                stack_probe::CORE0.headroom().unwrap_or(0),
            );
        }
        heap_line("stage 3: APSTA idle");
        core::future::pending::<()>().await
    };

    select(
        select(station_loop(&mut controller, stored), ap_runner.run()),
        stack_line,
    )
    .await;
}
