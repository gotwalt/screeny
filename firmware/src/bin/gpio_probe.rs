//! `gpio_probe` — a one-minute bench answer to "which GPIO is the button?"
//!
//! Card 202 identified the Tidbyt Gen 1 reset button as **GPIO15** by static
//! analysis of Tidbyt's own firmware (see `docs/research/008-button.md`): the
//! stock image carries a 24-byte `gpio_config_t` whose `pin_bit_mask` is
//! `0x8000`, input, internal pull-up, any-edge interrupt, and its press test is
//! `gpio_get_level(15) == 0`. This binary exists to *confirm* that on the
//! bench, and to rule the other candidates out, without anyone having to guess.
//!
//! It is deliberately tiny: **no WiFi, no panel, no DMA, no esp-rtos, no
//! embassy.** Nothing is driven; every candidate is configured as an input and
//! polled. The panel stays dark, so nothing here can exceed the brightness cap.
//!
//! ## What it does
//!
//! Two phases, announced on serial and repeated forever:
//!
//! * **phase A** — every pull-capable candidate gets an internal **pull-up**.
//!   A button to ground reads HIGH at rest and LOW while held.
//! * **phase B** — the same pins get an internal **pull-down**. A pin with
//!   nothing on it now reads LOW; a pin held up by an external pull-up (or a
//!   board strap) stays HIGH, which is how an externally-loaded pin gives
//!   itself away.
//!
//! Each phase prints the resting level of every pin, then logs every level
//! change with the pin number and a millisecond timestamp, and prints a
//! one-line summary every 5 s. A floating pin can produce thousands of edges a
//! second, so each pin is capped at [`MAX_EVENTS_PER_SECOND`] logged edges per
//! second; when a pin is capped it says so once and the summary reports how
//! many edges were swallowed. A pin that is capped every second is floating,
//! not pressed.
//!
//! ## Build (it is not part of the normal firmware build)
//!
//! ```text
//! . ~/export-esp.sh
//! cd firmware && cargo build --release --features gpio-probe --bin gpio_probe
//! ```
//!
//! The `required-features` key in `Cargo.toml` keeps a plain
//! `cargo build --release` from building this at all.
//!
//! ## Pins that are NOT probed, and why
//!
//! The 14 HUB75 lines in `tidbyt::pins` (2, 4, 5, 18, 19, 21, 22, 23, 25, 26,
//! 27, 32, 33), GPIO16/17 (PSRAM), GPIO1/3 (UART0, which is this log), and
//! GPIO6-11 (SPI flash). Driving or loading any of those is how a bench
//! session ends early.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::delay::Delay;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Pin, Pull};
use esp_hal::time::Instant;
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

// ---------------------------------------------------------------------------
// What we probe
// ---------------------------------------------------------------------------

/// A candidate pin: its number, whether we may apply an internal pull, and a
/// one-line note that goes into the legend so the serial log is self-describing.
struct Candidate {
    gpio: u8,
    /// `false` means "read it, but never pull it".
    pulls: bool,
    note: &'static str,
}

const CANDIDATES: [Candidate; 9] = [
    Candidate {
        gpio: 0,
        pulls: true,
        note: "boot strap; also on the CP2102N auto-reset circuit",
    },
    Candidate {
        gpio: 12,
        pulls: false,
        note: "MTDI flash-voltage strap: READ ONLY, no internal pull applied",
    },
    Candidate {
        gpio: 13,
        pulls: true,
        note: "board-ID strap, ADC2_CH4 in the stock firmware",
    },
    Candidate {
        gpio: 14,
        pulls: true,
        note: "believed free (Tidbyt's CH_E comment)",
    },
    Candidate {
        gpio: 15,
        pulls: true,
        note: "EXPECTED BUTTON: stock gpio_config_t is 1<<15, input, pull-up",
    },
    Candidate {
        gpio: 34,
        pulls: false,
        note: "input-only, no internal pulls exist",
    },
    Candidate {
        gpio: 35,
        pulls: false,
        note: "input-only, no internal pulls exist",
    },
    Candidate {
        gpio: 36,
        pulls: false,
        note: "input-only, no internal pulls exist",
    },
    Candidate {
        gpio: 39,
        pulls: false,
        note: "input-only, no internal pulls exist",
    },
];

const N: usize = CANDIDATES.len();

/// How long each phase runs before the other one starts.
const PHASE_MILLIS: u64 = 25_000;

/// Poll period. A human press is tens of milliseconds at the very shortest, so
/// 1 ms cannot miss one, and it is slow enough that the log is the bottleneck
/// rather than the CPU.
const POLL_MILLIS: u32 = 1;

/// Logged edges per pin per second. Above this the pin is floating, not pressed,
/// and printing every edge would flood the port and hide the real answer.
const MAX_EVENTS_PER_SECOND: u32 = 8;

/// Summary cadence.
const SUMMARY_MILLIS: u64 = 5_000;

// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

fn level_str(level: Level) -> &'static str {
    match level {
        Level::High => "HIGH",
        Level::Low => "LOW ",
    }
}

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    // Order must match `CANDIDATES`.
    let pins: [AnyPin<'_>; N] = [
        peripherals.GPIO0.degrade(),
        peripherals.GPIO12.degrade(),
        peripherals.GPIO13.degrade(),
        peripherals.GPIO14.degrade(),
        peripherals.GPIO15.degrade(),
        peripherals.GPIO34.degrade(),
        peripherals.GPIO35.degrade(),
        peripherals.GPIO36.degrade(),
        peripherals.GPIO39.degrade(),
    ];
    // The pin table has to agree with itself, or every number printed below is
    // a lie. Cheap to check, so check it before anything is configured.
    for (i, pin) in pins.iter().enumerate() {
        assert_eq!(
            pin.number(),
            CANDIDATES[i].gpio,
            "gpio_probe pin table is out of order"
        );
    }

    let mut inputs: [Input<'_>; N] = pins.map(|pin| Input::new(pin, InputConfig::default()));

    println!();
    println!("=====================================================================");
    println!("screeny gpio_probe (card 202) — nothing is driven, no WiFi, no panel");
    println!("=====================================================================");
    println!("Candidates (pin: note):");
    for c in CANDIDATES.iter() {
        println!("  GPIO{:<2}: {}", c.gpio, c.note);
    }
    println!();
    println!("Phases alternate every {} s and repeat forever:", PHASE_MILLIS / 1000);
    println!("  phase A = internal PULL-UP   -> a button to ground rests HIGH, reads LOW while held");
    println!("  phase B = internal PULL-DOWN -> an unloaded pin rests LOW; one that stays HIGH is");
    println!("            held up by something external (a board strap or an external pull-up)");
    println!("Pins marked READ ONLY / input-only get no internal pull in either phase.");
    println!();
    println!("Edge lines look like:  [   1234 ms] GPIO15  HIGH -> LOW");
    println!(
        "At most {} edges per pin per second are printed; past that the pin is floating,",
        MAX_EVENTS_PER_SECOND
    );
    println!("not pressed, and the summary says how many edges were dropped.");
    println!();
    println!("What to do: press and release the button a few times in phase A, then hold it");
    println!("for ~10 s. Then do the same again in phase B. One pin should change; that is it.");
    println!("=====================================================================");
    println!();

    let mut last: [Level; N] = [Level::High; N];
    let mut edges: [u32; N] = [0; N];
    let mut dropped: [u32; N] = [0; N];
    let mut in_window: [u32; N] = [0; N];
    let mut capped: [bool; N] = [false; N];

    let mut phase_pull_up = true;
    let mut phase_started = 0u64;
    let mut window_started = 0u64;
    let mut last_summary = 0u64;
    let mut start_phase = true;

    loop {
        let t = now_ms();

        // ---- phase boundary ------------------------------------------------
        if start_phase || t.saturating_sub(phase_started) >= PHASE_MILLIS {
            if !start_phase {
                phase_pull_up = !phase_pull_up;
            }
            start_phase = false;
            phase_started = t;
            window_started = t;
            last_summary = t;

            let pull = if phase_pull_up { Pull::Up } else { Pull::Down };
            let name = if phase_pull_up { "A: PULL-UP" } else { "B: PULL-DOWN" };

            for (i, input) in inputs.iter_mut().enumerate() {
                let want = if CANDIDATES[i].pulls {
                    pull
                } else {
                    Pull::None
                };
                input.apply_config(&InputConfig::default().with_pull(want));
            }
            // Give the pads a moment to settle before the resting levels are read.
            delay.delay_millis(5);

            println!();
            println!("---- phase {name} (next {} s) ----", PHASE_MILLIS / 1000);
            for (i, input) in inputs.iter().enumerate() {
                let level = input.level();
                last[i] = level;
                edges[i] = 0;
                dropped[i] = 0;
                in_window[i] = 0;
                capped[i] = false;
                let pulled = if CANDIDATES[i].pulls {
                    if phase_pull_up { "pull-up" } else { "pull-down" }
                } else {
                    "no pull"
                };
                println!(
                    "  rest: GPIO{:<2} {}  ({})",
                    CANDIDATES[i].gpio,
                    level_str(level),
                    pulled
                );
            }
            println!();
        }

        // ---- rate-limit window ---------------------------------------------
        if t.saturating_sub(window_started) >= 1_000 {
            window_started = t;
            for i in 0..N {
                in_window[i] = 0;
                capped[i] = false;
            }
        }

        // ---- edges -----------------------------------------------------------
        for (i, input) in inputs.iter().enumerate() {
            let level = input.level();
            if level == last[i] {
                continue;
            }
            let from = last[i];
            last[i] = level;
            edges[i] = edges[i].saturating_add(1);

            if in_window[i] < MAX_EVENTS_PER_SECOND {
                in_window[i] += 1;
                println!(
                    "[{:>7} ms] GPIO{:<2} {} -> {}",
                    t,
                    CANDIDATES[i].gpio,
                    level_str(from),
                    level_str(level)
                );
            } else {
                dropped[i] = dropped[i].saturating_add(1);
                if !capped[i] {
                    capped[i] = true;
                    println!(
                        "[{:>7} ms] GPIO{:<2} capping at {} edges/s — this pin is floating, not pressed",
                        t, CANDIDATES[i].gpio, MAX_EVENTS_PER_SECOND
                    );
                }
            }
        }

        // ---- summary ---------------------------------------------------------
        if t.saturating_sub(last_summary) >= SUMMARY_MILLIS {
            last_summary = t;
            let phase = if phase_pull_up { 'A' } else { 'B' };
            print_summary(phase, t, &inputs, &edges, &dropped);
        }

        delay.delay_millis(POLL_MILLIS);
    }
}

/// One line, every 5 s: where every pin is sitting and how busy it has been.
fn print_summary(
    phase: char,
    t: u64,
    inputs: &[Input<'_>; N],
    edges: &[u32; N],
    dropped: &[u32; N],
) {
    // `heapless` is a dependency already, but a summary is easier to read (and
    // impossible to overflow) if it is printed piecewise.
    esp_println::print!("[{t:>7} ms] phase {phase} summary:");
    for (i, input) in inputs.iter().enumerate() {
        let level = match input.level() {
            Level::High => 'H',
            Level::Low => 'L',
        };
        esp_println::print!(" {}={}", CANDIDATES[i].gpio, level);
        if edges[i] > 0 {
            esp_println::print!("/{}", edges[i]);
        }
        if dropped[i] > 0 {
            esp_println::print!("+{}drop", dropped[i]);
        }
    }
    println!();
}
