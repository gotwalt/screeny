//! FM6124 / FM6126A / ICN2038S register init.
//!
//! Tidbyt's firmware declares the panel as `HUB75_I2S_CFG::FM6126A`
//! (`hdk/src/display.cpp:59`), which in the HUB75 library routes to
//! `fm6124init()`. Those driver chips ignore pixel data until two 16-bit
//! configuration registers have been shifted in: REG1 sets the global current
//! gain and REG2 enables the output drivers. A panel that needs this and does
//! not get it stays dark, or comes up very dim.
//!
//! `esp-hub75` has no notion of driver-chip init, so we do it ourselves,
//! bit-banged over the same GPIOs before the I2S peripheral takes them over —
//! which is exactly when the C library does it too.
//!
//! This is a direct port of `fm6124init()` from
//! `ESP32-HUB75-MatrixPanel-leddrivers.cpp` (Tidbyt's fork, lines 45-101).
//! The tronbyt maintainer reports that the silicon on at least some Tidbyts is
//! actually an ICN2037, which does not need this; on such a panel the sequence
//! is harmless, because all it does is clock zeros through the shift
//! registers and blank them again.

use esp_hal::gpio::Output;

/// Columns in one row of the chain. 64 columns, one panel.
const PIXELS_PER_ROW: usize = super::COLS;

/// Global brightness / current gain. Bits 5..=10 set.
const REG1: [bool; 16] = [
    false, false, false, false, false, true, true, true, true, true, true, false, false, false,
    false, false,
];

/// Output enable: one bit, at position 9.
const REG2: [bool; 16] = [
    false, false, false, false, false, false, false, false, false, true, false, false, false,
    false, false, false,
];

/// Shifts REG1 and REG2 into the panel's driver chips.
///
/// `rgb` is `[r1, r2, g1, g2, b1, b2]`; the order does not matter, since every
/// data line carries the same bit.
pub fn fm6124_init(
    rgb: &mut [Output<'_>; 6],
    clk: &mut Output<'_>,
    lat: &mut Output<'_>,
    oe: &mut Output<'_>,
) {
    for pin in rgb.iter_mut() {
        pin.set_low();
    }
    clk.set_low();
    lat.set_low();
    // Blank the panel while we shift configuration through it.
    oe.set_high();

    // REG1. The latch goes high 11 clocks before the end of the row so the
    // driver chips start counting the value in.
    shift_register(rgb, clk, lat, &REG1, PIXELS_PER_ROW - 11);
    // REG2, latched one clock earlier.
    shift_register(rgb, clk, lat, &REG2, PIXELS_PER_ROW - 12);

    // Clock zeros through so the panel is clear afterwards.
    for pin in rgb.iter_mut() {
        pin.set_low();
    }
    for _ in 0..PIXELS_PER_ROW {
        pulse(clk);
    }

    lat.set_high();
    pulse(clk);
    lat.set_low();
    oe.set_low();
    pulse(clk);
}

fn shift_register(
    rgb: &mut [Output<'_>; 6],
    clk: &mut Output<'_>,
    lat: &mut Output<'_>,
    reg: &[bool; 16],
    latch_from: usize,
) {
    for col in 0..PIXELS_PER_ROW {
        // The chain is made of 16-bit shift registers, so the same 16-bit
        // pattern repeats across the row.
        let bit = reg[col % 16];
        for pin in rgb.iter_mut() {
            pin.set_level(bit.into());
        }
        if col >= latch_from {
            lat.set_high();
        }
        pulse(clk);
    }
    lat.set_low();
}

#[inline(always)]
fn pulse(clk: &mut Output<'_>) {
    clk.set_high();
    settle();
    clk.set_low();
    settle();
}

/// The C version relies on Arduino's `digitalWrite` being slow. Our GPIO
/// writes are a couple of register stores, so hold each level briefly; the
/// driver chips want tens of nanoseconds and this is a few hundred.
#[inline(always)]
fn settle() {
    let mut i = 0u32;
    while i < 20 {
        i = core::hint::black_box(i) + 1;
    }
}
