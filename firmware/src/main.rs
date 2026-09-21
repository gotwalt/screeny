//! screeny firmware — a Gen 1 Tidbyt as a UDP network frame buffer.
//!
//! Grown out of `spike/fw-skeleton` (card 001), which proved the stack
//! compiles and boots. Card 007 made the *display half* right on the real
//! panel: orientation, ghosting, gamma, brightness, dithering, a status
//! screen. Card 008 makes the *network half* real: `crates/proto` for every
//! byte on the wire, the full control surface, and the display moved onto
//! core 1.
//!
//! ## Why the cores are split the way they are
//!
//! Card 007 measured temporal dithering at ~48% of a core, permanently: the
//! DMA framebuffer has to be rewritten every refresh (154 Hz), not once per
//! frame, because that is what spends the sub-level remainder over time. On
//! one executor that render also *held the frame lock* across a synchronous
//! 3 ms conversion, so the frame task could not run while it was in flight
//! and "drain the socket, newest wins" could never fire.
//!
//! So: **core 1 does nothing but convert sRGB to the panel and swap DMA
//! buffers**, and **core 0 does WiFi, the network stack, decode, control and
//! mDNS**. The handoff is [`fb`], a lock-free triple buffer: no mutex between
//! the halves, no copy, and no critical section to disturb the refresh ISR.
//!
//! | core | task | job |
//! |---|---|---|
//! | 1 | `display` | owns esp-hub75 and both DMA buffers; gamma + dither + swap |
//! | 0 | `wifi` | associate, reconnect forever, modem sleep off, RSSI |
//! | 0 | `net` | embassy-net runner |
//! | 0 | `frames` | UDP 49374: drain, newest wins, decode, source lock, idle screens |
//! | 0 | `control` | UDP 49375: every opcode in spec section 6.3 |
//! | 0 | `mdns` | `_screeny._udp` and `_http._tcp`, TXT from the `GET_INFO` bytes |
//! | 0 | `http` x2 | TCP 80: the status page and the `screeny-device-api` JSON |
//! | 0 | `telemetry` | the serial-log numbers a camera cannot measure |

#![no_std]
#![no_main]

/// Card 240's staging buffer is the one thing in this firmware that asks the
/// allocator for memory by name. Everything else that uses the heap does so
/// through a library (esp-radio's driver buffers are almost all of it), so
/// this is the first `extern crate alloc` the crate has needed.
extern crate alloc;

/// Card 220's `apsta-probe` build: what APSTA costs in heap. Off by default.
#[cfg(feature = "apsta-probe")]
mod apsta_probe;
mod display;
mod fb;
mod gamma;
mod http;
mod mdns;
mod net;
/// Card 240: staging a firmware image into the inactive app slot.
mod ota;
mod panel_init;
/// Card 243: the `#[panic_handler]`, the RTC breadcrumb and the crash-loop
/// guard. It is this crate's panic handler, so it is not optional and not
/// feature-gated.
mod panic;
mod patterns;
mod receiver;
mod screens;
// The `apsta-probe` build owns the radio itself (its whole job is the mode
// switch), so it does not spawn the provisioning task or the AP's services -
// which makes most of this module dead code *in that build only*. It is still
// compiled, and `main` still builds the AP stack out of it, so there is one
// definition of the AP's address and socket count rather than two.
#[cfg_attr(feature = "apsta-probe", allow(dead_code))]
mod provision;
#[cfg(feature = "spike-ota")]
mod spike_ota;
mod stack_probe;
mod store;
mod tidbyt;
// Card 201's compile-only spike (`spike-ap`, `spike-portal`, `spike-qr`,
// `device-web-spike`) was **retired by card 223**: the soft-AP, the DHCP
// server, the DNS catch-all and the QR screen are in the default build now, so
// a second, never-flashed spelling of each was two places to get them wrong.
// `src/web_spike.rs` and `src/web_spike/` are gone with it.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant as EmbassyInstant, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig, Pin};
use esp_hal::interrupt::Priority;
use esp_hal::rng::Rng;
use esp_hal::system::Stack as CoreStack;
use esp_hal::time::{Instant, Rate};
use esp_hal::timer::timg::TimerGroup;
use esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer;
use esp_hub75::framebuffer::compute_rows;
use esp_hub75::{Hub75, Hub75Config, Hub75Pins16};
use esp_radio::wifi::sta::{ScanMethod, StationConfig};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, ControllerConfig, Interface, PowerSaveMode,
    WifiController,
};
use log::{info, warn};
use screeny_proto::control::MAX_SSID_LEN;
use screeny_settings::Wifi;

use display::Mode;

// The app descriptor every image carries, and the thing card 240's validator
// reads out of an upload before it will stage it (`crates/fwimage`, check 4).
//
// **Spelled out rather than the no-argument form** since 0.6.0. The short form
// takes `CARGO_PKG_VERSION`, which is `firmware/Cargo.toml`'s `0.1.0` and has
// never moved - so every image ever built said `0.1.0` and an uploaded one was
// indistinguishable from the running one. `FW_VERSION` is the number this
// project actually versions by: it is the `fw=` mDNS TXT key, the `GET_INFO`
// field and `status.fw`, and now it is what `POST /api/v1/firmware` reports
// back about the image it just staged. Everything else is the macro's own
// default, copied from its definition.
esp_bootloader_esp_idf::esp_app_desc!(
    FW_VERSION,
    env!("CARGO_PKG_NAME"),
    esp_bootloader_esp_idf::BUILD_TIME,
    esp_bootloader_esp_idf::BUILD_DATE,
    esp_bootloader_esp_idf::ESP_IDF_COMPATIBLE_VERSION,
    esp_bootloader_esp_idf::MMU_PAGE_SIZE,
    0,
    u16::MAX,
    esp_bootloader_esp_idf::SECURE_VERSION
);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The **bench override** credentials, supplied by `build.rs` from outside git.
///
/// These exist only in a `--features bench-wifi` build (owner's decision,
/// 2026-09-20). In a default build the constants are not declared at all, so
/// there is no name by which a credential could reach the binary, and anything
/// that tried would fail to compile. The device joins from its settings store;
/// the compiled-in pair is a bench convenience that seeds an empty store and
/// acts as spec section 8.3's step-2 fallback, and nothing else.
#[cfg(feature = "bench-wifi")]
pub const SSID: &str = env!("SCREENY_WIFI_SSID");
#[cfg(feature = "bench-wifi")]
const PASSWORD: &str = env!("SCREENY_WIFI_PASSWORD");

/// The `fw=` TXT key and `GET_INFO` field.
///
/// 0.3.0 was card 212: settings live in flash. 0.4.0 is card 222: the device
/// answers HTTP on the LAN. 0.4.1 fixed `SET_WIFI` committing credentials to
/// flash before proving them. 0.4.2 is card 227: core 1's stack measured and
/// cut to fit, the heap arena trimmed, and a **second HTTP connection
/// worker** - which is the part visible from outside, because back-to-back
/// connections no longer pay a 1 s SYN retransmit. 0.4.3 is card 233: one HTTP
/// dispatch instead of nine nested router futures, every refusal in the API's
/// error shape (a verb the API has no method for is now `method_not_allowed`
/// and not picoserve's plain text), a reboot without the magic word is
/// `out_of_range`, and each route's own `max_request_len` is enforced. 0.5.0 is
/// card 223 (the soft-AP, the portal and the setup page), 0.5.1 its five
/// phone-test fixes, and **0.5.2 is card 243: a panic prints, leaves a
/// breadcrumb in RTC memory and reboots the chip** instead of spinning core 0
/// for ever with the panel still lit - plus the crash-loop guard, the
/// breadcrumb in `GET /api/v1/status`, and one partition-table read for the
/// whole boot path. **0.5.3 is card 236: an HTTP worker gets back to `accept`
/// in a time this device sets** - the graceful close no longer waits for the
/// client's own `close()`, only for the acknowledgement that proves the reply
/// arrived - which is what was refusing 9-17 of ~35 sequential requests.
/// **0.6.0 is card 240: `POST /api/v1/firmware` stages an image into the
/// inactive slot** - streamed straight from the socket into flash a sector at
/// a time, validated by research 006 section 5's checks, with an "updating"
/// screen on the panel and dither off while it runs. It does not switch the
/// boot slot: `otadata` is untouched and card 241 is what makes a staged image
/// bootable. From this version the string is also `esp_app_desc.version`
/// inside the image, so an upload can say which build it just staged.
/// **0.7.0 is card 241: a staged image is activated, boots on trial, and
/// either proves itself or is replaced by the one before it.** An accepted
/// upload now writes `otadata` and restarts the device; the image that comes
/// up has to show a DHCP address, a served request (or two minutes) and a
/// panel swap before 60 s to confirm itself, and is reverted at 180 s if it
/// does not. A panic, a watchdog, a brownout or a pulled cable during that
/// window is handled by the bootloader instead, which turns the trial entry
/// into `ABORTED` on any reset and boots the other slot. `GET /api/v1/panic`
/// gains an `update` object saying which of those happened.
pub const FW_VERSION: &str = "0.7.0";

pub const FRAME_PORT: u16 = screeny_proto::DEFAULT_FRAME_PORT;
pub const CONTROL_PORT: u16 = screeny_proto::DEFAULT_CONTROL_PORT;

/// One frame per datagram, at most one Ethernet MTU of payload. Only holds
/// because `.cargo/config.toml` raises esp-radio's MTU to 1500.
pub const MAX_DATAGRAM: usize = screeny_proto::MAX_UDP_PAYLOAD;

/// Ceiling on the runtime brightness a `SET_BRIGHTNESS` can reach, and what
/// the reply's `applied` byte reports back (spec section 6.3).
///
/// The *power* cap is [`display::MAX_OE_SLOTS`] and lives in the duty
/// mapping, where card 007 put it; this is a second, softer limit so that a
/// sender on the LAN cannot drive a USB-powered panel to its ceiling during
/// an unattended soak. 160/255 is 16 of 64 output-enable slots, 25% duty.
pub const BRIGHTNESS_CAP: u8 = 160;

pub const COLS: usize = tidbyt::PANEL_COLS;
pub const ROWS: usize = tidbyt::PANEL_ROWS;
/// 1/16 scan: the panel shifts two rows at a time.
const NROWS: usize = compute_rows(ROWS);

/// Binary-code-modulation depth. Every extra plane halves the refresh rate;
/// the gamma table is built against this number.
const PLANES: usize = gamma::PLANES;

const PIXEL_CLOCK: Rate = Rate::from_mhz(tidbyt::PIXEL_CLOCK_MHZ);

pub type FrameBuffer = DmaFrameBuffer<NROWS, COLS, PLANES>;

const REFRESH_HZ: u32 = esp_hub75::refresh_hz::<FrameBuffer>(PIXEL_CLOCK);

const _: () = assert!(
    REFRESH_HZ >= 120,
    "panel refresh below 120 Hz: reduce PLANES or raise PIXEL_CLOCK"
);

/// Core 1's stack. **6 KB, and it is measured, not guessed** (card 227).
///
/// It runs one task whose deepest call is `display::render` (a 192-byte row
/// buffer), plus the HUB75 DMA interrupt at `Priority3`, which lands on
/// whatever stack is current - `xtensa-lx-rt`'s `SAVE_CONTEXT` opens with
/// `addmi sp, sp, -256` on the interrupted stack, and there is no separate
/// interrupt stack on this chip.
///
/// It was 16 KB from the first flash of this firmware to card 227, on the
/// reasoning that 16 KB is "generous" - and it was: `stack_probe::CORE1`
/// paints the region from core 0 before the core starts and scans it from
/// core 0 afterwards, and after 200 s of 30 fps streaming with dither on and
/// 2,378 HTTP connections in flight on the other core, **the high-water mark
/// was 1,872 bytes of 16,384**. Three quarters of it had never been touched.
///
/// That was not free. Core 1's stack is ordinary `.bss`, and on this chip
/// `.data`, `.bss` and core 0's main stack come out of one DRAM region with
/// the stack as the remainder - so every byte over-provisioned here was a
/// byte core 0 did not have. Releasing 10,240 of them is what paid for the
/// second HTTP worker.
///
/// The size is card 227's rule, `max(2 * high_water, 6 KB)` rounded up to a
/// kilobyte: `max(3744, 6144)` = 6,144. It is the 6 KB floor that binds, not
/// the measurement, which means there is better than a **3.2x** margin over
/// anything ever observed. Do not cut it further without a reason and a
/// number; esp-rtos checks the guard on every context switch and panics with
/// the range, so an undersized stack fails loudly - but the panic is a boot
/// loop on a device that may not be on your desk.
#[cfg_attr(feature = "display-on-core0", allow(dead_code))]
static APP_CORE_STACK: static_cell::ConstStaticCell<CoreStack<6144>> =
    static_cell::ConstStaticCell::new(CoreStack::new());

/// The two DMA framebuffers core 1 swaps between.
///
/// `ConstStaticCell`, not `StaticCell::write(FrameBuffer::new())`, and that is
/// the whole point of card 220. `DmaFrameBuffer::new()` is a `const fn`, so
/// written this way the 12 KB value is produced by the compiler and placed in
/// `.data`; written the old way it was produced at runtime as a temporary on
/// whichever stack ran the constructor, and two of them at once is 24 KB.
/// Core 1's 16 KB stack did not survive that on the very first flash of this
/// firmware, and core 0's main stack — which is only the *remainder* of a DRAM
/// region the linker fills with `.data` and `.bss` first — was carrying the
/// same 24 KB transient every boot since.
///
/// `.data` and `.bss` come out of the same pocket, so moving 24 KB from one to
/// the other leaves `.stack`'s size exactly where it was. What it removes is
/// the *transient*: what the region has to be big enough for, as opposed to
/// what it nominally is. It costs 24 KB of flash, which a 2 MB app slot has.
///
/// They still have to come up dimmed: see the `set_oe_slots` call in `main`.
/// `new()` formats for the widest output-enable window the build can produce,
/// which is well over the power cap, and letting a single refresh out at that
/// duty is a bug with a current spike attached to it.
#[cfg(not(feature = "fb-on-stack"))]
static FB0: static_cell::ConstStaticCell<FrameBuffer> =
    static_cell::ConstStaticCell::new(FrameBuffer::new());
#[cfg(not(feature = "fb-on-stack"))]
static FB1: static_cell::ConstStaticCell<FrameBuffer> =
    static_cell::ConstStaticCell::new(FrameBuffer::new());

/// The decoded-frame handoff between core 0 and core 1.
static SLOTS: fb::Slots = fb::Slots::new();

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Panel refreshes actually pushed, for the measured refresh rate.
pub static SWAPS: AtomicU32 = AtomicU32::new(0);
/// Last sRGB888 -> DMA conversion, in microseconds.
pub static RENDER_US: AtomicU32 = AtomicU32::new(0);
/// Maximum since boot, and maximum within the current serial-telemetry period.
///
/// Both, because they answer different questions and the first one hides the
/// second. Card 007 found WiFi association preempting the display task for
/// most of a second exactly once, at boot; a since-boot maximum therefore
/// read ~640000 forever and said nothing about the steady state. Whether that
/// still happens with the display on core 1 is one of card 008's measurements.
pub static RENDER_US_MAX: AtomicU32 = AtomicU32::new(0);
pub static RENDER_US_MAX_WINDOW: AtomicU32 = AtomicU32::new(0);
/// The same maximum again, for telemetry byte 42, where `RESET_STATS` owns
/// the reset instead of the 5-second log window.
pub static RENDER_US_MAX_PROTO: AtomicU32 = AtomicU32::new(0);

pub static BRIGHTNESS: AtomicU8 = AtomicU8::new(display::DEFAULT_BRIGHTNESS);
/// Bench-only escape hatch: raw output-enable slots, ignoring the power cap.
/// `u8::MAX` means "not overridden". See `receiver::Core::bench`.
pub static OE_OVERRIDE: AtomicU8 = AtomicU8::new(u8::MAX);
pub static OE_OVERRIDE_DEADLINE_MS: AtomicU32 = AtomicU32::new(0);
/// Set whenever the brightness the panel is showing may be stale. Two,
/// because there are two framebuffers and the setting lives in the buffer.
pub static BRIGHTNESS_DIRTY: AtomicU8 = AtomicU8::new(2);

/// First pixel-clock slot of the scan row that the panel is allowed to be
/// lit for. **This is the anti-ghosting control.** Card 007 swept it on the
/// bench and found nothing to fix; it stays at the `trail-blank-8` default.
pub static OE_START: AtomicU8 = AtomicU8::new(FrameBuffer::OE_DEFAULT_START as u8);

pub static GAMMA_ON: AtomicBool = AtomicBool::new(Mode::DEFAULT.gamma);
pub static DITHER_ON: AtomicBool = AtomicBool::new(Mode::DEFAULT.dither);

/// `0` = follow the stream / idle screen. `n > 0` = hold pattern `n - 1`.
pub static PATTERN_HOLD: AtomicU8 = AtomicU8::new(0);

/// Last beacon RSSI, telemetry byte 44. 0 means "never reported".
pub static RSSI_DBM: AtomicI8 = AtomicI8::new(0);
// Card 223: the `GET_WIFI` join state is no longer an atomic here. It is
// `Provisioner::wifi_state()`, read through `crate::provision`, because the
// machine is the only thing that knows about the sticky `FAILED` a posted pair
// leaves behind and about the difference between "the portal is up because
// something failed" and "the portal is up because there was nothing to try".

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static CELL: ::static_cell::StaticCell<$t> = ::static_cell::StaticCell::new();
        CELL.uninit().write($val)
    }};
}
pub(crate) use mk_static;

pub fn now_ms() -> u32 {
    EmbassyInstant::now().as_millis() as u32
}

fn mode() -> Mode {
    Mode {
        gamma: GAMMA_ON.load(Ordering::Relaxed),
        dither: DITHER_ON.load(Ordering::Relaxed),
    }
}

/// Output-enable slots the panel should be lit for right now.
fn target_oe_slots() -> usize {
    let raw = OE_OVERRIDE.load(Ordering::Relaxed);
    if raw != u8::MAX {
        // The override is time-boxed on purpose: it exists so a bench session
        // can photograph the panel at a duty cycle we would never ship, and
        // an override left on by a forgotten script is a power problem.
        if now_ms().wrapping_sub(OE_OVERRIDE_DEADLINE_MS.load(Ordering::Relaxed)) as i32 > 0 {
            OE_OVERRIDE.store(u8::MAX, Ordering::Relaxed);
            BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
            warn!("display: OE override expired, back to brightness cap");
        } else {
            return raw as usize;
        }
    }
    display::slots_for(BRIGHTNESS.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// Display — core 1, and nothing else runs there
// ---------------------------------------------------------------------------

/// Owns the panel and both framebuffers.
///
/// Two regimes, because they want opposite things:
///
/// * **dither off** — the panel content only changes when someone changes it,
///   so the task sleeps between frames and the DMA engine loops on the buffer
///   it already has. Costs almost no CPU.
/// * **dither on** — the sub-level remainder is spent across successive
///   refreshes, so the buffer has to be rewritten every refresh and the loop
///   free-runs. `swap()` blocks until a frame boundary, so the loop rate *is*
///   the refresh rate; nothing else paces it.
///
/// Since card 008 it takes its frames from [`fb::Consumer`], which never
/// blocks and never copies, so nothing on core 0 can stall a refresh and a
/// refresh cannot stall a decode.
#[embassy_executor::task]
async fn display_task(
    hub75: Hub75<esp_hal::Async, FrameBuffer>,
    mut fb: &'static mut FrameBuffer,
    mut frames: fb::Consumer,
) {
    info!(
        "display: core 1, {} planes, {} Hz refresh (driver), {} bytes/buffer, OE slots 0..={} (cap {}), OE start {}",
        PLANES,
        REFRESH_HZ,
        core::mem::size_of::<FrameBuffer>(),
        FrameBuffer::OE_SLOTS,
        display::MAX_OE_SLOTS,
        OE_START.load(Ordering::Relaxed),
    );

    let mut phase: u16 = 0;
    let mut last_mode = mode();
    loop {
        let m = mode();
        let mode_changed = m != last_mode;
        last_mode = m;
        let fresh = frames.acquire();
        let dirty = BRIGHTNESS_DIRTY.load(Ordering::Relaxed) > 0;

        if !m.dither && !fresh && !dirty && !mode_changed {
            // Nothing to do. At 30 fps this wakes about twice per frame.
            Timer::after(Duration::from_millis(4)).await;
            continue;
        }

        if dirty {
            // The setting lives in the framebuffer, not the peripheral, so it
            // has to be written into each of the two in turn.
            fb.set_oe_window(OE_START.load(Ordering::Relaxed) as usize, target_oe_slots());
            BRIGHTNESS_DIRTY.fetch_sub(1, Ordering::Relaxed);
        }

        let t0 = Instant::now();
        display::render(frames.front(), fb, m, phase);
        let us = t0.elapsed().as_micros() as u32;
        RENDER_US.store(us, Ordering::Relaxed);
        RENDER_US_MAX.fetch_max(us, Ordering::Relaxed);
        RENDER_US_MAX_WINDOW.fetch_max(us, Ordering::Relaxed);
        RENDER_US_MAX_PROTO.fetch_max(us, Ordering::Relaxed);
        phase = phase.wrapping_add(1);

        let mut xfer = hub75.swap(fb).expect("swap already in flight");
        xfer.wait_for_done().await;
        fb = xfer.wait().expect("hub75 DMA transfer failed");
        SWAPS.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// smoltcp socket slots the station's `embassy-net` stack is given.
///
/// **Counted on the device, not on paper.** The obvious count is five - the
/// frame port, the control port, mDNS, DHCP and card 222's `http_task` - and
/// six was tried first on that basis. It panicked: `SocketSet::add` found the
/// set full, so something already takes a sixth (`edge-nal-embassy`'s `Udp`
/// holds more than the one socket its buffer type names). Seven is five plus
/// the measured extra plus one genuinely spare slot, and the spare is what the
/// `http-selftest` build's client socket uses.
///
/// `StackResources` is `.bss`, and `.bss` is core 0's stack, so this is not a
/// free number. Card 223's AP gets its own stack and its own resources, so it
/// does not need room here.
///
/// **Eight since card 227**, because [`http::HTTP_TASKS`] is two and each
/// worker holds its own `TcpSocket` for as long as it is listening or
/// serving. Seven would have been exactly enough and left no spare at all,
/// and the failure mode of getting this wrong is not a degraded server: it is
/// `SocketSet::add` panicking on the first poll of a task, which is a boot
/// loop. 408 bytes is the right price for the slot that is not needed.
const NET_SOCKETS: usize = 8;

// Card 223: the attempt count (spec 8.3's three), the retry interval and the
// whole fallback ladder are `screeny_provision::Timing` and
// `Provisioner::step`. They were three constants and a hand-written loop here;
// they are one host-tested state machine now, and `src/provision.rs` is the
// only thing that drives it.

/// A credential pair `SET_WIFI` wants the station to switch to.
///
/// A [`Signal`] and not a channel: only the newest request matters, which is
/// exactly what a `Signal` keeps. It carries a [`Wifi`], whose `Debug` is safe
/// because `Psk`'s prints a byte count and nothing else (spec section 8.4) -
/// and nothing formats it anyway.
pub static NEW_WIFI: Signal<CriticalSectionRawMutex, NewWifi> = Signal::new();

/// Credentials somebody asked the station to try, and whether to keep them.
///
/// **They are written to flash only after they have joined**, by the WiFi task,
/// never by the handler that received them. Firmware 0.4.0 wrote them first
/// (spec 8.2 then said "store, then try"), and one mistyped password posted
/// over HTTP replaced a working pair in flash: the device stayed up on its
/// in-RAM fallback until the next reboot and then could not join anything.
#[derive(Clone)]
pub struct NewWifi {
    /// The network to try.
    pub wifi: Wifi,
    /// Commit to the store once (and only if) the join succeeds.
    pub persist: bool,
}

/// The SSID the station is currently configured for, for `GET_WIFI`.
///
/// [`screeny_receiver::Host::wifi`] must return a `&'static str`, so this is a
/// fixed static buffer the Wi-Fi task fills at join time rather than anything
/// borrowed from the store.
///
/// **One writer, one reader, neither of which suspends.** [`set_current_ssid`]
/// is called only from the Wi-Fi task and [`current_ssid`] only from the
/// receiver's synchronous control handler; both are on core 0's single
/// executor, neither contains an `await`, so they cannot interleave, and core 1
/// never touches this.
///
/// An 802.11 SSID is opaque bytes while `GET_WIFI` is typed as text, so a
/// non-UTF-8 SSID is stored **lossily** - anything that is not printable ASCII
/// becomes `?`. That keeps the buffer valid UTF-8 at every instant, which is
/// what lets the reader hand out a `&'static str` at all.
struct CurrentSsid {
    buf: UnsafeCell<[u8; MAX_SSID_LEN]>,
    len: AtomicUsize,
}

// SAFETY: see the type's documentation - single writer, single reader, no
// suspension point between the write and the read, and one core.
unsafe impl Sync for CurrentSsid {}

static CURRENT_SSID: CurrentSsid = CurrentSsid {
    buf: UnsafeCell::new([0; MAX_SSID_LEN]),
    len: AtomicUsize::new(0),
};

#[cfg_attr(feature = "apsta-probe", allow(dead_code))]
fn set_current_ssid(bytes: &[u8]) {
    let n = bytes.len().min(MAX_SSID_LEN);
    let utf8 = core::str::from_utf8(&bytes[..n]).is_ok();
    // SAFETY: see [`CurrentSsid`].
    let dst = unsafe { &mut *CURRENT_SSID.buf.get() };
    for (i, b) in bytes[..n].iter().enumerate() {
        dst[i] = if utf8 || b.is_ascii_graphic() || *b == b' ' {
            *b
        } else {
            b'?'
        };
    }
    CURRENT_SSID.len.store(n, Ordering::Release);
}

/// The SSID `GET_WIFI` reports: the one actually in use, not the one the build
/// was compiled with. Empty before the first join attempt.
pub fn current_ssid() -> &'static str {
    let n = CURRENT_SSID.len.load(Ordering::Acquire);
    // SAFETY: see [`CurrentSsid`].
    let buf = unsafe { &*CURRENT_SSID.buf.get() };
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

/// The join state `GET_WIFI` reports (spec section 6.3).
///
/// Card 223: straight from the one `Provisioner`, sticky `FAILED` and all.
/// The firmware no longer keeps a second opinion about it.
pub fn wifi_report_state() -> u8 {
    provision::wifi_state()
}

/// The pair this build was compiled with, or `None` - which is every default
/// build (owner's decision, 2026-09-20). `crates/provision` models the same
/// thing as `has_builtin: false`.
pub fn builtin_wifi() -> Option<Wifi> {
    #[cfg(feature = "bench-wifi")]
    {
        // `build.rs` has already checked both lengths against 802.11's limits,
        // so this cannot fail; `ok()` rather than `expect` keeps a panic out of
        // the boot path regardless.
        Wifi::new(SSID.as_bytes(), PASSWORD.as_bytes()).ok()
    }
    #[cfg(not(feature = "bench-wifi"))]
    {
        None
    }
}

/// Turn a stored credential pair into a radio configuration.
///
/// `None` when the radio cannot express it: an SSID longer than 32 bytes, or a
/// PSK that is not UTF-8 (`esp-radio`'s `Password` is built from text, and
/// every PSK that can reach the store came in over `SET_WIFI`, which spec
/// section 8.2 types as UTF-8). Nothing here logs, returns or otherwise leaks
/// the PSK (spec section 8.4).
fn station_config(w: &Wifi) -> Option<StationConfig> {
    let ssid = esp_radio::wifi::Ssid::try_from(w.ssid.as_bytes()).ok()?;
    let psk = core::str::from_utf8(w.psk.as_bytes()).ok()?;
    let authentication = if psk.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(psk.try_into().ok()?)
    };
    Some(
        StationConfig::default()
            .with_ssid(ssid)
            // The default fast scan joins the first access point that answers.
            // On a mesh that is a lottery: five boots in a row picked five
            // different nodes, from -54 to -78 dBm (card 220). Scanning every
            // channel lets the driver's by-signal sort choose the strongest,
            // for ~2 s at join. **Keep this** in anything that builds a
            // `StationConfig`.
            .with_scan_method(ScanMethod::AllChannels)
            .with_authentication(authentication),
    )
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

// ---------------------------------------------------------------------------
// Telemetry (serial)
// ---------------------------------------------------------------------------

/// The numbers the camera cannot measure, on the serial log. The protocol's
/// own telemetry is spec section 6.7 and goes out over UDP; this is for a
/// bench session watching `espflash monitor`.
#[embassy_executor::task]
async fn telemetry_task() {
    const PERIOD_S: u32 = 5;
    /// Card 220: one stack line, at one minute. Long enough to include the
    /// WiFi association, DHCP, mDNS and a minute of the decode path, which is
    /// every deep call this firmware makes; short enough that a bench session
    /// sees it. Once, not periodically — the scan is cheap but the number
    /// only moves when something new goes deep, and a line per period would
    /// bury the telemetry.
    const STACK_TICK: u32 = 60 / PERIOD_S;

    let mut last_swaps = 0u32;
    let mut last_rx = 0u32;
    let mut last_shown = 0u32;
    let mut tick = 0u32;
    loop {
        Timer::after(Duration::from_secs(PERIOD_S as u64)).await;
        // **Card 241b: this is the device's liveness proof.** One feed per
        // five seconds, from a task that does nothing else load-bearing, so
        // "the watchdog was fed" means exactly "core 0's executor ran
        // something" and nothing more. `ota::LIVENESS_WDT_S` is four of these.
        ota::feed();
        tick += 1;
        if tick == STACK_TICK
            && let Some(hw) = stack_probe::CORE0.high_water()
        {
            info!(
                "stack: core 0 main high-water {} of {} bytes, {} free (painted at boot)",
                hw,
                stack_probe::CORE0.size(),
                stack_probe::CORE0.headroom().unwrap_or(0),
            );
            // Card 227: the same line for core 1, which until this card had
            // never been measured at all. Its stack is ordinary `.bss`, so
            // whatever it does not use is core 0's `.stack` being held hostage.
            if let Some(hw1) = stack_probe::CORE1.high_water() {
                info!(
                    "stack: core 1 display high-water {} of {} bytes, {} free (painted before the core started)",
                    hw1,
                    stack_probe::CORE1.size(),
                    stack_probe::CORE1.headroom().unwrap_or(0),
                );
            }
        }
        let swaps = SWAPS.load(Ordering::Relaxed);
        let stats = esp_alloc::HEAP.stats();
        let t = {
            let mut guard = net::CORE.lock().await;
            let core = guard.as_mut().expect("core exists");
            core.telemetry(EmbassyInstant::now().as_micros())
        };
        info!(
            "telemetry: {} fps rx, {} fps shown, {} swaps/s | drops stale {} superseded {} decode {} rejected {} gaps {} | ia {} us jit {} us | decode {} us (max {}) | render {} us (max {} window, {} boot) | state {} codec {:#04x} rssi {} bright {} | heap {}/{}",
            t.frames_rx.wrapping_sub(last_rx) / PERIOD_S,
            t.frames_shown.wrapping_sub(last_shown) / PERIOD_S,
            swaps.wrapping_sub(last_swaps) / PERIOD_S,
            t.frames_dropped_stale,
            t.frames_dropped_superseded,
            t.frames_dropped_decode,
            t.frames_rejected,
            t.seq_gaps,
            t.interarrival_us,
            t.jitter_us,
            t.decode_us,
            t.decode_us_max,
            RENDER_US.load(Ordering::Relaxed),
            RENDER_US_MAX_WINDOW.swap(0, Ordering::Relaxed),
            RENDER_US_MAX.load(Ordering::Relaxed),
            t.state,
            t.last_codec,
            t.rssi_dbm,
            t.brightness,
            stats.current_usage,
            stats.size,
        );
        last_swaps = swaps;
        last_rx = t.frames_rx;
        last_shown = t.frames_shown;
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_pin(pin: &impl Pin, expected: u8) {
    assert_eq!(pin.number(), expected, "pin map disagrees with tidbyt::pins");
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    // First, before anything has had a chance to go deep: card 220's paint.
    // Everything below this line is inside the measurement.
    stack_probe::paint_core0();

    esp_println::logger::init_logger_from_env();
    // Card 243, and **before anything that can fail**: count this boot, say
    // what the last one left in RTC memory, and learn whether the crash-loop
    // guard has latched. A panic in the boot path below is then counted like
    // any other, which is the case the guard exists for.
    let crashed = panic::boot();
    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // **Card 241b, and the first thing after `esp_hal::init`.** That call has
    // just disabled every watchdog this chip has - `rtc.swd`, `rtc.rwdt` and
    // both timer-group WDTs - so from here until something arms one there is
    // nothing at all that would catch a hang, and this device has now gone
    // silently quiet twice (cards 234 and 241b) with no panic, no reset and no
    // evidence. So core 0 gets a liveness watchdog on **every** boot, fed by
    // the telemetry task's five-second loop, and a device that stops
    // scheduling tasks reboots in twenty seconds and says so in its first
    // line instead of waiting for somebody to walk over and unplug it.
    //
    // Synchronous, and deliberately so: card 241 arm`ed` an RTC watchdog here
    // through an `async fn` that `await`ed a mutex **before `esp_rtos::start`
    // had run**, and held an `esp_hal::rtc_cntl::Rtc` in a static for the life
    // of the device. `Wdt::<TIMG0>::new()` needs no peripheral token and no
    // storage, so none of that is necessary - see this card's Log.
    ota::arm_liveness_watchdog();

    // 64 KB of reclaimed ROM DRAM, which lives above `_stack_start_cpu0` and
    // costs nothing, plus a smaller slice of ordinary `.bss`.
    //
    // Card 007 shipped 64 + 48 KB and measured 45.4 KB in use. Card 008 adds
    // ~40 KB of `.bss` — three 6 KB frame slots, the cross-fade source, core
    // 1's stack and two more sockets — and on this chip **`.data`, `.bss` and
    // core 0's main stack come out of the same pocket**: the stack is whatever
    // is left between `_bss_end` and 0x3ffe0000. Taking 16 KB back off the
    // heap is what pays for it.
    //
    // Card 220 took the 24 KB of framebuffer temporaries off that stack (see
    // [`FB0`]) and then measured what is left: `stack_probe` paints the region
    // at boot and the telemetry task reports the high-water mark once, at the
    // 60 s mark. Read that line before moving either number here. The heap
    // side of the same question — what the radio wants with a soft-AP up —
    // is the `apsta-probe` build.
    // Card 227 takes the second arena from 32 KB to 24 KB. It is the card's
    // last-resort lever and it was measured before it was pulled, against the
    // **APSTA** peak rather than the station one, because the station number
    // would have flattered it: `esp-alloc`'s own all-allocations watermark
    // (the `apsta-probe` build turns on `internal-heap-stats`) is the only
    // thing that sees the radio's transients, and it read 53,968 of 98,304
    // with a soft-AP up. Eight kilobytes off the ceiling still leaves the
    // worst instant of an APSTA run comfortably clear - see
    // `docs/research/010-stack-and-ram-levers.md` for the run. **24 KB is the
    // floor**: card 227 was told not to go below it and the margin above is
    // now small enough that the next person should measure again rather than
    // shave.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 24 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // --- settings (card 212) ----------------------------------------------
    //
    // First, before the panel and before the receiver. Before the panel because
    // the two framebuffers are formatted for an output-enable window that has
    // to be the *stored* brightness, not the default one, or the first refresh
    // is visibly wrong. Before the receiver because `Params` seeds the name and
    // the idle mode, and mDNS should announce the stored name from its very
    // first announcement rather than rename itself a second later.
    //
    // This is a read, and core 1 is not running yet, so nothing is parked.
    let (settings, _report) = store::init(peripherals.FLASH).await;
    // Card 222: `fw_slot` and `fw_state` for `GET /api/v1/status`, read once
    // here rather than per request - it needs the `STORE` lock, and nothing can
    // change the answer until card 241's confirm/revert lands. Card 243: it
    // takes the partition entries `store::init` already read, from beside the
    // flash handle, instead of reading the 3 KB table a second time underneath
    // esp-storage's own frames.
    http::read_fw_health().await;
    // Card 241: `read_fw_health` has just classified this boot from `otadata`
    // and the MMU. Since 241b there is nothing to arm or disarm here - the
    // liveness watchdog is already running and covers a trial that hangs far
    // better than the trial-only one did (20 s against 240) - so all this is
    // now is the answer to "does this boot need the confirm/revert task".
    let on_trial = ota::boot_class().on_trial();
    let boot_brightness = settings.brightness.min(BRIGHTNESS_CAP);
    BRIGHTNESS.store(boot_brightness, Ordering::Relaxed);

    // --- panel ------------------------------------------------------------
    assert_pin(&peripherals.GPIO21, tidbyt::pins::R1);
    assert_pin(&peripherals.GPIO2, tidbyt::pins::G1);
    assert_pin(&peripherals.GPIO22, tidbyt::pins::B1);
    assert_pin(&peripherals.GPIO23, tidbyt::pins::R2);
    assert_pin(&peripherals.GPIO4, tidbyt::pins::G2);
    assert_pin(&peripherals.GPIO27, tidbyt::pins::B2);
    assert_pin(&peripherals.GPIO26, tidbyt::pins::A);
    assert_pin(&peripherals.GPIO5, tidbyt::pins::B);
    assert_pin(&peripherals.GPIO25, tidbyt::pins::C);
    assert_pin(&peripherals.GPIO18, tidbyt::pins::D);
    assert_pin(&peripherals.GPIO14, tidbyt::pins::E_UNUSED);
    assert_pin(&peripherals.GPIO19, tidbyt::pins::LAT);
    assert_pin(&peripherals.GPIO32, tidbyt::pins::OE);
    assert_pin(&peripherals.GPIO33, tidbyt::pins::CLK);

    {
        let out = OutputConfig::default();
        let mut rgb = [
            Output::new(peripherals.GPIO21.reborrow(), Level::Low, out), // R1
            Output::new(peripherals.GPIO23.reborrow(), Level::Low, out), // R2
            Output::new(peripherals.GPIO2.reborrow(), Level::Low, out),  // G1
            Output::new(peripherals.GPIO4.reborrow(), Level::Low, out),  // G2
            Output::new(peripherals.GPIO22.reborrow(), Level::Low, out), // B1
            Output::new(peripherals.GPIO27.reborrow(), Level::Low, out), // B2
        ];
        let mut clk = Output::new(peripherals.GPIO33.reborrow(), Level::Low, out);
        let mut lat = Output::new(peripherals.GPIO19.reborrow(), Level::Low, out);
        let mut oe = Output::new(peripherals.GPIO32.reborrow(), Level::High, out);
        panel_init::fm6124_init(&mut rgb, &mut clk, &mut lat, &mut oe);
    }

    let pins = Hub75Pins16 {
        // This unit's colour lines are rotated relative to the hdk's names:
        // the lines it calls R/G/B drive blue/red/green. Confirmed on the
        // panel at bring-up (card 001) and again by card 007's test card.
        // Card 021 owns the board-revision story.
        red1: peripherals.GPIO2.degrade(),
        grn1: peripherals.GPIO22.degrade(),
        blu1: peripherals.GPIO21.degrade(),
        red2: peripherals.GPIO4.degrade(),
        grn2: peripherals.GPIO27.degrade(),
        blu2: peripherals.GPIO23.degrade(),
        addr0: peripherals.GPIO26.degrade(),
        addr1: peripherals.GPIO5.degrade(),
        addr2: peripherals.GPIO25.degrade(),
        addr3: peripherals.GPIO18.degrade(),
        addr4: peripherals.GPIO14.degrade(),
        blank: peripherals.GPIO32.degrade(),
        clock: peripherals.GPIO33.degrade(),
        latch: peripherals.GPIO19.degrade(),
    };

    let (producer, consumer) = SLOTS.split();

    // --- core 1 ------------------------------------------------------------
    //
    // The panel is built *here*, inside the second core's entry point, rather
    // than on core 0 and moved. `esp_hal` enables an interrupt on whichever
    // core asks, so constructing the HUB75 driver on core 1 puts its DMA
    // completion interrupt on core 1 too. Built on core 0 it would still
    // work, but every one of the 154 refreshes a second would take an
    // interrupt on the core we are trying to keep free.
    let i2s = peripherals.I2S0;
    let dma = peripherals.DMA_I2S0;
    // Only the `&'static mut`s cross over to core 1; the buffers themselves
    // are the two statics at the top of this file and are never on anybody's
    // stack. See [`FB0`].
    #[cfg(not(feature = "fb-on-stack"))]
    let (fb0, fb1) = (FB0.take(), FB1.take());
    // The card 220 A/B: the construction as it was, with a 12 KB temporary on
    // core 0's main stack for each buffer. Only for measuring against.
    #[cfg(feature = "fb-on-stack")]
    let (fb0, fb1) = {
        warn!("display: BENCH BUILD - framebuffers built on core 0's stack");
        (
            mk_static!(FrameBuffer, FrameBuffer::new()),
            mk_static!(FrameBuffer, FrameBuffer::new()),
        )
    };
    // Come up already dimmed, and at the *stored* brightness. `FrameBuffer::new()`
    // formats for the widest output-enable window the build can produce, which
    // is well over the power cap; letting a single refresh out at that duty
    // would be a bug with a current spike attached to it.
    let slots = display::slots_for(boot_brightness);
    fb0.set_oe_slots(slots);
    fb1.set_oe_slots(slots);
    BRIGHTNESS_DIRTY.store(0, Ordering::Relaxed);
    let tx_descriptors = esp_hub75::hub75_dma_descriptors!(FrameBuffer);

    let build_hub75 = move || {
        Hub75::new_async(
            i2s,
            pins,
            dma,
            tx_descriptors,
            Hub75Config::new()
                .with_frequency(PIXEL_CLOCK)
                .with_interrupt_priority(Priority::Priority3),
            &*fb0,
        )
        .expect("hub75 init failed")
    };

    #[cfg(not(feature = "display-on-core0"))]
    let app_core_stack = {
        let s = APP_CORE_STACK.take();
        // Card 227: paint it here, from core 0, while the core it belongs to
        // has not started and therefore nothing is live on it. `bottom()` and
        // `top()` want `&mut`, which is why the `take()` is hoisted out of the
        // `start_second_core` call it used to be an argument of.
        let (bottom, top) = (s.bottom() as usize, s.top() as usize);
        // SAFETY: core 1 is not running yet - `start_second_core` is the next
        // statement - so no frame is live anywhere in this region.
        unsafe { stack_probe::paint_core1(bottom, top) };
        s
    };

    #[cfg(not(feature = "display-on-core0"))]
    esp_rtos::start_second_core(
        peripherals.CPU_CTRL,
        peripherals.FROM_CPU_INTR1,
        app_core_stack,
        move || {
            let hub75 = build_hub75();
            let executor = mk_static!(
                esp_rtos::embassy::Executor,
                esp_rtos::embassy::Executor::new()
            );
            executor.run(|spawner| {
                spawner.spawn(display_task(hub75, fb1, consumer).unwrap());
            });
        },
    );

    // The `display-on-core0` build exists only to be measured against the
    // one above: same decode path, same triple buffer, same everything, with
    // the display sharing core 0's executor the way card 007 shipped it. It
    // is not a configuration to run the panel in.
    #[cfg(feature = "display-on-core0")]
    {
        let _ = (peripherals.CPU_CTRL, peripherals.FROM_CPU_INTR1);
        warn!("display: BENCH BUILD - display task is on core 0");
        spawner.spawn(display_task(build_hub75(), fb1, consumer).unwrap());
    }

    // --- the crash-loop guard's last stop (card 243) -----------------------
    //
    // The breadcrumb says this device has panicked `CRASH_LOOP_MAX` times in a
    // row, each within a minute of a boot. It stops here: the panel says so and
    // nothing else is started - no radio, no HTTP, no stream - because a panel
    // that reboots for ever on USB power is worse than one that says it is
    // broken (card 243, the owner's "pretty crash proof").
    //
    // **Here** and not earlier, because this is the first point at which there
    // is a panel to say it on, and not later, because everything below is a
    // thing that could panic again. A power cycle clears the breadcrumb - the
    // RTC region is zeroed on a power-on reset and on nothing else - so the
    // recovery is the one the owner would try anyway.
    if crashed {
        // Read the breadcrumb again here rather than carrying it from
        // `panic::boot()`: `main`'s locals live in a future, which is `.bss`,
        // which is core 0's stack (card 243's stack fix).
        let crumb = panic::report();
        let last = crumb.last;
        let (file, line) = match &last {
            Some(p) => (p.file(), p.line),
            None => ("?", 0),
        };
        // Card 241b: **the one place the liveness watchdog must be turned
        // off.** This branch parks the device on purpose, with no tasks and a
        // message on the panel, and a watchdog would turn the crash-loop
        // guard's deliberate stop into the boot loop it exists to end.
        ota::stop_liveness_watchdog();
        let mut producer = producer;
        screens::crashed(producer.back(), file, line, crumb.panics);
        producer.publish();
        warn!(
            "boot: stopped after {} panics ({}:{}). The panel says CRASHED; power-cycle to clear.",
            crumb.panics, file, line
        );
        loop {
            Timer::after(Duration::from_secs(60)).await;
        }
    }

    // --- wifi -------------------------------------------------------------
    //
    // The controller is built with whatever `StationConfig::default()` is; the
    // real one is applied by `try_join`, which is the only place that turns a
    // credential pair into a radio configuration. That keeps `ScanMethod::AllChannels`
    // and the authentication method in one function instead of two.
    let mut controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default()
            .with_initial_config(WifiConfig::Station(StationConfig::default()))
            .with_rx_queue_size(3)
            .with_country_info(*b"US"),
    )
    .expect("wifi init failed");

    controller
        .set_power_saving(PowerSaveMode::None)
        .expect("failed to disable power saving");

    let interface = Interface::station();

    // Spec section 5.1: the device id is the last three bytes of the station
    // MAC in lowercase hex, and it is what `id=`, the default name and the
    // mDNS host name are all built from.
    let mac = interface.mac_address();
    // `&'static str`, not a local: `GET /api/v1/status` reports it and so does
    // the mDNS TXT of the `_http._tcp` service, and both live in tasks.
    let id: &'static str = {
        let s = mk_static!(heapless::String<8>, heapless::String::new());
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for b in &mac[3..6] {
            let _ = s.push(HEX[(b >> 4) as usize] as char);
            let _ = s.push(HEX[(b & 0xf) as usize] as char);
        }
        s.as_str()
    };
    info!("device: mac {:02x?} id {}", mac, id);

    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;
    // Card 222: a number the Studio can use to tell "the device rebooted" from
    // "the link flapped", drawn from the hardware RNG once and never stored -
    // a persisted counter would cost a flash write every boot.
    http::BOOT_ID.store(rng.random(), Ordering::Relaxed);

    let (stack, runner) = embassy_net::new(
        interface,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<NET_SOCKETS>, StackResources::<NET_SOCKETS>::new()),
        seed,
    );

    {
        let mut guard = net::CORE.lock().await;
        *guard = Some(receiver::Core::new(id, &settings));
    }

    let host: &'static str = {
        let s = mk_static!(heapless::String<16>, heapless::String::new());
        let _ = s.push_str("screeny-");
        let _ = s.push_str(id);
        s.as_str()
    };

    // --- provisioning (card 223) -------------------------------------------
    //
    // The soft-AP's own `embassy-net` stack is built here, at boot, whether or
    // not the AP is ever raised: an embassy task's future and a `StaticCell`
    // are `.bss` either way, so building it on demand would save nothing in
    // the pool that breaks first. See `provision::ap_stack`.
    let (ap_stack, ap_runner) = provision::ap_stack(seed ^ 0x5a5a_5a5a);

    // The **bench feature that boots as if the store were empty**, without
    // touching a byte of it. The credentials stay exactly where they are - the
    // owner's real pair is the only way this device gets on a network - and
    // the machine is simply told there are none, so it goes straight to
    // `Portal` and raises the AP. A real trial typed into the form then joins,
    // and `CommitCredentials` writes the same pair back over itself.
    #[cfg(feature = "start-in-portal")]
    let stored = {
        warn!("provision: BENCH BUILD - ignoring the stored credentials (they are NOT erased)");
        let _ = &settings.wifi;
        None
    };
    #[cfg(not(feature = "start-in-portal"))]
    let stored = settings.wifi.clone();

    // Device-web decision 6: a default build has no compile-time credentials
    // at all, and `bench-wifi` is the one exception. The machine models it as
    // `has_builtin`, and a built-in pair that joins seeds an *empty* store
    // through `CommitCredentials { Builtin }` - which is the only place in
    // this firmware that seeds one now.
    provision::init(host, stored.is_some(), builtin_wifi().is_some());

    // Card 220's measurement build owns the controller instead, because the
    // switch into APSTA, the re-association and the AP's own stack all have
    // to happen in order and in one place. The default build is unaffected.
    #[cfg(not(feature = "apsta-probe"))]
    {
        spawner.spawn(provision::provision_task(controller, stored, host, stack).unwrap());
        spawner.spawn(provision::ap_net_task(ap_runner).unwrap());
        spawner.spawn(provision::dhcp_task(ap_stack).unwrap());
        spawner.spawn(provision::dns_task(ap_stack).unwrap());
    }
    #[cfg(feature = "apsta-probe")]
    {
        let _ = (ap_stack, stored);
        spawner.spawn(
            apsta_probe::probe_task(controller, host, ap_runner, settings.wifi.clone()).unwrap(),
        );
        spawner.spawn(apsta_probe::heap_task().unwrap());
    }
    spawner.spawn(net_task(runner).unwrap());
    // On core 0, like everything else that touches flash (research 006 §4).
    spawner.spawn(store::store_task().unwrap());
    #[cfg(feature = "store-selftest")]
    spawner.spawn(store::selftest::selftest_task(settings.clone()).unwrap());
    spawner.spawn(net::frames_task(stack, producer, host).unwrap());
    spawner.spawn(net::control_task(stack).unwrap());
    spawner.spawn(mdns::mdns_task(stack, host, id).unwrap());
    spawner.spawn(telemetry_task().unwrap());
    // Card 227: says when a stack goes deeper than it ever has, at 4 Hz, so
    // the line lands next to whatever caused it. See `stack_probe`.
    spawner.spawn(stack_probe::watch_task().unwrap());

    // Card 222: the LAN web server. `init` first - the handlers reach the
    // stack, the id and the host name through it, and it must be set before a
    // worker can accept a connection.
    http::init(http::Ctx { id, host });
    for i in 0..http::HTTP_TASKS {
        spawner.spawn(http::http_task(i, stack, ap_stack).unwrap());
    }
    spawner.spawn(http::deferred_task().unwrap());
    // Card 241. `activate_task` sleeps on a signal that only an accepted
    // upload raises, and is what writes `otadata` and resets the chip - out of
    // the HTTP handler, after the reply has been acknowledged.
    spawner.spawn(ota::activate_task().unwrap());
    // `trial_task` is spawned **only on a trial boot**, which is what makes
    // "this firmware cannot confirm an image that is not on trial" a property
    // of the program rather than of a check inside a loop: on an ordinary boot
    // the task does not exist and nothing writes `otadata` at all.
    if on_trial {
        spawner.spawn(ota::trial_task().unwrap());
    }
    // Card 241's two bench builds, each a deliberately broken update. Neither
    // does anything on a boot that is not a trial, because neither is any use
    // except as the image being tried.
    #[cfg(feature = "ota-test-unhealthy")]
    if on_trial {
        warn!(
            "ota-test: BENCH BUILD - this image will never report healthy, so it must be reverted at {} s (card 241)",
            screeny_otastate::REVERT_AT_MS / 1000
        );
    }
    #[cfg(feature = "ota-test-panic")]
    spawner.spawn(ota::panic_test_task().unwrap());
    // Card 222's `http-selftest` used to be spawned here. Card 243 moved it
    // into HTTP worker 0, where it borrows that worker's buffers instead of
    // carrying 5.6 KB of `.bss` of its own - which is what put the build 4 KB
    // under the `fw-size.sh` floor. `http::selftest` has the arithmetic.
    // Card 243's bench build: one deliberate panic on core 0, once per
    // power-on. See the feature's comment in `Cargo.toml`.
    #[cfg(feature = "panic-test")]
    spawner.spawn(panic::panic_test_task().unwrap());

    // Card 200 spike: reachable from `main` so the linker keeps it and
    // `xtensa-esp32-elf-size` measures something real. Off by default. Its
    // settings half was retired by card 212 - `src/store.rs` is the real thing
    // now - so what is left is the OTA evidence, and it borrows the flash
    // handle the store already owns rather than trying to build a second
    // `FlashStorage` (which panics).
    #[cfg(feature = "spike-ota")]
    {
        let mut guard = store::STORE.lock().await;
        if let Some(f) = guard.as_mut() {
            let r = spike_ota::spike_report(f.raw());
            info!("spike: {:?}", r);
        }
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
