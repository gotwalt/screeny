//! screeny firmware skeleton — compile-only spike for card 001.
//!
//! Proves that one binary can hold, at the same time:
//!
//!   * the embassy executor on top of `esp-rtos`,
//!   * `esp-radio` WiFi in station mode with `embassy-net` (DHCP),
//!   * a UDP socket bound to the frame port,
//!   * an `edge-mdns` DNS-SD responder advertising `_screeny._udp`,
//!   * `esp-hub75` driving the 64x32 panel over I2S0 parallel DMA with the
//!     Tidbyt Gen 1 pin map, including the FM6124 driver-chip init,
//!
//! and that the RAM they want together fits. It has never been run on
//! hardware: see `docs/research/001-firmware-stack.md` for what that leaves
//! unverified.

#![no_std]
#![no_main]

mod mdns;
mod panel_init;
mod tidbyt;

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use embedded_graphics::geometry::Point;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig, Pin};
use esp_hal::interrupt::Priority;
use esp_hal::rng::Rng;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer;
use esp_hub75::framebuffer::compute_rows;
use esp_hub75::{Color, Hub75, Hub75Config, Hub75Pins16};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, ControllerConfig, Interface, PowerSaveMode,
    WifiController,
};
use log::{info, warn};

esp_bootloader_esp_idf::esp_app_desc!();

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const SSID: &str = "Example-Wifi1";
const PASSWORD: &str = "password9";

/// Ports from card 003. The control port is not implemented here; it is named
/// so that nobody else claims it.
const FRAME_PORT: u16 = 49374;
const CONTROL_PORT: u16 = 49375;

/// One frame per datagram, at most one Ethernet MTU of payload.
///
/// This only holds because `.cargo/config.toml` raises esp-radio's MTU to
/// 1500. Its default is 1492, which would cap the payload at 1464.
const MAX_DATAGRAM: usize = 1472;

const COLS: usize = tidbyt::PANEL_COLS;
const ROWS: usize = tidbyt::PANEL_ROWS;
/// 1/16 scan: the panel shifts two rows at a time.
const NROWS: usize = compute_rows(ROWS);

/// Binary-code-modulation depth, i.e. bits per colour channel the panel can
/// actually show. Every extra plane halves the refresh rate.
const PLANES: usize = 6;

/// I2S pixel clock. 10 MHz is what Tidbyt's own firmware runs this panel at,
/// so it is the only rate we know the hardware tolerates. The ESP32's I2S
/// tops out near 19 MHz; see the refresh-rate table in the research doc for
/// what raising it buys and costs.
const PIXEL_CLOCK: Rate = Rate::from_mhz(tidbyt::PIXEL_CLOCK_MHZ);

type FrameBuffer = DmaFrameBuffer<NROWS, COLS, PLANES>;

/// Panel refresh rate for the configuration above, computed by the driver at
/// compile time from the BCM sequence and the pixel clock.
const REFRESH_HZ: u32 = esp_hub75::refresh_hz::<FrameBuffer>(PIXEL_CLOCK);

// A configuration that cannot keep up with the eye is a bug, not a tuning
// question, so fail the build rather than the bring-up.
const _: () = assert!(
    REFRESH_HZ >= 120,
    "panel refresh below 120 Hz: reduce PLANES or raise PIXEL_CLOCK"
);

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// The most recently received frame, as RGB888. The real firmware will hold a
/// decoded frame here instead; the size is the same either way, and the point
/// of carrying it in the skeleton is that it is the largest single block of
/// SRAM we allocate outside the DMA buffers.
struct RgbFrame {
    px: [[u8; 3]; COLS * ROWS],
}

impl RgbFrame {
    const fn new() -> Self {
        Self {
            px: [[0u8; 3]; COLS * ROWS],
        }
    }
}

static FRAME: Mutex<CriticalSectionRawMutex, RgbFrame> = Mutex::new(RgbFrame::new());

/// Bumped every time a datagram lands, so the display task can tell a new
/// frame from a repeat without holding the frame lock.
static FRAME_SEQ: AtomicU32 = AtomicU32::new(0);
static FRAMES_DROPPED: AtomicU32 = AtomicU32::new(0);

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static CELL: ::static_cell::StaticCell<$t> = ::static_cell::StaticCell::new();
        CELL.uninit().write($val)
    }};
}
pub(crate) use mk_static;

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// Owns the panel. Redraws whenever the network task reports a new frame, and
/// otherwise leaves the DMA engine looping over the buffer it already has.
#[embassy_executor::task]
async fn display_task(hub75: Hub75<esp_hal::Async, FrameBuffer>, mut fb: &'static mut FrameBuffer) {
    info!("display: refresh {} Hz, {} planes", REFRESH_HZ, PLANES);

    let mut last_seq = u32::MAX;
    loop {
        let seq = FRAME_SEQ.load(Ordering::Relaxed);
        if seq == last_seq {
            // Nothing new. At 30 fps this wakes us about twice per frame.
            Timer::after(Duration::from_millis(8)).await;
            continue;
        }
        last_seq = seq;

        {
            let frame = FRAME.lock().await;
            for y in 0..ROWS {
                for x in 0..COLS {
                    let [r, g, b] = frame.px[y * COLS + x];
                    fb.set_pixel(
                        Point::new(x as i32, y as i32),
                        Color::new(scale(r), scale(g), scale(b)),
                    );
                }
            }
        }

        let mut xfer = hub75.swap(fb).expect("swap already in flight");
        xfer.wait_for_done().await;
        fb = xfer.wait().expect("hub75 DMA transfer failed");
    }
}

/// Checks a typed GPIO peripheral against the GPIO number the Tidbyt sources
/// give for that signal.
#[track_caller]
fn assert_pin(pin: &impl Pin, expected: u8) {
    assert_eq!(pin.number(), expected, "pin map disagrees with tidbyt::pins");
}

/// Hard brightness cap. The panel is powered from laptop USB, so full-scale
/// white is not a thing we are allowed to draw.
#[inline]
fn scale(v: u8) -> u8 {
    ((v as u16 * tidbyt::BRIGHTNESS_CAP as u16) / 255) as u8
}

/// Keeps the station associated, reconnecting forever.
#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>) {
    // Bring-up aid: list what the radio can actually see before trying to join.
    match controller
        .scan_async(&esp_radio::wifi::scan::ScanConfig::default().with_max(20))
        .await
    {
        Ok(aps) => {
            for ap in aps {
                info!(
                    "scan: {:?} ch {} rssi {} auth {:?}",
                    ap.ssid, ap.channel, ap.signal_strength, ap.auth_method
                );
            }
        }
        Err(e) => warn!("scan failed {:?}", e),
    }
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                info!("wifi: connected {:?}", info);
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

/// Receives one frame per datagram and drops it into `FRAME`.
///
/// Newest wins: if the display task still holds the frame lock when a datagram
/// arrives we throw the datagram away rather than queue it, because a late
/// frame is worth less than the next one.
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

        // Card 002 picks the real codec; the skeleton only proves the path, so
        // it treats the payload as raw RGB888 and pads short frames with black.
        match FRAME.try_lock() {
            Ok(mut frame) => {
                let pixels = (len / 3).min(COLS * ROWS);
                for i in 0..pixels {
                    frame.px[i] = [datagram[i * 3], datagram[i * 3 + 1], datagram[i * 3 + 2]];
                }
                for px in frame.px.iter_mut().skip(pixels) {
                    *px = [0, 0, 0];
                }
                drop(frame);
                FRAME_SEQ.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                FRAMES_DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Prints the counters that the camera rig cannot measure.
#[embassy_executor::task]
async fn telemetry_task() {
    let mut last = 0u32;
    loop {
        Timer::after(Duration::from_secs(5)).await;
        let seq = FRAME_SEQ.load(Ordering::Relaxed);
        info!(
            "telemetry: {} fps, {} dropped, heap {:?}",
            (seq.wrapping_sub(last)) / 5,
            FRAMES_DROPPED.load(Ordering::Relaxed),
            esp_alloc::HEAP.stats()
        );
        last = seq;
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();
    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // esp-radio allocates its WiFi buffers from the global heap. The first
    // region is DRAM the ROM bootloader no longer needs; the second is plain
    // internal DRAM.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 48 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // --- panel ------------------------------------------------------------
    let fb0 = mk_static!(FrameBuffer, FrameBuffer::new());
    let fb1 = mk_static!(FrameBuffer, FrameBuffer::new());
    info!(
        "display: framebuffer {} bytes each, {} Hz refresh",
        core::mem::size_of::<FrameBuffer>(),
        REFRESH_HZ
    );

    let tx_descriptors = esp_hub75::hub75_dma_descriptors!(FrameBuffer);

    // The pin map lives in `tidbyt::pins` as plain numbers, because that is
    // how the Tidbyt sources state it and how a human checks it against them.
    // The driver wants typed peripherals instead, so the two could drift;
    // this ties them together and costs one boot-time comparison each.
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

    // The driver chips ignore pixel data until their configuration registers
    // are loaded, and that has to happen while the GPIOs are still ours —
    // once I2S owns them we cannot bit-bang. See `panel_init`.
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

    // Tidbyt Gen 1 map; see `tidbyt::pins` for where each number comes from.
    let pins = Hub75Pins16 {
        // Observed on this unit (MAC b4:8a:0a:4a:00:a4): the lines the hdk names
        // R/G/B drive blue/red/green respectively, so the colours are rotated here.
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
        // 1/16 scan: no E line on this board. See `tidbyt::pins::E_UNUSED`.
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
            // The refresh ISR must outrank WiFi's handlers or the panel
            // flickers whenever the radio is busy. `circular-dma` means the
            // ISR only fires on a buffer swap, but a swap that lands late
            // still tears.
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
            // We drain to the newest frame and throw the rest away, so a deep
            // driver queue only buys us stale frames and latency. Three is a
            // starting point, not a measured optimum.
            .with_rx_queue_size(3)
            // The default is "CN", which is the wrong channel set here.
            .with_country_info(*b"US"),
    )
    .expect("wifi init failed");

    // Modem power save parks the radio between beacons, which costs tens of
    // milliseconds of latency on an unsolicited datagram. We are streaming,
    // so it has to go.
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

    spawner.spawn(display_task(hub75, fb1).unwrap());
    spawner.spawn(wifi_task(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(frame_task(stack).unwrap());
    spawner.spawn(mdns::mdns_task(stack).unwrap());
    spawner.spawn(telemetry_task().unwrap());

    // Until the first datagram arrives, show something so a human can tell the
    // panel is alive and the colour channels are in the right order: a red,
    // green and blue band, top to bottom.
    {
        let mut frame = FRAME.lock().await;
        for y in 0..ROWS {
            let band = match y * 3 / ROWS {
                0 => [255, 0, 0],
                1 => [0, 255, 0],
                _ => [0, 0, 255],
            };
            for x in 0..COLS {
                // Ramp left to right so a wrong BCM depth shows up as banding.
                let k = (x * 255 / (COLS - 1)) as u8;
                frame.px[y * COLS + x] = [
                    ((band[0] as u16 * k as u16) / 255) as u8,
                    ((band[1] as u16 * k as u16) / 255) as u8,
                    ((band[2] as u16 * k as u16) / 255) as u8,
                ];
            }
        }
    }
    FRAME_SEQ.fetch_add(1, Ordering::Relaxed);

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
