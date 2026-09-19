//! screeny firmware — a Gen 1 Tidbyt as a UDP network frame buffer.
//!
//! Grown out of `spike/fw-skeleton` (card 001), which proved the stack
//! compiles and boots. Card 007 makes the *display half* right on the real
//! panel: orientation, ghosting, gamma, brightness, a status screen.
//!
//! Networking is still the spike's throwaway test receiver (`testcmd`); card
//! 008 replaces it with `crates/proto` once that exists.
//!
//! Task layout follows `docs/design/architecture.md`:
//!
//! | task | job |
//! |---|---|
//! | `display` | owns esp-hub75 and both DMA buffers; converts sRGB -> BCM planes and swaps |
//! | `wifi` | associate, reconnect forever, modem sleep off |
//! | `net` | embassy-net runner |
//! | `frames` | UDP 49374, newest frame wins |
//! | `status` | draws the idle screen when nothing is streaming |
//! | `mdns` | `_screeny._udp` responder |
//! | `telemetry` | the numbers a camera cannot measure |

#![no_std]
#![no_main]

mod display;
mod gamma;
mod mdns;
mod panel_init;
mod patterns;
mod status;
mod testcmd;
mod tidbyt;

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant as EmbassyInstant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig, Pin};
use esp_hal::interrupt::Priority;
use esp_hal::rng::Rng;
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

use display::{Frame, Mode};

esp_bootloader_esp_idf::esp_app_desc!();

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const SSID: &str = "Example-Wifi1";
const PASSWORD: &str = "password9";

const FRAME_PORT: u16 = 49374;
const CONTROL_PORT: u16 = 49375;

/// One frame per datagram, at most one Ethernet MTU of payload. Only holds
/// because `.cargo/config.toml` raises esp-radio's MTU to 1500.
const MAX_DATAGRAM: usize = 1472;

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

/// How long after the last datagram we go back to the status screen.
const STREAM_IDLE: Duration = Duration::from_millis(1500);

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// What the display task is asked to put on the panel. One writer at a time
/// by convention: `frames` owns it while a stream is running, `status` owns
/// it otherwise, and a pattern command freezes whatever it drew.
static FRAME: Mutex<CriticalSectionRawMutex, Frame> = Mutex::new(Frame::new());

/// Bumped by whoever last changed `FRAME`, so the display task can tell new
/// content from a repeat without taking the lock.
static FRAME_SEQ: AtomicU32 = AtomicU32::new(0);
static FRAMES_RECEIVED: AtomicU32 = AtomicU32::new(0);
static FRAMES_DROPPED: AtomicU32 = AtomicU32::new(0);

/// Panel refreshes actually pushed, for the measured refresh rate.
static SWAPS: AtomicU32 = AtomicU32::new(0);
/// Last sRGB888 -> DMA conversion, in microseconds.
static RENDER_US: AtomicU32 = AtomicU32::new(0);
/// Rolling maximum of the same, since it is the number that can eat a frame.
static RENDER_US_MAX: AtomicU32 = AtomicU32::new(0);

static BRIGHTNESS: AtomicU8 = AtomicU8::new(display::DEFAULT_BRIGHTNESS);
/// Bench-only escape hatch: raw output-enable slots, ignoring the power cap.
/// `u8::MAX` means "not overridden". See `testcmd`.
static OE_OVERRIDE: AtomicU8 = AtomicU8::new(u8::MAX);
static OE_OVERRIDE_DEADLINE_MS: AtomicU32 = AtomicU32::new(0);
/// Set whenever the brightness the panel is showing may be stale. Two,
/// because there are two framebuffers and the setting lives in the buffer.
static BRIGHTNESS_DIRTY: AtomicU8 = AtomicU8::new(2);

/// First pixel-clock slot of the scan row that the panel is allowed to be
/// lit for. **This is the anti-ghosting control.** A HUB75 panel ghosts onto
/// the scan-adjacent row when it is still lit as the next row's address goes
/// out, so the first slots after the latch have to stay dark. The
/// `trail-blank-8` feature sets the default; card 007 swept it on the bench
/// from here rather than by rebuilding eight times.
static OE_START: AtomicU8 = AtomicU8::new(FrameBuffer::OE_DEFAULT_START as u8);

static GAMMA_ON: AtomicBool = AtomicBool::new(Mode::DEFAULT.gamma);
static DITHER_ON: AtomicBool = AtomicBool::new(Mode::DEFAULT.dither);

/// `0` = follow the stream / status screen. `n > 0` = hold pattern `n - 1`.
static PATTERN_HOLD: AtomicU8 = AtomicU8::new(0);

/// Milliseconds since boot of the last received frame datagram.
static LAST_FRAME_MS: AtomicU32 = AtomicU32::new(0);

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static CELL: ::static_cell::StaticCell<$t> = ::static_cell::StaticCell::new();
        CELL.uninit().write($val)
    }};
}
pub(crate) use mk_static;

fn now_ms() -> u32 {
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
// Display
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
#[embassy_executor::task]
async fn display_task(hub75: Hub75<esp_hal::Async, FrameBuffer>, mut fb: &'static mut FrameBuffer) {
    info!(
        "display: {} planes, {} Hz refresh (driver), {} bytes/buffer, OE slots 0..={} (cap {}), OE start {}",
        PLANES,
        REFRESH_HZ,
        core::mem::size_of::<FrameBuffer>(),
        FrameBuffer::OE_SLOTS,
        display::MAX_OE_SLOTS,
        OE_START.load(Ordering::Relaxed),
    );

    let mut last_seq = u32::MAX;
    let mut phase: u16 = 0;
    loop {
        let dither = DITHER_ON.load(Ordering::Relaxed);
        let seq = FRAME_SEQ.load(Ordering::Relaxed);
        let dirty = BRIGHTNESS_DIRTY.load(Ordering::Relaxed) > 0;

        if !dither && seq == last_seq && !dirty {
            // Nothing to do. At 30 fps this wakes about twice per frame.
            Timer::after(Duration::from_millis(4)).await;
            continue;
        }
        last_seq = seq;

        if dirty {
            // The setting lives in the framebuffer, not the peripheral, so it
            // has to be written into each of the two in turn.
            fb.set_oe_window(
                OE_START.load(Ordering::Relaxed) as usize,
                target_oe_slots(),
            );
            BRIGHTNESS_DIRTY.fetch_sub(1, Ordering::Relaxed);
        }

        {
            // The clock starts *after* the lock: waiting for the frame task
            // to finish writing is queueing, not conversion, and mixing the
            // two produced a 641 ms "conversion" during WiFi association the
            // first time this was measured.
            let frame = FRAME.lock().await;
            let t0 = Instant::now();
            display::render(&frame, fb, mode(), phase);
            let us = t0.elapsed().as_micros() as u32;
            RENDER_US.store(us, Ordering::Relaxed);
            RENDER_US_MAX.fetch_max(us, Ordering::Relaxed);
        }
        phase = phase.wrapping_add(1);

        let mut xfer = hub75.swap(fb).expect("swap already in flight");
        xfer.wait_for_done().await;
        fb = xfer.wait().expect("hub75 DMA transfer failed");
        SWAPS.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Status screen
// ---------------------------------------------------------------------------

/// Draws the idle screen whenever no stream is running and no pattern is
/// held. Redraws on state change, and every second while idle so the clock of
/// "nothing is streaming" keeps ticking even if the display task is asleep.
#[embassy_executor::task]
async fn status_task(stack: embassy_net::Stack<'static>) {
    let mut shown: Option<(status::Net, u8, bool)> = None;
    // "joining" and "lost" are the same state to the network stack and very
    // different states to someone looking at the panel, so the difference is
    // whether we ever got as far as an address.
    let mut had_address = false;
    loop {
        let net = if let Some(cfg) = stack.config_v4() {
            let o = cfg.address.address().octets();
            had_address = true;
            status::Net::Address(o)
        } else if stack.is_link_up() {
            status::Net::Associated
        } else if had_address {
            status::Net::Lost
        } else {
            status::Net::Joining
        };

        let streaming =
            now_ms().wrapping_sub(LAST_FRAME_MS.load(Ordering::Relaxed)) < STREAM_IDLE.as_millis() as u32;
        let hold = PATTERN_HOLD.load(Ordering::Relaxed);
        let brightness = BRIGHTNESS.load(Ordering::Relaxed);

        if hold == 0 && !streaming {
            let want = (net, brightness, true);
            if shown != Some(want) {
                let mut frame = FRAME.lock().await;
                status::draw(&mut frame, net, brightness);
                drop(frame);
                FRAME_SEQ.fetch_add(1, Ordering::Relaxed);
                shown = Some(want);
            }
        } else {
            shown = None;
        }
        Timer::after(Duration::from_millis(200)).await;
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                info!(
                    "wifi: connected ssid {:?} ch {} bssid {:02x?}",
                    info.ssid, info.channel, info.bssid
                );
                let reason = controller.wait_for_disconnect_async().await;
                warn!("wifi: disconnected {:?}", reason);
            }
            Err(e) => warn!("wifi: connect failed {:?}", e),
        }
        Timer::after(Duration::from_millis(2000)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

/// UDP 49374. Still the spike's raw-RGB888 test receiver, plus the bench
/// command channel in `testcmd`. Card 008 replaces the whole task.
#[embassy_executor::task]
async fn frame_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("net: address {}", cfg.address);
    }

    let rx_meta = mk_static!([PacketMetadata; 8], [PacketMetadata::EMPTY; 8]);
    let rx_buf = mk_static!([u8; 4 * MAX_DATAGRAM], [0u8; 4 * MAX_DATAGRAM]);
    let tx_meta = mk_static!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let tx_buf = mk_static!([u8; MAX_DATAGRAM], [0u8; MAX_DATAGRAM]);

    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(FRAME_PORT).expect("bind failed");
    info!("net: listening on udp/{}", FRAME_PORT);

    let mut datagram = [0u8; MAX_DATAGRAM];
    loop {
        let (len, _from) = match socket.recv_from(&mut datagram).await {
            Ok(v) => v,
            Err(e) => {
                warn!("net: recv error {:?}", e);
                continue;
            }
        };
        testcmd::handle(&datagram[..len]).await;
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// The numbers the camera cannot measure. Every one of these feeds the frame
/// time budget table in card 007's log.
#[embassy_executor::task]
async fn telemetry_task() {
    const PERIOD_S: u32 = 5;
    let mut last_frames = 0u32;
    let mut last_swaps = 0u32;
    loop {
        Timer::after(Duration::from_secs(PERIOD_S as u64)).await;
        let frames = FRAMES_RECEIVED.load(Ordering::Relaxed);
        let swaps = SWAPS.load(Ordering::Relaxed);
        let stats = esp_alloc::HEAP.stats();
        info!(
            "telemetry: {} fps in, {} swaps/s, {} dropped | render {} us (max {}) | bright {} -> {} slots from {} | gamma {} dither {} | heap used {} free {}",
            frames.wrapping_sub(last_frames) / PERIOD_S,
            swaps.wrapping_sub(last_swaps) / PERIOD_S,
            FRAMES_DROPPED.load(Ordering::Relaxed),
            RENDER_US.load(Ordering::Relaxed),
            RENDER_US_MAX.load(Ordering::Relaxed),
            BRIGHTNESS.load(Ordering::Relaxed),
            target_oe_slots(),
            OE_START.load(Ordering::Relaxed),
            GAMMA_ON.load(Ordering::Relaxed) as u8,
            DITHER_ON.load(Ordering::Relaxed) as u8,
            stats.current_usage,
            stats.size - stats.current_usage,
        );
        last_frames = frames;
        last_swaps = swaps;
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
    esp_println::logger::init_logger_from_env();
    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 48 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // --- panel ------------------------------------------------------------
    let fb0 = mk_static!(FrameBuffer, FrameBuffer::new());
    let fb1 = mk_static!(FrameBuffer, FrameBuffer::new());
    // Come up already dimmed. `FrameBuffer::new()` formats for the widest
    // output-enable window the build can produce, which is well over the
    // power cap; letting a single refresh out at that duty would be a bug
    // with a current spike attached to it.
    let slots = display::slots_for(display::DEFAULT_BRIGHTNESS);
    fb0.set_oe_slots(slots);
    fb1.set_oe_slots(slots);
    BRIGHTNESS_DIRTY.store(0, Ordering::Relaxed);

    let tx_descriptors = esp_hub75::hub75_dma_descriptors!(FrameBuffer);

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

    let hub75 = Hub75::new_async(
        peripherals.I2S0,
        pins,
        peripherals.DMA_I2S0,
        tx_descriptors,
        Hub75Config::new()
            .with_frequency(PIXEL_CLOCK)
            .with_interrupt_priority(Priority::Priority3),
        &*fb0,
    )
    .expect("hub75 init failed");

    // --- wifi -------------------------------------------------------------
    let station = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(SSID.try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD.try_into().unwrap(),
            )),
    );

    let mut controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default()
            .with_initial_config(station)
            .with_rx_queue_size(3)
            .with_country_info(*b"US"),
    )
    .expect("wifi init failed");

    controller
        .set_power_saving(PowerSaveMode::None)
        .expect("failed to disable power saving");

    let interface = Interface::station();

    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        interface,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    );

    // Something on the panel before WiFi has a chance to take nine seconds.
    {
        let mut frame = FRAME.lock().await;
        status::draw(&mut frame, status::Net::Joining, display::DEFAULT_BRIGHTNESS);
    }
    FRAME_SEQ.fetch_add(1, Ordering::Relaxed);

    spawner.spawn(display_task(hub75, fb1).unwrap());
    spawner.spawn(wifi_task(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(frame_task(stack).unwrap());
    spawner.spawn(status_task(stack).unwrap());
    spawner.spawn(mdns::mdns_task(stack).unwrap());
    spawner.spawn(telemetry_task().unwrap());

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
