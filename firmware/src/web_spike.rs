//! Card 201's compile-only spike. **Evidence, not the implementation.**
//!
//! Everything here exists to make the compiler agree with a claim in
//! `docs/research/007-device-web-and-portal.md`, and to put a real number on
//! "what does a soft-AP + a captive portal cost in flash and `.bss` on this
//! stack". It is never flashed, it is not wired into any
//! product behaviour, and the build cards replace it wholesale.
//!
//! What it proves links, in order:
//!
//! 1. `esp-radio 1.0.0-beta.1` gives a second `Interface::access_point()`
//!    alongside the station one, and `Config::AccessPointStation` puts the
//!    radio in APSTA mode.
//! 2. A second `embassy_net::Stack` can be built over that interface with a
//!    **static** 192.168.4.1/24 config, its own `StackResources` and its own
//!    runner task.
//! 3. **Retired by card 222.** The HTTP third of this spike proved
//!    `picoserve 0.20` links against `embassy-net 0.9.1` on Xtensa; the real
//!    server in `crate::http` is now in the default build and is the evidence.
//! 4. `edge-dhcp 0.8` runs a DHCP server over a plain `edge-nal-embassy` UDP
//!    socket - no raw socket - and emits RFC 8910 option 114.
//! 5. `edge-captive 0.8` answers every A query with 192.168.4.1 over the same
//!    kind of socket, reusing the `domain` crate `edge-mdns` already pulls in.
//! 6. `qrcodegen-no-heap 1.8` encodes `WIFI:T:nopass;S:screeny-<id>;;` at
//!    version 2-L and renders into a [`Frame`] with no allocator.
//!
//! The PSK invariant of spec section 8.4 holds here too: nothing in this file
//! reads back, returns, logs or draws a password.

use core::net::Ipv4Addr;

#[cfg(feature = "spike-ap")]
pub mod ap;
#[cfg(feature = "spike-portal")]
pub mod portal;
#[cfg(feature = "spike-qr")]
pub mod qr;

// ---------------------------------------------------------------------------
// Addresses and sizes the research doc quotes
// ---------------------------------------------------------------------------

/// The soft-AP's own address, and the only address the portal answers on.
/// 192.168.4.1/24 is what every ESP soft-AP uses, so a phone that has met one
/// before already expects the number.
pub const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const AP_PREFIX: u8 = 24;

/// DHCP and DNS each get one datagram's worth; neither protocol needs more.
const DHCP_BUF: usize = 640;
const DNS_BUF: usize = 512;

/// Lease table. Four phones at once is generous for a provisioning AP.
const DHCP_LEASES: usize = 4;

// ---------------------------------------------------------------------------
// Wiring, so nothing above is dead code the linker can drop
// ---------------------------------------------------------------------------

/// Called from `main` under the feature. Spawns everything and returns.
#[cfg(feature = "spike-ap")]
pub fn spawn_all(spawner: embassy_executor::Spawner, seed: u64) {
    let (stack, runner) = ap::ap_stack(seed);
    spawner.spawn(ap::ap_net_task(runner).unwrap());
    #[cfg(feature = "spike-portal")]
    {
        spawner.spawn(portal::dhcp_task(stack).unwrap());
        spawner.spawn(portal::dns_task(stack).unwrap());
    }
    #[cfg(not(feature = "spike-portal"))]
    let _ = stack;
}

/// Referenced from `main` so the QR path is linked in too.
#[cfg(feature = "spike-qr")]
pub fn portal_frame_demo(frame: &mut crate::display::Frame, ssid: &str) {
    if !qr::portal_screen(frame, ssid) {
        log::warn!("portal: ssid too long for a 64x32 QR");
    }
}

/// Keeps `Timer` from being dropped as unused.
pub async fn settle() {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(1)).await;
}
