//! `hub75_bitbang` — drive a HUB75 panel by hand, with no DMA, to separate
//! "the pins and the panel work" from "the DMA driver is set up wrong".
//!
//! Built for the LED Art Panel's pin map (`src/art_panel.rs`), 32 columns,
//! 1/16 scan. **No WiFi, no esp-hub75, no esp-rtos.** It shifts each row out
//! with plain GPIO writes, latches it, and lights it for a short window, then
//! moves to the next row address, forever.
//!
//! What it draws: eight 5 s phases (see `main`), each testing one line or one
//! assumption, slow clock edges (1 us) to rule timing out. One primary at a
//! time, never white; the lit window is about a fifth of each row, under the
//! firmware's duty cap. Refresh is several hundred Hz: nothing flashes.
//!
//! Build: `ESP_HAL_CONFIG_MIN_CHIP_REVISION=100 cargo build --release
//! --features hub75-bitbang --bin hub75_bitbang`.
#![no_std]
#![no_main]

#[allow(dead_code)]
#[path = "../panic.rs"]
mod panic;

use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

const COLS: usize = 32;
const ROW_ADDRS: usize = 16;

#[esp_hal::main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();
    let cfg = OutputConfig::default();

    // `art_panel::pins`, by hand: this binary does not link the firmware.
    let mut r1 = Output::new(p.GPIO18, Level::Low, cfg);
    let mut g1 = Output::new(p.GPIO21, Level::Low, cfg);
    let mut b1 = Output::new(p.GPIO4, Level::Low, cfg);
    let mut r2 = Output::new(p.GPIO32, Level::Low, cfg);
    let mut g2 = Output::new(p.GPIO19, Level::Low, cfg);
    let mut b2 = Output::new(p.GPIO33, Level::Low, cfg);
    let mut a = Output::new(p.GPIO25, Level::Low, cfg);
    let mut b = Output::new(p.GPIO5, Level::Low, cfg);
    let mut c = Output::new(p.GPIO26, Level::Low, cfg);
    let mut d = Output::new(p.GPIO17, Level::Low, cfg);
    let mut clk = Output::new(p.GPIO27, Level::Low, cfg);
    let mut lat = Output::new(p.GPIO16, Level::Low, cfg);
    // Output enable is active low: high is dark.
    let mut oe = Output::new(p.GPIO14, Level::High, cfg);

    println!("hub75_bitbang: 32x32, 1/16 scan, art panel pins; 8 phases of 5 s");

    // Phases, 5 s each, looping:
    //   0..=5  one data line held on for every pixel, every row:
    //          R1, G1, B1 (top half), R2, G2, B2 (bottom half)
    //   6      R1 and R2 on for the first 4 pixels shifted in only
    //   7      R1 on every pixel, but only row addresses 0 and 5
    const NAMES: [&str; 8] = ["R1", "G1", "B1", "R2", "G2", "B2", "first-4", "rows-0-5"];
    let lv = |on: bool| if on { Level::High } else { Level::Low };
    let start = esp_hal::time::Instant::now();
    let mut last_phase = usize::MAX;
    loop {
        let phase = (start.elapsed().as_millis() / 5000) as usize % 8;
        if phase != last_phase {
            println!("hub75_bitbang: t={} ms phase {} {}", start.elapsed().as_millis(), phase, NAMES[phase]);
            last_phase = phase;
        }
        for row in 0..ROW_ADDRS {
            for x in 0..COLS {
                let all = phase < 6;
                let first4 = phase == 6 && x < 4;
                let rows05 = phase == 7 && (row == 0 || row == 5);
                r1.set_level(lv((all && phase == 0) || first4 || rows05));
                g1.set_level(lv(all && phase == 1));
                b1.set_level(lv(all && phase == 2));
                r2.set_level(lv((all && phase == 3) || first4));
                g2.set_level(lv(all && phase == 4));
                b2.set_level(lv(all && phase == 5));
                delay.delay_micros(1);
                clk.set_high();
                delay.delay_micros(1);
                clk.set_low();
            }
            oe.set_high();
            a.set_level(lv(row & 1 != 0));
            b.set_level(lv(row & 2 != 0));
            c.set_level(lv(row & 4 != 0));
            d.set_level(lv(row & 8 != 0));
            delay.delay_micros(1);
            lat.set_high();
            delay.delay_micros(1);
            lat.set_low();
            delay.delay_micros(1);
            oe.set_low();
            delay.delay_micros(16);
            oe.set_high();
        }
    }
}
