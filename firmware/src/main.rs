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
//! | 0 | `mdns` | `_screeny._udp`, TXT from the same bytes as `GET_INFO` |
//! | 0 | `telemetry` | the serial-log numbers a camera cannot measure |

#![no_std]
#![no_main]

/// Card 220's `apsta-probe` build: what APSTA costs in heap. Off by default.
#[cfg(feature = "apsta-probe")]
mod apsta_probe;
mod display;
mod fb;
mod gamma;
mod mdns;
mod net;
mod panel_init;
mod patterns;
mod receiver;
mod screens;
#[cfg(feature = "spike-ota")]
mod spike_ota;
mod stack_probe;
mod tidbyt;
/// Card 201's compile-only spike. Never flashed; see the module docs.
#[cfg(any(
    feature = "spike-ap",
    feature = "spike-http",
    feature = "spike-portal",
    feature = "spike-qr"
))]
mod web_spike;

use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, AtomicU8, Ordering};

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Instant as EmbassyInstant, Timer};
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
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, ControllerConfig, Interface, PowerSaveMode,
    WifiController,
};
use log::{info, warn};
use screeny_proto::control::wifi_state;

use display::Mode;

esp_bootloader_esp_idf::esp_app_desc!();

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Compile-time credentials, supplied by `build.rs` from outside git (environment,
/// `firmware/wifi.env`, or `~/.config/screeny/wifi.env`). Never write them here.
/// Runtime provisioning (a captive portal) is planned; until then this is the only
/// network we join.
pub const SSID: &str = env!("SCREENY_WIFI_SSID");
const PASSWORD: &str = env!("SCREENY_WIFI_PASSWORD");

/// The `fw=` TXT key and `GET_INFO` field.
pub const FW_VERSION: &str = "0.2.0";

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

/// Core 1's stack.
///
/// It runs one task whose deepest call is `display::render` (a 192-byte row
/// buffer), plus the HUB75 DMA interrupt at `Priority3`, which lands on
/// whatever stack is current. 16 KB is generous for that; esp-rtos checks the
/// guard on every switch and panics with the range, which is how the first
/// flash of this firmware reported an 8 KB stack being eaten by two 12 KB
/// framebuffers built in the wrong place.
#[cfg_attr(feature = "display-on-core0", allow(dead_code))]
static APP_CORE_STACK: static_cell::ConstStaticCell<CoreStack<16384>> =
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

/// The station's credentials, in one place: `main` uses it for the initial
/// `ControllerConfig`, and the `apsta-probe` build hands the same value back
/// to `set_config` when it switches the radio into APSTA. Nothing here logs,
/// returns or otherwise leaks the PSK (spec section 8.4).
fn station_config() -> StationConfig {
    StationConfig::default()
        .with_ssid(SSID.try_into().unwrap())
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
            PASSWORD.try_into().unwrap(),
        ))
}

/// Associate, hold the association, report RSSI, reconnect forever.
///
/// Extracted from [`wifi_task`] so the `apsta-probe` build can run exactly
/// this loop before and after it flips the radio into APSTA, instead of
/// keeping a second, slightly different copy of it. Never returns.
async fn station_loop(controller: &mut WifiController<'static>) {
    loop {
        WIFI_STATE.store(wifi_state::CONNECTING, Ordering::Relaxed);
        match controller.connect_async().await {
            Ok(info) => {
                info!(
                    "wifi: connected ssid {:?} ch {} bssid {:02x?}",
                    info.ssid, info.channel, info.bssid
                );
                WIFI_STATE.store(wifi_state::CONNECTED, Ordering::Relaxed);
                // Poll the beacon RSSI for telemetry byte 44 and the status
                // screen's bars, and notice a disconnect either way.
                loop {
                    if let Ok(r) = controller.rssi() {
                        RSSI_DBM.store(r.clamp(-128, 0) as i8, Ordering::Relaxed);
                    }
                    match embassy_time::with_timeout(
                        Duration::from_secs(2),
                        controller.wait_for_disconnect_async(),
                    )
                    .await
                    {
                        Ok(reason) => {
                            warn!("wifi: disconnected {:?}", reason);
                            break;
                        }
                        Err(_) => continue,
                    }
                }
                WIFI_STATE.store(wifi_state::DISCONNECTED, Ordering::Relaxed);
                RSSI_DBM.store(0, Ordering::Relaxed);
            }
            Err(e) => {
                warn!("wifi: connect failed {:?}", e);
                WIFI_STATE.store(wifi_state::FAILED, Ordering::Relaxed);
            }
        }
        Timer::after(Duration::from_millis(2000)).await;
    }
}

#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>) {
    station_loop(&mut controller).await
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
        if tick == STACK_TICK {
            if let Some(hw) = stack_probe::high_water() {
                info!(
                    "stack: core 0 main high-water {} of {} bytes, {} free (painted at boot)",
                    hw,
                    stack_probe::size(),
                    stack_probe::headroom().unwrap_or(0),
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
    stack_probe::paint();

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
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

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
    // Come up already dimmed. `FrameBuffer::new()` formats for the widest
    // output-enable window the build can produce, which is well over the
    // power cap; letting a single refresh out at that duty would be a bug
    // with a current spike attached to it.
    let slots = display::slots_for(display::DEFAULT_BRIGHTNESS);
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
    esp_rtos::start_second_core(
        peripherals.CPU_CTRL,
        peripherals.FROM_CPU_INTR1,
        APP_CORE_STACK.take(),
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
    let mut controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default()
            .with_initial_config(WifiConfig::Station(station_config()))
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
    let id = {
        let mut s = heapless::String::<8>::new();
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for b in &mac[3..6] {
            let _ = s.push(HEX[(b >> 4) as usize] as char);
            let _ = s.push(HEX[(b & 0xf) as usize] as char);
        }
        s
    };
    info!("device: mac {:02x?} id {}", mac, id.as_str());

    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        interface,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<6>, StackResources::<6>::new()),
        seed,
    );

    {
        let mut guard = net::CORE.lock().await;
        *guard = Some(receiver::Core::new(&id));
    }

    let host: &'static str = {
        let s = mk_static!(heapless::String<16>, heapless::String::new());
        let _ = s.push_str("screeny-");
        let _ = s.push_str(&id);
        s.as_str()
    };

    // Card 220's measurement build owns the controller instead, because the
    // switch into APSTA, the re-association and the AP's own stack all have
    // to happen in order and in one place. The default build is unaffected.
    #[cfg(not(feature = "apsta-probe"))]
    spawner.spawn(wifi_task(controller).unwrap());
    #[cfg(feature = "apsta-probe")]
    {
        let (ap_stack, ap_runner) = apsta_probe::ap_stack(seed ^ 0x5a5a_5a5a);
        let _ = ap_stack;
        spawner.spawn(apsta_probe::probe_task(controller, host, ap_runner).unwrap());
        spawner.spawn(apsta_probe::heap_task().unwrap());
    }
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(net::frames_task(stack, producer, host).unwrap());
    spawner.spawn(net::control_task(stack).unwrap());
    spawner.spawn(mdns::mdns_task(stack, host).unwrap());
    spawner.spawn(telemetry_task().unwrap());

    // Card 200 spike: reachable from `main` so the linker keeps it and
    // `xtensa-esp32-elf-size` measures something real. Off by default.
    #[cfg(feature = "spike-ota")]
    {
        let r = spike_ota::spike_report(peripherals.FLASH).await;
        info!("spike: {:?}", r);
    }

    // Card 201's spike: the AP stack, the HTTP server, DHCP, DNS and the QR
    // portal screen, all reachable so the linker keeps them. Compile-only.
    #[cfg(any(
        feature = "spike-ap",
        feature = "spike-http",
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
