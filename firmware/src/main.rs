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

/// Card 220's `apsta-probe` build: what APSTA costs in heap. Off by default.
#[cfg(feature = "apsta-probe")]
mod apsta_probe;
mod display;
mod fb;
mod gamma;
mod http;
mod mdns;
mod net;
mod panel_init;
mod patterns;
mod receiver;
mod screens;
#[cfg(feature = "spike-ota")]
mod spike_ota;
mod stack_probe;
mod store;
mod tidbyt;
/// Card 201's compile-only spike. Never flashed; see the module docs.
#[cfg(any(feature = "spike-ap", feature = "spike-portal", feature = "spike-qr"))]
mod web_spike;

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant as EmbassyInstant, Timer};
use esp_backtrace as _;
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
    AuthenticationMethodConfig, Config as WifiConfig, ConnectionError, ControllerConfig, Interface,
    PowerSaveMode, WifiController,
};
use log::{info, warn};
use screeny_proto::control::{wifi_state, MAX_SSID_LEN};
use screeny_settings::Wifi;

use display::Mode;

esp_bootloader_esp_idf::esp_app_desc!();

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
/// `out_of_range`, and each route's own `max_request_len` is enforced.
pub const FW_VERSION: &str = "0.4.3";

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
/// The join state of `GET_WIFI`; see [`screeny_proto::control::wifi_state`].
pub static WIFI_STATE: AtomicU8 = AtomicU8::new(wifi_state::CONNECTING);

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

/// How many times one credential pair is tried before the next is (spec 8.3).
const JOIN_ATTEMPTS: u8 = 3;

/// How long the station waits before trying the stored credentials again once
/// every pair it has has failed. Long enough not to hold the radio (and, with
/// `ScanMethod::AllChannels`, ~2 s of scan) against the rest of the device;
/// short enough that an access point that came back is found without a reboot.
const RETRY_AFTER: Duration = Duration::from_secs(30);

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

/// Store credentials that have just proved themselves. A failure is logged and
/// counted (`store_errors` on the status page); the join stands either way.
async fn persist_joined(w: &Wifi) {
    let what = store::Immediate::Wifi { wifi: w.clone(), persist: true };
    match store::commit_immediate(&what).await {
        Ok(()) => info!("wifi: the new credentials joined and are now stored"),
        Err(e) => {
            store::FAILURES.fetch_add(1, Ordering::Relaxed);
            warn!("wifi: joined, but storing the credentials failed: {:?}", e);
        }
    }
}

/// Set when a `SET_WIFI` could not join and the station fell back to what it
/// had (spec section 8.2 step 4).
///
/// **Sticky**, on purpose. The question the sender asked was "does the network
/// I just gave you work?", and the answer is no; letting `GET_WIFI` go back to
/// `CONNECTED` a few seconds later - because the *old* network came back - would
/// read as a yes. It is cleared by a `SET_WIFI` that does join, and by a reboot.
static WIFI_SET_FAILED: AtomicBool = AtomicBool::new(false);

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

/// The join state `GET_WIFI` reports, with [`WIFI_SET_FAILED`] applied.
pub fn wifi_report_state() -> u8 {
    if WIFI_SET_FAILED.load(Ordering::Relaxed) {
        wifi_state::FAILED
    } else {
        WIFI_STATE.load(Ordering::Relaxed)
    }
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

/// Apply `w` and try to associate, at most `attempts` times.
///
/// The SSID goes into [`CURRENT_SSID`] before the first attempt, so `GET_WIFI`
/// describes what the device is *trying*, which is the useful answer while a
/// `SET_WIFI` is in flight.
async fn try_join(controller: &mut WifiController<'static>, w: &Wifi, attempts: u8) -> bool {
    let Some(cfg) = station_config(w) else {
        warn!("wifi: those credentials are not expressible to the radio (SSID too long, or a non-UTF-8 PSK)");
        return false;
    };
    set_current_ssid(w.ssid.as_bytes());
    if let Err(e) = controller.set_config(&WifiConfig::Station(cfg)) {
        warn!("wifi: set_config failed {:?}", e);
        WIFI_STATE.store(wifi_state::FAILED, Ordering::Relaxed);
        return false;
    }

    for attempt in 1..=attempts {
        WIFI_STATE.store(wifi_state::CONNECTING, Ordering::Relaxed);
        match controller.connect_async().await {
            Ok(info) => {
                // The one line in this firmware that says an SSID out loud.
                info!(
                    "wifi: connected ssid {:?} ch {} bssid {:02x?}",
                    info.ssid, info.channel, info.bssid
                );
                WIFI_STATE.store(wifi_state::CONNECTED, Ordering::Relaxed);
                http::clear_wifi_failure();
                return true;
            }
            // Only the reason, not the whole `DisconnectedInfo`: the reason is
            // the diagnostic, and it keeps one more copy of the SSID out of the
            // bench log.
            Err(ConnectionError::Failed(info)) => {
                http::note_wifi_failure(fail_reason(info.reason));
                warn!(
                    "wifi: join attempt {} of {} failed: {:?}",
                    attempt, attempts, info.reason
                );
            }
            Err(e) => {
                http::note_wifi_failure(screeny_device_api::FailReason::Other);
                warn!(
                    "wifi: join attempt {} of {} failed: {:?}",
                    attempt, attempts, e
                );
            }
        }
        Timer::after(Duration::from_millis(2000)).await;
    }
    WIFI_STATE.store(wifi_state::FAILED, Ordering::Relaxed);
    false
}

/// Sort a radio disconnect reason into the three buckets the HTTP API and the
/// portal page use (`crates/provision`'s `FailReason`, by way of
/// `crates/device-api`'s).
///
/// Only "wrong password" and "no such network" are worth telling apart: they
/// are the two a person standing at the device can act on, and everything else
/// is "try again". The 802.11 reason codes that mean a key exchange did not
/// complete are all `auth`, because from the outside they are all a wrong
/// password.
fn fail_reason(r: esp_radio::wifi::DisconnectReason) -> screeny_device_api::FailReason {
    use esp_radio::wifi::DisconnectReason as D;
    use screeny_device_api::FailReason as F;
    match r {
        D::NoAccessPointFound
        | D::NoAccessPointFoundWithCompatibleSecurity
        | D::NoAccessPointFoundInAuthmodeThreshold
        | D::NoAccessPointFoundInRssiThreshold => F::NotFound,
        D::AuthenticationFailed
        | D::AuthenticationExpired
        | D::FourWayHandshakeTimeout
        | D::HandshakeTimeout
        | D::MicFailure
        | D::GroupKeyUpdateTimeout
        | D::_802_1xAuthenticationFailed => F::Auth,
        _ => F::Other,
    }
}

/// How an association ended.
enum Held {
    /// The access point went away.
    Disconnected,
    /// `SET_WIFI` asked for a different network.
    NewCredentials(NewWifi),
}

/// Hold the association: poll the beacon RSSI for telemetry byte 44 and the
/// status screen's bars, notice a disconnect, and notice a `SET_WIFI`.
async fn hold(controller: &mut WifiController<'static>) -> Held {
    loop {
        if let Ok(r) = controller.rssi() {
            RSSI_DBM.store(r.clamp(-128, 0) as i8, Ordering::Relaxed);
        }
        let watch = with_timeout(
            Duration::from_secs(2),
            controller.wait_for_disconnect_async(),
        );
        match select(watch, NEW_WIFI.wait()).await {
            Either::First(Ok(reason)) => {
                warn!("wifi: disconnected {:?}", reason);
                return Held::Disconnected;
            }
            // The 2 s poll expired: go round, re-read the RSSI.
            Either::First(Err(_)) => continue,
            Either::Second(n) => return Held::NewCredentials(n),
        }
    }
}

/// Associate, hold the association, reconnect forever, and honour `SET_WIFI`.
///
/// Spec section 8.3's fallback, as amended by the owner on 2026-09-20:
///
/// 1. the **stored** credentials, [`JOIN_ATTEMPTS`] times;
/// 2. in a `bench-wifi` build only, the compiled-in pair, [`JOIN_ATTEMPTS`]
///    times. A default build has no step 2 and nothing to have one with;
/// 3. otherwise the existing "wifi failed" idle screen (`WIFI_STATE` is
///    `FAILED`, which is what `screens::status` draws from) and a retry of the
///    whole list every [`RETRY_AFTER`]. With no credentials at all there is
///    nothing to retry, so the task simply waits for a `SET_WIFI`; the portal
///    that makes that reachable is card 223.
///
/// A `SET_WIFI` that fails falls back to the pair that was working and sets
/// [`WIFI_SET_FAILED`], per section 8.2 step 4.
///
/// Extracted from [`wifi_task`] so the `apsta-probe` build runs exactly this
/// loop before and after it flips the radio into APSTA. Never returns.
pub async fn station_loop(controller: &mut WifiController<'static>, stored: Option<Wifi>) {
    let builtin = builtin_wifi();
    // What the station is using now. `SET_WIFI` replaces it, and a `SET_WIFI`
    // that cannot join puts the previous value back.
    let mut active = stored.or_else(|| builtin.clone());
    // Set while `active` holds credentials that asked to be kept and have not
    // joined yet; the first successful join of them is what stores them.
    let mut persist_on_join = false;

    loop {
        let Some(w) = active.clone() else {
            WIFI_STATE.store(wifi_state::FAILED, Ordering::Relaxed);
            warn!("wifi: no credentials stored and none compiled in - waiting for SET_WIFI (the setup portal is card 223)");
            let n = NEW_WIFI.wait().await;
            persist_on_join = n.persist;
            active = Some(n.wifi);
            continue;
        };

        if try_join(controller, &w, JOIN_ATTEMPTS).await {
            if core::mem::take(&mut persist_on_join) {
                persist_joined(&w).await;
            }
            match hold(controller).await {
                Held::Disconnected => {
                    WIFI_STATE.store(wifi_state::DISCONNECTED, Ordering::Relaxed);
                    RSSI_DBM.store(0, Ordering::Relaxed);
                    Timer::after(Duration::from_millis(2000)).await;
                }
                Held::NewCredentials(next) => {
                    info!("wifi: SET_WIFI - trying the new network, {} attempts", JOIN_ATTEMPTS);
                    // Section 8.2: the reply has already gone out; now drop the
                    // association we have. `NotConnected` here just means the
                    // link had already gone.
                    let _ = controller.disconnect_async().await;
                    RSSI_DBM.store(0, Ordering::Relaxed);
                    let NewWifi { wifi: next, persist } = next;
                    if try_join(controller, &next, JOIN_ATTEMPTS).await {
                        WIFI_SET_FAILED.store(false, Ordering::Relaxed);
                        if persist {
                            persist_joined(&next).await;
                        }
                        active = Some(next);
                        // The TXT record does not carry the SSID, but the
                        // address may well have changed; re-announce.
                        net::INFO_CHANGED.signal(());
                    } else {
                        warn!("wifi: SET_WIFI failed after {} attempts - falling back to the previous network; GET_WIFI reports FAILED", JOIN_ATTEMPTS);
                        WIFI_SET_FAILED.store(true, Ordering::Relaxed);
                        // `active` is unchanged, so the top of the loop retries
                        // the pair that was working.
                    }
                }
            }
            continue;
        }

        // Step 1 failed. Step 2 exists only in a `bench-wifi` build.
        if let Some(b) = builtin.clone() {
            if Some(&b) != active.as_ref() {
                info!("wifi: stored credentials failed - falling back to the build's (bench-wifi)");
                if try_join(controller, &b, JOIN_ATTEMPTS).await {
                    // Deliberately *not* stored: a failing stored pair is the
                    // owner's to replace, and silently overwriting it with the
                    // bench network would hide the problem.
                    match hold(controller).await {
                        Held::Disconnected => {
                            WIFI_STATE.store(wifi_state::DISCONNECTED, Ordering::Relaxed);
                            RSSI_DBM.store(0, Ordering::Relaxed);
                        }
                        Held::NewCredentials(next) => {
                            // Drop the bench association first, exactly as the
                            // stored-credentials path above does. Without this
                            // `connect_async` was called while still associated
                            // and never returned: found on the bench, recovering
                            // a device whose stored pair had gone bad.
                            let _ = controller.disconnect_async().await;
                            RSSI_DBM.store(0, Ordering::Relaxed);
                            persist_on_join = next.persist;
                            active = Some(next.wifi);
                        }
                    }
                    continue;
                }
            }
        }

        warn!(
            "wifi: nothing joined; the panel shows the failure screen, retrying in {} s",
            RETRY_AFTER.as_secs()
        );
        // Either wait out the retry interval or jump straight to a SET_WIFI.
        if let Either::Second(next) =
            select(Timer::after(RETRY_AFTER), NEW_WIFI.wait()).await
        {
            persist_on_join = next.persist;
            active = Some(next.wifi);
        }
    }
}

#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>, stored: Option<Wifi>) {
    station_loop(&mut controller, stored).await
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
    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

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
    // here rather than per request - it needs the `STORE` lock and a 3 KB
    // partition-table buffer, and nothing can change the answer until card
    // 241's confirm/revert lands.
    http::read_fw_health().await;
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

    // A `bench-wifi` build seeds an **empty** store from its compiled-in pair,
    // through the ordinary save path, so that from the first boot onwards the
    // device is running on stored credentials and the path that matters is the
    // one being exercised. A store that already holds credentials is never
    // overwritten: a `SET_WIFI` outranks the build. This is the one flash write
    // in the boot path, and by here core 1 is running, so it parks it.
    #[cfg(feature = "bench-wifi")]
    if settings.wifi.is_none()
        && let Some(w) = builtin_wifi()
    {
        store::seed_wifi(&w).await;
    }

    // Card 220's measurement build owns the controller instead, because the
    // switch into APSTA, the re-association and the AP's own stack all have
    // to happen in order and in one place. The default build is unaffected.
    #[cfg(not(feature = "apsta-probe"))]
    spawner.spawn(wifi_task(controller, settings.wifi.clone()).unwrap());
    #[cfg(feature = "apsta-probe")]
    {
        let (ap_stack, ap_runner) = apsta_probe::ap_stack(seed ^ 0x5a5a_5a5a);
        let _ = ap_stack;
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
        spawner.spawn(http::http_task(i, stack).unwrap());
    }
    spawner.spawn(http::deferred_task().unwrap());
    #[cfg(feature = "http-selftest")]
    spawner.spawn(http::selftest_task(stack).unwrap());

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

    // Card 201's spike: the AP stack, the HTTP server, DHCP, DNS and the QR
    // portal screen, all reachable so the linker keeps them. Compile-only.
    #[cfg(any(
        feature = "spike-ap",
        feature = "spike-portal",
        feature = "spike-qr"
    ))]
    {
        let ap_ssid = mk_static!(heapless::String<32>, {
            let mut s = heapless::String::new();
            let _ = s.push_str(host);
            s
        });
        #[cfg(feature = "spike-ap")]
        {
            let _ = web_spike::ap::apsta_config(
                esp_radio::wifi::sta::StationConfig::default(),
                ap_ssid.as_str(),
            );
            web_spike::spawn_all(spawner, seed ^ 0x5a5a_5a5a);
        }
        #[cfg(feature = "spike-qr")]
        {
            let f = mk_static!(display::Frame, display::Frame::new());
            web_spike::portal_frame_demo(f, ap_ssid.as_str());
        }
        web_spike::settle().await;
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
