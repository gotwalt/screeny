//! Everything the panel shows that did not arrive over the wire: the idle
//! status screen of spec section 7.5 and the `IDENTIFY` overlay of section 6.3.
//!
//! Both are drawn into the sRGB [`Frame`] like any other content, not straight
//! into the DMA buffer, so they go through the same gamma and brightness path
//! as a streamed frame — a status screen that ignored the brightness setting
//! would be the one thing on the device that could dazzle you.
//!
//! Fonts are `embedded-graphics`' own `FONT_5X7` and `FONT_4X6`. Both are
//! ASCII-only bitmaps with zero character spacing, so `FONT_4X6` fits 16
//! characters across the panel — enough for `255.255.255.255` with room over.
//! Card 007 photographed the 4x6 IP address and confirmed it is legible.

use core::fmt::Write;

use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_5X7};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};

use crate::display::Frame;
use crate::{COLS, ROWS};

/// What the frame task knows about the network, in the order it learns it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Net {
    Joining,
    Associated,
    Address([u8; 4]),
    Lost,
}

/// Deliberately not white. The panel is bright, this screen may be up for
/// hours, and a blue-white on black matches what the stock firmware does.
const TITLE: Rgb888 = Rgb888::new(0x5a, 0x9e, 0xff);
const LABEL: Rgb888 = Rgb888::new(0x70, 0x70, 0x70);
const VALUE: Rgb888 = Rgb888::new(0xc8, 0xc8, 0xc8);
const OK: Rgb888 = Rgb888::new(0x30, 0xc0, 0x50);
const WARN: Rgb888 = Rgb888::new(0xd0, 0x80, 0x20);

/// The idle screen: name, address, signal, and a slow ambient sweep.
///
/// `phase` advances once per redraw (about 10 Hz); the sweep is the "slow
/// ambient animation" section 7.5 asks for, and it doubles as proof of life —
/// a frozen panel and an idle panel look identical without it.
pub fn status(frame: &mut Frame, name: &str, net: Net, phase: u32) {
    frame.clear();

    let (state_text, state_colour): (&str, Rgb888) = match net {
        Net::Joining => ("joining wifi", WARN),
        Net::Associated => ("dhcp...", WARN),
        Net::Address(_) => ("ready", OK),
        Net::Lost => ("network down", WARN),
    };

    // A 12-character name fits the wider font; anything longer drops to 4x6,
    // and past 16 characters it is simply cut off rather than wrapped.
    if name.len() <= 12 {
        let s = MonoTextStyle::new(&FONT_5X7, TITLE);
        let _ = Text::with_baseline(name, Point::new(1, 0), s, Baseline::Top).draw(frame);
    } else {
        let s = MonoTextStyle::new(&FONT_4X6, TITLE);
        let cut = &name[..name.len().min(15)];
        let _ = Text::with_baseline(cut, Point::new(1, 1), s, Baseline::Top).draw(frame);
    }

    let state = MonoTextStyle::new(&FONT_4X6, state_colour);
    let _ = Text::with_baseline(state_text, Point::new(1, 9), state, Baseline::Top).draw(frame);

    let value = MonoTextStyle::new(&FONT_4X6, VALUE);
    let label = MonoTextStyle::new(&FONT_4X6, LABEL);
    if let Net::Address(ip) = net {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        let _ = Text::with_baseline(&line, Point::new(1, 16), value, Baseline::Top).draw(frame);
        let _ = Text::with_baseline("screeny.local", Point::new(1, 23), label, Baseline::Top)
            .draw(frame);
        rssi_bars(frame, crate::RSSI_DBM.load(core::sync::atomic::Ordering::Relaxed));
    } else {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "bright {}", crate::BRIGHTNESS.load(core::sync::atomic::Ordering::Relaxed));
        let _ = Text::with_baseline(&line, Point::new(1, 16), label, Baseline::Top).draw(frame);
    }

    ambient(frame, phase);
}

/// Four signal bars in the top-right corner, from the last beacon RSSI.
///
/// The thresholds are the usual Wi-Fi ones: -55 excellent, -65 good, -75
/// fair, below that one bar. An unfilled bar is drawn dim rather than left
/// black so that "no signal" and "no bars widget" cannot be confused.
fn rssi_bars(frame: &mut Frame, rssi_dbm: i8) {
    let bars = match rssi_dbm {
        0 => 0, // never reported
        r if r >= -55 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        _ => 1,
    };
    for b in 0..4usize {
        let h = b + 1;
        let x = COLS - 10 + b * 2;
        let on = b < bars;
        let c = if on { [0x30, 0xc0, 0x50] } else { [0x18, 0x18, 0x18] };
        for dy in 0..h {
            frame.set(x, 6 - dy, c);
            frame.set(x, 6 - dy, c);
        }
    }
}

/// A single dim pixel crossing the bottom row, once every ~13 seconds.
fn ambient(frame: &mut Frame, phase: u32) {
    let x = (phase as usize / 2) % COLS;
    let y = ROWS - 1;
    for d in 0..6usize {
        if x < d {
            continue;
        }
        let fade = (6 - d) as u32;
        let v = (0x40 * fade / 6) as u8;
        frame.set(x - d, y, [v / 3, v / 2, v]);
    }
}

/// The `IDENTIFY` overlay: "which one is this?".
///
/// Section 6.3 requires it to work in any state, including while another
/// sender holds the lock, so the frame task draws it over whatever else it
/// would have composed. High contrast, the name, and the address, because
/// those are the two things you are trying to match up.
pub fn identify(frame: &mut Frame, name: &str, net: Net, phase: u32) {
    // A moving chevron border: unmistakable across a room, and cheap.
    let on = (phase / 3) % 2 == 0;
    let (a, b) = if on {
        ([0xff, 0xff, 0x00], [0x00, 0x00, 0x00])
    } else {
        ([0x00, 0x00, 0x00], [0xff, 0xff, 0x00])
    };
    frame.clear();
    for x in 0..COLS {
        let c = if (x / 4) % 2 == 0 { a } else { b };
        frame.set(x, 0, c);
        frame.set(x, 1, c);
        frame.set(x, ROWS - 2, c);
        frame.set(x, ROWS - 1, c);
    }
    for y in 2..ROWS - 2 {
        let c = if (y / 4) % 2 == 0 { a } else { b };
        frame.set(0, y, c);
        frame.set(1, y, c);
        frame.set(COLS - 2, y, c);
        frame.set(COLS - 1, y, c);
    }

    let s = MonoTextStyle::new(&FONT_4X6, Rgb888::new(0xff, 0xff, 0xff));
    let cut = &name[..name.len().min(13)];
    let _ = Text::with_baseline(cut, Point::new(4, 9), s, Baseline::Top).draw(frame);
    if let Net::Address(ip) = net {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        let _ = Text::with_baseline(&line, Point::new(4, 17), s, Baseline::Top).draw(frame);
    }
}
