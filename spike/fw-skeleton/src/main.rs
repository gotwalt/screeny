//! screeny firmware skeleton — compile-only spike for card 001.
//!
//! Proves that one binary can hold, at the same time:
//!
//!   * the embassy executor on top of `esp-rtos`,
//!   * `esp-radio` WiFi in station mode with `embassy-net` (DHCP),
//!   * a UDP socket bound to the frame port,
//!   * `esp-hub75` driving the 64x32 panel over I2S0 parallel DMA with the
//!     Tidbyt Gen 1 pin map,
//!
//! and that the RAM they want together fits. It has never been run on
//! hardware: see `docs/research/001-firmware-stack.md` for what that leaves
//! unverified.

#![no_std]
#![no_main]

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
use esp_hal::gpio::Pin;
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
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const SSID: &str = "example-wifi1";
const PASSWORD: &str = "password9";

/// Provisional frame port. Card 003 owns the real protocol.
const FRAME_PORT: u16 = 33550;

/// One frame per datagram, at most one Ethernet MTU of payload.
const MAX_DATAGRAM: usize = 1472;

const COLS: usize = tidbyt::PANEL_COLS;
const ROWS: usize = tidbyt::PANEL_ROWS;
/// 1/16 scan: the panel shifts two rows at a time.
const NROWS: usize = compute_rows(ROWS);

/// Binary-code-modulation depth, i.e. bits per colour channel the panel can
/// actually show. Every extra plane halves the refresh rate.
const PLANES: usize = 6;

/// I2S pixel clock. The ESP32's I2S peripheral tops out around 19 MHz.
const PIXEL_CLOCK: Rate = Rate::from_mhz(19);

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
        static CELL: StaticCell<$t> = StaticCell::new();
        CELL.uninit().write($val)
    }};
}

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

/// Hard brightness cap. The panel is powered from laptop USB, so full-scale
/// white is not a thing we are allowed to draw.
#[inline]
fn scale(v: u8) -> u8 {
    ((v as u16 * tidbyt::BRIGHTNESS_CAP as u16) / 255) as u8
}

/// Keeps the station associated, reconnecting forever.
#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>) {
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
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

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

    let pins = Hub75Pins16 {
        red1: peripherals.GPIO21.degrade(),
        grn1: peripherals.GPIO2.degrade(),
        blu1: peripherals.GPIO22.degrade(),
        red2: peripherals.GPIO23.degrade(),
        grn2: peripherals.GPIO4.degrade(),
        blu2: peripherals.GPIO27.degrade(),
        addr0: peripherals.GPIO26.degrade(),
        addr1: peripherals.GPIO5.degrade(),
        addr2: peripherals.GPIO25.degrade(),
        addr3: peripherals.GPIO18.degrade(),
        // 1/16 scan panel: no E line on the board, see `tidbyt::pins`.
        addr4: peripherals.GPIO33.degrade(),
        blank: peripherals.GPIO32.degrade(),
        clock: peripherals.GPIO14.degrade(),
        latch: peripherals.GPIO19.degrade(),
    };

    let hub75 = Hub75::new_async(
        peripherals.I2S0,
        pins,
        peripherals.DMA_I2S0,
        tx_descriptors,
        Hub75Config::new().with_frequency(PIXEL_CLOCK),
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
        ControllerConfig::default().with_initial_config(station),
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
